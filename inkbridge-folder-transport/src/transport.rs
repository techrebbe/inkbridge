use crate::{
    CloudFolder, CloudObject, DocumentFolders, FileObservation, PendingUpload, SyncReport,
    TransportAction, TransportState,
};
use inkbridge_broker::{sha256_hex, DeviceSide, RevisionPair, BROKER_PRODUCER};
use inkbridge_convert::{build_manifest, parse_baseline_bytes};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const GENERATED_BY: &str = "inkbridge-generated-by";
const DOCUMENT_ID: &str = "inkbridge-document-id";
const SOURCE_REVISIONS: &str = "inkbridge-source-revisions";
const SOURCE_REVISION: &str = "inkbridge-source-revision";
const SOURCE_VIEW_SHA256: &str = "inkbridge-source-view-sha256";
const CONTENT_SHA256: &str = "inkbridge-content-sha256";

pub trait BooxManifestBuilder {
    fn build(&self, pdf: &Path, baselines: &[PathBuf]) -> Result<BuiltBooxManifest, String>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BuiltBooxManifest {
    pub bytes: Vec<u8>,
    pub source_pdf_sha256: String,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct NativeBooxManifestBuilder;

impl BooxManifestBuilder for NativeBooxManifestBuilder {
    fn build(&self, pdf: &Path, baselines: &[PathBuf]) -> Result<BuiltBooxManifest, String> {
        // The folder adapter emits canonical, unshifted points. The broker is
        // the single owner of the presentation calibration applied to the
        // Supernote-bound manifest.
        let manifest = build_manifest(pdf, baselines, 0.0)?;
        let source_pdf_sha256 = manifest.document.pdf_sha256.clone();
        let mut bytes = serde_json::to_vec_pretty(&manifest).map_err(|error| error.to_string())?;
        bytes.push(b'\n');
        Ok(BuiltBooxManifest {
            bytes,
            source_pdf_sha256,
        })
    }
}

pub struct FolderTransport<'a, C, B> {
    cloud: &'a C,
    manifest_builder: &'a B,
    settle: Duration,
}

impl<'a, C: CloudFolder, B: BooxManifestBuilder> FolderTransport<'a, C, B> {
    pub fn new(cloud: &'a C, manifest_builder: &'a B, settle: Duration) -> Self {
        Self {
            cloud,
            manifest_builder,
            settle,
        }
    }

    pub fn sync_document(
        &self,
        document: &DocumentFolders,
        state: &mut TransportState,
        now: SystemTime,
    ) -> Result<SyncReport, String> {
        let mut next = state.clone();
        let report = self.sync_document_transaction(document, &mut next, now)?;
        *state = next;
        Ok(report)
    }

    fn sync_document_transaction(
        &self,
        document: &DocumentFolders,
        state: &mut TransportState,
        now: SystemTime,
    ) -> Result<SyncReport, String> {
        let mut report = SyncReport::default();
        fs::create_dir_all(&document.supernote_export_directory).map_err(|error| {
            format!(
                "could not create {}: {error}",
                document.supernote_export_directory.display()
            )
        })?;
        fs::create_dir_all(&document.supernote_incoming_directory).map_err(|error| {
            format!(
                "could not create {}: {error}",
                document.supernote_incoming_directory.display()
            )
        })?;

        self.observe_conflicts(document, state, &mut report)?;
        if !state
            .document_mut(&document.document_id)
            .conflicts
            .is_empty()
        {
            return Ok(report);
        }
        self.deliver_outputs(document, state, &mut report)?;
        self.recover_acknowledged_uploads(document, state)?;
        self.upload_boox_if_ready(document, state, now, &mut report)?;
        self.upload_supernote_if_ready(document, state, now, &mut report)?;
        Ok(report)
    }

    fn observe_conflicts(
        &self,
        document: &DocumentFolders,
        state: &mut TransportState,
        report: &mut SyncReport,
    ) -> Result<(), String> {
        let prefix = format!("Conflicts/{}/", document.document_id);
        let objects = self.cloud.list(&prefix)?;
        let previous = state.document_mut(&document.document_id).conflicts.clone();
        let current = objects
            .iter()
            .map(CloudObject::generation_key)
            .collect::<std::collections::BTreeSet<_>>();
        state
            .document_mut(&document.document_id)
            .conflicts
            .clone_from(&current);
        for object in objects {
            let key = object.generation_key();
            if !previous.contains(&key) {
                report.actions.push(TransportAction::Conflict {
                    object_path: object.path,
                });
            }
        }
        Ok(())
    }

    fn recover_acknowledged_uploads(
        &self,
        document: &DocumentFolders,
        state: &mut TransportState,
    ) -> Result<(), String> {
        let supernote_files = supernote_export_files(&document.supernote_export_directory)?
            .into_iter()
            .map(|path| {
                let hash = sha256_file(&path)?;
                Ok((hash, canonical_path_key(&path)))
            })
            .collect::<Result<Vec<_>, String>>()?;

        for side in [DeviceSide::Boox, DeviceSide::Supernote] {
            let accepted_revision = state
                .document_mut(&document.document_id)
                .revisions
                .get(side);
            if accepted_revision == 0 {
                continue;
            }
            let root = match side {
                DeviceSide::Boox => "BOOX_Folder",
                DeviceSide::Supernote => "Supernote_Folder",
            };
            let mut accepted_hashes = BTreeMap::<u64, String>::new();
            for object in self
                .cloud
                .list(&format!("{root}/{}/uploads/", document.document_id))?
            {
                if object
                    .metadata
                    .get(DOCUMENT_ID)
                    .is_none_or(|id| id != &document.document_id)
                {
                    continue;
                }
                let Some(source_revision) = object
                    .metadata
                    .get(SOURCE_REVISION)
                    .and_then(|value| value.parse::<u64>().ok())
                    .filter(|revision| *revision > 0 && *revision <= accepted_revision)
                else {
                    continue;
                };
                let Some(source_hash) = object
                    .metadata
                    .get(SOURCE_VIEW_SHA256)
                    .filter(|hash| is_sha256(hash))
                    .cloned()
                else {
                    continue;
                };
                if let Some(previous) = accepted_hashes.insert(source_revision, source_hash.clone())
                {
                    if previous != source_hash {
                        return Err(format!(
                            "accepted {side:?} revision {source_revision} has multiple immutable source views; preserve both inputs before resuming"
                        ));
                    }
                }
            }

            for source_hash in accepted_hashes.into_values() {
                let local_keys = match side {
                    DeviceSide::Boox => vec![canonical_path_key(&document.boox_pdf)],
                    DeviceSide::Supernote => supernote_files
                        .iter()
                        .filter(|(hash, _)| hash == &source_hash)
                        .map(|(_, key)| key.clone())
                        .collect(),
                };
                for local_key in local_keys {
                    let side_state = state.document_mut(&document.document_id).side_mut(side);
                    side_state
                        .uploaded_local_hashes
                        .insert(local_key.clone(), source_hash.clone());
                    side_state
                        .accepted_local_hashes
                        .insert(local_key, source_hash.clone());
                }
            }
        }
        Ok(())
    }

    fn deliver_outputs(
        &self,
        document: &DocumentFolders,
        state: &mut TransportState,
        report: &mut SyncReport,
    ) -> Result<(), String> {
        let mut candidates = Vec::new();
        candidates.extend(
            self.cloud
                .list(&format!(
                    "Supernote_Folder/{}/incoming/",
                    document.document_id
                ))?
                .into_iter()
                .map(|object| (DeviceSide::Supernote, object)),
        );
        candidates.extend(
            self.cloud
                .list(&format!("BOOX_Folder/{}/", document.document_id))?
                .into_iter()
                .map(|object| (DeviceSide::Boox, object)),
        );
        candidates.retain(|(_, object)| {
            object
                .metadata
                .get(GENERATED_BY)
                .is_some_and(|producer| producer == BROKER_PRODUCER)
                && object
                    .metadata
                    .get(DOCUMENT_ID)
                    .is_some_and(|id| id == &document.document_id)
        });
        candidates.sort_by_key(|(_, object)| object.generation);

        for (side, object) in candidates {
            let generation_key = object.generation_key();
            if state
                .document_mut(&document.document_id)
                .delivered_generations
                .contains(&generation_key)
            {
                continue;
            }
            let revisions = parse_revision_metadata(&object)?;
            let current = state.document_mut(&document.document_id).revisions;
            if !dominates(revisions, current) {
                state
                    .document_mut(&document.document_id)
                    .delivered_generations
                    .insert(generation_key);
                continue;
            }

            let local_path = match side {
                DeviceSide::Boox => document.boox_pdf.clone(),
                DeviceSide::Supernote => document.supernote_incoming_directory.join(
                    Path::new(&object.path)
                        .file_name()
                        .ok_or_else(|| format!("remote path {} has no file name", object.path))?,
                ),
            };
            let expected_hash = required_metadata(&object, CONTENT_SHA256)?.to_owned();
            let already_installed = local_path.is_file()
                && sha256_file(&local_path).is_ok_and(|hash| hash == expected_hash);
            if side == DeviceSide::Boox
                && !already_installed
                && self.boox_has_unpublished_local_edit(document, state)?
            {
                report.actions.push(TransportAction::Deferred {
                    side,
                    reason: format!(
                        "broker view {} was not installed because the local BOOX PDF has an unpublished edit",
                        object.path
                    ),
                });
                continue;
            }

            if !already_installed {
                self.download_verified(&object, &local_path)?;
            }
            remember_file_hash(&local_path, state, SystemTime::now(), &expected_hash)?;
            let document_state = state.document_mut(&document.document_id);
            document_state.revisions = revisions;
            document_state.side_mut(side).delivered_content_sha256 = Some(expected_hash);
            for source in [DeviceSide::Boox, DeviceSide::Supernote] {
                let accepted_revision = revisions.get(source);
                let accepted_pending = document_state
                    .side(source)
                    .pending
                    .as_ref()
                    .is_some_and(|pending| pending.source_revision <= accepted_revision)
                    .then(|| document_state.side_mut(source).pending.take())
                    .flatten();
                if let Some(pending) = accepted_pending {
                    document_state
                        .side_mut(source)
                        .accepted_local_hashes
                        .insert(pending.local_path, pending.local_content_sha256);
                }
            }
            document_state.delivered_generations.insert(generation_key);
            report.actions.push(TransportAction::Delivered {
                side,
                object_path: object.path,
                local_path,
                generation: object.generation,
            });
        }
        Ok(())
    }

    fn upload_boox_if_ready(
        &self,
        document: &DocumentFolders,
        state: &mut TransportState,
        now: SystemTime,
        report: &mut SyncReport,
    ) -> Result<(), String> {
        if !document.boox_pdf.is_file() {
            return Ok(());
        }
        if state
            .document_mut(&document.document_id)
            .boox
            .pending
            .is_some()
        {
            return Ok(());
        }
        if !file_is_settled(&document.boox_pdf, state, now, self.settle)? {
            return Ok(());
        }
        let local_key = canonical_path_key(&document.boox_pdf);
        let cached_source_hash = state
            .observations
            .get(&local_key)
            .and_then(|observation| observation.content_sha256.clone());
        if cached_source_hash.is_some() {
            // Size and mtime establish that a file has settled, but they are not
            // a content identity: sync tools can replace a file while preserving
            // both. Verify the bytes before suppressing a potentially large BOOX
            // conversion as already delivered or uploaded.
            let source_hash = sha256_file(&document.boox_pdf)?;
            state
                .observations
                .get_mut(&local_key)
                .ok_or_else(|| {
                    format!(
                        "settled observation disappeared for {}",
                        document.boox_pdf.display()
                    )
                })?
                .content_sha256 = Some(source_hash.clone());
            let side = &state.document_mut(&document.document_id).boox;
            if side.delivered_content_sha256.as_deref() == Some(source_hash.as_str())
                || side
                    .uploaded_local_hashes
                    .get(&local_key)
                    .is_some_and(|hash| hash == &source_hash)
            {
                return Ok(());
            }
        }
        let baseline_candidates = supernote_export_files(&document.supernote_export_directory)?;
        let accepted = &state
            .document_mut(&document.document_id)
            .supernote
            .accepted_local_hashes;
        let mut baselines = Vec::new();
        let mut baseline_hashes = Vec::new();
        let mut unaccepted = Vec::new();
        for candidate in baseline_candidates {
            let key = canonical_path_key(&candidate);
            let hash = sha256_file(&candidate)?;
            if accepted.get(&key).is_some_and(|accepted| accepted == &hash) {
                baseline_hashes.push((candidate.clone(), hash));
                baselines.push(candidate);
            } else {
                unaccepted.push(candidate);
            }
        }
        if baselines.is_empty() || !unaccepted.is_empty() {
            report.actions.push(TransportAction::Deferred {
                side: DeviceSide::Boox,
                reason: "BOOX edit is ready, but the current Supernote native exports have not all been accepted as its identity baseline".to_owned(),
            });
            return Ok(());
        }
        validate_unique_baseline_pages(&baselines)?;
        let built = self
            .manifest_builder
            .build(&document.boox_pdf, &baselines)?;
        if !file_is_settled(&document.boox_pdf, state, now, self.settle)? {
            report.actions.push(TransportAction::Deferred {
                side: DeviceSide::Boox,
                reason: "the BOOX PDF changed while its compact manifest was being built"
                    .to_owned(),
            });
            return Ok(());
        }
        let post_build_hash = sha256_file(&document.boox_pdf)?;
        if post_build_hash != built.source_pdf_sha256 {
            remember_content_change(&document.boox_pdf, state, now, &post_build_hash)?;
            report.actions.push(TransportAction::Deferred {
                side: DeviceSide::Boox,
                reason: "the BOOX PDF content changed while its compact manifest was being built"
                    .to_owned(),
            });
            return Ok(());
        }
        let current_baselines = supernote_export_files(&document.supernote_export_directory)?;
        if current_baselines != baselines {
            report.actions.push(TransportAction::Deferred {
                side: DeviceSide::Boox,
                reason:
                    "the Supernote baseline set changed while the compact manifest was being built"
                        .to_owned(),
            });
            return Ok(());
        }
        for (baseline, accepted_hash) in baseline_hashes {
            let current_hash = sha256_file(&baseline)?;
            if current_hash != accepted_hash {
                remember_content_change(&baseline, state, now, &current_hash)?;
                report.actions.push(TransportAction::Deferred {
                    side: DeviceSide::Boox,
                    reason: format!(
                        "Supernote baseline {} changed while the compact manifest was being built",
                        baseline.display()
                    ),
                });
                return Ok(());
            }
        }
        let source_hash = built.source_pdf_sha256;
        state
            .observations
            .get_mut(&local_key)
            .ok_or_else(|| {
                format!(
                    "settled observation disappeared for {}",
                    document.boox_pdf.display()
                )
            })?
            .content_sha256 = Some(source_hash.clone());
        let side = &state.document_mut(&document.document_id).boox;
        if side.delivered_content_sha256.as_deref() == Some(source_hash.as_str())
            || side
                .uploaded_local_hashes
                .get(&local_key)
                .is_some_and(|hash| hash == &source_hash)
        {
            return Ok(());
        }
        let manifest_bytes = built.bytes;
        let payload_hash = sha256_hex(&manifest_bytes);
        let temporary = sibling_temporary(&document.boox_pdf, "compact-upload");
        fs::write(&temporary, &manifest_bytes)
            .map_err(|error| format!("could not write {}: {error}", temporary.display()))?;
        let result = self.upload_source(
            document,
            state,
            DeviceSide::Boox,
            &temporary,
            &local_key,
            &source_hash,
            &payload_hash,
            "boox_operation_manifest",
            "operations.json",
        );
        let _ = fs::remove_file(&temporary);
        let (object, source_revision) = result?;
        state
            .document_mut(&document.document_id)
            .boox
            .uploaded_local_hashes
            .insert(local_key, source_hash);
        report.actions.push(TransportAction::Uploaded {
            side: DeviceSide::Boox,
            local_path: document.boox_pdf.clone(),
            object_path: object.path,
            source_revision,
            uploaded_bytes: manifest_bytes.len() as u64,
        });
        Ok(())
    }

    fn upload_supernote_if_ready(
        &self,
        document: &DocumentFolders,
        state: &mut TransportState,
        now: SystemTime,
        report: &mut SyncReport,
    ) -> Result<(), String> {
        if state
            .document_mut(&document.document_id)
            .supernote
            .pending
            .is_some()
        {
            return Ok(());
        }
        for export in supernote_export_files(&document.supernote_export_directory)? {
            if !file_is_settled(&export, state, now, self.settle)? {
                continue;
            }
            let bytes = fs::read(&export)
                .map_err(|error| format!("could not read {}: {error}", export.display()))?;
            if !file_is_settled(&export, state, now, self.settle)? {
                continue;
            }
            let parsed = parse_baseline_bytes(&bytes, &export.to_string_lossy())?;
            if parsed.source_file_name.as_deref() != Some(document.original_file_name.as_str()) {
                return Err(format!(
                    "{} targets {:?}, not configured document {}",
                    export.display(),
                    parsed.source_file_name,
                    document.original_file_name
                ));
            }
            let local_key = canonical_path_key(&export);
            let content_hash = sha256_hex(&bytes);
            if state
                .document_mut(&document.document_id)
                .supernote
                .uploaded_local_hashes
                .get(&local_key)
                .is_some_and(|hash| hash == &content_hash)
            {
                continue;
            }
            let temporary = sibling_temporary(&export, "native-upload");
            fs::write(&temporary, &bytes)
                .map_err(|error| format!("could not write {}: {error}", temporary.display()))?;
            let result = self.upload_source(
                document,
                state,
                DeviceSide::Supernote,
                &temporary,
                &local_key,
                &content_hash,
                &content_hash,
                "device_view",
                "json",
            );
            let _ = fs::remove_file(&temporary);
            let (object, source_revision) = result?;
            state
                .document_mut(&document.document_id)
                .supernote
                .uploaded_local_hashes
                .insert(local_key, content_hash);
            report.actions.push(TransportAction::Uploaded {
                side: DeviceSide::Supernote,
                local_path: export,
                object_path: object.path,
                source_revision,
                uploaded_bytes: object.size,
            });
            break;
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn upload_source(
        &self,
        document: &DocumentFolders,
        state: &mut TransportState,
        side: DeviceSide,
        payload_path: &Path,
        local_key: &str,
        local_hash: &str,
        payload_hash: &str,
        payload_kind: &str,
        extension: &str,
    ) -> Result<(CloudObject, u64), String> {
        let document_state = state.document_mut(&document.document_id);
        if !document_state.conflicts.is_empty() {
            return Err(format!(
                "{} has an unresolved broker conflict; preserve both device files before resuming",
                document.document_id
            ));
        }
        let based_on = document_state.revisions;
        let source_revision = based_on.get(side) + 1;
        let side_name = match side {
            DeviceSide::Boox => "boox",
            DeviceSide::Supernote => "supernote",
        };
        let root = match side {
            DeviceSide::Boox => "BOOX_Folder",
            DeviceSide::Supernote => "Supernote_Folder",
        };
        let object_path = format!(
            "{root}/{}/uploads/{side_name}-r{source_revision}-{}.{}",
            document.document_id,
            &local_hash[..16],
            extension
        );
        let metadata = BTreeMap::from([
            (DOCUMENT_ID.to_owned(), document.document_id.clone()),
            (SOURCE_REVISION.to_owned(), source_revision.to_string()),
            (
                "inkbridge-based-on-boox".to_owned(),
                based_on.boox.to_string(),
            ),
            (
                "inkbridge-based-on-supernote".to_owned(),
                based_on.supernote.to_string(),
            ),
            (CONTENT_SHA256.to_owned(), payload_hash.to_owned()),
            ("inkbridge-sync-ready".to_owned(), "true".to_owned()),
            ("inkbridge-payload-kind".to_owned(), payload_kind.to_owned()),
            (SOURCE_VIEW_SHA256.to_owned(), local_hash.to_owned()),
        ]);
        let object = self
            .cloud
            .upload_create(payload_path, &object_path, &metadata)?;
        document_state.side_mut(side).pending = Some(PendingUpload {
            object_path: object_path.clone(),
            generation: object.generation,
            source_revision,
            based_on,
            local_path: local_key.to_owned(),
            local_content_sha256: local_hash.to_owned(),
            payload_content_sha256: payload_hash.to_owned(),
        });
        Ok((object, source_revision))
    }

    fn boox_has_unpublished_local_edit(
        &self,
        document: &DocumentFolders,
        state: &mut TransportState,
    ) -> Result<bool, String> {
        let document_state = state.document_mut(&document.document_id);
        if document_state.boox.pending.is_some() {
            return Ok(true);
        }
        if !document.boox_pdf.is_file() {
            return Ok(false);
        }
        let hash = sha256_file(&document.boox_pdf)?;
        if format!("inkbridge-doc-v1-{hash}") == document.document_id {
            // A pristine immutable-original copy is a safe first destination.
            // Without this identity check an existing unknown PDF is never
            // overwritten merely because the adapter has no prior state.
            return Ok(false);
        }
        let key = canonical_path_key(&document.boox_pdf);
        Ok(
            document_state.boox.delivered_content_sha256.as_deref() != Some(hash.as_str())
                && document_state
                    .boox
                    .uploaded_local_hashes
                    .get(&key)
                    .is_none_or(|uploaded| uploaded != &hash),
        )
    }

    fn download_verified(&self, object: &CloudObject, destination: &Path) -> Result<(), String> {
        let parent = destination.parent().unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent)
            .map_err(|error| format!("could not create {}: {error}", parent.display()))?;
        let temporary = sibling_temporary(destination, &format!("g{}", object.generation));
        if temporary.exists() {
            fs::remove_file(&temporary)
                .map_err(|error| format!("could not remove {}: {error}", temporary.display()))?;
        }
        self.cloud.download(object, &temporary)?;
        let expected_hash = required_metadata(object, CONTENT_SHA256)?;
        let actual_hash = sha256_file(&temporary)?;
        if actual_hash != expected_hash {
            let _ = fs::remove_file(&temporary);
            return Err(format!(
                "downloaded {} hash {actual_hash} does not match broker metadata {expected_hash}",
                object.path
            ));
        }
        replace_file(&temporary, destination)
    }
}

fn required_metadata<'a>(object: &'a CloudObject, key: &str) -> Result<&'a str, String> {
    object
        .metadata
        .get(key)
        .map(String::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("broker output {} is missing {key}", object.path))
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn parse_revision_metadata(object: &CloudObject) -> Result<RevisionPair, String> {
    let value = required_metadata(object, SOURCE_REVISIONS)?;
    let (boox, supernote) = value
        .split_once(':')
        .ok_or_else(|| format!("invalid broker revision metadata {value}"))?;
    Ok(RevisionPair {
        boox: boox
            .parse()
            .map_err(|error| format!("invalid BOOX revision: {error}"))?,
        supernote: supernote
            .parse()
            .map_err(|error| format!("invalid Supernote revision: {error}"))?,
    })
}

fn dominates(candidate: RevisionPair, current: RevisionPair) -> bool {
    candidate.boox >= current.boox && candidate.supernote >= current.supernote
}

fn supernote_export_files(directory: &Path) -> Result<Vec<PathBuf>, String> {
    if !directory.exists() {
        return Ok(Vec::new());
    }
    let mut files = fs::read_dir(directory)
        .map_err(|error| format!("could not read {}: {error}", directory.display()))?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_file()
                && path
                    .extension()
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("json"))
                && !path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with('.') || name.ends_with(".part.json"))
        })
        .collect::<Vec<_>>();
    files.sort();
    Ok(files)
}

fn file_is_settled(
    path: &Path,
    state: &mut TransportState,
    now: SystemTime,
    settle: Duration,
) -> Result<bool, String> {
    let metadata = fs::metadata(path)
        .map_err(|error| format!("could not inspect {}: {error}", path.display()))?;
    let modified = metadata
        .modified()
        .map_err(|error| format!("could not read mtime for {}: {error}", path.display()))?;
    let modified_millis = unix_millis(modified)?;
    let now_millis = unix_millis(now)?;
    let key = canonical_path_key(path);
    let observation = state.observations.entry(key).or_insert(FileObservation {
        size: metadata.len(),
        modified_unix_millis: modified_millis,
        first_seen_unix_millis: now_millis,
        content_sha256: None,
    });
    if observation.size != metadata.len() || observation.modified_unix_millis != modified_millis {
        *observation = FileObservation {
            size: metadata.len(),
            modified_unix_millis: modified_millis,
            first_seen_unix_millis: now_millis,
            content_sha256: None,
        };
        return Ok(false);
    }
    let quiet_since = observation.first_seen_unix_millis.max(modified_millis);
    Ok(now_millis.saturating_sub(quiet_since) >= settle.as_millis() as u64)
}

fn remember_file_hash(
    path: &Path,
    state: &mut TransportState,
    now: SystemTime,
    hash: &str,
) -> Result<(), String> {
    let metadata = fs::metadata(path)
        .map_err(|error| format!("could not inspect {}: {error}", path.display()))?;
    let key = canonical_path_key(path);
    state.observations.insert(
        key,
        FileObservation {
            size: metadata.len(),
            modified_unix_millis: unix_millis(metadata.modified().map_err(|error| {
                format!("could not read mtime for {}: {error}", path.display())
            })?)?,
            first_seen_unix_millis: unix_millis(now)?,
            content_sha256: Some(hash.to_owned()),
        },
    );
    Ok(())
}

fn remember_content_change(
    path: &Path,
    state: &mut TransportState,
    now: SystemTime,
    hash: &str,
) -> Result<(), String> {
    let metadata = fs::metadata(path)
        .map_err(|error| format!("could not inspect {}: {error}", path.display()))?;
    state.observations.insert(
        canonical_path_key(path),
        FileObservation {
            size: metadata.len(),
            modified_unix_millis: unix_millis(metadata.modified().map_err(|error| {
                format!("could not read mtime for {}: {error}", path.display())
            })?)?,
            first_seen_unix_millis: unix_millis(now)?,
            content_sha256: Some(hash.to_owned()),
        },
    );
    Ok(())
}

fn validate_unique_baseline_pages(paths: &[PathBuf]) -> Result<(), String> {
    let mut pages = BTreeMap::<u32, &Path>::new();
    for path in paths {
        let bytes = fs::read(path)
            .map_err(|error| format!("could not read {}: {error}", path.display()))?;
        let export = parse_baseline_bytes(&bytes, &path.to_string_lossy())?;
        if let Some(previous) = pages.insert(export.page_index, path) {
            return Err(format!(
                "Supernote outgoing folder has more than one accepted export for page {}: {} and {}",
                export.page_index + 1,
                previous.display(),
                path.display()
            ));
        }
    }
    Ok(())
}

fn unix_millis(value: SystemTime) -> Result<u64, String> {
    value
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .map_err(|_| "filesystem timestamp predates the Unix epoch".to_owned())
}

fn canonical_path_key(path: &Path) -> String {
    let value = path
        .canonicalize()
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .replace('\\', "/");
    if cfg!(windows) {
        value.to_lowercase()
    } else {
        value
    }
}

fn sha256_file(path: &Path) -> Result<String, String> {
    let file =
        File::open(path).map_err(|error| format!("could not read {}: {error}", path.display()))?;
    let mut reader = BufReader::with_capacity(1024 * 1024, file);
    let mut digest = Sha256::new();
    let mut buffer = vec![0_u8; 1024 * 1024];
    loop {
        let count = reader
            .read(&mut buffer)
            .map_err(|error| format!("could not read {}: {error}", path.display()))?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn sibling_temporary(path: &Path, suffix: &str) -> PathBuf {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("inkbridge");
    path.with_file_name(format!(".{name}.{suffix}.part"))
}

fn replace_file(source: &Path, destination: &Path) -> Result<(), String> {
    if !destination.exists() {
        return fs::rename(source, destination).map_err(|error| {
            format!(
                "could not publish {} as {}: {error}",
                source.display(),
                destination.display()
            )
        });
    }
    let backup = sibling_temporary(destination, "previous");
    if backup.exists() {
        fs::remove_file(&backup)
            .map_err(|error| format!("could not remove {}: {error}", backup.display()))?;
    }
    fs::rename(destination, &backup).map_err(|error| {
        format!(
            "could not stage {} as {}: {error}",
            destination.display(),
            backup.display()
        )
    })?;
    if let Err(error) = fs::rename(source, destination) {
        let _ = fs::rename(&backup, destination);
        return Err(format!(
            "could not publish {} as {}: {error}",
            source.display(),
            destination.display()
        ));
    }
    fs::remove_file(backup).map_err(|error| format!("could not retire old file: {error}"))
}

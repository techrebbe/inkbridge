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

#[derive(Clone, Debug, PartialEq, Eq)]
enum FileIdentity {
    Missing,
    Sha256(String),
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
            if side == DeviceSide::Boox {
                reconcile_staged_backup(&local_path, &expected_hash)?;
            }
            let boox_destination_identity = (side == DeviceSide::Boox)
                .then(|| file_identity(&local_path))
                .transpose()?;
            let already_installed = if let Some(identity) = &boox_destination_identity {
                identity == &FileIdentity::Sha256(expected_hash.clone())
            } else {
                metadata_if_exists(&local_path)?.is_some_and(|metadata| metadata.is_file())
                    && sha256_file(&local_path).is_ok_and(|hash| hash == expected_hash)
            };
            if side == DeviceSide::Boox
                && !already_installed
                && self.boox_has_unpublished_local_edit(
                    document,
                    state,
                    boox_destination_identity
                        .as_ref()
                        .expect("BOOX destinations are inspected above"),
                )?
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

            if !already_installed
                && !self.download_verified(
                    &object,
                    &local_path,
                    boox_destination_identity.as_ref(),
                )?
            {
                report.actions.push(TransportAction::Deferred {
                    side,
                    reason: format!(
                        "broker view {} was not installed because the local BOOX PDF changed while it was downloading",
                        object.path
                    ),
                });
                continue;
            }
            if side == DeviceSide::Boox
                && file_identity(&local_path)? != FileIdentity::Sha256(expected_hash.clone())
            {
                report.actions.push(TransportAction::Deferred {
                    side,
                    reason: format!(
                        "broker view {} was not acknowledged because the local BOOX PDF changed during inspection",
                        object.path
                    ),
                });
                continue;
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
        match metadata_if_exists(&document.boox_pdf)? {
            None => return Ok(()),
            Some(metadata) if !metadata.is_file() => {
                return Err(format!(
                    "configured BOOX path {} is not a regular file",
                    document.boox_pdf.display()
                ));
            }
            Some(_) => {}
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
        if let Some(cached_source_hash) = cached_source_hash {
            // Size and mtime establish that a file has settled, but they are not
            // a content identity: sync tools can replace a file while preserving
            // both. Verify the bytes before suppressing a potentially large BOOX
            // conversion as already delivered or uploaded.
            let source_hash = sha256_file(&document.boox_pdf)?;
            if source_hash != cached_source_hash {
                remember_content_change(
                    &document.boox_pdf,
                    state,
                    SystemTime::now(),
                    &source_hash,
                )?;
                report.actions.push(TransportAction::Deferred {
                    side: DeviceSide::Boox,
                    reason: "the settled BOOX PDF content changed without new filesystem metadata"
                        .to_owned(),
                });
                return Ok(());
            }
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
        let current_baseline_keys = baseline_candidates
            .iter()
            .map(|candidate| canonical_path_key(candidate))
            .collect::<std::collections::BTreeSet<_>>();
        let missing_accepted = accepted
            .keys()
            .filter(|key| !current_baseline_keys.contains(*key))
            .cloned()
            .collect::<Vec<_>>();
        if !missing_accepted.is_empty() {
            report.actions.push(TransportAction::Deferred {
                side: DeviceSide::Boox,
                reason: format!(
                    "BOOX edit is ready, but accepted Supernote baseline files are missing: {}",
                    missing_accepted.join(", ")
                ),
            });
            return Ok(());
        }
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
            remember_content_change(
                &document.boox_pdf,
                state,
                SystemTime::now(),
                &post_build_hash,
            )?;
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
                remember_content_change(&baseline, state, SystemTime::now(), &current_hash)?;
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
            let local_key = canonical_path_key(&export);
            let observed_hash = state
                .observations
                .get(&local_key)
                .and_then(|observation| observation.content_sha256.clone());
            let (bytes, content_hash, post_read_hash) =
                read_snapshot_and_current_hash(&export, |path| {
                    fs::read(path)
                        .map_err(|error| format!("could not read {}: {error}", path.display()))
                })?;
            if !file_is_settled(&export, state, now, self.settle)? {
                continue;
            }
            if post_read_hash != content_hash {
                remember_content_change(&export, state, SystemTime::now(), &post_read_hash)?;
                report.actions.push(TransportAction::Deferred {
                    side: DeviceSide::Supernote,
                    reason: format!(
                        "Supernote export {} changed while its upload snapshot was being read",
                        export.display()
                    ),
                });
                return Ok(());
            }
            if observed_hash
                .as_deref()
                .is_some_and(|observed| observed != content_hash)
            {
                remember_content_change(&export, state, SystemTime::now(), &post_read_hash)?;
                report.actions.push(TransportAction::Deferred {
                    side: DeviceSide::Supernote,
                    reason: format!(
                        "settled Supernote export {} changed without new filesystem metadata",
                        export.display()
                    ),
                });
                return Ok(());
            }
            state
                .observations
                .get_mut(&local_key)
                .ok_or_else(|| format!("settled observation disappeared for {}", export.display()))?
                .content_sha256 = Some(content_hash.clone());
            let parsed = parse_baseline_bytes(&bytes, &export.to_string_lossy())?;
            if parsed.source_file_name.as_deref() != Some(document.original_file_name.as_str()) {
                return Err(format!(
                    "{} targets {:?}, not configured document {}",
                    export.display(),
                    parsed.source_file_name,
                    document.original_file_name
                ));
            }
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
        identity: &FileIdentity,
    ) -> Result<bool, String> {
        let document_state = state.document_mut(&document.document_id);
        if document_state.boox.pending.is_some() {
            return Ok(true);
        }
        let FileIdentity::Sha256(hash) = identity else {
            return Ok(false);
        };
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
                    .is_none_or(|uploaded| uploaded != hash),
        )
    }

    fn download_verified(
        &self,
        object: &CloudObject,
        destination: &Path,
        expected_destination: Option<&FileIdentity>,
    ) -> Result<bool, String> {
        let parent = destination.parent().unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent)
            .map_err(|error| format!("could not create {}: {error}", parent.display()))?;
        let temporary = sibling_temporary(destination, &format!("g{}", object.generation));
        remove_file_if_exists(&temporary)?;
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
        replace_file(&temporary, destination, expected_destination)
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
    match metadata_if_exists(directory)? {
        None => return Ok(Vec::new()),
        Some(metadata) if !metadata.is_dir() => {
            return Err(format!(
                "configured Supernote export path {} is not a directory",
                directory.display()
            ));
        }
        Some(_) => {}
    }
    let entries = fs::read_dir(directory)
        .map_err(|error| format!("could not read {}: {error}", directory.display()))?;
    let mut files = Vec::new();
    for entry in entries {
        let path = entry
            .map_err(|error| {
                format!(
                    "could not enumerate an entry in {}: {error}",
                    directory.display()
                )
            })?
            .path();
        let is_json = path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("json"));
        let is_temporary = path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with('.') || name.ends_with(".part.json"));
        if !is_json || is_temporary {
            continue;
        }
        let metadata = fs::metadata(&path).map_err(|error| {
            format!(
                "could not inspect candidate export {} from {}: {error}",
                path.display(),
                directory.display()
            )
        })?;
        if metadata.is_file() {
            files.push(path);
        }
    }
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

fn file_identity(path: &Path) -> Result<FileIdentity, String> {
    match metadata_if_exists(path)? {
        None => Ok(FileIdentity::Missing),
        Some(metadata) if !metadata.is_file() => Err(format!(
            "configured BOOX path {} is not a regular file",
            path.display()
        )),
        Some(_) => sha256_file(path).map(FileIdentity::Sha256),
    }
}

fn metadata_if_exists(path: &Path) -> Result<Option<fs::Metadata>, String> {
    match fs::metadata(path) {
        Ok(metadata) => Ok(Some(metadata)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("could not inspect {}: {error}", path.display())),
    }
}

fn remove_file_if_exists(path: &Path) -> Result<(), String> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("could not remove {}: {error}", path.display())),
    }
}

fn read_snapshot_and_current_hash<F>(
    path: &Path,
    read: F,
) -> Result<(Vec<u8>, String, String), String>
where
    F: FnOnce(&Path) -> Result<Vec<u8>, String>,
{
    let bytes = read(path)?;
    let content_hash = sha256_hex(&bytes);
    let current_hash = sha256_file(path)?;
    Ok((bytes, content_hash, current_hash))
}

fn sibling_temporary(path: &Path, suffix: &str) -> PathBuf {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("inkbridge");
    path.with_file_name(format!(".{name}.{suffix}.part"))
}

fn replace_file(
    source: &Path,
    destination: &Path,
    expected_destination: Option<&FileIdentity>,
) -> Result<bool, String> {
    let Some(expected_destination) = expected_destination else {
        replace_file_unconditionally(source, destination)?;
        return Ok(true);
    };
    match expected_destination {
        FileIdentity::Missing => publish_create_only(source, destination),
        FileIdentity::Sha256(expected_hash) => {
            replace_existing_file_conditionally(source, destination, expected_hash)
        }
    }
}

fn replace_existing_file_conditionally(
    source: &Path,
    destination: &Path,
    expected_hash: &str,
) -> Result<bool, String> {
    match metadata_if_exists(destination)? {
        None => {
            remove_file_if_exists(source)?;
            return Ok(false);
        }
        Some(metadata) if !metadata.is_file() => {
            return Err(format!(
                "refusing to replace non-file destination {}",
                destination.display()
            ));
        }
        Some(_) => {}
    }
    let backup = sibling_temporary(destination, "previous");
    if metadata_if_exists(&backup)?.is_some() {
        return Err(format!(
            "refusing to replace {} while staged backup {} already exists",
            destination.display(),
            backup.display()
        ));
    }
    fs::rename(destination, &backup).map_err(|error| {
        format!(
            "could not stage {} as {}: {error}",
            destination.display(),
            backup.display()
        )
    })?;
    let staged_hash = match sha256_file(&backup) {
        Ok(hash) => hash,
        Err(error) => {
            let restore_error = restore_staged_file(&backup, destination).err();
            return Err(match restore_error {
                Some(restore_error) => {
                    format!("{error}; additionally could not restore staged file: {restore_error}")
                }
                None => error,
            });
        }
    };
    if staged_hash != expected_hash {
        remove_file_if_exists(source)?;
        restore_staged_file(&backup, destination)?;
        return Ok(false);
    }
    match publish_create_only(source, destination) {
        Ok(published) => {
            remove_file_if_exists(&backup)?;
            Ok(published)
        }
        Err(error) => {
            let restore_error = restore_staged_file(&backup, destination).err();
            Err(match restore_error {
                Some(restore_error) => {
                    format!("{error}; additionally could not restore staged file: {restore_error}")
                }
                None => error,
            })
        }
    }
}

fn publish_create_only(source: &Path, destination: &Path) -> Result<bool, String> {
    match rename_create_only(source, destination) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            remove_file_if_exists(source)?;
            Ok(false)
        }
        Err(error) => Err(format!(
            "could not publish {} as new destination {}: {error}",
            source.display(),
            destination.display()
        )),
    }
}

#[cfg(windows)]
fn rename_create_only(source: &Path, destination: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::MoveFileW;

    let source = source
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let destination = destination
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    // SAFETY: Both pointers reference NUL-terminated UTF-16 buffers for the
    // duration of the call. MoveFileW has create-only destination semantics.
    if unsafe { MoveFileW(source.as_ptr(), destination.as_ptr()) } == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(any(
    target_os = "linux",
    target_os = "android",
    target_os = "macos",
    target_os = "ios",
    target_os = "redox"
))]
fn rename_create_only(source: &Path, destination: &Path) -> std::io::Result<()> {
    use rustix::fs::{renameat_with, RenameFlags, CWD};

    renameat_with(CWD, source, CWD, destination, RenameFlags::NOREPLACE).map_err(Into::into)
}

#[cfg(not(any(
    windows,
    target_os = "linux",
    target_os = "android",
    target_os = "macos",
    target_os = "ios",
    target_os = "redox"
)))]
fn rename_create_only(_source: &Path, _destination: &Path) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "atomic no-replace rename is not available on this platform",
    ))
}

fn reconcile_staged_backup(destination: &Path, published_hash: &str) -> Result<(), String> {
    let backup = sibling_temporary(destination, "previous");
    match metadata_if_exists(&backup)? {
        None => return Ok(()),
        Some(metadata) if !metadata.is_file() => {
            return Err(format!(
                "staged BOOX backup {} is not a regular file",
                backup.display()
            ));
        }
        Some(_) => {}
    }
    match file_identity(destination)? {
        FileIdentity::Missing => restore_staged_file(&backup, destination),
        FileIdentity::Sha256(current_hash) if current_hash == published_hash => {
            remove_file_if_exists(&backup)
        }
        FileIdentity::Sha256(_) => Err(format!(
            "both BOOX destination {} and interrupted staged backup {} contain distinct data; preserved both",
            destination.display(),
            backup.display()
        )),
    }
}

fn restore_staged_file(backup: &Path, destination: &Path) -> Result<(), String> {
    if metadata_if_exists(destination)?.is_some() {
        return Err(format!(
            "destination {} was recreated; preserved staged file at {}",
            destination.display(),
            backup.display()
        ));
    }
    fs::rename(backup, destination).map_err(|error| {
        format!(
            "could not restore {} as {}: {error}",
            backup.display(),
            destination.display()
        )
    })
}

fn replace_file_unconditionally(source: &Path, destination: &Path) -> Result<(), String> {
    match metadata_if_exists(destination)? {
        None => {
            return fs::rename(source, destination).map_err(|error| {
                format!(
                    "could not publish {} as {}: {error}",
                    source.display(),
                    destination.display()
                )
            });
        }
        Some(metadata) if !metadata.is_file() => {
            return Err(format!(
                "refusing to replace non-file destination {}",
                destination.display()
            ));
        }
        Some(_) => {}
    }
    let backup = sibling_temporary(destination, "previous");
    remove_file_if_exists(&backup)?;
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

#[cfg(test)]
mod snapshot_tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn snapshot_hash_detects_same_length_replacement_after_read() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("page.json");
        fs::write(&path, b"first-export").unwrap();

        let (bytes, content_hash, current_hash) = read_snapshot_and_current_hash(&path, |path| {
            let bytes = fs::read(path).map_err(|error| error.to_string())?;
            fs::write(path, b"later-export").map_err(|error| error.to_string())?;
            Ok(bytes)
        })
        .unwrap();

        assert_eq!(bytes, b"first-export");
        assert_ne!(content_hash, current_hash);
        assert_eq!(current_hash, sha256_hex(b"later-export"));
    }

    #[test]
    fn conditional_replace_restores_a_destination_changed_before_staging() {
        let directory = tempdir().unwrap();
        let source = directory.path().join("broker.part");
        let destination = directory.path().join("book.pdf");
        fs::write(&source, b"broker view").unwrap();
        fs::write(&destination, b"local edit").unwrap();

        assert!(!replace_file(
            &source,
            &destination,
            Some(&FileIdentity::Sha256(sha256_hex(b"old broker view"))),
        )
        .unwrap());
        assert_eq!(fs::read(&destination).unwrap(), b"local edit");
        assert!(!source.exists());
        assert!(!sibling_temporary(&destination, "previous").exists());
    }

    #[test]
    fn create_only_publish_never_overwrites_a_recreated_destination() {
        let directory = tempdir().unwrap();
        let source = directory.path().join("broker.part");
        let destination = directory.path().join("book.pdf");
        fs::write(&source, b"broker view").unwrap();
        fs::write(&destination, b"new local edit").unwrap();

        assert!(!replace_file(&source, &destination, Some(&FileIdentity::Missing),).unwrap());
        assert_eq!(fs::read(&destination).unwrap(), b"new local edit");
        assert!(!source.exists());
    }

    #[test]
    fn interrupted_staged_backup_is_restored_when_destination_is_missing() {
        let directory = tempdir().unwrap();
        let destination = directory.path().join("book.pdf");
        let backup = sibling_temporary(&destination, "previous");
        fs::write(&backup, b"local file staged before crash").unwrap();

        reconcile_staged_backup(&destination, &sha256_hex(b"broker view")).unwrap();

        assert_eq!(
            fs::read(&destination).unwrap(),
            b"local file staged before crash"
        );
        assert!(!backup.exists());
    }

    #[test]
    fn completed_publication_retires_an_interrupted_staged_backup() {
        let directory = tempdir().unwrap();
        let destination = directory.path().join("book.pdf");
        let backup = sibling_temporary(&destination, "previous");
        let published = b"broker view";
        fs::write(&destination, published).unwrap();
        fs::write(&backup, b"previous broker view").unwrap();

        reconcile_staged_backup(&destination, &sha256_hex(published)).unwrap();

        assert_eq!(fs::read(&destination).unwrap(), published);
        assert!(!backup.exists());
    }
}

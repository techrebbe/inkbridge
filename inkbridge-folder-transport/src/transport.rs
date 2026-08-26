use crate::supernote_snapshot::{
    accepted_snapshot_path as supernote_accepted_snapshot_path,
    materialize_non_overlapping_baselines as materialize_non_overlapping_supernote_baselines,
    object_page_indices as supernote_object_page_indices, page_identity as supernote_page_identity,
    persist_snapshot_bytes as persist_supernote_snapshot_bytes,
    snapshot_identity as supernote_snapshot_identity, SOURCE_PAGE_INDEX, SOURCE_PAGE_INDICES,
};
use crate::{
    boox_handoff::{
        BooxHandoffEndpoint, FinalizedBooxArtifact, InstalledBooxDelivery, MAX_DESCRIPTOR_BYTES,
    },
    conflicts::unresolved_conflict_groups,
    CloudFolder, CloudObject, DocumentFolders, DocumentTransportState, FileObservation,
    PendingUpload, SyncReport, TransportAction, TransportState, VerifiedBooxInstall,
};
use inkbridge_broker::{sha256_hex, DevicePayloadKind, DeviceSide, RevisionPair, BROKER_PRODUCER};
use inkbridge_convert::{
    build_manifest, parse_baseline_bytes, serialize_baseline_export, BaselineExport,
    BaselineRevisions, Manifest,
};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const GENERATED_BY: &str = "inkbridge-generated-by";
const GENERATED_EVENT_ID: &str = "inkbridge-event-id";
const DOCUMENT_ID: &str = "inkbridge-document-id";
const SOURCE_REVISIONS: &str = "inkbridge-source-revisions";
const SOURCE_REVISION: &str = "inkbridge-source-revision";
const SOURCE_VIEW_SHA256: &str = "inkbridge-source-view-sha256";
const SOURCE_LOCAL_ID: &str = "inkbridge-source-local-id";
const CONTENT_SHA256: &str = "inkbridge-content-sha256";
const RECOVERED_MISSING_PREFIX: &str = "inkbridge-missing-accepted://";
const MAX_SUPERNOTE_MANIFEST_BYTES: u64 = 16 * 1024 * 1024;
const MAX_BOOX_OPERATION_MANIFEST_BYTES: u64 = 64 * 1024 * 1024;
const MAX_SUPERNOTE_ACK_BYTES: u64 = 256 * 1024;

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
    boox_handoff_root: Option<PathBuf>,
}

impl<'a, C: CloudFolder, B: BooxManifestBuilder> FolderTransport<'a, C, B> {
    pub fn new(cloud: &'a C, manifest_builder: &'a B, settle: Duration) -> Self {
        Self {
            cloud,
            manifest_builder,
            settle,
            boox_handoff_root: None,
        }
    }

    pub fn with_boox_handoff_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.boox_handoff_root = Some(root.into());
        self
    }

    fn boox_handoff_endpoint(
        &self,
        document: &DocumentFolders,
    ) -> Result<Option<BooxHandoffEndpoint>, String> {
        self.boox_handoff_root
            .as_deref()
            .map(|root| BooxHandoffEndpoint::new(root, document))
            .transpose()
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
        let current = unresolved_conflict_groups(&objects, &document.document_id);
        state
            .document_mut(&document.document_id)
            .conflicts
            .clone_from(&current);
        for group_path in &current {
            if !previous.contains(group_path) {
                report.actions.push(TransportAction::Conflict {
                    object_path: group_path.clone(),
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
            let side_state = state.document_mut(&document.document_id).side_mut(side);
            if accepted_revision == 0 {
                side_state.accepted_source_revisions.clear();
            } else {
                side_state
                    .accepted_source_revisions
                    .retain(|_, revision| *revision <= accepted_revision);
            }
            if accepted_revision == 0 {
                continue;
            }
            let root = match side {
                DeviceSide::Boox => "BOOX_Folder",
                DeviceSide::Supernote => "Supernote_Folder",
            };
            let mut accepted_hashes = BTreeMap::<String, (u64, String, CloudObject)>::new();
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
                let source_local_id = match side {
                    DeviceSide::Boox => object
                        .metadata
                        .get(SOURCE_LOCAL_ID)
                        .filter(|identity| is_sha256(identity))
                        .cloned()
                        .unwrap_or_else(|| format!("legacy-revision-{source_revision}")),
                    DeviceSide::Supernote => {
                        self.recover_supernote_source_identity(document, state, &object)?
                    }
                };
                if let Some((previous_revision, previous_hash, _)) =
                    accepted_hashes.get(&source_local_id)
                {
                    if *previous_revision == source_revision && previous_hash != &source_hash {
                        return Err(format!(
                            "accepted {side:?} revision {source_revision} has multiple immutable source views; preserve both inputs before resuming"
                        ));
                    }
                }
                if accepted_hashes
                    .get(&source_local_id)
                    .is_none_or(|(revision, _, _)| *revision <= source_revision)
                {
                    accepted_hashes.insert(source_local_id, (source_revision, source_hash, object));
                }
            }

            if side == DeviceSide::Supernote {
                state
                    .document_mut(&document.document_id)
                    .supernote
                    .accepted_local_hashes
                    .clear();
            }
            for (source_local_id, (source_revision, source_hash, object)) in accepted_hashes {
                let entry = state
                    .document_mut(&document.document_id)
                    .side_mut(side)
                    .accepted_source_revisions
                    .entry(source_local_id.clone())
                    .or_insert(source_revision);
                *entry = (*entry).max(source_revision);
                if side == DeviceSide::Supernote {
                    if let Some(page_indices) = supernote_object_page_indices(&object)? {
                        for page_index in page_indices {
                            let page_identity =
                                supernote_page_identity(&document.document_id, page_index);
                            let page_entry = state
                                .document_mut(&document.document_id)
                                .supernote
                                .accepted_source_revisions
                                .entry(page_identity)
                                .or_insert(source_revision);
                            *page_entry = (*page_entry).max(source_revision);
                        }
                    }
                }
                let local_keys = match side {
                    DeviceSide::Boox => vec![canonical_path_key(&document.boox_pdf)],
                    DeviceSide::Supernote => vec![canonical_path_key(
                        &self.ensure_supernote_accepted_snapshot(
                            document,
                            &source_local_id,
                            source_revision,
                            &source_hash,
                            &object,
                        )?,
                    )],
                };
                if side == DeviceSide::Supernote {
                    for (_, current_key) in supernote_files
                        .iter()
                        .filter(|(hash, _)| hash == &source_hash)
                    {
                        state
                            .document_mut(&document.document_id)
                            .supernote
                            .uploaded_local_hashes
                            .insert(current_key.clone(), source_hash.clone());
                    }
                }
                for local_key in local_keys {
                    let side_state = state.document_mut(&document.document_id).side_mut(side);
                    if side == DeviceSide::Boox {
                        side_state
                            .uploaded_local_hashes
                            .insert(local_key.clone(), source_hash.clone());
                    }
                    side_state
                        .accepted_local_hashes
                        .insert(local_key, source_hash.clone());
                }
            }
            if side == DeviceSide::Supernote {
                let accepted = state
                    .document_mut(&document.document_id)
                    .supernote
                    .accepted_local_hashes
                    .clone();
                state
                    .document_mut(&document.document_id)
                    .supernote
                    .accepted_local_hashes =
                    materialize_non_overlapping_supernote_baselines(document, &accepted)?;
            }
        }
        Ok(())
    }

    fn ensure_supernote_accepted_snapshot(
        &self,
        document: &DocumentFolders,
        source_local_id: &str,
        source_revision: u64,
        source_hash: &str,
        object: &CloudObject,
    ) -> Result<PathBuf, String> {
        let destination = supernote_accepted_snapshot_path(
            document,
            source_local_id,
            source_revision,
            source_hash,
        );
        match symlink_metadata_if_exists(&destination)? {
            Some(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
                return Err(format!(
                    "accepted Supernote snapshot {} is not a regular file",
                    destination.display()
                ));
            }
            Some(_) => {
                let actual_hash = sha256_file(&destination)?;
                if actual_hash == source_hash {
                    return Ok(destination);
                }
                // This directory is transport-managed cache data. A corrupt
                // entry has no authority over the immutable accepted cloud
                // generation, so discard it and reinstall the verified bytes.
                remove_file_if_exists(&destination)?;
            }
            None => {}
        }

        let parent = destination.parent().unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent)
            .map_err(|error| format!("could not create {}: {error}", parent.display()))?;
        let temporary = sibling_temporary(&destination, &format!("g{}", object.generation));
        remove_file_if_exists(&temporary)?;
        self.cloud.download(object, &temporary)?;
        let downloaded_hash = sha256_file(&temporary)?;
        if downloaded_hash != source_hash {
            let _ = fs::remove_file(&temporary);
            return Err(format!(
                "accepted Supernote upload {} hash {downloaded_hash} does not match immutable source hash {source_hash}",
                object.path
            ));
        }
        let _ = publish_create_only(&temporary, &destination)?;
        let installed_hash = sha256_file(&destination)?;
        if installed_hash != source_hash {
            return Err(format!(
                "accepted Supernote snapshot {} changed while it was being installed",
                destination.display()
            ));
        }
        Ok(destination)
    }

    fn recover_supernote_source_identity(
        &self,
        document: &DocumentFolders,
        state: &mut TransportState,
        object: &CloudObject,
    ) -> Result<String, String> {
        if let Some(page_indices) = supernote_object_page_indices(object)? {
            return supernote_snapshot_identity(&document.document_id, &page_indices);
        }
        let generation_key = object.generation_key();
        if let Some(identity) = state
            .document_mut(&document.document_id)
            .supernote
            .recovered_source_identities
            .get(&generation_key)
            .filter(|identity| is_sha256(identity))
            .cloned()
        {
            return Ok(identity);
        }

        // Uploads created before page identities were introduced only carry
        // a path-derived source ID. Recover their logical page from the small
        // immutable native-export payload once, then checkpoint the result so
        // normal polling does not repeatedly download legacy objects.
        let temporary = tempfile::NamedTempFile::new()
            .map_err(|error| format!("could not create legacy recovery file: {error}"))?;
        let recovered = (|| {
            self.cloud.download(object, temporary.path())?;
            let bytes = fs::read(temporary.path()).map_err(|error| {
                format!("could not read {}: {error}", temporary.path().display())
            })?;
            let expected_hash = object
                .metadata
                .get(SOURCE_VIEW_SHA256)
                .ok_or_else(|| format!("{} has no source view hash", object.path))?;
            let actual_hash = sha256_hex(&bytes);
            if &actual_hash != expected_hash {
                return Err(format!(
                    "legacy Supernote upload {} content hash {actual_hash} does not match source view hash {expected_hash}",
                    object.path
                ));
            }
            let parsed = parse_baseline_bytes(&bytes, &object.path)?;
            validate_supernote_export_identity(&parsed, document, &object.path)?;
            let page_indices = parsed
                .pages
                .iter()
                .map(|page| page.page_index)
                .collect::<Vec<_>>();
            supernote_snapshot_identity(&document.document_id, &page_indices)
        })();
        let identity = recovered?;
        state
            .document_mut(&document.document_id)
            .supernote
            .recovered_source_identities
            .insert(generation_key, identity.clone());
        Ok(identity)
    }

    fn restore_verified_boox_install_pair(
        &self,
        endpoint: &BooxHandoffEndpoint,
        document: &DocumentFolders,
        receipt: &VerifiedBooxInstall,
        state: &mut TransportState,
    ) -> Result<(), String> {
        let object = verified_boox_install_object(receipt, document)?;
        let delivery = endpoint.prepare_delivery(
            document,
            &object,
            receipt.source_revisions,
            &receipt.content_sha256,
        )?;
        if delivery.event_id != receipt.event_id {
            return Err(format!(
                "verified BOOX install {} reconstructed a different delivery identity",
                receipt.event_id
            ));
        }

        let descriptor_was_missing = metadata_if_exists(&delivery.descriptor_path)?.is_none();
        if !descriptor_was_missing
            && publish_bytes_create_only_or_verify(
                &delivery.descriptor_bytes,
                &delivery.descriptor_path,
            )? == DescriptorPublication::Conflict
        {
            return Err(format!(
                "verified BOOX recovery descriptor {} has unexpected content and was preserved for inspection",
                delivery.descriptor_path.display()
            ));
        }

        let pdf_metadata = metadata_if_exists(&delivery.pdf_path)?;
        let pdf_is_ready = pdf_metadata
            .as_ref()
            .is_some_and(|metadata| metadata.is_file())
            && sha256_file(&delivery.pdf_path).is_ok_and(|hash| hash == receipt.content_sha256);
        if !pdf_is_ready {
            if pdf_metadata.is_some() {
                return Err(format!(
                    "verified BOOX recovery PDF {} has unexpected content and was preserved for inspection",
                    delivery.pdf_path.display()
                ));
            }
            if receipt.source_object_path.is_none() || receipt.source_object_size.is_none() {
                return Err(format!(
                    "verified BOOX install {} predates recoverable broker object receipts and its local recovery PDF is missing",
                    receipt.event_id
                ));
            }
            if !self.download_verified(&object, &delivery.pdf_path, Some(&FileIdentity::Missing))? {
                return Err(format!(
                    "verified BOOX recovery PDF {} changed while its historical generation was downloading",
                    delivery.pdf_path.display()
                ));
            }
            remember_file_hash(
                &delivery.pdf_path,
                state,
                SystemTime::now(),
                &receipt.content_sha256,
            )?;
        }

        if publish_bytes_create_only_or_verify(
            &delivery.descriptor_bytes,
            &delivery.descriptor_path,
        )? == DescriptorPublication::Conflict
        {
            return Err(format!(
                "verified BOOX recovery descriptor {} has unexpected content and was preserved for inspection",
                delivery.descriptor_path.display()
            ));
        }
        Ok(())
    }
    fn deliver_outputs(
        &self,
        document: &DocumentFolders,
        state: &mut TransportState,
        report: &mut SyncReport,
    ) -> Result<(), String> {
        let boox_handoff_endpoint = self.boox_handoff_endpoint(document)?;
        let installed_boox_delivery = boox_handoff_endpoint
            .as_ref()
            .map(|endpoint| endpoint.installed_delivery(document))
            .transpose()?
            .flatten();
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
        let mut candidates = candidates
            .into_iter()
            .map(|(side, object)| {
                let revisions = parse_revision_metadata(&object)?;
                Ok((side, object, revisions))
            })
            .collect::<Result<Vec<_>, String>>()?;
        candidates.sort_by_key(|(_, object, revisions)| {
            (
                u128::from(revisions.boox) + u128::from(revisions.supernote),
                revisions.boox,
                revisions.supernote,
                object.generation,
            )
        });

        let mut installed_is_verified = false;
        if let (Some(endpoint), Some(installed)) = (
            boox_handoff_endpoint.as_ref(),
            installed_boox_delivery.as_ref(),
        ) {
            let matches_durable_receipt = state
                .documents
                .get(&document.document_id)
                .and_then(|document_state| document_state.verified_boox_install.as_ref())
                .is_some_and(|receipt| verified_boox_install_matches_ack(receipt, installed));
            let live_broker_output = candidates.iter().find_map(|(side, object, revisions)| {
                if *side != DeviceSide::Boox
                    || *revisions != installed.source_revisions
                    || object.generation != installed.source_generation
                {
                    return None;
                }
                let Ok(expected_hash) = required_metadata(object, CONTENT_SHA256) else {
                    return None;
                };
                if expected_hash != installed.content_sha256 {
                    return None;
                }
                if endpoint
                    .prepare_delivery(document, object, *revisions, expected_hash)
                    .is_ok_and(|delivery| delivery.event_id == installed.event_id)
                {
                    Some(object.clone())
                } else {
                    None
                }
            });
            installed_is_verified = matches_durable_receipt || live_broker_output.is_some();
            if installed_is_verified {
                if let Some(object) = live_broker_output.as_ref() {
                    state
                        .document_mut(&document.document_id)
                        .verified_boox_install = Some(verified_boox_install(installed, object));
                }
                let receipt = state
                    .documents
                    .get(&document.document_id)
                    .and_then(|document_state| document_state.verified_boox_install.clone())
                    .ok_or_else(|| {
                        format!(
                            "verified BOOX acknowledgement {} has no durable receipt",
                            installed.event_id
                        )
                    })?;
                self.restore_verified_boox_install_pair(endpoint, document, &receipt, state)?;
                endpoint.retire_superseded_incoming(document, installed)?;
                let current = state.document_mut(&document.document_id).revisions;
                let known_content_hash = state
                    .document_mut(&document.document_id)
                    .boox
                    .delivered_content_sha256
                    .as_deref();
                if installed.source_revisions == current
                    && known_content_hash.is_some_and(|hash| hash != installed.content_sha256)
                {
                    return Err(format!(
                        "BOOX installed acknowledgement {} reports different content for frontier {}:{}",
                        installed.event_id, current.boox, current.supernote,
                    ));
                }
                if dominates(installed.source_revisions, current) {
                    record_delivered_frontier(
                        state.document_mut(&document.document_id),
                        installed.source_revisions,
                        DeviceSide::Boox,
                        installed.content_sha256.clone(),
                    );
                } else if !dominates(current, installed.source_revisions) {
                    return Err(format!(
                        "BOOX installed acknowledgement {} at {}:{} conflicts with transport frontier {}:{}",
                        installed.event_id,
                        installed.source_revisions.boox,
                        installed.source_revisions.supernote,
                        current.boox,
                        current.supernote,
                    ));
                }
            }
        }
        let verified_installed_boox_delivery = installed_boox_delivery
            .as_ref()
            .filter(|_| installed_is_verified);
        for (side, object, revisions) in candidates {
            let generation_key = object.generation_key();
            let expected_hash = required_metadata(&object, CONTENT_SHA256)?.to_owned();
            let boox_handoff_delivery = if side == DeviceSide::Boox {
                boox_handoff_endpoint
                    .as_ref()
                    .map(|endpoint| {
                        endpoint.prepare_delivery(document, &object, revisions, &expected_hash)
                    })
                    .transpose()?
            } else {
                None
            };
            if let (Some(installed), Some(_delivery)) = (
                verified_installed_boox_delivery,
                boox_handoff_delivery.as_ref(),
            ) {
                if revisions == installed.source_revisions {
                    if expected_hash != installed.content_sha256 {
                        return Err(format!(
                            "broker BOOX output {} reports different content for installed frontier {}:{}",
                            object.path, revisions.boox, revisions.supernote,
                        ));
                    }
                    state
                        .document_mut(&document.document_id)
                        .delivered_generations
                        .insert(generation_key);
                    continue;
                }
                if dominates(installed.source_revisions, revisions) {
                    state
                        .document_mut(&document.document_id)
                        .delivered_generations
                        .insert(generation_key);
                    continue;
                }
            }
            let local_path = match side {
                DeviceSide::Boox => boox_handoff_delivery
                    .as_ref()
                    .map(|delivery| delivery.pdf_path.clone())
                    .unwrap_or_else(|| document.boox_pdf.clone()),
                DeviceSide::Supernote => {
                    let remote_name = Path::new(&object.path)
                        .file_name()
                        .and_then(|name| name.to_str())
                        .ok_or_else(|| format!("remote path {} has no file name", object.path))?;
                    document.supernote_incoming_directory.join(format!(
                        "r{:020}-r{:020}-g{:020}-{remote_name}",
                        revisions.boox, revisions.supernote, object.generation
                    ))
                }
            };
            if state
                .document_mut(&document.document_id)
                .delivered_generations
                .contains(&generation_key)
            {
                if let Some(delivery) = &boox_handoff_delivery {
                    let descriptor_was_missing =
                        metadata_if_exists(&delivery.descriptor_path)?.is_none();
                    if !descriptor_was_missing
                        && publish_bytes_create_only_or_verify(
                            &delivery.descriptor_bytes,
                            &delivery.descriptor_path,
                        )? == DescriptorPublication::Conflict
                    {
                        report.actions.push(TransportAction::Deferred {
                            side,
                            reason: format!(
                                "versioned BOOX handoff descriptor {} has unexpected content and was preserved for inspection",
                                delivery.descriptor_path.display()
                            ),
                        });
                        continue;
                    }
                    let installed = metadata_if_exists(&delivery.pdf_path)?
                        .is_some_and(|metadata| metadata.is_file())
                        && sha256_file(&delivery.pdf_path).is_ok_and(|hash| hash == expected_hash);
                    if !installed {
                        if metadata_if_exists(&delivery.pdf_path)?.is_some() {
                            report.actions.push(TransportAction::Deferred {
                                side,
                                reason: format!(
                                    "versioned BOOX handoff destination {} changed after delivery and was preserved for inspection",
                                    delivery.pdf_path.display()
                                ),
                            });
                            continue;
                        }
                        self.download_verified(
                            &object,
                            &delivery.pdf_path,
                            Some(&FileIdentity::Missing),
                        )?;
                        remember_file_hash(
                            &delivery.pdf_path,
                            state,
                            SystemTime::now(),
                            &expected_hash,
                        )?;
                    }
                    if publish_bytes_create_only_or_verify(
                        &delivery.descriptor_bytes,
                        &delivery.descriptor_path,
                    )? == DescriptorPublication::Conflict
                    {
                        report.actions.push(TransportAction::Deferred {
                            side,
                            reason: format!(
                                "versioned BOOX handoff descriptor {} has unexpected content and was preserved for inspection",
                                delivery.descriptor_path.display()
                            ),
                        });
                        continue;
                    }
                    if !installed || descriptor_was_missing {
                        report.actions.push(TransportAction::Delivered {
                            side,
                            object_path: object.path,
                            local_path: delivery.pdf_path.clone(),
                            generation: object.generation,
                        });
                    }
                } else if side == DeviceSide::Supernote
                    && !supernote_delivery_is_acknowledged(document, &expected_hash)?
                {
                    let installed = metadata_if_exists(&local_path)?
                        .is_some_and(|metadata| metadata.is_file())
                        && sha256_file(&local_path).is_ok_and(|hash| hash == expected_hash);
                    if !installed {
                        self.download_verified(&object, &local_path, None)?;
                        remember_file_hash(&local_path, state, SystemTime::now(), &expected_hash)?;
                        report.actions.push(TransportAction::Delivered {
                            side,
                            object_path: object.path,
                            local_path,
                            generation: object.generation,
                        });
                    }
                }
                continue;
            }
            let current = state.document_mut(&document.document_id).revisions;
            if !dominates(revisions, current) {
                state
                    .document_mut(&document.document_id)
                    .delivered_generations
                    .insert(generation_key);
                continue;
            }
            if side == DeviceSide::Supernote
                && revisions != current
                && (revisions.boox != current.boox + 1 || revisions.supernote != current.supernote)
            {
                report.actions.push(TransportAction::Deferred {
                    side,
                    reason: format!(
                        "broker manifest {} at revisions {}:{} is waiting for predecessor {}:{}",
                        object.path,
                        revisions.boox,
                        revisions.supernote,
                        current.boox + 1,
                        current.supernote,
                    ),
                });
                continue;
            }

            if let Some(delivery) = &boox_handoff_delivery {
                if metadata_if_exists(&delivery.descriptor_path)?.is_some()
                    && publish_bytes_create_only_or_verify(
                        &delivery.descriptor_bytes,
                        &delivery.descriptor_path,
                    )? == DescriptorPublication::Conflict
                {
                    report.actions.push(TransportAction::Deferred {
                        side,
                        reason: format!(
                            "versioned BOOX handoff descriptor {} has unexpected content and was preserved for inspection",
                            delivery.descriptor_path.display()
                        ),
                    });
                    continue;
                }
            }
            if side == DeviceSide::Boox {
                reconcile_staged_backup(&local_path, &expected_hash)?;
            }
            let boox_destination_identity = (side == DeviceSide::Boox
                && boox_handoff_delivery.is_none())
            .then(|| file_identity(&local_path))
            .transpose()?;
            let already_installed = if let Some(identity) = &boox_destination_identity {
                identity == &FileIdentity::Sha256(expected_hash.clone())
            } else {
                metadata_if_exists(&local_path)?.is_some_and(|metadata| metadata.is_file())
                    && sha256_file(&local_path).is_ok_and(|hash| hash == expected_hash)
            };
            if side == DeviceSide::Boox
                && boox_handoff_delivery.is_none()
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

            if boox_handoff_delivery.is_some()
                && !already_installed
                && metadata_if_exists(&local_path)?.is_some()
            {
                report.actions.push(TransportAction::Deferred {
                    side,
                    reason: format!(
                        "versioned BOOX handoff destination {} already exists with unexpected content and was preserved for inspection",
                        local_path.display()
                    ),
                });
                continue;
            }
            let handoff_missing = boox_handoff_delivery
                .as_ref()
                .map(|_| FileIdentity::Missing);
            let expected_destination = boox_destination_identity
                .as_ref()
                .or(handoff_missing.as_ref());
            if !already_installed
                && !self.download_verified(&object, &local_path, expected_destination)?
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
            if let Some(delivery) = &boox_handoff_delivery {
                if publish_bytes_create_only_or_verify(
                    &delivery.descriptor_bytes,
                    &delivery.descriptor_path,
                )? == DescriptorPublication::Conflict
                {
                    report.actions.push(TransportAction::Deferred {
                        side,
                        reason: format!(
                            "versioned BOOX handoff descriptor {} has unexpected content and was preserved for inspection",
                            delivery.descriptor_path.display()
                        ),
                    });
                    continue;
                }
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
            record_delivered_frontier(document_state, revisions, side, expected_hash);
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
        let Some(endpoint) = self.boox_handoff_endpoint(document)? else {
            return self.upload_legacy_boox_if_ready(document, state, now, report);
        };
        self.upload_boox_handoff_if_ready(&endpoint, document, state, now, report)
    }

    fn upload_boox_handoff_if_ready(
        &self,
        endpoint: &BooxHandoffEndpoint,
        document: &DocumentFolders,
        state: &mut TransportState,
        now: SystemTime,
        report: &mut SyncReport,
    ) -> Result<(), String> {
        if state
            .document_mut(&document.document_id)
            .boox
            .pending
            .is_some()
        {
            return Ok(());
        }
        let installed_boox_delivery = endpoint.installed_delivery(document)?.filter(|installed| {
            state
                .documents
                .get(&document.document_id)
                .and_then(|document_state| document_state.verified_boox_install.as_ref())
                .is_some_and(|receipt| verified_boox_install_matches_ack(receipt, installed))
        });
        for artifact in endpoint.finalized_artifacts(document)? {
            let local_key = canonical_path_key(&artifact.payload_path);
            let source_local_id = sha256_hex(artifact.event.event_id.as_bytes());
            let expected_source_hash = artifact.event.content_sha256.clone();
            let (already_uploaded, accepted) = {
                let boox_state = &state.document_mut(&document.document_id).boox;
                let already_uploaded = boox_state
                    .uploaded_local_hashes
                    .get(&local_key)
                    .is_some_and(|hash| hash == &expected_source_hash);
                let accepted = boox_state
                    .accepted_local_hashes
                    .get(&local_key)
                    .is_some_and(|hash| hash == &expected_source_hash)
                    || boox_state
                        .accepted_source_revisions
                        .get(&source_local_id)
                        .is_some_and(|revision| *revision >= artifact.event.source_revision);
                (already_uploaded, accepted)
            };
            if accepted {
                if installed_boox_delivery.as_ref().is_some_and(|installed| {
                    installed.source_revisions.boox >= artifact.event.source_revision
                }) {
                    endpoint.retire_accepted_artifact(document, &artifact)?;
                }
                continue;
            }
            if already_uploaded {
                continue;
            }
            if !file_is_settled(&artifact.descriptor_path, state, now, self.settle)?
                || !file_is_settled(&artifact.payload_path, state, now, self.settle)?
            {
                continue;
            }
            let source_hash = sha256_file(&artifact.payload_path)?;
            if source_hash != expected_source_hash {
                report.actions.push(TransportAction::Deferred {
                    side: DeviceSide::Boox,
                    reason: format!(
                        "finalized BOOX payload {} has content hash {source_hash}, not descriptor hash {}, and was preserved for inspection",
                        artifact.payload_path.display(),
                        expected_source_hash
                    ),
                });
                continue;
            }
            if artifact.event.payload_kind == DevicePayloadKind::BooxOperationManifest {
                return self.upload_prebuilt_boox_manifest(
                    document,
                    state,
                    &artifact,
                    &source_hash,
                    report,
                );
            }
            let current = state.document_mut(&document.document_id).revisions;
            if artifact.event.based_on == current {
                let mut companion_document = document.clone();
                companion_document
                    .boox_pdf
                    .clone_from(&artifact.payload_path);
                return self.upload_boox_pdf_if_ready(
                    &companion_document,
                    state,
                    now,
                    report,
                    Some(&source_local_id),
                );
            }
            return self.upload_conflicting_boox_handoff(
                document,
                state,
                &artifact,
                &source_hash,
                report,
            );
        }
        Ok(())
    }

    fn upload_prebuilt_boox_manifest(
        &self,
        document: &DocumentFolders,
        state: &mut TransportState,
        artifact: &FinalizedBooxArtifact,
        payload_hash: &str,
        report: &mut SyncReport,
    ) -> Result<(), String> {
        let local_key = canonical_path_key(&artifact.payload_path);
        let source_local_id = sha256_hex(artifact.event.event_id.as_bytes());
        let source_view_hash = read_boox_operation_source_view_hash(&artifact.payload_path)?;
        let (object, source_revision) = self.upload_source_at(
            document,
            state,
            DeviceSide::Boox,
            &artifact.payload_path,
            &local_key,
            &source_local_id,
            &[],
            &source_view_hash,
            payload_hash,
            "boox_operation_manifest",
            "operations.json",
            artifact.event.based_on,
            artifact.event.source_revision,
        )?;
        state
            .document_mut(&document.document_id)
            .boox
            .uploaded_local_hashes
            .insert(local_key, payload_hash.to_owned());
        report.actions.push(TransportAction::Uploaded {
            side: DeviceSide::Boox,
            local_path: artifact.payload_path.clone(),
            object_path: object.path,
            source_revision,
            uploaded_bytes: object.size,
        });
        Ok(())
    }
    fn upload_conflicting_boox_handoff(
        &self,
        document: &DocumentFolders,
        state: &mut TransportState,
        artifact: &FinalizedBooxArtifact,
        source_hash: &str,
        report: &mut SyncReport,
    ) -> Result<(), String> {
        let source_local_id = sha256_hex(artifact.event.event_id.as_bytes());
        let (object, source_revision) = self.upload_source_at(
            document,
            state,
            DeviceSide::Boox,
            &artifact.payload_path,
            &canonical_path_key(&artifact.payload_path),
            &source_local_id,
            &[],
            source_hash,
            source_hash,
            "device_view",
            "pdf",
            artifact.event.based_on,
            artifact.event.source_revision,
        )?;
        state
            .document_mut(&document.document_id)
            .boox
            .uploaded_local_hashes
            .insert(
                canonical_path_key(&artifact.payload_path),
                source_hash.to_owned(),
            );
        report.actions.push(TransportAction::Uploaded {
            side: DeviceSide::Boox,
            local_path: artifact.payload_path.clone(),
            object_path: object.path,
            source_revision,
            uploaded_bytes: object.size,
        });
        Ok(())
    }

    fn upload_legacy_boox_if_ready(
        &self,
        document: &DocumentFolders,
        state: &mut TransportState,
        now: SystemTime,
        report: &mut SyncReport,
    ) -> Result<(), String> {
        self.upload_boox_pdf_if_ready(document, state, now, report, None)
    }

    fn upload_boox_pdf_if_ready(
        &self,
        document: &DocumentFolders,
        state: &mut TransportState,
        now: SystemTime,
        report: &mut SyncReport,
        stable_source_local_id: Option<&str>,
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
        let accepted = state
            .document_mut(&document.document_id)
            .supernote
            .accepted_local_hashes
            .clone();
        let mut missing_accepted = Vec::new();
        let mut baselines = Vec::new();
        let mut baseline_hashes = Vec::new();
        for (key, expected_hash) in &accepted {
            if key.starts_with(RECOVERED_MISSING_PREFIX) {
                missing_accepted.push(key.clone());
                continue;
            }
            let baseline = PathBuf::from(key);
            if !metadata_if_exists(&baseline)?.is_some_and(|metadata| metadata.is_file()) {
                missing_accepted.push(key.clone());
                continue;
            }
            let actual_hash = sha256_file(&baseline)?;
            if &actual_hash != expected_hash {
                missing_accepted.push(format!(
                    "{} (expected {expected_hash}, found {actual_hash})",
                    baseline.display()
                ));
                continue;
            }
            baseline_hashes.push((baseline.clone(), expected_hash.clone()));
            baselines.push(baseline);
        }
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
        if baselines.is_empty() {
            report.actions.push(TransportAction::Deferred {
                side: DeviceSide::Boox,
                reason:
                    "BOOX edit is ready, but no accepted Supernote identity baseline is available"
                        .to_owned(),
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
        let source_local_id = stable_source_local_id
            .map(str::to_owned)
            .unwrap_or_else(|| sha256_hex(local_key.as_bytes()));
        let temporary = sibling_temporary(&document.boox_pdf, "compact-upload");
        fs::write(&temporary, &manifest_bytes)
            .map_err(|error| format!("could not write {}: {error}", temporary.display()))?;
        let result = self.upload_source(
            document,
            state,
            DeviceSide::Boox,
            &temporary,
            &local_key,
            &source_local_id,
            &[],
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
        let exports = supernote_export_files(&document.supernote_export_directory)?;
        if exports.is_empty() {
            return Ok(());
        }
        if let Some(waiting) = first_unacknowledged_supernote_delivery(document)? {
            report.actions.push(TransportAction::Deferred {
                side: DeviceSide::Supernote,
                reason: format!(
                    "Supernote export upload is paused until the downloaded manifest {} is applied and acknowledged",
                    waiting.display()
                ),
            });
            return Ok(());
        }
        for export in exports {
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
            validate_supernote_export_identity(&parsed, document, &export.to_string_lossy())?;
            if state
                .document_mut(&document.document_id)
                .supernote
                .uploaded_local_hashes
                .get(&local_key)
                .is_some_and(|hash| hash == &content_hash)
            {
                continue;
            }
            let current = state.document_mut(&document.document_id).revisions;
            let exported_at = parsed.based_on.map(|based_on| RevisionPair {
                boox: based_on.boox,
                supernote: based_on.supernote,
            });
            // The complete set of represented original pages is the logical local source.
            // Its identity survives export-file renames and keeps one Virtual Spread
            // revision atomic from the device folder through broker acceptance.
            let source_page_indices = parsed
                .pages
                .iter()
                .map(|page| page.page_index)
                .collect::<Vec<_>>();
            let source_local_id =
                supernote_snapshot_identity(&document.document_id, &source_page_indices)?;
            if exported_at.is_none() && current != RevisionPair::default() {
                report.actions.push(TransportAction::Deferred {
                    side: DeviceSide::Supernote,
                    reason: format!(
                        "Supernote export {} has no revision frontier but the broker is already at {}:{}; export the page again with the current plugin",
                        export.display(), current.boox, current.supernote,
                    ),
                });
                return Ok(());
            }
            if let Some(exported_at) = exported_at {
                let same_page_revision = source_page_indices
                    .iter()
                    .map(|page_index| supernote_page_identity(&document.document_id, *page_index))
                    .chain(std::iter::once(source_local_id.clone()))
                    .filter_map(|identity| {
                        state
                            .document_mut(&document.document_id)
                            .supernote
                            .accepted_source_revisions
                            .get(&identity)
                            .copied()
                    })
                    .max()
                    .unwrap_or(0);
                let unsafe_to_rebase = exported_at.boox != current.boox
                    || exported_at.supernote > current.supernote
                    || same_page_revision > exported_at.supernote;
                if unsafe_to_rebase {
                    let cause = if exported_at.boox != current.boox {
                        "the BOOX revision changed"
                    } else if exported_at.supernote > current.supernote {
                        "the export is ahead of the current Supernote revision"
                    } else {
                        "the same Supernote page changed after this export was captured"
                    };
                    report.actions.push(TransportAction::Deferred {
                        side: DeviceSide::Supernote,
                        reason: format!(
                            "Supernote export {} was captured at revisions {}:{} but the current broker frontier is {}:{} and {cause}; export the page again after applying all incoming updates",
                            export.display(),
                            exported_at.boox,
                            exported_at.supernote,
                            current.boox,
                            current.supernote,
                        ),
                    });
                    return Ok(());
                }
            }
            let payload_bytes = if exported_at.is_some_and(|frontier| frontier != current) {
                serialize_baseline_export(
                    &parsed,
                    BaselineRevisions {
                        boox: current.boox,
                        supernote: current.supernote,
                    },
                )?
            } else {
                bytes
            };
            let payload_hash = sha256_hex(&payload_bytes);
            let source_revision = current.supernote + 1;
            let snapshot = persist_supernote_snapshot_bytes(
                document,
                &source_local_id,
                source_revision,
                &payload_hash,
                &payload_bytes,
            )?;
            let snapshot_key = canonical_path_key(&snapshot);
            let temporary = sibling_temporary(&export, "native-upload");
            fs::write(&temporary, &payload_bytes)
                .map_err(|error| format!("could not write {}: {error}", temporary.display()))?;
            let result = self.upload_source(
                document,
                state,
                DeviceSide::Supernote,
                &temporary,
                &snapshot_key,
                &source_local_id,
                &source_page_indices,
                &payload_hash,
                &payload_hash,
                "device_view",
                "json",
            );
            let _ = fs::remove_file(&temporary);
            let (object, source_revision) = result?;
            if source_revision != current.supernote + 1 {
                return Err(format!(
                    "Supernote snapshot revision {} does not match uploaded revision {source_revision}",
                    current.supernote + 1
                ));
            }
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
        source_local_id: &str,
        source_page_indices: &[u32],
        local_hash: &str,
        payload_hash: &str,
        payload_kind: &str,
        extension: &str,
    ) -> Result<(CloudObject, u64), String> {
        let based_on = state.document_mut(&document.document_id).revisions;
        let source_revision = based_on.get(side) + 1;
        self.upload_source_at(
            document,
            state,
            side,
            payload_path,
            local_key,
            source_local_id,
            source_page_indices,
            local_hash,
            payload_hash,
            payload_kind,
            extension,
            based_on,
            source_revision,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn upload_source_at(
        &self,
        document: &DocumentFolders,
        state: &mut TransportState,
        side: DeviceSide,
        payload_path: &Path,
        local_key: &str,
        source_local_id: &str,
        source_page_indices: &[u32],
        local_hash: &str,
        payload_hash: &str,
        payload_kind: &str,
        extension: &str,
        based_on: RevisionPair,
        source_revision: u64,
    ) -> Result<(CloudObject, u64), String> {
        let document_state = state.document_mut(&document.document_id);
        if !document_state.conflicts.is_empty() {
            return Err(format!(
                "{} has an unresolved broker conflict; preserve both device files before resuming",
                document.document_id
            ));
        }
        if source_revision != based_on.get(side) + 1 {
            return Err(format!(
                "{side:?} source revision {source_revision} does not immediately follow basedOn revision {}",
                based_on.get(side)
            ));
        }
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
        let mut metadata = BTreeMap::from([
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
            (SOURCE_LOCAL_ID.to_owned(), source_local_id.to_owned()),
        ]);
        match source_page_indices {
            [] => {}
            [page_index] => {
                metadata.insert(SOURCE_PAGE_INDEX.to_owned(), page_index.to_string());
            }
            page_indices => {
                metadata.insert(
                    SOURCE_PAGE_INDICES.to_owned(),
                    page_indices
                        .iter()
                        .map(u32::to_string)
                        .collect::<Vec<_>>()
                        .join(","),
                );
            }
        }
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

fn verified_boox_install(
    installed: &InstalledBooxDelivery,
    object: &CloudObject,
) -> VerifiedBooxInstall {
    VerifiedBooxInstall {
        event_id: installed.event_id.clone(),
        source_revisions: installed.source_revisions,
        source_generation: installed.source_generation,
        content_sha256: installed.content_sha256.clone(),
        source_object_path: Some(object.path.clone()),
        source_object_size: Some(object.size),
    }
}

fn verified_boox_install_matches_ack(
    receipt: &VerifiedBooxInstall,
    installed: &InstalledBooxDelivery,
) -> bool {
    receipt.event_id == installed.event_id
        && receipt.source_revisions == installed.source_revisions
        && receipt.source_generation == installed.source_generation
        && receipt.content_sha256 == installed.content_sha256
}

fn verified_boox_install_object(
    receipt: &VerifiedBooxInstall,
    document: &DocumentFolders,
) -> Result<CloudObject, String> {
    let expected_prefix = format!("BOOX_Folder/{}/", document.document_id);
    let object_path = match receipt.source_object_path.as_deref() {
        Some(path) if path.starts_with(&expected_prefix) && path != expected_prefix => {
            path.to_owned()
        }
        Some(path) => {
            return Err(format!(
                "verified BOOX install {} has invalid broker object path {}",
                receipt.event_id, path
            ));
        }
        None => format!("{expected_prefix}.legacy-receipt"),
    };
    let object_size = receipt.source_object_size.unwrap_or(0);
    let mut metadata = BTreeMap::new();
    metadata.insert(GENERATED_BY.to_owned(), BROKER_PRODUCER.to_owned());
    metadata.insert(DOCUMENT_ID.to_owned(), document.document_id.clone());
    metadata.insert(GENERATED_EVENT_ID.to_owned(), receipt.event_id.clone());
    metadata.insert(
        SOURCE_REVISIONS.to_owned(),
        format!(
            "{}:{}",
            receipt.source_revisions.boox, receipt.source_revisions.supernote
        ),
    );
    metadata.insert(CONTENT_SHA256.to_owned(), receipt.content_sha256.clone());
    Ok(CloudObject {
        path: object_path,
        generation: receipt.source_generation,
        size: object_size,
        metadata,
    })
}

fn read_boox_operation_source_view_hash(path: &Path) -> Result<String, String> {
    let metadata = fs::metadata(path).map_err(|error| {
        format!(
            "could not inspect BOOX operation manifest {}: {error}",
            path.display()
        )
    })?;
    if metadata.len() > MAX_BOOX_OPERATION_MANIFEST_BYTES {
        return Err(format!(
            "BOOX operation manifest {} exceeds the {} byte safety limit",
            path.display(),
            MAX_BOOX_OPERATION_MANIFEST_BYTES
        ));
    }
    let file = File::open(path).map_err(|error| {
        format!(
            "could not read BOOX operation manifest {}: {error}",
            path.display()
        )
    })?;
    let manifest: Manifest = serde_json::from_reader(BufReader::new(file)).map_err(|error| {
        format!(
            "invalid BOOX operation manifest {}: {error}",
            path.display()
        )
    })?;
    let hash = manifest.document.pdf_sha256;
    let valid = hash.len() == 64
        && hash
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase());
    if !valid {
        return Err(format!(
            "BOOX operation manifest {} has an invalid source PDF hash",
            path.display()
        ));
    }
    Ok(hash)
}
fn record_delivered_frontier(
    document_state: &mut DocumentTransportState,
    revisions: RevisionPair,
    side: DeviceSide,
    content_sha256: String,
) {
    document_state.revisions = revisions;
    document_state.side_mut(side).delivered_content_sha256 = Some(content_sha256);
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

pub(crate) fn first_unacknowledged_supernote_delivery(
    document: &DocumentFolders,
) -> Result<Option<PathBuf>, String> {
    let incoming = &document.supernote_incoming_directory;
    let Some(metadata) = symlink_metadata_if_exists(incoming)? else {
        return Ok(None);
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(format!(
            "configured Supernote incoming path {} is not a regular directory",
            incoming.display()
        ));
    }
    let mut deliveries = Vec::new();
    for entry in fs::read_dir(incoming)
        .map_err(|error| format!("could not read {}: {error}", incoming.display()))?
    {
        let path = entry
            .map_err(|error| {
                format!(
                    "could not enumerate an entry in {}: {error}",
                    incoming.display()
                )
            })?
            .path();
        if !path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| !name.starts_with('.') && name.ends_with(".operations.json"))
        {
            continue;
        }
        let metadata = fs::symlink_metadata(&path)
            .map_err(|error| format!("could not inspect {}: {error}", path.display()))?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(format!(
                "Supernote incoming delivery {} is not a regular file",
                path.display()
            ));
        }
        if metadata.len() > MAX_SUPERNOTE_MANIFEST_BYTES {
            return Err(format!(
                "Supernote incoming delivery {} exceeds {} bytes",
                path.display(),
                MAX_SUPERNOTE_MANIFEST_BYTES
            ));
        }
        deliveries.push(path);
    }
    deliveries.sort();
    for delivery in deliveries {
        let bytes = fs::read(&delivery)
            .map_err(|error| format!("could not read {}: {error}", delivery.display()))?;
        let delivery_id = sha256_hex(&bytes);
        if !supernote_delivery_is_acknowledged(document, &delivery_id)? {
            return Ok(Some(delivery));
        }
    }
    Ok(None)
}

fn supernote_delivery_is_acknowledged(
    document: &DocumentFolders,
    delivery_id: &str,
) -> Result<bool, String> {
    if !is_sha256(delivery_id) {
        return Err(format!("invalid Supernote delivery identity {delivery_id}"));
    }
    let ack = document
        .supernote_acknowledged_directory()
        .join(format!("{delivery_id}.ack.json"));
    let Some(metadata) = symlink_metadata_if_exists(&ack)? else {
        return Ok(false);
    };
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.len() > MAX_SUPERNOTE_ACK_BYTES
    {
        return Err(format!(
            "Supernote acknowledgement {} is not a valid regular file",
            ack.display()
        ));
    }
    let value: serde_json::Value = serde_json::from_slice(
        &fs::read(&ack).map_err(|error| format!("could not read {}: {error}", ack.display()))?,
    )
    .map_err(|error| {
        format!(
            "invalid Supernote acknowledgement {}: {error}",
            ack.display()
        )
    })?;
    if value
        .get("schemaVersion")
        .and_then(serde_json::Value::as_u64)
        != Some(1)
        || value.get("deliveryId").and_then(serde_json::Value::as_str) != Some(delivery_id)
        || value.get("documentId").and_then(serde_json::Value::as_str)
            != Some(document.document_id.as_str())
    {
        return Err(format!(
            "Supernote acknowledgement {} does not match its delivery and document",
            ack.display()
        ));
    }
    Ok(true)
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
        for page in export.pages {
            if let Some(previous) = pages.insert(page.page_index, path) {
                return Err(format!(
                    "Supernote outgoing folder has more than one accepted export for page {}: {} and {}",
                    page.page_index + 1,
                    previous.display(),
                    path.display()
                ));
            }
        }
    }
    Ok(())
}

fn validate_supernote_export_identity(
    export: &BaselineExport,
    document: &DocumentFolders,
    source: &str,
) -> Result<(), String> {
    if let Some(document_id) = export.document_id.as_deref() {
        if document_id == document.document_id {
            return Ok(());
        }
        return Err(format!(
            "{source} targets stable document {document_id}, not {}",
            document.document_id
        ));
    }
    if export.source_file_name.as_deref() == Some(document.original_file_name.as_str()) {
        return Ok(());
    }
    Err(format!(
        "legacy Supernote export {source} targets {:?}, not configured document {}",
        export.source_file_name, document.original_file_name
    ))
}

fn unix_millis(value: SystemTime) -> Result<u64, String> {
    value
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .map_err(|_| "filesystem timestamp predates the Unix epoch".to_owned())
}

pub(crate) fn canonical_path_key(path: &Path) -> String {
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
    sha256_open_file(&file, path)
}

fn sha256_open_file(file: &File, path: &Path) -> Result<String, String> {
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

pub(crate) fn symlink_metadata_if_exists(path: &Path) -> Result<Option<fs::Metadata>, String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => Ok(Some(metadata)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!(
            "could not inspect filesystem entry {}: {error}",
            path.display()
        )),
    }
}

pub(crate) fn remove_file_if_exists(path: &Path) -> Result<(), String> {
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

pub(crate) fn sibling_temporary(path: &Path, suffix: &str) -> PathBuf {
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
    let staged_metadata = symlink_metadata_if_exists(&backup)?.ok_or_else(|| {
        format!(
            "staged BOOX destination {} disappeared before verification",
            backup.display()
        )
    })?;
    if staged_metadata.file_type().is_symlink() {
        remove_file_if_exists(source)?;
        restore_staged_file(&backup, destination)?;
        return Ok(false);
    }
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DescriptorPublication {
    Ready,
    Conflict,
}

fn publish_bytes_create_only_or_verify(
    bytes: &[u8],
    destination: &Path,
) -> Result<DescriptorPublication, String> {
    if let Some(metadata) = metadata_if_exists(destination)? {
        if !metadata.is_file() {
            return Ok(DescriptorPublication::Conflict);
        }
        return Ok(if descriptor_matches(bytes, destination, &metadata)? {
            DescriptorPublication::Ready
        } else {
            DescriptorPublication::Conflict
        });
    }
    let parent = destination.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)
        .map_err(|error| format!("could not create {}: {error}", parent.display()))?;
    let temporary = sibling_temporary(destination, "descriptor");
    remove_file_if_exists(&temporary)?;
    let mut output = File::create(&temporary)
        .map_err(|error| format!("could not write {}: {error}", temporary.display()))?;
    output
        .write_all(bytes)
        .and_then(|_| output.flush())
        .and_then(|_| output.sync_all())
        .map_err(|error| format!("could not finalize {}: {error}", temporary.display()))?;
    drop(output);
    let published = publish_create_only(&temporary, destination)?;
    if !published {
        let Some(metadata) = metadata_if_exists(destination)? else {
            return Err(format!(
                "handoff descriptor {} disappeared after concurrent publication",
                destination.display()
            ));
        };
        if !metadata.is_file() {
            return Ok(DescriptorPublication::Conflict);
        }
        if !descriptor_matches(bytes, destination, &metadata)? {
            return Ok(DescriptorPublication::Conflict);
        }
    }
    Ok(DescriptorPublication::Ready)
}
fn descriptor_matches(bytes: &[u8], path: &Path, metadata: &fs::Metadata) -> Result<bool, String> {
    if bytes.len() as u64 > MAX_DESCRIPTOR_BYTES || metadata.len() > MAX_DESCRIPTOR_BYTES {
        return Ok(false);
    }
    let mut existing = Vec::with_capacity(metadata.len() as usize);
    File::open(path)
        .map_err(|error| format!("could not read {}: {error}", path.display()))?
        .take(MAX_DESCRIPTOR_BYTES + 1)
        .read_to_end(&mut existing)
        .map_err(|error| format!("could not read {}: {error}", path.display()))?;
    Ok(existing.len() as u64 <= MAX_DESCRIPTOR_BYTES && existing == bytes)
}

pub(crate) fn publish_create_only(source: &Path, destination: &Path) -> Result<bool, String> {
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

    match renameat_with(CWD, source, CWD, destination, RenameFlags::NOREPLACE) {
        Ok(()) => Ok(()),
        Err(error) => {
            let error: std::io::Error = error.into();
            if create_only_rename_is_unsupported(&error) {
                hard_link_create_only(source, destination)
            } else {
                Err(error)
            }
        }
    }
}

#[cfg(any(
    target_os = "linux",
    target_os = "android",
    target_os = "macos",
    target_os = "ios",
    target_os = "redox"
))]
fn create_only_rename_is_unsupported(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::InvalidInput
        || error.kind() == std::io::ErrorKind::Unsupported
}

#[cfg(any(
    target_os = "linux",
    target_os = "android",
    target_os = "macos",
    target_os = "ios",
    target_os = "redox"
))]
fn hard_link_create_only(source: &Path, destination: &Path) -> std::io::Result<()> {
    // Some mounted filesystems (notably WSL DrvFs) reject renameat2 with
    // RENAME_NOREPLACE even though they support atomic create-only hard links.
    // Linking in the same directory preserves the no-overwrite guarantee. Once
    // the destination exists, the hidden source name is only a disposable alias.
    fs::hard_link(source, destination)?;
    let _ = fs::remove_file(source);
    Ok(())
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
    reconcile_staged_backup_with(destination, published_hash, || Ok(()))
}

fn reconcile_staged_backup_with<F>(
    destination: &Path,
    published_hash: &str,
    after_destination_opened: F,
) -> Result<(), String>
where
    F: FnOnce() -> Result<(), String>,
{
    let backup = sibling_temporary(destination, "previous");
    let backup_is_symlink = match symlink_metadata_if_exists(&backup)? {
        None => return Ok(()),
        Some(metadata) if metadata.file_type().is_symlink() => true,
        Some(metadata) if !metadata.is_file() => {
            return Err(format!(
                "staged BOOX backup {} is not a regular file",
                backup.display()
            ));
        }
        Some(_) => false,
    };
    let destination_metadata = symlink_metadata_if_exists(destination)?;
    if destination_metadata
        .as_ref()
        .is_some_and(|metadata| metadata.file_type().is_symlink())
    {
        return Err(format!(
            "BOOX destination {} was recreated as a symlink; preserved staged backup {}",
            destination.display(),
            backup.display()
        ));
    }
    match destination_metadata {
        None => restore_staged_file(&backup, destination),
        Some(metadata) if !metadata.is_file() => Err(format!(
            "BOOX destination {} is not a regular file; preserved staged backup {}",
            destination.display(),
            backup.display()
        )),
        Some(_) => {
            let opened_destination = open_installed_regular_file(destination)?;
            after_destination_opened()?;
            let current_hash = sha256_open_file(&opened_destination, destination)?;
            if current_hash == published_hash && !backup_is_symlink {
                // Keep this second handle open through backup retirement. On Windows it
                // is opened without delete sharing, preventing a concurrent replacement;
                // on Unix its file identity and two lstat checks detect a replacement.
                let _installed_guard =
                    open_same_installed_regular_file(destination, &opened_destination)?;
                retire_validated_staged_backup(&backup)
            } else {
                Err(format!(
                    "both BOOX destination {} and interrupted staged backup {} contain distinct data; preserved both",
                    destination.display(),
                    backup.display()
                ))
            }
        }
    }
}

#[cfg(windows)]
fn retire_validated_staged_backup(backup: &Path) -> Result<(), String> {
    // The live destination guard omits FILE_SHARE_DELETE, so its pathname
    // cannot be replaced between validation and backup retirement.
    remove_file_if_exists(backup)
}

#[cfg(unix)]
fn retire_validated_staged_backup(backup: &Path) -> Result<(), String> {
    // Unix open handles do not pin pathnames. There is no atomic primitive
    // that ties removing this separate backup path to the validated identity
    // of the live destination, so preserve the predecessor for explicit review.
    Err(format!(
        "confirmed the published BOOX destination, but cannot safely retire staged backup {} on Unix; preserved it for explicit reconciliation",
        backup.display()
    ))
}

fn open_installed_regular_file(path: &Path) -> Result<File, String> {
    let metadata = symlink_metadata_if_exists(path)?.ok_or_else(|| {
        format!(
            "BOOX destination {} disappeared during recovery; preserved staged backup",
            path.display()
        )
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(format!(
            "BOOX destination {} is no longer a regular file; preserved staged backup",
            path.display()
        ));
    }
    let file = open_entry_guard(path).map_err(|error| {
        format!(
            "could not open BOOX destination {} during recovery: {error}; preserved staged backup",
            path.display()
        )
    })?;
    open_same_installed_regular_file(path, &file)?;
    Ok(file)
}

fn open_same_installed_regular_file(path: &Path, opened: &File) -> Result<File, String> {
    open_same_installed_regular_file_with(path, opened, || Ok(()))
}

fn open_same_installed_regular_file_with<F>(
    path: &Path,
    opened: &File,
    after_current_opened: F,
) -> Result<File, String>
where
    F: FnOnce() -> Result<(), String>,
{
    let before = symlink_metadata_if_exists(path)?.ok_or_else(|| {
        format!(
            "BOOX destination {} disappeared during recovery; preserved staged backup",
            path.display()
        )
    })?;
    if before.file_type().is_symlink() || !before.is_file() {
        return Err(format!(
            "BOOX destination {} changed filesystem entry during recovery; preserved staged backup",
            path.display()
        ));
    }
    let current = open_entry_guard(path).map_err(|error| {
        format!(
            "could not re-open BOOX destination {} during recovery: {error}; preserved staged backup",
            path.display()
        )
    })?;
    after_current_opened()?;
    let after = symlink_metadata_if_exists(path)?.ok_or_else(|| {
        format!(
            "BOOX destination {} disappeared during recovery; preserved staged backup",
            path.display()
        )
    })?;
    if after.file_type().is_symlink()
        || !after.is_file()
        || open_file_identity(opened)? != open_file_identity(&current)?
        || !metadata_matches_open_file(&after, &current)?
    {
        return Err(format!(
            "BOOX destination {} changed filesystem entry during recovery; preserved staged backup",
            path.display()
        ));
    }
    Ok(current)
}

#[cfg(unix)]
fn metadata_matches_open_file(metadata: &fs::Metadata, file: &File) -> Result<bool, String> {
    use std::os::unix::fs::MetadataExt;

    Ok((metadata.dev(), metadata.ino()) == open_file_identity(file)?)
}

#[cfg(windows)]
fn metadata_matches_open_file(_metadata: &fs::Metadata, _file: &File) -> Result<bool, String> {
    // `open_entry_guard` omits FILE_SHARE_DELETE on Windows, so the pathname
    // cannot be replaced while the current handle is alive.
    Ok(true)
}

#[cfg(windows)]
fn open_entry_guard(path: &Path) -> std::io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::{FILE_SHARE_READ, FILE_SHARE_WRITE};

    fs::OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .open(path)
}

#[cfg(not(windows))]
fn open_entry_guard(path: &Path) -> std::io::Result<File> {
    File::open(path)
}

#[cfg(unix)]
fn open_file_identity(file: &File) -> Result<(u64, u64), String> {
    use std::os::unix::fs::MetadataExt;

    let metadata = file
        .metadata()
        .map_err(|error| format!("could not inspect opened BOOX destination: {error}"))?;
    Ok((metadata.dev(), metadata.ino()))
}

#[cfg(windows)]
fn open_file_identity(file: &File) -> Result<(u32, u64), String> {
    use std::mem::MaybeUninit;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::Storage::FileSystem::{
        GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
    };

    let mut information = MaybeUninit::<BY_HANDLE_FILE_INFORMATION>::zeroed();
    // SAFETY: The handle remains owned by `file` for the call duration and
    // `information` points to writable storage of the required type.
    if unsafe {
        GetFileInformationByHandle(file.as_raw_handle() as HANDLE, information.as_mut_ptr())
    } == 0
    {
        return Err(format!(
            "could not identify opened BOOX destination: {}",
            std::io::Error::last_os_error()
        ));
    }
    // SAFETY: A successful GetFileInformationByHandle initializes the full structure.
    let information = unsafe { information.assume_init() };
    Ok((
        information.dwVolumeSerialNumber,
        (u64::from(information.nFileIndexHigh) << 32) | u64::from(information.nFileIndexLow),
    ))
}

fn restore_staged_file(backup: &Path, destination: &Path) -> Result<(), String> {
    if symlink_metadata_if_exists(destination)?.is_some() {
        return Err(format!(
            "destination {} was recreated; preserved staged file at {}",
            destination.display(),
            backup.display()
        ));
    }
    match rename_create_only(backup, destination) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Err(format!(
            "destination {} was recreated during recovery; preserved staged file at {}",
            destination.display(),
            backup.display()
        )),
        Err(error) => Err(format!(
            "could not restore {} as {}: {error}",
            backup.display(),
            destination.display()
        )),
    }
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

    fn configured_document(root: &Path) -> DocumentFolders {
        DocumentFolders {
            document_id: format!("inkbridge-doc-v1-{}", "a".repeat(64)),
            original_file_name: "Configured.pdf".to_owned(),
            boox_pdf: root.join("boox/Configured.pdf"),
            supernote_export_directory: root.join("supernote/outgoing"),
            supernote_incoming_directory: root.join("supernote/incoming"),
        }
    }

    #[test]
    fn descriptor_comparison_rejects_oversized_existing_file() {
        let directory = tempdir().unwrap();
        let descriptor = directory.path().join("delivery.inkbridge.json");
        fs::write(&descriptor, vec![b'x'; MAX_DESCRIPTOR_BYTES as usize + 1]).unwrap();
        let metadata = fs::metadata(&descriptor).unwrap();

        assert!(!descriptor_matches(b"expected", &descriptor, &metadata).unwrap());
    }

    #[test]
    fn descriptor_comparison_bounds_file_that_grows_after_metadata_check() {
        let directory = tempdir().unwrap();
        let descriptor = directory.path().join("delivery.inkbridge.json");
        fs::write(&descriptor, b"expected").unwrap();
        let stale_metadata = fs::metadata(&descriptor).unwrap();
        fs::write(&descriptor, vec![b'x'; MAX_DESCRIPTOR_BYTES as usize + 1]).unwrap();

        assert!(!descriptor_matches(b"expected", &descriptor, &stale_metadata).unwrap());
    }

    #[test]
    fn stable_document_identity_allows_device_specific_filenames() {
        let directory = tempdir().unwrap();
        let document = configured_document(directory.path());
        let export = BaselineExport {
            source_file_name: Some("Supernote Copy.pdf".to_owned()),
            document_id: Some(document.document_id.clone()),
            based_on: None,
            pages: vec![inkbridge_convert::BaselinePage {
                page_index: 0,
                strokes: Vec::new(),
            }],
        };
        validate_supernote_export_identity(&export, &document, "page-0001.json").unwrap();
    }

    #[test]
    fn accepted_snapshot_path_distinguishes_changed_retry_content() {
        let directory = tempdir().unwrap();
        let document = configured_document(directory.path());
        let source_local_id = "b".repeat(64);
        let first_hash = "c".repeat(64);
        let changed_hash = "d".repeat(64);

        let first = supernote_accepted_snapshot_path(&document, &source_local_id, 2, &first_hash);
        let changed =
            supernote_accepted_snapshot_path(&document, &source_local_id, 2, &changed_hash);

        assert_ne!(first, changed);
        assert!(first.to_string_lossy().contains(&first_hash));
        assert!(changed.to_string_lossy().contains(&changed_hash));
    }

    #[test]
    fn legacy_export_without_document_identity_still_requires_the_configured_filename() {
        let directory = tempdir().unwrap();
        let document = configured_document(directory.path());
        let export = BaselineExport {
            source_file_name: Some("Different.pdf".to_owned()),
            document_id: None,
            based_on: None,
            pages: vec![inkbridge_convert::BaselinePage {
                page_index: 0,
                strokes: Vec::new(),
            }],
        };
        let error =
            validate_supernote_export_identity(&export, &document, "page-0001.json").unwrap_err();
        assert!(error.contains("legacy Supernote export"));
    }

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

    #[cfg(unix)]
    #[test]
    fn hard_link_fallback_publishes_without_overwriting() {
        let directory = tempdir().unwrap();
        let source = directory.path().join("broker.part");
        let destination = directory.path().join("book.pdf");
        fs::write(&source, b"broker view").unwrap();

        hard_link_create_only(&source, &destination).unwrap();

        assert_eq!(fs::read(&destination).unwrap(), b"broker view");
        assert!(!source.exists());
    }

    #[cfg(unix)]
    #[test]
    fn hard_link_fallback_rejects_an_existing_destination() {
        let directory = tempdir().unwrap();
        let source = directory.path().join("broker.part");
        let destination = directory.path().join("book.pdf");
        fs::write(&source, b"broker view").unwrap();
        fs::write(&destination, b"new local edit").unwrap();

        let error = hard_link_create_only(&source, &destination)
            .expect_err("the create-only fallback must not overwrite");

        assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(&source).unwrap(), b"broker view");
        assert_eq!(fs::read(&destination).unwrap(), b"new local edit");
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

    #[cfg(windows)]
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

    #[cfg(unix)]
    #[test]
    fn completed_publication_preserves_an_interrupted_staged_backup() {
        let directory = tempdir().unwrap();
        let destination = directory.path().join("book.pdf");
        let backup = sibling_temporary(&destination, "previous");
        let published = b"broker view";
        fs::write(&destination, published).unwrap();
        fs::write(&backup, b"previous broker view").unwrap();

        let error = reconcile_staged_backup(&destination, &sha256_hex(published))
            .expect_err("Unix recovery must retain the predecessor");

        assert!(error.contains("cannot safely retire staged backup"));
        assert_eq!(fs::read(&destination).unwrap(), published);
        assert_eq!(fs::read(&backup).unwrap(), b"previous broker view");
    }

    #[cfg(unix)]
    #[test]
    fn conditional_replace_restores_a_leaf_symlink_created_after_validation() {
        use std::os::unix::fs::symlink;

        let directory = tempdir().unwrap();
        let source = directory.path().join("broker.part");
        let destination = directory.path().join("book.pdf");
        let target = directory.path().join("sync-client-book.pdf");
        fs::write(&source, b"broker view").unwrap();
        fs::write(&target, b"expected old view").unwrap();
        symlink(&target, &destination).unwrap();

        assert!(!replace_file(
            &source,
            &destination,
            Some(&FileIdentity::Sha256(sha256_hex(b"expected old view"))),
        )
        .unwrap());
        assert!(fs::symlink_metadata(&destination)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(fs::read(&target).unwrap(), b"expected old view");
        assert!(!source.exists());
        assert!(!sibling_temporary(&destination, "previous").exists());
    }

    #[cfg(unix)]
    #[test]
    fn recovery_preserves_a_recreated_dangling_destination_symlink() {
        use std::os::unix::fs::symlink;

        let directory = tempdir().unwrap();
        let destination = directory.path().join("book.pdf");
        let backup = sibling_temporary(&destination, "previous");
        let missing_target = directory.path().join("sync-client-missing.pdf");
        fs::write(&backup, b"staged broker predecessor").unwrap();
        symlink(&missing_target, &destination).unwrap();

        let error = reconcile_staged_backup(&destination, &sha256_hex(b"broker view"))
            .expect_err("the recreated symlink must block restoration");

        assert!(error.contains("recreated"));
        assert!(fs::symlink_metadata(&destination)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(fs::read(&backup).unwrap(), b"staged broker predecessor");
        assert!(!missing_target.exists());
    }

    #[cfg(unix)]
    #[test]
    fn recovery_preserves_a_recreated_symlink_to_published_bytes() {
        use std::os::unix::fs::symlink;

        let directory = tempdir().unwrap();
        let destination = directory.path().join("book.pdf");
        let backup = sibling_temporary(&destination, "previous");
        let target = directory.path().join("sync-client-book.pdf");
        let published = b"published broker view";
        fs::write(&backup, b"staged broker predecessor").unwrap();
        fs::write(&target, published).unwrap();
        symlink(&target, &destination).unwrap();

        let error = reconcile_staged_backup(&destination, &sha256_hex(published))
            .expect_err("the recreated symlink must preserve the staged backup");

        assert!(error.contains("recreated as a symlink"));
        assert!(fs::symlink_metadata(&destination)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(fs::read(&target).unwrap(), published);
        assert_eq!(fs::read(&backup).unwrap(), b"staged broker predecessor");
    }

    #[cfg(unix)]
    #[test]
    fn recovery_preserves_backup_when_destination_becomes_symlink_after_open() {
        use std::os::unix::fs::symlink;

        let directory = tempdir().unwrap();
        let destination = directory.path().join("book.pdf");
        let backup = sibling_temporary(&destination, "previous");
        let target = directory.path().join("sync-client-book.pdf");
        let published = b"published broker view";
        fs::write(&destination, published).unwrap();
        fs::write(&backup, b"staged broker predecessor").unwrap();
        fs::write(&target, published).unwrap();

        let error = reconcile_staged_backup_with(&destination, &sha256_hex(published), || {
            fs::remove_file(&destination).map_err(|error| error.to_string())?;
            symlink(&target, &destination).map_err(|error| error.to_string())
        })
        .expect_err("a symlink replacement after open must preserve the backup");

        assert!(error.contains("changed filesystem entry"));
        assert!(fs::symlink_metadata(&destination)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(fs::read(&target).unwrap(), published);
        assert_eq!(fs::read(&backup).unwrap(), b"staged broker predecessor");
    }

    #[cfg(unix)]
    #[test]
    fn recovery_rejects_regular_path_replacement_after_reopen() {
        let directory = tempdir().unwrap();
        let destination = directory.path().join("book.pdf");
        let backup = sibling_temporary(&destination, "previous");
        let replacement = directory.path().join("sync-client-book.pdf");
        fs::write(&destination, b"published broker view").unwrap();
        fs::write(&backup, b"staged broker predecessor").unwrap();
        fs::write(&replacement, b"new local edit").unwrap();
        let opened = open_installed_regular_file(&destination).unwrap();

        let error = open_same_installed_regular_file_with(&destination, &opened, || {
            fs::rename(&replacement, &destination).map_err(|error| error.to_string())
        })
        .expect_err("a regular pathname replacement must fail identity validation");

        assert!(error.contains("changed filesystem entry"));
        assert_eq!(fs::read(&destination).unwrap(), b"new local edit");
        assert_eq!(fs::read(&backup).unwrap(), b"staged broker predecessor");
    }
}

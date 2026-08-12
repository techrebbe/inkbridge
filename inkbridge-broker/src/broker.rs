use crate::model::*;
use crate::pdf_view::write_boox_view_with_tombstones_owned;
use crate::storage::*;
use inkbridge_convert::{
    build_manifest, parse_baseline_bytes, Manifest, Operation, StrokeSnapshot,
};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Path, PathBuf};

const GENERATED_BY_KEY: &str = "inkbridge-generated-by";
const GENERATED_EVENT_KEY: &str = "inkbridge-event-id";
const GENERATED_DOCUMENT_KEY: &str = "inkbridge-document-id";
const GENERATED_REVISIONS_KEY: &str = "inkbridge-source-revisions";
const GENERATED_CONTENT_HASH_KEY: &str = "inkbridge-content-sha256";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BrokerError {
    InvalidEvent(String),
    MissingObject(String),
    CorruptState(String),
    Conversion(String),
    StaleDestination {
        path: String,
        expected_hash: Option<String>,
        actual_hash: Option<String>,
    },
    ConditionalWrite(CommitError),
    Storage(String),
}

impl fmt::Display for BrokerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidEvent(message)
            | Self::MissingObject(message)
            | Self::CorruptState(message)
            | Self::Conversion(message)
            | Self::Storage(message) => formatter.write_str(message),
            Self::StaleDestination {
                path,
                expected_hash,
                actual_hash,
            } => write!(
                formatter,
                "destination {path} changed since the broker generated it (expected {expected_hash:?}, found {actual_hash:?})"
            ),
            Self::ConditionalWrite(error) => write!(formatter, "conditional write failed: {error:?}"),
        }
    }
}

impl std::error::Error for BrokerError {}

pub struct Broker {
    normalized_y_offset: f64,
}

impl Default for Broker {
    fn default() -> Self {
        Self {
            normalized_y_offset: -0.0008,
        }
    }
}

impl Broker {
    pub fn with_normalized_y_offset(normalized_y_offset: f64) -> Self {
        Self {
            normalized_y_offset,
        }
    }

    pub fn register_document<S: BrokerStorage>(
        &self,
        storage: &mut S,
        original_file_name: &str,
        original_pdf: &[u8],
    ) -> Result<CanonicalDocumentState, BrokerError> {
        if original_file_name.trim().is_empty() {
            return Err(BrokerError::InvalidEvent(
                "original file name must not be empty".to_owned(),
            ));
        }
        // Parsing now prevents registering a hash-stable but unusable original.
        let original_document = lopdf::Document::load_mem(original_pdf).map_err(|error| {
            BrokerError::InvalidEvent(format!("immutable original is not a readable PDF: {error}"))
        })?;
        let original_page_count = original_document.get_pages().len();
        drop(original_document);
        let document_id = stable_document_id(original_pdf);
        let original_path = original_path(&document_id);
        let state_path = state_path(&document_id);
        if let Some(existing) = storage.read(&state_path).map_err(BrokerError::Storage)? {
            let mut state = decode_state(&existing.bytes)?;
            if state.original_pdf_sha256 != sha256_hex(original_pdf) {
                return Err(BrokerError::InvalidEvent(
                    "document id collision with a different immutable original".to_owned(),
                ));
            }
            // States created before originalPageCount was introduced deserialize it as zero.
            // Persist the value while the immutable original is already available so compact
            // BOOX manifests cannot bypass page-bound validation for legacy documents.
            if state.original_page_count == 0 {
                state.original_page_count = original_page_count;
                storage
                    .commit(vec![state_write(
                        &state_path,
                        &state,
                        GenerationPrecondition::Match(existing.generation),
                    )?])
                    .map_err(BrokerError::ConditionalWrite)?;
            }
            return Ok(state);
        }
        let state = CanonicalDocumentState {
            schema_version: STATE_SCHEMA_VERSION,
            document_id: document_id.clone(),
            original_object_path: original_path.clone(),
            original_pdf_sha256: sha256_hex(original_pdf),
            original_file_name: original_file_name.to_owned(),
            original_page_count,
            state_revision: 0,
            boox: DeviceRevision::default(),
            supernote: DeviceRevision::default(),
            last_common_revisions: RevisionPair::default(),
            processed_event_ids: BTreeSet::new(),
            strokes: BTreeMap::new(),
            source_generations: BTreeMap::new(),
            generated_views: BTreeMap::new(),
            conflicts: Vec::new(),
        };
        let writes = vec![
            ConditionalWrite {
                path: original_path,
                bytes: blob(original_pdf.to_vec()),
                metadata: BTreeMap::from([
                    ("inkbridge-kind".to_owned(), "immutable-original".to_owned()),
                    (GENERATED_DOCUMENT_KEY.to_owned(), document_id),
                ]),
                precondition: GenerationPrecondition::DoesNotExist,
            },
            state_write(&state_path, &state, GenerationPrecondition::DoesNotExist)?,
        ];
        storage
            .commit(writes)
            .map_err(BrokerError::ConditionalWrite)?;
        Ok(state)
    }

    pub fn process<S: BrokerStorage>(
        &self,
        storage: &mut S,
        event: &StorageEvent,
    ) -> Result<ProcessOutcome, BrokerError> {
        validate_event(event)?;
        let state_path = state_path(&event.document_id);
        let state_object = storage
            .read(&state_path)
            .map_err(BrokerError::Storage)?
            .ok_or_else(|| {
                BrokerError::MissingObject(format!(
                    "canonical state does not exist for {}",
                    event.document_id
                ))
            })?;
        let mut state = decode_state(&state_object.bytes)?;
        if state.document_id != event.document_id {
            return Err(BrokerError::CorruptState(
                "state path contains a different document id".to_owned(),
            ));
        }
        if state.processed_event_ids.contains(&event.event_id) {
            return Ok(ProcessOutcome::Duplicate {
                document_id: event.document_id.clone(),
                event_id: event.event_id.clone(),
            });
        }

        let source = storage
            .read_generation(&event.object_path, event.source_generation)
            .map_err(BrokerError::Storage)?;
        let Some(source) = source else {
            if storage
                .read(&event.object_path)
                .map_err(BrokerError::Storage)?
                .is_some_and(|latest| latest.generation > event.source_generation)
            {
                mark_event_only(
                    storage,
                    &mut state,
                    &state_path,
                    state_object.generation,
                    event,
                )?;
                return Ok(ProcessOutcome::IgnoredStaleSource {
                    document_id: event.document_id.clone(),
                    event_id: event.event_id.clone(),
                });
            }
            return Err(BrokerError::MissingObject(format!(
                "source generation {}@{} does not exist",
                event.object_path, event.source_generation
            )));
        };
        let actual_hash = sha256_hex(&source.bytes);
        if !event.content_sha256.is_empty() && actual_hash != event.content_sha256 {
            return Err(BrokerError::InvalidEvent(format!(
                "source content hash mismatch: event={}, actual={actual_hash}",
                event.content_sha256
            )));
        }
        let mut effective_event = event.clone();
        effective_event.content_sha256.clone_from(&actual_hash);
        let event = &effective_event;
        if is_broker_output(event, &source.metadata, &actual_hash) {
            mark_event_only(
                storage,
                &mut state,
                &state_path,
                state_object.generation,
                event,
            )?;
            return Ok(ProcessOutcome::IgnoredBrokerOutput {
                document_id: event.document_id.clone(),
                event_id: event.event_id.clone(),
            });
        }
        let current = state.revisions();
        let source_state = state.device(event.source);
        if event.source_revision <= source_state.revision {
            if event.source_revision == source_state.revision
                && event.content_sha256 != source_state.content_sha256
            {
                return self.preserve_conflict(
                    storage,
                    state,
                    state_object.generation,
                    event,
                    &source.bytes,
                );
            }
            mark_event_only(
                storage,
                &mut state,
                &state_path,
                state_object.generation,
                event,
            )?;
            return Ok(ProcessOutcome::IgnoredStaleSource {
                document_id: event.document_id.clone(),
                event_id: event.event_id.clone(),
            });
        }
        if event.source_revision != event.based_on.get(event.source) + 1 {
            return Err(BrokerError::InvalidEvent(format!(
                "source revision {} must immediately follow based-on revision {}",
                event.source_revision,
                event.based_on.get(event.source)
            )));
        }
        if event.based_on != current {
            return self.preserve_conflict(
                storage,
                state,
                state_object.generation,
                event,
                &source.bytes,
            );
        }

        let source_bytes = source.bytes;
        let (destination_path, output_bytes, boox_source_file_name) = match event.source {
            DeviceSide::Boox => {
                let manifest = match event.payload_kind {
                    DevicePayloadKind::DeviceView => {
                        self.boox_to_supernote(&state, event, source_bytes)?
                    }
                    DevicePayloadKind::BooxOperationManifest => {
                        self.validate_boox_manifest(&state, &source_bytes)?
                    }
                };
                let source_file_name = manifest.document.source_file_name.clone();
                apply_manifest(&mut state, &manifest, event);
                let bytes = serde_json::to_vec_pretty(&manifest)
                    .map_err(|error| BrokerError::Conversion(error.to_string()))?;
                (
                    supernote_manifest_path(&event.document_id, &event.event_id),
                    add_newline(bytes),
                    Some(source_file_name),
                )
            }
            DeviceSide::Supernote => {
                apply_supernote_export(&mut state, event, &source_bytes)?;
                drop(source_bytes);
                let original = storage
                    .read(&state.original_object_path)
                    .map_err(BrokerError::Storage)?
                    .ok_or_else(|| {
                        BrokerError::MissingObject(state.original_object_path.clone())
                    })?;
                if sha256_hex(&original.bytes) != state.original_pdf_sha256 {
                    return Err(BrokerError::CorruptState(
                        "immutable original PDF hash changed".to_owned(),
                    ));
                }
                let active = state
                    .strokes
                    .values()
                    .filter(|stroke| stroke.tombstone.is_none())
                    .map(|stroke| stroke.snapshot.clone())
                    .collect::<Vec<_>>();
                let tombstones = state
                    .strokes
                    .values()
                    .filter(|stroke| stroke.tombstone.is_some())
                    .map(|stroke| stroke.stroke_id.clone())
                    .collect::<Vec<_>>();
                let pdf = write_boox_view_with_tombstones_owned(original.bytes, active, tombstones)
                    .map_err(BrokerError::Conversion)?;
                (boox_view_path(&state), pdf, None)
            }
        };

        let revisions = {
            let device = state.device_mut(event.source);
            device.revision = event.source_revision;
            device.content_sha256.clone_from(&event.content_sha256);
            device.source_generation = event.source_generation;
            device.source_object_path.clone_from(&event.object_path);
            // New states use the immutable device object generation directly.
            // accepted_object_path remains readable for backward compatibility.
            device.accepted_object_path.clear();
            if event.source == DeviceSide::Boox {
                device.source_file_name = boox_source_file_name;
            }
            state.revisions()
        };
        state.last_common_revisions = revisions;
        state
            .source_generations
            .insert(event.object_path.clone(), event.source_generation);
        if let Some(consumed_view) = state.generated_views.get_mut(&event.object_path) {
            // The device may edit its generated view in place. Once that input
            // has been accepted, its bytes are the new known-safe destination
            // baseline; only a subsequent, unconsumed change should trip the
            // stale-destination guard.
            consumed_view
                .content_sha256
                .clone_from(&event.content_sha256);
            consumed_view.source_revisions = revisions;
            consumed_view.event_id.clone_from(&event.event_id);
        }
        state.processed_event_ids.insert(event.event_id.clone());
        state.state_revision += 1;

        let output_hash = sha256_hex(&output_bytes);
        let destination = storage
            .read(&destination_path)
            .map_err(BrokerError::Storage)?;
        let destination_precondition =
            destination_precondition(&state, &destination_path, &destination)?;
        state.generated_views.insert(
            destination_path.clone(),
            GeneratedView {
                object_path: destination_path.clone(),
                content_sha256: output_hash.clone(),
                source_revisions: revisions,
                event_id: event.event_id.clone(),
            },
        );
        let marker = BrokerOutputMarker {
            producer: BROKER_PRODUCER.to_owned(),
            event_id: event.event_id.clone(),
            document_id: event.document_id.clone(),
            source_revisions: revisions,
        };
        let writes = vec![
            ConditionalWrite {
                path: destination_path.clone(),
                bytes: blob(output_bytes),
                metadata: output_metadata(&marker, &output_hash),
                precondition: destination_precondition,
            },
            state_write(
                &state_path,
                &state,
                GenerationPrecondition::Match(state_object.generation),
            )?,
        ];
        let committed = storage
            .commit(writes)
            .map_err(BrokerError::ConditionalWrite)?;
        Ok(ProcessOutcome::Applied {
            document_id: event.document_id.clone(),
            event_id: event.event_id.clone(),
            destination_path,
            destination_generation: committed[0].generation,
            source_revisions: revisions,
        })
    }

    fn boox_to_supernote(
        &self,
        state: &CanonicalDocumentState,
        event: &StorageEvent,
        pdf: Blob,
    ) -> Result<Manifest, BrokerError> {
        let work =
            tempfile::tempdir().map_err(|error| BrokerError::Conversion(error.to_string()))?;
        let file_name = Path::new(&event.object_path)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("document.pdf");
        let pdf_path = work.path().join(file_name);
        std::fs::write(&pdf_path, &pdf)
            .map_err(|error| BrokerError::Conversion(error.to_string()))?;
        drop(pdf);
        let baselines = write_baselines(work.path(), state)?;
        build_manifest(&pdf_path, &baselines, self.normalized_y_offset)
            .map_err(BrokerError::Conversion)
    }

    fn validate_boox_manifest(
        &self,
        state: &CanonicalDocumentState,
        bytes: &[u8],
    ) -> Result<Manifest, BrokerError> {
        let mut manifest: Manifest = serde_json::from_slice(bytes).map_err(|error| {
            BrokerError::InvalidEvent(format!("BOOX operation manifest is invalid JSON: {error}"))
        })?;
        if manifest.schema_version != 1 || manifest.source != "boox-neoreader-embedded-pdf" {
            return Err(BrokerError::InvalidEvent(
                "BOOX operation manifest has an unsupported schema or source".to_owned(),
            ));
        }
        if state.original_page_count != 0
            && manifest.document.page_count != state.original_page_count
        {
            return Err(BrokerError::InvalidEvent(format!(
                "BOOX operation manifest page count {} does not match original page count {}",
                manifest.document.page_count, state.original_page_count
            )));
        }
        // Compact inputs do not necessarily carry a baseline-derived document guard. The
        // canonical Supernote file name is authoritative and prevents the headless plugin from
        // applying otherwise valid operations to whichever document happens to be open.
        manifest.document.target_file_names = vec![state
            .supernote
            .source_file_name
            .clone()
            .unwrap_or_else(|| state.original_file_name.clone())];
        for operation in &manifest.operations {
            let (source_uuid, page_index, snapshot) = match operation {
                Operation::UpsertStroke {
                    source_uuid,
                    page_index,
                    after,
                    ..
                } => (source_uuid, page_index, after),
                Operation::DeleteStroke {
                    source_uuid,
                    page_index,
                    before,
                } => (source_uuid, page_index, before),
            };
            if source_uuid.trim().is_empty()
                || source_uuid != &snapshot.source_uuid
                || page_index != &snapshot.page_index
                || usize::try_from(*page_index)
                    .ok()
                    .is_none_or(|page| page >= manifest.document.page_count)
                || snapshot.samples.len() < 2
                || snapshot
                    .samples
                    .iter()
                    .flatten()
                    .any(|value| !value.is_finite())
            {
                return Err(BrokerError::InvalidEvent(
                    "BOOX operation manifest contains an invalid stroke operation".to_owned(),
                ));
            }
        }
        let stroke_ids = manifest
            .operations
            .iter()
            .map(|operation| match operation {
                Operation::UpsertStroke { source_uuid, .. }
                | Operation::DeleteStroke { source_uuid, .. } => source_uuid.as_str(),
            })
            .collect::<BTreeSet<_>>();
        for source_uuid in stroke_ids {
            let mut deletes = manifest
                .operations
                .iter()
                .filter_map(|operation| match operation {
                    Operation::DeleteStroke {
                        source_uuid: candidate,
                        before,
                        ..
                    } if candidate == source_uuid => Some(before),
                    _ => None,
                });
            let delete = deletes.next();
            if deletes.next().is_some() {
                return Err(BrokerError::InvalidEvent(format!(
                    "BOOX operation manifest contains duplicate deletes for {source_uuid}"
                )));
            }
            let mut upserts = manifest
                .operations
                .iter()
                .filter_map(|operation| match operation {
                    Operation::UpsertStroke {
                        source_uuid: candidate,
                        before,
                        after,
                        ..
                    } if candidate == source_uuid => Some((before, after)),
                    _ => None,
                });
            let upsert = upserts.next();
            if upserts.next().is_some() {
                return Err(BrokerError::InvalidEvent(format!(
                    "BOOX operation manifest contains duplicate upserts for {source_uuid}"
                )));
            }
            let active = state
                .strokes
                .get(source_uuid)
                .filter(|canonical| canonical.tombstone.is_none());
            let valid_operation_set = match (delete, upsert, active) {
                (Some(deleted), None, Some(canonical)) => canonical.snapshot == *deleted,
                (None, Some((Some(before), after)), Some(canonical)) => {
                    canonical.snapshot == *before
                        && after.page_index == canonical.snapshot.page_index
                }
                (None, Some((None, _)), None) => !state.strokes.contains_key(source_uuid),
                (Some(deleted), Some((None, after)), Some(canonical)) => {
                    canonical.snapshot == *deleted
                        && after.page_index != canonical.snapshot.page_index
                }
                _ => false,
            };
            if !valid_operation_set {
                return Err(BrokerError::InvalidEvent(format!(
                    "BOOX operation manifest does not match active canonical stroke {source_uuid}"
                )));
            }
        }
        Ok(manifest)
    }

    fn preserve_conflict<S: BrokerStorage>(
        &self,
        storage: &mut S,
        mut state: CanonicalDocumentState,
        state_generation: u64,
        event: &StorageEvent,
        input: &[u8],
    ) -> Result<ProcessOutcome, BrokerError> {
        let current = state.revisions();
        let extension = Path::new(&event.object_path)
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or("bin");
        let preserved_path = format!(
            "Conflicts/{}/{}/incoming.{extension}",
            event.document_id,
            event_path_segment(&event.event_id)
        );
        let mut conflict_writes = vec![ConditionalWrite {
            path: preserved_path.clone(),
            bytes: blob(input.to_vec()),
            metadata: BTreeMap::from([
                ("inkbridge-kind".to_owned(), "conflict-input".to_owned()),
                (GENERATED_DOCUMENT_KEY.to_owned(), event.document_id.clone()),
                (GENERATED_EVENT_KEY.to_owned(), event.event_id.clone()),
            ]),
            precondition: GenerationPrecondition::DoesNotExist,
        }];
        let mut competing_preserved_paths = Vec::new();
        for side in [DeviceSide::Boox, DeviceSide::Supernote] {
            let device = state.device(side);
            if device.revision <= event.based_on.get(side) || device.source_object_path.is_empty() {
                continue;
            }
            let evidence_path = if device.accepted_object_path.is_empty() {
                &device.source_object_path
            } else {
                &device.accepted_object_path
            };
            let current_input = if device.accepted_object_path.is_empty() {
                storage.read_generation(evidence_path, device.source_generation)
            } else {
                storage.read(evidence_path)
            }
            .map_err(BrokerError::Storage)?
            .ok_or_else(|| BrokerError::MissingObject(evidence_path.clone()))?;
            let evidence_hash = sha256_hex(&current_input.bytes);
            if evidence_hash != device.content_sha256 {
                return Err(BrokerError::CorruptState(format!(
                    "accepted {side:?} source evidence hash changed: expected {}, found {evidence_hash}",
                    device.content_sha256
                )));
            }
            let current_extension = Path::new(evidence_path)
                .extension()
                .and_then(|value| value.to_str())
                .unwrap_or("bin");
            let side_name = match side {
                DeviceSide::Boox => "boox",
                DeviceSide::Supernote => "supernote",
            };
            let current_path = format!(
                "Conflicts/{}/{}/current-{side_name}.{current_extension}",
                event.document_id,
                event_path_segment(&event.event_id),
            );
            competing_preserved_paths.push(current_path.clone());
            conflict_writes.push(ConditionalWrite {
                path: current_path,
                bytes: current_input.bytes,
                metadata: BTreeMap::from([
                    (
                        "inkbridge-kind".to_owned(),
                        "conflict-competing-input".to_owned(),
                    ),
                    (GENERATED_DOCUMENT_KEY.to_owned(), event.document_id.clone()),
                    (GENERATED_EVENT_KEY.to_owned(), event.event_id.clone()),
                ]),
                precondition: GenerationPrecondition::DoesNotExist,
            });
        }
        state.conflicts.push(PreservedInput {
            event_id: event.event_id.clone(),
            source: event.source,
            object_path: event.object_path.clone(),
            preserved_path: preserved_path.clone(),
            competing_preserved_paths,
            source_generation: event.source_generation,
            source_revision: event.source_revision,
            content_sha256: event.content_sha256.clone(),
            based_on: event.based_on,
            current_revisions: current,
        });
        state.processed_event_ids.insert(event.event_id.clone());
        state.state_revision += 1;
        conflict_writes.push(state_write(
            &state_path(&event.document_id),
            &state,
            GenerationPrecondition::Match(state_generation),
        )?);
        storage
            .commit(conflict_writes)
            .map_err(BrokerError::ConditionalWrite)?;
        Ok(ProcessOutcome::Conflict {
            document_id: event.document_id.clone(),
            event_id: event.event_id.clone(),
            preserved_path,
            current_revisions: current,
            based_on: event.based_on,
        })
    }
}

fn validate_event(event: &StorageEvent) -> Result<(), BrokerError> {
    if event.schema_version != EVENT_SCHEMA_VERSION {
        return Err(BrokerError::InvalidEvent(format!(
            "unsupported event schema version {}",
            event.schema_version
        )));
    }
    if event.event_id.trim().is_empty()
        || event.document_id.trim().is_empty()
        || event.object_path.trim().is_empty()
    {
        return Err(BrokerError::InvalidEvent(
            "event id, document id, and object path are required".to_owned(),
        ));
    }
    Ok(())
}

fn is_broker_output(
    event: &StorageEvent,
    metadata: &BTreeMap<String, String>,
    content_sha256: &str,
) -> bool {
    let envelope_matches = event.broker_output.as_ref().is_some_and(|marker| {
        marker.producer == BROKER_PRODUCER
            && marker.document_id == event.document_id
            && metadata.get(GENERATED_EVENT_KEY) == Some(&marker.event_id)
    });
    let metadata_matches = metadata.get(GENERATED_BY_KEY).map(String::as_str)
        == Some(BROKER_PRODUCER)
        && metadata.get(GENERATED_DOCUMENT_KEY) == Some(&event.document_id)
        && metadata.get(GENERATED_CONTENT_HASH_KEY).map(String::as_str) == Some(content_sha256);
    metadata_matches && envelope_matches
}

fn output_metadata(marker: &BrokerOutputMarker, hash: &str) -> BTreeMap<String, String> {
    BTreeMap::from([
        (GENERATED_BY_KEY.to_owned(), marker.producer.clone()),
        (GENERATED_EVENT_KEY.to_owned(), marker.event_id.clone()),
        (
            GENERATED_DOCUMENT_KEY.to_owned(),
            marker.document_id.clone(),
        ),
        (
            GENERATED_REVISIONS_KEY.to_owned(),
            format!(
                "{}:{}",
                marker.source_revisions.boox, marker.source_revisions.supernote
            ),
        ),
        (GENERATED_CONTENT_HASH_KEY.to_owned(), hash.to_owned()),
    ])
}

fn destination_precondition(
    state: &CanonicalDocumentState,
    path: &str,
    current: &Option<StoredObject>,
) -> Result<GenerationPrecondition, BrokerError> {
    match (state.generated_views.get(path), current) {
        (None, None) => Ok(GenerationPrecondition::DoesNotExist),
        (Some(previous), Some(current)) => {
            let actual_hash = sha256_hex(&current.bytes);
            if actual_hash != previous.content_sha256 {
                return Err(BrokerError::StaleDestination {
                    path: path.to_owned(),
                    expected_hash: Some(previous.content_sha256.clone()),
                    actual_hash: Some(actual_hash),
                });
            }
            Ok(GenerationPrecondition::Match(current.generation))
        }
        (Some(previous), None) => Err(BrokerError::StaleDestination {
            path: path.to_owned(),
            expected_hash: Some(previous.content_sha256.clone()),
            actual_hash: None,
        }),
        (None, Some(current)) => Err(BrokerError::StaleDestination {
            path: path.to_owned(),
            expected_hash: None,
            actual_hash: Some(sha256_hex(&current.bytes)),
        }),
    }
}

fn mark_event_only<S: BrokerStorage>(
    storage: &mut S,
    state: &mut CanonicalDocumentState,
    path: &str,
    generation: u64,
    event: &StorageEvent,
) -> Result<(), BrokerError> {
    state.processed_event_ids.insert(event.event_id.clone());
    state
        .source_generations
        .insert(event.object_path.clone(), event.source_generation);
    state.state_revision += 1;
    storage
        .commit(vec![state_write(
            path,
            state,
            GenerationPrecondition::Match(generation),
        )?])
        .map_err(BrokerError::ConditionalWrite)?;
    Ok(())
}

fn apply_manifest(state: &mut CanonicalDocumentState, manifest: &Manifest, event: &StorageEvent) {
    let mut revisions = event.based_on;
    revisions.set(event.source, event.source_revision);
    let upserted_ids = manifest
        .operations
        .iter()
        .filter_map(|operation| match operation {
            Operation::UpsertStroke { source_uuid, .. } => Some(source_uuid.as_str()),
            Operation::DeleteStroke { .. } => None,
        })
        .collect::<BTreeSet<_>>();

    // A cross-page move is encoded as a delete on the old page plus an upsert
    // on the new page. Apply only terminal deletions here; the upsert pass below
    // makes the result independent of manifest page ordering.
    for operation in &manifest.operations {
        let Operation::DeleteStroke {
            source_uuid,
            before,
            ..
        } = operation
        else {
            continue;
        };
        if upserted_ids.contains(source_uuid.as_str()) {
            continue;
        }
        let canonical =
            state
                .strokes
                .entry(source_uuid.clone())
                .or_insert_with(|| CanonicalStroke {
                    stroke_id: source_uuid.clone(),
                    snapshot: before.clone(),
                    last_modified_by: event.source,
                    source_revisions: revisions,
                    tombstone: None,
                });
        canonical.last_modified_by = event.source;
        canonical.source_revisions = revisions;
        canonical.tombstone = Some(Tombstone {
            deleted_by: event.source,
            deleted_at_revision: event.source_revision,
            event_id: event.event_id.clone(),
        });
    }
    for operation in &manifest.operations {
        let Operation::UpsertStroke {
            source_uuid, after, ..
        } = operation
        else {
            continue;
        };
        state.strokes.insert(
            source_uuid.clone(),
            CanonicalStroke {
                stroke_id: source_uuid.clone(),
                snapshot: after.clone(),
                last_modified_by: event.source,
                source_revisions: revisions,
                tombstone: None,
            },
        );
    }
}

fn apply_supernote_export(
    state: &mut CanonicalDocumentState,
    event: &StorageEvent,
    bytes: &[u8],
) -> Result<(), BrokerError> {
    let export =
        parse_baseline_bytes(bytes, &event.object_path).map_err(BrokerError::Conversion)?;
    if let Some(source_file_name) = export.source_file_name.clone() {
        state.supernote.source_file_name = Some(source_file_name);
    }
    let page_index = export.page_index;
    let incoming_ids = export
        .strokes
        .iter()
        .map(|stroke| stroke.source_uuid.clone())
        .collect::<BTreeSet<_>>();
    let mut revisions = event.based_on;
    revisions.set(event.source, event.source_revision);
    for canonical in state.strokes.values_mut().filter(|stroke| {
        stroke.snapshot.page_index == page_index
            && stroke.tombstone.is_none()
            && !incoming_ids.contains(&stroke.stroke_id)
    }) {
        canonical.last_modified_by = event.source;
        canonical.source_revisions = revisions;
        canonical.tombstone = Some(Tombstone {
            deleted_by: event.source,
            deleted_at_revision: event.source_revision,
            event_id: event.event_id.clone(),
        });
    }
    for snapshot in export.strokes {
        let stroke_id = snapshot.source_uuid.clone();
        state.strokes.insert(
            stroke_id.clone(),
            CanonicalStroke {
                stroke_id,
                snapshot,
                last_modified_by: event.source,
                source_revisions: revisions,
                tombstone: None,
            },
        );
    }
    Ok(())
}

fn write_baselines(
    directory: &Path,
    state: &CanonicalDocumentState,
) -> Result<Vec<PathBuf>, BrokerError> {
    let mut pages = BTreeMap::<u32, Vec<&StrokeSnapshot>>::new();
    for canonical in state
        .strokes
        .values()
        .filter(|stroke| stroke.tombstone.is_none())
    {
        pages
            .entry(canonical.snapshot.page_index)
            .or_default()
            .push(&canonical.snapshot);
    }
    // build_manifest derives its Supernote document guard from baseline
    // exports. Keep that guard even before the first stroke exists (or after
    // every stroke has been tombstoned) by emitting one empty page baseline.
    if pages.is_empty() {
        pages.insert(0, Vec::new());
    }
    let mut paths = Vec::new();
    for (page_index, mut strokes) in pages {
        strokes.sort_by(|left, right| left.source_uuid.cmp(&right.source_uuid));
        let exported = strokes
            .iter()
            .map(|stroke| {
                json!({
                    "sourceUuid": stroke.source_uuid,
                    "sourceKey": stroke.source_uuid,
                    "layerNum": stroke.native_style.layer_num,
                    "thickness": stroke.native_style.thickness,
                    "penColor": stroke.native_style.pen_color,
                    "penType": stroke.native_style.pen_type,
                    "samples": stroke.samples,
                })
            })
            .collect::<Vec<_>>();
        let page = json!({
            "sourceFileName": state.supernote.source_file_name.as_deref().unwrap_or(&state.original_file_name),
            "pageIndex": page_index,
            "strokes": exported,
        });
        let path = directory.join(format!("baseline-page-{page_index}.json"));
        std::fs::write(
            &path,
            serde_json::to_vec(&page)
                .map_err(|error| BrokerError::Conversion(error.to_string()))?,
        )
        .map_err(|error| BrokerError::Conversion(error.to_string()))?;
        paths.push(path);
    }
    Ok(paths)
}

fn state_write(
    path: &str,
    state: &CanonicalDocumentState,
    precondition: GenerationPrecondition,
) -> Result<ConditionalWrite, BrokerError> {
    Ok(ConditionalWrite {
        path: path.to_owned(),
        bytes: blob(add_newline(
            serde_json::to_vec_pretty(state)
                .map_err(|error| BrokerError::CorruptState(error.to_string()))?,
        )),
        metadata: BTreeMap::from([
            ("inkbridge-kind".to_owned(), "canonical-state".to_owned()),
            (GENERATED_DOCUMENT_KEY.to_owned(), state.document_id.clone()),
        ]),
        precondition,
    })
}

fn decode_state(bytes: &[u8]) -> Result<CanonicalDocumentState, BrokerError> {
    let state: CanonicalDocumentState = serde_json::from_slice(bytes)
        .map_err(|error| BrokerError::CorruptState(error.to_string()))?;
    if state.schema_version != STATE_SCHEMA_VERSION {
        return Err(BrokerError::CorruptState(format!(
            "unsupported state schema version {}",
            state.schema_version
        )));
    }
    Ok(state)
}

fn add_newline(mut bytes: Vec<u8>) -> Vec<u8> {
    bytes.push(b'\n');
    bytes
}

fn readable_segment(value: &str) -> String {
    let segment = value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
                character
            } else {
                '_'
            }
        })
        .collect::<String>();
    if segment.is_empty() {
        "item".to_owned()
    } else {
        segment
    }
}

fn event_path_segment(event_id: &str) -> String {
    let prefix = readable_segment(event_id)
        .chars()
        .take(48)
        .collect::<String>();
    format!("{prefix}-{}", sha256_hex(event_id.as_bytes()))
}

fn safe_pdf_file_name(original_file_name: &str) -> String {
    let base_name = original_file_name
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("document.pdf");
    let stem = base_name
        .strip_suffix(".pdf")
        .or_else(|| base_name.strip_suffix(".PDF"))
        .unwrap_or(base_name);
    format!("{}.pdf", readable_segment(stem))
}

pub fn original_path(document_id: &str) -> String {
    format!("Originals/{document_id}/original.pdf")
}

pub fn state_path(document_id: &str) -> String {
    format!("Canonical/{document_id}/state.json")
}

pub fn supernote_manifest_path(document_id: &str, event_id: &str) -> String {
    format!(
        "Supernote_Folder/{document_id}/incoming/{}.operations.json",
        event_path_segment(event_id)
    )
}

pub fn boox_view_path(state: &CanonicalDocumentState) -> String {
    format!(
        "BOOX_Folder/{}/{}",
        state.document_id,
        safe_pdf_file_name(&state.original_file_name)
    )
}

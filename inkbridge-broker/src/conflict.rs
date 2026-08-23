use crate::broker::{
    add_newline, apply_manifest, decode_state, destination_precondition,
    ensure_original_page_count, event_path_segment, normalize_supernote_pen_color, output_metadata,
    refresh_manifest_id, state_write, valid_supernote_native_style, write_baselines,
    GENERATED_BY_KEY, GENERATED_DOCUMENT_KEY, GENERATED_EVENT_KEY,
};
use crate::model::*;
use crate::pdf_view::write_boox_view_with_tombstones_owned;
use crate::storage::*;
use crate::{boox_view_path, state_path, supernote_manifest_path, Broker, BrokerError};
use inkbridge_convert::{
    build_manifest, geometry_fingerprint, parse_baseline_bytes, CoordinateTransform,
    DocumentIdentity, Manifest, Operation, StrokeSnapshot, Summary,
};
use std::collections::BTreeMap;

const RESOLUTION_KIND: &str = "conflict-resolution";

#[derive(Clone)]
struct ConflictContext {
    state_object: StoredObject,
    state: CanonicalDocumentState,
    conflict: PreservedInput,
    input: StoredObject,
}

#[derive(Clone)]
struct OperationGroup {
    change: ConflictStrokeChange,
    operations: Vec<Operation>,
    safe: bool,
}

impl Broker {
    pub fn list_conflicts<S: BrokerStorage>(
        &self,
        storage: &S,
        document_id: &str,
    ) -> Result<Vec<ConflictSummary>, BrokerError> {
        let state_object = storage
            .read(&state_path(document_id))
            .map_err(BrokerError::Storage)?
            .ok_or_else(|| {
                BrokerError::MissingObject(format!(
                    "canonical state does not exist for {document_id}"
                ))
            })?;
        let state = decode_state(&state_object.bytes)?;
        if state.document_id != document_id {
            return Err(BrokerError::CorruptState(format!(
                "canonical state identity {} does not match requested document {document_id}",
                state.document_id
            )));
        }
        let current_revisions = state.revisions();
        Ok(state
            .conflicts
            .iter()
            .map(|conflict| ConflictSummary {
                document_id: document_id.to_owned(),
                conflict_event_id: conflict.event_id.clone(),
                source: conflict.source,
                source_revision: conflict.source_revision,
                based_on: conflict.based_on,
                current_revisions,
                state_revision: state.state_revision,
                payload_kind: conflict.payload_kind,
            })
            .collect())
    }

    pub fn inspect_conflict<S: BrokerStorage>(
        &self,
        storage: &S,
        document_id: &str,
        conflict_event_id: &str,
    ) -> Result<ConflictAnalysis, BrokerError> {
        let context = load_context(storage, document_id, conflict_event_id)?;
        let manifest = self.proposed_conflict_manifest(
            &context.state,
            &context.conflict,
            context.input.bytes.clone(),
        )?;
        let groups = classify_operations(&context.state, &context.conflict, &manifest.operations)?;
        Ok(analysis_from_groups(&context, &groups))
    }

    pub fn resolve_conflict<S: BrokerStorage>(
        &self,
        storage: &mut S,
        request: &ConflictResolutionRequest,
    ) -> Result<ConflictResolutionOutcome, BrokerError> {
        validate_resolution_request(request)?;
        let state_object = storage
            .read(&state_path(&request.document_id))
            .map_err(BrokerError::Storage)?
            .ok_or_else(|| {
                BrokerError::MissingObject(format!(
                    "canonical state does not exist for {}",
                    request.document_id
                ))
            })?;
        let state = decode_state(&state_object.bytes)?;
        if let Some(record) = state.resolved_conflicts.get(&request.conflict_event_id) {
            return if record.resolution_id == request.resolution_id
                && record.strategy == request.strategy
            {
                Ok(ConflictResolutionOutcome::Duplicate {
                    document_id: request.document_id.clone(),
                    conflict_event_id: request.conflict_event_id.clone(),
                    resolution_id: request.resolution_id.clone(),
                    strategy: record.strategy,
                })
            } else if record.resolution_id == request.resolution_id {
                Err(BrokerError::InvalidEvent(format!(
                    "resolution ID {} already recorded strategy {:?}; requested {:?}",
                    request.resolution_id, record.strategy, request.strategy
                )))
            } else {
                Err(BrokerError::InvalidEvent(format!(
                    "conflict {} was already resolved by {}",
                    request.conflict_event_id, record.resolution_id
                )))
            };
        }
        if state.state_revision != request.expected_state_revision
            || state.revisions() != request.expected_current_revisions
        {
            return Err(BrokerError::InvalidEvent(format!(
                "conflict analysis is stale: expected state revision {} at {:?}, found {} at {:?}",
                request.expected_state_revision,
                request.expected_current_revisions,
                state.state_revision,
                state.revisions()
            )));
        }

        let context =
            load_context_from_state(storage, state_object, state, &request.conflict_event_id)?;
        let current_source_revision = context.state.device(context.conflict.source).revision;
        let equal_source_revision = context.conflict.source_revision == current_source_revision;
        if context.conflict.source_revision < current_source_revision {
            return supersede_conflict(storage, context, request);
        }
        if equal_source_revision && request.strategy != ConflictResolutionStrategy::KeepCurrent {
            return Err(BrokerError::InvalidEvent(format!(
                "conflict {} is an alternate payload for already accepted {:?} revision {}; only keep_current is safe, and incoming changes must be re-exported as a newer revision",
                context.conflict.event_id,
                context.conflict.source,
                current_source_revision
            )));
        }

        ensure_incremental_conflict_resolution_order(&context.state, &context.conflict)?;

        let proposed = self.proposed_conflict_manifest(
            &context.state,
            &context.conflict,
            context.input.bytes.clone(),
        )?;
        let groups = classify_operations(&context.state, &context.conflict, &proposed.operations)?;
        let previous_state = context.state.clone();
        let previous_revisions = previous_state.revisions();

        let (selected_operations, applied_stroke_ids, preserved_current_stroke_ids) =
            select_operations(&groups, request.strategy);
        let mut state = context.state;
        if request.strategy != ConflictResolutionStrategy::KeepCurrent {
            let mut selected_manifest = proposed.clone();
            selected_manifest.operations = selected_operations;
            selected_manifest.summary = manifest_summary(
                &selected_manifest.operations,
                preserved_current_stroke_ids.len(),
            );
            refresh_manifest_id(&mut selected_manifest);
            let mut effective_event = conflict_event(&context.conflict, &request.document_id);
            effective_event.event_id.clone_from(&request.resolution_id);
            effective_event.based_on = previous_revisions;
            apply_manifest(&mut state, &selected_manifest, &effective_event);
        }

        if !equal_source_revision {
            // Advancing resolutions consume the preserved source revision,
            // including keep-current. Otherwise the next event from that device
            // cannot name a coherent based-on frontier and the same conflict is
            // rediscovered indefinitely.
            let device = state.device_mut(context.conflict.source);
            device.revision = context.conflict.source_revision;
            device
                .content_sha256
                .clone_from(&context.conflict.content_sha256);
            device.source_generation = context.input.generation;
            device
                .source_object_path
                .clone_from(&context.conflict.preserved_path);
            device.accepted_object_path.clear();
            device.source_file_name = Some(proposed.document.source_file_name.clone());
            state.source_generations.insert(
                context.conflict.preserved_path.clone(),
                context.input.generation,
            );
        }

        let resulting_revisions = state.revisions();
        state.last_common_revisions = resulting_revisions;
        state
            .processed_event_ids
            .insert(request.resolution_id.clone());
        state.state_revision += 1;
        state
            .conflicts
            .retain(|conflict| conflict.event_id != request.conflict_event_id);

        let original = storage
            .read(&state.original_object_path)
            .map_err(BrokerError::Storage)?
            .ok_or_else(|| BrokerError::MissingObject(state.original_object_path.clone()))?;
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
        let boox_pdf = write_boox_view_with_tombstones_owned(original.bytes, active, tombstones)
            .map_err(BrokerError::Conversion)?;
        let supernote_manifest = self.build_resolution_manifest(
            &previous_state,
            &state,
            &context.conflict,
            &context.input.bytes,
            &boox_pdf,
        )?;

        let boox_path = boox_view_path(&state);
        let boox_destination = storage.read(&boox_path).map_err(BrokerError::Storage)?;
        let boox_precondition = resolution_destination_precondition(
            &previous_state,
            &context.conflict,
            &boox_path,
            &boox_destination,
        )?;
        let boox_hash = sha256_hex(&boox_pdf);
        let marker = BrokerOutputMarker {
            producer: BROKER_PRODUCER.to_owned(),
            event_id: request.resolution_id.clone(),
            document_id: request.document_id.clone(),
            source_revisions: resulting_revisions,
        };
        state.generated_views.insert(
            boox_path.clone(),
            GeneratedView {
                object_path: boox_path.clone(),
                content_sha256: boox_hash.clone(),
                source_revisions: resulting_revisions,
                event_id: request.resolution_id.clone(),
            },
        );

        let mut writes = vec![ConditionalWrite {
            path: boox_path.clone(),
            bytes: blob(boox_pdf),
            metadata: output_metadata(&marker, &boox_hash),
            precondition: boox_precondition,
        }];
        let mut output_sides = vec![DeviceSide::Boox];

        if !supernote_manifest.operations.is_empty() {
            let manifest_path =
                supernote_manifest_path(&request.document_id, &request.resolution_id);
            let manifest_bytes = add_newline(
                serde_json::to_vec_pretty(&supernote_manifest)
                    .map_err(|error| BrokerError::Conversion(error.to_string()))?,
            );
            let manifest_hash = sha256_hex(&manifest_bytes);
            state.generated_views.insert(
                manifest_path.clone(),
                GeneratedView {
                    object_path: manifest_path.clone(),
                    content_sha256: manifest_hash.clone(),
                    source_revisions: resulting_revisions,
                    event_id: request.resolution_id.clone(),
                },
            );
            writes.push(ConditionalWrite {
                path: manifest_path,
                bytes: blob(manifest_bytes),
                metadata: output_metadata(&marker, &manifest_hash),
                precondition: GenerationPrecondition::DoesNotExist,
            });
            output_sides.push(DeviceSide::Supernote);
        }

        let marker_path =
            conflict_resolution_path(&request.document_id, &request.conflict_event_id);
        let record = ConflictResolutionRecord {
            resolution_id: request.resolution_id.clone(),
            conflict_event_id: request.conflict_event_id.clone(),
            strategy: request.strategy,
            superseded: false,
            source: context.conflict.source,
            previous_revisions,
            resulting_revisions,
            applied_stroke_ids: applied_stroke_ids.clone(),
            preserved_current_stroke_ids: preserved_current_stroke_ids.clone(),
            marker_path: marker_path.clone(),
        };
        state
            .resolved_conflicts
            .insert(request.conflict_event_id.clone(), record.clone());
        let marker_bytes = add_newline(
            serde_json::to_vec_pretty(&record)
                .map_err(|error| BrokerError::CorruptState(error.to_string()))?,
        );
        writes.push(ConditionalWrite {
            path: marker_path,
            bytes: blob(marker_bytes),
            metadata: BTreeMap::from([
                (GENERATED_BY_KEY.to_owned(), BROKER_PRODUCER.to_owned()),
                (
                    GENERATED_DOCUMENT_KEY.to_owned(),
                    request.document_id.clone(),
                ),
                (
                    GENERATED_EVENT_KEY.to_owned(),
                    request.resolution_id.clone(),
                ),
                ("inkbridge-kind".to_owned(), RESOLUTION_KIND.to_owned()),
                (
                    "inkbridge-conflict-event-id".to_owned(),
                    request.conflict_event_id.clone(),
                ),
            ]),
            precondition: GenerationPrecondition::DoesNotExist,
        });
        writes.push(state_write(
            &state_path(&request.document_id),
            &state,
            GenerationPrecondition::Match(context.state_object.generation),
        )?);

        let committed = storage
            .commit(writes)
            .map_err(BrokerError::ConditionalWrite)?;
        let outputs = output_sides
            .into_iter()
            .zip(committed.iter())
            .map(|(side, object)| GeneratedResolutionOutput {
                side,
                object_path: match side {
                    DeviceSide::Boox => boox_path.clone(),
                    DeviceSide::Supernote => {
                        supernote_manifest_path(&request.document_id, &request.resolution_id)
                    }
                },
                generation: object.generation,
            })
            .collect();

        Ok(ConflictResolutionOutcome::Resolved {
            document_id: request.document_id.clone(),
            conflict_event_id: request.conflict_event_id.clone(),
            resolution_id: request.resolution_id.clone(),
            strategy: request.strategy,
            source_revisions: resulting_revisions,
            applied_stroke_ids,
            preserved_current_stroke_ids,
            outputs,
        })
    }

    fn proposed_conflict_manifest(
        &self,
        state: &CanonicalDocumentState,
        conflict: &PreservedInput,
        input: Blob,
    ) -> Result<Manifest, BrokerError> {
        let event = conflict_event(conflict, &state.document_id);
        match conflict.source {
            DeviceSide::Boox => match conflict.payload_kind {
                DevicePayloadKind::DeviceView => self.boox_to_supernote(state, &event, input),
                DevicePayloadKind::BooxOperationManifest => {
                    validate_conflict_boox_manifest(state, &input, self.normalized_y_offset)
                }
            },
            DeviceSide::Supernote => supernote_export_manifest(state, &input),
        }
    }

    fn build_resolution_manifest(
        &self,
        previous_state: &CanonicalDocumentState,
        resulting_state: &CanonicalDocumentState,
        conflict: &PreservedInput,
        conflict_input: &[u8],
        boox_pdf: &[u8],
    ) -> Result<Manifest, BrokerError> {
        let work =
            tempfile::tempdir().map_err(|error| BrokerError::Conversion(error.to_string()))?;
        let mut baselines = write_baselines(work.path(), previous_state)?;
        if conflict.source == DeviceSide::Supernote {
            let export = parse_baseline_bytes(conflict_input, &conflict.object_path)
                .map_err(BrokerError::Conversion)?;
            let path = work
                .path()
                .join(format!("baseline-page-{}.json", export.page_index));
            std::fs::write(&path, conflict_input)
                .map_err(|error| BrokerError::Conversion(error.to_string()))?;
            if !baselines.contains(&path) {
                baselines.push(path);
            }
        }
        let pdf_path = work.path().join("resolved.pdf");
        std::fs::write(&pdf_path, boox_pdf)
            .map_err(|error| BrokerError::Conversion(error.to_string()))?;
        let mut manifest = build_manifest(&pdf_path, &baselines, self.normalized_y_offset)
            .map_err(BrokerError::Conversion)?;
        manifest.document.target_file_names = vec![resulting_state
            .supernote
            .source_file_name
            .clone()
            .unwrap_or_else(|| resulting_state.original_file_name.clone())];
        refresh_manifest_id(&mut manifest);
        Ok(manifest)
    }
}

pub fn conflict_resolution_path(document_id: &str, conflict_event_id: &str) -> String {
    format!(
        "Conflicts/{}/{}/resolution.json",
        document_id,
        event_path_segment(conflict_event_id)
    )
}

fn ensure_incremental_conflict_resolution_order(
    state: &CanonicalDocumentState,
    conflict: &PreservedInput,
) -> Result<(), BrokerError> {
    if conflict.source != DeviceSide::Supernote
        && conflict.payload_kind != DevicePayloadKind::BooxOperationManifest
    {
        return Ok(());
    }

    if let Some(predecessor) = state
        .conflicts
        .iter()
        .filter(|candidate| {
            candidate.source == conflict.source
                && candidate.event_id != conflict.event_id
                && candidate.source_revision < conflict.source_revision
        })
        .min_by_key(|candidate| (candidate.source_revision, candidate.event_id.as_str()))
    {
        return Err(BrokerError::InvalidEvent(format!(
            "incremental conflict {} at source revision {} cannot be resolved before earlier active conflict {} at source revision {}; resolve the earlier conflict first",
            conflict.event_id,
            conflict.source_revision,
            predecessor.event_id,
            predecessor.source_revision
        )));
    }

    Ok(())
}

fn resolution_destination_precondition(
    previous_state: &CanonicalDocumentState,
    conflict: &PreservedInput,
    destination_path: &str,
    destination: &Option<StoredObject>,
) -> Result<GenerationPrecondition, BrokerError> {
    if conflict.source == DeviceSide::Boox
        && conflict.payload_kind == DevicePayloadKind::DeviceView
        && conflict.object_path == destination_path
    {
        if let Some(destination) = destination {
            if destination.generation == conflict.source_generation
                && sha256_hex(&destination.bytes) == conflict.content_sha256
            {
                return Ok(GenerationPrecondition::Match(destination.generation));
            }
        }
    }

    destination_precondition(previous_state, destination_path, destination)
}

fn supersede_conflict<S: BrokerStorage>(
    storage: &mut S,
    context: ConflictContext,
    request: &ConflictResolutionRequest,
) -> Result<ConflictResolutionOutcome, BrokerError> {
    let mut state = context.state;
    let revisions = state.revisions();
    state
        .processed_event_ids
        .insert(request.resolution_id.clone());
    state.state_revision += 1;
    state
        .conflicts
        .retain(|conflict| conflict.event_id != request.conflict_event_id);

    let marker_path = conflict_resolution_path(&request.document_id, &request.conflict_event_id);
    let record = ConflictResolutionRecord {
        resolution_id: request.resolution_id.clone(),
        conflict_event_id: request.conflict_event_id.clone(),
        strategy: ConflictResolutionStrategy::KeepCurrent,
        superseded: true,
        source: context.conflict.source,
        previous_revisions: revisions,
        resulting_revisions: revisions,
        applied_stroke_ids: Vec::new(),
        preserved_current_stroke_ids: Vec::new(),
        marker_path: marker_path.clone(),
    };
    state
        .resolved_conflicts
        .insert(request.conflict_event_id.clone(), record.clone());
    let marker_bytes = add_newline(
        serde_json::to_vec_pretty(&record)
            .map_err(|error| BrokerError::CorruptState(error.to_string()))?,
    );
    storage
        .commit(vec![
            ConditionalWrite {
                path: marker_path,
                bytes: blob(marker_bytes),
                metadata: BTreeMap::from([
                    (GENERATED_BY_KEY.to_owned(), BROKER_PRODUCER.to_owned()),
                    (
                        GENERATED_DOCUMENT_KEY.to_owned(),
                        request.document_id.clone(),
                    ),
                    (
                        GENERATED_EVENT_KEY.to_owned(),
                        request.resolution_id.clone(),
                    ),
                    ("inkbridge-kind".to_owned(), RESOLUTION_KIND.to_owned()),
                    (
                        "inkbridge-conflict-event-id".to_owned(),
                        request.conflict_event_id.clone(),
                    ),
                ]),
                precondition: GenerationPrecondition::DoesNotExist,
            },
            state_write(
                &state_path(&request.document_id),
                &state,
                GenerationPrecondition::Match(context.state_object.generation),
            )?,
        ])
        .map_err(BrokerError::ConditionalWrite)?;

    Ok(ConflictResolutionOutcome::Superseded {
        document_id: request.document_id.clone(),
        conflict_event_id: request.conflict_event_id.clone(),
        resolution_id: request.resolution_id.clone(),
        source_revisions: revisions,
    })
}
fn load_context<S: BrokerStorage>(
    storage: &S,
    document_id: &str,
    conflict_event_id: &str,
) -> Result<ConflictContext, BrokerError> {
    let state_object = storage
        .read(&state_path(document_id))
        .map_err(BrokerError::Storage)?
        .ok_or_else(|| {
            BrokerError::MissingObject(format!("canonical state does not exist for {document_id}"))
        })?;
    let state = decode_state(&state_object.bytes)?;
    load_context_from_state(storage, state_object, state, conflict_event_id)
}

fn load_context_from_state<S: BrokerStorage>(
    storage: &S,
    state_object: StoredObject,
    mut state: CanonicalDocumentState,
    conflict_event_id: &str,
) -> Result<ConflictContext, BrokerError> {
    let conflict = state
        .conflicts
        .iter()
        .find(|conflict| conflict.event_id == conflict_event_id)
        .cloned()
        .ok_or_else(|| {
            BrokerError::InvalidEvent(format!(
                "conflict {conflict_event_id} is not active for {}",
                state.document_id
            ))
        })?;
    if conflict.source == DeviceSide::Boox
        && conflict.payload_kind == DevicePayloadKind::BooxOperationManifest
    {
        ensure_original_page_count(storage, &mut state)?;
    }
    let input = storage
        .read(&conflict.preserved_path)
        .map_err(BrokerError::Storage)?
        .ok_or_else(|| BrokerError::MissingObject(conflict.preserved_path.clone()))?;
    let actual_hash = sha256_hex(&input.bytes);
    if actual_hash != conflict.content_sha256 {
        return Err(BrokerError::CorruptState(format!(
            "preserved conflict input hash changed: expected {}, found {actual_hash}",
            conflict.content_sha256
        )));
    }
    Ok(ConflictContext {
        state_object,
        state,
        conflict,
        input,
    })
}

fn conflict_event(conflict: &PreservedInput, document_id: &str) -> StorageEvent {
    StorageEvent {
        schema_version: EVENT_SCHEMA_VERSION,
        event_id: conflict.event_id.clone(),
        document_id: document_id.to_owned(),
        source: conflict.source,
        object_path: conflict.preserved_path.clone(),
        source_generation: conflict.source_generation,
        source_revision: conflict.source_revision,
        based_on: conflict.based_on,
        content_sha256: conflict.content_sha256.clone(),
        payload_kind: conflict.payload_kind,
        broker_output: None,
    }
}

fn validate_conflict_boox_manifest(
    state: &CanonicalDocumentState,
    bytes: &[u8],
    normalized_y_offset: f64,
) -> Result<Manifest, BrokerError> {
    let mut manifest: Manifest = serde_json::from_slice(bytes).map_err(|error| {
        BrokerError::InvalidEvent(format!(
            "preserved BOOX operation manifest is invalid JSON: {error}"
        ))
    })?;
    if manifest.schema_version != 1
        || manifest.manifest_id.trim().is_empty()
        || manifest.source != "boox-neoreader-embedded-pdf"
        || manifest.document.source_file_name.trim().is_empty()
        || manifest.document.pdf_sha256.trim().is_empty()
    {
        return Err(BrokerError::InvalidEvent(
            "preserved BOOX operation manifest has an unsupported schema or identity".to_owned(),
        ));
    }
    if state.original_page_count != 0 && manifest.document.page_count != state.original_page_count {
        return Err(BrokerError::InvalidEvent(format!(
            "preserved BOOX operation manifest page count {} does not match original page count {}",
            manifest.document.page_count, state.original_page_count
        )));
    }

    let mut operation_kinds = BTreeMap::<String, (bool, bool)>::new();
    for operation in &mut manifest.operations {
        match operation {
            Operation::DeleteStroke {
                source_uuid,
                page_index,
                before,
            } => {
                validate_conflict_snapshot(
                    source_uuid,
                    *page_index,
                    before,
                    manifest.document.page_count,
                )?;
                normalize_supernote_pen_color(before);
                let kinds = operation_kinds.entry(source_uuid.clone()).or_default();
                if std::mem::replace(&mut kinds.0, true) {
                    return Err(BrokerError::InvalidEvent(format!(
                        "preserved BOOX operation manifest contains duplicate deletes for {source_uuid}"
                    )));
                }
            }
            Operation::UpsertStroke {
                source_uuid,
                page_index,
                before,
                after,
            } => {
                if let Some(before) = before {
                    validate_conflict_snapshot(
                        source_uuid,
                        *page_index,
                        before,
                        manifest.document.page_count,
                    )?;
                    normalize_supernote_pen_color(before);
                }
                validate_conflict_snapshot(
                    source_uuid,
                    *page_index,
                    after,
                    manifest.document.page_count,
                )?;
                normalize_supernote_pen_color(after);
                let kinds = operation_kinds.entry(source_uuid.clone()).or_default();
                if std::mem::replace(&mut kinds.1, true) {
                    return Err(BrokerError::InvalidEvent(format!(
                        "preserved BOOX operation manifest contains duplicate upserts for {source_uuid}"
                    )));
                }
            }
        }
    }
    manifest
        .coordinate_transform
        .pdf_to_supernote_normalized_y_offset = normalized_y_offset;
    manifest.document.target_file_names = vec![state
        .supernote
        .source_file_name
        .clone()
        .unwrap_or_else(|| state.original_file_name.clone())];
    manifest.summary = manifest_summary(&manifest.operations, 0);
    refresh_manifest_id(&mut manifest);
    Ok(manifest)
}

fn validate_conflict_snapshot(
    source_uuid: &str,
    page_index: u32,
    snapshot: &StrokeSnapshot,
    page_count: usize,
) -> Result<(), BrokerError> {
    let valid = !source_uuid.trim().is_empty()
        && source_uuid == snapshot.source_uuid
        && page_index == snapshot.page_index
        && usize::try_from(page_index)
            .ok()
            .is_some_and(|page| page < page_count)
        && snapshot.samples.len() >= 2
        && valid_supernote_native_style(&snapshot.native_style)
        && snapshot.samples.iter().all(|[x, y, pressure]| {
            x.is_finite()
                && y.is_finite()
                && pressure.is_finite()
                && (0.0..=1.0).contains(x)
                && (0.0..=1.0).contains(y)
                && (0.0..=4096.0).contains(pressure)
        })
        && snapshot.geometry_fingerprint
            == geometry_fingerprint(&snapshot.native_style, &snapshot.samples);
    if valid {
        Ok(())
    } else {
        Err(BrokerError::InvalidEvent(format!(
            "preserved BOOX operation manifest contains an invalid stroke operation for {source_uuid}"
        )))
    }
}

fn supernote_export_manifest(
    state: &CanonicalDocumentState,
    bytes: &[u8],
) -> Result<Manifest, BrokerError> {
    let export =
        parse_baseline_bytes(bytes, "conflict-supernote.json").map_err(BrokerError::Conversion)?;
    let mut active = state
        .strokes
        .values()
        .filter(|stroke| {
            stroke.tombstone.is_none() && stroke.snapshot.page_index == export.page_index
        })
        .map(|stroke| (stroke.stroke_id.clone(), stroke.snapshot.clone()))
        .collect::<BTreeMap<_, _>>();
    let mut operations = Vec::new();
    let mut unchanged = 0;
    for snapshot in export.strokes {
        let source_uuid = snapshot.source_uuid.clone();
        match active.remove(&source_uuid) {
            Some(before) if before == snapshot => unchanged += 1,
            Some(before) => operations.push(Operation::UpsertStroke {
                source_uuid,
                page_index: snapshot.page_index,
                before: Some(before),
                after: snapshot,
            }),
            None => operations.push(Operation::UpsertStroke {
                source_uuid,
                page_index: snapshot.page_index,
                before: None,
                after: snapshot,
            }),
        }
    }
    for (source_uuid, before) in active {
        operations.push(Operation::DeleteStroke {
            source_uuid,
            page_index: before.page_index,
            before,
        });
    }
    operations.sort_by(|left, right| operation_id(left).cmp(operation_id(right)));
    let mut manifest = Manifest {
        schema_version: 1,
        manifest_id: "pending".to_owned(),
        source: "supernote-native-conflict-export".to_owned(),
        document: DocumentIdentity {
            source_file_name: export
                .source_file_name
                .or_else(|| state.supernote.source_file_name.clone())
                .unwrap_or_else(|| state.original_file_name.clone()),
            target_file_names: vec![state.original_file_name.clone()],
            page_count: state.original_page_count,
            pdf_sha256: state.original_pdf_sha256.clone(),
        },
        coordinate_transform: CoordinateTransform {
            pdf_to_supernote_normalized_y_offset: 0.0,
        },
        summary: Summary {
            unchanged,
            ..manifest_summary(&operations, 0)
        },
        operations,
    };
    refresh_manifest_id(&mut manifest);
    Ok(manifest)
}

fn classify_operations(
    state: &CanonicalDocumentState,
    conflict: &PreservedInput,
    operations: &[Operation],
) -> Result<Vec<OperationGroup>, BrokerError> {
    let mut grouped = BTreeMap::<String, Vec<Operation>>::new();
    for operation in operations {
        grouped
            .entry(operation_id(operation).to_owned())
            .or_default()
            .push(operation.clone());
    }
    grouped
        .into_iter()
        .map(|(stroke_id, operations)| {
            let change = describe_change(&stroke_id, &operations)?;
            let canonical = state.strokes.get(&stroke_id);
            let unchanged_since_baseline = match change.kind {
                ConflictChangeKind::Add => canonical.is_none(),
                ConflictChangeKind::Update
                | ConflictChangeKind::Delete
                | ConflictChangeKind::Move => canonical.is_some_and(|stroke| {
                    stroke.tombstone.is_none()
                        && !changed_after(stroke.source_revisions, conflict.based_on)
                }),
            };
            // A later incremental conflict can legitimately reference an intermediate
            // snapshot that an earlier conflict resolution rejected. Treat that mismatch
            // as an overlap: merge-preserving-current will not apply it, while an explicit
            // accept-incoming decision can still choose the descendant version.
            let safe = unchanged_since_baseline
                && operations_match_current(&operations, canonical.map(|stroke| &stroke.snapshot));
            Ok(OperationGroup {
                change,
                operations,
                safe,
            })
        })
        .collect()
}

fn operations_match_current(operations: &[Operation], current: Option<&StrokeSnapshot>) -> bool {
    let Some(current) = current else {
        return operations
            .iter()
            .all(|operation| matches!(operation, Operation::UpsertStroke { before: None, .. }));
    };
    operations.iter().all(|operation| {
        match operation {
        Operation::DeleteStroke { before, .. } => before == current,
        Operation::UpsertStroke {
            before: Some(before),
            ..
        } => before == current,
        Operation::UpsertStroke { before: None, .. } => operations.iter().any(|candidate| {
            matches!(candidate, Operation::DeleteStroke { before, .. } if before == current)
        }),
    }
    })
}

fn describe_change(
    stroke_id: &str,
    operations: &[Operation],
) -> Result<ConflictStrokeChange, BrokerError> {
    let delete = operations.iter().find_map(|operation| match operation {
        Operation::DeleteStroke { before, .. } => Some(before),
        Operation::UpsertStroke { .. } => None,
    });
    let upsert = operations.iter().find_map(|operation| match operation {
        Operation::UpsertStroke { before, after, .. } => Some((before, after)),
        Operation::DeleteStroke { .. } => None,
    });
    let (kind, page_index) = match (delete, upsert) {
        (Some(before), Some((None, after))) if before.page_index != after.page_index => {
            (ConflictChangeKind::Move, after.page_index)
        }
        (None, Some((None, after))) => (ConflictChangeKind::Add, after.page_index),
        (None, Some((Some(before), after)))
            if before.native_style == after.native_style && before.samples != after.samples =>
        {
            (ConflictChangeKind::Move, after.page_index)
        }
        (None, Some((Some(_), after))) => (ConflictChangeKind::Update, after.page_index),
        (Some(before), None) => (ConflictChangeKind::Delete, before.page_index),
        _ => {
            return Err(BrokerError::Conversion(format!(
                "conflict contains an unsupported operation set for {stroke_id}"
            )))
        }
    };
    Ok(ConflictStrokeChange {
        stroke_id: stroke_id.to_owned(),
        kind,
        page_index,
    })
}

fn changed_after(revisions: RevisionPair, baseline: RevisionPair) -> bool {
    revisions.boox > baseline.boox || revisions.supernote > baseline.supernote
}

fn select_operations(
    groups: &[OperationGroup],
    strategy: ConflictResolutionStrategy,
) -> (Vec<Operation>, Vec<String>, Vec<String>) {
    let mut operations = Vec::new();
    let mut applied = Vec::new();
    let mut preserved = Vec::new();
    for group in groups {
        let apply = match strategy {
            ConflictResolutionStrategy::KeepCurrent => false,
            ConflictResolutionStrategy::AcceptIncoming => true,
            ConflictResolutionStrategy::MergePreservingCurrent => group.safe,
        };
        if apply {
            operations.extend(group.operations.clone());
            applied.push(group.change.stroke_id.clone());
        } else {
            preserved.push(group.change.stroke_id.clone());
        }
    }
    (operations, applied, preserved)
}

fn analysis_from_groups(context: &ConflictContext, groups: &[OperationGroup]) -> ConflictAnalysis {
    ConflictAnalysis {
        document_id: context.state.document_id.clone(),
        conflict_event_id: context.conflict.event_id.clone(),
        source: context.conflict.source,
        source_revision: context.conflict.source_revision,
        based_on: context.conflict.based_on,
        current_revisions: context.state.revisions(),
        state_revision: context.state.state_revision,
        safe_changes: groups
            .iter()
            .filter(|group| group.safe)
            .map(|group| group.change.clone())
            .collect(),
        overlapping_changes: groups
            .iter()
            .filter(|group| !group.safe)
            .map(|group| group.change.clone())
            .collect(),
    }
}

fn manifest_summary(operations: &[Operation], skipped: usize) -> Summary {
    Summary {
        upserted: operations
            .iter()
            .filter(|operation| matches!(operation, Operation::UpsertStroke { .. }))
            .count(),
        deleted: operations
            .iter()
            .filter(|operation| matches!(operation, Operation::DeleteStroke { .. }))
            .count(),
        unchanged: 0,
        skipped,
    }
}

fn operation_id(operation: &Operation) -> &str {
    match operation {
        Operation::UpsertStroke { source_uuid, .. }
        | Operation::DeleteStroke { source_uuid, .. } => source_uuid,
    }
}

fn validate_resolution_request(request: &ConflictResolutionRequest) -> Result<(), BrokerError> {
    if request.schema_version != RESOLUTION_SCHEMA_VERSION {
        return Err(BrokerError::InvalidEvent(format!(
            "unsupported conflict resolution schema version {}",
            request.schema_version
        )));
    }
    if request.resolution_id.trim().is_empty()
        || request.document_id.trim().is_empty()
        || request.conflict_event_id.trim().is_empty()
    {
        return Err(BrokerError::InvalidEvent(
            "resolution id, document id, and conflict event id are required".to_owned(),
        ));
    }
    Ok(())
}

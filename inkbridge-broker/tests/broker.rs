use inkbridge_broker::*;
use inkbridge_convert::{geometry_fingerprint, Manifest, NativeStyle, Operation, StrokeSnapshot};
use lopdf::{dictionary, Document, Object, Stream};
use serde_json::json;
use std::collections::BTreeMap;
use std::path::PathBuf;

fn original_pdf() -> Vec<u8> {
    original_pdf_with_pages(1)
}

fn original_pdf_with_anonymous_ink() -> Vec<u8> {
    let mut document = Document::with_version("1.7");
    let pages_id = document.new_object_id();
    let anonymous_ink_id = document.add_object(dictionary! {
        "Type" => "Annot",
        "Subtype" => "Ink",
        "InkList" => vec![Object::Array(vec![30.into(), 30.into(), 40.into(), 40.into()])],
    });
    let page_id = document.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
        "Resources" => dictionary! {},
        "Annots" => vec![Object::Reference(anonymous_ink_id)],
    });
    document.objects.insert(
        pages_id,
        dictionary! {
            "Type" => "Pages",
            "Kids" => vec![Object::Reference(page_id)],
            "Count" => 1,
        }
        .into(),
    );
    let catalog_id = document.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
    });
    document.trailer.set("Root", catalog_id);
    let mut bytes = Vec::new();
    document.save_to(&mut bytes).unwrap();
    bytes
}

fn original_pdf_with_pages(page_count: usize) -> Vec<u8> {
    let mut document = Document::with_version("1.7");
    let pages_id = document.new_object_id();
    let page_ids = (0..page_count)
        .map(|_| {
            document.add_object(dictionary! {
                "Type" => "Page",
                "Parent" => pages_id,
                "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
                "Resources" => dictionary! {},
            })
        })
        .collect::<Vec<_>>();
    document.objects.insert(
        pages_id,
        dictionary! {
            "Type" => "Pages",
            "Kids" => page_ids.into_iter().map(Into::into).collect::<Vec<_>>(),
            "Count" => page_count as i64,
        }
        .into(),
    );
    let catalog_id = document.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
    });
    document.trailer.set("Root", catalog_id);
    let mut bytes = Vec::new();
    document.save_to(&mut bytes).unwrap();
    bytes
}

fn original_pdf_with_padding(payload_mib: usize) -> Vec<u8> {
    let mut document = Document::load_mem(&original_pdf()).unwrap();
    let padding = document.add_object(Stream::new(
        dictionary! {},
        vec![0x5a; payload_mib * 1024 * 1024],
    ));
    let catalog_id = document
        .trailer
        .get(b"Root")
        .unwrap()
        .as_reference()
        .unwrap();
    document
        .get_dictionary_mut(catalog_id)
        .unwrap()
        .set("InkBridgeLargeDocumentFixture", padding);
    let mut bytes = Vec::new();
    document.save_to(&mut bytes).unwrap();
    bytes
}

fn stroke(id: &str, x: f64, y: f64) -> StrokeSnapshot {
    stroke_on_page(id, 0, x, y)
}

fn stroke_on_page(id: &str, page_index: u32, x: f64, y: f64) -> StrokeSnapshot {
    let native_style = NativeStyle::default();
    let samples = vec![[x, y, 900.0], [x + 0.08, y + 0.04, 1100.0]];
    StrokeSnapshot {
        source_uuid: id.to_owned(),
        origin: "supernote-native".to_owned(),
        page_index,
        geometry_fingerprint: geometry_fingerprint(&native_style, &samples),
        native_style,
        samples,
    }
}

fn supernote_export(strokes: &[StrokeSnapshot]) -> Vec<u8> {
    supernote_export_page(0, strokes)
}

fn supernote_export_page(page_index: u32, strokes: &[StrokeSnapshot]) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "sourceFileName": "document.pdf",
        "pageIndex": page_index,
        "strokes": strokes.iter().map(|stroke| json!({
            "sourceUuid": stroke.source_uuid,
            "sourceKey": stroke.source_uuid,
            "layerNum": stroke.native_style.layer_num,
            "thickness": stroke.native_style.thickness,
            "penColor": stroke.native_style.pen_color,
            "penType": stroke.native_style.pen_type,
            "samples": stroke.samples,
        })).collect::<Vec<_>>()
    }))
    .unwrap()
}

struct Harness {
    broker: Broker,
    storage: MemoryStorage,
    document_id: String,
    original: Vec<u8>,
}

impl Harness {
    fn new() -> Self {
        Self::with_original(original_pdf())
    }

    fn with_original(original: Vec<u8>) -> Self {
        let mut storage = MemoryStorage::default();
        let broker = Broker::default();
        let state = broker
            .register_document(&mut storage, "document.pdf", &original)
            .unwrap();
        Self {
            broker,
            storage,
            document_id: state.document_id,
            original,
        }
    }

    fn event(
        &mut self,
        event_id: &str,
        source: DeviceSide,
        revision: u64,
        based_on: RevisionPair,
        bytes: Vec<u8>,
    ) -> StorageEvent {
        let object_path = match source {
            DeviceSide::Boox => format!("BOOX_Folder/{}/upload-{revision}.pdf", self.document_id),
            DeviceSide::Supernote => {
                format!(
                    "Supernote_Folder/{}/export-{revision}.json",
                    self.document_id
                )
            }
        };
        let content_sha256 = sha256_hex(&bytes);
        let object = self
            .storage
            .put_unchecked(&object_path, bytes, BTreeMap::new());
        StorageEvent {
            schema_version: EVENT_SCHEMA_VERSION,
            event_id: event_id.to_owned(),
            document_id: self.document_id.clone(),
            source,
            object_path,
            source_generation: object.generation,
            source_revision: revision,
            based_on,
            content_sha256,
            payload_kind: DevicePayloadKind::DeviceView,
            broker_output: None,
        }
    }

    fn state(&self) -> CanonicalDocumentState {
        serde_json::from_slice(
            &self
                .storage
                .object(&state_path(&self.document_id))
                .unwrap()
                .bytes,
        )
        .unwrap()
    }
}

#[test]
fn stable_document_id_is_independent_of_file_name() {
    let original = original_pdf();
    let broker = Broker::default();
    let mut first = MemoryStorage::default();
    let mut second = MemoryStorage::default();
    let a = broker
        .register_document(&mut first, "one.pdf", &original)
        .unwrap();
    let b = broker
        .register_document(&mut second, "renamed.pdf", &original)
        .unwrap();
    assert_eq!(a.document_id, b.document_id);
}

#[test]
fn re_registering_legacy_document_persists_original_page_count() {
    let original = original_pdf_with_pages(3);
    let broker = Broker::default();
    let mut storage = MemoryStorage::default();
    let mut legacy = broker
        .register_document(&mut storage, "legacy.pdf", &original)
        .unwrap();
    legacy.original_page_count = 0;
    storage.put_unchecked(
        state_path(&legacy.document_id),
        serde_json::to_vec(&legacy).unwrap(),
        BTreeMap::new(),
    );

    let migrated = broker
        .register_document(&mut storage, "renamed-legacy.pdf", &original)
        .unwrap();
    let persisted: CanonicalDocumentState = serde_json::from_slice(
        &storage
            .object(&state_path(&legacy.document_id))
            .unwrap()
            .bytes,
    )
    .unwrap();

    assert_eq!(migrated.original_page_count, 3);
    assert_eq!(persisted.original_page_count, 3);
}

#[test]
fn boox_only_update_emits_supernote_manifest() {
    let mut harness = Harness::new();
    let pdf = write_boox_view(&harness.original, [stroke("boox-a", 0.1, 0.2)]).unwrap();
    let event = harness.event("boox-1", DeviceSide::Boox, 1, RevisionPair::default(), pdf);
    let outcome = harness
        .broker
        .process(&mut harness.storage, &event)
        .unwrap();
    assert!(matches!(outcome, ProcessOutcome::Applied { .. }));
    let output = harness
        .storage
        .object(&supernote_manifest_path(&harness.document_id, "boox-1"))
        .unwrap();
    let manifest: Manifest = serde_json::from_slice(&output.bytes).unwrap();
    assert_eq!(manifest.summary.upserted, 1);
    assert_eq!(manifest.summary.deleted, 0);
    assert!(matches!(
        &manifest.operations[0],
        Operation::UpsertStroke { source_uuid, .. } if source_uuid == "boox-a"
    ));
    assert!(harness
        .storage
        .paths()
        .all(|path| !path.contains("/accepted/")));
    assert_eq!(
        harness
            .storage
            .object(&original_path(&harness.document_id))
            .unwrap()
            .bytes
            .as_ref(),
        harness.original.as_slice()
    );
}

#[test]
fn compact_boox_manifest_produces_the_same_supernote_operations() {
    let mut full = Harness::new();
    let pdf = write_boox_view(&full.original, [stroke("boox-a", 0.1, 0.2)]).unwrap();
    let full_event = full.event(
        "boox-full",
        DeviceSide::Boox,
        1,
        RevisionPair::default(),
        pdf,
    );
    full.broker.process(&mut full.storage, &full_event).unwrap();
    let full_manifest_bytes = full
        .storage
        .object(&supernote_manifest_path(&full.document_id, "boox-full"))
        .unwrap()
        .bytes
        .to_vec();
    let expected: Manifest = serde_json::from_slice(&full_manifest_bytes).unwrap();

    let mut compact = Harness::new();
    let mut compact_event = compact.event(
        "boox-compact",
        DeviceSide::Boox,
        1,
        RevisionPair::default(),
        full_manifest_bytes,
    );
    compact_event.payload_kind = DevicePayloadKind::BooxOperationManifest;
    compact
        .broker
        .process(&mut compact.storage, &compact_event)
        .unwrap();
    let actual: Manifest = serde_json::from_slice(
        &compact
            .storage
            .object(&supernote_manifest_path(
                &compact.document_id,
                "boox-compact",
            ))
            .unwrap()
            .bytes,
    )
    .unwrap();
    assert_eq!(actual, expected);
}

#[test]
#[ignore = "large-memory validation; set INKBRIDGE_LARGE_DOCUMENT_MIB (default 300)"]
fn large_document_round_trip_does_not_create_an_accepted_pdf_copy() {
    let size_mib = std::env::var("INKBRIDGE_LARGE_DOCUMENT_MIB")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(300);
    let mut harness = Harness::with_original(original_pdf_with_padding(size_mib));
    let event = harness.event(
        "large-supernote-update",
        DeviceSide::Supernote,
        1,
        RevisionPair::default(),
        supernote_export(&[stroke("large-doc-stroke", 0.2, 0.3)]),
    );
    harness
        .broker
        .process(&mut harness.storage, &event)
        .unwrap();
    let state = harness.state();
    let generated = harness.storage.object(&boox_view_path(&state)).unwrap();
    assert!(generated.bytes.len() >= size_mib * 1024 * 1024);
    assert!(harness
        .storage
        .paths()
        .all(|path| !path.contains("/accepted/")));
}

#[test]
fn empty_initial_boox_update_still_targets_the_registered_document() {
    let mut harness = Harness::new();
    let event = harness.event(
        "boox-empty-1",
        DeviceSide::Boox,
        1,
        RevisionPair::default(),
        harness.original.clone(),
    );
    harness
        .broker
        .process(&mut harness.storage, &event)
        .unwrap();

    let output = harness
        .storage
        .object(&supernote_manifest_path(
            &harness.document_id,
            "boox-empty-1",
        ))
        .unwrap();
    let manifest: Manifest = serde_json::from_slice(&output.bytes).unwrap();
    assert_eq!(manifest.document.target_file_names, ["document.pdf"]);
    assert!(manifest.operations.is_empty());
}

#[test]
fn anonymous_original_ink_does_not_hide_a_canonical_boox_deletion() {
    let mut harness = Harness::with_original(original_pdf_with_anonymous_ink());
    let supernote = harness.event(
        "sn-with-tracked-stroke",
        DeviceSide::Supernote,
        1,
        RevisionPair::default(),
        supernote_export(&[stroke("tracked-stroke", 0.2, 0.3)]),
    );
    harness
        .broker
        .process(&mut harness.storage, &supernote)
        .unwrap();

    // This is the generated BOOX view after the tracked stroke is erased:
    // the anonymous original ink remains, but the canonical stroke is absent.
    let edited = write_boox_view(&harness.original, []).unwrap();
    let boox = harness.event(
        "boox-delete-beside-anonymous",
        DeviceSide::Boox,
        1,
        RevisionPair {
            boox: 0,
            supernote: 1,
        },
        edited,
    );
    harness.broker.process(&mut harness.storage, &boox).unwrap();

    let manifest: Manifest = serde_json::from_slice(
        &harness
            .storage
            .object(&supernote_manifest_path(
                &harness.document_id,
                "boox-delete-beside-anonymous",
            ))
            .unwrap()
            .bytes,
    )
    .unwrap();
    assert_eq!(manifest.summary.deleted, 1);
    assert!(matches!(
        &manifest.operations[0],
        Operation::DeleteStroke { source_uuid, .. } if source_uuid == "tracked-stroke"
    ));
    assert!(harness.state().strokes["tracked-stroke"]
        .tombstone
        .is_some());
}

#[test]
fn supernote_only_update_emits_editable_boox_pdf() {
    let mut harness = Harness::new();
    let event = harness.event(
        "sn-1",
        DeviceSide::Supernote,
        1,
        RevisionPair::default(),
        supernote_export(&[stroke("sn-a", 0.2, 0.3)]),
    );
    harness
        .broker
        .process(&mut harness.storage, &event)
        .unwrap();
    let state = harness.state();
    let path = boox_view_path(&state);
    assert!(path.ends_with("/document.pdf"));
    let view = harness.storage.object(&path).unwrap();
    let document = Document::load_mem(&view.bytes).unwrap();
    let page = *document.get_pages().get(&1).unwrap();
    let annotations = document.get_page_annotations(page).unwrap();
    assert_eq!(annotations.len(), 1);
    assert_eq!(
        annotations[0].get(b"Subtype").unwrap().as_name().unwrap(),
        b"Ink"
    );
    assert_eq!(
        annotations[0].get(b"NM").unwrap().as_str().unwrap(),
        b"sn-a"
    );
    assert!(annotations[0].get(b"AP").is_ok());
}

#[test]
fn event_ids_with_the_same_readable_form_get_distinct_manifest_paths() {
    let document_id = "inkbridge-doc-v1-test";
    let slash = supernote_manifest_path(document_id, "a/b");
    let question = supernote_manifest_path(document_id, "a?b");
    assert_ne!(slash, question);
    assert!(slash.ends_with(".operations.json"));
    assert!(question.ends_with(".operations.json"));
}

#[test]
fn repeated_event_is_idempotent() {
    let mut harness = Harness::new();
    let event = harness.event(
        "sn-duplicate",
        DeviceSide::Supernote,
        1,
        RevisionPair::default(),
        supernote_export(&[stroke("only-once", 0.2, 0.3)]),
    );
    let first = harness
        .broker
        .process(&mut harness.storage, &event)
        .unwrap();
    let generation = match first {
        ProcessOutcome::Applied {
            destination_generation,
            ..
        } => destination_generation,
        other => panic!("unexpected outcome: {other:?}"),
    };
    assert!(matches!(
        harness
            .broker
            .process(&mut harness.storage, &event)
            .unwrap(),
        ProcessOutcome::Duplicate { .. }
    ));
    let state = harness.state();
    assert_eq!(state.strokes.len(), 1);
    assert_eq!(
        harness
            .storage
            .object(&boox_view_path(&state))
            .unwrap()
            .generation,
        generation
    );
}

#[test]
fn out_of_order_event_reads_its_exact_immutable_generation() {
    let mut harness = Harness::new();
    let stale = harness.event(
        "sn-stale-generation",
        DeviceSide::Supernote,
        1,
        RevisionPair::default(),
        supernote_export(&[stroke("old", 0.2, 0.3)]),
    );
    harness.storage.put_unchecked(
        &stale.object_path,
        b"newer generation bytes".to_vec(),
        BTreeMap::new(),
    );

    assert!(matches!(
        harness
            .broker
            .process(&mut harness.storage, &stale)
            .unwrap(),
        ProcessOutcome::Applied { .. }
    ));
    let state = harness.state();
    assert_eq!(state.supernote.revision, 1);
    assert!(state.strokes.contains_key("old"));
    assert!(state.processed_event_ids.contains("sn-stale-generation"));
}

#[test]
fn broker_generated_output_event_is_ignored() {
    let mut harness = Harness::new();
    let event = harness.event(
        "sn-source",
        DeviceSide::Supernote,
        1,
        RevisionPair::default(),
        supernote_export(&[stroke("sn-a", 0.2, 0.3)]),
    );
    let applied = harness
        .broker
        .process(&mut harness.storage, &event)
        .unwrap();
    let (path, revisions) = match applied {
        ProcessOutcome::Applied {
            destination_path,
            source_revisions,
            ..
        } => (destination_path, source_revisions),
        other => panic!("unexpected outcome: {other:?}"),
    };
    let output = harness.storage.object(&path).unwrap().clone();
    let delivery = StorageEvent {
        schema_version: EVENT_SCHEMA_VERSION,
        event_id: "cloud-finalize-output".to_owned(),
        document_id: harness.document_id.clone(),
        source: DeviceSide::Boox,
        object_path: path,
        source_generation: output.generation,
        source_revision: 1,
        based_on: revisions,
        content_sha256: sha256_hex(&output.bytes),
        payload_kind: DevicePayloadKind::DeviceView,
        broker_output: Some(BrokerOutputMarker {
            producer: BROKER_PRODUCER.to_owned(),
            event_id: "sn-source".to_owned(),
            document_id: harness.document_id.clone(),
            source_revisions: revisions,
        }),
    };
    assert!(matches!(
        harness
            .broker
            .process(&mut harness.storage, &delivery)
            .unwrap(),
        ProcessOutcome::IgnoredBrokerOutput { .. }
    ));
}

#[test]
fn stale_destination_generation_does_not_overwrite_newer_content() {
    let mut harness = Harness::new();
    let first = harness.event(
        "sn-1",
        DeviceSide::Supernote,
        1,
        RevisionPair::default(),
        supernote_export(&[stroke("sn-a", 0.2, 0.3)]),
    );
    harness
        .broker
        .process(&mut harness.storage, &first)
        .unwrap();
    let path = boox_view_path(&harness.state());
    let external = b"newer device content".to_vec();
    harness
        .storage
        .put_unchecked(&path, external.clone(), BTreeMap::new());
    let second = harness.event(
        "sn-2",
        DeviceSide::Supernote,
        2,
        RevisionPair {
            boox: 0,
            supernote: 1,
        },
        supernote_export(&[stroke("sn-a", 0.3, 0.4)]),
    );
    let error = harness
        .broker
        .process(&mut harness.storage, &second)
        .unwrap_err();
    assert!(matches!(error, BrokerError::StaleDestination { .. }));
    assert_eq!(
        harness.storage.object(&path).unwrap().bytes.as_ref(),
        external
    );
    assert_eq!(harness.state().supernote.revision, 1);
}

#[test]
fn accepted_in_place_boox_edit_becomes_the_next_destination_baseline() {
    let mut harness = Harness::new();
    let original = stroke("round-trip", 0.2, 0.3);
    let supernote_first = harness.event(
        "sn-1",
        DeviceSide::Supernote,
        1,
        RevisionPair::default(),
        supernote_export(&[original]),
    );
    harness
        .broker
        .process(&mut harness.storage, &supernote_first)
        .unwrap();

    let boox_path = boox_view_path(&harness.state());
    let generated = harness.storage.object(&boox_path).unwrap().clone();
    let moved = stroke("round-trip", 0.35, 0.4);
    let edited_pdf = write_boox_view(&harness.original, [moved.clone()]).unwrap();
    let edited_hash = sha256_hex(&edited_pdf);
    let edited_object = harness
        .storage
        .put_unchecked(&boox_path, edited_pdf, generated.metadata);
    let boox_event = StorageEvent {
        schema_version: EVENT_SCHEMA_VERSION,
        event_id: "boox-in-place".to_owned(),
        document_id: harness.document_id.clone(),
        source: DeviceSide::Boox,
        object_path: boox_path.clone(),
        source_generation: edited_object.generation,
        source_revision: 1,
        based_on: RevisionPair {
            boox: 0,
            supernote: 1,
        },
        content_sha256: edited_hash.clone(),
        payload_kind: DevicePayloadKind::DeviceView,
        broker_output: Some(BrokerOutputMarker {
            producer: BROKER_PRODUCER.to_owned(),
            event_id: "sn-1".to_owned(),
            document_id: harness.document_id.clone(),
            source_revisions: RevisionPair {
                boox: 0,
                supernote: 1,
            },
        }),
    };
    harness
        .broker
        .process(&mut harness.storage, &boox_event)
        .unwrap();
    assert_eq!(
        harness.state().generated_views[&boox_path].content_sha256,
        edited_hash
    );

    let supernote_second = harness.event(
        "sn-2",
        DeviceSide::Supernote,
        2,
        RevisionPair {
            boox: 1,
            supernote: 1,
        },
        supernote_export(&[moved]),
    );
    assert!(matches!(
        harness
            .broker
            .process(&mut harness.storage, &supernote_second)
            .unwrap(),
        ProcessOutcome::Applied {
            destination_path,
            ..
        } if destination_path == boox_path
    ));
}

#[test]
fn simultaneous_edits_preserve_incoming_input_as_conflict() {
    let mut harness = Harness::new();
    let initial = harness.event(
        "sn-1",
        DeviceSide::Supernote,
        1,
        RevisionPair::default(),
        supernote_export(&[stroke("shared", 0.2, 0.3)]),
    );
    harness
        .broker
        .process(&mut harness.storage, &initial)
        .unwrap();
    let common = RevisionPair {
        boox: 0,
        supernote: 1,
    };
    let boox_pdf = write_boox_view(&harness.original, [stroke("shared", 0.3, 0.3)]).unwrap();
    let boox = harness.event("boox-1", DeviceSide::Boox, 1, common, boox_pdf);
    harness.broker.process(&mut harness.storage, &boox).unwrap();
    let supernote = harness.event(
        "sn-2-concurrent",
        DeviceSide::Supernote,
        2,
        common,
        supernote_export(&[stroke("shared", 0.2, 0.4)]),
    );
    let outcome = harness
        .broker
        .process(&mut harness.storage, &supernote)
        .unwrap();
    let preserved_path = match outcome {
        ProcessOutcome::Conflict { preserved_path, .. } => preserved_path,
        other => panic!("unexpected outcome: {other:?}"),
    };
    assert!(harness.storage.object(&preserved_path).is_some());
    assert_eq!(harness.state().supernote.revision, 1);
    assert_eq!(harness.state().conflicts.len(), 1);
    let conflict = &harness.state().conflicts[0];
    assert_eq!(conflict.competing_preserved_paths.len(), 1);
    assert!(harness
        .storage
        .object(&conflict.competing_preserved_paths[0])
        .is_some());
}

#[test]
fn conflict_preserves_the_immutable_accepted_device_revision() {
    let mut harness = Harness::new();
    let initial = harness.event(
        "sn-1",
        DeviceSide::Supernote,
        1,
        RevisionPair::default(),
        supernote_export(&[stroke("shared", 0.2, 0.3)]),
    );
    harness
        .broker
        .process(&mut harness.storage, &initial)
        .unwrap();

    let common = RevisionPair {
        boox: 0,
        supernote: 1,
    };
    let boox_path = boox_view_path(&harness.state());
    let accepted_boox = write_boox_view(&harness.original, [stroke("shared", 0.35, 0.3)]).unwrap();
    let accepted_object =
        harness
            .storage
            .put_unchecked(&boox_path, accepted_boox.clone(), BTreeMap::new());
    let boox_event = StorageEvent {
        schema_version: EVENT_SCHEMA_VERSION,
        event_id: "boox-accepted".to_owned(),
        document_id: harness.document_id.clone(),
        source: DeviceSide::Boox,
        object_path: boox_path.clone(),
        source_generation: accepted_object.generation,
        source_revision: 1,
        based_on: common,
        content_sha256: sha256_hex(&accepted_boox),
        payload_kind: DevicePayloadKind::DeviceView,
        broker_output: None,
    };
    harness
        .broker
        .process(&mut harness.storage, &boox_event)
        .unwrap();
    let accepted_state = harness.state();
    assert!(accepted_state.boox.accepted_object_path.is_empty());
    assert_eq!(
        harness
            .storage
            .read_generation(&boox_path, accepted_object.generation)
            .unwrap()
            .unwrap()
            .bytes
            .as_ref(),
        accepted_boox
    );

    harness
        .storage
        .put_unchecked(&boox_path, b"later mutable bytes".to_vec(), BTreeMap::new());
    let concurrent_supernote = harness.event(
        "sn-concurrent",
        DeviceSide::Supernote,
        2,
        common,
        supernote_export(&[stroke("shared", 0.2, 0.45)]),
    );
    assert!(matches!(
        harness
            .broker
            .process(&mut harness.storage, &concurrent_supernote)
            .unwrap(),
        ProcessOutcome::Conflict { .. }
    ));
    let state = harness.state();
    let conflict = state.conflicts.last().unwrap();
    assert_eq!(conflict.competing_preserved_paths.len(), 1);
    assert_eq!(
        harness
            .storage
            .object(&conflict.competing_preserved_paths[0])
            .unwrap()
            .bytes
            .as_ref(),
        accepted_boox
    );
}

#[test]
fn boox_move_and_deletion_update_canonical_strokes_and_tombstones() {
    let mut harness = Harness::new();
    let original_a = stroke("move-me", 0.1, 0.2);
    let original_b = stroke("delete-me", 0.4, 0.5);
    let initial = harness.event(
        "sn-1",
        DeviceSide::Supernote,
        1,
        RevisionPair::default(),
        supernote_export(&[original_a.clone(), original_b]),
    );
    harness
        .broker
        .process(&mut harness.storage, &initial)
        .unwrap();
    let moved = stroke("move-me", 0.25, 0.35);
    let returned = write_boox_view(&harness.original, [moved.clone()]).unwrap();
    let boox = harness.event(
        "boox-1",
        DeviceSide::Boox,
        1,
        RevisionPair {
            boox: 0,
            supernote: 1,
        },
        returned,
    );
    harness.broker.process(&mut harness.storage, &boox).unwrap();
    let state = harness.state();
    assert_eq!(
        state.strokes["move-me"].snapshot.geometry_fingerprint,
        moved.geometry_fingerprint
    );
    assert!(state.strokes["move-me"].tombstone.is_none());
    assert!(state.strokes["delete-me"].tombstone.is_some());
    let manifest: Manifest = serde_json::from_slice(
        &harness
            .storage
            .object(&supernote_manifest_path(&harness.document_id, "boox-1"))
            .unwrap()
            .bytes,
    )
    .unwrap();
    assert_eq!(manifest.summary.upserted, 1);
    assert_eq!(manifest.summary.deleted, 1);
}

#[test]
fn boox_cross_page_move_to_lower_page_remains_active() {
    let mut harness = Harness::with_original(original_pdf_with_pages(2));
    let original = stroke_on_page("cross-page", 1, 0.1, 0.2);
    let initial = harness.event(
        "sn-page-2",
        DeviceSide::Supernote,
        1,
        RevisionPair::default(),
        supernote_export_page(1, &[original]),
    );
    harness
        .broker
        .process(&mut harness.storage, &initial)
        .unwrap();

    let moved = stroke_on_page("cross-page", 0, 0.25, 0.35);
    let returned = write_boox_view(&harness.original, [moved.clone()]).unwrap();
    let boox = harness.event(
        "boox-cross-page",
        DeviceSide::Boox,
        1,
        RevisionPair {
            boox: 0,
            supernote: 1,
        },
        returned,
    );
    harness.broker.process(&mut harness.storage, &boox).unwrap();

    let canonical = &harness.state().strokes["cross-page"];
    assert_eq!(canonical.snapshot.page_index, 0);
    assert_eq!(
        canonical.snapshot.geometry_fingerprint,
        moved.geometry_fingerprint
    );
    assert!(canonical.tombstone.is_none());

    let manifest: Manifest = serde_json::from_slice(
        &harness
            .storage
            .object(&supernote_manifest_path(
                &harness.document_id,
                "boox-cross-page",
            ))
            .unwrap()
            .bytes,
    )
    .unwrap();
    assert_eq!(manifest.summary.upserted, 1);
    assert_eq!(manifest.summary.deleted, 1);
}

#[test]
fn empty_supernote_page_export_tombstones_every_stroke_on_that_page() {
    let mut harness = Harness::new();
    let initial = harness.event(
        "sn-1",
        DeviceSide::Supernote,
        1,
        RevisionPair::default(),
        supernote_export(&[stroke("erase-a", 0.1, 0.2), stroke("erase-b", 0.3, 0.4)]),
    );
    harness
        .broker
        .process(&mut harness.storage, &initial)
        .unwrap();
    let cleared = harness.event(
        "sn-2-clear",
        DeviceSide::Supernote,
        2,
        RevisionPair {
            boox: 0,
            supernote: 1,
        },
        supernote_export(&[]),
    );
    harness
        .broker
        .process(&mut harness.storage, &cleared)
        .unwrap();
    let state = harness.state();
    assert!(state
        .strokes
        .values()
        .all(|stroke| stroke.tombstone.is_some()));
    let view = harness.storage.object(&boox_view_path(&state)).unwrap();
    let document = Document::load_mem(&view.bytes).unwrap();
    let page = *document.get_pages().get(&1).unwrap();
    assert!(document.get_page_annotations(page).unwrap().is_empty());
}

#[test]
fn malformed_neoreader_pdf_is_recovered_with_qpdf() {
    if std::process::Command::new("qpdf")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("skipping qpdf recovery test because qpdf is not installed");
        return;
    }
    let mut harness = Harness::new();
    let mut pdf = write_boox_view(&harness.original, [stroke("recover-me", 0.1, 0.2)]).unwrap();
    let marker = b"startxref\n";
    let start = pdf
        .windows(marker.len())
        .rposition(|window| window == marker)
        .unwrap()
        + marker.len();
    let end = pdf[start..]
        .iter()
        .position(|byte| *byte == b'\n' || *byte == b'\r')
        .map(|offset| start + offset)
        .unwrap();
    for byte in &mut pdf[start..end] {
        *byte = b'9';
    }
    assert!(Document::load_mem(&pdf).is_err());
    let event = harness.event(
        "boox-malformed",
        DeviceSide::Boox,
        1,
        RevisionPair::default(),
        pdf,
    );
    harness
        .broker
        .process(&mut harness.storage, &event)
        .unwrap();
    let manifest: Manifest = serde_json::from_slice(
        &harness
            .storage
            .object(&supernote_manifest_path(
                &harness.document_id,
                "boox-malformed",
            ))
            .unwrap()
            .bytes,
    )
    .unwrap();
    assert_eq!(manifest.summary.upserted, 1);
}

#[test]
#[ignore = "requires the private real-device fixture directory"]
fn real_device_manifest_is_byte_identical_to_proven_converter_output() {
    let root = std::env::var_os("INKBRIDGE_REAL_FIXTURE_ROOT")
        .map(PathBuf::from)
        .expect("set INKBRIDGE_REAL_FIXTURE_ROOT to artifacts/dual-device-test");
    let original = std::fs::read(root.join("Shapiro0146-0153-Supernote-original.pdf")).unwrap();
    let baseline = std::fs::read(root.join("supernote/InkBridge_Baseline.json")).unwrap();
    let boox = std::fs::read(root.join("boox/Shapiro0146-0153-NeoReader-Embedded.pdf")).unwrap();
    let expected = std::fs::read(root.join("return/Shapiro-review-fix-6-test.json")).unwrap();
    let mut storage = MemoryStorage::default();
    let broker = Broker::default();
    let state = broker
        .register_document(
            &mut storage,
            "Shapiro0146-0153-Supernote-original.pdf",
            &original,
        )
        .unwrap();
    let baseline_path = format!("Supernote_Folder/{}/baseline.json", state.document_id);
    let baseline_object = storage.put_unchecked(&baseline_path, baseline.clone(), BTreeMap::new());
    let sn_event = StorageEvent {
        schema_version: EVENT_SCHEMA_VERSION,
        event_id: "real-sn-baseline".to_owned(),
        document_id: state.document_id.clone(),
        source: DeviceSide::Supernote,
        object_path: baseline_path,
        source_generation: baseline_object.generation,
        source_revision: 1,
        based_on: RevisionPair::default(),
        content_sha256: sha256_hex(&baseline),
        payload_kind: DevicePayloadKind::DeviceView,
        broker_output: None,
    };
    broker.process(&mut storage, &sn_event).unwrap();
    if let Some(output) = std::env::var_os("INKBRIDGE_REAL_PDF_OUTPUT") {
        let state_object: CanonicalDocumentState = serde_json::from_slice(
            &storage
                .object(&state_path(&state.document_id))
                .unwrap()
                .bytes,
        )
        .unwrap();
        std::fs::write(
            PathBuf::from(output),
            &storage
                .object(&boox_view_path(&state_object))
                .unwrap()
                .bytes,
        )
        .unwrap();
    }
    let boox_path = format!(
        "BOOX_Folder/{}/Shapiro0146-0153-NeoReader-Embedded.pdf",
        state.document_id
    );
    let boox_object = storage.put_unchecked(&boox_path, boox.clone(), BTreeMap::new());
    let boox_event = StorageEvent {
        schema_version: EVENT_SCHEMA_VERSION,
        event_id: "real-boox-return".to_owned(),
        document_id: state.document_id.clone(),
        source: DeviceSide::Boox,
        object_path: boox_path,
        source_generation: boox_object.generation,
        source_revision: 1,
        based_on: RevisionPair {
            boox: 0,
            supernote: 1,
        },
        content_sha256: sha256_hex(&boox),
        payload_kind: DevicePayloadKind::DeviceView,
        broker_output: None,
    };
    broker.process(&mut storage, &boox_event).unwrap();
    let actual = &storage
        .object(&supernote_manifest_path(
            &state.document_id,
            "real-boox-return",
        ))
        .unwrap()
        .bytes;
    assert_eq!(actual.as_ref(), expected.as_slice());
}

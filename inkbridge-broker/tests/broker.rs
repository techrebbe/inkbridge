use inkbridge_broker::*;
use inkbridge_convert::{
    geometry_fingerprint, CoordinateTransform, DocumentIdentity, Manifest, NativeStyle, Operation,
    StrokeSnapshot, Summary,
};
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

fn supernote_export_pages(pages: &[(u32, &[StrokeSnapshot])]) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "schemaVersion": 1,
        "sourceFileName": "document.pdf",
        "pages": pages.iter().map(|(page_index, strokes)| json!({
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
fn registration_rejects_a_zero_page_pdf() {
    let broker = Broker::default();
    let mut storage = MemoryStorage::default();
    let error = broker
        .register_document(&mut storage, "empty.pdf", &original_pdf_with_pages(0))
        .expect_err("a device view cannot address an empty page tree");

    assert!(
        matches!(error, BrokerError::InvalidEvent(message) if message.contains("at least one page"))
    );
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
fn compact_processing_backfills_legacy_document_page_count() {
    let mut harness = Harness::new();
    let mut legacy = harness.state();
    legacy.original_page_count = 0;
    harness.storage.put_unchecked(
        state_path(&harness.document_id),
        serde_json::to_vec(&legacy).unwrap(),
        BTreeMap::new(),
    );
    let snapshot = stroke("legacy-compact", 0.2, 0.3);
    let mut event = harness.event(
        "boox-legacy-compact",
        DeviceSide::Boox,
        1,
        RevisionPair::default(),
        compact_manifest(
            vec![Operation::UpsertStroke {
                source_uuid: snapshot.source_uuid.clone(),
                page_index: snapshot.page_index,
                before: None,
                after: snapshot,
            }],
            1,
        ),
    );
    event.payload_kind = DevicePayloadKind::BooxOperationManifest;

    harness
        .broker
        .process(&mut harness.storage, &event)
        .unwrap();
    assert_eq!(harness.state().original_page_count, 1);
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
    let mut compact_input = expected.clone();
    compact_input.document.target_file_names.clear();

    let mut compact = Harness::new();
    let mut compact_event = compact.event(
        "boox-compact",
        DeviceSide::Boox,
        1,
        RevisionPair::default(),
        serde_json::to_vec(&compact_input).unwrap(),
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

fn compact_manifest(operations: Vec<Operation>, page_count: usize) -> Vec<u8> {
    let upserted = operations
        .iter()
        .filter(|operation| matches!(operation, Operation::UpsertStroke { .. }))
        .count();
    let deleted = operations
        .iter()
        .filter(|operation| matches!(operation, Operation::DeleteStroke { .. }))
        .count();
    serde_json::to_vec(&Manifest {
        schema_version: 1,
        manifest_id: "compact-test".to_owned(),
        source: "boox-neoreader-embedded-pdf".to_owned(),
        document: DocumentIdentity {
            source_file_name: "boox-view.pdf".to_owned(),
            target_file_names: Vec::new(),
            page_count,
            pdf_sha256: "compact-input".to_owned(),
        },
        coordinate_transform: CoordinateTransform {
            pdf_to_supernote_normalized_y_offset: -0.0008,
        },
        operations,
        summary: Summary {
            upserted,
            deleted,
            ..Summary::default()
        },
    })
    .unwrap()
}

#[test]
fn compact_manifest_requires_identity_and_verified_fingerprints() {
    let mut harness = Harness::new();
    let snapshot = stroke("new-stroke", 0.2, 0.3);
    let valid = compact_manifest(
        vec![Operation::UpsertStroke {
            source_uuid: snapshot.source_uuid.clone(),
            page_index: snapshot.page_index,
            before: None,
            after: snapshot,
        }],
        1,
    );
    let mut missing_id: Manifest = serde_json::from_slice(&valid).unwrap();
    missing_id.manifest_id.clear();
    let mut missing_id_event = harness.event(
        "boox-compact-missing-id",
        DeviceSide::Boox,
        1,
        RevisionPair::default(),
        serde_json::to_vec(&missing_id).unwrap(),
    );
    missing_id_event.payload_kind = DevicePayloadKind::BooxOperationManifest;
    assert!(matches!(
        harness
            .broker
            .process(&mut harness.storage, &missing_id_event),
        Err(BrokerError::InvalidEvent(_))
    ));

    let mut stale_fingerprint: Manifest = serde_json::from_slice(&valid).unwrap();
    let Operation::UpsertStroke { after, .. } = &mut stale_fingerprint.operations[0] else {
        panic!("fixture must contain an upsert")
    };
    after.geometry_fingerprint = "fnv1a32:00000000".to_owned();
    let mut stale_fingerprint_event = harness.event(
        "boox-compact-stale-fingerprint",
        DeviceSide::Boox,
        1,
        RevisionPair::default(),
        serde_json::to_vec(&stale_fingerprint).unwrap(),
    );
    stale_fingerprint_event.payload_kind = DevicePayloadKind::BooxOperationManifest;
    assert!(matches!(
        harness
            .broker
            .process(&mut harness.storage, &stale_fingerprint_event),
        Err(BrokerError::InvalidEvent(_))
    ));
    assert!(!harness.state().strokes.contains_key("new-stroke"));
}

#[test]
fn compact_manifest_rejects_samples_the_supernote_would_clamp() {
    let mut harness = Harness::new();
    let mut out_of_range = stroke("out-of-range", 0.2, 0.3);
    out_of_range.samples[0] = [1.01, -0.01, 4097.0];
    out_of_range.geometry_fingerprint =
        geometry_fingerprint(&out_of_range.native_style, &out_of_range.samples);
    let mut event = harness.event(
        "boox-compact-out-of-range",
        DeviceSide::Boox,
        1,
        RevisionPair::default(),
        compact_manifest(
            vec![Operation::UpsertStroke {
                source_uuid: out_of_range.source_uuid.clone(),
                page_index: out_of_range.page_index,
                before: None,
                after: out_of_range,
            }],
            1,
        ),
    );
    event.payload_kind = DevicePayloadKind::BooxOperationManifest;

    assert!(matches!(
        harness.broker.process(&mut harness.storage, &event),
        Err(BrokerError::InvalidEvent(_))
    ));
    assert!(!harness.state().strokes.contains_key("out-of-range"));
}

#[test]
fn compact_manifest_rejects_native_style_values_the_plugin_cannot_preserve() {
    let beyond_android_integer = i64::from(i32::MAX) + 1;
    let invalid_styles = [
        NativeStyle {
            layer_num: beyond_android_integer,
            ..NativeStyle::default()
        },
        NativeStyle {
            thickness: 0,
            ..NativeStyle::default()
        },
        NativeStyle {
            thickness: beyond_android_integer,
            ..NativeStyle::default()
        },
        NativeStyle {
            pen_type: beyond_android_integer,
            ..NativeStyle::default()
        },
    ];

    for (index, native_style) in invalid_styles.into_iter().enumerate() {
        let mut harness = Harness::new();
        let mut invalid = stroke(&format!("invalid-style-{index}"), 0.2, 0.3);
        invalid.native_style = native_style;
        invalid.geometry_fingerprint =
            geometry_fingerprint(&invalid.native_style, &invalid.samples);
        let mut event = harness.event(
            &format!("boox-compact-invalid-style-{index}"),
            DeviceSide::Boox,
            1,
            RevisionPair::default(),
            compact_manifest(
                vec![Operation::UpsertStroke {
                    source_uuid: invalid.source_uuid.clone(),
                    page_index: invalid.page_index,
                    before: None,
                    after: invalid,
                }],
                1,
            ),
        );
        event.payload_kind = DevicePayloadKind::BooxOperationManifest;

        assert!(matches!(
            harness.broker.process(&mut harness.storage, &event),
            Err(BrokerError::InvalidEvent(_))
        ));
    }
}

#[test]
fn compact_manifest_normalizes_pen_colors_before_canonical_storage() {
    let mut harness = Harness::new();
    let mut original = stroke("normalized-color", 0.2, 0.3);
    original.native_style.pen_color = 130;
    original.geometry_fingerprint = geometry_fingerprint(&original.native_style, &original.samples);
    let mut first = harness.event(
        "boox-compact-normalized-color-1",
        DeviceSide::Boox,
        1,
        RevisionPair::default(),
        compact_manifest(
            vec![Operation::UpsertStroke {
                source_uuid: original.source_uuid.clone(),
                page_index: original.page_index,
                before: None,
                after: original.clone(),
            }],
            1,
        ),
    );
    first.payload_kind = DevicePayloadKind::BooxOperationManifest;
    harness
        .broker
        .process(&mut harness.storage, &first)
        .unwrap();

    let first_state = harness.state();
    let normalized = &first_state.strokes["normalized-color"].snapshot;
    assert_eq!(normalized.native_style.pen_color, 0x9d);
    assert_eq!(
        normalized.geometry_fingerprint,
        geometry_fingerprint(&normalized.native_style, &normalized.samples)
    );
    let emitted: Manifest = serde_json::from_slice(
        &harness
            .storage
            .object(&supernote_manifest_path(
                &harness.document_id,
                "boox-compact-normalized-color-1",
            ))
            .unwrap()
            .bytes,
    )
    .unwrap();
    let Operation::UpsertStroke { after, .. } = &emitted.operations[0] else {
        panic!("fixture must contain an upsert")
    };
    assert_eq!(after, normalized);

    let mut moved = original.clone();
    moved.samples[0][0] += 0.02;
    moved.geometry_fingerprint = geometry_fingerprint(&moved.native_style, &moved.samples);
    let mut second = harness.event(
        "boox-compact-normalized-color-2",
        DeviceSide::Boox,
        2,
        RevisionPair {
            boox: 1,
            supernote: 0,
        },
        compact_manifest(
            vec![Operation::UpsertStroke {
                source_uuid: moved.source_uuid.clone(),
                page_index: moved.page_index,
                before: Some(original),
                after: moved.clone(),
            }],
            1,
        ),
    );
    second.payload_kind = DevicePayloadKind::BooxOperationManifest;
    harness
        .broker
        .process(&mut harness.storage, &second)
        .unwrap();
    let second_state = harness.state();
    let normalized_moved = &second_state.strokes["normalized-color"].snapshot;
    assert_eq!(normalized_moved.native_style.pen_color, 0x9d);
    assert_eq!(normalized_moved.samples, moved.samples);
}

#[test]
fn compact_manifest_accepts_the_broker_pdf_projection_and_preserves_native_metadata() {
    let mut harness = Harness::new();
    let mut original = stroke("pdf-projected-move", 0.2, 0.3);
    original.origin = "boox-neoreader".to_owned();
    original.native_style.layer_num = 3;
    original.native_style.pen_type = 16;
    original.native_style.pen_color = 130;
    original.samples[0][2] = 900.0;
    original.samples[1][2] = 2200.0;
    original.geometry_fingerprint = geometry_fingerprint(&original.native_style, &original.samples);
    let mut deleted_original = original.clone();
    deleted_original.source_uuid = "pdf-projected-delete".to_owned();
    for sample in &mut deleted_original.samples {
        sample[0] += 0.3;
    }
    deleted_original.geometry_fingerprint =
        geometry_fingerprint(&deleted_original.native_style, &deleted_original.samples);
    let mut first = harness.event(
        "boox-before-pdf-projection",
        DeviceSide::Boox,
        1,
        RevisionPair::default(),
        compact_manifest(
            vec![
                Operation::UpsertStroke {
                    source_uuid: original.source_uuid.clone(),
                    page_index: original.page_index,
                    before: None,
                    after: original,
                },
                Operation::UpsertStroke {
                    source_uuid: deleted_original.source_uuid.clone(),
                    page_index: deleted_original.page_index,
                    before: None,
                    after: deleted_original,
                },
            ],
            1,
        ),
    );
    first.payload_kind = DevicePayloadKind::BooxOperationManifest;
    harness
        .broker
        .process(&mut harness.storage, &first)
        .unwrap();

    let canonical = harness.state().strokes["pdf-projected-move"]
        .snapshot
        .clone();
    let mut projected = canonical.clone();
    projected.origin = "pdf-ink".to_owned();
    projected.native_style.layer_num = 0;
    projected.native_style.pen_type = 10;
    for sample in &mut projected.samples {
        sample[2] = 1612.0;
    }
    projected.geometry_fingerprint =
        geometry_fingerprint(&projected.native_style, &projected.samples);
    let deleted_canonical = harness.state().strokes["pdf-projected-delete"]
        .snapshot
        .clone();
    let mut projected_deleted = deleted_canonical.clone();
    projected_deleted.origin = "pdf-ink".to_owned();
    projected_deleted.native_style.layer_num = 0;
    projected_deleted.native_style.pen_type = 10;
    for sample in &mut projected_deleted.samples {
        sample[2] = 1612.0;
    }
    projected_deleted.geometry_fingerprint =
        geometry_fingerprint(&projected_deleted.native_style, &projected_deleted.samples);
    let mut moved = projected.clone();
    for sample in &mut moved.samples {
        sample[0] += 0.1;
        sample[1] -= 0.03;
    }
    moved.geometry_fingerprint = geometry_fingerprint(&moved.native_style, &moved.samples);
    let mut second = harness.event(
        "boox-after-pdf-projection",
        DeviceSide::Boox,
        2,
        RevisionPair {
            boox: 1,
            supernote: 0,
        },
        compact_manifest(
            vec![
                Operation::UpsertStroke {
                    source_uuid: moved.source_uuid.clone(),
                    page_index: moved.page_index,
                    before: Some(projected),
                    after: moved,
                },
                Operation::DeleteStroke {
                    source_uuid: projected_deleted.source_uuid.clone(),
                    page_index: projected_deleted.page_index,
                    before: projected_deleted,
                },
            ],
            1,
        ),
    );
    second.payload_kind = DevicePayloadKind::BooxOperationManifest;
    harness
        .broker
        .process(&mut harness.storage, &second)
        .unwrap();

    let stored = &harness.state().strokes["pdf-projected-move"].snapshot;
    assert_eq!(stored.origin, "boox-neoreader");
    assert_eq!(stored.native_style.layer_num, 3);
    assert_eq!(stored.native_style.pen_type, 16);
    assert_eq!(stored.native_style.pen_color, 0x9d);
    assert_eq!(stored.samples[0][2], 900.0);
    assert_eq!(stored.samples[1][2], 2200.0);
    assert!((stored.samples[0][0] - (canonical.samples[0][0] + 0.1)).abs() < 1.0e-12);
    assert!((stored.samples[0][1] - (canonical.samples[0][1] - 0.03)).abs() < 1.0e-12);
    assert!(harness.state().strokes["pdf-projected-delete"]
        .tombstone
        .is_some());
}

#[test]
fn compact_manifest_indexes_thousands_of_strokes_without_rescanning_operations() {
    const STROKE_COUNT: usize = 5_000;
    let mut harness = Harness::new();
    let operations = (0..STROKE_COUNT)
        .map(|index| {
            let snapshot = stroke(&format!("bulk-{index:05}"), 0.2, 0.3);
            Operation::UpsertStroke {
                source_uuid: snapshot.source_uuid.clone(),
                page_index: snapshot.page_index,
                before: None,
                after: snapshot,
            }
        })
        .collect();
    let mut event = harness.event(
        "boox-compact-bulk",
        DeviceSide::Boox,
        1,
        RevisionPair::default(),
        compact_manifest(operations, 1),
    );
    event.payload_kind = DevicePayloadKind::BooxOperationManifest;

    harness
        .broker
        .process(&mut harness.storage, &event)
        .unwrap();

    assert_eq!(harness.state().strokes.len(), STROKE_COUNT);
}

#[test]
fn compact_manifest_uses_the_broker_coordinate_calibration() {
    let mut harness = Harness::new();
    let snapshot = stroke("calibrated-stroke", 0.2, 0.3);
    let mut manifest: Manifest = serde_json::from_slice(&compact_manifest(
        vec![Operation::UpsertStroke {
            source_uuid: snapshot.source_uuid.clone(),
            page_index: snapshot.page_index,
            before: None,
            after: snapshot,
        }],
        1,
    ))
    .unwrap();
    let adapter_manifest_id = manifest.manifest_id.clone();
    manifest
        .coordinate_transform
        .pdf_to_supernote_normalized_y_offset = 0.25;
    let mut event = harness.event(
        "boox-compact-wrong-calibration",
        DeviceSide::Boox,
        1,
        RevisionPair::default(),
        serde_json::to_vec(&manifest).unwrap(),
    );
    event.payload_kind = DevicePayloadKind::BooxOperationManifest;

    harness
        .broker
        .process(&mut harness.storage, &event)
        .unwrap();
    let emitted: Manifest = serde_json::from_slice(
        &harness
            .storage
            .object(&supernote_manifest_path(
                &harness.document_id,
                "boox-compact-wrong-calibration",
            ))
            .unwrap()
            .bytes,
    )
    .unwrap();
    assert_eq!(
        emitted
            .coordinate_transform
            .pdf_to_supernote_normalized_y_offset,
        -0.0008
    );
    assert_ne!(emitted.manifest_id, adapter_manifest_id);
    assert!(emitted.manifest_id.starts_with("inkbridge-"));
}

#[test]
fn compact_delete_must_match_the_active_canonical_snapshot() {
    let mut harness = Harness::new();
    let export = harness.event(
        "sn-before-compact-delete",
        DeviceSide::Supernote,
        1,
        RevisionPair::default(),
        supernote_export(&[stroke("tracked-stroke", 0.2, 0.3)]),
    );
    harness
        .broker
        .process(&mut harness.storage, &export)
        .unwrap();
    let mut mismatched = harness.state().strokes["tracked-stroke"].snapshot.clone();
    mismatched.samples[0][0] += 0.01;
    mismatched.geometry_fingerprint =
        geometry_fingerprint(&mismatched.native_style, &mismatched.samples);
    let mut event = harness.event(
        "boox-bad-compact-delete",
        DeviceSide::Boox,
        1,
        RevisionPair {
            boox: 0,
            supernote: 1,
        },
        compact_manifest(
            vec![Operation::DeleteStroke {
                source_uuid: mismatched.source_uuid.clone(),
                page_index: mismatched.page_index,
                before: mismatched,
            }],
            1,
        ),
    );
    event.payload_kind = DevicePayloadKind::BooxOperationManifest;

    let error = harness
        .broker
        .process(&mut harness.storage, &event)
        .unwrap_err();
    assert!(matches!(error, BrokerError::InvalidEvent(_)));
    assert!(harness.state().strokes["tracked-stroke"]
        .tombstone
        .is_none());
}

#[test]
fn compact_delete_rejects_an_unknown_stroke() {
    let mut harness = Harness::new();
    let mut event = harness.event(
        "boox-unknown-compact-delete",
        DeviceSide::Boox,
        1,
        RevisionPair::default(),
        compact_manifest(
            vec![Operation::DeleteStroke {
                source_uuid: "unknown-stroke".to_owned(),
                page_index: 0,
                before: stroke("unknown-stroke", 0.2, 0.3),
            }],
            1,
        ),
    );
    event.payload_kind = DevicePayloadKind::BooxOperationManifest;

    assert!(matches!(
        harness.broker.process(&mut harness.storage, &event),
        Err(BrokerError::InvalidEvent(_))
    ));
    assert!(!harness.state().strokes.contains_key("unknown-stroke"));
}

#[test]
fn compact_upsert_must_match_the_active_canonical_snapshot() {
    let mut harness = Harness::new();
    let export = harness.event(
        "sn-before-compact-upsert",
        DeviceSide::Supernote,
        1,
        RevisionPair::default(),
        supernote_export(&[stroke("tracked-stroke", 0.2, 0.3)]),
    );
    harness
        .broker
        .process(&mut harness.storage, &export)
        .unwrap();
    let canonical = harness.state().strokes["tracked-stroke"].snapshot.clone();
    let mut stale_before = canonical.clone();
    stale_before.samples[0][0] += 0.01;
    stale_before.geometry_fingerprint =
        geometry_fingerprint(&stale_before.native_style, &stale_before.samples);
    let mut after = canonical.clone();
    after.samples[1][0] += 0.02;
    after.geometry_fingerprint = geometry_fingerprint(&after.native_style, &after.samples);
    let mut event = harness.event(
        "boox-bad-compact-upsert",
        DeviceSide::Boox,
        1,
        RevisionPair {
            boox: 0,
            supernote: 1,
        },
        compact_manifest(
            vec![Operation::UpsertStroke {
                source_uuid: after.source_uuid.clone(),
                page_index: after.page_index,
                before: Some(stale_before),
                after: after.clone(),
            }],
            1,
        ),
    );
    event.payload_kind = DevicePayloadKind::BooxOperationManifest;

    assert!(matches!(
        harness.broker.process(&mut harness.storage, &event),
        Err(BrokerError::InvalidEvent(_))
    ));
    assert_eq!(
        harness.state().strokes["tracked-stroke"].snapshot,
        canonical
    );

    let mut missing_before = harness.event(
        "boox-compact-upsert-without-before",
        DeviceSide::Boox,
        1,
        RevisionPair {
            boox: 0,
            supernote: 1,
        },
        compact_manifest(
            vec![Operation::UpsertStroke {
                source_uuid: after.source_uuid.clone(),
                page_index: after.page_index,
                before: None,
                after,
            }],
            1,
        ),
    );
    missing_before.payload_kind = DevicePayloadKind::BooxOperationManifest;
    assert!(matches!(
        harness
            .broker
            .process(&mut harness.storage, &missing_before),
        Err(BrokerError::InvalidEvent(_))
    ));
}

#[test]
fn compact_cross_page_move_accepts_none_before_with_matching_delete() {
    let mut harness = Harness::with_original(original_pdf_with_pages(2));
    let before = stroke_on_page("cross-page", 0, 0.2, 0.3);
    let export = harness.event(
        "sn-before-compact-cross-page",
        DeviceSide::Supernote,
        1,
        RevisionPair::default(),
        supernote_export(std::slice::from_ref(&before)),
    );
    harness
        .broker
        .process(&mut harness.storage, &export)
        .unwrap();
    let after = stroke_on_page("cross-page", 1, 0.3, 0.4);
    let mut event = harness.event(
        "boox-compact-cross-page",
        DeviceSide::Boox,
        1,
        RevisionPair {
            boox: 0,
            supernote: 1,
        },
        compact_manifest(
            vec![
                Operation::DeleteStroke {
                    source_uuid: before.source_uuid.clone(),
                    page_index: before.page_index,
                    before,
                },
                Operation::UpsertStroke {
                    source_uuid: after.source_uuid.clone(),
                    page_index: after.page_index,
                    before: None,
                    after: after.clone(),
                },
            ],
            2,
        ),
    );
    event.payload_kind = DevicePayloadKind::BooxOperationManifest;

    harness
        .broker
        .process(&mut harness.storage, &event)
        .unwrap();
    let canonical = &harness.state().strokes["cross-page"];
    assert_eq!(canonical.snapshot, after);
    assert!(canonical.tombstone.is_none());
}

#[test]
fn compact_cross_page_upsert_requires_a_matching_delete() {
    let mut harness = Harness::with_original(original_pdf_with_pages(2));
    let before = stroke_on_page("cross-page", 0, 0.2, 0.3);
    let export = harness.event(
        "sn-before-invalid-cross-page",
        DeviceSide::Supernote,
        1,
        RevisionPair::default(),
        supernote_export(std::slice::from_ref(&before)),
    );
    harness
        .broker
        .process(&mut harness.storage, &export)
        .unwrap();
    let after = stroke_on_page("cross-page", 1, 0.3, 0.4);
    let mut event = harness.event(
        "boox-cross-page-without-delete",
        DeviceSide::Boox,
        1,
        RevisionPair {
            boox: 0,
            supernote: 1,
        },
        compact_manifest(
            vec![Operation::UpsertStroke {
                source_uuid: after.source_uuid.clone(),
                page_index: after.page_index,
                before: Some(before.clone()),
                after,
            }],
            2,
        ),
    );
    event.payload_kind = DevicePayloadKind::BooxOperationManifest;

    assert!(matches!(
        harness.broker.process(&mut harness.storage, &event),
        Err(BrokerError::InvalidEvent(_))
    ));
    assert_eq!(harness.state().strokes["cross-page"].snapshot, before);
}

#[test]
fn preserved_compact_cross_page_upsert_requires_a_matching_delete() {
    let mut harness = Harness::with_original(original_pdf_with_pages(2));
    let before = stroke_on_page("preserved-cross-page", 0, 0.2, 0.3);
    let export = harness.event(
        "sn-before-preserved-cross-page",
        DeviceSide::Supernote,
        1,
        RevisionPair::default(),
        supernote_export(std::slice::from_ref(&before)),
    );
    harness
        .broker
        .process(&mut harness.storage, &export)
        .unwrap();

    let after = stroke_on_page("preserved-cross-page", 1, 0.3, 0.4);
    let mut event = harness.event(
        "boox-preserved-cross-page-without-delete",
        DeviceSide::Boox,
        1,
        RevisionPair::default(),
        compact_manifest(
            vec![Operation::UpsertStroke {
                source_uuid: after.source_uuid.clone(),
                page_index: after.page_index,
                before: Some(before.clone()),
                after,
            }],
            2,
        ),
    );
    event.payload_kind = DevicePayloadKind::BooxOperationManifest;
    assert!(matches!(
        harness
            .broker
            .process(&mut harness.storage, &event)
            .unwrap(),
        ProcessOutcome::Conflict { .. }
    ));

    assert!(matches!(
        harness.broker.inspect_conflict(
            &harness.storage,
            &harness.document_id,
            "boox-preserved-cross-page-without-delete",
        ),
        Err(BrokerError::InvalidEvent(_))
    ));

    let state_before = harness.state();
    let request = ConflictResolutionRequest {
        schema_version: RESOLUTION_SCHEMA_VERSION,
        resolution_id: "resolve-malformed-preserved-cross-page".to_owned(),
        document_id: harness.document_id.clone(),
        conflict_event_id: "boox-preserved-cross-page-without-delete".to_owned(),
        expected_state_revision: state_before.state_revision,
        expected_current_revisions: state_before.revisions(),
        strategy: ConflictResolutionStrategy::MergePreservingCurrent,
    };
    assert!(matches!(
        harness
            .broker
            .resolve_conflict(&mut harness.storage, &request),
        Err(BrokerError::InvalidEvent(_))
    ));
    assert_eq!(harness.state(), state_before);
    assert_eq!(
        harness.state().strokes["preserved-cross-page"].snapshot,
        before
    );
    assert!(harness
        .storage
        .object(&conflict_resolution_path(
            &harness.document_id,
            "boox-preserved-cross-page-without-delete"
        ))
        .is_none());
}
#[test]
fn preserved_compact_rejects_unknown_updates_and_deletes_before_resolution() {
    let unknown_update_before = stroke("preserved-unknown-update", 0.3, 0.4);
    let unknown_update_after = stroke("preserved-unknown-update", 0.5, 0.6);
    for (case, operation) in [
        (
            "delete",
            Operation::DeleteStroke {
                source_uuid: "preserved-unknown-delete".to_owned(),
                page_index: 0,
                before: stroke("preserved-unknown-delete", 0.2, 0.3),
            },
        ),
        (
            "update",
            Operation::UpsertStroke {
                source_uuid: "preserved-unknown-update".to_owned(),
                page_index: 0,
                before: Some(unknown_update_before.clone()),
                after: unknown_update_after.clone(),
            },
        ),
    ] {
        let mut harness = Harness::new();
        let base = stroke("sn-before-preserved-unknown", 0.1, 0.2);
        let initial = harness.event(
            &format!("sn-before-preserved-unknown-{case}"),
            DeviceSide::Supernote,
            1,
            RevisionPair::default(),
            supernote_export(std::slice::from_ref(&base)),
        );
        harness
            .broker
            .process(&mut harness.storage, &initial)
            .unwrap();

        let event_id = format!("boox-preserved-unknown-{case}");
        let mut event = harness.event(
            &event_id,
            DeviceSide::Boox,
            1,
            RevisionPair::default(),
            compact_manifest(vec![operation], 1),
        );
        event.payload_kind = DevicePayloadKind::BooxOperationManifest;
        assert!(matches!(
            harness
                .broker
                .process(&mut harness.storage, &event)
                .unwrap(),
            ProcessOutcome::Conflict { .. }
        ));

        assert!(matches!(
            harness
                .broker
                .inspect_conflict(&harness.storage, &harness.document_id, &event_id),
            Err(BrokerError::InvalidEvent(message)) if message.contains("unknown stroke")
        ));

        let state_before = harness.state();
        let request = ConflictResolutionRequest {
            schema_version: RESOLUTION_SCHEMA_VERSION,
            resolution_id: format!("resolve-preserved-unknown-{case}"),
            document_id: harness.document_id.clone(),
            conflict_event_id: event_id.clone(),
            expected_state_revision: state_before.state_revision,
            expected_current_revisions: state_before.revisions(),
            strategy: ConflictResolutionStrategy::AcceptIncoming,
        };
        assert!(matches!(
            harness
                .broker
                .resolve_conflict(&mut harness.storage, &request),
            Err(BrokerError::InvalidEvent(message)) if message.contains("unknown stroke")
        ));
        assert_eq!(harness.state(), state_before);
        assert!(harness
            .storage
            .object(&conflict_resolution_path(&harness.document_id, &event_id))
            .is_none());
    }
}

#[test]
fn compact_manifest_rejects_duplicate_upserts_for_one_stroke() {
    let mut harness = Harness::new();
    let first = stroke("duplicate", 0.2, 0.3);
    let second = stroke("duplicate", 0.4, 0.5);
    let mut event = harness.event(
        "boox-duplicate-upserts",
        DeviceSide::Boox,
        1,
        RevisionPair::default(),
        compact_manifest(
            vec![
                Operation::UpsertStroke {
                    source_uuid: first.source_uuid.clone(),
                    page_index: first.page_index,
                    before: None,
                    after: first,
                },
                Operation::UpsertStroke {
                    source_uuid: second.source_uuid.clone(),
                    page_index: second.page_index,
                    before: None,
                    after: second,
                },
            ],
            1,
        ),
    );
    event.payload_kind = DevicePayloadKind::BooxOperationManifest;

    assert!(matches!(
        harness.broker.process(&mut harness.storage, &event),
        Err(BrokerError::InvalidEvent(_))
    ));
    assert!(!harness.state().strokes.contains_key("duplicate"));
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
fn delayed_older_same_side_edit_is_preserved_as_conflict() {
    let mut harness = Harness::new();
    for revision in 1..=3 {
        let based_on = RevisionPair {
            boox: revision - 1,
            supernote: 0,
        };
        let pdf = write_boox_view(
            &harness.original,
            [stroke(
                &format!("accepted-boox-{revision}"),
                0.1 * revision as f64,
                0.2,
            )],
        )
        .unwrap();
        let event = harness.event(
            &format!("accepted-boox-{revision}"),
            DeviceSide::Boox,
            revision,
            based_on,
            pdf,
        );
        assert!(matches!(
            harness
                .broker
                .process(&mut harness.storage, &event)
                .unwrap(),
            ProcessOutcome::Applied { .. }
        ));
    }

    let delayed_edit = write_boox_view(
        &harness.original,
        [stroke("edit-from-older-delivery", 0.75, 0.65)],
    )
    .unwrap();
    let event = harness.event(
        "delayed-older-same-side-edit",
        DeviceSide::Boox,
        2,
        RevisionPair {
            boox: 1,
            supernote: 0,
        },
        delayed_edit,
    );
    let outcome = harness
        .broker
        .process(&mut harness.storage, &event)
        .unwrap();

    assert!(matches!(outcome, ProcessOutcome::Conflict { .. }));
    let state = harness.state();
    assert_eq!(state.boox.revision, 3);
    assert_eq!(state.conflicts.len(), 1);
    assert_eq!(state.conflicts[0].event_id, "delayed-older-same-side-edit");
}

#[test]
fn stale_based_event_cannot_jump_over_unreserved_source_revisions() {
    let mut harness = Harness::new();
    let future = write_boox_view(
        &harness.original,
        [stroke("unbounded-future-revision", 0.65, 0.7)],
    )
    .unwrap();
    let event = harness.event(
        "unbounded-future-revision",
        DeviceSide::Boox,
        9,
        RevisionPair::default(),
        future,
    );

    let error = harness
        .broker
        .process(&mut harness.storage, &event)
        .expect_err("unreserved future revision must fail closed");
    assert!(matches!(
        error,
        BrokerError::InvalidEvent(message)
            if message.contains("preserved predecessor conflict")
    ));
    assert!(harness.state().conflicts.is_empty());
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
fn atomic_spread_snapshot_moves_and_deletes_without_touching_other_pages() {
    let mut harness = Harness::with_original(original_pdf_with_pages(3));
    let outside = stroke_on_page("outside-spread", 2, 0.1, 0.2);
    let outside_event = harness.event(
        "sn-outside",
        DeviceSide::Supernote,
        1,
        RevisionPair::default(),
        supernote_export_page(2, std::slice::from_ref(&outside)),
    );
    harness
        .broker
        .process(&mut harness.storage, &outside_event)
        .unwrap();

    let left = stroke_on_page("move-between-halves", 0, 0.2, 0.3);
    let right = stroke_on_page("delete-on-right", 1, 0.6, 0.5);
    let initial_pages = [
        (0, std::slice::from_ref(&left)),
        (1, std::slice::from_ref(&right)),
    ];
    let initial = harness.event(
        "sn-spread-initial",
        DeviceSide::Supernote,
        2,
        RevisionPair {
            boox: 0,
            supernote: 1,
        },
        supernote_export_pages(&initial_pages),
    );
    harness
        .broker
        .process(&mut harness.storage, &initial)
        .unwrap();

    let moved = stroke_on_page("move-between-halves", 1, 0.35, 0.4);
    let empty: &[StrokeSnapshot] = &[];
    let replacement_pages = [(0, empty), (1, std::slice::from_ref(&moved))];
    let replacement = harness.event(
        "sn-spread-replacement",
        DeviceSide::Supernote,
        3,
        RevisionPair {
            boox: 0,
            supernote: 2,
        },
        supernote_export_pages(&replacement_pages),
    );
    assert!(matches!(
        harness
            .broker
            .process(&mut harness.storage, &replacement)
            .unwrap(),
        ProcessOutcome::Applied { .. }
    ));

    let state = harness.state();
    let moved_state = &state.strokes["move-between-halves"];
    assert_eq!(moved_state.snapshot.page_index, 1);
    assert_eq!(
        moved_state.snapshot.geometry_fingerprint,
        moved.geometry_fingerprint
    );
    assert!(moved_state.tombstone.is_none());
    assert!(state.strokes["delete-on-right"].tombstone.is_some());
    assert_eq!(
        state.strokes["outside-spread"]
            .snapshot
            .geometry_fingerprint,
        outside.geometry_fingerprint
    );
    assert_eq!(state.strokes["outside-spread"].snapshot.page_index, 2);
    assert!(state.strokes["outside-spread"].tombstone.is_none());

    let state_revision = state.state_revision;
    assert!(matches!(
        harness
            .broker
            .process(&mut harness.storage, &replacement)
            .unwrap(),
        ProcessOutcome::Duplicate { .. }
    ));
    assert_eq!(harness.state().state_revision, state_revision);
}

#[test]
fn invalid_spread_snapshot_fails_before_canonical_state_changes() {
    let mut harness = Harness::with_original(original_pdf_with_pages(2));
    let before = harness.state();
    let duplicate_id = stroke_on_page("duplicate", 0, 0.1, 0.2);
    let same_id_other_page = stroke_on_page("duplicate", 1, 0.3, 0.4);
    let pages = [
        (0, std::slice::from_ref(&duplicate_id)),
        (1, std::slice::from_ref(&same_id_other_page)),
    ];
    let event = harness.event(
        "sn-invalid-spread",
        DeviceSide::Supernote,
        1,
        RevisionPair::default(),
        supernote_export_pages(&pages),
    );
    assert!(matches!(
        harness.broker.process(&mut harness.storage, &event),
        Err(BrokerError::Conversion(message)) if message.contains("repeats stroke identity duplicate")
    ));
    assert_eq!(harness.state(), before);
}

#[test]
fn spread_snapshot_rejects_a_mismatched_document_or_revision_frontier() {
    let mut harness = Harness::with_original(original_pdf_with_pages(2));
    let wrong_document = serde_json::to_vec(&json!({
        "schemaVersion": 1,
        "documentId": format!("inkbridge-doc-v1-{}", "f".repeat(64)),
        "basedOn": {"boox": 0, "supernote": 0},
        "pages": [
            {"pageIndex": 0, "strokes": []},
            {"pageIndex": 1, "strokes": []}
        ]
    }))
    .unwrap();
    let event = harness.event(
        "sn-wrong-document",
        DeviceSide::Supernote,
        1,
        RevisionPair::default(),
        wrong_document,
    );
    assert!(matches!(
        harness.broker.process(&mut harness.storage, &event),
        Err(BrokerError::InvalidEvent(message)) if message.contains("documentId does not match")
    ));

    let wrong_frontier = serde_json::to_vec(&json!({
        "schemaVersion": 1,
        "documentId": harness.document_id,
        "basedOn": {"boox": 1, "supernote": 0},
        "pages": [
            {"pageIndex": 0, "strokes": []},
            {"pageIndex": 1, "strokes": []}
        ]
    }))
    .unwrap();
    let event = harness.event(
        "sn-wrong-frontier",
        DeviceSide::Supernote,
        1,
        RevisionPair::default(),
        wrong_frontier,
    );
    assert!(matches!(
        harness.broker.process(&mut harness.storage, &event),
        Err(BrokerError::InvalidEvent(message)) if message.contains("basedOn does not match")
    ));
    assert_eq!(harness.state().state_revision, 0);
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

fn harness_with_in_place_boox_conflict() -> Harness {
    let mut harness = Harness::new();
    let initial = harness.event(
        "sn-base-in-place-conflict",
        DeviceSide::Supernote,
        1,
        RevisionPair::default(),
        supernote_export(&[stroke("shared", 0.2, 0.3)]),
    );
    harness
        .broker
        .process(&mut harness.storage, &initial)
        .unwrap();

    let boox_path = boox_view_path(&harness.state());
    let generated = harness.storage.object(&boox_path).unwrap().clone();
    let edited_pdf = write_boox_view(
        &harness.original,
        [
            stroke("shared", 0.35, 0.4),
            stroke("boox-concurrent", 0.65, 0.65),
        ],
    )
    .unwrap();
    let edited_hash = sha256_hex(&edited_pdf);
    let edited_object = harness
        .storage
        .put_unchecked(&boox_path, edited_pdf, generated.metadata);
    let event = StorageEvent {
        schema_version: EVENT_SCHEMA_VERSION,
        event_id: "boox-in-place-conflict".to_owned(),
        document_id: harness.document_id.clone(),
        source: DeviceSide::Boox,
        object_path: boox_path,
        source_generation: edited_object.generation,
        source_revision: 1,
        based_on: RevisionPair::default(),
        content_sha256: edited_hash,
        payload_kind: DevicePayloadKind::DeviceView,
        broker_output: Some(BrokerOutputMarker {
            producer: BROKER_PRODUCER.to_owned(),
            event_id: "sn-base-in-place-conflict".to_owned(),
            document_id: harness.document_id.clone(),
            source_revisions: RevisionPair {
                boox: 0,
                supernote: 1,
            },
        }),
    };
    assert!(matches!(
        harness
            .broker
            .process(&mut harness.storage, &event)
            .unwrap(),
        ProcessOutcome::Conflict { .. }
    ));
    harness
}

fn harness_with_mixed_supernote_conflict() -> Harness {
    let mut harness = Harness::new();
    let initial = harness.event(
        "sn-base",
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
    let current_pdf = write_boox_view(
        &harness.original,
        [
            stroke("shared", 0.35, 0.3),
            stroke("boox-current", 0.55, 0.55),
        ],
    )
    .unwrap();
    let current = harness.event("boox-current", DeviceSide::Boox, 1, common, current_pdf);
    harness
        .broker
        .process(&mut harness.storage, &current)
        .unwrap();

    let incoming = harness.event(
        "sn-concurrent",
        DeviceSide::Supernote,
        2,
        common,
        supernote_export(&[stroke("shared", 0.2, 0.45), stroke("sn-new", 0.75, 0.7)]),
    );
    assert!(matches!(
        harness
            .broker
            .process(&mut harness.storage, &incoming)
            .unwrap(),
        ProcessOutcome::Conflict { .. }
    ));
    harness
}

fn harness_with_rejected_compact_ancestor() -> (Harness, StrokeSnapshot, StrokeSnapshot) {
    let mut harness = Harness::new();
    let base = stroke("shared", 0.2, 0.3);
    let initial = harness.event(
        "sn-base-dependent-conflict",
        DeviceSide::Supernote,
        1,
        RevisionPair::default(),
        supernote_export(std::slice::from_ref(&base)),
    );
    harness
        .broker
        .process(&mut harness.storage, &initial)
        .unwrap();

    let intermediate = stroke("shared", 0.4, 0.4);
    let mut first = harness.event(
        "boox-dependent-conflict-1",
        DeviceSide::Boox,
        1,
        RevisionPair::default(),
        compact_manifest(
            vec![Operation::UpsertStroke {
                source_uuid: "shared".to_owned(),
                page_index: 0,
                before: Some(base.clone()),
                after: intermediate.clone(),
            }],
            1,
        ),
    );
    first.payload_kind = DevicePayloadKind::BooxOperationManifest;
    assert!(matches!(
        harness
            .broker
            .process(&mut harness.storage, &first)
            .unwrap(),
        ProcessOutcome::Conflict { .. }
    ));

    let descendant = stroke("shared", 0.65, 0.55);
    let mut second = harness.event(
        "boox-dependent-conflict-2",
        DeviceSide::Boox,
        2,
        RevisionPair {
            boox: 1,
            supernote: 1,
        },
        compact_manifest(
            vec![Operation::UpsertStroke {
                source_uuid: "shared".to_owned(),
                page_index: 0,
                before: Some(intermediate),
                after: descendant.clone(),
            }],
            1,
        ),
    );
    second.payload_kind = DevicePayloadKind::BooxOperationManifest;
    assert!(matches!(
        harness
            .broker
            .process(&mut harness.storage, &second)
            .unwrap(),
        ProcessOutcome::Conflict { .. }
    ));

    let first_analysis = harness
        .broker
        .inspect_conflict(
            &harness.storage,
            &harness.document_id,
            "boox-dependent-conflict-1",
        )
        .unwrap();
    let first_request = resolution_request(
        &harness,
        &first_analysis,
        "reject-dependent-conflict-1",
        ConflictResolutionStrategy::KeepCurrent,
    );
    harness
        .broker
        .resolve_conflict(&mut harness.storage, &first_request)
        .unwrap();
    assert_eq!(harness.state().strokes["shared"].snapshot, base);

    (harness, base, descendant)
}

fn resolution_request(
    harness: &Harness,
    analysis: &ConflictAnalysis,
    id: &str,
    strategy: ConflictResolutionStrategy,
) -> ConflictResolutionRequest {
    ConflictResolutionRequest {
        schema_version: RESOLUTION_SCHEMA_VERSION,
        resolution_id: id.to_owned(),
        document_id: harness.document_id.clone(),
        conflict_event_id: analysis.conflict_event_id.clone(),
        expected_state_revision: analysis.state_revision,
        expected_current_revisions: analysis.current_revisions,
        strategy,
    }
}

#[test]
fn resolution_id_does_not_deduplicate_a_later_device_event() {
    let mut harness = Harness::new();
    let base = stroke("resolution-id-base", 0.2, 0.3);
    let initial = harness.event(
        "resolution-id-sn-base",
        DeviceSide::Supernote,
        1,
        RevisionPair::default(),
        supernote_export(std::slice::from_ref(&base)),
    );
    harness
        .broker
        .process(&mut harness.storage, &initial)
        .unwrap();

    let incoming = stroke("resolution-id-incoming", 0.55, 0.55);
    let conflict_pdf = write_boox_view(&harness.original, [base, incoming.clone()]).unwrap();
    let conflict = harness.event(
        "resolution-id-conflict",
        DeviceSide::Boox,
        1,
        RevisionPair::default(),
        conflict_pdf,
    );
    assert!(matches!(
        harness
            .broker
            .process(&mut harness.storage, &conflict)
            .unwrap(),
        ProcessOutcome::Conflict { .. }
    ));

    let analysis = harness
        .broker
        .inspect_conflict(
            &harness.storage,
            &harness.document_id,
            "resolution-id-conflict",
        )
        .unwrap();
    let shared_id = "later-device-event-id";
    let request = resolution_request(
        &harness,
        &analysis,
        shared_id,
        ConflictResolutionStrategy::MergePreservingCurrent,
    );
    harness
        .broker
        .resolve_conflict(&mut harness.storage, &request)
        .unwrap();

    let resolved = harness.state();
    assert_eq!(resolved.boox.revision, 1);
    assert!(!resolved.processed_event_ids.contains(shared_id));
    let imported = &resolved.strokes["resolution-id-incoming"];
    assert!(imported.tombstone.is_none());

    let next = stroke("resolution-id-next-device-stroke", 0.75, 0.7);
    let mut active = resolved
        .strokes
        .values()
        .filter(|stroke| stroke.tombstone.is_none())
        .map(|stroke| stroke.snapshot.clone())
        .collect::<Vec<_>>();
    active.push(next.clone());
    let next_pdf = write_boox_view(&harness.original, active).unwrap();
    let next_event = harness.event(
        shared_id,
        DeviceSide::Boox,
        2,
        resolved.revisions(),
        next_pdf,
    );
    assert!(matches!(
        harness
            .broker
            .process(&mut harness.storage, &next_event)
            .unwrap(),
        ProcessOutcome::Applied { .. }
    ));

    let state = harness.state();
    assert_eq!(state.boox.revision, 2);
    assert!(state.processed_event_ids.contains(shared_id));
    let applied = &state.strokes["resolution-id-next-device-stroke"];
    assert!(applied.tombstone.is_none());
}

#[test]
fn tracked_in_place_boox_conflict_resolves_against_its_exact_generation() {
    let mut harness = harness_with_in_place_boox_conflict();
    let boox_path = boox_view_path(&harness.state());
    let conflict_generation = harness.storage.object(&boox_path).unwrap().generation;
    let analysis = harness
        .broker
        .inspect_conflict(
            &harness.storage,
            &harness.document_id,
            "boox-in-place-conflict",
        )
        .unwrap();
    let request = resolution_request(
        &harness,
        &analysis,
        "resolve-boox-in-place",
        ConflictResolutionStrategy::MergePreservingCurrent,
    );

    assert!(matches!(
        harness
            .broker
            .resolve_conflict(&mut harness.storage, &request)
            .unwrap(),
        ConflictResolutionOutcome::Resolved { .. }
    ));

    let state = harness.state();
    assert!(state.conflicts.is_empty());
    assert_eq!(state.boox.revision, 1);
    assert!(state.strokes["boox-concurrent"].tombstone.is_none());
    assert!(
        harness.storage.object(&boox_path).unwrap().generation > conflict_generation,
        "the exact conflicting generation should be accepted as the write baseline"
    );
    assert!(harness
        .storage
        .object(&conflict_resolution_path(
            &harness.document_id,
            "boox-in-place-conflict"
        ))
        .is_some());
}

#[test]
fn tracked_in_place_boox_conflict_rejects_a_later_destination_edit() {
    let mut harness = harness_with_in_place_boox_conflict();
    let analysis = harness
        .broker
        .inspect_conflict(
            &harness.storage,
            &harness.document_id,
            "boox-in-place-conflict",
        )
        .unwrap();
    let request = resolution_request(
        &harness,
        &analysis,
        "resolve-boox-in-place-after-newer-edit",
        ConflictResolutionStrategy::MergePreservingCurrent,
    );
    let boox_path = boox_view_path(&harness.state());
    let newer = harness.storage.put_unchecked(
        &boox_path,
        b"newer post-preservation BOOX edit".to_vec(),
        BTreeMap::new(),
    );
    let state_before = harness.state();

    assert!(matches!(
        harness
            .broker
            .resolve_conflict(&mut harness.storage, &request),
        Err(BrokerError::StaleDestination { .. })
    ));
    assert_eq!(harness.state(), state_before);
    assert_eq!(
        harness.storage.object(&boox_path).unwrap().generation,
        newer.generation
    );
    assert!(harness
        .storage
        .object(&conflict_resolution_path(
            &harness.document_id,
            "boox-in-place-conflict"
        ))
        .is_none());
}

#[test]
fn conflict_inspection_separates_safe_changes_from_overlaps() {
    let harness = harness_with_mixed_supernote_conflict();
    let analysis = harness
        .broker
        .inspect_conflict(&harness.storage, &harness.document_id, "sn-concurrent")
        .unwrap();
    let summaries = harness
        .broker
        .list_conflicts(&harness.storage, &harness.document_id)
        .unwrap();
    assert_eq!(summaries.len(), 1);
    assert_eq!(summaries[0].conflict_event_id, "sn-concurrent");
    assert_eq!(summaries[0].current_revisions, analysis.current_revisions);
    assert_eq!(summaries[0].state_revision, analysis.state_revision);

    assert_eq!(
        analysis.current_revisions,
        RevisionPair {
            boox: 1,
            supernote: 1
        }
    );
    assert_eq!(
        analysis.based_on,
        RevisionPair {
            boox: 0,
            supernote: 1
        }
    );
    assert_eq!(
        analysis
            .safe_changes
            .iter()
            .map(|change| (change.stroke_id.as_str(), change.kind))
            .collect::<Vec<_>>(),
        vec![("sn-new", ConflictChangeKind::Add)]
    );
    assert_eq!(
        analysis
            .overlapping_changes
            .iter()
            .map(|change| change.stroke_id.as_str())
            .collect::<std::collections::BTreeSet<_>>(),
        std::collections::BTreeSet::from(["boox-current", "shared"])
    );
}

#[test]
fn conflict_inspection_classifies_a_page_only_spread_change_as_a_move() {
    let mut harness = Harness::with_original(original_pdf_with_pages(2));
    let original = stroke_on_page("page-only-move", 0, 0.2, 0.3);
    let initial_pages = [
        (0, std::slice::from_ref(&original)),
        (1, &[] as &[StrokeSnapshot]),
    ];
    let initial = harness.event(
        "sn-page-only-move-base",
        DeviceSide::Supernote,
        1,
        RevisionPair::default(),
        supernote_export_pages(&initial_pages),
    );
    harness
        .broker
        .process(&mut harness.storage, &initial)
        .unwrap();

    let common = RevisionPair {
        boox: 0,
        supernote: 1,
    };
    let mut boox = harness.event(
        "boox-empty-concurrent-revision",
        DeviceSide::Boox,
        1,
        common,
        compact_manifest(Vec::new(), 2),
    );
    boox.payload_kind = DevicePayloadKind::BooxOperationManifest;
    harness.broker.process(&mut harness.storage, &boox).unwrap();

    let mut moved = original.clone();
    moved.page_index = 1;
    let moved_pages = [
        (0, &[] as &[StrokeSnapshot]),
        (1, std::slice::from_ref(&moved)),
    ];
    let incoming = harness.event(
        "sn-page-only-move-conflict",
        DeviceSide::Supernote,
        2,
        common,
        supernote_export_pages(&moved_pages),
    );
    assert!(matches!(
        harness
            .broker
            .process(&mut harness.storage, &incoming)
            .unwrap(),
        ProcessOutcome::Conflict { .. }
    ));

    let analysis = harness
        .broker
        .inspect_conflict(
            &harness.storage,
            &harness.document_id,
            "sn-page-only-move-conflict",
        )
        .unwrap();
    assert_eq!(
        analysis
            .safe_changes
            .iter()
            .map(|change| (change.stroke_id.as_str(), change.kind, change.page_index))
            .collect::<Vec<_>>(),
        vec![("page-only-move", ConflictChangeKind::Move, 1)]
    );
    assert!(analysis.overlapping_changes.is_empty());
}

#[test]
fn atomic_spread_conflict_can_merge_safe_half_without_losing_the_other_half() {
    let mut harness = Harness::with_original(original_pdf_with_pages(2));
    let left = stroke_on_page("spread-left", 0, 0.2, 0.3);
    let right = stroke_on_page("spread-right", 1, 0.6, 0.5);
    let initial_pages = [
        (0, std::slice::from_ref(&left)),
        (1, std::slice::from_ref(&right)),
    ];
    let initial = harness.event(
        "sn-spread-base",
        DeviceSide::Supernote,
        1,
        RevisionPair::default(),
        supernote_export_pages(&initial_pages),
    );
    harness
        .broker
        .process(&mut harness.storage, &initial)
        .unwrap();

    let common = RevisionPair {
        boox: 0,
        supernote: 1,
    };
    let boox_left = stroke_on_page("spread-left", 0, 0.3, 0.35);
    let boox_pdf = write_boox_view(&harness.original, [boox_left.clone(), right.clone()]).unwrap();
    let boox = harness.event("boox-spread-edit", DeviceSide::Boox, 1, common, boox_pdf);
    harness.broker.process(&mut harness.storage, &boox).unwrap();

    let supernote_right = stroke_on_page("spread-right", 1, 0.7, 0.55);
    let supernote_new = stroke_on_page("spread-new", 0, 0.75, 0.7);
    let incoming_left = [left.clone(), supernote_new.clone()];
    let incoming_pages = [
        (0, incoming_left.as_slice()),
        (1, std::slice::from_ref(&supernote_right)),
    ];
    let incoming = harness.event(
        "sn-spread-conflict",
        DeviceSide::Supernote,
        2,
        common,
        supernote_export_pages(&incoming_pages),
    );
    assert!(matches!(
        harness
            .broker
            .process(&mut harness.storage, &incoming)
            .unwrap(),
        ProcessOutcome::Conflict { .. }
    ));

    let analysis = harness
        .broker
        .inspect_conflict(&harness.storage, &harness.document_id, "sn-spread-conflict")
        .unwrap();
    assert!(analysis
        .safe_changes
        .iter()
        .any(|change| change.stroke_id == "spread-right"));
    assert!(analysis
        .safe_changes
        .iter()
        .any(|change| change.stroke_id == "spread-new"));
    assert!(analysis
        .overlapping_changes
        .iter()
        .any(|change| change.stroke_id == "spread-left"));

    let request = resolution_request(
        &harness,
        &analysis,
        "resolve-spread-conflict",
        ConflictResolutionStrategy::MergePreservingCurrent,
    );
    harness
        .broker
        .resolve_conflict(&mut harness.storage, &request)
        .unwrap();

    let state = harness.state();
    assert_eq!(
        state.strokes["spread-left"].snapshot.geometry_fingerprint,
        boox_left.geometry_fingerprint
    );
    assert_eq!(
        state.strokes["spread-right"].snapshot.geometry_fingerprint,
        supernote_right.geometry_fingerprint
    );
    assert_eq!(
        state.strokes["spread-new"].snapshot.geometry_fingerprint,
        supernote_new.geometry_fingerprint
    );
    assert!(state.conflicts.is_empty());
}

#[test]
fn merge_preserving_current_applies_safe_changes_and_is_idempotent() {
    let mut harness = harness_with_mixed_supernote_conflict();
    let before = harness.state();
    let preserved_path = before.conflicts[0].preserved_path.clone();
    let expected_shared = before.strokes["shared"].snapshot.clone();
    let expected_current = before.strokes["boox-current"].snapshot.clone();
    let analysis = harness
        .broker
        .inspect_conflict(&harness.storage, &harness.document_id, "sn-concurrent")
        .unwrap();
    let request = resolution_request(
        &harness,
        &analysis,
        "resolve-merge",
        ConflictResolutionStrategy::MergePreservingCurrent,
    );

    let outcome = harness
        .broker
        .resolve_conflict(&mut harness.storage, &request)
        .unwrap();
    let ConflictResolutionOutcome::Resolved {
        source_revisions,
        applied_stroke_ids,
        preserved_current_stroke_ids,
        outputs,
        ..
    } = outcome
    else {
        panic!("first resolution was not applied")
    };
    assert_eq!(
        source_revisions,
        RevisionPair {
            boox: 1,
            supernote: 2
        }
    );
    assert_eq!(applied_stroke_ids, vec!["sn-new"]);
    assert_eq!(
        preserved_current_stroke_ids
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>(),
        std::collections::BTreeSet::from(["boox-current".to_owned(), "shared".to_owned()])
    );
    assert_eq!(outputs.len(), 2);

    let state = harness.state();
    assert_eq!(
        state.revisions(),
        RevisionPair {
            boox: 1,
            supernote: 2
        }
    );
    assert!(state.conflicts.is_empty());
    assert!(harness
        .broker
        .list_conflicts(&harness.storage, &harness.document_id)
        .unwrap()
        .is_empty());
    assert_eq!(state.strokes["shared"].snapshot, expected_shared);
    assert_eq!(state.strokes["boox-current"].snapshot, expected_current);
    assert!(state.strokes.contains_key("sn-new"));
    assert!(harness.storage.object(&preserved_path).is_some());
    let marker_path = conflict_resolution_path(&harness.document_id, "sn-concurrent");
    let marker = harness.storage.object(&marker_path).unwrap();
    assert_eq!(
        marker.metadata.get("inkbridge-kind").map(String::as_str),
        Some("conflict-resolution")
    );

    assert!(matches!(
        harness
            .broker
            .resolve_conflict(&mut harness.storage, &request)
            .unwrap(),
        ConflictResolutionOutcome::Duplicate {
            strategy: ConflictResolutionStrategy::MergePreservingCurrent,
            ..
        }
    ));

    let mut changed_decision = request.clone();
    changed_decision.strategy = ConflictResolutionStrategy::AcceptIncoming;
    assert!(matches!(
        harness
            .broker
            .resolve_conflict(&mut harness.storage, &changed_decision),
        Err(BrokerError::InvalidEvent(message))
            if message.contains("already recorded strategy")
                && message.contains("MergePreservingCurrent")
                && message.contains("AcceptIncoming")
    ));
}

#[test]
fn supernote_conflict_without_a_filename_preserves_the_tracked_name() {
    let mut harness = Harness::new();
    let shared = stroke("shared-renamed-document", 0.2, 0.3);
    let mut renamed_export = serde_json::from_slice::<serde_json::Value>(&supernote_export(
        std::slice::from_ref(&shared),
    ))
    .unwrap();
    renamed_export["sourceFileName"] = json!("renamed-on-supernote.pdf");
    let initial = harness.event(
        "sn-renamed-document",
        DeviceSide::Supernote,
        1,
        RevisionPair::default(),
        serde_json::to_vec(&renamed_export).unwrap(),
    );
    harness
        .broker
        .process(&mut harness.storage, &initial)
        .unwrap();
    assert_eq!(
        harness.state().supernote.source_file_name.as_deref(),
        Some("renamed-on-supernote.pdf")
    );

    let common = RevisionPair {
        boox: 0,
        supernote: 1,
    };
    let boox_pdf = write_boox_view(
        &harness.original,
        [
            shared.clone(),
            stroke("boox-current-renamed-document", 0.55, 0.55),
        ],
    )
    .unwrap();
    let boox = harness.event(
        "boox-current-renamed-document",
        DeviceSide::Boox,
        1,
        common,
        boox_pdf,
    );
    harness.broker.process(&mut harness.storage, &boox).unwrap();

    let mut unnamed_export = serde_json::from_slice::<serde_json::Value>(&supernote_export(&[
        shared,
        stroke("sn-new-renamed-document", 0.75, 0.7),
    ]))
    .unwrap();
    unnamed_export
        .as_object_mut()
        .unwrap()
        .remove("sourceFileName");
    let conflict = harness.event(
        "sn-unnamed-conflict",
        DeviceSide::Supernote,
        2,
        common,
        serde_json::to_vec(&unnamed_export).unwrap(),
    );
    assert!(matches!(
        harness
            .broker
            .process(&mut harness.storage, &conflict)
            .unwrap(),
        ProcessOutcome::Conflict { .. }
    ));

    let analysis = harness
        .broker
        .inspect_conflict(
            &harness.storage,
            &harness.document_id,
            "sn-unnamed-conflict",
        )
        .unwrap();
    let request = resolution_request(
        &harness,
        &analysis,
        "resolve-unnamed-supernote-conflict",
        ConflictResolutionStrategy::MergePreservingCurrent,
    );
    harness
        .broker
        .resolve_conflict(&mut harness.storage, &request)
        .unwrap();

    assert_eq!(
        harness.state().supernote.source_file_name.as_deref(),
        Some("renamed-on-supernote.pdf")
    );
    let manifest: Manifest = serde_json::from_slice(
        &harness
            .storage
            .object(&supernote_manifest_path(
                &harness.document_id,
                "resolve-unnamed-supernote-conflict",
            ))
            .unwrap()
            .bytes,
    )
    .unwrap();
    assert_eq!(
        manifest.document.target_file_names,
        vec!["renamed-on-supernote.pdf"]
    );
}

#[test]
fn explicit_keep_current_advances_the_rejected_source_frontier() {
    let mut harness = harness_with_mixed_supernote_conflict();
    let expected = harness.state();
    let analysis = harness
        .broker
        .inspect_conflict(&harness.storage, &harness.document_id, "sn-concurrent")
        .unwrap();
    let request = resolution_request(
        &harness,
        &analysis,
        "resolve-keep-current",
        ConflictResolutionStrategy::KeepCurrent,
    );
    harness
        .broker
        .resolve_conflict(&mut harness.storage, &request)
        .unwrap();

    let state = harness.state();
    assert_eq!(
        state.revisions(),
        RevisionPair {
            boox: 1,
            supernote: 2
        }
    );
    assert_eq!(state.strokes, expected.strokes);
    assert!(state.conflicts.is_empty());

    let reconciled = harness.event(
        "sn-after-resolution",
        DeviceSide::Supernote,
        3,
        RevisionPair {
            boox: 1,
            supernote: 2,
        },
        supernote_export(&[
            state.strokes["shared"].snapshot.clone(),
            state.strokes["boox-current"].snapshot.clone(),
        ]),
    );
    assert!(matches!(
        harness
            .broker
            .process(&mut harness.storage, &reconciled)
            .unwrap(),
        ProcessOutcome::Applied { .. }
    ));
}

#[test]
fn equal_revision_alternate_payload_can_only_be_discarded() {
    let mut harness = Harness::new();
    let accepted = stroke("accepted-at-revision-one", 0.2, 0.3);
    let initial = harness.event(
        "sn-accepted-revision-one",
        DeviceSide::Supernote,
        1,
        RevisionPair::default(),
        supernote_export(std::slice::from_ref(&accepted)),
    );
    harness
        .broker
        .process(&mut harness.storage, &initial)
        .unwrap();
    let accepted_state = harness.state();

    let alternate = stroke("alternate-at-revision-one", 0.7, 0.65);
    let conflicting = harness.event(
        "sn-alternate-revision-one",
        DeviceSide::Supernote,
        1,
        RevisionPair::default(),
        supernote_export(std::slice::from_ref(&alternate)),
    );
    assert!(matches!(
        harness
            .broker
            .process(&mut harness.storage, &conflicting)
            .unwrap(),
        ProcessOutcome::Conflict { .. }
    ));

    let analysis = harness
        .broker
        .inspect_conflict(
            &harness.storage,
            &harness.document_id,
            "sn-alternate-revision-one",
        )
        .unwrap();
    let conflicted_state = harness.state();
    for (resolution_id, strategy) in [
        (
            "reject-equal-revision-accept",
            ConflictResolutionStrategy::AcceptIncoming,
        ),
        (
            "reject-equal-revision-merge",
            ConflictResolutionStrategy::MergePreservingCurrent,
        ),
    ] {
        let request = resolution_request(&harness, &analysis, resolution_id, strategy);
        assert!(
            matches!(
                harness
                    .broker
                    .resolve_conflict(&mut harness.storage, &request),
                Err(BrokerError::InvalidEvent(message))
                    if message.contains("only keep_current is safe")
            ),
            "{strategy:?} must not change content at an accepted revision"
        );
        assert_eq!(harness.state(), conflicted_state);
        assert!(harness
            .storage
            .object(&conflict_resolution_path(
                &harness.document_id,
                "sn-alternate-revision-one",
            ))
            .is_none());
    }

    let keep = resolution_request(
        &harness,
        &analysis,
        "discard-equal-revision-alternate",
        ConflictResolutionStrategy::KeepCurrent,
    );
    let outcome = harness
        .broker
        .resolve_conflict(&mut harness.storage, &keep)
        .unwrap();
    let ConflictResolutionOutcome::Resolved {
        source_revisions,
        outputs,
        ..
    } = outcome
    else {
        panic!("equal-revision keep-current did not generate corrective outputs")
    };
    assert_eq!(source_revisions, accepted_state.revisions());
    assert_eq!(outputs.len(), 2);

    let discarded_state = harness.state();
    assert_eq!(discarded_state.revisions(), accepted_state.revisions());
    assert_eq!(
        discarded_state.supernote.content_sha256,
        accepted_state.supernote.content_sha256
    );
    assert_eq!(
        discarded_state.strokes["accepted-at-revision-one"].snapshot,
        accepted
    );
    assert!(!discarded_state
        .strokes
        .contains_key("alternate-at-revision-one"));
    assert!(discarded_state.conflicts.is_empty());
    let record = &discarded_state.resolved_conflicts["sn-alternate-revision-one"];
    assert!(!record.superseded);
    assert_eq!(record.strategy, ConflictResolutionStrategy::KeepCurrent);

    let retry = harness.event(
        "sn-alternate-retried-at-revision-two",
        DeviceSide::Supernote,
        2,
        RevisionPair {
            boox: 0,
            supernote: 1,
        },
        supernote_export(&[accepted, alternate.clone()]),
    );
    assert!(matches!(
        harness
            .broker
            .process(&mut harness.storage, &retry)
            .unwrap(),
        ProcessOutcome::Applied { .. }
    ));
    assert_eq!(
        harness.state().strokes["alternate-at-revision-one"].snapshot,
        alternate
    );
}

#[test]
fn equal_revision_in_place_boox_keep_current_rebuilds_canonical_view() {
    let mut harness = Harness::new();
    let base = stroke("sn-base-equal-boox", 0.2, 0.3);
    let initial = harness.event(
        "sn-base-equal-boox",
        DeviceSide::Supernote,
        1,
        RevisionPair::default(),
        supernote_export(std::slice::from_ref(&base)),
    );
    harness
        .broker
        .process(&mut harness.storage, &initial)
        .unwrap();

    let boox_path = boox_view_path(&harness.state());
    let generated = harness.storage.object(&boox_path).unwrap().clone();
    let accepted = stroke("boox-accepted-equal-revision", 0.45, 0.5);
    let accepted_pdf =
        write_boox_view(&harness.original, [base.clone(), accepted.clone()]).unwrap();
    let accepted_hash = sha256_hex(&accepted_pdf);
    let accepted_object =
        harness
            .storage
            .put_unchecked(&boox_path, accepted_pdf.clone(), generated.metadata.clone());
    let accepted_event = StorageEvent {
        schema_version: EVENT_SCHEMA_VERSION,
        event_id: "boox-accepted-equal-revision".to_owned(),
        document_id: harness.document_id.clone(),
        source: DeviceSide::Boox,
        object_path: boox_path.clone(),
        source_generation: accepted_object.generation,
        source_revision: 1,
        based_on: RevisionPair {
            boox: 0,
            supernote: 1,
        },
        content_sha256: accepted_hash.clone(),
        payload_kind: DevicePayloadKind::DeviceView,
        broker_output: Some(BrokerOutputMarker {
            producer: BROKER_PRODUCER.to_owned(),
            event_id: "sn-base-equal-boox".to_owned(),
            document_id: harness.document_id.clone(),
            source_revisions: RevisionPair {
                boox: 0,
                supernote: 1,
            },
        }),
    };
    assert!(matches!(
        harness
            .broker
            .process(&mut harness.storage, &accepted_event)
            .unwrap(),
        ProcessOutcome::Applied { .. }
    ));
    let accepted_state = harness.state();
    assert_eq!(accepted_state.boox.content_sha256, accepted_hash);
    assert_eq!(
        accepted_state.boox.source_generation,
        accepted_object.generation
    );

    let alternate = stroke("boox-rejected-equal-revision", 0.75, 0.7);
    let alternate_pdf = write_boox_view(&harness.original, [base.clone(), alternate]).unwrap();
    let alternate_hash = sha256_hex(&alternate_pdf);
    let current = harness.storage.object(&boox_path).unwrap().clone();
    let alternate_object =
        harness
            .storage
            .put_unchecked(&boox_path, alternate_pdf, current.metadata);
    let conflicting = StorageEvent {
        schema_version: EVENT_SCHEMA_VERSION,
        event_id: "boox-rejected-equal-revision".to_owned(),
        document_id: harness.document_id.clone(),
        source: DeviceSide::Boox,
        object_path: boox_path.clone(),
        source_generation: alternate_object.generation,
        source_revision: 1,
        based_on: RevisionPair {
            boox: 0,
            supernote: 1,
        },
        content_sha256: alternate_hash.clone(),
        payload_kind: DevicePayloadKind::DeviceView,
        broker_output: Some(BrokerOutputMarker {
            producer: BROKER_PRODUCER.to_owned(),
            event_id: "sn-base-equal-boox".to_owned(),
            document_id: harness.document_id.clone(),
            source_revisions: RevisionPair {
                boox: 0,
                supernote: 1,
            },
        }),
    };
    assert!(matches!(
        harness
            .broker
            .process(&mut harness.storage, &conflicting)
            .unwrap(),
        ProcessOutcome::Conflict { .. }
    ));
    assert_eq!(
        sha256_hex(&harness.storage.object(&boox_path).unwrap().bytes),
        alternate_hash
    );

    let analysis = harness
        .broker
        .inspect_conflict(
            &harness.storage,
            &harness.document_id,
            "boox-rejected-equal-revision",
        )
        .unwrap();
    let keep = resolution_request(
        &harness,
        &analysis,
        "rebuild-equal-revision-boox-view",
        ConflictResolutionStrategy::KeepCurrent,
    );
    let outcome = harness
        .broker
        .resolve_conflict(&mut harness.storage, &keep)
        .unwrap();
    let ConflictResolutionOutcome::Resolved { outputs, .. } = outcome else {
        panic!("equal-revision BOOX keep-current did not rebuild the view")
    };
    assert_eq!(outputs.len(), 1);
    assert_eq!(outputs[0].side, DeviceSide::Boox);

    let corrected = harness.storage.object(&boox_path).unwrap();
    assert_ne!(sha256_hex(&corrected.bytes), alternate_hash);
    let corrected_pdf = Document::load_mem(&corrected.bytes).unwrap();
    let page = *corrected_pdf.get_pages().get(&1).unwrap();
    let annotation_ids = corrected_pdf
        .get_page_annotations(page)
        .unwrap()
        .iter()
        .map(|annotation| {
            String::from_utf8(annotation.get(b"NM").unwrap().as_str().unwrap().to_vec()).unwrap()
        })
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        annotation_ids,
        std::collections::BTreeSet::from([
            "boox-accepted-equal-revision".to_owned(),
            "sn-base-equal-boox".to_owned(),
        ])
    );
    let state = harness.state();
    assert_eq!(state.boox.revision, 1);
    assert_eq!(state.boox.content_sha256, accepted_hash);
    assert_eq!(state.boox.source_generation, accepted_object.generation);
    assert!(state.strokes["boox-accepted-equal-revision"]
        .tombstone
        .is_none());
    assert!(!state.strokes.contains_key("boox-rejected-equal-revision"));
    assert_eq!(
        state.generated_views[&boox_path].content_sha256,
        sha256_hex(&corrected.bytes)
    );
}

#[test]
fn keep_current_deletes_rejected_ink_on_a_page_absent_from_canonical_baselines() {
    let mut harness = Harness::with_original(original_pdf_with_pages(2));
    let boox_current = stroke_on_page("boox-current-page-2", 1, 0.4, 0.5);
    let boox_pdf = write_boox_view(&harness.original, [boox_current]).unwrap();
    let boox = harness.event(
        "boox-other-page",
        DeviceSide::Boox,
        1,
        RevisionPair::default(),
        boox_pdf,
    );
    harness.broker.process(&mut harness.storage, &boox).unwrap();

    let rejected = stroke_on_page("sn-rejected-page-1", 0, 0.2, 0.3);
    let supernote = harness.event(
        "sn-conflict-new-page",
        DeviceSide::Supernote,
        1,
        RevisionPair::default(),
        supernote_export_page(0, std::slice::from_ref(&rejected)),
    );
    assert!(matches!(
        harness
            .broker
            .process(&mut harness.storage, &supernote)
            .unwrap(),
        ProcessOutcome::Conflict { .. }
    ));

    let analysis = harness
        .broker
        .inspect_conflict(
            &harness.storage,
            &harness.document_id,
            "sn-conflict-new-page",
        )
        .unwrap();
    let request = resolution_request(
        &harness,
        &analysis,
        "resolve-rejected-new-page",
        ConflictResolutionStrategy::KeepCurrent,
    );
    harness
        .broker
        .resolve_conflict(&mut harness.storage, &request)
        .unwrap();

    let manifest: Manifest = serde_json::from_slice(
        &harness
            .storage
            .object(&supernote_manifest_path(
                &harness.document_id,
                "resolve-rejected-new-page",
            ))
            .unwrap()
            .bytes,
    )
    .unwrap();
    assert_eq!(manifest.summary.deleted, 1);
    assert!(manifest.operations.iter().any(|operation| matches!(
        operation,
        Operation::DeleteStroke { source_uuid, before, .. }
            if source_uuid == "sn-rejected-page-1" && before == &rejected
    )));
}

#[test]
fn older_full_boox_conflict_is_superseded_without_rolling_back_revision() {
    let mut harness = Harness::new();
    let base = stroke("shared", 0.2, 0.3);
    let initial = harness.event(
        "sn-base-full-boox-supersession",
        DeviceSide::Supernote,
        1,
        RevisionPair::default(),
        supernote_export(std::slice::from_ref(&base)),
    );
    harness
        .broker
        .process(&mut harness.storage, &initial)
        .unwrap();

    let older_pdf = write_boox_view(
        &harness.original,
        [base.clone(), stroke("boox-older", 0.55, 0.55)],
    )
    .unwrap();
    let older = harness.event(
        "boox-older-full-conflict",
        DeviceSide::Boox,
        1,
        RevisionPair::default(),
        older_pdf,
    );
    assert!(matches!(
        harness
            .broker
            .process(&mut harness.storage, &older)
            .unwrap(),
        ProcessOutcome::Conflict { .. }
    ));

    let newer_pdf =
        write_boox_view(&harness.original, [base, stroke("boox-newer", 0.75, 0.7)]).unwrap();
    let newer = harness.event(
        "boox-newer-full-conflict",
        DeviceSide::Boox,
        2,
        RevisionPair {
            boox: 1,
            supernote: 0,
        },
        newer_pdf,
    );
    assert!(matches!(
        harness
            .broker
            .process(&mut harness.storage, &newer)
            .unwrap(),
        ProcessOutcome::Conflict { .. }
    ));

    let newer_analysis = harness
        .broker
        .inspect_conflict(
            &harness.storage,
            &harness.document_id,
            "boox-newer-full-conflict",
        )
        .unwrap();
    let newer_request = resolution_request(
        &harness,
        &newer_analysis,
        "resolve-newer-full-boox",
        ConflictResolutionStrategy::MergePreservingCurrent,
    );
    harness
        .broker
        .resolve_conflict(&mut harness.storage, &newer_request)
        .unwrap();
    let before_superseding = harness.state();
    assert_eq!(
        before_superseding.revisions(),
        RevisionPair {
            boox: 2,
            supernote: 1,
        }
    );
    assert_eq!(before_superseding.conflicts.len(), 1);

    let older_analysis = harness
        .broker
        .inspect_conflict(
            &harness.storage,
            &harness.document_id,
            "boox-older-full-conflict",
        )
        .unwrap();
    let older_request = resolution_request(
        &harness,
        &older_analysis,
        "supersede-older-full-boox",
        ConflictResolutionStrategy::AcceptIncoming,
    );
    assert!(matches!(
        harness
            .broker
            .resolve_conflict(&mut harness.storage, &older_request)
            .unwrap(),
        ConflictResolutionOutcome::Superseded {
            source_revisions: RevisionPair {
                boox: 2,
                supernote: 1
            },
            ..
        }
    ));

    let state = harness.state();
    assert_eq!(state.revisions(), before_superseding.revisions());
    assert_eq!(state.strokes, before_superseding.strokes);
    assert!(state.conflicts.is_empty());
    assert!(!state
        .processed_event_ids
        .contains("supersede-older-full-boox"));
    let record = &state.resolved_conflicts["boox-older-full-conflict"];
    assert!(record.superseded);
    assert_eq!(record.strategy, ConflictResolutionStrategy::AcceptIncoming);
    assert!(matches!(
        harness
            .broker
            .resolve_conflict(&mut harness.storage, &older_request)
            .unwrap(),
        ConflictResolutionOutcome::Duplicate {
            strategy: ConflictResolutionStrategy::AcceptIncoming,
            ..
        }
    ));
}

#[test]
fn page_scoped_supernote_conflicts_must_resolve_in_source_order() {
    let mut harness = Harness::with_original(original_pdf_with_pages(2));
    let base = stroke_on_page("shared", 0, 0.2, 0.3);
    let initial = harness.event(
        "sn-base-page-order",
        DeviceSide::Supernote,
        1,
        RevisionPair::default(),
        supernote_export_page(0, std::slice::from_ref(&base)),
    );
    harness
        .broker
        .process(&mut harness.storage, &initial)
        .unwrap();

    let common = RevisionPair {
        boox: 0,
        supernote: 1,
    };
    let boox_pdf = write_boox_view(
        &harness.original,
        [base.clone(), stroke_on_page("boox-current", 0, 0.5, 0.5)],
    )
    .unwrap();
    let boox = harness.event(
        "boox-current-page-order",
        DeviceSide::Boox,
        1,
        common,
        boox_pdf,
    );
    harness.broker.process(&mut harness.storage, &boox).unwrap();

    let page_a = stroke_on_page("sn-page-a", 0, 0.65, 0.65);
    let older = harness.event(
        "sn-page-a-conflict",
        DeviceSide::Supernote,
        2,
        common,
        supernote_export_page(0, &[base, page_a.clone()]),
    );
    assert!(matches!(
        harness
            .broker
            .process(&mut harness.storage, &older)
            .unwrap(),
        ProcessOutcome::Conflict { .. }
    ));

    let page_b = stroke_on_page("sn-page-b", 1, 0.75, 0.7);
    let newer = harness.event(
        "sn-page-b-conflict",
        DeviceSide::Supernote,
        3,
        RevisionPair {
            boox: 0,
            supernote: 2,
        },
        supernote_export_page(1, std::slice::from_ref(&page_b)),
    );
    assert!(matches!(
        harness
            .broker
            .process(&mut harness.storage, &newer)
            .unwrap(),
        ProcessOutcome::Conflict { .. }
    ));

    let newer_analysis = harness
        .broker
        .inspect_conflict(&harness.storage, &harness.document_id, "sn-page-b-conflict")
        .unwrap();
    let newer_request = resolution_request(
        &harness,
        &newer_analysis,
        "resolve-sn-page-b-too-early",
        ConflictResolutionStrategy::MergePreservingCurrent,
    );
    let state_before = harness.state();
    assert!(matches!(
        harness
            .broker
            .resolve_conflict(&mut harness.storage, &newer_request),
        Err(BrokerError::InvalidEvent(message))
            if message.contains("sn-page-a-conflict")
                && message.contains("resolve the earlier conflict first")
    ));
    assert_eq!(harness.state(), state_before);

    let older_analysis = harness
        .broker
        .inspect_conflict(&harness.storage, &harness.document_id, "sn-page-a-conflict")
        .unwrap();
    let older_request = resolution_request(
        &harness,
        &older_analysis,
        "resolve-sn-page-a",
        ConflictResolutionStrategy::MergePreservingCurrent,
    );
    harness
        .broker
        .resolve_conflict(&mut harness.storage, &older_request)
        .unwrap();
    assert_eq!(harness.state().strokes["sn-page-a"].snapshot, page_a);

    let newer_analysis = harness
        .broker
        .inspect_conflict(&harness.storage, &harness.document_id, "sn-page-b-conflict")
        .unwrap();
    let newer_request = resolution_request(
        &harness,
        &newer_analysis,
        "resolve-sn-page-b",
        ConflictResolutionStrategy::MergePreservingCurrent,
    );
    harness
        .broker
        .resolve_conflict(&mut harness.storage, &newer_request)
        .unwrap();

    let state = harness.state();
    assert_eq!(
        state.revisions(),
        RevisionPair {
            boox: 1,
            supernote: 3
        }
    );
    assert_eq!(state.strokes["sn-page-a"].snapshot, page_a);
    assert_eq!(state.strokes["sn-page-b"].snapshot, page_b);
    assert!(state.strokes.contains_key("boox-current"));
    assert!(state.conflicts.is_empty());
}

#[test]
fn delayed_supernote_successor_retries_after_predecessor_arrives() {
    let mut harness = Harness::with_original(original_pdf_with_pages(2));
    let base = stroke_on_page("sn-delayed-base", 0, 0.2, 0.3);
    let initial = harness.event(
        "sn-delayed-revision-1",
        DeviceSide::Supernote,
        1,
        RevisionPair::default(),
        supernote_export_page(0, std::slice::from_ref(&base)),
    );
    harness
        .broker
        .process(&mut harness.storage, &initial)
        .unwrap();

    let newest = stroke_on_page("sn-delayed-revision-3-stroke", 1, 0.7, 0.65);
    let revision_three = harness.event(
        "sn-delayed-revision-3",
        DeviceSide::Supernote,
        3,
        RevisionPair {
            boox: 0,
            supernote: 2,
        },
        supernote_export_page(1, std::slice::from_ref(&newest)),
    );
    assert!(matches!(
        harness
            .broker
            .process(&mut harness.storage, &revision_three),
        Err(BrokerError::InvalidEvent(message))
            if message.contains("preserved predecessor conflict")
    ));
    assert!(harness.state().conflicts.is_empty());

    let middle = stroke_on_page("sn-delayed-revision-2-stroke", 0, 0.5, 0.55);
    let revision_two = harness.event(
        "sn-delayed-revision-2",
        DeviceSide::Supernote,
        2,
        RevisionPair {
            boox: 0,
            supernote: 1,
        },
        supernote_export_page(0, &[base, middle.clone()]),
    );
    assert!(matches!(
        harness
            .broker
            .process(&mut harness.storage, &revision_two)
            .unwrap(),
        ProcessOutcome::Applied { .. }
    ));

    assert!(matches!(
        harness
            .broker
            .process(&mut harness.storage, &revision_three)
            .unwrap(),
        ProcessOutcome::Applied { .. }
    ));

    let state = harness.state();
    assert_eq!(state.supernote.revision, 3);
    assert_eq!(
        state.strokes["sn-delayed-revision-2-stroke"].snapshot,
        middle
    );
    assert_eq!(
        state.strokes["sn-delayed-revision-3-stroke"].snapshot,
        newest
    );
}

#[test]
fn delayed_compact_boox_successor_retries_after_predecessor_arrives() {
    let mut harness = Harness::new();
    let supernote_base = stroke("sn-before-delayed-compact", 0.2, 0.3);
    let initial = harness.event(
        "sn-before-delayed-compact",
        DeviceSide::Supernote,
        1,
        RevisionPair::default(),
        supernote_export(std::slice::from_ref(&supernote_base)),
    );
    harness
        .broker
        .process(&mut harness.storage, &initial)
        .unwrap();

    let newest = stroke("boox-delayed-compact-2-stroke", 0.7, 0.65);
    let mut revision_two = harness.event(
        "boox-delayed-compact-2",
        DeviceSide::Boox,
        2,
        RevisionPair {
            boox: 1,
            supernote: 1,
        },
        compact_manifest(
            vec![Operation::UpsertStroke {
                source_uuid: newest.source_uuid.clone(),
                page_index: 0,
                before: None,
                after: newest.clone(),
            }],
            1,
        ),
    );
    revision_two.payload_kind = DevicePayloadKind::BooxOperationManifest;
    assert!(matches!(
        harness
            .broker
            .process(&mut harness.storage, &revision_two),
        Err(BrokerError::InvalidEvent(message))
            if message.contains("preserved predecessor conflict")
    ));
    assert!(harness.state().conflicts.is_empty());

    let middle = stroke("boox-delayed-compact-1-stroke", 0.5, 0.55);
    let mut revision_one = harness.event(
        "boox-delayed-compact-1",
        DeviceSide::Boox,
        1,
        RevisionPair {
            boox: 0,
            supernote: 1,
        },
        compact_manifest(
            vec![Operation::UpsertStroke {
                source_uuid: middle.source_uuid.clone(),
                page_index: 0,
                before: None,
                after: middle.clone(),
            }],
            1,
        ),
    );
    revision_one.payload_kind = DevicePayloadKind::BooxOperationManifest;
    assert!(matches!(
        harness
            .broker
            .process(&mut harness.storage, &revision_one)
            .unwrap(),
        ProcessOutcome::Applied { .. }
    ));

    assert!(matches!(
        harness
            .broker
            .process(&mut harness.storage, &revision_two)
            .unwrap(),
        ProcessOutcome::Applied { .. }
    ));

    let state = harness.state();
    assert_eq!(state.boox.revision, 2);
    assert_eq!(
        state.strokes["boox-delayed-compact-1-stroke"].snapshot,
        middle
    );
    assert_eq!(
        state.strokes["boox-delayed-compact-2-stroke"].snapshot,
        newest
    );
}

#[test]
fn later_same_page_supernote_conflict_does_not_reintroduce_rejected_ink() {
    let mut harness = Harness::new();
    let base = stroke("shared-inherited-page", 0.2, 0.3);
    let initial = harness.event(
        "sn-base-inherited-page",
        DeviceSide::Supernote,
        1,
        RevisionPair::default(),
        supernote_export(std::slice::from_ref(&base)),
    );
    harness
        .broker
        .process(&mut harness.storage, &initial)
        .unwrap();

    let common = RevisionPair {
        boox: 0,
        supernote: 1,
    };
    let boox_current = stroke("boox-current-inherited-page", 0.5, 0.5);
    let boox_pdf =
        write_boox_view(&harness.original, [base.clone(), boox_current.clone()]).unwrap();
    let boox = harness.event(
        "boox-current-inherited-page",
        DeviceSide::Boox,
        1,
        common,
        boox_pdf,
    );
    harness.broker.process(&mut harness.storage, &boox).unwrap();

    let rejected = stroke("sn-rejected-inherited-page", 0.65, 0.65);
    let older = harness.event(
        "sn-rejected-inherited-conflict",
        DeviceSide::Supernote,
        2,
        common,
        supernote_export(std::slice::from_ref(&rejected)),
    );
    assert!(matches!(
        harness
            .broker
            .process(&mut harness.storage, &older)
            .unwrap(),
        ProcessOutcome::Conflict { .. }
    ));

    let later_safe = stroke("sn-later-safe-page", 0.8, 0.75);
    let newer = harness.event(
        "sn-descendant-inherited-conflict",
        DeviceSide::Supernote,
        3,
        RevisionPair {
            boox: 0,
            supernote: 2,
        },
        supernote_export(&[rejected.clone(), later_safe.clone()]),
    );
    assert!(matches!(
        harness
            .broker
            .process(&mut harness.storage, &newer)
            .unwrap(),
        ProcessOutcome::Conflict { .. }
    ));

    let older_analysis = harness
        .broker
        .inspect_conflict(
            &harness.storage,
            &harness.document_id,
            "sn-rejected-inherited-conflict",
        )
        .unwrap();
    let older_request = resolution_request(
        &harness,
        &older_analysis,
        "reject-sn-inherited-predecessor",
        ConflictResolutionStrategy::KeepCurrent,
    );
    harness
        .broker
        .resolve_conflict(&mut harness.storage, &older_request)
        .unwrap();
    assert!(!harness.state().strokes.contains_key(&rejected.source_uuid));

    let newer_analysis = harness
        .broker
        .inspect_conflict(
            &harness.storage,
            &harness.document_id,
            "sn-descendant-inherited-conflict",
        )
        .unwrap();
    assert!(newer_analysis
        .safe_changes
        .iter()
        .any(|change| change.stroke_id == later_safe.source_uuid));
    assert!(newer_analysis
        .overlapping_changes
        .iter()
        .any(|change| change.stroke_id == rejected.source_uuid));
    assert!(newer_analysis
        .overlapping_changes
        .iter()
        .any(|change| change.stroke_id == base.source_uuid));

    let newer_request = resolution_request(
        &harness,
        &newer_analysis,
        "merge-sn-inherited-descendant",
        ConflictResolutionStrategy::MergePreservingCurrent,
    );
    harness
        .broker
        .resolve_conflict(&mut harness.storage, &newer_request)
        .unwrap();

    let state = harness.state();
    assert!(state.strokes.contains_key(&later_safe.source_uuid));
    assert!(!state.strokes.contains_key(&rejected.source_uuid));
    assert!(state.strokes.contains_key(&base.source_uuid));
    assert!(state.strokes.contains_key(&boox_current.source_uuid));
}

#[test]
fn accept_incoming_replaces_overlaps_and_deletes_current_only_ink() {
    let mut harness = harness_with_mixed_supernote_conflict();
    let analysis = harness
        .broker
        .inspect_conflict(&harness.storage, &harness.document_id, "sn-concurrent")
        .unwrap();
    let request = resolution_request(
        &harness,
        &analysis,
        "resolve-accept-incoming",
        ConflictResolutionStrategy::AcceptIncoming,
    );
    harness
        .broker
        .resolve_conflict(&mut harness.storage, &request)
        .unwrap();

    let state = harness.state();
    assert_eq!(
        state.revisions(),
        RevisionPair {
            boox: 1,
            supernote: 2
        }
    );
    assert_eq!(
        state.strokes["shared"].snapshot.samples,
        stroke("shared", 0.2, 0.45).samples
    );
    assert!(state.strokes["boox-current"].tombstone.is_some());
    assert!(state.strokes["sn-new"].tombstone.is_none());
}

#[test]
fn stale_conflict_analysis_or_destination_cannot_commit_a_resolution() {
    let mut stale_analysis = harness_with_mixed_supernote_conflict();
    let analysis = stale_analysis
        .broker
        .inspect_conflict(
            &stale_analysis.storage,
            &stale_analysis.document_id,
            "sn-concurrent",
        )
        .unwrap();
    let mut request = resolution_request(
        &stale_analysis,
        &analysis,
        "resolve-stale-analysis",
        ConflictResolutionStrategy::MergePreservingCurrent,
    );
    request.expected_state_revision += 1;
    assert!(matches!(
        stale_analysis
            .broker
            .resolve_conflict(&mut stale_analysis.storage, &request),
        Err(BrokerError::InvalidEvent(message)) if message.contains("analysis is stale")
    ));
    assert!(stale_analysis
        .storage
        .object(&conflict_resolution_path(
            &stale_analysis.document_id,
            "sn-concurrent"
        ))
        .is_none());

    let mut stale_destination = harness_with_mixed_supernote_conflict();
    let analysis = stale_destination
        .broker
        .inspect_conflict(
            &stale_destination.storage,
            &stale_destination.document_id,
            "sn-concurrent",
        )
        .unwrap();
    let request = resolution_request(
        &stale_destination,
        &analysis,
        "resolve-stale-destination",
        ConflictResolutionStrategy::MergePreservingCurrent,
    );
    let path = boox_view_path(&stale_destination.state());
    stale_destination
        .storage
        .put_unchecked(&path, b"newer destination".to_vec(), BTreeMap::new());
    let state_before = stale_destination.state();
    assert!(matches!(
        stale_destination
            .broker
            .resolve_conflict(&mut stale_destination.storage, &request),
        Err(BrokerError::StaleDestination { .. })
    ));
    assert_eq!(stale_destination.state(), state_before);
    assert!(stale_destination
        .storage
        .object(&conflict_resolution_path(
            &stale_destination.document_id,
            "sn-concurrent"
        ))
        .is_none());
}

#[test]
fn compact_conflict_recovers_legacy_page_count_before_validating_pages() {
    let mut harness = Harness::new();
    let initial = harness.event(
        "sn-base-legacy-conflict",
        DeviceSide::Supernote,
        1,
        RevisionPair::default(),
        supernote_export(&[stroke("sn-current", 0.2, 0.3)]),
    );
    harness
        .broker
        .process(&mut harness.storage, &initial)
        .unwrap();
    let mut legacy = harness.state();
    legacy.original_page_count = 0;
    harness.storage.put_unchecked(
        state_path(&harness.document_id),
        serde_json::to_vec(&legacy).unwrap(),
        BTreeMap::new(),
    );

    let mut invalid_page = stroke("boox-impossible-page", 0.7, 0.7);
    invalid_page.page_index = 1;
    let mut event = harness.event(
        "boox-legacy-invalid-page-conflict",
        DeviceSide::Boox,
        1,
        RevisionPair::default(),
        compact_manifest(
            vec![Operation::UpsertStroke {
                source_uuid: invalid_page.source_uuid.clone(),
                page_index: invalid_page.page_index,
                before: None,
                after: invalid_page,
            }],
            2,
        ),
    );
    event.payload_kind = DevicePayloadKind::BooxOperationManifest;
    assert!(matches!(
        harness
            .broker
            .process(&mut harness.storage, &event)
            .unwrap(),
        ProcessOutcome::Conflict { .. }
    ));

    assert!(matches!(
        harness.broker.inspect_conflict(
            &harness.storage,
            &harness.document_id,
            "boox-legacy-invalid-page-conflict",
        ),
        Err(BrokerError::InvalidEvent(message))
            if message.contains("page count 2 does not match original page count 1")
    ));
}

#[test]
fn supernote_conflict_recovers_legacy_page_count_before_validating_pages() {
    let mut harness = Harness::new();
    let boox = stroke("boox-current-before-legacy-supernote-conflict", 0.2, 0.3);
    let initial_pdf = write_boox_view(&harness.original, [boox]).unwrap();
    let initial = harness.event(
        "boox-base-legacy-supernote-conflict",
        DeviceSide::Boox,
        1,
        RevisionPair::default(),
        initial_pdf,
    );
    harness
        .broker
        .process(&mut harness.storage, &initial)
        .unwrap();

    let mut legacy = harness.state();
    legacy.original_page_count = 0;
    harness.storage.put_unchecked(
        state_path(&harness.document_id),
        serde_json::to_vec(&legacy).unwrap(),
        BTreeMap::new(),
    );

    let incoming = stroke("supernote-after-legacy-migration", 0.7, 0.7);
    let event = harness.event(
        "supernote-legacy-page-count-conflict",
        DeviceSide::Supernote,
        1,
        RevisionPair::default(),
        supernote_export_pages(&[(0, std::slice::from_ref(&incoming))]),
    );
    assert!(matches!(
        harness
            .broker
            .process(&mut harness.storage, &event)
            .unwrap(),
        ProcessOutcome::Conflict { .. }
    ));

    let analysis = harness
        .broker
        .inspect_conflict(
            &harness.storage,
            &harness.document_id,
            "supernote-legacy-page-count-conflict",
        )
        .unwrap();
    assert!(analysis
        .safe_changes
        .iter()
        .any(|change| change.stroke_id == incoming.source_uuid));
    let request = resolution_request(
        &harness,
        &analysis,
        "resolve-legacy-supernote-page-count",
        ConflictResolutionStrategy::MergePreservingCurrent,
    );
    harness
        .broker
        .resolve_conflict(&mut harness.storage, &request)
        .unwrap();

    let state = harness.state();
    assert_eq!(state.original_page_count, 1);
    assert!(state.strokes.contains_key(&incoming.source_uuid));
}

#[test]
fn resolving_valid_compact_conflict_persists_recovered_legacy_page_count() {
    let mut harness = Harness::new();
    let initial = harness.event(
        "sn-base-legacy-valid-conflict",
        DeviceSide::Supernote,
        1,
        RevisionPair::default(),
        supernote_export(&[stroke("sn-current", 0.2, 0.3)]),
    );
    harness
        .broker
        .process(&mut harness.storage, &initial)
        .unwrap();
    let mut legacy = harness.state();
    legacy.original_page_count = 0;
    harness.storage.put_unchecked(
        state_path(&harness.document_id),
        serde_json::to_vec(&legacy).unwrap(),
        BTreeMap::new(),
    );

    let snapshot = stroke("boox-valid-page", 0.7, 0.7);
    let mut event = harness.event(
        "boox-legacy-valid-page-conflict",
        DeviceSide::Boox,
        1,
        RevisionPair::default(),
        compact_manifest(
            vec![Operation::UpsertStroke {
                source_uuid: snapshot.source_uuid.clone(),
                page_index: snapshot.page_index,
                before: None,
                after: snapshot,
            }],
            1,
        ),
    );
    event.payload_kind = DevicePayloadKind::BooxOperationManifest;
    harness
        .broker
        .process(&mut harness.storage, &event)
        .unwrap();
    let analysis = harness
        .broker
        .inspect_conflict(
            &harness.storage,
            &harness.document_id,
            "boox-legacy-valid-page-conflict",
        )
        .unwrap();
    let request = resolution_request(
        &harness,
        &analysis,
        "resolve-legacy-valid-page",
        ConflictResolutionStrategy::MergePreservingCurrent,
    );
    harness
        .broker
        .resolve_conflict(&mut harness.storage, &request)
        .unwrap();

    let state = harness.state();
    assert_eq!(state.original_page_count, 1);
    assert_eq!(
        state.revisions(),
        RevisionPair {
            boox: 1,
            supernote: 1,
        }
    );
}

#[test]
fn legacy_compact_conflict_without_payload_kind_is_inferred_from_json_evidence() {
    let mut harness = Harness::new();
    let base = stroke("sn-before-legacy-payload-kind", 0.2, 0.3);
    let initial = harness.event(
        "sn-before-legacy-payload-kind",
        DeviceSide::Supernote,
        1,
        RevisionPair::default(),
        supernote_export(std::slice::from_ref(&base)),
    );
    harness
        .broker
        .process(&mut harness.storage, &initial)
        .unwrap();

    let incoming = stroke("boox-legacy-payload-kind", 0.7, 0.7);
    let compact_bytes = compact_manifest(
        vec![Operation::UpsertStroke {
            source_uuid: incoming.source_uuid.clone(),
            page_index: incoming.page_index,
            before: None,
            after: incoming.clone(),
        }],
        1,
    );
    let mut event = harness.event(
        "boox-legacy-payload-kind-conflict",
        DeviceSide::Boox,
        1,
        RevisionPair::default(),
        compact_bytes.clone(),
    );
    let json_path = format!(
        "BOOX_Folder/{}/incoming-r1.operations.json",
        harness.document_id
    );
    let json_object = harness
        .storage
        .put_unchecked(&json_path, compact_bytes, BTreeMap::new());
    event.object_path = json_path;
    event.source_generation = json_object.generation;
    event.payload_kind = DevicePayloadKind::BooxOperationManifest;
    assert!(matches!(
        harness
            .broker
            .process(&mut harness.storage, &event)
            .unwrap(),
        ProcessOutcome::Conflict { .. }
    ));

    let mut legacy = serde_json::to_value(harness.state()).unwrap();
    legacy["conflicts"][0]
        .as_object_mut()
        .unwrap()
        .remove("payloadKind");
    harness.storage.put_unchecked(
        state_path(&harness.document_id),
        serde_json::to_vec(&legacy).unwrap(),
        BTreeMap::new(),
    );

    let conflicts = harness
        .broker
        .list_conflicts(&harness.storage, &harness.document_id)
        .unwrap();
    assert_eq!(
        conflicts[0].payload_kind,
        DevicePayloadKind::BooxOperationManifest
    );
    let analysis = harness
        .broker
        .inspect_conflict(
            &harness.storage,
            &harness.document_id,
            "boox-legacy-payload-kind-conflict",
        )
        .unwrap();
    let request = resolution_request(
        &harness,
        &analysis,
        "resolve-legacy-payload-kind",
        ConflictResolutionStrategy::MergePreservingCurrent,
    );
    harness
        .broker
        .resolve_conflict(&mut harness.storage, &request)
        .unwrap();

    let state = harness.state();
    assert_eq!(state.boox.revision, 1);
    assert!(state.strokes["boox-legacy-payload-kind"]
        .tombstone
        .is_none());
    assert!(state.conflicts.is_empty());
}

#[test]
fn incremental_boox_conflicts_must_resolve_in_source_order() {
    let mut harness = Harness::new();
    let initial = harness.event(
        "sn-base-incremental-order",
        DeviceSide::Supernote,
        1,
        RevisionPair::default(),
        supernote_export(&[stroke("sn-current", 0.2, 0.3)]),
    );
    harness
        .broker
        .process(&mut harness.storage, &initial)
        .unwrap();

    let first_bytes = compact_manifest(
        vec![Operation::UpsertStroke {
            source_uuid: "boox-a".to_owned(),
            page_index: 0,
            before: None,
            after: stroke("boox-a", 0.55, 0.55),
        }],
        1,
    );
    let mut first = harness.event(
        "boox-compact-conflict-1",
        DeviceSide::Boox,
        1,
        RevisionPair::default(),
        first_bytes,
    );
    first.payload_kind = DevicePayloadKind::BooxOperationManifest;
    assert!(matches!(
        harness
            .broker
            .process(&mut harness.storage, &first)
            .unwrap(),
        ProcessOutcome::Conflict { .. }
    ));

    let second_bytes = compact_manifest(
        vec![Operation::UpsertStroke {
            source_uuid: "boox-b".to_owned(),
            page_index: 0,
            before: None,
            after: stroke("boox-b", 0.75, 0.7),
        }],
        1,
    );
    let mut second = harness.event(
        "boox-compact-conflict-2",
        DeviceSide::Boox,
        2,
        RevisionPair {
            boox: 1,
            supernote: 0,
        },
        second_bytes,
    );
    second.payload_kind = DevicePayloadKind::BooxOperationManifest;
    assert!(matches!(
        harness
            .broker
            .process(&mut harness.storage, &second)
            .unwrap(),
        ProcessOutcome::Conflict { .. }
    ));

    let newer_analysis = harness
        .broker
        .inspect_conflict(
            &harness.storage,
            &harness.document_id,
            "boox-compact-conflict-2",
        )
        .unwrap();
    let newer_request = resolution_request(
        &harness,
        &newer_analysis,
        "resolve-compact-2-too-early",
        ConflictResolutionStrategy::MergePreservingCurrent,
    );
    let state_before = harness.state();
    assert!(matches!(
        harness
            .broker
            .resolve_conflict(&mut harness.storage, &newer_request),
        Err(BrokerError::InvalidEvent(message))
            if message.contains("boox-compact-conflict-1")
                && message.contains("resolve the earlier conflict first")
    ));
    assert_eq!(harness.state(), state_before);

    let older_analysis = harness
        .broker
        .inspect_conflict(
            &harness.storage,
            &harness.document_id,
            "boox-compact-conflict-1",
        )
        .unwrap();
    let older_request = resolution_request(
        &harness,
        &older_analysis,
        "resolve-compact-1",
        ConflictResolutionStrategy::MergePreservingCurrent,
    );
    harness
        .broker
        .resolve_conflict(&mut harness.storage, &older_request)
        .unwrap();
    assert!(harness.state().strokes.contains_key("boox-a"));

    let newer_analysis = harness
        .broker
        .inspect_conflict(
            &harness.storage,
            &harness.document_id,
            "boox-compact-conflict-2",
        )
        .unwrap();
    let newer_request = resolution_request(
        &harness,
        &newer_analysis,
        "resolve-compact-2",
        ConflictResolutionStrategy::MergePreservingCurrent,
    );
    harness
        .broker
        .resolve_conflict(&mut harness.storage, &newer_request)
        .unwrap();

    let state = harness.state();
    assert_eq!(
        state.revisions(),
        RevisionPair {
            boox: 2,
            supernote: 1
        }
    );
    assert!(state.strokes.contains_key("boox-a"));
    assert!(state.strokes.contains_key("boox-b"));
    assert!(state.conflicts.is_empty());
}

#[test]
fn dependent_compact_conflict_after_rejected_ancestor_is_an_explicit_overlap() {
    for (case, strategy, accepts_descendant) in [
        (
            "merge-current",
            ConflictResolutionStrategy::MergePreservingCurrent,
            false,
        ),
        (
            "accept-descendant",
            ConflictResolutionStrategy::AcceptIncoming,
            true,
        ),
    ] {
        let (mut harness, base, descendant) = harness_with_rejected_compact_ancestor();
        let analysis = harness
            .broker
            .inspect_conflict(
                &harness.storage,
                &harness.document_id,
                "boox-dependent-conflict-2",
            )
            .unwrap();
        assert!(analysis.safe_changes.is_empty());
        assert_eq!(
            analysis
                .overlapping_changes
                .iter()
                .map(|change| change.stroke_id.as_str())
                .collect::<Vec<_>>(),
            vec!["shared"]
        );

        let request = resolution_request(
            &harness,
            &analysis,
            &format!("resolve-dependent-{case}"),
            strategy,
        );
        harness
            .broker
            .resolve_conflict(&mut harness.storage, &request)
            .unwrap();

        let state = harness.state();
        assert_eq!(
            state.revisions(),
            RevisionPair {
                boox: 2,
                supernote: 1,
            }
        );
        let expected = if accepts_descendant {
            &descendant
        } else {
            &base
        };
        assert_eq!(&state.strokes["shared"].snapshot, expected);
        assert!(state.conflicts.is_empty());
    }
}

#[test]
fn compact_boox_conflict_merges_safe_operations_and_preserves_current_supernote_edits() {
    let mut harness = Harness::new();
    let base = stroke("shared", 0.2, 0.3);
    let initial = harness.event(
        "sn-base-compact-conflict",
        DeviceSide::Supernote,
        1,
        RevisionPair::default(),
        supernote_export(std::slice::from_ref(&base)),
    );
    harness
        .broker
        .process(&mut harness.storage, &initial)
        .unwrap();
    let common = RevisionPair {
        boox: 0,
        supernote: 1,
    };
    let supernote_shared = stroke("shared", 0.2, 0.45);
    let supernote_new = stroke("sn-current", 0.55, 0.55);
    let current = harness.event(
        "sn-current-compact-conflict",
        DeviceSide::Supernote,
        2,
        common,
        supernote_export(&[supernote_shared.clone(), supernote_new.clone()]),
    );
    harness
        .broker
        .process(&mut harness.storage, &current)
        .unwrap();

    let boox_shared = stroke("shared", 0.4, 0.3);
    let boox_new = stroke("boox-new", 0.75, 0.7);
    let bytes = compact_manifest(
        vec![
            Operation::UpsertStroke {
                source_uuid: "shared".to_owned(),
                page_index: 0,
                before: Some(base),
                after: boox_shared,
            },
            Operation::UpsertStroke {
                source_uuid: "boox-new".to_owned(),
                page_index: 0,
                before: None,
                after: boox_new,
            },
        ],
        1,
    );
    let mut incoming = harness.event(
        "boox-compact-concurrent",
        DeviceSide::Boox,
        1,
        common,
        bytes,
    );
    incoming.payload_kind = DevicePayloadKind::BooxOperationManifest;
    assert!(matches!(
        harness
            .broker
            .process(&mut harness.storage, &incoming)
            .unwrap(),
        ProcessOutcome::Conflict { .. }
    ));

    let analysis = harness
        .broker
        .inspect_conflict(
            &harness.storage,
            &harness.document_id,
            "boox-compact-concurrent",
        )
        .unwrap();
    assert_eq!(
        analysis
            .safe_changes
            .iter()
            .map(|change| (change.stroke_id.as_str(), change.kind))
            .collect::<Vec<_>>(),
        vec![("boox-new", ConflictChangeKind::Add)]
    );
    assert_eq!(
        analysis
            .overlapping_changes
            .iter()
            .map(|change| (change.stroke_id.as_str(), change.kind))
            .collect::<Vec<_>>(),
        vec![("shared", ConflictChangeKind::Move)]
    );
    let request = resolution_request(
        &harness,
        &analysis,
        "resolve-compact-boox",
        ConflictResolutionStrategy::MergePreservingCurrent,
    );
    harness
        .broker
        .resolve_conflict(&mut harness.storage, &request)
        .unwrap();

    let state = harness.state();
    assert_eq!(
        state.revisions(),
        RevisionPair {
            boox: 1,
            supernote: 2
        }
    );
    assert_eq!(state.strokes["shared"].snapshot, supernote_shared);
    assert_eq!(state.strokes["sn-current"].snapshot, supernote_new);
    assert!(state.strokes.contains_key("boox-new"));
    assert!(state.conflicts.is_empty());
}

fn fixture_file(directory: &PathBuf, prefix: &str, suffix: &str) -> Vec<u8> {
    let mut matches = std::fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(prefix) && name.ends_with(suffix))
        })
        .collect::<Vec<_>>();
    matches.sort();
    assert_eq!(matches.len(), 1, "fixture match for {prefix}*{suffix}");
    std::fs::read(&matches[0]).unwrap()
}

fn process_fixture_event(
    harness: &mut Harness,
    event_id: &str,
    side: DeviceSide,
    revision: u64,
    based_on: RevisionPair,
    kind: DevicePayloadKind,
    bytes: Vec<u8>,
) -> ProcessOutcome {
    let mut event = harness.event(event_id, side, revision, based_on, bytes);
    event.payload_kind = kind;
    harness
        .broker
        .process(&mut harness.storage, &event)
        .unwrap()
}

#[test]
#[ignore = "requires the private real-device E2E conflict fixture directory"]
fn real_device_simultaneous_edit_evidence_resolves_without_losing_either_side() {
    let root = std::env::var_os("INKBRIDGE_CONFLICT_FIXTURE_ROOT")
        .map(PathBuf::from)
        .expect("set INKBRIDGE_CONFLICT_FIXTURE_ROOT to inkbridge-runs/e2e-925715a-20260823");
    let original_directory = root.join("original");
    let original = fixture_file(&original_directory, "InkBridge-E2E-", ".pdf");
    let mut harness = Harness::with_original(original);
    let document_root = root.join("Supernote_Folder").join(&harness.document_id);
    let accepted = document_root.join(".inkbridge-accepted");
    let incoming = document_root.join("incoming");

    assert!(matches!(
        process_fixture_event(
            &mut harness,
            "real-sn-1",
            DeviceSide::Supernote,
            1,
            RevisionPair::default(),
            DevicePayloadKind::DeviceView,
            fixture_file(&accepted, "r00000000000000000001-", ".json"),
        ),
        ProcessOutcome::Applied { .. }
    ));
    assert!(matches!(
        process_fixture_event(
            &mut harness,
            "real-boox-1",
            DeviceSide::Boox,
            1,
            RevisionPair {
                boox: 0,
                supernote: 1
            },
            DevicePayloadKind::BooxOperationManifest,
            fixture_file(&incoming, "r00000000000000000001-r", ".operations.json"),
        ),
        ProcessOutcome::Applied { .. }
    ));
    assert!(matches!(
        process_fixture_event(
            &mut harness,
            "real-sn-2",
            DeviceSide::Supernote,
            2,
            RevisionPair {
                boox: 1,
                supernote: 1
            },
            DevicePayloadKind::DeviceView,
            fixture_file(&accepted, "r00000000000000000002-", ".json"),
        ),
        ProcessOutcome::Applied { .. }
    ));
    assert!(matches!(
        process_fixture_event(
            &mut harness,
            "real-boox-2",
            DeviceSide::Boox,
            2,
            RevisionPair {
                boox: 1,
                supernote: 2
            },
            DevicePayloadKind::BooxOperationManifest,
            fixture_file(&incoming, "r00000000000000000002-r", ".operations.json"),
        ),
        ProcessOutcome::Applied { .. }
    ));
    for (revision, based_on) in [
        (
            3,
            RevisionPair {
                boox: 2,
                supernote: 2,
            },
        ),
        (
            4,
            RevisionPair {
                boox: 2,
                supernote: 3,
            },
        ),
        (
            5,
            RevisionPair {
                boox: 2,
                supernote: 4,
            },
        ),
    ] {
        assert!(matches!(
            process_fixture_event(
                &mut harness,
                &format!("real-sn-{revision}"),
                DeviceSide::Supernote,
                revision,
                based_on,
                DevicePayloadKind::DeviceView,
                fixture_file(&accepted, &format!("r{revision:020}-"), ".json"),
            ),
            ProcessOutcome::Applied { .. }
        ));
    }

    let conflict = process_fixture_event(
        &mut harness,
        "real-boox-concurrent",
        DeviceSide::Boox,
        3,
        RevisionPair {
            boox: 2,
            supernote: 4,
        },
        DevicePayloadKind::DeviceView,
        std::fs::read(root.join("conflict-evidence/incoming.pdf")).unwrap(),
    );
    assert!(matches!(conflict, ProcessOutcome::Conflict { .. }));
    let analysis = harness
        .broker
        .inspect_conflict(
            &harness.storage,
            &harness.document_id,
            "real-boox-concurrent",
        )
        .unwrap();
    assert!(analysis
        .safe_changes
        .iter()
        .any(|change| change.kind == ConflictChangeKind::Add));
    assert!(analysis
        .overlapping_changes
        .iter()
        .any(|change| change.kind == ConflictChangeKind::Delete));
    let request = resolution_request(
        &harness,
        &analysis,
        "resolve-real-device-conflict",
        ConflictResolutionStrategy::MergePreservingCurrent,
    );
    harness
        .broker
        .resolve_conflict(&mut harness.storage, &request)
        .unwrap();
    let state = harness.state();
    assert_eq!(
        state.revisions(),
        RevisionPair {
            boox: 3,
            supernote: 5
        }
    );
    assert!(state.conflicts.is_empty());
    assert!(state
        .strokes
        .values()
        .any(|stroke| stroke.last_modified_by == DeviceSide::Boox && stroke.tombstone.is_none()));
    assert!(state.strokes.values().any(
        |stroke| stroke.last_modified_by == DeviceSide::Supernote && stroke.tombstone.is_none()
    ));
}

use inkbridge_broker::*;
use inkbridge_convert::{
    build_document_baseline, geometry_fingerprint,
    verify_virtual_spread_page_143_production_fixture, AffinePoint, Manifest, NativeStyle,
    Operation, StrokeSnapshot,
};
use serde_json::json;
use std::collections::BTreeMap;

const SOURCE: &[u8] = include_bytes!(
    "../../inkbridge-convert/tests/fixtures/virtual-spread/page-143-v1/page-143-source-v1.pdf"
);
const GENERATED: &[u8] = include_bytes!(
    "../../inkbridge-convert/tests/fixtures/virtual-spread/page-143-v1/page-143-virtual-spread-v1.pdf"
);
const SIDECAR: &[u8] = include_bytes!(
    "../../inkbridge-convert/tests/fixtures/virtual-spread/page-143-v1/page-143-virtual-spread-v1.pdf.json"
);
const DESCRIPTOR: &[u8] = include_bytes!(
    "../../inkbridge-convert/tests/fixtures/virtual-spread/page-143-v1/page-143-artifacts-v1.json"
);
const PDF_TAIL: &[u8] = include_bytes!(
    "../../inkbridge-convert/tests/fixtures/virtual-spread/page-143-v1/page-143-pdf-tail-authorities-v1.txt"
);

struct Harness {
    broker: Broker,
    storage: MemoryStorage,
    document_id: String,
}

impl Harness {
    fn new() -> Self {
        let mut storage = MemoryStorage::default();
        let broker = Broker::default();
        let state = broker
            .register_document(&mut storage, "page-143-source-v1.pdf", SOURCE)
            .unwrap();
        Self {
            broker,
            storage,
            document_id: state.document_id,
        }
    }

    fn event(
        &mut self,
        event_id: &str,
        source: DeviceSide,
        source_revision: u64,
        based_on: RevisionPair,
        bytes: Vec<u8>,
    ) -> StorageEvent {
        let extension = if source == DeviceSide::Boox {
            "pdf"
        } else {
            "json"
        };
        let object_path = format!(
            "{}_Folder/{}/page-143-{source_revision}.{extension}",
            if source == DeviceSide::Boox {
                "BOOX"
            } else {
                "Supernote"
            },
            self.document_id
        );
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
            source_revision,
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
fn real_page_143_create_move_delete_and_duplicate_delivery_are_one_stable_stroke() {
    let verified = verify_virtual_spread_page_143_production_fixture(
        SOURCE, GENERATED, SIDECAR, DESCRIPTOR, PDF_TAIL,
    )
    .unwrap();
    let mapping = verified.manifest().mapping_for_source_page(2).unwrap();
    let canonical_points = [[0.1, 0.2], [0.5, 0.5], [0.9, 0.8]];
    let canonical_samples = canonical_points
        .into_iter()
        .enumerate()
        .map(|(index, point)| {
            let spread = mapping
                .canonical_to_spread(AffinePoint::new(point[0], point[1]))
                .unwrap();
            let recovered = mapping.spread_to_canonical(spread).unwrap();
            [recovered.x, recovered.y, 900.0 + index as f64 * 100.0]
        })
        .collect::<Vec<_>>();
    let created = stroke("page-143-stroke-a", canonical_samples);
    let mut harness = Harness::new();
    assert_eq!(harness.document_id, verified.manifest().document_id);

    let create_bytes = supernote_export(
        &harness.document_id,
        RevisionPair::default(),
        std::slice::from_ref(&created),
    );
    let create = harness.event(
        "page-143-create",
        DeviceSide::Supernote,
        1,
        RevisionPair::default(),
        create_bytes,
    );
    assert!(matches!(
        harness
            .broker
            .process(&mut harness.storage, &create)
            .unwrap(),
        ProcessOutcome::Applied { .. }
    ));
    let created_state = harness.state();
    assert_eq!(created_state.strokes.len(), 1);
    assert!(created_state.strokes["page-143-stroke-a"]
        .tombstone
        .is_none());
    assert_eq!(created_state.strokes["page-143-stroke-a"].snapshot, created);
    let boox_view = harness
        .storage
        .object(&boox_view_path(&created_state))
        .unwrap()
        .bytes
        .clone();
    assert_pdf_contains_strokes(&boox_view, 1);

    let mut moved = created.clone();
    for sample in &mut moved.samples {
        sample[0] += 0.03;
        sample[1] += 0.02;
    }
    moved.geometry_fingerprint = geometry_fingerprint(&moved.native_style, &moved.samples);
    let returned = write_boox_view(SOURCE, [moved.clone()]).unwrap();
    let boox_move = harness.event(
        "page-143-move",
        DeviceSide::Boox,
        1,
        RevisionPair {
            boox: 0,
            supernote: 1,
        },
        returned,
    );
    harness
        .broker
        .process(&mut harness.storage, &boox_move)
        .unwrap();
    let moved_state = harness.state();
    assert_same_visible_stroke(&moved_state.strokes["page-143-stroke-a"].snapshot, &moved);
    assert!(moved_state.strokes["page-143-stroke-a"].tombstone.is_none());
    let manifest: Manifest = serde_json::from_slice(
        &harness
            .storage
            .object(&supernote_manifest_path(
                &harness.document_id,
                "page-143-move",
            ))
            .unwrap()
            .bytes,
    )
    .unwrap();
    assert_eq!(manifest.summary.upserted, 1);
    assert_eq!(manifest.summary.deleted, 0);
    let [Operation::UpsertStroke {
        source_uuid,
        page_index: 2,
        before: Some(_),
        after,
    }] = manifest.operations.as_slice()
    else {
        panic!("BOOX move must produce one stable-ID page-143 upsert")
    };
    assert_eq!(source_uuid, "page-143-stroke-a");
    assert_same_visible_stroke(after, &moved);

    let deletion_bytes = supernote_export(
        &harness.document_id,
        RevisionPair {
            boox: 1,
            supernote: 1,
        },
        &[],
    );
    let deletion = harness.event(
        "page-143-delete",
        DeviceSide::Supernote,
        2,
        RevisionPair {
            boox: 1,
            supernote: 1,
        },
        deletion_bytes,
    );
    harness
        .broker
        .process(&mut harness.storage, &deletion)
        .unwrap();
    let deleted_state = harness.state();
    let tombstone = deleted_state.strokes["page-143-stroke-a"]
        .tombstone
        .as_ref()
        .unwrap();
    assert_eq!(tombstone.deleted_by, DeviceSide::Supernote);
    assert_eq!(tombstone.deleted_at_revision, 2);
    assert_pdf_contains_strokes(
        &harness
            .storage
            .object(&boox_view_path(&deleted_state))
            .unwrap()
            .bytes,
        0,
    );

    assert!(matches!(
        harness
            .broker
            .process(&mut harness.storage, &deletion)
            .unwrap(),
        ProcessOutcome::Duplicate { .. }
    ));
    let duplicate_state = harness.state();
    assert_eq!(duplicate_state.strokes.len(), 1);
    assert_eq!(duplicate_state.state_revision, deleted_state.state_revision);
    assert!(duplicate_state.strokes["page-143-stroke-a"]
        .tombstone
        .is_some());
}

fn stroke(source_uuid: &str, samples: Vec<[f64; 3]>) -> StrokeSnapshot {
    let native_style = NativeStyle::default();
    StrokeSnapshot {
        source_uuid: source_uuid.to_owned(),
        origin: "supernote-native".to_owned(),
        page_index: 2,
        geometry_fingerprint: geometry_fingerprint(&native_style, &samples),
        native_style,
        samples,
    }
}

fn supernote_export(
    document_id: &str,
    based_on: RevisionPair,
    page_143_strokes: &[StrokeSnapshot],
) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "schemaVersion": 1,
        "sourceFileName": "page-143-source-v1.pdf",
        "documentId": document_id,
        "basedOn": { "boox": based_on.boox, "supernote": based_on.supernote },
        "pages": [
            { "pageIndex": 1, "strokes": [] },
            {
                "pageIndex": 2,
                "strokes": page_143_strokes.iter().map(|stroke| json!({
                    "sourceUuid": stroke.source_uuid,
                    "sourceKey": stroke.source_uuid,
                    "layerNum": stroke.native_style.layer_num,
                    "thickness": stroke.native_style.thickness,
                    "penColor": stroke.native_style.pen_color,
                    "penType": stroke.native_style.pen_type,
                    "samples": stroke.samples,
                })).collect::<Vec<_>>()
            }
        ]
    }))
    .unwrap()
}

fn assert_pdf_contains_strokes(bytes: &[u8], expected: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("boox-view.pdf");
    std::fs::write(&path, bytes).unwrap();
    let baseline = build_document_baseline(&path, "page-143-source-v1.pdf").unwrap();
    assert_eq!(baseline.strokes.len(), expected);
}

fn assert_same_visible_stroke(actual: &StrokeSnapshot, expected: &StrokeSnapshot) {
    assert_eq!(actual.source_uuid, expected.source_uuid);
    assert_eq!(actual.page_index, expected.page_index);
    assert_eq!(actual.native_style, expected.native_style);
    assert_eq!(actual.geometry_fingerprint, expected.geometry_fingerprint);
    assert_eq!(actual.samples.len(), expected.samples.len());
    for (actual, expected) in actual.samples.iter().zip(&expected.samples) {
        assert!((actual[0] - expected[0]).abs() <= 1.0e-7);
        assert!((actual[1] - expected[1]).abs() <= 1.0e-7);
        assert_eq!(actual[2], expected[2]);
    }
}

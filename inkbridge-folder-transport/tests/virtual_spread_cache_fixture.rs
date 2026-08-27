use inkbridge_convert::verify_virtual_spread_golden_fixture;
use inkbridge_folder_transport::{
    CacheRegenerationPhase, CacheRegenerationTransaction, CandidateVerificationEvidence,
    DirtyMarkExportEvidence, HydrationEvidence, RollbackEvidence, VirtualSpreadViewEvidence,
};

const FIXTURE: &[u8] = include_bytes!(
    "../../inkbridge-convert/tests/fixtures/virtual-spread/page-143-contract-v1.json"
);

fn view(
    document_id: &str,
    view_id: String,
    mapping_authority_sha256: String,
    pdf_fill: char,
    manifest_fill: char,
) -> VirtualSpreadViewEvidence {
    VirtualSpreadViewEvidence {
        cache_basename: format!("{document_id}.{view_id}.virtual-spread.pdf"),
        document_id: document_id.to_owned(),
        view_id,
        generated_pdf_sha256: pdf_fill.to_string().repeat(64),
        manifest_sha256: manifest_fill.to_string().repeat(64),
        mapping_authority_sha256,
    }
}

#[test]
fn page_143_dirty_cache_regeneration_requires_atomic_two_page_hydration() {
    let golden = verify_virtual_spread_golden_fixture(FIXTURE).unwrap();
    let selected_virtual_page =
        golden.mappings[golden.page_143_mapping_index as usize].virtual_page_index;
    let represented_pages = golden
        .mappings
        .iter()
        .filter(|mapping| mapping.virtual_page_index == selected_virtual_page)
        .map(|mapping| mapping.source_page_index)
        .collect::<Vec<_>>();
    assert_eq!(represented_pages, vec![1, 2]);

    let previous = view(
        &golden.document_id,
        format!("inkbridge-view-v1-{}", "c".repeat(64)),
        "d".repeat(64),
        'e',
        'f',
    );
    let candidate = view(
        &golden.document_id,
        golden.view_id.clone(),
        golden.mapping_authority_sha256.clone(),
        'a',
        'b',
    );
    let mut transaction = CacheRegenerationTransaction::begin_dirty(
        "page-143-regeneration".to_owned(),
        golden.original_pdf_sha256,
        represented_pages.clone(),
        previous.clone(),
        candidate.clone(),
    )
    .unwrap();

    assert!(transaction.record_generated(&candidate).is_err());
    transaction
        .record_dirty_export(DirtyMarkExportEvidence {
            active_view_id: previous.view_id.clone(),
            exported_snapshot_sha256: "1".repeat(64),
            canonical_revision: 41,
        })
        .unwrap();
    transaction.record_generated(&candidate).unwrap();

    let mut incomplete_pages = represented_pages.clone();
    incomplete_pages.pop();
    assert!(transaction
        .record_hydration(HydrationEvidence {
            candidate_view_id: candidate.view_id.clone(),
            canonical_revision: 41,
            represented_source_pages: incomplete_pages,
            hydrated_mark_sha256: "2".repeat(64),
        })
        .is_err());
    transaction
        .record_hydration(HydrationEvidence {
            candidate_view_id: candidate.view_id.clone(),
            canonical_revision: 41,
            represented_source_pages: represented_pages,
            hydrated_mark_sha256: "2".repeat(64),
        })
        .unwrap();
    transaction
        .record_verification(
            CandidateVerificationEvidence {
                candidate_view_id: candidate.view_id,
                generated_pdf_sha256: candidate.generated_pdf_sha256,
                manifest_sha256: candidate.manifest_sha256,
                mapping_authority_sha256: candidate.mapping_authority_sha256,
                hydrated_mark_sha256: "2".repeat(64),
            },
            RollbackEvidence {
                previous_mapping_sha256: "3".repeat(64),
                previous_view_id: Some(previous.view_id),
            },
        )
        .unwrap();
    transaction.commit_activation("4".repeat(64)).unwrap();
    assert_eq!(transaction.phase, CacheRegenerationPhase::Activated);
    let restored: CacheRegenerationTransaction =
        serde_json::from_slice(&serde_json::to_vec(&transaction).unwrap()).unwrap();
    restored.validate_persisted().unwrap();
}

#[test]
fn page_143_regeneration_can_roll_back_before_activation() {
    let golden = verify_virtual_spread_golden_fixture(FIXTURE).unwrap();
    let candidate = view(
        &golden.document_id,
        golden.view_id,
        golden.mapping_authority_sha256,
        'a',
        'b',
    );
    let mut transaction = CacheRegenerationTransaction::begin_clean(
        "page-143-rollback".to_owned(),
        golden.original_pdf_sha256,
        42,
        vec![1, 2],
        None,
        candidate,
    )
    .unwrap();

    transaction.rollback().unwrap();
    assert_eq!(transaction.phase, CacheRegenerationPhase::RolledBack);
    transaction.validate_persisted().unwrap();
}

use inkbridge_convert::{
    verify_virtual_spread_golden_fixture, verify_virtual_spread_page_143_production_fixture,
};
use inkbridge_folder_transport::{
    materialize_virtual_spread_cache, CacheRegenerationPhase, CacheRegenerationTransaction,
    CandidateVerificationEvidence, DirtyMarkExportEvidence, HydrationEvidence, RollbackEvidence,
    VirtualSpreadViewEvidence, VIRTUAL_SPREAD_CACHE_RELATIVE_ROOT,
};
use sha2::{Digest, Sha256};

const FIXTURE: &[u8] = include_bytes!(
    "../../inkbridge-convert/tests/fixtures/virtual-spread/page-143-contract-v1.json"
);
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

#[cfg(any(target_os = "linux", target_os = "android"))]
#[test]
fn verified_real_pair_materializes_at_the_hardware_proven_hidden_path() {
    let verified = verify_virtual_spread_page_143_production_fixture(
        SOURCE, GENERATED, SIDECAR, DESCRIPTOR, PDF_TAIL,
    )
    .unwrap();
    let view = VirtualSpreadViewEvidence::from_production_verification(&verified);
    let shared_storage = tempfile::tempdir().unwrap();

    let installed = materialize_virtual_spread_cache(
        shared_storage.path(),
        &verified,
        GENERATED.to_vec(),
        SIDECAR.to_vec(),
    )
    .unwrap();
    let expected_directory = shared_storage
        .path()
        .join(VIRTUAL_SPREAD_CACHE_RELATIVE_ROOT)
        .join(&view.document_id)
        .join(&view.view_id);
    assert_eq!(installed.directory, expected_directory);
    assert_eq!(
        installed.pdf_path,
        expected_directory.join(&view.cache_basename)
    );
    assert_eq!(
        installed.manifest_path,
        expected_directory.join(format!("{}.json", view.cache_basename))
    );
    assert_eq!(std::fs::read(&installed.pdf_path).unwrap(), GENERATED);
    assert_eq!(std::fs::read(&installed.manifest_path).unwrap(), SIDECAR);
    assert_eq!(std::fs::read(&installed.nomedia_path).unwrap(), b"");
    let repeated = materialize_virtual_spread_cache(
        shared_storage.path(),
        &verified,
        GENERATED.to_vec(),
        SIDECAR.to_vec(),
    )
    .unwrap();
    assert_eq!(repeated, installed);
    let activation = installed.into_verified_activation();
    assert_eq!(activation.pdf_bytes(), GENERATED);
    assert_eq!(activation.manifest_bytes(), SIDECAR);
}

#[cfg(any(target_os = "linux", target_os = "android"))]
#[test]
fn materialization_handoff_retains_verified_objects_after_path_replacement() {
    let verified = verify_virtual_spread_page_143_production_fixture(
        SOURCE, GENERATED, SIDECAR, DESCRIPTOR, PDF_TAIL,
    )
    .unwrap();
    let shared_storage = tempfile::tempdir().unwrap();
    let installed = materialize_virtual_spread_cache(
        shared_storage.path(),
        &verified,
        GENERATED.to_vec(),
        SIDECAR.to_vec(),
    )
    .unwrap();
    let detached_pdf = installed.directory.join("detached.pdf");
    let detached_manifest = installed.directory.join("detached.json");

    std::fs::rename(&installed.pdf_path, &detached_pdf).unwrap();
    std::fs::write(&installed.pdf_path, b"replacement PDF").unwrap();
    std::fs::rename(&installed.manifest_path, &detached_manifest).unwrap();
    std::fs::write(&installed.manifest_path, b"replacement sidecar").unwrap();

    assert_eq!(
        std::fs::read(&installed.pdf_path).unwrap(),
        b"replacement PDF"
    );
    assert_eq!(
        std::fs::read(&installed.manifest_path).unwrap(),
        b"replacement sidecar"
    );
    let activation = installed.into_verified_activation();
    assert_eq!(activation.pdf_bytes(), GENERATED);
    assert_eq!(activation.manifest_bytes(), SIDECAR);
}

#[cfg(any(target_os = "linux", target_os = "android"))]
#[test]
fn activation_handoff_ignores_in_place_cache_mutation() {
    let verified = verify_virtual_spread_page_143_production_fixture(
        SOURCE, GENERATED, SIDECAR, DESCRIPTOR, PDF_TAIL,
    )
    .unwrap();
    let shared_storage = tempfile::tempdir().unwrap();
    let installed = materialize_virtual_spread_cache(
        shared_storage.path(),
        &verified,
        GENERATED.to_vec(),
        SIDECAR.to_vec(),
    )
    .unwrap();
    let replacement = vec![b'X'; GENERATED.len()];

    // Truncate/write the existing pathname so its inode stays the same while
    // its contents no longer match the privately retained activation bytes.
    std::fs::write(&installed.pdf_path, replacement).unwrap();

    let activation = installed.into_verified_activation();
    assert_eq!(activation.pdf_bytes(), GENERATED);
    assert_eq!(activation.manifest_bytes(), SIDECAR);
}

#[cfg(any(target_os = "linux", target_os = "android"))]
#[test]
fn concurrent_first_materialization_is_idempotent() {
    let verified = verify_virtual_spread_page_143_production_fixture(
        SOURCE, GENERATED, SIDECAR, DESCRIPTOR, PDF_TAIL,
    )
    .unwrap();
    let shared_storage = tempfile::tempdir().unwrap();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));

    let installed = std::thread::scope(|scope| {
        let workers = (0..8)
            .map(|_| {
                let barrier = barrier.clone();
                let verified = &verified;
                let root = shared_storage.path();
                scope.spawn(move || {
                    barrier.wait();
                    materialize_virtual_spread_cache(
                        root,
                        verified,
                        GENERATED.to_vec(),
                        SIDECAR.to_vec(),
                    )
                })
            })
            .collect::<Vec<_>>();
        workers
            .into_iter()
            .map(|worker| worker.join().unwrap().unwrap())
            .collect::<Vec<_>>()
    });

    assert!(installed.windows(2).all(|pair| pair[0] == pair[1]));
    assert_eq!(std::fs::read(&installed[0].pdf_path).unwrap(), GENERATED);
    assert_eq!(std::fs::read(&installed[0].manifest_path).unwrap(), SIDECAR);
}

#[cfg(any(target_os = "linux", target_os = "android"))]
#[test]
fn materialization_rejects_a_legacy_partial_final_directory_without_repairing_it() {
    let verified = verify_virtual_spread_page_143_production_fixture(
        SOURCE, GENERATED, SIDECAR, DESCRIPTOR, PDF_TAIL,
    )
    .unwrap();
    let view = VirtualSpreadViewEvidence::from_production_verification(&verified);
    let shared_storage = tempfile::tempdir().unwrap();
    let directory = shared_storage
        .path()
        .join(VIRTUAL_SPREAD_CACHE_RELATIVE_ROOT)
        .join(&view.document_id)
        .join(&view.view_id);
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(directory.join(&view.cache_basename), GENERATED).unwrap();

    let error = materialize_virtual_spread_cache(
        shared_storage.path(),
        &verified,
        GENERATED.to_vec(),
        SIDECAR.to_vec(),
    )
    .unwrap_err();
    assert!(error.contains("media-index marker"));
    assert!(!directory.join(".nomedia").exists());
    assert!(!directory
        .join(format!("{}.json", view.cache_basename))
        .exists());
}

#[cfg(any(target_os = "linux", target_os = "android"))]
#[test]
fn sidecar_conflict_is_rejected_before_any_pdf_is_published() {
    let verified = verify_virtual_spread_page_143_production_fixture(
        SOURCE, GENERATED, SIDECAR, DESCRIPTOR, PDF_TAIL,
    )
    .unwrap();
    let view = VirtualSpreadViewEvidence::from_production_verification(&verified);
    let shared_storage = tempfile::tempdir().unwrap();
    let directory = shared_storage
        .path()
        .join(VIRTUAL_SPREAD_CACHE_RELATIVE_ROOT)
        .join(&view.document_id)
        .join(&view.view_id);
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(
        directory.join(format!("{}.json", view.cache_basename)),
        b"conflicting sidecar",
    )
    .unwrap();

    let error = materialize_virtual_spread_cache(
        shared_storage.path(),
        &verified,
        GENERATED.to_vec(),
        SIDECAR.to_vec(),
    )
    .unwrap_err();

    assert!(error.contains("media-index marker"));
    assert!(!directory.join(&view.cache_basename).exists());
    assert!(!directory.join(".nomedia").exists());
}

#[cfg(any(target_os = "linux", target_os = "android"))]
#[test]
fn materialization_rejects_oversized_conflicts_without_reading_their_contents() {
    let verified = verify_virtual_spread_page_143_production_fixture(
        SOURCE, GENERATED, SIDECAR, DESCRIPTOR, PDF_TAIL,
    )
    .unwrap();
    let view = VirtualSpreadViewEvidence::from_production_verification(&verified);
    let shared_storage = tempfile::tempdir().unwrap();
    let directory = shared_storage
        .path()
        .join(VIRTUAL_SPREAD_CACHE_RELATIVE_ROOT)
        .join(&view.document_id)
        .join(&view.view_id);
    std::fs::create_dir_all(&directory).unwrap();

    // A sparse conflict models a corrupt or hostile shared-storage entry without
    // allocating its apparent size. Verification must reject it by length.
    let oversized = std::fs::File::create(directory.join(".nomedia")).unwrap();
    oversized.set_len(64 * 1024 * 1024).unwrap();

    let error = materialize_virtual_spread_cache(
        shared_storage.path(),
        &verified,
        GENERATED.to_vec(),
        SIDECAR.to_vec(),
    )
    .unwrap_err();
    assert!(error.contains("already contains different bytes"));
}

#[cfg(any(target_os = "linux", target_os = "android"))]
#[test]
fn abandoned_full_size_staging_directory_is_reclaimed_before_publication() {
    let verified = verify_virtual_spread_page_143_production_fixture(
        SOURCE, GENERATED, SIDECAR, DESCRIPTOR, PDF_TAIL,
    )
    .unwrap();
    let view = VirtualSpreadViewEvidence::from_production_verification(&verified);
    let shared_storage = tempfile::tempdir().unwrap();
    let document_directory = shared_storage
        .path()
        .join(VIRTUAL_SPREAD_CACHE_RELATIVE_ROOT)
        .join(&view.document_id);
    let staging_directory = document_directory.join(format!(".{}.part", view.view_id));
    std::fs::create_dir_all(&staging_directory).unwrap();
    let abandoned = std::fs::File::create(staging_directory.join(&view.cache_basename)).unwrap();
    abandoned.set_len(512 * 1024 * 1024).unwrap();

    let installed = materialize_virtual_spread_cache(
        shared_storage.path(),
        &verified,
        GENERATED.to_vec(),
        SIDECAR.to_vec(),
    )
    .unwrap();

    assert!(!staging_directory.exists());
    assert_eq!(std::fs::read(installed.pdf_path).unwrap(), GENERATED);
    assert_eq!(std::fs::read(installed.manifest_path).unwrap(), SIDECAR);
}

#[test]
fn real_candidate_requires_complete_hydration_and_retains_rollback_evidence() {
    let verified = verify_virtual_spread_page_143_production_fixture(
        SOURCE, GENERATED, SIDECAR, DESCRIPTOR, PDF_TAIL,
    )
    .unwrap();
    let candidate = VirtualSpreadViewEvidence::from_production_verification(&verified);
    let previous = view(
        &candidate.document_id,
        format!("inkbridge-view-v1-{}", "a".repeat(64)),
        "b".repeat(64),
        'c',
        'd',
    );
    let represented_pages = vec![1, 2];
    let mut transaction = CacheRegenerationTransaction::begin_dirty(
        "real-page-143-regeneration".to_owned(),
        verified.source_pdf_sha256().to_owned(),
        represented_pages.clone(),
        previous.clone(),
        candidate.clone(),
    )
    .unwrap();
    transaction
        .record_dirty_export(DirtyMarkExportEvidence {
            active_view_id: previous.view_id.clone(),
            exported_snapshot_sha256: sha256(b"dirty-native-snapshot"),
            canonical_revision: 12,
        })
        .unwrap();
    transaction.record_generated(&candidate).unwrap();
    transaction
        .record_hydration(HydrationEvidence {
            candidate_view_id: candidate.view_id.clone(),
            canonical_revision: 12,
            represented_source_pages: represented_pages,
            hydrated_mark_sha256: sha256(b"hydrated-native-mark"),
        })
        .unwrap();
    transaction
        .record_verification(
            CandidateVerificationEvidence {
                candidate_view_id: candidate.view_id,
                generated_pdf_sha256: candidate.generated_pdf_sha256,
                manifest_sha256: candidate.manifest_sha256,
                mapping_authority_sha256: candidate.mapping_authority_sha256,
                hydrated_mark_sha256: sha256(b"hydrated-native-mark"),
            },
            RollbackEvidence {
                previous_mapping_sha256: sha256(b"previous-active-mapping"),
                previous_view_id: Some(previous.view_id),
            },
        )
        .unwrap();
    assert_eq!(transaction.phase, CacheRegenerationPhase::ReadyToActivate);
    transaction.validate_persisted().unwrap();

    let mut interrupted = transaction.clone();
    interrupted.rollback().unwrap();
    assert_eq!(interrupted.phase, CacheRegenerationPhase::RolledBack);
    interrupted.validate_persisted().unwrap();
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

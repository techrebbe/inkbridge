mod affine;
mod baseline;
mod model;
mod neoreader_repair;
mod pdf;
mod virtual_spread;

pub use affine::{AffineError, AffinePoint, AffineTransform};
pub use baseline::{
    parse_baseline_bytes, parse_document_baseline_bytes, serialize_baseline_export,
    serialize_baseline_page, BaselineExport, BaselinePage, BaselineRevisions, DocumentBaseline,
    DOCUMENT_BASELINE_SCHEMA_VERSION, SUPERNOTE_EXPORT_SCHEMA_VERSION,
};
pub use model::{
    geometry_fingerprint, CoordinateTransform, DocumentIdentity, Manifest, NativeStyle, Operation,
    StrokeSnapshot, Summary,
};
pub use virtual_spread::{
    parse_virtual_spread_manifest, stable_supernote_annotation_id,
    verify_virtual_spread_golden_fixture, VirtualSpreadGoldenVerification, VirtualSpreadManifest,
    VirtualSpreadMapping, VirtualSpreadSide, VIRTUAL_SPREAD_CONTRACT_TOLERANCE,
    VIRTUAL_SPREAD_GENERATOR_VERSION, VIRTUAL_SPREAD_GOLDEN_SCHEMA, VIRTUAL_SPREAD_MAPPING_DOMAIN,
    VIRTUAL_SPREAD_PAGE_143_FIXTURE_SHA256, VIRTUAL_SPREAD_PRODUCTION_ACTIVATION_ENABLED,
    VIRTUAL_SPREAD_SCHEMA, VIRTUAL_SPREAD_VIEW_DOMAIN,
};

use baseline::{index_baseline, load_baseline};
use pdf::{extract_pdf_strokes, PdfStrokeExtraction};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};

pub fn build_manifest(
    pdf_path: &Path,
    baseline_paths: &[PathBuf],
    normalized_y_offset: f64,
) -> Result<Manifest, String> {
    let mut baseline_strokes = Vec::new();
    let mut target_file_names = Vec::new();
    for path in baseline_paths {
        let export = load_baseline(path)?;
        if let Some(source_file_name) = export.source_file_name {
            add_target_file_name(&mut target_file_names, source_file_name)?;
        }
        baseline_strokes.extend(export.pages.into_iter().flat_map(|page| page.strokes));
    }
    let extraction = extract_pdf_strokes(pdf_path)?;
    build_manifest_from_extraction(
        pdf_path,
        extraction,
        baseline_strokes,
        target_file_names,
        normalized_y_offset,
    )
}

pub fn build_document_baseline(
    pdf_path: &Path,
    source_file_name: &str,
) -> Result<DocumentBaseline, String> {
    let pdf_sha256 = sha256_file(pdf_path)?;
    let PdfStrokeExtraction {
        page_count,
        strokes,
        broker_owned_source_uuids,
        incomplete_pages,
        failed_source_uuids,
        ..
    } = extract_pdf_strokes(pdf_path)?;
    if !incomplete_pages.is_empty() || !failed_source_uuids.is_empty() {
        return Err(format!(
            "could not safely snapshot all editable BOOX strokes in {}",
            pdf_path.display()
        ));
    }
    let (strokes, immutable_original_source_uuids) =
        partition_document_baseline_strokes(strokes, &broker_owned_source_uuids);
    let baseline = DocumentBaseline {
        schema_version: DOCUMENT_BASELINE_SCHEMA_VERSION,
        source_file_name: source_file_name.to_owned(),
        page_count,
        pdf_sha256,
        strokes,
        immutable_original_source_uuids,
    };
    baseline.validate()?;
    Ok(baseline)
}

fn partition_document_baseline_strokes(
    strokes: Vec<StrokeSnapshot>,
    broker_owned_source_uuids: &HashSet<String>,
) -> (Vec<StrokeSnapshot>, Vec<String>) {
    let (canonical, immutable_original): (Vec<_>, Vec<_>) = strokes
        .into_iter()
        .partition(|stroke| broker_owned_source_uuids.contains(&stroke.source_uuid));
    let mut immutable_original_source_uuids = immutable_original
        .into_iter()
        .map(|stroke| stroke.source_uuid)
        .collect::<Vec<_>>();
    immutable_original_source_uuids.sort();
    immutable_original_source_uuids.dedup();
    (canonical, immutable_original_source_uuids)
}

pub fn build_manifest_from_document_baseline(
    pdf_path: &Path,
    baseline: &DocumentBaseline,
    normalized_y_offset: f64,
) -> Result<Manifest, String> {
    baseline.validate()?;
    let extraction = exclude_immutable_original_annotations(
        extract_pdf_strokes(pdf_path)?,
        &baseline.immutable_original_source_uuids,
    );
    validate_document_page_count(baseline.page_count, extraction.page_count)?;
    let mut manifest = build_manifest_from_extraction(
        pdf_path,
        extraction,
        baseline.strokes.clone(),
        vec![baseline.source_file_name.clone()],
        normalized_y_offset,
    )?;
    manifest
        .document
        .source_file_name
        .clone_from(&baseline.source_file_name);
    Ok(manifest)
}

fn exclude_immutable_original_annotations(
    mut extraction: PdfStrokeExtraction,
    source_uuids: &[String],
) -> PdfStrokeExtraction {
    let ignored = source_uuids
        .iter()
        .map(String::as_str)
        .collect::<HashSet<_>>();
    extraction
        .strokes
        .retain(|stroke| !ignored.contains(stroke.source_uuid.as_str()));
    extraction
        .failed_source_uuids
        .retain(|source_uuid| !ignored.contains(source_uuid.as_str()));
    extraction
        .tombstoned_source_uuids
        .retain(|source_uuid| !ignored.contains(source_uuid.as_str()));
    extraction
}

fn validate_document_page_count(
    baseline_page_count: usize,
    returned_page_count: usize,
) -> Result<(), String> {
    if returned_page_count != baseline_page_count {
        return Err(format!(
            "NeoReader returned a PDF with {returned_page_count} pages, but the BOOX baseline has {baseline_page_count}; full-PDF fallback is required"
        ));
    }
    Ok(())
}

fn build_manifest_from_extraction(
    pdf_path: &Path,
    extraction: PdfStrokeExtraction,
    baseline_strokes: Vec<StrokeSnapshot>,
    target_file_names: Vec<String>,
    normalized_y_offset: f64,
) -> Result<Manifest, String> {
    let pdf_sha256 = sha256_file(pdf_path)?;
    let PdfStrokeExtraction {
        page_count,
        strokes: extracted,
        mut skipped,
        incomplete_pages,
        failed_source_uuids,
        tombstoned_source_uuids,
        ..
    } = extraction;
    validate_baseline_pages(&baseline_strokes, page_count)?;
    let baseline = index_baseline(baseline_strokes);
    let (operations, unchanged) = diff_strokes(
        extracted,
        &baseline,
        &incomplete_pages,
        &failed_source_uuids,
        &tombstoned_source_uuids,
    );

    let upserted = operations
        .iter()
        .filter(|operation| matches!(operation, Operation::UpsertStroke { .. }))
        .count();
    let deleted = operations.len() - upserted;
    skipped += baseline
        .values()
        .filter(|stroke| stroke.samples.len() < 2)
        .count();
    let manifest_seed = format!(
        "{pdf_sha256}|{}|{}|{}|{normalized_y_offset:.8}",
        operations.len(),
        upserted,
        deleted
    );
    let manifest_hash = format!("{:x}", Sha256::digest(manifest_seed.as_bytes()));
    let source_file_name = pdf_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("document.pdf")
        .to_owned();

    Ok(Manifest {
        schema_version: 1,
        manifest_id: format!("inkbridge-{}", &manifest_hash[..20]),
        source: "boox-neoreader-embedded-pdf".to_owned(),
        document: DocumentIdentity {
            source_file_name,
            target_file_names,
            page_count,
            pdf_sha256,
        },
        coordinate_transform: CoordinateTransform {
            pdf_to_supernote_normalized_y_offset: normalized_y_offset,
        },
        operations,
        summary: Summary {
            upserted,
            deleted,
            unchanged,
            skipped,
        },
    })
}

fn sha256_file(path: &Path) -> Result<String, String> {
    let file = fs::File::open(path)
        .map_err(|error| format!("could not read PDF {}: {error}", path.display()))?;
    let mut reader = BufReader::with_capacity(1024 * 1024, file);
    let mut digest = Sha256::new();
    let mut buffer = vec![0_u8; 1024 * 1024];
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|error| format!("could not read PDF {}: {error}", path.display()))?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn add_target_file_name(targets: &mut Vec<String>, candidate: String) -> Result<(), String> {
    if candidate.trim().is_empty() {
        return Ok(());
    }
    if let Some(existing) = targets.first() {
        if existing != &candidate {
            return Err(format!(
                "baseline exports name different target documents: {existing} and {candidate}"
            ));
        }
    } else {
        targets.push(candidate);
    }
    Ok(())
}

fn validate_baseline_pages(strokes: &[StrokeSnapshot], page_count: usize) -> Result<(), String> {
    if let Some(stroke) = strokes
        .iter()
        .find(|stroke| stroke.page_index as usize >= page_count)
    {
        return Err(format!(
            "baseline stroke {} references page {}, but the returned PDF has only {page_count} pages",
            stroke.source_uuid,
            stroke.page_index + 1
        ));
    }
    Ok(())
}

fn diff_strokes(
    extracted: Vec<StrokeSnapshot>,
    baseline: &HashMap<String, StrokeSnapshot>,
    incomplete_pages: &HashSet<u32>,
    failed_source_uuids: &HashSet<String>,
    tombstoned_source_uuids: &HashSet<String>,
) -> (Vec<Operation>, usize) {
    let baseline_pages = baseline
        .values()
        .map(|stroke| stroke.page_index)
        .collect::<HashSet<_>>();
    let mut active_ids = failed_source_uuids.clone();
    let mut operations = Vec::new();
    let mut unchanged = 0usize;

    for mut after in extracted {
        active_ids.insert(after.source_uuid.clone());
        let before = baseline.get(&after.source_uuid).cloned();
        if let Some(before) = &before {
            if after.origin == "pdf-ink" {
                let same_visible_thickness =
                    after.native_style.thickness == before.native_style.thickness;
                let same_visible_color =
                    after.native_style.pen_color == before.native_style.pen_color;
                after.native_style.layer_num = before.native_style.layer_num;
                after.native_style.pen_type = before.native_style.pen_type;
                if same_visible_thickness {
                    after.native_style.thickness = before.native_style.thickness;
                    preserve_pressure_profile(&before.samples, &mut after.samples);
                }
                if same_visible_color {
                    after.native_style.pen_color = before.native_style.pen_color;
                }
                after.geometry_fingerprint =
                    geometry_fingerprint(&after.native_style, &after.samples);
            }
            if after.page_index != before.page_index {
                operations.push(Operation::DeleteStroke {
                    source_uuid: before.source_uuid.clone(),
                    page_index: before.page_index,
                    before: before.clone(),
                });
                operations.push(Operation::UpsertStroke {
                    source_uuid: after.source_uuid.clone(),
                    page_index: after.page_index,
                    before: None,
                    after,
                });
                continue;
            }
            if after.geometry_fingerprint == before.geometry_fingerprint {
                unchanged += 1;
                continue;
            }
        }
        operations.push(Operation::UpsertStroke {
            source_uuid: after.source_uuid.clone(),
            page_index: after.page_index,
            before,
            after,
        });
    }

    for before in baseline.values() {
        if !active_ids.contains(&before.source_uuid)
            && (tombstoned_source_uuids.contains(&before.source_uuid)
                || should_infer_deletion(before, &baseline_pages, &active_ids, incomplete_pages))
        {
            operations.push(Operation::DeleteStroke {
                source_uuid: before.source_uuid.clone(),
                page_index: before.page_index,
                before: before.clone(),
            });
        }
    }
    operations.sort_by(|left, right| operation_key(left).cmp(&operation_key(right)));
    (operations, unchanged)
}

fn should_infer_deletion(
    before: &StrokeSnapshot,
    baseline_pages: &HashSet<u32>,
    active_ids: &HashSet<String>,
    incomplete_pages: &HashSet<u32>,
) -> bool {
    baseline_pages.contains(&before.page_index)
        && !incomplete_pages.contains(&before.page_index)
        && !active_ids.contains(&before.source_uuid)
}

fn preserve_pressure_profile(before: &[[f64; 3]], after: &mut [[f64; 3]]) {
    if before.is_empty() || after.is_empty() {
        return;
    }
    let last_before = before.len().saturating_sub(1);
    let last_after = after.len().saturating_sub(1).max(1);
    for (index, sample) in after.iter_mut().enumerate() {
        let source_index = ((index * last_before) as f64 / last_after as f64).round() as usize;
        sample[2] = before[source_index.min(last_before)][2];
    }
}

fn operation_key(operation: &Operation) -> (u32, &str, u8) {
    match operation {
        Operation::UpsertStroke {
            source_uuid,
            page_index,
            ..
        } => (*page_index, source_uuid, 0),
        Operation::DeleteStroke {
            source_uuid,
            page_index,
            ..
        } => (*page_index, source_uuid, 1),
    }
}

pub fn baseline_by_id(
    baseline_paths: &[PathBuf],
) -> Result<HashMap<String, StrokeSnapshot>, String> {
    let mut all = Vec::new();
    for path in baseline_paths {
        all.extend(
            load_baseline(path)?
                .pages
                .into_iter()
                .flat_map(|page| page.strokes),
        );
    }
    Ok(index_baseline(all))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn test_stroke(source_uuid: &str, page_index: u32) -> StrokeSnapshot {
        let native_style = NativeStyle::default();
        let samples = vec![[0.1, 0.2, 1000.0], [0.2, 0.3, 1100.0]];
        let geometry_fingerprint = geometry_fingerprint(&native_style, &samples);
        StrokeSnapshot {
            source_uuid: source_uuid.to_owned(),
            origin: "supernote-native".to_owned(),
            page_index,
            native_style,
            samples,
            geometry_fingerprint,
        }
    }

    #[test]
    fn pressure_profile_resamples_without_losing_endpoints() {
        let before = vec![[0.0, 0.0, 100.0], [0.0, 0.0, 200.0], [0.0, 0.0, 300.0]];
        let mut after = vec![[0.0, 0.0, 0.0]; 5];
        preserve_pressure_profile(&before, &mut after);
        assert_eq!(after[0][2], 100.0);
        assert_eq!(after[4][2], 300.0);
    }

    #[test]
    fn extraction_failure_page_is_excluded_from_deletion_inference() {
        let before = StrokeSnapshot {
            source_uuid: "stroke-on-incomplete-page".to_owned(),
            origin: "supernote-native".to_owned(),
            page_index: 2,
            native_style: NativeStyle::default(),
            samples: vec![[0.1, 0.2, 1000.0], [0.2, 0.3, 1000.0]],
            geometry_fingerprint: "fingerprint".to_owned(),
        };
        let baseline = HashMap::from([(before.source_uuid.clone(), before)]);
        let baseline_pages = HashSet::from([2]);
        let active_ids = HashSet::new();
        let incomplete_pages = HashSet::from([2]);
        let deletions = baseline
            .values()
            .filter(|stroke| {
                should_infer_deletion(stroke, &baseline_pages, &active_ids, &incomplete_pages)
            })
            .count();
        assert_eq!(deletions, 0);
    }

    #[test]
    fn cross_page_stroke_is_deleted_from_source_and_inserted_on_destination() {
        let before = test_stroke("moved-stroke", 0);
        let mut after = before.clone();
        after.page_index = 1;
        let baseline = HashMap::from([(before.source_uuid.clone(), before.clone())]);

        let (operations, unchanged) = diff_strokes(
            vec![after.clone()],
            &baseline,
            &HashSet::new(),
            &HashSet::new(),
            &HashSet::new(),
        );

        assert_eq!(unchanged, 0);
        assert_eq!(operations.len(), 2);
        assert!(operations.iter().any(|operation| matches!(
            operation,
            Operation::DeleteStroke {
                source_uuid,
                page_index: 0,
                before: deleted,
            } if source_uuid == "moved-stroke" && deleted == &before
        )));
        assert!(operations.iter().any(|operation| matches!(
            operation,
            Operation::UpsertStroke {
                source_uuid,
                page_index: 1,
                before: None,
                after: inserted,
            } if source_uuid == "moved-stroke" && inserted == &after
        )));
    }

    #[test]
    fn pdf_ink_diff_preserves_explicit_visible_restyle() {
        let mut before = test_stroke("restyled", 0);
        before.native_style.layer_num = 3;
        before.native_style.pen_type = 16;
        before.native_style.pen_color = 0x9d;
        before.samples[0][2] = 900.0;
        before.samples[1][2] = 2200.0;
        before.geometry_fingerprint = geometry_fingerprint(&before.native_style, &before.samples);
        let mut after = before.clone();
        after.origin = "pdf-ink".to_owned();
        after.native_style.layer_num = 0;
        after.native_style.pen_type = 10;
        after.native_style.thickness += 200;
        after.native_style.pen_color = 0;
        after.samples[0][2] = 2100.0;
        after.samples[1][2] = 2100.0;
        after.geometry_fingerprint = geometry_fingerprint(&after.native_style, &after.samples);
        let baseline = HashMap::from([(before.source_uuid.clone(), before)]);

        let (operations, unchanged) = diff_strokes(
            vec![after],
            &baseline,
            &HashSet::new(),
            &HashSet::new(),
            &HashSet::new(),
        );

        assert_eq!(unchanged, 0);
        let Operation::UpsertStroke { after, .. } = &operations[0] else {
            panic!("restyle must emit an upsert")
        };
        assert_eq!(after.native_style.layer_num, 3);
        assert_eq!(after.native_style.pen_type, 16);
        assert_eq!(
            after.native_style.thickness,
            baseline["restyled"].native_style.thickness + 200
        );
        assert_eq!(after.native_style.pen_color, 0);
        assert_eq!(after.samples[0][2], 2100.0);
        assert_eq!(after.samples[1][2], 2100.0);
    }

    #[test]
    fn baseline_page_must_exist_in_returned_pdf() {
        let baseline = vec![test_stroke("out-of-range", 2)];
        let error = validate_baseline_pages(&baseline, 2)
            .expect_err("a zero-based page index equal to page count is invalid");
        assert!(error.contains("out-of-range"));
        assert!(error.contains("page 3"));
        assert!(error.contains("2 pages"));
    }

    #[test]
    fn compact_document_page_count_must_match_baseline() {
        validate_document_page_count(9, 9).unwrap();

        let added_page = validate_document_page_count(9, 10)
            .expect_err("adding a page must require the full-PDF fallback");
        assert!(added_page.contains("10 pages"));
        assert!(added_page.contains("baseline has 9"));

        let removed_page = validate_document_page_count(9, 8)
            .expect_err("removing a page must require the full-PDF fallback");
        assert!(removed_page.contains("8 pages"));
        assert!(removed_page.contains("baseline has 9"));
    }

    #[test]
    fn baseline_exports_must_name_the_same_target_document() {
        let mut targets = Vec::new();
        add_target_file_name(&mut targets, "document-a.pdf".to_owned()).unwrap();
        add_target_file_name(&mut targets, "document-a.pdf".to_owned()).unwrap();
        let error = add_target_file_name(&mut targets, "document-b.pdf".to_owned())
            .expect_err("operations cannot safely target two different documents");

        assert_eq!(targets, vec!["document-a.pdf"]);
        assert!(error.contains("document-a.pdf"));
        assert!(error.contains("document-b.pdf"));
    }

    #[test]
    fn failed_destination_annotation_preserves_cross_page_source_identity() {
        let before = test_stroke("moved-but-damaged", 0);
        let baseline = HashMap::from([(before.source_uuid.clone(), before)]);
        let incomplete_pages = HashSet::from([1]);
        let failed_source_uuids = HashSet::from(["moved-but-damaged".to_owned()]);

        let (operations, unchanged) = diff_strokes(
            Vec::new(),
            &baseline,
            &incomplete_pages,
            &failed_source_uuids,
            &HashSet::new(),
        );

        assert!(operations.is_empty());
        assert_eq!(unchanged, 0);
    }

    #[test]
    fn explicit_broker_tombstone_deletes_only_its_stroke_on_an_incomplete_page() {
        let deleted = test_stroke("explicitly-deleted", 0);
        let preserved = test_stroke("unrelated-malformed", 0);
        let baseline = HashMap::from([
            (deleted.source_uuid.clone(), deleted.clone()),
            (preserved.source_uuid.clone(), preserved),
        ]);
        let incomplete_pages = HashSet::from([0]);
        let tombstones = HashSet::from([deleted.source_uuid.clone()]);

        let (operations, unchanged) = diff_strokes(
            Vec::new(),
            &baseline,
            &incomplete_pages,
            &HashSet::new(),
            &tombstones,
        );

        assert_eq!(unchanged, 0);
        assert_eq!(operations.len(), 1);
        assert!(matches!(
            &operations[0],
            Operation::DeleteStroke {
                source_uuid,
                page_index: 0,
                before,
            } if source_uuid == "explicitly-deleted" && before == &deleted
        ));
    }

    #[test]
    fn pdf_hashing_streams_the_file_and_matches_in_memory_sha256() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("large-enough-to-cross-chunks.pdf");
        let bytes = vec![0x5a; 2 * 1024 * 1024 + 17];
        let mut file = fs::File::create(&path).unwrap();
        file.write_all(&bytes).unwrap();
        drop(file);

        assert_eq!(
            sha256_file(&path).unwrap(),
            format!("{:x}", Sha256::digest(&bytes))
        );
    }

    #[test]
    fn compact_baseline_ignores_immutable_original_ink_but_keeps_new_boox_ink() {
        let canonical = test_stroke("canonical", 0);
        let immutable_original = test_stroke("original-ink", 0);
        let (baseline_strokes, ignored) = partition_document_baseline_strokes(
            vec![canonical.clone(), immutable_original.clone()],
            &HashSet::from([canonical.source_uuid.clone()]),
        );
        assert_eq!(baseline_strokes, vec![canonical.clone()]);
        assert_eq!(ignored, vec![immutable_original.source_uuid.clone()]);

        let mut moved_original = immutable_original.clone();
        moved_original.samples[0][0] += 0.1;
        moved_original.geometry_fingerprint =
            geometry_fingerprint(&moved_original.native_style, &moved_original.samples);
        let new_boox_stroke = test_stroke("new-boox-stroke", 0);
        let extraction = PdfStrokeExtraction {
            page_count: 1,
            strokes: vec![moved_original, canonical.clone(), new_boox_stroke.clone()],
            skipped: 0,
            incomplete_pages: HashSet::new(),
            failed_source_uuids: HashSet::new(),
            tombstoned_source_uuids: HashSet::new(),
            broker_owned_source_uuids: HashSet::from([canonical.source_uuid.clone()]),
        };
        let filtered = exclude_immutable_original_annotations(extraction, &ignored);
        let baseline = HashMap::from([(canonical.source_uuid.clone(), canonical)]);
        let (operations, unchanged) = diff_strokes(
            filtered.strokes,
            &baseline,
            &filtered.incomplete_pages,
            &filtered.failed_source_uuids,
            &filtered.tombstoned_source_uuids,
        );

        assert_eq!(unchanged, 1);
        assert_eq!(operations.len(), 1);
        assert!(matches!(
            &operations[0],
            Operation::UpsertStroke {
                source_uuid,
                before: None,
                after,
                ..
            } if source_uuid == "new-boox-stroke" && after == &new_boox_stroke
        ));
    }
}

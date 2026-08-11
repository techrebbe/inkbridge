mod baseline;
mod model;
mod pdf;

pub use model::{
    geometry_fingerprint, CoordinateTransform, DocumentIdentity, Manifest, NativeStyle, Operation,
    StrokeSnapshot, Summary,
};

use baseline::{index_baseline, load_baseline};
use pdf::extract_pdf_strokes;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

pub fn build_manifest(
    pdf_path: &Path,
    baseline_paths: &[PathBuf],
    normalized_y_offset: f64,
) -> Result<Manifest, String> {
    let pdf_bytes = fs::read(pdf_path)
        .map_err(|error| format!("could not read PDF {}: {error}", pdf_path.display()))?;
    let pdf_sha256 = format!("{:x}", Sha256::digest(&pdf_bytes));
    let (page_count, extracted, mut skipped, incomplete_pages) = extract_pdf_strokes(pdf_path)?;

    let mut baseline_strokes = Vec::new();
    let mut target_file_names = Vec::new();
    for path in baseline_paths {
        let export = load_baseline(path)?;
        if let Some(source_file_name) = export.source_file_name {
            if !source_file_name.trim().is_empty() && !target_file_names.contains(&source_file_name)
            {
                target_file_names.push(source_file_name);
            }
        }
        baseline_strokes.extend(export.strokes);
    }
    let baseline = index_baseline(baseline_strokes);
    let baseline_pages = baseline
        .values()
        .map(|stroke| stroke.page_index)
        .collect::<HashSet<_>>();
    let mut active_ids = HashSet::new();
    let mut operations = Vec::new();
    let mut unchanged = 0usize;

    for mut after in extracted {
        active_ids.insert(after.source_uuid.clone());
        let before = baseline.get(&after.source_uuid).cloned();
        if let Some(before) = &before {
            if after.origin == "pdf-ink" {
                after.native_style = before.native_style.clone();
                preserve_pressure_profile(&before.samples, &mut after.samples);
                after.geometry_fingerprint =
                    geometry_fingerprint(&after.native_style, &after.samples);
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
        if should_infer_deletion(before, &baseline_pages, &active_ids, &incomplete_pages) {
            operations.push(Operation::DeleteStroke {
                source_uuid: before.source_uuid.clone(),
                page_index: before.page_index,
                before: before.clone(),
            });
        }
    }
    operations.sort_by(|left, right| operation_key(left).cmp(&operation_key(right)));

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
        all.extend(load_baseline(path)?.strokes);
    }
    Ok(index_baseline(all))
}

#[cfg(test)]
mod tests {
    use super::*;

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
}

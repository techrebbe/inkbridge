use crate::transport::{
    canonical_path_key, publish_create_only, remove_file_if_exists, sibling_temporary,
    symlink_metadata_if_exists,
};
use crate::{CloudObject, DocumentFolders};
use inkbridge_broker::sha256_hex;
use inkbridge_convert::{parse_baseline_bytes, serialize_baseline_page};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

pub(crate) const SOURCE_PAGE_INDEX: &str = "inkbridge-source-page-index";
pub(crate) const SOURCE_PAGE_INDICES: &str = "inkbridge-source-page-indices";

pub(crate) fn page_identity(document_id: &str, page_index: u32) -> String {
    sha256_hex(format!("{document_id}\0supernote-page\0{page_index}").as_bytes())
}

pub(crate) fn snapshot_identity(document_id: &str, page_indices: &[u32]) -> Result<String, String> {
    if page_indices.is_empty() {
        return Err("Supernote snapshot contains no represented page".to_owned());
    }
    if !page_indices.windows(2).all(|pair| pair[0] < pair[1]) {
        return Err("Supernote snapshot page indices must be strictly increasing".to_owned());
    }
    if let [page_index] = page_indices {
        return Ok(page_identity(document_id, *page_index));
    }
    let pages = page_indices
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(",");
    Ok(sha256_hex(
        format!("{document_id}\0supernote-pages-v1\0{pages}").as_bytes(),
    ))
}

pub(crate) fn object_page_indices(object: &CloudObject) -> Result<Option<Vec<u32>>, String> {
    let single = object.metadata.get(SOURCE_PAGE_INDEX);
    let multiple = object.metadata.get(SOURCE_PAGE_INDICES);
    if single.is_some() && multiple.is_some() {
        return Err(format!(
            "accepted Supernote upload {} declares both single- and multi-page scope",
            object.path
        ));
    }
    let Some(serialized) = single.or(multiple) else {
        return Ok(None);
    };
    let page_indices = serialized
        .split(',')
        .map(|value| {
            value.parse::<u32>().map_err(|error| {
                format!(
                    "accepted Supernote upload {} has invalid page index {value}: {error}",
                    object.path
                )
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    if page_indices.is_empty() || !page_indices.windows(2).all(|pair| pair[0] < pair[1]) {
        return Err(format!(
            "accepted Supernote upload {} has invalid page-index scope {serialized}",
            object.path
        ));
    }
    if single.is_some() && page_indices.len() != 1 {
        return Err(format!(
            "accepted Supernote upload {} has an invalid single-page scope",
            object.path
        ));
    }
    Ok(Some(page_indices))
}

pub(crate) fn accepted_snapshot_path(
    document: &DocumentFolders,
    source_local_id: &str,
    source_revision: u64,
    source_hash: &str,
) -> PathBuf {
    let root = document.supernote_accepted_directory();
    root.join(format!(
        "r{source_revision:020}-{source_local_id}-{source_hash}.json"
    ))
}

pub(crate) fn persist_snapshot_bytes(
    document: &DocumentFolders,
    source_local_id: &str,
    source_revision: u64,
    expected_hash: &str,
    bytes: &[u8],
) -> Result<PathBuf, String> {
    let actual_hash = sha256_hex(bytes);
    if actual_hash != expected_hash {
        return Err(format!(
            "Supernote snapshot bytes hash {actual_hash} does not match expected hash {expected_hash}"
        ));
    }
    let destination =
        accepted_snapshot_path(document, source_local_id, source_revision, expected_hash);
    match symlink_metadata_if_exists(&destination)? {
        Some(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            return Err(format!(
                "Supernote snapshot destination {} is not a regular file",
                destination.display()
            ));
        }
        Some(_) => {
            let installed_hash =
                sha256_hex(&fs::read(&destination).map_err(|error| {
                    format!("could not read {}: {error}", destination.display())
                })?);
            if installed_hash == expected_hash {
                return Ok(destination);
            }
            remove_file_if_exists(&destination)?;
        }
        None => {}
    }

    let parent = destination.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)
        .map_err(|error| format!("could not create {}: {error}", parent.display()))?;
    let temporary = sibling_temporary(&destination, "snapshot");
    remove_file_if_exists(&temporary)?;
    fs::write(&temporary, bytes)
        .map_err(|error| format!("could not write {}: {error}", temporary.display()))?;
    let _ = publish_create_only(&temporary, &destination)?;
    let installed_hash = sha256_hex(
        &fs::read(&destination)
            .map_err(|error| format!("could not read {}: {error}", destination.display()))?,
    );
    if installed_hash != expected_hash {
        return Err(format!(
            "Supernote snapshot {} changed while it was being installed",
            destination.display()
        ));
    }
    Ok(destination)
}

pub(crate) fn materialize_non_overlapping_baselines(
    document: &DocumentFolders,
    accepted: &BTreeMap<String, String>,
) -> Result<BTreeMap<String, String>, String> {
    let mut candidates = Vec::new();
    for (path_key, expected_hash) in accepted {
        let path = PathBuf::from(path_key);
        let bytes = fs::read(&path)
            .map_err(|error| format!("could not read {}: {error}", path.display()))?;
        let actual_hash = sha256_hex(&bytes);
        if &actual_hash != expected_hash {
            return Err(format!(
                "accepted Supernote snapshot {} hash {actual_hash} does not match {expected_hash}",
                path.display()
            ));
        }
        let export = parse_baseline_bytes(&bytes, path_key)?;
        let revision = accepted_snapshot_revision(&path)?;
        candidates.push((path, expected_hash.clone(), revision, export));
    }

    let mut selected = BTreeMap::<u32, (u64, usize)>::new();
    for (candidate_index, (_, _, revision, export)) in candidates.iter().enumerate() {
        for page in &export.pages {
            match selected.get(&page.page_index) {
                Some((selected_revision, _)) if selected_revision > revision => {}
                Some((selected_revision, selected_index)) if selected_revision == revision => {
                    if *selected_index != candidate_index {
                        return Err(format!(
                            "accepted Supernote revision {revision} contains competing snapshots for page {}",
                            page.page_index + 1
                        ));
                    }
                }
                _ => {
                    selected.insert(page.page_index, (*revision, candidate_index));
                }
            }
        }
    }

    let mut materialized = BTreeMap::new();
    for (candidate_index, (path, expected_hash, revision, export)) in candidates.iter().enumerate()
    {
        let selected_pages = export
            .pages
            .iter()
            .filter(|page| {
                selected
                    .get(&page.page_index)
                    .is_some_and(|(_, index)| *index == candidate_index)
            })
            .collect::<Vec<_>>();
        if selected_pages.is_empty() {
            continue;
        }
        if selected_pages.len() == export.pages.len() {
            materialized.insert(canonical_path_key(path), expected_hash.clone());
            continue;
        }
        for page in selected_pages {
            let bytes = serialize_baseline_page(export, page)?;
            let page_hash = sha256_hex(&bytes);
            let page_identity = page_identity(&document.document_id, page.page_index);
            let page_path =
                persist_snapshot_bytes(document, &page_identity, *revision, &page_hash, &bytes)?;
            materialized.insert(canonical_path_key(&page_path), page_hash);
        }
    }
    Ok(materialized)
}

fn accepted_snapshot_revision(path: &Path) -> Result<u64, String> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            format!(
                "accepted Supernote snapshot {} has no file name",
                path.display()
            )
        })?;
    let revision = name
        .strip_prefix('r')
        .and_then(|name| name.split_once('-'))
        .map(|(revision, _)| revision)
        .ok_or_else(|| {
            format!(
                "accepted Supernote snapshot {} has no revision prefix",
                path.display()
            )
        })?;
    revision.parse::<u64>().map_err(|error| {
        format!(
            "accepted Supernote snapshot {} has invalid revision {revision}: {error}",
            path.display()
        )
    })
}

#[cfg(test)]
mod tests {
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
    fn accepted_batch_is_split_when_only_one_half_is_superseded() {
        let directory = tempdir().unwrap();
        let document = configured_document(directory.path());
        let batch = br#"{
            "schemaVersion":1,
            "sourceFileName":"Configured.pdf",
            "pages":[
                {"pageIndex":0,"strokes":[{"sourceUuid":"old-left","sourceKey":"old-left","thickness":400,"penColor":0,"penType":16,"samples":[[0.1,0.2,900],[0.2,0.3,1000]]}]},
                {"pageIndex":1,"strokes":[{"sourceUuid":"right","sourceKey":"right","thickness":400,"penColor":0,"penType":16,"samples":[[0.3,0.4,900],[0.4,0.5,1000]]}]}
            ]
        }"#;
        let newer_left = br#"{
            "schemaVersion":1,
            "sourceFileName":"Configured.pdf",
            "pageIndex":0,
            "strokes":[{"sourceUuid":"new-left","sourceKey":"new-left","thickness":400,"penColor":0,"penType":16,"samples":[[0.5,0.6,900],[0.6,0.7,1000]]}]
        }"#;
        let batch_hash = sha256_hex(batch);
        let batch_identity = snapshot_identity(&document.document_id, &[0, 1]).unwrap();
        let batch_path =
            persist_snapshot_bytes(&document, &batch_identity, 1, &batch_hash, batch).unwrap();
        let left_hash = sha256_hex(newer_left);
        let left_path = persist_snapshot_bytes(
            &document,
            &page_identity(&document.document_id, 0),
            2,
            &left_hash,
            newer_left,
        )
        .unwrap();
        let accepted = BTreeMap::from([
            (canonical_path_key(&batch_path), batch_hash),
            (canonical_path_key(&left_path), left_hash),
        ]);

        let materialized = materialize_non_overlapping_baselines(&document, &accepted).unwrap();
        assert_eq!(materialized.len(), 2);
        let mut by_page = BTreeMap::new();
        for path in materialized.keys() {
            let export = parse_baseline_bytes(&fs::read(path).unwrap(), path).unwrap();
            assert_eq!(export.pages.len(), 1);
            by_page.insert(
                export.pages[0].page_index,
                export.pages[0].strokes[0].source_uuid.clone(),
            );
        }
        assert_eq!(by_page.get(&0).map(String::as_str), Some("new-left"));
        assert_eq!(by_page.get(&1).map(String::as_str), Some("right"));
    }
}

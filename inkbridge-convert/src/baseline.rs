use crate::model::{geometry_fingerprint, NativeStyle, StrokeSnapshot};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::path::Path;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ExportEnvelope {
    #[serde(default)]
    schema_version: Option<u32>,
    #[serde(default)]
    source_file_name: Option<String>,
    #[serde(default)]
    document_id: Option<String>,
    #[serde(default)]
    based_on: Option<BaselineRevisions>,
    #[serde(default)]
    page_index: Option<u32>,
    #[serde(default)]
    strokes: Option<Vec<ExportStroke>>,
    #[serde(default)]
    pages: Option<Vec<ExportPage>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ExportPage {
    page_index: u32,
    strokes: Vec<ExportStroke>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct BaselineRevisions {
    pub boox: u64,
    pub supernote: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ExportStroke {
    source_uuid: Option<String>,
    source_key: String,
    #[serde(default)]
    layer_num: i64,
    thickness: i64,
    pen_color: i64,
    pen_type: i64,
    samples: Vec<[f64; 3]>,
}

#[derive(Clone, Debug)]
pub struct BaselineExport {
    pub source_file_name: Option<String>,
    pub document_id: Option<String>,
    pub based_on: Option<BaselineRevisions>,
    pub pages: Vec<BaselinePage>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct BaselinePage {
    pub page_index: u32,
    pub strokes: Vec<StrokeSnapshot>,
}

pub const SUPERNOTE_EXPORT_SCHEMA_VERSION: u32 = 1;
const LEGACY_SINGLE_PAGE_EXPORT_SCHEMA_VERSION: u32 = 2;

pub const DOCUMENT_BASELINE_SCHEMA_VERSION: u32 = 1;

/// Compact, device-local snapshot of the editable ink in one broker-generated PDF.
///
/// The BOOX companion records this before opening NeoReader. After NeoReader closes,
/// the same converter diffs the edited PDF against this snapshot. The original PDF
/// bytes never have to leave the device just to preserve stroke identities or infer
/// deletions.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DocumentBaseline {
    pub schema_version: u32,
    pub source_file_name: String,
    pub page_count: usize,
    pub pdf_sha256: String,
    pub strokes: Vec<StrokeSnapshot>,
    /// Stable identities belonging to annotations already present in the immutable original.
    #[serde(default)]
    pub immutable_original_source_uuids: Vec<String>,
}

impl DocumentBaseline {
    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != DOCUMENT_BASELINE_SCHEMA_VERSION {
            return Err(format!(
                "unsupported BOOX baseline schema {}",
                self.schema_version
            ));
        }
        if self.source_file_name.trim().is_empty() {
            return Err("BOOX baseline sourceFileName is empty".to_owned());
        }
        if self.page_count == 0 {
            return Err("BOOX baseline pageCount must be positive".to_owned());
        }
        if self.pdf_sha256.len() != 64
            || !self
                .pdf_sha256
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        {
            return Err("BOOX baseline pdfSha256 is not lowercase SHA-256".to_owned());
        }
        let mut identities = std::collections::HashSet::new();
        for stroke in &self.strokes {
            if stroke.source_uuid.trim().is_empty() {
                return Err("BOOX baseline contains a stroke without an identity".to_owned());
            }
            if !identities.insert(stroke.source_uuid.as_str()) {
                return Err(format!(
                    "BOOX baseline contains duplicate stroke identity {}",
                    stroke.source_uuid
                ));
            }
            if stroke.page_index as usize >= self.page_count {
                return Err(format!(
                    "BOOX baseline stroke {} references page {}, but pageCount is {}",
                    stroke.source_uuid,
                    stroke.page_index + 1,
                    self.page_count
                ));
            }
            if stroke.samples.len() < 2 {
                return Err(format!(
                    "BOOX baseline stroke {} contains fewer than two samples",
                    stroke.source_uuid
                ));
            }
        }
        for source_uuid in &self.immutable_original_source_uuids {
            if source_uuid.trim().is_empty() {
                return Err(
                    "BOOX baseline contains an immutable-original annotation without an identity"
                        .to_owned(),
                );
            }
            if !identities.insert(source_uuid.as_str()) {
                return Err(format!(
                    "BOOX baseline repeats annotation identity {source_uuid} across canonical and immutable-original inventories"
                ));
            }
        }
        Ok(())
    }
}

pub fn parse_document_baseline_bytes(
    bytes: &[u8],
    source_name: &str,
) -> Result<DocumentBaseline, String> {
    let baseline: DocumentBaseline = serde_json::from_slice(bytes)
        .map_err(|error| format!("invalid BOOX baseline JSON in {source_name}: {error}"))?;
    baseline.validate()?;
    Ok(baseline)
}

pub fn load_baseline(path: &Path) -> Result<BaselineExport, String> {
    let text = fs::read_to_string(path)
        .map_err(|error| format!("could not read baseline {}: {error}", path.display()))?;
    parse_baseline_text(&text, &path.display().to_string())
}

pub fn parse_baseline_bytes(bytes: &[u8], source_name: &str) -> Result<BaselineExport, String> {
    let text = std::str::from_utf8(bytes)
        .map_err(|error| format!("Supernote export {source_name} is not UTF-8: {error}"))?;
    parse_baseline_text(text, source_name)
}

/// Serialize one parsed page as a standalone legacy-compatible snapshot.
///
/// The folder transport uses this to materialize non-overlapping accepted
/// baselines when a newer one-page export supersedes only half of an older
/// atomic Virtual Spread batch.
pub fn serialize_baseline_page(
    export: &BaselineExport,
    page: &BaselinePage,
) -> Result<Vec<u8>, String> {
    let strokes = serialize_strokes(page);
    let mut value = serde_json::Map::from_iter([
        (
            "schemaVersion".to_owned(),
            serde_json::Value::from(SUPERNOTE_EXPORT_SCHEMA_VERSION),
        ),
        (
            "pageIndex".to_owned(),
            serde_json::Value::from(page.page_index),
        ),
        ("strokes".to_owned(), serde_json::Value::Array(strokes)),
    ]);
    if let Some(source_file_name) = &export.source_file_name {
        value.insert(
            "sourceFileName".to_owned(),
            serde_json::Value::String(source_file_name.clone()),
        );
    }
    if let Some(document_id) = &export.document_id {
        value.insert(
            "documentId".to_owned(),
            serde_json::Value::String(document_id.clone()),
        );
    }
    if let Some(based_on) = export.based_on {
        value.insert(
            "basedOn".to_owned(),
            serde_json::to_value(based_on).map_err(|error| error.to_string())?,
        );
    }
    serde_json::to_vec(&serde_json::Value::Object(value)).map_err(|error| error.to_string())
}

/// Serialize a complete parsed snapshot with an authoritative revision
/// frontier supplied by the transport that verified a safe rebase.
pub fn serialize_baseline_export(
    export: &BaselineExport,
    based_on: BaselineRevisions,
) -> Result<Vec<u8>, String> {
    let pages = export
        .pages
        .iter()
        .map(|page| {
            serde_json::json!({
                "pageIndex": page.page_index,
                "strokes": serialize_strokes(page),
            })
        })
        .collect::<Vec<_>>();
    let mut value = serde_json::Map::from_iter([
        (
            "schemaVersion".to_owned(),
            serde_json::Value::from(SUPERNOTE_EXPORT_SCHEMA_VERSION),
        ),
        ("pages".to_owned(), serde_json::Value::Array(pages)),
        (
            "basedOn".to_owned(),
            serde_json::to_value(based_on).map_err(|error| error.to_string())?,
        ),
    ]);
    if let Some(source_file_name) = &export.source_file_name {
        value.insert(
            "sourceFileName".to_owned(),
            serde_json::Value::String(source_file_name.clone()),
        );
    }
    if let Some(document_id) = &export.document_id {
        value.insert(
            "documentId".to_owned(),
            serde_json::Value::String(document_id.clone()),
        );
    }
    serde_json::to_vec(&serde_json::Value::Object(value)).map_err(|error| error.to_string())
}

fn serialize_strokes(page: &BaselinePage) -> Vec<serde_json::Value> {
    page.strokes
        .iter()
        .map(|stroke| {
            serde_json::json!({
                "sourceUuid": stroke.source_uuid,
                "sourceKey": stroke.source_uuid,
                "layerNum": stroke.native_style.layer_num,
                "thickness": stroke.native_style.thickness,
                "penColor": stroke.native_style.pen_color,
                "penType": stroke.native_style.pen_type,
                "samples": stroke.samples,
            })
        })
        .collect()
}

fn parse_baseline_text(text: &str, source_name: &str) -> Result<BaselineExport, String> {
    let json = extract_json(text)?;
    let envelope: ExportEnvelope = serde_json::from_str(&json)
        .map_err(|error| format!("invalid baseline JSON in {source_name}: {error}"))?;
    if envelope.schema_version.is_some_and(|version| {
        version != SUPERNOTE_EXPORT_SCHEMA_VERSION
            && version != LEGACY_SINGLE_PAGE_EXPORT_SCHEMA_VERSION
    }) {
        return Err(format!(
            "unsupported Supernote export schema {} in {source_name}",
            envelope.schema_version.unwrap_or_default()
        ));
    }
    if envelope
        .source_file_name
        .as_deref()
        .is_some_and(|name| name.trim().is_empty())
    {
        return Err(format!(
            "Supernote export {source_name} contains an empty sourceFileName"
        ));
    }

    let legacy_page = match (envelope.page_index, envelope.strokes) {
        (Some(page_index), Some(strokes)) => Some(ExportPage {
            page_index,
            strokes,
        }),
        (None, None) => None,
        _ => {
            return Err(format!(
                "Supernote export {source_name} must provide pageIndex and strokes together"
            ))
        }
    };
    let pages = match (legacy_page, envelope.pages) {
        (Some(page), None) => vec![page],
        (None, Some(pages)) => {
            if envelope.schema_version != Some(SUPERNOTE_EXPORT_SCHEMA_VERSION) {
                return Err(format!(
                    "multi-page Supernote export {source_name} must declare schemaVersion {SUPERNOTE_EXPORT_SCHEMA_VERSION}"
                ));
            }
            if pages.is_empty() {
                return Err(format!(
                    "multi-page Supernote export {source_name} contains no page snapshots"
                ));
            }
            pages
        }
        (Some(_), Some(_)) => {
            return Err(format!(
                "Supernote export {source_name} cannot contain both a legacy page and pages"
            ))
        }
        (None, None) => {
            return Err(format!(
                "Supernote export {source_name} contains no page snapshot"
            ))
        }
    };

    let mut seen_pages = BTreeSet::new();
    let mut seen_strokes = BTreeSet::new();
    let mut parsed_pages = pages
        .into_iter()
        .map(|page| {
            if !seen_pages.insert(page.page_index) {
                return Err(format!(
                    "Supernote export {source_name} repeats pageIndex {}",
                    page.page_index
                ));
            }
            let strokes = page
                .strokes
                .into_iter()
                .map(|stroke| {
                    let source_uuid = stroke.source_uuid.unwrap_or(stroke.source_key);
                    if source_uuid.trim().is_empty() {
                        return Err(format!(
                            "baseline {source_name} contains a stroke without an identity"
                        ));
                    }
                    if !seen_strokes.insert(source_uuid.clone()) {
                        return Err(format!(
                            "Supernote export {source_name} repeats stroke identity {source_uuid}"
                        ));
                    }
                    if stroke.samples.len() < 2 {
                        return Err(format!(
                            "baseline stroke {source_uuid} contains fewer than two samples"
                        ));
                    }
                    if !stroke.samples.iter().all(|[x, y, pressure]| {
                        x.is_finite()
                            && y.is_finite()
                            && pressure.is_finite()
                            && (0.0..=1.0).contains(x)
                            && (0.0..=1.0).contains(y)
                            && (0.0..=4096.0).contains(pressure)
                    }) {
                        return Err(format!(
                            "baseline stroke {source_uuid} contains an invalid normalized sample"
                        ));
                    }
                    let native_style = NativeStyle {
                        layer_num: stroke.layer_num,
                        thickness: stroke.thickness,
                        pen_color: stroke.pen_color,
                        pen_type: stroke.pen_type,
                    };
                    let geometry_fingerprint = geometry_fingerprint(&native_style, &stroke.samples);
                    Ok(StrokeSnapshot {
                        source_uuid,
                        origin: "supernote-native".to_owned(),
                        page_index: page.page_index,
                        native_style,
                        samples: stroke.samples,
                        geometry_fingerprint,
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok(BaselinePage {
                page_index: page.page_index,
                strokes,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    parsed_pages.sort_by_key(|page| page.page_index);
    Ok(BaselineExport {
        source_file_name: envelope.source_file_name,
        document_id: envelope.document_id,
        based_on: envelope.based_on,
        pages: parsed_pages,
    })
}

fn extract_json(text: &str) -> Result<String, String> {
    let trimmed = text.trim();
    if trimmed.starts_with('{') {
        return Ok(trimmed.to_owned());
    }

    let mut expected_total = None;
    let mut chunks = BTreeMap::<usize, String>::new();
    for line in text.lines() {
        let Some(marker) = line.find("INKBRIDGE_EXPORT ") else {
            continue;
        };
        let remainder = &line[marker + "INKBRIDGE_EXPORT ".len()..];
        let Some((sequence, chunk)) = remainder.split_once(' ') else {
            continue;
        };
        let Some((index, total)) = sequence.split_once('/') else {
            continue;
        };
        let index = index
            .parse::<usize>()
            .map_err(|_| format!("invalid InkBridge export chunk index: {sequence}"))?;
        let total = total
            .parse::<usize>()
            .map_err(|_| format!("invalid InkBridge export chunk count: {sequence}"))?;
        if let Some(expected) = expected_total {
            if expected != total {
                return Err("InkBridge export log contains inconsistent chunk counts".to_owned());
            }
        } else {
            expected_total = Some(total);
        }
        chunks.insert(index, chunk.to_owned());
    }

    let total = expected_total.ok_or_else(|| {
        "baseline is neither JSON nor a log containing INKBRIDGE_EXPORT chunks".to_owned()
    })?;
    if chunks.len() != total || !(1..=total).all(|index| chunks.contains_key(&index)) {
        return Err(format!(
            "incomplete InkBridge export: found {} of {total} chunks",
            chunks.len()
        ));
    }
    Ok((1..=total).map(|index| chunks[&index].as_str()).collect())
}

pub fn index_baseline(strokes: Vec<StrokeSnapshot>) -> HashMap<String, StrokeSnapshot> {
    strokes
        .into_iter()
        .map(|stroke| (stroke.source_uuid.clone(), stroke))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn reconstructs_numbered_log_chunks() {
        let json = r#"{"pageIndex":0,"strokes":[{"sourceUuid":"abc","sourceKey":"abc","layerNum":0,"thickness":400,"penColor":0,"penType":16,"samples":[[0.1,0.2,1000],[0.2,0.3,1100]]}]}"#;
        let split = json.len() / 2;
        let mut file = tempfile::NamedTempFile::new().unwrap();
        writeln!(
            file,
            "noise\nINKBRIDGE_EXPORT 1/2 {}\nINKBRIDGE_EXPORT 2/2 {}\n",
            &json[..split],
            &json[split..]
        )
        .unwrap();
        let loaded = load_baseline(file.path()).unwrap();
        assert_eq!(loaded.pages.len(), 1);
        assert_eq!(loaded.pages[0].strokes.len(), 1);
        assert_eq!(loaded.pages[0].strokes[0].source_uuid, "abc");
    }

    #[test]
    fn preserves_the_stable_document_identity_when_present() {
        let id = format!("inkbridge-doc-v1-{}", "a".repeat(64));
        let json = format!(
            r#"{{"documentId":"{id}","pageIndex":0,"strokes":[{{"sourceKey":"abc","thickness":400,"penColor":0,"penType":16,"samples":[[0.1,0.2,1000],[0.2,0.3,1100]]}}]}}"#
        );
        let parsed = parse_baseline_bytes(json.as_bytes(), "page-0001.json").unwrap();
        assert_eq!(parsed.document_id.as_deref(), Some(id.as_str()));
    }

    #[test]
    fn preserves_the_export_revision_frontier_when_present() {
        let json = r#"{
            "sourceFileName":"book.pdf",
            "basedOn":{"boox":4,"supernote":7},
            "pageIndex":0,
            "strokes":[]
        }"#;
        let parsed = parse_baseline_bytes(json.as_bytes(), "page-0001.json").unwrap();
        assert_eq!(
            parsed.based_on,
            Some(BaselineRevisions {
                boox: 4,
                supernote: 7,
            })
        );
    }

    #[test]
    fn accepts_the_installed_companions_legacy_schema_two_page_export() {
        let json = r#"{
            "schemaVersion":2,
            "sourceFileName":"book.pdf",
            "pageIndex":0,
            "strokes":[]
        }"#;
        let parsed = parse_baseline_bytes(json.as_bytes(), "page-0001.json").unwrap();
        assert_eq!(parsed.pages.len(), 1);
        assert_eq!(parsed.pages[0].page_index, 0);
        assert!(parsed.pages[0].strokes.is_empty());

        let invalid_batch = r#"{
            "schemaVersion":2,
            "pages":[{"pageIndex":0,"strokes":[]}]
        }"#;
        assert!(parse_baseline_bytes(invalid_batch.as_bytes(), "batch.json")
            .unwrap_err()
            .contains("must declare schemaVersion 1"));
    }

    #[test]
    fn parses_an_atomic_multi_page_snapshot_and_preserves_empty_pages() {
        let json = r#"{
            "schemaVersion":1,
            "sourceFileName":"book.pdf",
            "documentId":"inkbridge-doc-v1-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "basedOn":{"boox":4,"supernote":7},
            "pages":[
                {"pageIndex":143,"strokes":[{
                    "sourceUuid":"right-stroke",
                    "sourceKey":"right-stroke",
                    "thickness":400,
                    "penColor":0,
                    "penType":16,
                    "samples":[[0.1,0.2,1000],[0.2,0.3,1100]]
                }]},
                {"pageIndex":142,"strokes":[]}
            ]
        }"#;
        let parsed = parse_baseline_bytes(json.as_bytes(), "spread-page-72.json").unwrap();
        assert_eq!(
            parsed
                .pages
                .iter()
                .map(|page| page.page_index)
                .collect::<Vec<_>>(),
            vec![142, 143]
        );
        assert!(parsed.pages[0].strokes.is_empty());
        assert_eq!(parsed.pages[1].strokes[0].source_uuid, "right-stroke");
        assert_eq!(
            parsed.based_on,
            Some(BaselineRevisions {
                boox: 4,
                supernote: 7,
            })
        );
    }

    #[test]
    fn rejects_ambiguous_or_incomplete_multi_page_snapshots() {
        let no_schema = r#"{"pages":[{"pageIndex":0,"strokes":[]}]}"#;
        assert!(
            parse_baseline_bytes(no_schema.as_bytes(), "missing-schema.json")
                .unwrap_err()
                .contains("must declare schemaVersion")
        );

        let duplicate_page = r#"{
            "schemaVersion":1,
            "pages":[
                {"pageIndex":0,"strokes":[]},
                {"pageIndex":0,"strokes":[]}
            ]
        }"#;
        assert!(
            parse_baseline_bytes(duplicate_page.as_bytes(), "duplicate-page.json")
                .unwrap_err()
                .contains("repeats pageIndex")
        );

        let both_shapes = r#"{
            "schemaVersion":1,
            "pageIndex":0,
            "strokes":[],
            "pages":[{"pageIndex":1,"strokes":[]}]
        }"#;
        assert!(parse_baseline_bytes(both_shapes.as_bytes(), "both.json")
            .unwrap_err()
            .contains("cannot contain both"));
    }

    #[test]
    fn rejects_duplicate_ids_across_spread_halves() {
        let json = r#"{
            "schemaVersion":1,
            "pages":[
                {"pageIndex":142,"strokes":[{
                    "sourceUuid":"same",
                    "sourceKey":"same",
                    "thickness":400,
                    "penColor":0,
                    "penType":16,
                    "samples":[[0.1,0.2,1000],[0.2,0.3,1100]]
                }]},
                {"pageIndex":143,"strokes":[{
                    "sourceUuid":"same",
                    "sourceKey":"same",
                    "thickness":400,
                    "penColor":0,
                    "penType":16,
                    "samples":[[0.3,0.4,1000],[0.4,0.5,1100]]
                }]}
            ]
        }"#;
        assert!(
            parse_baseline_bytes(json.as_bytes(), "duplicate-stroke.json")
                .unwrap_err()
                .contains("repeats stroke identity same")
        );
    }

    #[test]
    fn standalone_page_serialization_round_trips_batch_metadata_and_geometry() {
        let json = r#"{
            "schemaVersion":1,
            "sourceFileName":"book.pdf",
            "documentId":"inkbridge-doc-v1-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "basedOn":{"boox":4,"supernote":7},
            "pages":[
                {"pageIndex":142,"strokes":[]},
                {"pageIndex":143,"strokes":[{
                    "sourceUuid":"right-stroke",
                    "sourceKey":"right-stroke",
                    "thickness":400,
                    "penColor":0,
                    "penType":16,
                    "samples":[[0.1,0.2,1000],[0.2,0.3,1100]]
                }]}
            ]
        }"#;
        let batch = parse_baseline_bytes(json.as_bytes(), "batch.json").unwrap();
        let bytes = serialize_baseline_page(&batch, &batch.pages[1]).unwrap();
        let page = parse_baseline_bytes(&bytes, "page.json").unwrap();
        assert_eq!(page.source_file_name, batch.source_file_name);
        assert_eq!(page.document_id, batch.document_id);
        assert_eq!(page.based_on, batch.based_on);
        assert_eq!(page.pages, vec![batch.pages[1].clone()]);
    }

    #[test]
    fn complete_snapshot_serialization_replaces_only_the_revision_frontier() {
        let json = r#"{
            "schemaVersion":1,
            "sourceFileName":"book.pdf",
            "documentId":"inkbridge-doc-v1-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "basedOn":{"boox":2,"supernote":3},
            "pages":[
                {"pageIndex":142,"strokes":[]},
                {"pageIndex":143,"strokes":[{
                    "sourceUuid":"right-stroke",
                    "sourceKey":"right-stroke",
                    "thickness":400,
                    "penColor":0,
                    "penType":16,
                    "samples":[[0.1,0.2,1000],[0.2,0.3,1100]]
                }]}
            ]
        }"#;
        let original = parse_baseline_bytes(json.as_bytes(), "batch.json").unwrap();
        let rebased = BaselineRevisions {
            boox: 2,
            supernote: 4,
        };
        let bytes = serialize_baseline_export(&original, rebased).unwrap();
        let parsed = parse_baseline_bytes(&bytes, "rebased.json").unwrap();

        assert_eq!(parsed.pages, original.pages);
        assert_eq!(parsed.source_file_name, original.source_file_name);
        assert_eq!(parsed.document_id, original.document_id);
        assert_eq!(parsed.based_on, Some(rebased));
    }
}

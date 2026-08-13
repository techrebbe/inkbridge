use crate::model::{geometry_fingerprint, NativeStyle, StrokeSnapshot};
use serde::Deserialize;
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::Path;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ExportPage {
    #[serde(default)]
    source_file_name: Option<String>,
    #[serde(default)]
    document_id: Option<String>,
    #[serde(default)]
    based_on: Option<BaselineRevisions>,
    page_index: u32,
    strokes: Vec<ExportStroke>,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
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
    pub page_index: u32,
    pub strokes: Vec<StrokeSnapshot>,
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

fn parse_baseline_text(text: &str, source_name: &str) -> Result<BaselineExport, String> {
    let json = extract_json(text)?;
    let page: ExportPage = serde_json::from_str(&json)
        .map_err(|error| format!("invalid baseline JSON in {source_name}: {error}"))?;

    let source_file_name = page.source_file_name.clone();
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
            if stroke.samples.len() < 2 {
                return Err(format!(
                    "baseline stroke {source_uuid} contains fewer than two samples"
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
    Ok(BaselineExport {
        source_file_name,
        document_id: page.document_id,
        based_on: page.based_on,
        page_index: page.page_index,
        strokes,
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
        assert_eq!(loaded.strokes.len(), 1);
        assert_eq!(loaded.strokes[0].source_uuid, "abc");
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
}

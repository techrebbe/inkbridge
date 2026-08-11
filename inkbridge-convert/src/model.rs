use serde::{Deserialize, Serialize};

pub type Sample = [f64; 3];

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct NativeStyle {
    #[serde(default)]
    pub layer_num: i64,
    pub thickness: i64,
    pub pen_color: i64,
    pub pen_type: i64,
}

impl Default for NativeStyle {
    fn default() -> Self {
        Self {
            layer_num: 0,
            thickness: 400,
            pen_color: 0,
            pen_type: 16,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct StrokeSnapshot {
    pub source_uuid: String,
    pub origin: String,
    pub page_index: u32,
    pub native_style: NativeStyle,
    pub samples: Vec<Sample>,
    pub geometry_fingerprint: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum Operation {
    UpsertStroke {
        source_uuid: String,
        page_index: u32,
        #[serde(skip_serializing_if = "Option::is_none")]
        before: Option<StrokeSnapshot>,
        after: StrokeSnapshot,
    },
    DeleteStroke {
        source_uuid: String,
        page_index: u32,
        before: StrokeSnapshot,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DocumentIdentity {
    pub source_file_name: String,
    #[serde(default)]
    pub target_file_names: Vec<String>,
    pub page_count: usize,
    pub pdf_sha256: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CoordinateTransform {
    pub pdf_to_supernote_normalized_y_offset: f64,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Summary {
    pub upserted: usize,
    pub deleted: usize,
    pub unchanged: usize,
    pub skipped: usize,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    pub schema_version: u32,
    pub manifest_id: String,
    pub source: String,
    pub document: DocumentIdentity,
    pub coordinate_transform: CoordinateTransform,
    pub operations: Vec<Operation>,
    pub summary: Summary,
}

pub fn geometry_fingerprint(style: &NativeStyle, samples: &[Sample]) -> String {
    let mut canonical = format!(
        "{}|{}|{}|{}|",
        style.layer_num, style.thickness, style.pen_color, style.pen_type
    );
    for [x, y, pressure] in samples {
        canonical.push_str(&format!(
            "{},{},{};",
            (x * 100_000.0).round() as i64,
            (y * 100_000.0).round() as i64,
            pressure.round() as i64
        ));
    }
    format!("fnv1a32:{:08x}", fnv1a32(canonical.as_bytes()))
}

pub fn fnv1a32(bytes: &[u8]) -> u32 {
    let mut hash = 0x811c9dc5u32;
    for byte in bytes {
        hash ^= u32::from(*byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_is_stable_and_geometry_sensitive() {
        let style = NativeStyle::default();
        let first = geometry_fingerprint(&style, &[[0.1, 0.2, 1000.0]]);
        let same = geometry_fingerprint(&style, &[[0.100_000_01, 0.2, 1000.0]]);
        let changed = geometry_fingerprint(&style, &[[0.11, 0.2, 1000.0]]);
        let changed_layer = geometry_fingerprint(
            &NativeStyle {
                layer_num: 1,
                ..style.clone()
            },
            &[[0.1, 0.2, 1000.0]],
        );
        assert_eq!(first, same);
        assert_ne!(first, changed);
        assert_ne!(first, changed_layer);
    }
}

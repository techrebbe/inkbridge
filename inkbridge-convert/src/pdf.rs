use crate::model::{geometry_fingerprint, NativeStyle, Sample, StrokeSnapshot};
use lopdf::content::Content;
use lopdf::{Dictionary, Document, Object, Stream};
use serde_json::Value;
use std::collections::HashSet;
use std::ffi::OsString;
use std::path::Path;
use std::process::Command;

const MAX_APPEARANCE_BYTES: usize = 32 * 1024 * 1024;

#[derive(Clone, Copy, Debug)]
struct PageGeometry {
    left: f64,
    bottom: f64,
    width: f64,
    height: f64,
    rotation: i64,
}

#[derive(Clone, Copy, Debug)]
struct Matrix {
    a: f64,
    b: f64,
    c: f64,
    d: f64,
    e: f64,
    f: f64,
}

impl Matrix {
    const IDENTITY: Self = Self {
        a: 1.0,
        b: 0.0,
        c: 0.0,
        d: 1.0,
        e: 0.0,
        f: 0.0,
    };

    fn transform(self, x: f64, y: f64) -> (f64, f64) {
        (
            self.a * x + self.c * y + self.e,
            self.b * x + self.d * y + self.f,
        )
    }

    fn then(self, next: Self) -> Self {
        Self {
            a: next.a * self.a + next.c * self.b,
            b: next.b * self.a + next.d * self.b,
            c: next.a * self.c + next.c * self.d,
            d: next.b * self.c + next.d * self.d,
            e: next.a * self.e + next.c * self.f + next.e,
            f: next.b * self.e + next.d * self.f + next.f,
        }
    }
}

pub fn extract_pdf_strokes(
    path: &Path,
) -> Result<(usize, Vec<StrokeSnapshot>, usize, HashSet<u32>), String> {
    let document = load_neoreader_pdf(path)?;
    let pages = document.get_pages();
    let mut strokes = Vec::new();
    let mut skipped = 0usize;
    let mut incomplete_pages = HashSet::new();

    for (page_number, page_id) in &pages {
        let page_index = page_number.saturating_sub(1);
        let geometry = page_geometry(&document, *page_id)?;
        let annotations = document.get_page_annotations(*page_id).map_err(|error| {
            format!(
                "could not read annotations on page {page_number} of {}: {error}",
                path.display()
            )
        })?;

        for annotation in annotations {
            match extract_annotation(&document, annotation, page_index, geometry) {
                Ok(extracted) => strokes.extend(extracted),
                Err(error) => {
                    eprintln!("warning: skipped annotation on page {page_number}: {error}");
                    skipped += 1;
                    incomplete_pages.insert(page_index);
                }
            }
        }
    }
    Ok((pages.len(), strokes, skipped, incomplete_pages))
}

fn load_neoreader_pdf(path: &Path) -> Result<Document, String> {
    let initial_error = match Document::load(path) {
        Ok(document) => return Ok(document),
        Err(error) => error,
    };

    // NeoReader occasionally emits a readable PDF whose incremental xref stream
    // has an incorrect length or omits its own entry. PDF viewers recover it,
    // but lopdf correctly rejects the malformed table. qpdf provides the same
    // lossless structural rewrite we previously had to perform manually.
    let work = tempfile::tempdir().map_err(|error| {
        format!(
            "could not load PDF {}: {initial_error}; could not create repair directory: {error}",
            path.display(),
        )
    })?;
    let repaired = work.path().join("neoreader-repaired.pdf");
    let qpdf: OsString =
        std::env::var_os("INKBRIDGE_QPDF").unwrap_or_else(|| OsString::from("qpdf"));
    let output = Command::new(&qpdf)
        .arg("--warning-exit-0")
        .arg(path)
        .arg(&repaired)
        .output()
        .map_err(|error| {
            format!(
                "could not load PDF {}: {initial_error}; qpdf recovery could not start ({error}). Install qpdf or set INKBRIDGE_QPDF.",
                path.display(),
            )
        })?;
    if !output.status.success() {
        return Err(format!(
            "could not load PDF {}: {initial_error}; qpdf recovery failed: {}",
            path.display(),
            String::from_utf8_lossy(&output.stderr).trim(),
        ));
    }
    eprintln!(
        "warning: NeoReader PDF had an invalid cross-reference table; recovered it with qpdf"
    );
    Document::load(&repaired)
        .map_err(|error| format!("could not load repaired PDF {}: {error}", path.display(),))
}

fn extract_annotation(
    document: &Document,
    annotation: &Dictionary,
    page_index: u32,
    geometry: PageGeometry,
) -> Result<Vec<StrokeSnapshot>, String> {
    let subtype = name(annotation.get(b"Subtype").ok());
    match subtype.as_deref() {
        Some("Ink") => extract_standard_ink(document, annotation, page_index, geometry),
        Some("Stamp") if name(annotation.get(b"Name").ok()).as_deref() == Some("#ONYX-STROKE") => {
            extract_onyx_stroke(document, annotation, page_index, geometry)
                .map(|stroke| vec![stroke])
        }
        _ => Ok(Vec::new()),
    }
}

fn extract_standard_ink(
    document: &Document,
    annotation: &Dictionary,
    page_index: u32,
    geometry: PageGeometry,
) -> Result<Vec<StrokeSnapshot>, String> {
    let base_source_uuid = pdf_string(annotation.get(b"NM").ok())
        .or_else(|| onyx_tag_id(annotation))
        .ok_or_else(|| "standard /Ink annotation has no /NM or Onyx id".to_owned())?;
    let ink_list = resolve(document, annotation.get(b"InkList").map_err(display_error)?)?;
    let paths = ink_list
        .as_array()
        .map_err(|_| "standard /Ink annotation has an invalid /InkList".to_owned())?;
    if paths.is_empty() {
        return Err("standard /Ink annotation has an empty /InkList".to_owned());
    }
    if paths.len() > 1 {
        return Err(
            "standard /Ink annotation groups multiple paths without stable per-path identities"
                .to_owned(),
        );
    }

    let width = annotation
        .get(b"BS")
        .ok()
        .and_then(|value| resolve(document, value).ok())
        .and_then(|value| value.as_dict().ok())
        .and_then(|dict| dict.get(b"W").ok())
        .and_then(number)
        .unwrap_or(1.168_064_48);
    let style = NativeStyle {
        layer_num: 0,
        thickness: width_to_supernote_thickness(width),
        pen_color: annotation_luminance(document, annotation),
        pen_type: 10,
    };
    let pressure = width_to_pressure(width);
    let coordinates = resolve(document, &paths[0])?
        .as_array()
        .map_err(|_| "standard /Ink path is not an array".to_owned())?;
    if coordinates.len() < 4 || coordinates.len() % 2 != 0 {
        return Err("standard /Ink path contains invalid coordinate pairs".to_owned());
    }
    let mut samples = Vec::with_capacity(coordinates.len() / 2);
    for pair in coordinates.chunks_exact(2) {
        let x = number(&pair[0]).ok_or_else(|| "non-numeric /Ink x coordinate".to_owned())?;
        let y = number(&pair[1]).ok_or_else(|| "non-numeric /Ink y coordinate".to_owned())?;
        let (normalized_x, normalized_y) = geometry.normalize(x, y);
        samples.push([normalized_x, normalized_y, pressure]);
    }
    let geometry_fingerprint = geometry_fingerprint(&style, &samples);
    Ok(vec![StrokeSnapshot {
        source_uuid: base_source_uuid,
        origin: "pdf-ink".to_owned(),
        page_index,
        native_style: style,
        samples,
        geometry_fingerprint,
    }])
}

fn extract_onyx_stroke(
    document: &Document,
    annotation: &Dictionary,
    page_index: u32,
    geometry: PageGeometry,
) -> Result<StrokeSnapshot, String> {
    let source_uuid = onyx_tag_id(annotation)
        .or_else(|| pdf_string(annotation.get(b"NM").ok()))
        .ok_or_else(|| "BOOX stroke has no stable Onyx id".to_owned())?;
    let stream = appearance_stream(document, annotation)?;
    let stream_matrix = stream
        .dict
        .get(b"Matrix")
        .ok()
        .and_then(|value| matrix_from_object(document, value).ok())
        .unwrap_or(Matrix::IDENTITY);
    let content_bytes = stream
        .decompressed_content_with_limit(MAX_APPEARANCE_BYTES)
        .map_err(|error| format!("could not decompress BOOX appearance stream: {error}"))?;
    let content = Content::decode(&content_bytes)
        .map_err(|error| format!("could not decode BOOX appearance operations: {error}"))?;

    let mut width = annotation
        .get(b"BS")
        .ok()
        .and_then(|value| resolve(document, value).ok())
        .and_then(|value| value.as_dict().ok())
        .and_then(|dict| dict.get(b"W").ok())
        .and_then(number)
        .unwrap_or(1.168_064_48);
    let mut matrix = stream_matrix;
    let mut stack = Vec::new();
    let mut samples = Vec::new();
    for operation in content.operations {
        match operation.operator.as_str() {
            "q" => stack.push((matrix, width)),
            "Q" => {
                if let Some((saved_matrix, saved_width)) = stack.pop() {
                    matrix = saved_matrix;
                    width = saved_width;
                }
            }
            "cm" if operation.operands.len() == 6 => {
                let values = operation
                    .operands
                    .iter()
                    .map(number)
                    .collect::<Option<Vec<_>>>()
                    .ok_or_else(|| "BOOX appearance has a non-numeric cm matrix".to_owned())?;
                matrix = matrix.then(Matrix {
                    a: values[0],
                    b: values[1],
                    c: values[2],
                    d: values[3],
                    e: values[4],
                    f: values[5],
                });
            }
            "w" if operation.operands.len() == 1 => {
                width = number(&operation.operands[0])
                    .ok_or_else(|| "BOOX appearance has a non-numeric line width".to_owned())?;
            }
            "m" | "l" if operation.operands.len() >= 2 => {
                let x = number(&operation.operands[0])
                    .ok_or_else(|| "BOOX appearance has a non-numeric x coordinate".to_owned())?;
                let y = number(&operation.operands[1])
                    .ok_or_else(|| "BOOX appearance has a non-numeric y coordinate".to_owned())?;
                let (x, y) = matrix.transform(x, y);
                let (normalized_x, normalized_y) = geometry.normalize(x, y);
                push_distinct(
                    &mut samples,
                    [normalized_x, normalized_y, width_to_pressure(width)],
                );
            }
            _ => {}
        }
    }
    if samples.len() < 2 {
        return Err("BOOX appearance stream contains fewer than two centerline points".to_owned());
    }

    let style = NativeStyle {
        layer_num: 0,
        thickness: annotation
            .get(b"BS")
            .ok()
            .and_then(|value| resolve(document, value).ok())
            .and_then(|value| value.as_dict().ok())
            .and_then(|dict| dict.get(b"W").ok())
            .and_then(number)
            .map(width_to_supernote_thickness)
            .unwrap_or(400),
        pen_color: annotation_luminance(document, annotation),
        pen_type: 16,
    };
    let geometry_fingerprint = geometry_fingerprint(&style, &samples);
    Ok(StrokeSnapshot {
        source_uuid,
        origin: "boox-neoreader".to_owned(),
        page_index,
        native_style: style,
        samples,
        geometry_fingerprint,
    })
}

fn appearance_stream<'a>(
    document: &'a Document,
    annotation: &'a Dictionary,
) -> Result<&'a Stream, String> {
    let ap = resolve(document, annotation.get(b"AP").map_err(display_error)?)?
        .as_dict()
        .map_err(|_| "BOOX annotation /AP is not a dictionary".to_owned())?;
    resolve(document, ap.get(b"N").map_err(display_error)?)?
        .as_stream()
        .map_err(|_| "BOOX annotation /AP /N is not a stream".to_owned())
}

fn matrix_from_object(document: &Document, object: &Object) -> Result<Matrix, String> {
    let values = resolve(document, object)?
        .as_array()
        .map_err(|_| "appearance /Matrix is not an array".to_owned())?
        .iter()
        .map(number)
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| "appearance /Matrix contains non-numeric values".to_owned())?;
    if values.len() != 6 {
        return Err("appearance /Matrix does not contain six values".to_owned());
    }
    Ok(Matrix {
        a: values[0],
        b: values[1],
        c: values[2],
        d: values[3],
        e: values[4],
        f: values[5],
    })
}

fn push_distinct(samples: &mut Vec<Sample>, sample: Sample) {
    const EPSILON: f64 = 0.000_000_5;
    if let Some(previous) = samples.last() {
        if (previous[0] - sample[0]).abs() <= EPSILON && (previous[1] - sample[1]).abs() <= EPSILON
        {
            return;
        }
    }
    samples.push(sample);
}

fn annotation_luminance(document: &Document, annotation: &Dictionary) -> i64 {
    let Some(color) = annotation
        .get(b"C")
        .ok()
        .and_then(|value| resolve(document, value).ok())
        .and_then(|value| value.as_array().ok())
    else {
        return 0;
    };
    let channels = color.iter().filter_map(number).collect::<Vec<_>>();
    let luminance = match channels.as_slice() {
        [gray] => *gray,
        [red, green, blue, ..] => 0.2126 * red + 0.7152 * green + 0.0722 * blue,
        _ => 0.0,
    };
    (luminance.clamp(0.0, 1.0) * 255.0).round() as i64
}

fn onyx_tag_id(annotation: &Dictionary) -> Option<String> {
    let tag = pdf_string(annotation.get(b"onyxtag").ok())?;
    let value: Value = serde_json::from_str(&tag).ok()?;
    value.get("id")?.as_str().map(ToOwned::to_owned)
}

fn width_to_supernote_thickness(width: f64) -> i64 {
    ((width / 1.168_064_48) * 400.0).round().clamp(50.0, 4000.0) as i64
}

fn width_to_pressure(width: f64) -> f64 {
    ((width - 0.4) * 2000.0).round().clamp(0.0, 4096.0)
}

fn resolve<'a>(document: &'a Document, object: &'a Object) -> Result<&'a Object, String> {
    match object {
        Object::Reference(id) => document
            .get_object(*id)
            .map_err(|error| format!("could not resolve PDF object {id:?}: {error}")),
        _ => Ok(object),
    }
}

fn number(object: &Object) -> Option<f64> {
    match object {
        Object::Integer(value) => Some(*value as f64),
        Object::Real(value) => Some(f64::from(*value)),
        _ => None,
    }
}

fn name(object: Option<&Object>) -> Option<String> {
    match object? {
        Object::Name(bytes) => Some(String::from_utf8_lossy(bytes).into_owned()),
        _ => None,
    }
}

fn pdf_string(object: Option<&Object>) -> Option<String> {
    match object? {
        Object::String(bytes, _) => Some(String::from_utf8_lossy(bytes).into_owned()),
        _ => None,
    }
}

fn display_error(error: lopdf::Error) -> String {
    error.to_string()
}

fn page_geometry(document: &Document, page_id: lopdf::ObjectId) -> Result<PageGeometry, String> {
    let mut current_id = page_id;
    let mut crop_box = None;
    let mut media_box = None;
    let mut rotation = None;

    loop {
        let dictionary = document.get_dictionary(current_id).map_err(|error| {
            format!("could not resolve page tree object {current_id:?}: {error}")
        })?;
        if crop_box.is_none() {
            crop_box = dictionary
                .get(b"CropBox")
                .ok()
                .and_then(|object| rectangle(document, object).ok());
        }
        if media_box.is_none() {
            media_box = dictionary
                .get(b"MediaBox")
                .ok()
                .and_then(|object| rectangle(document, object).ok());
        }
        if rotation.is_none() {
            rotation = dictionary
                .get(b"Rotate")
                .ok()
                .and_then(number)
                .map(|value| value.round() as i64);
        }
        let Some(parent) = dictionary
            .get(b"Parent")
            .ok()
            .and_then(|object| object.as_reference().ok())
        else {
            break;
        };
        current_id = parent;
    }

    let [left, bottom, right, top] = crop_box
        .or(media_box)
        .ok_or_else(|| "PDF page has no inherited CropBox or MediaBox".to_owned())?;
    let width = right - left;
    let height = top - bottom;
    if width <= 0.0 || height <= 0.0 {
        return Err("PDF page has an invalid page box".to_owned());
    }
    Ok(PageGeometry {
        left,
        bottom,
        width,
        height,
        rotation: rotation.unwrap_or(0).rem_euclid(360),
    })
}

fn rectangle(document: &Document, object: &Object) -> Result<[f64; 4], String> {
    let values = resolve(document, object)?
        .as_array()
        .map_err(|_| "page box is not an array".to_owned())?
        .iter()
        .map(number)
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| "page box contains non-numeric values".to_owned())?;
    if values.len() != 4 {
        return Err("page box does not contain four values".to_owned());
    }
    Ok([values[0], values[1], values[2], values[3]])
}

impl PageGeometry {
    fn normalize(self, x: f64, y: f64) -> (f64, f64) {
        let x = ((x - self.left) / self.width).clamp(0.0, 1.0);
        let y = (1.0 - ((y - self.bottom) / self.height)).clamp(0.0, 1.0);
        match self.rotation {
            90 => (1.0 - y, x),
            180 => (1.0 - x, 1.0 - y),
            270 => (y, 1.0 - x),
            _ => (x, y),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lopdf::{dictionary, Object, Stream};

    fn test_geometry() -> PageGeometry {
        PageGeometry {
            left: 0.0,
            bottom: 0.0,
            width: 600.0,
            height: 800.0,
            rotation: 0,
        }
    }

    #[test]
    fn normalizes_pdf_bottom_left_to_display_top_left() {
        let geometry = test_geometry();
        assert_eq!(geometry.normalize(60.0, 600.0), (0.1, 0.25));
    }

    #[test]
    fn pressure_matches_observed_neoreader_width_formula() {
        assert_eq!(width_to_pressure(0.795), 790.0);
        assert_eq!(width_to_supernote_thickness(1.168_064_48), 400);
    }

    #[test]
    fn reads_standard_ink_list() {
        let document = Document::new();
        let annotation = dictionary! {
            "Subtype" => "Ink",
            "NM" => Object::string_literal("supernote-stroke"),
            "InkList" => vec![
                Object::Array(vec![60.into(), 600.into(), 120.into(), 560.into()])
            ],
            "BS" => dictionary! {"W" => Object::Real(1.168_064_48)},
            "C" => vec![0.into(), 0.into(), 0.into()],
        };
        let strokes = extract_standard_ink(&document, &annotation, 0, test_geometry()).unwrap();
        assert_eq!(strokes.len(), 1);
        let stroke = &strokes[0];
        assert_eq!(stroke.source_uuid, "supernote-stroke");
        assert_eq!(stroke.samples.len(), 2);
        assert!((stroke.samples[0][0] - 0.1).abs() < 0.000_001);
        assert!((stroke.samples[0][1] - 0.25).abs() < 0.000_001);
    }

    #[test]
    fn rejects_grouped_standard_ink_without_per_path_ids() {
        let document = Document::new();
        let annotation = dictionary! {
            "Subtype" => "Ink",
            "NM" => Object::string_literal("grouped-ink"),
            "InkList" => vec![
                Object::Array(vec![60.into(), 600.into(), 120.into(), 560.into()]),
                Object::Array(vec![180.into(), 520.into(), 240.into(), 480.into()]),
            ],
            "BS" => dictionary! {"W" => Object::Real(1.168_064_48)},
            "C" => vec![0.into(), 0.into(), 0.into()],
        };
        let error = extract_standard_ink(&document, &annotation, 0, test_geometry())
            .expect_err("grouped paths cannot be tracked safely by mutable array position");
        assert!(error.contains("without stable per-path identities"));
    }

    #[test]
    fn reads_neoreader_vector_appearance_centerline() {
        let mut document = Document::new();
        let appearance = Stream::new(
            dictionary! {
                "Matrix" => vec![1.into(), 0.into(), 0.into(), 1.into(), 0.into(), 0.into()]
            },
            b".795 w 60 600 m 120 560 l S".to_vec(),
        );
        let appearance_id = document.add_object(appearance);
        let annotation = dictionary! {
            "Subtype" => "Stamp",
            "Name" => "#ONYX-STROKE",
            "onyxtag" => Object::string_literal(
                r#"{"id":"boox-stroke","type":"BrushStroke"}"#
            ),
            "AP" => dictionary! {"N" => appearance_id},
            "BS" => dictionary! {"W" => Object::Real(1.168_064_48)},
            "C" => vec![0.into(), Object::Real(0.69), Object::Real(0.21)],
        };
        let stroke = extract_onyx_stroke(&document, &annotation, 0, test_geometry()).unwrap();
        assert_eq!(stroke.source_uuid, "boox-stroke");
        assert_eq!(stroke.samples.len(), 2);
        assert_eq!(stroke.samples[0][2], 790.0);
        assert!((stroke.samples[1][0] - 0.2).abs() < 0.000_001);
        assert!((stroke.samples[1][1] - 0.3).abs() < 0.000_001);
    }
}

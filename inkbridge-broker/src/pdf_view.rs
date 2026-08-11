use inkbridge_convert::StrokeSnapshot;
use lopdf::{dictionary, Dictionary, Document, Object, ObjectId, Stream};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug)]
struct PageGeometry {
    left: f64,
    bottom: f64,
    width: f64,
    height: f64,
    rotation: i64,
}

impl PageGeometry {
    fn denormalize(self, x: f64, y: f64) -> (f64, f64) {
        let (unrotated_x, unrotated_y) = match self.rotation {
            90 => (y, 1.0 - x),
            180 => (1.0 - x, 1.0 - y),
            270 => (1.0 - y, x),
            _ => (x, y),
        };
        (
            self.left + unrotated_x * self.width,
            self.bottom + (1.0 - unrotated_y) * self.height,
        )
    }
}

pub fn write_boox_view(
    original_pdf: &[u8],
    strokes: impl IntoIterator<Item = StrokeSnapshot>,
) -> Result<Vec<u8>, String> {
    write_boox_view_with_tombstones(original_pdf, strokes, std::iter::empty::<String>())
}

pub fn write_boox_view_with_tombstones(
    original_pdf: &[u8],
    strokes: impl IntoIterator<Item = StrokeSnapshot>,
    tombstones: impl IntoIterator<Item = String>,
) -> Result<Vec<u8>, String> {
    let mut document = Document::load_mem(original_pdf)
        .map_err(|error| format!("could not load immutable original PDF: {error}"))?;
    let pages = document.get_pages();
    let strokes = strokes.into_iter().collect::<Vec<_>>();
    let mut canonical_ids = strokes
        .iter()
        .map(|stroke| stroke.source_uuid.clone())
        .collect::<std::collections::BTreeSet<_>>();
    canonical_ids.extend(tombstones);
    for page_id in pages.values() {
        strip_canonical_ink_annotations(&mut document, *page_id, &canonical_ids)?;
    }
    let mut by_page = BTreeMap::<u32, Vec<StrokeSnapshot>>::new();
    for stroke in strokes {
        if stroke.samples.len() >= 2 {
            by_page.entry(stroke.page_index).or_default().push(stroke);
        }
    }

    for (page_index, mut page_strokes) in by_page {
        let page_number = page_index + 1;
        let page_id = *pages.get(&page_number).ok_or_else(|| {
            format!("stroke targets page {page_number}, but the original PDF has fewer pages")
        })?;
        let geometry = page_geometry(&document, page_id)?;
        page_strokes.sort_by(|left, right| left.source_uuid.cmp(&right.source_uuid));
        for stroke in page_strokes {
            let annotation_id = make_ink_annotation(&mut document, &stroke, geometry)?;
            append_annotation(&mut document, page_id, annotation_id)?;
        }
    }

    document.renumber_objects();
    let mut output = Vec::new();
    document
        .save_to(&mut output)
        .map_err(|error| format!("could not serialize BOOX PDF view: {error}"))?;
    Ok(output)
}

fn strip_canonical_ink_annotations(
    document: &mut Document,
    page_id: ObjectId,
    canonical_ids: &std::collections::BTreeSet<String>,
) -> Result<(), String> {
    let annots = document
        .get_dictionary(page_id)
        .map_err(|error| format!("could not read page annotations: {error}"))?
        .get(b"Annots")
        .ok()
        .cloned();
    let Some(annots) = annots else {
        return Ok(());
    };
    let (array_id, entries) = match annots {
        Object::Reference(id) => (
            Some(id),
            document
                .get_object(id)
                .map_err(|error| format!("could not resolve page annotation array: {error}"))?
                .as_array()
                .map_err(|_| "page /Annots reference is not an array".to_owned())?
                .clone(),
        ),
        Object::Array(entries) => (None, entries),
        _ => return Err("page /Annots is not an array".to_owned()),
    };
    let retained = entries
        .into_iter()
        .filter(|annotation| {
            annotation_stable_id(document, annotation).is_none_or(|id| !canonical_ids.contains(&id))
        })
        .collect::<Vec<_>>();
    if let Some(array_id) = array_id {
        *document
            .get_object_mut(array_id)
            .map_err(|error| format!("could not update page annotation array: {error}"))?
            .as_array_mut()
            .map_err(|_| "page /Annots reference is not an array".to_owned())? = retained;
    } else {
        document
            .get_dictionary_mut(page_id)
            .map_err(|error| format!("could not update page annotations: {error}"))?
            .set("Annots", retained);
    }
    Ok(())
}

fn annotation_stable_id(document: &Document, annotation: &Object) -> Option<String> {
    let dictionary = match annotation {
        Object::Reference(id) => document.get_dictionary(*id).ok(),
        Object::Dictionary(dictionary) => Some(dictionary),
        _ => None,
    }?;
    let is_ink = dictionary
        .get(b"Subtype")
        .is_ok_and(|value| object_name_is(document, value, b"Ink"));
    let is_onyx = dictionary
        .get(b"Subtype")
        .is_ok_and(|value| object_name_is(document, value, b"Stamp"))
        && dictionary
            .get(b"Name")
            .is_ok_and(|value| object_name_is(document, value, b"#ONYX-STROKE"));
    if !is_ink && !is_onyx {
        return None;
    }
    let nm = dictionary
        .get(b"NM")
        .ok()
        .and_then(|value| object_string(document, value));
    let onyx_id = dictionary
        .get(b"onyxtag")
        .ok()
        .and_then(|value| object_string(document, value))
        .and_then(|tag| serde_json::from_str::<serde_json::Value>(&tag).ok())
        .and_then(|tag| tag.get("id")?.as_str().map(ToOwned::to_owned));
    if is_onyx {
        onyx_id.or(nm)
    } else {
        nm.or(onyx_id)
    }
}

fn object_name_is(document: &Document, object: &Object, expected: &[u8]) -> bool {
    match object {
        Object::Name(name) => name == expected,
        Object::Reference(id) => document
            .get_object(*id)
            .is_ok_and(|value| object_name_is(document, value, expected)),
        _ => false,
    }
}

fn object_string(document: &Document, object: &Object) -> Option<String> {
    match object {
        Object::String(bytes, _) => Some(String::from_utf8_lossy(bytes).into_owned()),
        Object::Reference(id) => document
            .get_object(*id)
            .ok()
            .and_then(|value| object_string(document, value)),
        _ => None,
    }
}

fn make_ink_annotation(
    document: &mut Document,
    stroke: &StrokeSnapshot,
    geometry: PageGeometry,
) -> Result<ObjectId, String> {
    let points = stroke
        .samples
        .iter()
        .map(|sample| geometry.denormalize(sample[0], sample[1]))
        .collect::<Vec<_>>();
    let width = ((stroke.native_style.thickness as f64 / 400.0) * 1.168_064_48).clamp(0.25, 12.0);
    let padding = width.max(1.0);
    let min_x = points
        .iter()
        .map(|point| point.0)
        .fold(f64::INFINITY, f64::min)
        - padding;
    let max_x = points
        .iter()
        .map(|point| point.0)
        .fold(f64::NEG_INFINITY, f64::max)
        + padding;
    let min_y = points
        .iter()
        .map(|point| point.1)
        .fold(f64::INFINITY, f64::min)
        - padding;
    let max_y = points
        .iter()
        .map(|point| point.1)
        .fold(f64::NEG_INFINITY, f64::max)
        + padding;
    let gray = (stroke.native_style.pen_color as f64 / 255.0).clamp(0.0, 1.0);

    let mut content = format!("q {gray:.6} {gray:.6} {gray:.6} RG {width:.6} w 1 J 1 j ");
    let (first_x, first_y) = points[0];
    content.push_str(&format!("{first_x:.6} {first_y:.6} m "));
    for (x, y) in &points[1..] {
        content.push_str(&format!("{x:.6} {y:.6} l "));
    }
    content.push_str("S Q\n");
    let appearance = Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Form",
            "FormType" => 1,
            "BBox" => vec![real(min_x), real(min_y), real(max_x), real(max_y)],
            "Matrix" => vec![1.into(), 0.into(), 0.into(), 1.into(), 0.into(), 0.into()],
            "Resources" => Dictionary::new(),
        },
        content.into_bytes(),
    );
    let appearance_id = document.add_object(appearance);
    let ink_path = points
        .iter()
        .flat_map(|(x, y)| [real(*x), real(*y)])
        .collect::<Vec<_>>();
    let annotation = dictionary! {
        "Type" => "Annot",
        "Subtype" => "Ink",
        "Rect" => vec![real(min_x), real(min_y), real(max_x), real(max_y)],
        "InkList" => vec![Object::Array(ink_path)],
        "NM" => Object::string_literal(stroke.source_uuid.clone()),
        "F" => 4,
        "C" => vec![real(gray), real(gray), real(gray)],
        "CA" => real(1.0),
        "BS" => dictionary! {"Type" => "Border", "W" => real(width), "S" => "S"},
        "AP" => dictionary! {"N" => appearance_id},
        "InkBridgeProducer" => Object::string_literal("inkbridge-broker"),
        "InkBridgeFingerprint" => Object::string_literal(stroke.geometry_fingerprint.clone()),
    };
    Ok(document.add_object(annotation))
}

fn append_annotation(
    document: &mut Document,
    page_id: ObjectId,
    annotation_id: ObjectId,
) -> Result<(), String> {
    let annots = document
        .get_dictionary(page_id)
        .ok()
        .and_then(|page| page.get(b"Annots").ok())
        .cloned();
    match annots {
        Some(Object::Reference(array_id)) => document
            .get_object_mut(array_id)
            .map_err(|error| format!("could not resolve page annotation array: {error}"))?
            .as_array_mut()
            .map_err(|_| "page /Annots reference is not an array".to_owned())?
            .push(annotation_id.into()),
        Some(Object::Array(mut array)) => {
            array.push(annotation_id.into());
            document
                .get_dictionary_mut(page_id)
                .map_err(|error| format!("could not update page annotations: {error}"))?
                .set("Annots", array);
        }
        _ => document
            .get_dictionary_mut(page_id)
            .map_err(|error| format!("could not update page annotations: {error}"))?
            .set("Annots", vec![Object::Reference(annotation_id)]),
    }
    Ok(())
}

fn page_geometry(document: &Document, page_id: ObjectId) -> Result<PageGeometry, String> {
    let mut current_id = page_id;
    let mut crop_box = None;
    let mut media_box = None;
    let mut rotation = None;
    loop {
        let dictionary = document
            .get_dictionary(current_id)
            .map_err(|error| format!("could not resolve page tree object: {error}"))?;
        crop_box = crop_box.or_else(|| {
            dictionary
                .get(b"CropBox")
                .ok()
                .and_then(|object| rectangle(document, object).ok())
        });
        media_box = media_box.or_else(|| {
            dictionary
                .get(b"MediaBox")
                .ok()
                .and_then(|object| rectangle(document, object).ok())
        });
        rotation = rotation.or_else(|| dictionary.get(b"Rotate").ok().and_then(number));
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
    if right <= left || top <= bottom {
        return Err("PDF page has an invalid page box".to_owned());
    }
    Ok(PageGeometry {
        left,
        bottom,
        width: right - left,
        height: top - bottom,
        rotation: (rotation.unwrap_or(0.0).round() as i64).rem_euclid(360),
    })
}

fn rectangle(document: &Document, object: &Object) -> Result<[f64; 4], String> {
    let object = match object {
        Object::Reference(id) => document
            .get_object(*id)
            .map_err(|error| format!("could not resolve page box: {error}"))?,
        value => value,
    };
    let values = object
        .as_array()
        .map_err(|_| "page box is not an array".to_owned())?
        .iter()
        .map(number)
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| "page box contains non-numeric values".to_owned())?;
    values
        .try_into()
        .map_err(|_| "page box does not contain four values".to_owned())
}

fn number(object: &Object) -> Option<f64> {
    match object {
        Object::Integer(value) => Some(*value as f64),
        Object::Real(value) => Some(f64::from(*value)),
        _ => None,
    }
}

fn real(value: f64) -> Object {
    Object::Real(value as f32)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn original_with_ink_and_highlight() -> Vec<u8> {
        let mut document = Document::with_version("1.7");
        let pages_id = document.new_object_id();
        let ink_id = document.add_object(dictionary! {
            "Type" => "Annot",
            "Subtype" => "Ink",
            "NM" => Object::string_literal("original-ink"),
            "InkList" => vec![Object::Array(vec![10.into(), 10.into(), 20.into(), 20.into()])],
        });
        let anonymous_ink_id = document.add_object(dictionary! {
            "Type" => "Annot",
            "Subtype" => "Ink",
            "InkList" => vec![Object::Array(vec![30.into(), 30.into(), 40.into(), 40.into()])],
        });
        let highlight_id = document.add_object(dictionary! {
            "Type" => "Annot",
            "Subtype" => "Highlight",
            "NM" => Object::string_literal("keep-highlight"),
            "Rect" => vec![10.into(), 10.into(), 20.into(), 20.into()],
        });
        let annots_id = document.add_object(Object::Array(vec![
            ink_id.into(),
            anonymous_ink_id.into(),
            highlight_id.into(),
        ]));
        let page_id = document.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
            "Resources" => dictionary! {},
            "Annots" => annots_id,
        });
        document.objects.insert(
            pages_id,
            dictionary! {
                "Type" => "Pages",
                "Kids" => vec![page_id.into()],
                "Count" => 1,
            }
            .into(),
        );
        let catalog_id = document.add_object(dictionary! {
            "Type" => "Catalog",
            "Pages" => pages_id,
        });
        document.trailer.set("Root", catalog_id);
        let mut bytes = Vec::new();
        document.save_to(&mut bytes).unwrap();
        bytes
    }

    fn annotation_subtypes(bytes: &[u8]) -> Vec<String> {
        let document = Document::load_mem(bytes).unwrap();
        let page_id = *document.get_pages().get(&1).unwrap();
        document
            .get_page_annotations(page_id)
            .unwrap()
            .into_iter()
            .map(|annotation| {
                String::from_utf8_lossy(annotation.get(b"Subtype").unwrap().as_name().unwrap())
                    .into_owned()
            })
            .collect()
    }

    #[test]
    fn generated_view_replaces_original_ink_but_preserves_other_annotations() {
        let original = original_with_ink_and_highlight();
        let without_canonical = write_boox_view(&original, []).unwrap();
        assert_eq!(
            annotation_subtypes(&without_canonical),
            ["Ink", "Ink", "Highlight"]
        );

        let tombstoned =
            write_boox_view_with_tombstones(&original, [], ["original-ink".to_owned()]).unwrap();
        assert_eq!(annotation_subtypes(&tombstoned), ["Ink", "Highlight"]);

        let style = inkbridge_convert::NativeStyle::default();
        let samples = vec![[0.2, 0.3, 900.0], [0.3, 0.4, 1000.0]];
        let canonical = StrokeSnapshot {
            source_uuid: "original-ink".to_owned(),
            origin: "canonical".to_owned(),
            page_index: 0,
            geometry_fingerprint: inkbridge_convert::geometry_fingerprint(&style, &samples),
            native_style: style,
            samples,
        };
        let with_canonical = write_boox_view(&original, [canonical]).unwrap();
        assert_eq!(
            annotation_subtypes(&with_canonical),
            ["Ink", "Highlight", "Ink"]
        );
    }

    #[test]
    fn rotated_geometry_round_trips_display_coordinates() {
        for rotation in [0, 90, 180, 270] {
            let geometry = PageGeometry {
                left: 10.0,
                bottom: 20.0,
                width: 600.0,
                height: 800.0,
                rotation,
            };
            let (x, y) = geometry.denormalize(0.2, 0.3);
            let raw_x = (x - geometry.left) / geometry.width;
            let raw_y = 1.0 - (y - geometry.bottom) / geometry.height;
            let normalized = match rotation {
                90 => (1.0 - raw_y, raw_x),
                180 => (1.0 - raw_x, 1.0 - raw_y),
                270 => (raw_y, 1.0 - raw_x),
                _ => (raw_x, raw_y),
            };
            assert!((normalized.0 - 0.2).abs() < 0.000_001);
            assert!((normalized.1 - 0.3).abs() < 0.000_001);
        }
    }

    #[test]
    fn page_geometry_normalizes_negative_rotation() {
        let mut document = Document::new();
        let page_id = document.add_object(dictionary! {
            "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
            "Rotate" => -90,
        });
        let geometry = page_geometry(&document, page_id).unwrap();
        assert_eq!(geometry.rotation, 270);
    }
}

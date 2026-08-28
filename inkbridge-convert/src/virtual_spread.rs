use crate::{AffinePoint, AffineTransform};
use serde::de::{self, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fmt::Formatter;

mod golden;
mod production;

pub use golden::{
    verify_virtual_spread_golden_fixture, VirtualSpreadGoldenVerification,
    VIRTUAL_SPREAD_PAGE_143_FIXTURE_SHA256,
};
pub use production::{
    verify_virtual_spread_page_143_production_fixture, VirtualSpreadProductionVerification,
    VIRTUAL_SPREAD_PAGE_143_ARTIFACT_DESCRIPTOR_SHA256,
    VIRTUAL_SPREAD_PAGE_143_GENERATED_PDF_SHA256, VIRTUAL_SPREAD_PAGE_143_PDF_TAIL_SHA256,
    VIRTUAL_SPREAD_PAGE_143_SIDECAR_SHA256, VIRTUAL_SPREAD_PAGE_143_SOURCE_PDF_SHA256,
};

pub const VIRTUAL_SPREAD_SCHEMA: &str = "techrebbe.supernote.virtual-spread/v3";
pub const VIRTUAL_SPREAD_GENERATOR_VERSION: &str =
    "techrebbe.supernote.virtual-spread-generator/v1";
pub const VIRTUAL_SPREAD_MAPPING_DOMAIN: &str = "techrebbe.supernote.virtual-spread-mapping/v1";
pub const VIRTUAL_SPREAD_VIEW_DOMAIN: &str = "techrebbe.supernote.virtual-spread-view/v1";
pub const VIRTUAL_SPREAD_GOLDEN_SCHEMA: &str = "techrebbe.supernote.virtual-spread-golden/v1";
pub const VIRTUAL_SPREAD_CONTRACT_TOLERANCE: f64 = 1.0e-12;
pub const VIRTUAL_SPREAD_PRODUCTION_ACTIVATION_ENABLED: bool = false;

const DOCUMENT_ID_PREFIX: &str = "inkbridge-doc-v1-";
const VIEW_ID_PREFIX: &str = "inkbridge-view-v1-";
const MAX_PAGE_INDEX: u32 = i32::MAX as u32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VirtualSpreadSide {
    Left,
    Right,
}

impl VirtualSpreadSide {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Left => "left",
            Self::Right => "right",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct VirtualSpreadMapping {
    pub source_page_index: u32,
    pub virtual_page_index: u32,
    pub side: VirtualSpreadSide,
    pub source_rotation: u16,
    pub source_box: [f64; 4],
    pub normalized_source_box: [f64; 4],
    pub slot: [f64; 4],
    pub destination: [f64; 4],
    pub scale: f64,
    pub forward: AffineTransform,
}

#[derive(Clone, Debug, PartialEq)]
pub struct VirtualSpreadManifest {
    pub original_pdf_sha256: String,
    pub document_id: String,
    pub original_page_count: u32,
    pub generated_pdf_sha256: String,
    pub generated_page_count: u32,
    pub spread_size: [f64; 2],
    pub gutter: f64,
    pub cover_separate: bool,
    pub layout_authority_sha256: String,
    pub link_authority_sha256: String,
    pub mapping_authority_sha256: String,
    pub view_id: String,
    pub cache_basename: String,
    pub mappings: Vec<VirtualSpreadMapping>,
}

impl VirtualSpreadManifest {
    pub fn mapping_for_source_page(&self, page_index: u32) -> Option<&VirtualSpreadMapping> {
        self.mappings
            .get(page_index as usize)
            .filter(|mapping| mapping.source_page_index == page_index)
    }
}

impl VirtualSpreadMapping {
    pub fn canonical_to_spread(&self, point: AffinePoint) -> Result<AffinePoint, String> {
        validate_normalized_point(point)?;
        self.forward
            .apply(self.canonical_to_original(point))
            .map_err(|error| format!("Virtual Spread forward transform failed: {error}"))
    }

    pub fn spread_to_canonical(&self, point: AffinePoint) -> Result<AffinePoint, String> {
        let original = self
            .forward
            .inverse()
            .and_then(|inverse| inverse.apply(point))
            .map_err(|error| format!("Virtual Spread inverse transform failed: {error}"))?;
        let canonical = clamp_normalized(self.original_to_canonical(original))?;
        let reproduced = self.canonical_to_spread(canonical)?;
        if !within(reproduced.x, point.x) || !within(reproduced.y, point.y) {
            return Err(format!(
                "Virtual Spread inverse round trip exceeded {VIRTUAL_SPREAD_CONTRACT_TOLERANCE}"
            ));
        }
        Ok(canonical)
    }

    fn canonical_to_original(&self, point: AffinePoint) -> AffinePoint {
        let [left, bottom, right, top] = self.source_box;
        let width = right - left;
        let height = top - bottom;
        match self.source_rotation {
            0 => AffinePoint::new(left + point.x * width, top - point.y * height),
            90 => AffinePoint::new(left + point.y * width, bottom + point.x * height),
            180 => AffinePoint::new(right - point.x * width, bottom + point.y * height),
            270 => AffinePoint::new(right - point.y * width, top - point.x * height),
            _ => unreachable!("validated rotation"),
        }
    }

    fn original_to_canonical(&self, point: AffinePoint) -> AffinePoint {
        let [left, bottom, right, top] = self.source_box;
        let width = right - left;
        let height = top - bottom;
        match self.source_rotation {
            0 => AffinePoint::new((point.x - left) / width, (top - point.y) / height),
            90 => AffinePoint::new((point.y - bottom) / height, (point.x - left) / width),
            180 => AffinePoint::new((right - point.x) / width, (point.y - bottom) / height),
            270 => AffinePoint::new((top - point.y) / height, (right - point.x) / width),
            _ => unreachable!("validated rotation"),
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ManifestWire {
    schema: String,
    source: SourceWire,
    output: OutputWire,
    generator_version: String,
    direction: String,
    cover_separate: bool,
    spreads: Vec<SpreadWire>,
    source_pages: Vec<MappingWire>,
    links: Vec<Value>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SourceWire {
    name: String,
    path: String,
    size: u64,
    sha256: String,
    page_count: u32,
    document_id: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct OutputWire {
    name: String,
    path: String,
    size: u64,
    sha256: String,
    page_count: u32,
    spread_size: [f64; 2],
    gutter: f64,
    layout_authority_sha256: String,
    link_authority_sha256: String,
    mapping_authority_sha256: String,
    view_id: String,
    cache_basename: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SpreadWire {
    virtual_page_index: u32,
    virtual_page_number: u32,
    left: Option<MappingWire>,
    right: Option<MappingWire>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct MappingWire {
    source_page_index: u32,
    #[serde(default)]
    source_page_number: Option<u32>,
    virtual_page_index: u32,
    #[serde(default)]
    virtual_page_number: Option<u32>,
    side: String,
    source_rotation: u16,
    source_box: [f64; 4],
    normalized_source_box: [f64; 4],
    slot: [f64; 4],
    destination: [f64; 4],
    scale: f64,
    transform: [f64; 6],
}

/// Parse and validate the schema-v3 sidecar against the immutable original.
///
/// The returned mappings are safe for fixture conversion and hydration planning,
/// but production cache activation remains deliberately disabled until the shared
/// native hydration, rollback, and idempotent-reimport hardware gate passes.
pub fn parse_virtual_spread_manifest(
    bytes: &[u8],
    expected_original_pdf_sha256: &str,
) -> Result<VirtualSpreadManifest, String> {
    validate_sha256(
        expected_original_pdf_sha256,
        "expected original PDF SHA-256",
    )?;
    let value = strict_json_value(bytes)?;
    let wire: ManifestWire = serde_json::from_value(value)
        .map_err(|error| format!("invalid Virtual Spread schema-v3 manifest: {error}"))?;
    validate_wire(wire, expected_original_pdf_sha256)
}

pub fn stable_supernote_annotation_id(
    document_id: &str,
    retained_source_uuid: Option<&str>,
    native_source_key: &str,
) -> Result<String, String> {
    validate_document_id(document_id)?;
    if let Some(source_uuid) = retained_source_uuid {
        let source_uuid = source_uuid.trim();
        if !source_uuid.is_empty() {
            return Ok(source_uuid.to_owned());
        }
    }
    let native_source_key = native_source_key.trim();
    if native_source_key.is_empty() {
        return Err(
            "Supernote annotation has neither retained sourceUuid nor a stable native source key"
                .to_owned(),
        );
    }
    let seed = format!("{document_id}\0supernote-native-element/v1\0{native_source_key}");
    Ok(format!("inkbridge-sn-v1-{}", hex_sha256(seed.as_bytes())))
}

fn validate_wire(
    wire: ManifestWire,
    expected_original_pdf_sha256: &str,
) -> Result<VirtualSpreadManifest, String> {
    if wire.schema != VIRTUAL_SPREAD_SCHEMA {
        return Err(format!("unsupported Virtual Spread schema {}", wire.schema));
    }
    if wire.generator_version != VIRTUAL_SPREAD_GENERATOR_VERSION {
        return Err(format!(
            "unsupported Virtual Spread generator {}",
            wire.generator_version
        ));
    }
    if wire.direction != "rtl" {
        return Err("Virtual Spread direction must be rtl".to_owned());
    }
    if wire.source.sha256 != expected_original_pdf_sha256 {
        return Err(
            "Virtual Spread source SHA-256 does not match the immutable original".to_owned(),
        );
    }
    validate_sha256(&wire.source.sha256, "manifest source SHA-256")?;
    validate_sha256(&wire.output.sha256, "generated PDF SHA-256")?;
    validate_sha256(
        &wire.output.layout_authority_sha256,
        "layout authority SHA-256",
    )?;
    validate_sha256(&wire.output.link_authority_sha256, "link authority SHA-256")?;
    validate_sha256(
        &wire.output.mapping_authority_sha256,
        "mapping authority SHA-256",
    )?;
    let document_id = format!("{DOCUMENT_ID_PREFIX}{}", wire.source.sha256);
    if wire.source.document_id != document_id {
        return Err("Virtual Spread documentId is not derived from the original PDF".to_owned());
    }
    validate_document_id(&wire.source.document_id)?;
    if wire.source.page_count == 0 || wire.source.page_count > MAX_PAGE_INDEX {
        return Err(
            "Virtual Spread source pageCount is outside the supported int32 range".to_owned(),
        );
    }
    if wire.source.size == 0 || wire.output.size == 0 {
        return Err("Virtual Spread source and output sizes must be positive".to_owned());
    }
    validate_page_index(wire.output.page_count, "output pageCount")?;
    if wire.output.page_count == 0 || wire.spreads.len() != wire.output.page_count as usize {
        return Err("Virtual Spread output pageCount does not match spreads".to_owned());
    }
    if wire.source_pages.len() != wire.source.page_count as usize {
        return Err("Virtual Spread source pageCount does not match sourcePages".to_owned());
    }
    validate_spread_geometry(wire.output.spread_size, wire.output.gutter)?;
    validate_links(&wire.links, wire.source.page_count, wire.output.page_count)?;

    let mut mappings = Vec::with_capacity(wire.source_pages.len());
    let mut canonical_records = Vec::with_capacity(wire.source_pages.len());
    for (index, mapping) in wire.source_pages.iter().enumerate() {
        if mapping.source_page_index as usize != index {
            return Err(
                "Virtual Spread sourcePages must contain every zero-based page in order".to_owned(),
            );
        }
        let parsed = validate_mapping(
            mapping,
            wire.cover_separate,
            wire.output.spread_size,
            wire.output.gutter,
        )?;
        canonical_records.push(canonical_mapping_record(mapping)?);
        mappings.push(parsed);
    }
    validate_spreads(&wire.spreads, &wire.source_pages, wire.cover_separate)?;

    let mapping_authority_sha256 = mapping_authority(&canonical_records);
    if wire.output.mapping_authority_sha256 != mapping_authority_sha256 {
        return Err("Virtual Spread mapping authority does not match sourcePages".to_owned());
    }
    let view_id = compute_view_id(
        &wire.source.sha256,
        wire.cover_separate,
        wire.output.spread_size,
        wire.output.gutter,
        &mapping_authority_sha256,
    );
    if wire.output.view_id != view_id {
        return Err(
            "Virtual Spread viewId does not match authenticated representation inputs".to_owned(),
        );
    }
    let cache_basename = format!("{document_id}.{view_id}.virtual-spread.pdf");
    if wire.output.cache_basename != cache_basename {
        return Err(
            "Virtual Spread cacheBasename does not match document/view identity".to_owned(),
        );
    }

    Ok(VirtualSpreadManifest {
        original_pdf_sha256: wire.source.sha256,
        document_id,
        original_page_count: wire.source.page_count,
        generated_pdf_sha256: wire.output.sha256,
        generated_page_count: wire.output.page_count,
        spread_size: wire.output.spread_size,
        gutter: wire.output.gutter,
        cover_separate: wire.cover_separate,
        layout_authority_sha256: wire.output.layout_authority_sha256,
        link_authority_sha256: wire.output.link_authority_sha256,
        mapping_authority_sha256,
        view_id,
        cache_basename,
        mappings,
    })
}

fn validate_mapping(
    mapping: &MappingWire,
    cover_separate: bool,
    spread_size: [f64; 2],
    gutter: f64,
) -> Result<VirtualSpreadMapping, String> {
    validate_page_index(mapping.source_page_index, "sourcePageIndex")?;
    validate_page_index(mapping.virtual_page_index, "virtualPageIndex")?;
    if mapping
        .source_page_number
        .is_some_and(|number| number != mapping.source_page_index.saturating_add(1))
        || mapping
            .virtual_page_number
            .is_some_and(|number| number != mapping.virtual_page_index.saturating_add(1))
    {
        return Err(
            "Virtual Spread display page number disagrees with zero-based index".to_owned(),
        );
    }
    let side = match mapping.side.as_str() {
        "left" => VirtualSpreadSide::Left,
        "right" => VirtualSpreadSide::Right,
        _ => return Err("Virtual Spread side must be left or right".to_owned()),
    };
    if !mapping_placement_matches(
        mapping.source_page_index,
        mapping.virtual_page_index,
        side,
        cover_separate,
    ) {
        return Err("Virtual Spread page placement disagrees with RTL cover parity".to_owned());
    }
    if !matches!(mapping.source_rotation, 0 | 90 | 180 | 270) {
        return Err("Virtual Spread sourceRotation is not a quarter turn".to_owned());
    }
    validate_rect(mapping.source_box, "sourceBox")?;
    validate_rect(mapping.normalized_source_box, "normalizedSourceBox")?;
    validate_rect(mapping.slot, "slot")?;
    validate_rect(mapping.destination, "destination")?;
    if !mapping.scale.is_finite() || mapping.scale <= 0.0 {
        return Err("Virtual Spread scale must be finite and positive".to_owned());
    }
    if !mapping.transform.iter().all(|value| value.is_finite()) {
        return Err("Virtual Spread transform contains a non-finite number".to_owned());
    }
    validate_source_dimensions(mapping)?;
    validate_slot_and_destination(mapping, side, spread_size, gutter)?;
    validate_linear_orientation(mapping)?;
    let forward = AffineTransform::new(mapping.transform)
        .map_err(|error| format!("invalid Virtual Spread transform: {error}"))?;
    let parsed = VirtualSpreadMapping {
        source_page_index: mapping.source_page_index,
        virtual_page_index: mapping.virtual_page_index,
        side,
        source_rotation: mapping.source_rotation,
        source_box: mapping.source_box,
        normalized_source_box: mapping.normalized_source_box,
        slot: mapping.slot,
        destination: mapping.destination,
        scale: mapping.scale,
        forward,
    };
    validate_transformed_bounds(&parsed)?;
    for point in [
        AffinePoint::new(0.0, 0.0),
        AffinePoint::new(1.0, 0.0),
        AffinePoint::new(0.0, 1.0),
        AffinePoint::new(1.0, 1.0),
        AffinePoint::new(0.5, 0.5),
        AffinePoint::new(0.123_456_789, 0.876_543_211),
    ] {
        let spread = parsed.canonical_to_spread(point)?;
        let recovered = parsed.spread_to_canonical(spread)?;
        if !within(recovered.x, point.x) || !within(recovered.y, point.y) {
            return Err("Virtual Spread canonical round trip is numerically unstable".to_owned());
        }
    }
    Ok(parsed)
}

fn validate_source_dimensions(mapping: &MappingWire) -> Result<(), String> {
    let source_width = mapping.source_box[2] - mapping.source_box[0];
    let source_height = mapping.source_box[3] - mapping.source_box[1];
    let normalized_width = mapping.normalized_source_box[2] - mapping.normalized_source_box[0];
    let normalized_height = mapping.normalized_source_box[3] - mapping.normalized_source_box[1];
    let (expected_width, expected_height) = match mapping.source_rotation {
        0 | 180 => (source_width, source_height),
        90 | 270 => (source_height, source_width),
        _ => unreachable!("validated rotation"),
    };
    if !within(normalized_width, expected_width) || !within(normalized_height, expected_height) {
        return Err(
            "Virtual Spread normalizedSourceBox dimensions disagree with rotation".to_owned(),
        );
    }
    Ok(())
}

fn validate_spread_geometry(spread_size: [f64; 2], gutter: f64) -> Result<(), String> {
    if !spread_size
        .iter()
        .all(|value| value.is_finite() && *value > 0.0)
        || !gutter.is_finite()
        || gutter < 0.0
        || gutter >= spread_size[0]
    {
        return Err("Virtual Spread output geometry is invalid".to_owned());
    }
    Ok(())
}

fn validate_slot_and_destination(
    mapping: &MappingWire,
    side: VirtualSpreadSide,
    spread_size: [f64; 2],
    gutter: f64,
) -> Result<(), String> {
    let slot_width = (spread_size[0] - gutter) / 2.0;
    let expected_slot = match side {
        VirtualSpreadSide::Left => [0.0, 0.0, slot_width, spread_size[1]],
        VirtualSpreadSide::Right => [slot_width + gutter, 0.0, spread_size[0], spread_size[1]],
    };
    if !rect_within(mapping.slot, expected_slot) {
        return Err("Virtual Spread slot is not the declared target half".to_owned());
    }
    let normalized_width = mapping.normalized_source_box[2] - mapping.normalized_source_box[0];
    let normalized_height = mapping.normalized_source_box[3] - mapping.normalized_source_box[1];
    let expected_scale = (slot_width / normalized_width).min(spread_size[1] / normalized_height);
    if !within(mapping.scale, expected_scale) {
        return Err("Virtual Spread scale is not the uniform slot fit".to_owned());
    }
    let width = normalized_width * mapping.scale;
    let height = normalized_height * mapping.scale;
    let expected_destination = [
        expected_slot[0] + (slot_width - width) / 2.0,
        (spread_size[1] - height) / 2.0,
        expected_slot[0] + (slot_width + width) / 2.0,
        (spread_size[1] + height) / 2.0,
    ];
    if !rect_within(mapping.destination, expected_destination) {
        return Err("Virtual Spread destination is not centered in its slot".to_owned());
    }
    Ok(())
}

fn validate_linear_orientation(mapping: &MappingWire) -> Result<(), String> {
    let s = mapping.scale;
    let expected = match mapping.source_rotation {
        0 => [s, 0.0, 0.0, s],
        90 => [0.0, -s, s, 0.0],
        180 => [-s, 0.0, 0.0, -s],
        270 => [0.0, s, -s, 0.0],
        _ => unreachable!("validated rotation"),
    };
    let actual = [
        mapping.transform[0],
        mapping.transform[1],
        mapping.transform[2],
        mapping.transform[3],
    ];
    // These coefficients are generated from the same parsed binary64 scale.
    // Exact bits prevent an absolute epsilon from accepting a missing axis at
    // tiny scales; literal zero and signed orientation remain authoritative.
    if !actual
        .iter()
        .zip(expected)
        .all(|(actual, expected)| actual.to_bits() == expected.to_bits())
    {
        return Err(
            "Virtual Spread transform contains skew, reflection, or wrong rotation".to_owned(),
        );
    }
    Ok(())
}

fn validate_transformed_bounds(mapping: &VirtualSpreadMapping) -> Result<(), String> {
    let [left, bottom, right, top] = mapping.source_box;
    let corners = [
        AffinePoint::new(left, bottom),
        AffinePoint::new(left, top),
        AffinePoint::new(right, bottom),
        AffinePoint::new(right, top),
    ];
    let transformed = corners
        .into_iter()
        .map(|point| {
            mapping
                .forward
                .apply(point)
                .map_err(|error| error.to_string())
        })
        .collect::<Result<Vec<_>, _>>()?;
    let bounds = [
        transformed
            .iter()
            .map(|point| point.x)
            .fold(f64::INFINITY, f64::min),
        transformed
            .iter()
            .map(|point| point.y)
            .fold(f64::INFINITY, f64::min),
        transformed
            .iter()
            .map(|point| point.x)
            .fold(f64::NEG_INFINITY, f64::max),
        transformed
            .iter()
            .map(|point| point.y)
            .fold(f64::NEG_INFINITY, f64::max),
    ];
    if !rect_within(bounds, mapping.destination) {
        return Err("Virtual Spread transform corners do not bound destination".to_owned());
    }
    Ok(())
}

fn validate_spreads(
    spreads: &[SpreadWire],
    mappings: &[MappingWire],
    cover_separate: bool,
) -> Result<(), String> {
    for (index, spread) in spreads.iter().enumerate() {
        if spread.virtual_page_index as usize != index
            || spread.virtual_page_number != spread.virtual_page_index.saturating_add(1)
        {
            return Err("Virtual Spread spreads are not sequential and zero-based".to_owned());
        }
        for (side, candidate) in [
            (VirtualSpreadSide::Left, spread.left.as_ref()),
            (VirtualSpreadSide::Right, spread.right.as_ref()),
        ] {
            let expected = mappings.iter().find(|mapping| {
                mapping.virtual_page_index == spread.virtual_page_index
                    && mapping.side == side.as_str()
            });
            if candidate != expected {
                return Err("Virtual Spread spread/sourcePages mapping mismatch".to_owned());
            }
        }
    }
    let expected_pages = if cover_separate {
        1 + mappings.len().saturating_sub(1).div_ceil(2)
    } else {
        mappings.len().div_ceil(2)
    };
    if spreads.len() != expected_pages {
        return Err("Virtual Spread spread count disagrees with cover parity".to_owned());
    }
    Ok(())
}

fn validate_links(links: &[Value], source_pages: u32, output_pages: u32) -> Result<(), String> {
    for link in links {
        let object = link
            .as_object()
            .ok_or_else(|| "Virtual Spread link must be an object".to_owned())?;
        let kind = exact_string(object, "kind")?;
        let expected: BTreeSet<&str> = match kind {
            "internal" => [
                "sourcePage",
                "sourceSide",
                "outputPage",
                "kind",
                "targetSourcePage",
                "targetOutputPage",
                "targetSide",
                "targetView",
                "rect",
            ]
            .into_iter()
            .collect(),
            "uri" => [
                "sourcePage",
                "sourceSide",
                "outputPage",
                "kind",
                "uri",
                "rect",
            ]
            .into_iter()
            .collect(),
            _ => return Err("Virtual Spread link kind is unsupported".to_owned()),
        };
        let actual = object.keys().map(String::as_str).collect::<BTreeSet<_>>();
        if actual != expected {
            return Err("Virtual Spread link contains missing or unknown fields".to_owned());
        }
        validate_json_index(object, "sourcePage", source_pages)?;
        validate_json_index(object, "outputPage", output_pages)?;
        validate_side(exact_string(object, "sourceSide")?)?;
        validate_json_rect(object.get("rect"), "link rect")?;
        if kind == "internal" {
            validate_json_index(object, "targetSourcePage", source_pages)?;
            validate_json_index(object, "targetOutputPage", output_pages)?;
            validate_side(exact_string(object, "targetSide")?)?;
            match exact_string(object, "targetView")? {
                "fit-source-page" | "preserve" => {}
                _ => return Err("Virtual Spread targetView is unsupported".to_owned()),
            }
        } else if exact_string(object, "uri")?.is_empty() {
            return Err("Virtual Spread URI link is empty".to_owned());
        }
    }
    Ok(())
}

fn canonical_mapping_record(mapping: &MappingWire) -> Result<String, String> {
    validate_page_index(mapping.source_page_index, "sourcePageIndex")?;
    validate_page_index(mapping.virtual_page_index, "virtualPageIndex")?;
    let mut fields = vec![
        "page".to_owned(),
        mapping.source_page_index.to_string(),
        mapping.virtual_page_index.to_string(),
        mapping.side.clone(),
        mapping.source_rotation.to_string(),
    ];
    for value in mapping
        .source_box
        .iter()
        .chain(mapping.normalized_source_box.iter())
        .chain(mapping.slot.iter())
        .chain(mapping.destination.iter())
        .chain(std::iter::once(&mapping.scale))
        .chain(mapping.transform.iter())
    {
        if !value.is_finite() {
            return Err("Virtual Spread mapping contains a non-finite number".to_owned());
        }
        fields.push(format!("{:016x}", value.to_bits()));
    }
    Ok(fields.join("|"))
}

fn mapping_authority(records: &[String]) -> String {
    hex_sha256(canonical_mapping(records).as_bytes())
}

fn canonical_mapping(records: &[String]) -> String {
    let mut canonical = format!("{VIRTUAL_SPREAD_MAPPING_DOMAIN}\n");
    for record in records {
        canonical.push_str(record);
        canonical.push('\n');
    }
    canonical
}

fn compute_view_id(
    source_sha256: &str,
    cover_separate: bool,
    spread_size: [f64; 2],
    gutter: f64,
    mapping_authority_sha256: &str,
) -> String {
    let canonical = canonical_view(
        source_sha256,
        cover_separate,
        spread_size,
        gutter,
        mapping_authority_sha256,
    );
    format!("{VIEW_ID_PREFIX}{}", hex_sha256(canonical.as_bytes()))
}

fn canonical_view(
    source_sha256: &str,
    cover_separate: bool,
    spread_size: [f64; 2],
    gutter: f64,
    mapping_authority_sha256: &str,
) -> String {
    format!(
        "{VIRTUAL_SPREAD_VIEW_DOMAIN}\nsource|{source_sha256}\nschema|{VIRTUAL_SPREAD_SCHEMA}\ngenerator|{VIRTUAL_SPREAD_GENERATOR_VERSION}\ndirection|rtl\ncover|{}\nspread|{:016x}|{:016x}|{:016x}\nmapping|{mapping_authority_sha256}\n",
        u8::from(cover_separate),
        spread_size[0].to_bits(),
        spread_size[1].to_bits(),
        gutter.to_bits(),
    )
}

fn mapping_placement_matches(
    source_page: u32,
    virtual_page: u32,
    side: VirtualSpreadSide,
    cover_separate: bool,
) -> bool {
    if cover_separate && source_page == 0 {
        return virtual_page == 0 && side == VirtualSpreadSide::Right;
    }
    let first_source = u32::from(cover_separate);
    let first_virtual = u32::from(cover_separate);
    let Some(offset) = source_page.checked_sub(first_source) else {
        return false;
    };
    virtual_page == first_virtual + offset / 2
        && side
            == if offset % 2 == 0 {
                VirtualSpreadSide::Right
            } else {
                VirtualSpreadSide::Left
            }
}

fn validate_document_id(value: &str) -> Result<(), String> {
    let Some(hash) = value.strip_prefix(DOCUMENT_ID_PREFIX) else {
        return Err("InkBridge document ID has an unsupported prefix".to_owned());
    };
    validate_sha256(hash, "InkBridge document ID hash")
}

fn validate_sha256(value: &str, label: &str) -> Result<(), String> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(format!("{label} is not lowercase SHA-256"));
    }
    Ok(())
}

fn validate_page_index(value: u32, label: &str) -> Result<(), String> {
    if value > MAX_PAGE_INDEX {
        return Err(format!("{label} exceeds the nonnegative int32 range"));
    }
    Ok(())
}

fn validate_rect(rect: [f64; 4], label: &str) -> Result<(), String> {
    if !rect.iter().all(|value| value.is_finite()) || rect[0] >= rect[2] || rect[1] >= rect[3] {
        return Err(format!(
            "Virtual Spread {label} is not a positive finite rectangle"
        ));
    }
    Ok(())
}

fn rect_within(actual: [f64; 4], expected: [f64; 4]) -> bool {
    actual
        .iter()
        .zip(expected)
        .all(|(actual, expected)| within(*actual, expected))
}

fn within(left: f64, right: f64) -> bool {
    within_tolerance(left, right, VIRTUAL_SPREAD_CONTRACT_TOLERANCE)
}

fn within_tolerance(left: f64, right: f64, tolerance: f64) -> bool {
    left.is_finite()
        && right.is_finite()
        && tolerance.is_finite()
        && tolerance >= 0.0
        && (left - right).abs() <= tolerance
}

fn validate_normalized_point(point: AffinePoint) -> Result<(), String> {
    if !point.x.is_finite()
        || !point.y.is_finite()
        || !(0.0..=1.0).contains(&point.x)
        || !(0.0..=1.0).contains(&point.y)
    {
        return Err("canonical annotation point is outside normalized [0,1] bounds".to_owned());
    }
    Ok(())
}

fn clamp_normalized(point: AffinePoint) -> Result<AffinePoint, String> {
    fn clamp(value: f64) -> Result<f64, String> {
        if !value.is_finite()
            || !(-VIRTUAL_SPREAD_CONTRACT_TOLERANCE..=1.0 + VIRTUAL_SPREAD_CONTRACT_TOLERANCE)
                .contains(&value)
        {
            return Err(
                "inverse Virtual Spread point is outside normalized [0,1] bounds".to_owned(),
            );
        }
        Ok(if value.abs() <= VIRTUAL_SPREAD_CONTRACT_TOLERANCE {
            0.0
        } else if (value - 1.0).abs() <= VIRTUAL_SPREAD_CONTRACT_TOLERANCE {
            1.0
        } else {
            value
        })
    }
    Ok(AffinePoint::new(clamp(point.x)?, clamp(point.y)?))
}

fn validate_side(value: &str) -> Result<(), String> {
    if matches!(value, "left" | "right") {
        Ok(())
    } else {
        Err("Virtual Spread link side must be left or right".to_owned())
    }
}

fn exact_string<'a>(object: &'a Map<String, Value>, key: &str) -> Result<&'a str, String> {
    object
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("Virtual Spread {key} must be a string"))
}

fn validate_json_index(
    object: &Map<String, Value>,
    key: &str,
    exclusive_maximum: u32,
) -> Result<(), String> {
    let value = object
        .get(key)
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("Virtual Spread {key} must be a nonnegative integer"))?;
    if value > u64::from(MAX_PAGE_INDEX) || value >= u64::from(exclusive_maximum) {
        return Err(format!("Virtual Spread {key} is out of range"));
    }
    Ok(())
}

fn validate_json_rect(value: Option<&Value>, label: &str) -> Result<(), String> {
    let array = value
        .and_then(Value::as_array)
        .ok_or_else(|| format!("Virtual Spread {label} must be an array"))?;
    if array.len() != 4 {
        return Err(format!("Virtual Spread {label} must contain four numbers"));
    }
    let mut rect = [0.0; 4];
    for (index, value) in array.iter().enumerate() {
        rect[index] = value
            .as_f64()
            .filter(|value| value.is_finite())
            .ok_or_else(|| format!("Virtual Spread {label} contains a non-finite number"))?;
    }
    validate_rect(rect, label)
}

fn hex_sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn strict_json_value(bytes: &[u8]) -> Result<Value, String> {
    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    let StrictValue(value) = StrictValue::deserialize(&mut deserializer)
        .map_err(|error| format!("invalid strict JSON: {error}"))?;
    deserializer
        .end()
        .map_err(|error| format!("invalid trailing JSON data: {error}"))?;
    Ok(value)
}

struct StrictValue(Value);

impl<'de> Deserialize<'de> for StrictValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_any(StrictValueVisitor)
    }
}

struct StrictValueVisitor;

impl<'de> Visitor<'de> for StrictValueVisitor {
    type Value = StrictValue;

    fn expecting(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a JSON value without duplicate object keys")
    }

    fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::Bool(value)))
    }

    fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::Number(value.into())))
    }

    fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::Number(value.into())))
    }

    fn visit_f64<E>(self, value: f64) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        let number = serde_json::Number::from_f64(value)
            .ok_or_else(|| E::custom("non-finite JSON number"))?;
        Ok(StrictValue(Value::Number(number)))
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::String(value.to_owned())))
    }

    fn visit_string<E>(self, value: String) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::String(value)))
    }

    fn visit_none<E>(self) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::Null))
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::Null))
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut values = Vec::new();
        while let Some(StrictValue(value)) = sequence.next_element()? {
            values.push(value);
        }
        Ok(StrictValue(Value::Array(values)))
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut values = Map::new();
        while let Some(key) = map.next_key::<String>()? {
            if values.contains_key(&key) {
                return Err(de::Error::custom(format!(
                    "duplicate JSON object key {key}"
                )));
            }
            let StrictValue(value) = map.next_value()?;
            values.insert(key, value);
        }
        Ok(StrictValue(Value::Object(values)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HASH: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn mapping() -> MappingWire {
        MappingWire {
            source_page_index: 0,
            source_page_number: Some(1),
            virtual_page_index: 0,
            virtual_page_number: Some(1),
            side: "right".to_owned(),
            source_rotation: 90,
            source_box: [18.0, 36.0, 594.0, 756.0],
            normalized_source_box: [36.0, 18.0, 756.0, 594.0],
            slot: [432.0, 0.0, 864.0, 648.0],
            destination: [432.0, 151.2, 864.0, 496.8],
            scale: 0.6,
            transform: [0.0, -0.6, 0.6, 0.0, 410.4, 507.6],
        }
    }

    fn manifest_value() -> Value {
        let mapping = mapping();
        let canonical = canonical_mapping_record(&mapping).unwrap();
        let authority = mapping_authority(&[canonical]);
        let view_id = compute_view_id(HASH, true, [864.0, 648.0], 0.0, &authority);
        serde_json::json!({
            "schema": VIRTUAL_SPREAD_SCHEMA,
            "source": {
                "name": "fixture.pdf", "path": "diagnostic-only", "size": 100,
                "sha256": HASH, "pageCount": 1,
                "documentId": format!("{DOCUMENT_ID_PREFIX}{HASH}")
            },
            "output": {
                "name": "staging.pdf", "path": "diagnostic-only", "size": 200,
                "sha256": "a".repeat(64), "pageCount": 1,
                "spreadSize": [864.0, 648.0], "gutter": 0.0,
                "layoutAuthoritySha256": "b".repeat(64),
                "linkAuthoritySha256": "c".repeat(64),
                "mappingAuthoritySha256": authority,
                "viewId": view_id,
                "cacheBasename": format!("{DOCUMENT_ID_PREFIX}{HASH}.{view_id}.virtual-spread.pdf")
            },
            "generatorVersion": VIRTUAL_SPREAD_GENERATOR_VERSION,
            "direction": "rtl", "coverSeparate": true,
            "spreads": [{
                "virtualPageIndex": 0, "virtualPageNumber": 1,
                "left": null, "right": mapping
            }],
            "sourcePages": [mapping],
            "links": []
        })
    }

    fn manifest_bytes() -> Vec<u8> {
        serde_json::to_vec(&manifest_value()).unwrap()
    }

    #[test]
    fn parses_authenticated_schema_v3_scaffolding_and_round_trips_points() {
        let parsed = parse_virtual_spread_manifest(&manifest_bytes(), HASH).unwrap();
        assert_eq!(parsed.document_id, format!("{DOCUMENT_ID_PREFIX}{HASH}"));
        let mapping = parsed.mapping_for_source_page(0).unwrap();
        for point in [
            AffinePoint::new(0.0, 0.0),
            AffinePoint::new(0.25, 0.5),
            AffinePoint::new(1.0, 1.0),
        ] {
            let spread = mapping.canonical_to_spread(point).unwrap();
            let recovered = mapping.spread_to_canonical(spread).unwrap();
            assert!(within(point.x, recovered.x));
            assert!(within(point.y, recovered.y));
        }
    }

    #[test]
    fn rejects_duplicate_keys_unknown_fields_and_non_integer_indices() {
        let duplicate = br#"{"schema":"a","schema":"b"}"#;
        assert!(strict_json_value(duplicate)
            .unwrap_err()
            .contains("duplicate"));

        let mut unknown = manifest_value();
        unknown["unexpected"] = Value::Bool(true);
        assert!(
            parse_virtual_spread_manifest(&serde_json::to_vec(&unknown).unwrap(), HASH)
                .unwrap_err()
                .contains("unknown field")
        );

        let mut non_integer = manifest_value();
        non_integer["sourcePages"][0]["sourcePageIndex"] = Value::from(0.5);
        assert!(
            parse_virtual_spread_manifest(&serde_json::to_vec(&non_integer).unwrap(), HASH)
                .is_err()
        );
    }

    #[test]
    fn rejects_wrong_document_identity_digest_and_geometry() {
        let mut wrong_identity = manifest_value();
        wrong_identity["source"]["documentId"] =
            Value::String(format!("{DOCUMENT_ID_PREFIX}{}", "f".repeat(64)));
        assert!(
            parse_virtual_spread_manifest(&serde_json::to_vec(&wrong_identity).unwrap(), HASH)
                .unwrap_err()
                .contains("documentId")
        );

        let mut wrong_digest = manifest_value();
        wrong_digest["output"]["mappingAuthoritySha256"] = Value::String("f".repeat(64));
        assert!(
            parse_virtual_spread_manifest(&serde_json::to_vec(&wrong_digest).unwrap(), HASH)
                .unwrap_err()
                .contains("mapping authority")
        );

        let mut reflected = manifest_value();
        reflected["sourcePages"][0]["transform"][1] = Value::from(0.6);
        reflected["spreads"][0]["right"]["transform"][1] = Value::from(0.6);
        assert!(
            parse_virtual_spread_manifest(&serde_json::to_vec(&reflected).unwrap(), HASH)
                .unwrap_err()
                .contains("reflection")
        );
    }

    #[test]
    fn diagnostic_host_names_and_paths_do_not_change_authority() {
        let original = parse_virtual_spread_manifest(&manifest_bytes(), HASH).unwrap();
        let mut renamed = manifest_value();
        renamed["source"]["name"] = Value::String("renamed-source.pdf".to_owned());
        renamed["source"]["path"] = Value::String("/different/diagnostic/source".to_owned());
        renamed["output"]["name"] = Value::String("temporary-staging-name.pdf".to_owned());
        renamed["output"]["path"] = Value::String("D:\\temporary\\diagnostic".to_owned());
        let renamed =
            parse_virtual_spread_manifest(&serde_json::to_vec(&renamed).unwrap(), HASH).unwrap();

        assert_eq!(renamed.document_id, original.document_id);
        assert_eq!(
            renamed.mapping_authority_sha256,
            original.mapping_authority_sha256
        );
        assert_eq!(renamed.view_id, original.view_id);
        assert_eq!(renamed.cache_basename, original.cache_basename);
        assert_eq!(renamed.mappings, original.mappings);
    }

    #[test]
    fn tiny_scale_orientation_cannot_collapse_into_absolute_tolerance() {
        let mut tiny = mapping();
        tiny.scale = 1.0e-100;
        tiny.transform = [0.0, -tiny.scale, tiny.scale, 0.0, 0.0, 0.0];
        validate_linear_orientation(&tiny).unwrap();

        tiny.transform[1] = 0.0;
        assert!(validate_linear_orientation(&tiny)
            .unwrap_err()
            .contains("wrong rotation"));
    }

    #[test]
    fn far_offset_mapping_is_rejected_when_binary64_round_trip_is_unstable() {
        let mut far = MappingWire {
            source_page_index: 0,
            source_page_number: Some(1),
            virtual_page_index: 0,
            virtual_page_number: Some(1),
            side: "right".to_owned(),
            source_rotation: 0,
            source_box: [1.0e15, 0.0, 1.0e15 + 720.0, 576.0],
            normalized_source_box: [1.0e15, 0.0, 1.0e15 + 720.0, 576.0],
            slot: [432.0, 0.0, 864.0, 648.0],
            destination: [432.0, 151.2, 864.0, 496.8],
            scale: 0.6,
            transform: [0.6, 0.0, 0.0, 0.6, 432.0 - 0.6e15, 151.2],
        };
        let error = validate_mapping(&far, true, [864.0, 648.0], 0.0)
            .expect_err("far-offset cancellation must fail closed");
        assert!(error.contains("round trip") || error.contains("destination"));

        far.source_box[0] = 0.0;
        far.source_box[2] = 720.0;
        far.normalized_source_box = far.source_box;
        far.transform[4] = 432.0;
        validate_mapping(&far, true, [864.0, 648.0], 0.0).unwrap();
    }

    #[test]
    fn fallback_identity_is_document_bound_and_move_stable() {
        let document_id = format!("{DOCUMENT_ID_PREFIX}{HASH}");
        let retained =
            stable_supernote_annotation_id(&document_id, Some("kept-id"), "native").unwrap();
        assert_eq!(retained, "kept-id");
        let fallback = stable_supernote_annotation_id(&document_id, None, "native-key").unwrap();
        assert!(fallback.starts_with("inkbridge-sn-v1-"));
        assert_eq!(
            fallback,
            stable_supernote_annotation_id(&document_id, None, "native-key").unwrap()
        );
        assert!(stable_supernote_annotation_id(&document_id, None, "").is_err());
    }
}

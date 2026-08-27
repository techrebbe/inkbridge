use super::*;
use serde::Deserialize;

/// SHA-256 of RTL Reader v0.0.25's frozen synthetic page-143 fixture.
pub const VIRTUAL_SPREAD_PAGE_143_FIXTURE_SHA256: &str =
    "2a47cd7a461bacb9e0b441ca4ab0e6fc720cf927c667d68b1ad6f44a473cf539";

/// Verified, synthetic cross-project evidence for the frozen mapping contract.
///
/// This is not generated-PDF activation authority. Real PDF/sidecar hashes and
/// PDF-tail evidence remain a separate production gate.
#[derive(Clone, Debug, PartialEq)]
pub struct VirtualSpreadGoldenVerification {
    pub logical_case: String,
    pub original_pdf_sha256: String,
    pub document_id: String,
    pub mapping_authority_sha256: String,
    pub view_id: String,
    pub cache_basename: String,
    pub page_143_mapping_index: u32,
    pub mappings: Vec<VirtualSpreadMapping>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GoldenWire {
    schema: String,
    logical_case: String,
    index_base: u32,
    coordinate_system: GoldenCoordinateSystemWire,
    source_sha256: String,
    manifest_schema: String,
    generator_version: String,
    direction: String,
    cover_separate: bool,
    spread_size: [f64; 2],
    gutter: f64,
    page_143_mapping_index: u32,
    mappings: Vec<MappingWire>,
    canonical_mapping: String,
    mapping_authority_sha256: String,
    canonical_view: String,
    document_id: String,
    view_id: String,
    output_basename: String,
    point_round_trips: Vec<GoldenPointRoundTripWire>,
    stroke_round_trip: GoldenStrokeRoundTripWire,
    signed_zero_vectors: GoldenSignedZeroVectorsWire,
    tolerance: f64,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GoldenCoordinateSystemWire {
    space: String,
    origin: String,
    x_axis: String,
    y_axis: String,
    bounds: [f64; 2],
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GoldenPointRoundTripWire {
    normalized: [f64; 2],
    spread: [f64; 2],
    normalized_after_inverse: [f64; 2],
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GoldenStrokeRoundTripWire {
    normalized: Vec<[f64; 2]>,
    spread: Vec<[f64; 2]>,
    normalized_after_inverse: Vec<[f64; 2]>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GoldenSignedZeroVectorsWire {
    mapping_mutation: GoldenMappingMutationWire,
    view_mutation: GoldenViewMutationWire,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GoldenMappingMutationWire {
    mapping_index: u32,
    field: String,
    element_index: u32,
    canonical_record: String,
    mapping_authority_sha256: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GoldenViewMutationWire {
    field: String,
    canonical_bits: String,
    canonical_view: String,
    view_id: String,
}

/// Verify the frozen synthetic cross-project fixture produced by RTL Reader.
///
/// The fixture pins canonical bytes, signed-zero behavior, identities, and
/// point/stroke vectors. It deliberately contains no generated-PDF hash or
/// PDF-tail evidence and therefore cannot enable production cache activation.
pub fn verify_virtual_spread_golden_fixture(
    bytes: &[u8],
) -> Result<VirtualSpreadGoldenVerification, String> {
    if hex_sha256(bytes) != VIRTUAL_SPREAD_PAGE_143_FIXTURE_SHA256 {
        return Err("Virtual Spread frozen page-143 fixture digest mismatch".to_owned());
    }
    let value = strict_json_value(bytes)?;
    let wire: GoldenWire = serde_json::from_value(value)
        .map_err(|error| format!("invalid Virtual Spread golden fixture: {error}"))?;
    validate_golden_wire(wire)
}

fn validate_golden_wire(wire: GoldenWire) -> Result<VirtualSpreadGoldenVerification, String> {
    if wire.schema != VIRTUAL_SPREAD_GOLDEN_SCHEMA
        || wire.logical_case != "page-143"
        || wire.index_base != 0
        || wire.manifest_schema != VIRTUAL_SPREAD_SCHEMA
        || wire.generator_version != VIRTUAL_SPREAD_GENERATOR_VERSION
        || wire.direction != "rtl"
    {
        return Err("Virtual Spread golden fixture identity is unsupported".to_owned());
    }
    if wire.coordinate_system.space != "displayed-cropbox-normalized"
        || wire.coordinate_system.origin != "top-left"
        || wire.coordinate_system.x_axis != "right"
        || wire.coordinate_system.y_axis != "down"
        || wire.coordinate_system.bounds[0].to_bits() != 0.0f64.to_bits()
        || wire.coordinate_system.bounds[1].to_bits() != 1.0f64.to_bits()
    {
        return Err("Virtual Spread golden coordinate system is unsupported".to_owned());
    }
    if wire.tolerance.to_bits() != VIRTUAL_SPREAD_CONTRACT_TOLERANCE.to_bits() {
        return Err(
            "Virtual Spread golden tolerance does not match the frozen contract".to_owned(),
        );
    }
    validate_sha256(&wire.source_sha256, "golden source SHA-256")?;
    validate_sha256(
        &wire.mapping_authority_sha256,
        "golden mapping authority SHA-256",
    )?;
    validate_spread_geometry(wire.spread_size, wire.gutter)?;
    if wire.mappings.is_empty() || wire.mappings.len() > MAX_PAGE_INDEX as usize {
        return Err("Virtual Spread golden mappings are outside the int32 range".to_owned());
    }

    let mut mappings = Vec::with_capacity(wire.mappings.len());
    let mut canonical_records = Vec::with_capacity(wire.mappings.len());
    for (index, mapping) in wire.mappings.iter().enumerate() {
        if mapping.source_page_index as usize != index {
            return Err(
                "Virtual Spread golden mappings must contain every source index in order"
                    .to_owned(),
            );
        }
        mappings.push(validate_mapping(
            mapping,
            wire.cover_separate,
            wire.spread_size,
            wire.gutter,
        )?);
        canonical_records.push(canonical_mapping_record(mapping)?);
    }

    validate_page_index(wire.page_143_mapping_index, "golden page143MappingIndex")?;
    let selected_index = wire.page_143_mapping_index as usize;
    let selected = mappings
        .get(selected_index)
        .ok_or_else(|| "golden page143MappingIndex does not identify a mapping".to_owned())?;
    if selected.source_page_index != wire.page_143_mapping_index {
        return Err("golden page-143 mapping index disagrees with source page index".to_owned());
    }

    let canonical_mapping = canonical_mapping(&canonical_records);
    if wire.canonical_mapping != canonical_mapping {
        let first_difference = wire
            .canonical_mapping
            .bytes()
            .zip(canonical_mapping.bytes())
            .position(|(expected, actual)| expected != actual)
            .unwrap_or_else(|| wire.canonical_mapping.len().min(canonical_mapping.len()));
        let expected_byte = wire
            .canonical_mapping
            .as_bytes()
            .get(first_difference)
            .copied();
        let actual_byte = canonical_mapping.as_bytes().get(first_difference).copied();
        return Err(format!(
            "Virtual Spread canonical mapping bytes differ from the frozen fixture at byte {first_difference} (expected {expected_byte:?}, got {actual_byte:?}; computed SHA-256 {})",
            hex_sha256(canonical_mapping.as_bytes())
        ));
    }
    let mapping_authority_sha256 = hex_sha256(canonical_mapping.as_bytes());
    if wire.mapping_authority_sha256 != mapping_authority_sha256 {
        return Err(
            "Virtual Spread golden mapping authority does not match canonical bytes".to_owned(),
        );
    }

    let canonical_view = canonical_view(
        &wire.source_sha256,
        wire.cover_separate,
        wire.spread_size,
        wire.gutter,
        &mapping_authority_sha256,
    );
    if wire.canonical_view != canonical_view {
        return Err(
            "Virtual Spread canonical view bytes differ from the frozen fixture".to_owned(),
        );
    }
    let view_id = format!("{VIEW_ID_PREFIX}{}", hex_sha256(canonical_view.as_bytes()));
    if wire.view_id != view_id {
        return Err("Virtual Spread golden view ID does not match canonical bytes".to_owned());
    }
    let document_id = format!("{DOCUMENT_ID_PREFIX}{}", wire.source_sha256);
    if wire.document_id != document_id {
        return Err("Virtual Spread golden document ID is not original-PDF derived".to_owned());
    }
    let cache_basename = format!("{document_id}.{view_id}.virtual-spread.pdf");
    if wire.output_basename != cache_basename {
        return Err("Virtual Spread golden cache basename is not document/view derived".to_owned());
    }

    if wire.point_round_trips.is_empty() {
        return Err("Virtual Spread golden fixture has no point round trips".to_owned());
    }
    for vector in &wire.point_round_trips {
        validate_golden_round_trip(selected, vector, wire.tolerance)?;
    }
    let stroke = &wire.stroke_round_trip;
    if stroke.normalized.len() < 2
        || stroke.spread.len() != stroke.normalized.len()
        || stroke.normalized_after_inverse.len() != stroke.normalized.len()
    {
        return Err("Virtual Spread golden stroke vectors have inconsistent lengths".to_owned());
    }
    for index in 0..stroke.normalized.len() {
        validate_golden_round_trip(
            selected,
            &GoldenPointRoundTripWire {
                normalized: stroke.normalized[index],
                spread: stroke.spread[index],
                normalized_after_inverse: stroke.normalized_after_inverse[index],
            },
            wire.tolerance,
        )?;
    }

    validate_signed_zero_vectors(&wire, &canonical_records)?;

    Ok(VirtualSpreadGoldenVerification {
        logical_case: wire.logical_case,
        original_pdf_sha256: wire.source_sha256,
        document_id,
        mapping_authority_sha256,
        view_id,
        cache_basename,
        page_143_mapping_index: wire.page_143_mapping_index,
        mappings,
    })
}

fn validate_golden_round_trip(
    mapping: &VirtualSpreadMapping,
    vector: &GoldenPointRoundTripWire,
    tolerance: f64,
) -> Result<(), String> {
    let normalized = AffinePoint::new(vector.normalized[0], vector.normalized[1]);
    let expected_spread = AffinePoint::new(vector.spread[0], vector.spread[1]);
    let expected_inverse = AffinePoint::new(
        vector.normalized_after_inverse[0],
        vector.normalized_after_inverse[1],
    );
    let spread = mapping.canonical_to_spread(normalized)?;
    if !within_tolerance(spread.x, expected_spread.x, tolerance)
        || !within_tolerance(spread.y, expected_spread.y, tolerance)
    {
        return Err("Virtual Spread golden forward vector mismatch".to_owned());
    }
    let inverse = mapping.spread_to_canonical(expected_spread)?;
    if !within_tolerance(inverse.x, expected_inverse.x, tolerance)
        || !within_tolerance(inverse.y, expected_inverse.y, tolerance)
        || !within_tolerance(expected_inverse.x, normalized.x, tolerance)
        || !within_tolerance(expected_inverse.y, normalized.y, tolerance)
    {
        return Err("Virtual Spread golden inverse vector mismatch".to_owned());
    }
    Ok(())
}

fn validate_signed_zero_vectors(
    wire: &GoldenWire,
    canonical_records: &[String],
) -> Result<(), String> {
    let mutation = &wire.signed_zero_vectors.mapping_mutation;
    validate_page_index(mutation.mapping_index, "signed-zero mappingIndex")?;
    if mutation.field != "slot" || mutation.element_index != 0 {
        return Err("Virtual Spread signed-zero mapping vector is unsupported".to_owned());
    }
    let mutation_index = mutation.mapping_index as usize;
    let mut mutated_mapping = wire
        .mappings
        .get(mutation_index)
        .cloned()
        .ok_or_else(|| "signed-zero mappingIndex does not identify a mapping".to_owned())?;
    mutated_mapping.slot[0] = f64::from_bits(1_u64 << 63);
    validate_mapping(
        &mutated_mapping,
        wire.cover_separate,
        wire.spread_size,
        wire.gutter,
    )?;
    let mutated_record = canonical_mapping_record(&mutated_mapping)?;
    if mutation.canonical_record != mutated_record {
        return Err("Virtual Spread signed-zero mapping bits were not preserved".to_owned());
    }
    let mut mutated_records = canonical_records.to_vec();
    mutated_records[mutation_index] = mutated_record;
    if mutation.mapping_authority_sha256 != mapping_authority(&mutated_records) {
        return Err("Virtual Spread signed-zero mapping authority mismatch".to_owned());
    }

    let view_mutation = &wire.signed_zero_vectors.view_mutation;
    if view_mutation.field != "gutter" || view_mutation.canonical_bits != "8000000000000000" {
        return Err("Virtual Spread signed-zero view vector is unsupported".to_owned());
    }
    let negative_zero = f64::from_bits(1_u64 << 63);
    let mutated_view = canonical_view(
        &wire.source_sha256,
        wire.cover_separate,
        wire.spread_size,
        negative_zero,
        &wire.mapping_authority_sha256,
    );
    if view_mutation.canonical_view != mutated_view {
        return Err("Virtual Spread signed-zero view bits were not preserved".to_owned());
    }
    let mutated_view_id = format!("{VIEW_ID_PREFIX}{}", hex_sha256(mutated_view.as_bytes()));
    if view_mutation.view_id != mutated_view_id {
        return Err("Virtual Spread signed-zero view ID mismatch".to_owned());
    }
    Ok(())
}

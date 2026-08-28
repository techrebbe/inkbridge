use inkbridge_convert::{
    verify_virtual_spread_page_143_production_fixture, AffinePoint,
    VIRTUAL_SPREAD_CONTRACT_TOLERANCE,
};

const SOURCE: &[u8] = include_bytes!("fixtures/virtual-spread/page-143-v1/page-143-source-v1.pdf");
const GENERATED: &[u8] =
    include_bytes!("fixtures/virtual-spread/page-143-v1/page-143-virtual-spread-v1.pdf");
const SIDECAR: &[u8] =
    include_bytes!("fixtures/virtual-spread/page-143-v1/page-143-virtual-spread-v1.pdf.json");
const DESCRIPTOR: &[u8] =
    include_bytes!("fixtures/virtual-spread/page-143-v1/page-143-artifacts-v1.json");
const PDF_TAIL: &[u8] =
    include_bytes!("fixtures/virtual-spread/page-143-v1/page-143-pdf-tail-authorities-v1.txt");

#[test]
fn exact_real_pdf_bundle_is_accepted_as_cross_project_authority() {
    let verified = verify_virtual_spread_page_143_production_fixture(
        SOURCE, GENERATED, SIDECAR, DESCRIPTOR, PDF_TAIL,
    )
    .unwrap();

    assert_eq!(verified.authority_block_offset(), 5_844);
    assert_eq!(verified.startxref_offset(), 6_312);
    assert_eq!(verified.manifest().original_page_count, 3);
    assert_eq!(verified.manifest().generated_page_count, 2);
    assert_eq!(
        verified.manifest().view_id,
        "inkbridge-view-v1-7cb2c2fda17d5510d33b0a97e702cbc66d5124735be45f810aef6053c1775f30"
    );
    assert_eq!(
        verified.manifest().cache_basename,
        "inkbridge-doc-v1-c9271098e6d98f7fff378c4d630dc9c179cf45cb5283f3559eee910e3afafeb4.inkbridge-view-v1-7cb2c2fda17d5510d33b0a97e702cbc66d5124735be45f810aef6053c1775f30.virtual-spread.pdf"
    );
}

#[test]
fn real_page_143_vectors_round_trip_through_locally_derived_inverse() {
    let verified = verify_virtual_spread_page_143_production_fixture(
        SOURCE, GENERATED, SIDECAR, DESCRIPTOR, PDF_TAIL,
    )
    .unwrap();
    let mapping = verified.manifest().mapping_for_source_page(2).unwrap();
    let vectors = [
        ([0.0, 0.0], [0.0, 496.8], [0.0, 3.0839528461809905e-17]),
        (
            [0.25, 0.5],
            [108.0, 324.0],
            [0.24999999999999997, 0.5000000000000001],
        ),
        (
            [1.0, 1.0],
            [431.99999999999994, 151.20000000000005],
            [0.9999999999999999, 1.0],
        ),
    ];
    for (canonical, expected_spread, expected_inverse) in vectors {
        let spread = mapping
            .canonical_to_spread(AffinePoint::new(canonical[0], canonical[1]))
            .unwrap();
        assert_close(spread.x, expected_spread[0]);
        assert_close(spread.y, expected_spread[1]);
        let inverse = mapping
            .spread_to_canonical(AffinePoint::new(expected_spread[0], expected_spread[1]))
            .unwrap();
        assert_close(inverse.x, expected_inverse[0]);
        assert_close(inverse.y, expected_inverse[1]);
    }

    let normalized = [[0.1, 0.2], [0.5, 0.5], [0.9, 0.8]];
    let expected_spread = [
        [43.2, 427.68000000000006],
        [216.0, 324.0],
        [388.79999999999995, 220.32000000000005],
    ];
    let expected_inverse = [
        [0.09999999999999998, 0.19999999999999987],
        [0.5, 0.5000000000000001],
        [0.9, 0.7999999999999998],
    ];
    for index in 0..normalized.len() {
        let spread = mapping
            .canonical_to_spread(AffinePoint::new(normalized[index][0], normalized[index][1]))
            .unwrap();
        assert_close(spread.x, expected_spread[index][0]);
        assert_close(spread.y, expected_spread[index][1]);
        let inverse = mapping.spread_to_canonical(spread).unwrap();
        assert_close(inverse.x, expected_inverse[index][0]);
        assert_close(inverse.y, expected_inverse[index][1]);
    }
}

#[test]
fn every_real_fixture_authority_fails_closed_when_altered() {
    for index in 0..5 {
        let mut source = SOURCE.to_vec();
        let mut generated = GENERATED.to_vec();
        let mut sidecar = SIDECAR.to_vec();
        let mut descriptor = DESCRIPTOR.to_vec();
        let mut tail = PDF_TAIL.to_vec();
        match index {
            0 => source[0] ^= 1,
            1 => generated[0] ^= 1,
            2 => sidecar[0] ^= 1,
            3 => descriptor[0] ^= 1,
            4 => tail[0] ^= 1,
            _ => unreachable!(),
        }
        assert!(verify_virtual_spread_page_143_production_fixture(
            &source,
            &generated,
            &sidecar,
            &descriptor,
            &tail,
        )
        .is_err());
    }
}

fn assert_close(actual: f64, expected: f64) {
    assert!(
        (actual - expected).abs() <= VIRTUAL_SPREAD_CONTRACT_TOLERANCE,
        "expected {expected:?}, got {actual:?}"
    );
}

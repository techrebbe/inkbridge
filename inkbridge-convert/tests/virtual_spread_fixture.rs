use inkbridge_convert::{parse_virtual_spread_manifest, AffinePoint};

const ORIGINAL_SHA256: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

#[test]
fn external_schema_v3_scaffold_fixture_exercises_transform_harness() {
    let bytes = include_bytes!("fixtures/virtual-spread/scaffold-manifest-v3.json");
    let manifest = parse_virtual_spread_manifest(bytes, ORIGINAL_SHA256).unwrap();
    let mapping = manifest.mapping_for_source_page(0).unwrap();
    let vectors = [
        (AffinePoint::new(0.0, 0.0), AffinePoint::new(432.0, 496.8)),
        (AffinePoint::new(0.25, 0.5), AffinePoint::new(540.0, 324.0)),
        (AffinePoint::new(1.0, 1.0), AffinePoint::new(864.0, 151.2)),
    ];
    for (canonical, expected_spread) in vectors {
        let spread = mapping.canonical_to_spread(canonical).unwrap();
        assert!((spread.x - expected_spread.x).abs() <= 1.0e-12);
        assert!((spread.y - expected_spread.y).abs() <= 1.0e-12);
        let recovered = mapping.spread_to_canonical(spread).unwrap();
        assert!((recovered.x - canonical.x).abs() <= 1.0e-12);
        assert!((recovered.y - canonical.y).abs() <= 1.0e-12);
    }
}

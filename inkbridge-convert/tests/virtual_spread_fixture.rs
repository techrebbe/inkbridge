use inkbridge_convert::{
    verify_virtual_spread_golden_fixture, VirtualSpreadSide, VIRTUAL_SPREAD_PAGE_143_FIXTURE_SHA256,
};
use sha2::{Digest, Sha256};

const FIXTURE: &[u8] = include_bytes!("fixtures/virtual-spread/page-143-contract-v1.json");

#[test]
fn merged_v025_page_143_fixture_matches_frozen_cross_project_bytes() {
    assert_eq!(
        format!("{:x}", Sha256::digest(FIXTURE)),
        VIRTUAL_SPREAD_PAGE_143_FIXTURE_SHA256
    );
    let verified = verify_virtual_spread_golden_fixture(FIXTURE).unwrap();

    assert_eq!(verified.logical_case, "page-143");
    assert_eq!(verified.page_143_mapping_index, 2);
    assert_eq!(verified.mappings.len(), 3);
    let page_143 = &verified.mappings[2];
    assert_eq!(page_143.source_page_index, 2);
    assert_eq!(page_143.virtual_page_index, 1);
    assert_eq!(page_143.side, VirtualSpreadSide::Left);
    assert_eq!(
        verified.mapping_authority_sha256,
        "646b905c12266774882e0c4d7ebbbca77b2f386f432979ebcbfcda1d9ace268a"
    );
    assert_eq!(
        verified.view_id,
        "inkbridge-view-v1-43f3e4f6cafaa07589e7ea1d27ae821d785851a6f26ff0007f3faeb6323c6d74"
    );
}

#[test]
fn frozen_fixture_digest_detects_self_consistent_vector_replacement() {
    let mut replacement: serde_json::Value = serde_json::from_slice(FIXTURE).unwrap();
    replacement["pointRoundTrips"] = serde_json::json!([{
        "normalized": [0.5, 0.5],
        "spread": [216.0, 324.0],
        "normalizedAfterInverse": [0.5, 0.5]
    }]);
    let replacement = serde_json::to_vec(&replacement).unwrap();

    assert_ne!(
        format!("{:x}", Sha256::digest(&replacement)),
        VIRTUAL_SPREAD_PAGE_143_FIXTURE_SHA256
    );
    assert!(verify_virtual_spread_golden_fixture(&replacement)
        .unwrap_err()
        .contains("frozen page-143 fixture digest"));
}

#[test]
fn golden_fixture_verification_fails_closed_on_contract_drift() {
    let mut value: serde_json::Value = serde_json::from_slice(FIXTURE).unwrap();
    value["canonicalMapping"] = serde_json::Value::String("changed".to_owned());
    assert!(
        verify_virtual_spread_golden_fixture(&serde_json::to_vec(&value).unwrap())
            .unwrap_err()
            .contains("frozen page-143 fixture digest")
    );

    let mut value: serde_json::Value = serde_json::from_slice(FIXTURE).unwrap();
    value["unexpectedAuthority"] = serde_json::Value::Bool(true);
    assert!(
        verify_virtual_spread_golden_fixture(&serde_json::to_vec(&value).unwrap())
            .unwrap_err()
            .contains("frozen page-143 fixture digest")
    );
}

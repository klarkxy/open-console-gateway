use super::{CpaPlacement, cpa_placement_from_origins};

#[test]
fn empty_base_is_the_owned_pool_without_a_listener() {
    let placement = cpa_placement_from_origins("", "dest-any", &[], &[]).expect("empty base");
    assert_eq!(placement, CpaPlacement::OwnedPool);
}

#[test]
fn matching_loopback_is_the_owned_pool() {
    let placement = cpa_placement_from_origins(
        "http://127.0.0.1:9",
        "dest-native",
        &["http://127.0.0.1:9", "http://127.0.0.1:9"],
        &["cpa-owned-native-id"],
    )
    .expect("owned listener");
    assert_eq!(placement, CpaPlacement::OwnedPool);
}

#[test]
fn other_parseable_base_is_historical_remote() {
    let placement = cpa_placement_from_origins(
        "https://historical.example",
        "dest-remote",
        &["http://127.0.0.1:9"],
        &["cpa-owned-native-id"],
    )
    .expect("remote base");
    assert_eq!(placement, CpaPlacement::Remote);
}

#[test]
fn unparseable_base_keeps_the_existing_endpoint_error() {
    let error =
        cpa_placement_from_origins("not a url", "dest-remote", &[], &[]).expect_err("bad base");
    assert_eq!(error.code, "invalid_endpoint");
    assert_eq!(error.detail, "remote CPA base URL is invalid");
}

#[test]
fn named_owned_destination_with_a_different_origin_is_ambiguous() {
    let error = cpa_placement_from_origins(
        "https://historical.example",
        "owned-dest",
        &["http://127.0.0.1:9"],
        &["owned-dest"],
    )
    .expect_err("named mismatch");
    assert_eq!(error.code, "ambiguous_owned_pool");
    assert_eq!(
        error.detail,
        "owned destination reference does not match a known owned origin"
    );
}

#[test]
fn unparseable_owned_origin_keeps_the_existing_listener_error() {
    let error = cpa_placement_from_origins(
        "https://historical.example",
        "dest-remote",
        &["https://historical.example"],
        &[],
    )
    .expect_err("remote is not an owned listener");
    assert_eq!(error.code, "invalid_owned_listener");
    assert_eq!(error.detail, "owned CPA listener must be a loopback origin");
}

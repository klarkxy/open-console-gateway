use super::*;

#[test]
fn billing_tokens_clamped_normalizes_legacy_counts() {
    let tokens = BillingTokens::clamped(-8, -3, 40, 30);
    assert_eq!(tokens, BillingTokens::new(0, 0, 0, 0));

    let tokens = BillingTokens::clamped(100, -4, 70, 50);
    assert_eq!(tokens, BillingTokens::new(100, 0, 70, 30));
    assert!(tokens.valid());

    let tokens = BillingTokens::clamped(10, 2, -1, -2);
    assert_eq!(tokens, BillingTokens::new(10, 2, 0, 0));
}

#[test]
fn billing_model_and_source_use_snake_case_wire_names() {
    assert_eq!(
        serde_json::to_value(BillingModel::Credits).unwrap(),
        serde_json::json!("credits")
    );
    assert_eq!(
        serde_json::to_value(BillingSource::LocalEstimate).unwrap(),
        serde_json::json!("local_estimate")
    );
    assert_eq!(
        serde_json::from_value::<BillingSource>(serde_json::json!("unavailable")).unwrap(),
        BillingSource::Unavailable
    );
}

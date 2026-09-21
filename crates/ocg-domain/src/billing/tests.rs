use super::*;

#[test]
fn token_charge_weights_four_disjoint_groups() {
    let tokens = BillingTokens::new(100, 5, 20, 10);
    let rates = TokenRates {
        input: 2.0,
        output: 3.0,
        cache_read: Some(0.5),
        cache_write: Some(4.0),
        per_tokens: 100.0,
    };
    // uncached 70, output 5, cache_read 20, cache_write 10
    assert_eq!(token_charge(tokens, rates), Some(2.05));
}

#[test]
fn token_charge_missing_cache_rate_is_unknown_only_when_tokens_present() {
    let no_cache = BillingTokens::new(10, 2, 0, 0);
    let rates = TokenRates::per_million(1.0, 2.0, None, None);
    assert_eq!(
        token_charge(no_cache, rates),
        Some((10.0 * 1.0 + 2.0 * 2.0) / 1_000_000.0)
    );

    assert_eq!(token_charge(BillingTokens::new(10, 0, 5, 0), rates), None);
    assert_eq!(token_charge(BillingTokens::new(10, 0, 0, 5), rates), None);
    assert_eq!(
        token_charge(
            BillingTokens::new(10, 0, 5, 0),
            TokenRates::per_million(1.0, 2.0, Some(0.25), None),
        ),
        Some((5.0 * 1.0 + 5.0 * 0.25) / 1_000_000.0)
    );
}

#[test]
fn token_charge_overflow_and_invalid_inputs_are_unknown() {
    let huge = BillingTokens::new(i64::MAX, i64::MAX, 0, 0);
    let exploding = TokenRates {
        input: f64::MAX,
        output: f64::MAX,
        cache_read: None,
        cache_write: None,
        per_tokens: 1.0,
    };
    assert_eq!(token_charge(huge, exploding), None);

    let rates = TokenRates::per_million(1.0, 1.0, None, None);
    assert_eq!(token_charge(BillingTokens::new(-1, 0, 0, 0), rates), None);
    assert_eq!(token_charge(BillingTokens::new(1, 0, 2, 0), rates), None);
    assert!(!BillingTokens::new(i64::MAX, 0, i64::MAX, 1).valid());
    assert_eq!(
        token_charge(
            BillingTokens::new(1, 0, 0, 0),
            TokenRates {
                input: f64::NAN,
                output: 1.0,
                cache_read: None,
                cache_write: None,
                per_tokens: 1.0,
            },
        ),
        None
    );
}

#[test]
fn token_charge_zero_tokens_is_zero() {
    let tokens = BillingTokens::new(0, 0, 0, 0);
    let rates = TokenRates::per_million(3.0, 4.0, Some(1.0), Some(2.0));
    assert_eq!(token_charge(tokens, rates), Some(0.0));
    assert!(tokens.valid());
}

#[test]
fn token_charge_scales_by_per_tokens() {
    let tokens = BillingTokens::new(50, 10, 20, 5);
    let per_thousand = TokenRates {
        input: 2.0,
        output: 4.0,
        cache_read: Some(1.0),
        cache_write: Some(8.0),
        per_tokens: 1_000.0,
    };
    let per_million = TokenRates::per_million(2.0, 4.0, Some(1.0), Some(8.0));
    let thousand = token_charge(tokens, per_thousand).unwrap();
    let million = token_charge(tokens, per_million).unwrap();
    assert!((thousand - million * 1_000.0).abs() < 1e-12);
    // uncached 25*2 + output 10*4 + read 20*1 + write 5*8 = 150
    assert!((thousand - 0.15).abs() < 1e-12);
}

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
fn convert_charge_scales_finite_nonnegative_amount() {
    assert_eq!(convert_charge(2.0, 1_000_000.0), Some(2_000_000.0));
    assert_eq!(convert_charge(0.0, 4.0), Some(0.0));
    assert_eq!(convert_charge(-1.0, 2.0), None);
    assert_eq!(convert_charge(1.0, 0.0), None);
    assert_eq!(convert_charge(1.0, -2.0), None);
    assert_eq!(convert_charge(f64::MAX, 2.0), None);
    assert_eq!(convert_charge(f64::NAN, 1.0), None);
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

#[test]
fn official_api_peak_flash_matches_shared_engine() {
    let tokens = BillingTokens::new(1_000_000, 100_000, 100_000, 0);
    let peak = TokenRates::per_million(0.3, 1.2, Some(0.006), None);
    assert!((token_charge(tokens, peak).unwrap() - 0.3906).abs() < 1e-12);
}

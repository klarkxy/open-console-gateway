use super::*;

#[test]
fn token_counters_remain_raw_without_price_or_debit() {
    let metrics = pricing_metrics(
        &RequestPricingSnapshot::Unpriced,
        "model",
        100,
        25,
        30,
        10,
        Some("priority"),
    );
    assert_eq!(metrics.prompt_tokens, 100);
    assert_eq!(metrics.completion_tokens, 25);
    assert_eq!(metrics.cached_tokens, 30);
    assert_eq!(metrics.cache_creation_tokens, 10);
    assert_eq!(metrics.service_tier.as_deref(), Some("priority"));
    assert_eq!(metrics.cost_state, "unknown");
    assert_eq!(metrics.cost, 0.0);
    assert_eq!(metrics.raw_cost_usd, None);
    assert_eq!(metrics.quota_debit, None);
    assert_eq!(metrics.effective_paid_cost_usd, None);
    assert_eq!(metrics.pricing_revision_id, None);
    assert_eq!(metrics.pricing_provider_id, None);
    assert_eq!(metrics.quota_multiplier, None);
    assert_eq!(metrics.local_adjustment_multiplier, None);
}

#[test]
fn metadata_keeps_missing_usage_and_unknown_outcomes_distinct() {
    for (input, expected) in [
        ("usage_missing", "usage_missing"),
        ("outcome_unknown", "outcome_unknown"),
        ("free", "unknown"),
        ("priced", "unknown"),
    ] {
        let metrics = metadata_metrics(&RequestPricingSnapshot::Unpriced, None, input);
        assert_eq!(metrics.cost_state, expected);
        assert_eq!(metrics.pricing_revision_id, None);
        assert_eq!(metrics.pricing_provider_id, None);
        assert_eq!(metrics.raw_cost_usd, None);
        assert_eq!(metrics.quota_debit, None);
    }
}

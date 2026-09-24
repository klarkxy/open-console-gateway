use super::{
    GOAT_SOURCE_URL, OLLAMA_SOURCE_URL, ProviderCostEstimate, ProviderCostState,
    ProviderPricingEvidence, ProviderPricingSnapshot, ProviderPricingValue,
    ProviderScopedPricingSnapshot, embedded_seed, ensure_current_adjustment_policy,
    ensure_seed_model_coverage, fetch_goat_pricing_snapshot, fetch_official_snapshot,
    latest_provider_pricing_snapshot, legacy_policy_needs_multiplier_repair, parse_goat_html,
    parse_official_html, parse_ollama_html, prepare_provider_multiplier_update,
    provider_pricing_capability, quota_multiplier, store_provider_pricing_snapshot,
};
use chrono::{DateTime, Utc};

use crate::db::Database;
use crate::provider::{COMMAND_CODE_PROVIDER_ID, OLLAMA_PROVIDER_ID};

#[test]
fn seed_coverage_backfills_missing_models_and_prices_them() {
    // A snapshot from a database created before the seed grew: no muse rows.
    let mut snapshot = embedded_seed();
    let previous_revision = snapshot.revision.clone();
    snapshot
        .models
        .retain(|entry| !entry.model_id.starts_with("muse-spark-1.2"));
    assert!(
        snapshot
            .estimate("muse-spark-1.2", 1_000, 100, 0, 0, None)
            .cost
            .is_none()
    );

    let repaired = ensure_seed_model_coverage(snapshot);
    assert_ne!(repaired.revision, previous_revision);
    let estimate = repaired.estimate("muse-spark-1.2", 1_000, 100, 0, 0, None);
    let cost = estimate.cost.expect("muse-spark-1.2 must be priced");
    // 1k uncached input @ $0.10 + 100 output @ $0.20 per M tokens.
    assert!((cost - (1_000.0 * 0.10 + 100.0 * 0.20) / 1_000_000.0).abs() < 1e-9);
    assert!(
        repaired
            .estimate("muse-spark-1.2-contributor", 1_000, 100, 0, 0, None)
            .cost
            .is_some()
    );
}

#[test]
fn seed_coverage_never_overwrites_existing_rows() {
    // A snapshot whose muse-contributor row came from the official table
    // (or carries a user-edited multiplier) must survive the backfill.
    let mut snapshot = embedded_seed();
    snapshot
        .models
        .retain(|entry| entry.model_id != "muse-spark-1.2-contributor");
    let edited = super::PricingModel {
        model_id: "muse-spark-1.2-contributor".to_string(),
        display_name: "Muse Spark 1.2 Contributor".to_string(),
        input: 0.10,
        output: 0.20,
        cache_read: 0.002,
        cache_write: None,
        usage: 60.0,
        quota_multiplier: 2.5,
        min_input_tokens: None,
        max_input_tokens: None,
        time_window: super::PricingTimeWindow::Always,
        adjustments: Vec::new(),
    };
    snapshot.models.push(edited);

    let repaired = ensure_seed_model_coverage(snapshot);
    let row = repaired
        .models
        .iter()
        .find(|entry| entry.model_id == "muse-spark-1.2-contributor")
        .unwrap();
    assert_eq!(row.quota_multiplier, 2.5);
}

#[test]
fn seed_coverage_is_noop_when_every_seed_model_is_present() {
    let snapshot = embedded_seed();
    let repaired = ensure_seed_model_coverage(snapshot.clone());
    assert_eq!(repaired.revision, snapshot.revision);
    assert_eq!(repaired.activated_at, snapshot.activated_at);
}

#[test]
fn seed_coverage_does_not_revive_models_outside_the_muse_allowlist() {
    let mut snapshot = embedded_seed();
    snapshot.models.retain(|entry| entry.model_id != "grok-4.5");

    let repaired = ensure_seed_model_coverage(snapshot);

    assert!(
        repaired
            .models
            .iter()
            .all(|entry| entry.model_id != "grok-4.5")
    );
}

#[test]
fn provider_quota_formula_uses_plan_limit_over_model_allowance() {
    assert_eq!(quota_multiplier(60.0, 15.0).unwrap(), 4.0);
    assert_eq!(quota_multiplier(60.0, 60.0).unwrap(), 1.0);
    assert!(quota_multiplier(60.0, 0.0).is_err());

    let estimate = ProviderCostEstimate::from_raw(2.0, Some(60.0), Some(15.0), Some(10.0)).unwrap();
    assert_eq!(estimate.raw_cost, Some(2.0));
    assert_eq!(estimate.quota_debit, Some(8.0));
    assert!((estimate.paid_cost.unwrap() - (4.0 / 3.0)).abs() < 1e-12);
    assert_eq!(estimate.cost_state, ProviderCostState::Priced);

    let unknown = ProviderCostEstimate::from_raw(2.0, None, None, None).unwrap();
    assert_eq!(unknown.raw_cost, Some(2.0));
    assert_eq!(unknown.quota_debit, None);
    assert_eq!(unknown.paid_cost, None);
    assert_eq!(unknown.cost_state, ProviderCostState::Unpriced);
}

#[test]
fn zen_free_is_zero_in_every_cost_domain() {
    let estimate = ProviderCostEstimate::zen_free();
    assert_eq!(estimate.raw_cost, Some(0.0));
    assert_eq!(estimate.quota_debit, Some(0.0));
    assert_eq!(estimate.paid_cost, Some(0.0));
    assert_eq!(estimate.cost_state, ProviderCostState::Free);
}

#[test]
fn provider_snapshot_round_trips_legacy_go_shape() {
    let legacy = embedded_seed();
    let typed = ProviderScopedPricingSnapshot::from_opencode_go(&legacy).unwrap();
    let record = typed.to_storage_record().unwrap();
    let loaded = ProviderScopedPricingSnapshot::from_storage_record(&record).unwrap();
    assert_eq!(loaded.provider_id(), "opencode");
    assert_eq!(loaded.revision(), legacy.revision);
    assert_eq!(loaded.evidence(), ProviderPricingEvidence::Verified);
    assert_eq!(loaded.values().len(), legacy.models.len());

    let legacy_record = ProviderPricingSnapshot {
        provider_id: "opencode".to_string(),

        revision: legacy.revision.clone(),
        activated_at: legacy.activated_at.clone(),
        document_updated_at: Some(legacy.document_updated_at.clone()),
        source_url: legacy.source_url.clone(),
        content_hash: legacy.content_hash.clone(),
        snapshot_json: serde_json::to_string(&legacy).unwrap(),
    };
    let migrated = ProviderScopedPricingSnapshot::from_storage_record(&legacy_record).unwrap();
    assert_eq!(migrated.values().len(), legacy.models.len());
}

#[test]
fn provider_snapshot_revision_is_append_only_in_v22_store() {
    let dir = std::env::temp_dir().join(format!("ocg-provider-pricing-{}", uuid::Uuid::new_v4()));
    let db = Database::open(dir.clone()).unwrap();
    let value = |name: &str, allowance: f64| {
        ProviderPricingValue::new(
            "captured-model",
            name,
            None,
            None,
            None,
            None,
            Some(60.0),
            Some(allowance),
            None,
            None,
            None,
            None,
            super::PricingTimeWindow::Always,
        )
        .unwrap()
    };
    let snapshot = |name: &str, allowance: f64| {
        ProviderScopedPricingSnapshot::new(
            COMMAND_CODE_PROVIDER_ID,
            "capture-1",
            "2030-01-01T00:00:00Z",
            None,
            "",
            "",
            ProviderPricingEvidence::Experimental,
            vec![value(name, allowance)],
        )
        .unwrap()
    };
    store_provider_pricing_snapshot(&db, &snapshot("first", 15.0)).unwrap();
    // Same provider/offering/revision is ignored, not overwritten.
    store_provider_pricing_snapshot(&db, &snapshot("second", 60.0)).unwrap();
    let loaded = latest_provider_pricing_snapshot(&db, COMMAND_CODE_PROVIDER_ID)
        .unwrap()
        .unwrap();
    assert_eq!(loaded.values()[0].display_name(), "first");
    assert_eq!(loaded.values()[0].quota_multiplier(), Some(4.0));
    drop(db);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn provider_multiplier_override_round_trips_and_drives_estimates() {
    let value = ProviderPricingValue::new(
        "captured-model",
        "Captured Model",
        Some(1.0),
        Some(2.0),
        Some(0.5),
        None,
        Some(60.0),
        Some(15.0),
        Some(10.0),
        Some("USD".to_string()),
        None,
        None,
        super::PricingTimeWindow::Always,
    )
    .unwrap();
    let active = ProviderScopedPricingSnapshot::new(
        COMMAND_CODE_PROVIDER_ID,
        "official-1",
        "2030-01-01T00:00:00Z",
        None,
        GOAT_SOURCE_URL,
        "content-1",
        ProviderPricingEvidence::Verified,
        vec![value],
    )
    .unwrap();
    assert_eq!(active.values()[0].quota_multiplier(), Some(4.0));

    let overridden =
        prepare_provider_multiplier_update(&active, &[("captured-model".to_string(), 2.0)])
            .unwrap()
            .unwrap();
    assert_ne!(overridden.revision(), active.revision());
    assert_eq!(overridden.values()[0].quota_multiplier(), Some(2.0));

    let record = overridden.to_storage_record().unwrap();
    let loaded = ProviderScopedPricingSnapshot::from_storage_record(&record).unwrap();
    assert_eq!(loaded.values()[0].quota_multiplier(), Some(2.0));
    let estimate = loaded.estimate(
        "captured-model",
        1_000_000,
        0,
        0,
        0,
        DateTime::parse_from_rfc3339("2030-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc),
    );
    assert_eq!(estimate.raw_cost_usd, Some(1.0));
    assert_eq!(estimate.quota_multiplier, Some(2.0));
    assert_eq!(estimate.quota_debit, Some(2.0));
    assert!((estimate.effective_paid_cost_usd.unwrap() - (1.0 / 3.0)).abs() < 1e-12);
}

#[test]
fn goat_manual_pricing_refresh_uses_the_verified_official_source() {
    let capability = provider_pricing_capability(COMMAND_CODE_PROVIDER_ID).unwrap();
    assert_eq!(capability.evidence, ProviderPricingEvidence::Verified);
    assert!(!capability.experimental);
    assert_eq!(capability.source_url, Some(GOAT_SOURCE_URL));
    assert!(capability.manual_refresh_available);
}

#[test]
fn goat_parser_accepts_concatenated_discount_and_free_badges() {
    let html = r#"
            <p>GOAT plan 3 All plans 61</p>
            <p>unlimited coding for $10/month</p>
            <p>5-hour limit - $14 of usage</p>
            <p>Weekly limit - $35 of usage</p>
            <p>Monthly limit - $70 of usage</p>
            <table>
              <tr><th>Model&#8597;</th><th>Context&#8597;</th><th>Intelligence&#8597;</th><th>Tok/s&#8597;</th><th>Input&#8597;</th><th>Output&#8597;</th><th>Cache read&#8597;</th><th>Cache write&#8597;</th><th>Caps</th></tr>
              <tr><td>MiniMax M3-50%Ends December 31, 2026</td><td>1M</td><td>45</td><td>100</td><td>$0.30</td><td>$1.20</td><td>$0.06</td><td>-</td><td>+1</td></tr>
              <tr><td>MiniMax M3FreeEnds September 5, 2026</td><td>1M</td><td>45</td><td>100</td><td>Free</td><td>Free</td><td>Free</td><td>-</td><td>+1</td></tr>
              <tr><td>Laguna S 2.1 Free</td><td>256K</td><td>40</td><td>60</td><td>Free</td><td>Free</td><td>Free</td><td>-</td><td>+1</td></tr>
            </table>
            <table>
              <tr><th>Model</th><th>Input</th><th>Output</th><th>Cache Read</th><th>Cache Write</th><th>Monthly credits</th></tr>
              <tr><td>MiniMax M3</td><td>$0.30</td><td>$1.20</td><td>$0.06</td><td>-</td><td>$47</td></tr>
            </table>
        "#;

    let snapshot = parse_goat_html(html).unwrap();
    assert_eq!(snapshot.values().len(), 3);
    let paid = snapshot
        .values()
        .iter()
        .find(|value| value.model_id() == "minimax-m3")
        .unwrap();
    assert_eq!(paid.display_name(), "MiniMax M3");
    assert_eq!(paid.model_allowance(), Some(47.0));
    let free = snapshot
        .values()
        .iter()
        .find(|value| value.model_id() == "minimax-m3-free")
        .unwrap();
    assert_eq!(free.display_name(), "MiniMax M3 Free");
    assert_eq!(free.input_per_million(), None);
    assert_eq!(free.model_allowance(), None);
    assert!(
        snapshot
            .values()
            .iter()
            .any(|value| value.model_id() == "laguna-s-2-1-free")
    );

    let estimate = snapshot.estimate("vendor/minimax-m3", 1_000_000, 100_000, 0, 0, Utc::now());
    assert_eq!(estimate.cost_state, "priced");
    assert!((estimate.raw_cost_usd.unwrap() - 0.42).abs() < 1e-12);
    assert!((estimate.quota_multiplier.unwrap() - (70.0 / 47.0)).abs() < 1e-12);
    assert!((estimate.cost.unwrap() - (0.42 * 70.0 / 47.0)).abs() < 1e-12);
    assert!((estimate.effective_paid_cost_usd.unwrap() - (0.42 * 10.0 / 47.0)).abs() < 1e-12);

    let free = snapshot.estimate("laguna-s-2-1-free", 1_000, 100, 0, 0, Utc::now());
    assert_eq!(free.cost_state, "free");
    assert_eq!(free.raw_cost_usd, Some(0.0));
    let unknown = snapshot.estimate("not-in-the-price-table", 1_000, 100, 0, 0, Utc::now());
    assert_eq!(unknown.cost_state, "unpriced");
}

#[test]
fn goat_parser_materializes_peak_rates_and_uses_them_every_day() {
    let html = r#"
        <p>GOAT plan 1 All plans 1</p>
        <p>unlimited coding for $10/month</p>
        <p>5-hour limit - $14 of usage</p>
        <p>Weekly limit - $35 of usage</p>
        <p>Monthly limit - $70 of usage</p>
        <table>
          <tr><th>Model</th><th>Context</th><th>Intelligence</th><th>Tok/s</th><th>Input</th><th>Output</th><th>Cache read</th><th>Cache write</th><th>Caps</th></tr>
          <tr>
            <td>DeepSeek V4 Flash (latest)<span>Off-peak shown (17h/day) · peak $0.44 / $1.32 01–04 &amp; 06–10 UTC</span></td>
            <td>1M</td><td>52</td><td>129</td>
            <td>$0.22<button aria-label="DeepSeek V4 Flash (latest) input: $0.44 during peak hours"></button></td>
            <td>$0.66<button aria-label="DeepSeek V4 Flash (latest) output: $1.32 during peak hours"></button></td>
            <td>$0.007<button aria-label="DeepSeek V4 Flash (latest) cache read: $0.014 during peak hours"></button></td>
            <td>—</td><td>+1</td>
          </tr>
        </table>
        <table>
          <tr><th>Model</th><th>Input</th><th>Output</th><th>Cache Read</th><th>Cache Write</th><th>Monthly credits</th></tr>
          <tr><td>DeepSeek V4 Flash (latest)</td><td>$0.22</td><td>$0.66</td><td>$0.007</td><td>-</td><td>$60</td></tr>
        </table>
    "#;

    let snapshot = parse_goat_html(html).unwrap();
    assert_eq!(snapshot.values().len(), 2);
    let off_peak = snapshot
        .values()
        .iter()
        .find(|value| value.time_window() == super::PricingTimeWindow::OffPeak)
        .unwrap();
    assert_eq!(off_peak.input_per_million(), Some(0.22));
    assert_eq!(off_peak.output_per_million(), Some(0.66));
    assert_eq!(off_peak.cache_read_per_million(), Some(0.007));
    let peak = snapshot
        .values()
        .iter()
        .find(|value| value.time_window() == super::PricingTimeWindow::Peak)
        .unwrap();
    assert_eq!(peak.input_per_million(), Some(0.44));
    assert_eq!(peak.output_per_million(), Some(1.32));
    assert_eq!(peak.cache_read_per_million(), Some(0.014));

    let sunday_peak = DateTime::parse_from_rfc3339("2026-09-06T07:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    let sunday_off_peak = DateTime::parse_from_rfc3339("2026-09-06T12:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    assert_eq!(
        snapshot
            .estimate("deepseek-v4-flash", 1_000_000, 0, 1_000_000, 0, sunday_peak)
            .raw_cost_usd,
        Some(0.014)
    );
    assert_eq!(
        snapshot
            .estimate(
                "deepseek-v4-flash",
                1_000_000,
                0,
                1_000_000,
                0,
                sunday_off_peak,
            )
            .raw_cost_usd,
        Some(0.007)
    );

    let missing_peak_cache = html.replace(
        r#" aria-label="DeepSeek V4 Flash (latest) cache read: $0.014 during peak hours""#,
        "",
    );
    assert!(
        parse_goat_html(&missing_peak_cache)
            .unwrap_err()
            .to_string()
            .contains("missing its peak cache read price")
    );
}

#[test]
fn provider_pricing_matches_vendor_prefixed_catalog_ids_to_unique_display_names() {
    let snapshot = ProviderScopedPricingSnapshot::new(
        COMMAND_CODE_PROVIDER_ID,
        "goat-test",
        "2030-01-01T00:00:00Z",
        None,
        GOAT_SOURCE_URL,
        "hash",
        ProviderPricingEvidence::Verified,
        vec![
            ProviderPricingValue::new(
                "tencent-hy3",
                "Tencent Hy3",
                Some(0.14),
                Some(0.58),
                Some(0.035),
                None,
                Some(70.0),
                Some(70.0),
                Some(10.0),
                Some("USD".into()),
                None,
                None,
                super::PricingTimeWindow::Always,
            )
            .unwrap(),
        ],
    )
    .unwrap();
    let estimate = snapshot.estimate("provider/hy3", 1_000_000, 0, 0, 0, Utc::now());
    assert_eq!(estimate.cost_state, "priced");
    assert_eq!(estimate.raw_cost_usd, Some(0.14));
    assert_eq!(estimate.quota_multiplier, Some(1.0));
    assert_eq!(estimate.cost, Some(0.14));
}

#[test]
fn pro_usage_allowance_is_applied_after_the_official_table_rates() {
    let snapshot = embedded_seed();
    for (model_id, prompt, cached, completion, official_monthly_requests) in [
        ("deepseek-v4-pro", 82_750, 82_000, 290, 5_200.0),
        ("mimo-v2.5-pro", 86_790, 86_000, 305, 16_300.0),
    ] {
        let model = snapshot
            .models
            .iter()
            .find(|entry| entry.model_id == model_id)
            .unwrap();
        assert_eq!(model.usage, 15.0);
        assert_eq!(model.quota_multiplier, 4.0);

        let estimate = snapshot.estimate_at(
            model_id,
            prompt,
            completion,
            cached,
            0,
            None,
            DateTime::parse_from_rfc3339("2026-08-16T12:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
        );
        let estimated_monthly_requests = snapshot.limits.window_month / estimate.cost.unwrap();
        assert!(
            (estimated_monthly_requests / official_monthly_requests - 1.0).abs() < 0.01,
            "{model_id}: {estimated_monthly_requests} != {official_monthly_requests}",
        );
        assert_eq!(estimate.quota_multiplier, Some(4.0));
    }

    let grok = snapshot
        .models
        .iter()
        .find(|entry| entry.model_id == "grok-4.5")
        .unwrap();
    assert_eq!(grok.usage, 15.0);
    assert_eq!(grok.quota_multiplier, 4.0);
    assert_eq!(
        snapshot.estimate("grok-4.5", 1_000_000, 0, 0, 0, None).cost,
        Some(8.0)
    );
}

#[test]
fn policy_upgrade_repairs_persisted_pro_quota_multipliers() {
    let mut snapshot = embedded_seed();
    snapshot.adjustment_policy_version = "local-v2".to_string();
    for model in &mut snapshot.models {
        if matches!(model.model_id.as_str(), "deepseek-v4-pro" | "mimo-v2.5-pro") {
            model.quota_multiplier = 1.0;
        }
    }

    let upgraded = ensure_current_adjustment_policy(snapshot);
    for model_id in ["deepseek-v4-pro", "mimo-v2.5-pro"] {
        let model = upgraded
            .models
            .iter()
            .find(|entry| entry.model_id == model_id)
            .unwrap();
        assert_eq!(model.quota_multiplier, 4.0);
    }
}

#[test]
fn legacy_snapshot_json_drops_old_price_multiplier_and_repairs_applied_multiplier() {
    let mut value = serde_json::to_value(embedded_seed()).unwrap();
    value["adjustment_policy_version"] = serde_json::Value::String("local-v2".into());
    for model in value["models"].as_array_mut().unwrap() {
        let object = model.as_object_mut().unwrap();
        if matches!(
            object.get("model_id").and_then(serde_json::Value::as_str),
            Some("deepseek-v4-pro" | "mimo-v2.5-pro")
        ) {
            object.insert("official_price_multiplier".into(), serde_json::json!(4.0));
            object.insert("quota_multiplier".into(), serde_json::json!(1.0));
        }
    }

    let persisted = serde_json::from_value(value).unwrap();
    let upgraded = ensure_current_adjustment_policy(persisted);
    for model_id in ["deepseek-v4-pro", "mimo-v2.5-pro"] {
        let model = upgraded
            .models
            .iter()
            .find(|entry| entry.model_id == model_id)
            .unwrap();
        assert_eq!(model.quota_multiplier, 4.0);
    }
    assert!(
        !serde_json::to_string(&upgraded)
            .unwrap()
            .contains("official_price_multiplier")
    );
}

#[test]
fn editable_policy_versions_are_never_rebased() {
    assert!(legacy_policy_needs_multiplier_repair("local-v2"));
    assert!(!legacy_policy_needs_multiplier_repair("local-v3"));
    assert!(!legacy_policy_needs_multiplier_repair("local-v4"));
    assert!(!legacy_policy_needs_multiplier_repair("local-v99"));

    let mut snapshot = embedded_seed();
    snapshot.adjustment_policy_version = "local-v3".to_string();
    snapshot
        .models
        .iter_mut()
        .filter(|model| model.model_id == "qwen3.7-plus")
        .for_each(|model| model.quota_multiplier = 0.75);
    let upgraded = ensure_current_adjustment_policy(snapshot);
    assert!(
        upgraded
            .models
            .iter()
            .filter(|model| model.model_id == "qwen3.7-plus")
            .all(|model| model.quota_multiplier == 0.75)
    );
}

#[test]
fn minimax_adjustments_follow_local_policy() {
    let snapshot = embedded_seed();
    let at_boundary = snapshot.estimate("minimax-m3", 512_000, 10, 0, 0, None);
    let over_boundary = snapshot.estimate("minimax-m3", 512_001, 10, 0, 0, None);
    assert!((over_boundary.local_adjustment_multiplier.unwrap() - 2.0).abs() < 1e-12);
    assert_eq!(at_boundary.local_adjustment_multiplier, Some(1.0));
    let priority = snapshot.estimate("minimax-m3", 1000, 10, 0, 0, Some("priority"));
    assert!((priority.local_adjustment_multiplier.unwrap() - 1.5).abs() < 1e-12);
    let combined = snapshot.estimate("minimax-m3", 512_001, 10, 0, 0, Some("priority"));
    assert!((combined.local_adjustment_multiplier.unwrap() - 3.0).abs() < 1e-12);
}

#[test]
fn highspeed_only_doubles_input_and_output() {
    let snapshot = embedded_seed();
    let normal = snapshot
        .estimate("minimax-m2.7", 1000, 100, 400, 300, None)
        .cost
        .unwrap();
    let fast = snapshot
        .estimate("minimax-m2.7-highspeed", 1000, 100, 400, 300, None)
        .cost
        .unwrap();
    let expected = (300.0 * 0.60 + 100.0 * 2.40 + 400.0 * 0.06 + 300.0 * 0.375) / 1_000_000.0;
    assert!((fast - expected).abs() < 1e-12);
    assert!(fast < normal * 2.0);
}

#[test]
fn go_estimate_matches_shared_billing_groups_and_quota_scale() {
    let snapshot = embedded_seed();
    let model = snapshot
        .models
        .iter()
        .find(|entry| entry.model_id == "minimax-m2.7")
        .unwrap();
    let tokens = ocg_domain::billing::BillingTokens::clamped(1000, 100, 400, 300);
    let raw = ocg_domain::billing::token_charge(
        tokens,
        ocg_domain::billing::TokenRates::per_million(
            model.input,
            model.output,
            Some(model.cache_read),
            Some(model.cache_write.unwrap_or(model.input)),
        ),
    )
    .unwrap();
    let estimate = snapshot.estimate("minimax-m2.7", 1000, 100, 400, 300, None);
    assert_eq!(estimate.cost_state, "priced");
    assert!((estimate.raw_cost_usd.unwrap() - raw).abs() < 1e-12);
    assert_eq!(
        estimate.quota_debit,
        ocg_domain::billing::convert_charge(raw, model.quota_multiplier)
    );
    assert_eq!(estimate.cost, estimate.quota_debit);
}

#[test]
fn unknown_model_is_unpriced() {
    let estimate = embedded_seed().estimate("future-model", 1000, 100, 0, 0, None);
    assert_eq!(estimate.cost, None);
    assert_eq!(estimate.cost_state, "unpriced");
    let prefixed = embedded_seed().estimate("provider-minimax-m3", 1000, 100, 0, 0, None);
    assert_eq!(prefixed.cost, None);
}

#[test]
fn zen_free_models_do_not_enter_go_quota() {
    for model_id in [
        "mimo-v2.5-free",
        "muse-spark-1.3-contributor-free",
        "muse-spark-1.2-contributor-free",
    ] {
        let estimate = embedded_seed().estimate(model_id, 1000, 100, 0, 0, None);
        assert_eq!(estimate.cost, None, "{model_id}");
        assert_eq!(estimate.raw_cost_usd, Some(0.0), "{model_id}");
        assert_eq!(estimate.quota_debit, Some(0.0), "{model_id}");
        assert_eq!(estimate.effective_paid_cost_usd, Some(0.0), "{model_id}");
        assert_eq!(estimate.cost_state, "free", "{model_id}");
        assert_eq!(estimate.quota_multiplier, None, "{model_id}");
    }
    let paid = embedded_seed().estimate("deepseek-v4-flash", 1000, 100, 0, 0, None);
    assert_eq!(paid.cost_state, "priced");
    assert!(paid.cost.is_some());
    let suffix_follows_zen_catalog_naming =
        embedded_seed().estimate("brand-new-promo-free", 1000, 100, 0, 0, None);
    assert_eq!(suffix_follows_zen_catalog_naming.cost_state, "free");
}

#[test]
fn cache_write_dash_falls_back_to_new_input_price() {
    let estimate = embedded_seed().estimate("glm-5.2", 1000, 0, 0, 1000, None);
    assert!((estimate.cost.unwrap() - 0.0014).abs() < 1e-12);
}

#[test]
fn parses_current_official_document_shape() {
    let snapshot = parse_official_html(include_str!(
        "../../tests/fixtures/opencode-go-current.html"
    ))
    .unwrap();
    assert_eq!(snapshot.limits.window_5h, 12.0);
    assert_eq!(snapshot.limits.window_week, 30.0);
    assert_eq!(snapshot.limits.window_month, 60.0);
    assert_eq!(snapshot.document_updated_at, "2026-09-13T21:38:53.000Z");

    let flash = snapshot
        .models
        .iter()
        .find(|model| model.model_id == "glm-5.3-flash")
        .unwrap();
    assert_eq!(flash.usage, 60.0);
    assert_eq!(flash.quota_multiplier, 1.0);

    let grok_tiers = snapshot
        .models
        .iter()
        .filter(|model| model.model_id == "grok-4.6")
        .collect::<Vec<_>>();
    assert_eq!(grok_tiers.len(), 2);
    assert!(
        grok_tiers
            .iter()
            .any(|tier| tier.max_input_tokens == Some(200_000))
    );
    assert!(
        grok_tiers
            .iter()
            .any(|tier| tier.min_input_tokens == Some(200_001))
    );
    assert!(grok_tiers.iter().all(|tier| tier.usage == 15.0));
    assert!(grok_tiers.iter().all(|tier| tier.quota_multiplier == 4.0));

    let promo = snapshot
        .models
        .iter()
        .filter(|model| model.model_id == "deepseek-v4.1-flash")
        .collect::<Vec<_>>();
    assert_eq!(promo.len(), 2);
    assert!(promo.iter().all(|model| model.usage == 60.0));
    assert!(promo.iter().all(|model| model.quota_multiplier == 1.0));
    assert!(
        promo
            .iter()
            .any(|model| model.time_window == super::PricingTimeWindow::Peak)
    );
    assert!(
        promo
            .iter()
            .any(|model| model.time_window == super::PricingTimeWindow::OffPeak)
    );
}

#[test]
fn rejects_incomplete_grok_200k_tiers() {
    let fixture = include_str!("../../tests/fixtures/opencode-go-current.html").replace(
        "<tr><td>Grok 4.6 (&gt; 200K tokens)</td><td>$4.00</td><td>$12.00</td><td>$1.00</td><td>-</td><td><strong>$15</strong></td></tr>",
        "",
    );
    assert!(
        parse_official_html(&fixture)
            .unwrap_err()
            .to_string()
            .contains("must contain complete 200K pricing tiers")
    );
}

#[test]
fn parses_official_fixture() {
    let snapshot =
        parse_official_html(include_str!("../../tests/fixtures/opencode-go.html")).unwrap();
    assert_eq!(snapshot.limits.window_5h, 12.0);
    assert_eq!(snapshot.limits.window_week, 30.0);
    assert_eq!(snapshot.limits.window_month, 60.0);
    assert_eq!(snapshot.models.len(), 25);
    assert!(
        snapshot
            .models
            .iter()
            .any(|entry| entry.model_id == "kimi-k3" && entry.quota_multiplier == 4.0)
    );
    for model_id in [
        "deepseek-v4-pro",
        "deepseek-v4-flash",
        "mimo-v2.5-pro",
        "gpt-5.6-luna",
    ] {
        let model = snapshot
            .models
            .iter()
            .find(|entry| entry.model_id == model_id)
            .unwrap();
        assert_eq!(model.quota_multiplier, 4.0);
    }
    assert_eq!(
        snapshot
            .models
            .iter()
            .filter(|entry| entry.model_id == "deepseek-v4-flash")
            .count(),
        2
    );
    assert!(
        snapshot
            .models
            .iter()
            .any(|entry| entry.model_id == "hy3" && entry.quota_multiplier == 1.0)
    );
    assert!(
        !snapshot
            .models
            .iter()
            .any(|entry| entry.model_id == "ox-alpha-free"),
        "dash-priced Go promos must not enter the USD snapshot"
    );
    let luna_tiers = snapshot
        .models
        .iter()
        .filter(|entry| entry.model_id == "gpt-5.6-luna")
        .count();
    assert_eq!(luna_tiers, 2);
}

#[test]
fn deepseek_uses_utc_peak_and_off_peak_rows() {
    let snapshot = embedded_seed();
    // Both instants are on Monday 2026-08-17. They used to sit on Sunday
    // 2026-08-16, which made the `peak` half assert the weekend bug: a
    // Sunday 07:00Z session is off-peak, so it cannot cost 1.76. Keeping
    // this pair on a weekday leaves it testing the hour axis only; the
    // weekday axis is covered by the test below.
    let off_peak = DateTime::parse_from_rfc3339("2026-08-17T12:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    let peak = DateTime::parse_from_rfc3339("2026-08-17T07:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    let off = snapshot
        .estimate_at("deepseek-v4-flash", 1_000_000, 0, 0, 0, None, off_peak)
        .cost
        .unwrap();
    let on = snapshot
        .estimate_at("deepseek-v4-flash", 1_000_000, 0, 0, 0, None, peak)
        .cost
        .unwrap();
    assert!((off - 0.88).abs() < 1e-12);
    assert!((on - 1.76).abs() < 1e-12);
}

#[test]
fn deepseek_weekend_is_off_peak_at_every_hour() {
    // Peak is Monday through Friday only, so both weekend days must price
    // at the Off-Peak rows even inside the 01:00-04:00 / 06:00-10:00 windows.
    let snapshot = embedded_seed();
    for at in [
        "2026-08-15T02:00:00Z", // Saturday, inside the first peak window
        "2026-08-15T07:00:00Z", // Saturday, inside the second
        "2026-08-16T02:00:00Z", // Sunday, inside the first
        "2026-08-16T07:00:00Z", // Sunday, inside the second
    ] {
        let when = DateTime::parse_from_rfc3339(at)
            .unwrap()
            .with_timezone(&Utc);
        let cost = snapshot
            .estimate_at("deepseek-v4-flash", 1_000_000, 0, 0, 0, None, when)
            .cost
            .unwrap();
        assert!(
            (cost - 0.88).abs() < 1e-12,
            "{at} is on a weekend and must bill off-peak (0.88), got {cost}"
        );
    }

    // The Friday and Monday either side of that weekend still bill at peak,
    // so this is a weekday boundary and not a blanket disable.
    for at in ["2026-08-14T07:00:00Z", "2026-08-17T07:00:00Z"] {
        let when = DateTime::parse_from_rfc3339(at)
            .unwrap()
            .with_timezone(&Utc);
        let cost = snapshot
            .estimate_at("deepseek-v4-flash", 1_000_000, 0, 0, 0, None, when)
            .cost
            .unwrap();
        assert!(
            (cost - 1.76).abs() < 1e-12,
            "{at} is a weekday peak hour and must bill 1.76, got {cost}"
        );
    }
}

#[test]
fn rejects_incomplete_peak_off_peak_pair() {
    let fixture = include_str!("../../tests/fixtures/opencode-go.html").replace(
            "<tr><td>DeepSeek V4 Flash (Peak)</td><td>$0.44</td><td>$1.32</td><td>$0.014</td><td>-</td><td>$15</td></tr>",
            "",
        );
    assert!(
        parse_official_html(&fixture)
            .unwrap_err()
            .to_string()
            .contains("must contain both Peak and Off-Peak")
    );
}

#[test]
fn rejects_model_id_without_a_matching_price_row() {
    let fixture = include_str!("../../tests/fixtures/opencode-go.html");
    let incomplete = fixture.replace(
            "<tr><td>Grok 4.5</td><td>$2.00</td><td>$6.00</td><td>$0.30</td><td>-</td><td>$15</td></tr>",
            "",
        );
    assert!(
        parse_official_html(&incomplete)
            .unwrap_err()
            .to_string()
            .contains("model ID table contains models without pricing rows")
    );
}

#[test]
fn accepts_official_model_removal_when_both_tables_still_match() {
    let fixture = include_str!("../../tests/fixtures/opencode-go.html")
            .replace(
                "<tr><td>Grok 4.5</td><td>$2.00</td><td>$6.00</td><td>$0.30</td><td>-</td><td>$15</td></tr>",
                "",
            )
            .replace(
                "<tr><td>Grok 4.5</td><td>grok-4.5</td><td>x</td><td>x</td></tr>",
                "",
            );
    let snapshot = parse_official_html(&fixture).unwrap();
    assert!(
        snapshot
            .models
            .iter()
            .all(|model| model.model_id != "grok-4.5")
    );
}

#[test]
fn qwen_tier_validation_is_conditional_on_the_model_being_present() {
    let fixture = include_str!("../../tests/fixtures/opencode-go.html")
            .replace(
                "<tr><td>Qwen3.7 Plus (&#x2264; 256K tokens)</td><td>$0.40</td><td>$1.60</td><td>$0.04</td><td>$0.50</td><td>$60</td></tr>",
                "",
            )
            .replace(
                "<tr><td>Qwen3.7 Plus (&gt; 256K tokens)</td><td>$1.20</td><td>$4.80</td><td>$0.12</td><td>$1.50</td><td>$60</td></tr>",
                "",
            )
            .replace(
                "<tr><td>Qwen3.7 Plus</td><td>qwen3.7-plus</td><td>x</td><td>x</td></tr>",
                "",
            );
    let snapshot = parse_official_html(&fixture).unwrap();
    assert!(
        snapshot
            .models
            .iter()
            .all(|model| model.model_id != "qwen3.7-plus")
    );
}

#[test]
fn rejects_structurally_valid_but_empty_catalog() {
    let fixture = r#"
            <p>5 hour limit — $12 of usage</p>
            <p>Weekly limit — $30 of usage</p>
            <p>Monthly limit — $60 of usage</p>
            <table><thead><tr><th>Model</th><th>Input</th><th>Output</th><th>Cached Read</th><th>Cached Write</th><th>Usage</th></tr></thead><tbody></tbody></table>
            <table><thead><tr><th>Model</th><th>Model ID</th><th>Endpoint</th><th>AI SDK Package</th></tr></thead><tbody></tbody></table>
            <time datetime="2026-07-17T15:53:00.000Z">Jul 17, 2026</time>
        "#;
    assert!(
        parse_official_html(fixture)
            .unwrap_err()
            .to_string()
            .contains("must not be empty")
    );
}

#[test]
fn parsed_limit_and_price_changes_drive_dynamic_multiplier() {
    let fixture = include_str!("../../tests/fixtures/opencode-go.html")
        .replace(
            "Monthly limit — $60 of usage",
            "Monthly limit — $90 of usage",
        )
        .replace(
            "<tr><td>Kimi K3</td><td>$3.00</td>",
            "<tr><td>Kimi K3</td><td>$3.50</td>",
        );
    let snapshot = parse_official_html(&fixture).unwrap();
    let kimi = snapshot
        .models
        .iter()
        .find(|model| model.model_id == "kimi-k3")
        .unwrap();
    assert_eq!(snapshot.limits.window_month, 90.0);
    assert_eq!(kimi.input, 3.5);
    assert_eq!(kimi.quota_multiplier, 6.0);
}

#[test]
fn accepts_new_models_with_an_official_id_and_complete_prices() {
    let fixture = include_str!("../../tests/fixtures/opencode-go.html")
            .replace("\r\n", "\n")
            .replace(
                "</tbody></table>\n<table><thead><tr><th>Model</th><th>Model ID</th>",
                "<tr><td>Future Model</td><td>$1.00</td><td>$2.00</td><td>$0.10</td><td>-</td><td>$60</td></tr></tbody></table>\n<table><thead><tr><th>Model</th><th>Model ID</th>",
            )
            .replace(
                "</tbody></table>\n<footer>",
                "<tr><td>Future Model</td><td>future-model</td><td>x</td><td>x</td></tr></tbody></table>\n<footer>",
            );
    let snapshot = parse_official_html(&fixture).unwrap();
    assert!(
        snapshot
            .models
            .iter()
            .any(|model| model.model_id == "future-model")
    );
}

#[test]
fn rejects_missing_or_reordered_price_columns() {
    let fixture = include_str!("../../tests/fixtures/opencode-go.html").replace(
        "<th>Input</th><th>Output</th>",
        "<th>Output</th><th>Input</th>",
    );
    assert!(
        parse_official_html(&fixture)
            .unwrap_err()
            .to_string()
            .contains("pricing table was not found")
    );
}

#[test]
fn rejects_duplicate_price_rows() {
    let fixture = include_str!("../../tests/fixtures/opencode-go.html").replace(
            "<tr><td>Grok 4.5</td><td>$2.00</td><td>$6.00</td><td>$0.30</td><td>-</td><td>$15</td></tr>",
            "<tr><td>Grok 4.5</td><td>$2.00</td><td>$6.00</td><td>$0.30</td><td>-</td><td>$15</td></tr><tr><td>Grok 4.5</td><td>$2.00</td><td>$6.00</td><td>$0.30</td><td>-</td><td>$15</td></tr>",
        );
    assert!(
        parse_official_html(&fixture)
            .unwrap_err()
            .to_string()
            .contains("duplicate row")
    );
}

#[tokio::test]
#[ignore = "requires live access to opencode.ai"]
async fn live_official_document_still_matches_the_parser() {
    let snapshot = fetch_official_snapshot(&crate::models::AppConfig::default())
        .await
        .unwrap();
    assert_eq!(snapshot.source_url, super::SOURCE_URL);
    assert!(snapshot.models.len() >= 18);
}

#[tokio::test]
#[ignore = "requires live access to commandcode.ai"]
async fn live_goat_document_still_materializes_peak_and_off_peak_rows() {
    let snapshot = fetch_goat_pricing_snapshot(&crate::models::AppConfig::default())
        .await
        .unwrap();
    assert_eq!(snapshot.source_url(), GOAT_SOURCE_URL);
    assert!(
        snapshot
            .values()
            .iter()
            .any(|value| value.time_window() == super::PricingTimeWindow::Peak)
    );
    assert!(
        snapshot
            .values()
            .iter()
            .any(|value| value.time_window() == super::PricingTimeWindow::OffPeak)
    );
}

const OLLAMA_PRICING_HTML: &str = r#"
<table>
  <tr><th>Model</th><th>Input</th><th>Cached input</th><th>Output</th></tr>
  <tr><td>glm-5.3-flash</td><td>$0.15</td><td>$0.03</td><td>$0.50</td></tr>
  <tr><td>gpt-oss:20b</td><td>$0.07</td><td>$0.035</td><td>$0.30</td></tr>
  <tr><td>gpt-oss:120b</td><td>$0.15</td><td>$0.014</td><td>$0.60</td></tr>
  <tr><td>mistral-large-3</td><td>$0.50</td><td>-</td><td>$1.50</td></tr>
</table>
"#;

#[test]
fn ollama_parser_reads_official_table_and_pins_multiplier_to_one() {
    let snapshot = parse_ollama_html(OLLAMA_PRICING_HTML).unwrap();
    assert_eq!(snapshot.provider_id(), OLLAMA_PROVIDER_ID);
    assert_eq!(snapshot.source_url(), OLLAMA_SOURCE_URL);
    assert_eq!(snapshot.values().len(), 4);
    let flash = snapshot
        .values()
        .iter()
        .find(|value| value.model_id() == "glm-5.3-flash")
        .unwrap();
    assert_eq!(flash.input_per_million(), Some(0.15));
    assert_eq!(flash.cache_read_per_million(), Some(0.03));
    assert_eq!(flash.output_per_million(), Some(0.50));
    assert_eq!(flash.cache_write_per_million(), None);
    assert_eq!(flash.quota_multiplier(), Some(1.0));
    assert_eq!(flash.paid_plan_price(), None);
}

#[test]
fn ollama_estimate_matches_runtime_tags_and_uses_cached_price_only_when_reported() {
    let snapshot = parse_ollama_html(OLLAMA_PRICING_HTML).unwrap();
    let at = DateTime::parse_from_rfc3339("2026-09-01T00:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    let exact = snapshot.estimate("glm-5.3-flash", 1_000_000, 1_000_000, 0, 0, at);
    assert_eq!(exact.cost_state, "priced");
    assert_eq!(exact.raw_cost_usd, Some(0.15 + 0.50));
    assert_eq!(exact.quota_debit, exact.raw_cost_usd);
    assert_eq!(exact.cost, exact.raw_cost_usd);
    assert_eq!(exact.effective_paid_cost_usd, None);
    assert_eq!(exact.quota_multiplier, Some(1.0));

    let tagged = snapshot.estimate("glm-5.3-flash:cloud", 1_000_000, 0, 0, 0, at);
    assert_eq!(tagged.raw_cost_usd, Some(0.15));
    let dated = snapshot.estimate("glm-5.3-flash:0915", 1_000_000, 0, 0, 0, at);
    assert_eq!(dated.raw_cost_usd, Some(0.15));

    let cached = snapshot.estimate("glm-5.3-flash", 1_000_000, 0, 400_000, 0, at);
    assert_eq!(cached.raw_cost_usd, Some(0.15 * 0.6 + 0.03 * 0.4));

    let cache_create = snapshot.estimate("glm-5.3-flash", 1_000_000, 0, 0, 200_000, at);
    assert_eq!(cache_create.raw_cost_usd, Some(0.15));

    assert_eq!(
        snapshot.estimate("gpt-oss", 1_000, 0, 0, 0, at).cost_state,
        "unpriced"
    );
    assert_eq!(
        snapshot
            .estimate("unknown-model:cloud", 1_000, 0, 0, 0, at)
            .cost_state,
        "unpriced"
    );
    let oss = snapshot.estimate("gpt-oss:20b:cloud", 1_000_000, 0, 0, 0, at);
    assert_eq!(oss.raw_cost_usd, Some(0.07));
}

#[test]
fn ollama_dash_cached_price_bills_cached_tokens_at_the_input_rate() {
    let snapshot = parse_ollama_html(OLLAMA_PRICING_HTML).unwrap();
    let mistral = snapshot
        .values()
        .iter()
        .find(|value| value.model_id() == "mistral-large-3")
        .unwrap();
    assert_eq!(mistral.cache_read_per_million(), None);

    let at = DateTime::parse_from_rfc3339("2026-09-01T00:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    let estimate = snapshot.estimate("mistral-large-3", 1_000_000, 1_000_000, 400_000, 0, at);
    assert_eq!(estimate.cost_state, "priced");
    assert_eq!(estimate.raw_cost_usd, Some(0.50 + 1.50));
}

#[test]
fn ollama_parser_rejects_missing_empty_duplicate_and_incomplete_tables() {
    assert!(parse_ollama_html("<html><body>no table</body></html>").is_err());
    assert!(
        parse_ollama_html(
            r#"<table><tr><th>Model</th><th>Input</th><th>Cached input</th><th>Output</th></tr></table>"#
        )
        .is_err()
    );
    assert!(
        parse_ollama_html(
            r#"<table><tr><th>Model</th><th>Input</th><th>Cached input</th><th>Output</th></tr>
           <tr><td>duplicate</td><td>$0.1</td><td>$0.05</td><td>$0.2</td></tr>
           <tr><td>duplicate</td><td>$0.1</td><td>$0.05</td><td>$0.2</td></tr></table>"#
        )
        .is_err()
    );
    assert!(
        parse_ollama_html(
            r#"<table><tr><th>Model</th><th>Input</th><th>Cached input</th><th>Output</th></tr>
           <tr><td>only-one</td><td>$0.1</td></tr></table>"#
        )
        .is_err()
    );
}

#[test]
fn ollama_manual_pricing_refresh_uses_the_verified_official_source() {
    let capability = provider_pricing_capability(OLLAMA_PROVIDER_ID).unwrap();
    assert_eq!(capability.evidence, ProviderPricingEvidence::Verified);
    assert!(!capability.experimental);
    assert_eq!(capability.source_url, Some(OLLAMA_SOURCE_URL));
    assert!(capability.manual_refresh_available);
}

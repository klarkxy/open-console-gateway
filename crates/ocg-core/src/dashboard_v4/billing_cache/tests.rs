use super::*;
use crate::billing_types::{BillingModel, BillingSource};

fn status(id: &str) -> BillingStatus {
    BillingStatus {
        account_id: id.into(),
        model: BillingModel::Cash,
        surface_kind: crate::billing_types::BillingSurfaceKind::CashBalances,
        source: BillingSource::Unavailable,
        unit: "currency".into(),
        configurable_credits: false,
        manual_calibration: false,
        quota_manual_calibration: false,
        provider_windows: false,
        quota_editor_limits: Vec::new(),
        official_refresh: true,
        usage: None,
        cash: None,
        credits: None,
        presets: vec![],
        revision: 1,
        process_generation: 1,
    }
}
fn version() -> ReadVersion {
    ReadVersion {
        revision: 1,
        changes: 0,
        data_version: 1,
        pricing: "v1".into(),
        official_month: (2026, 10),
    }
}

#[test]
fn cache_is_bounded_expires_and_rejects_reversed_clock() {
    let mut cache = BillingReadCache::default();
    let now = Utc::now();
    let version = version();
    for id in 0..=MAX_ENTRIES {
        cache.insert(&version, status(&id.to_string()), now);
    }
    assert_eq!(cache.entries.len(), MAX_ENTRIES);
    assert!(cache.get("0", &version, now).is_none());
    assert!(cache.get("1", &version, now).is_some());
    assert!(
        cache
            .get("1", &version, now + Duration::seconds(15))
            .is_none()
    );
    assert!(
        cache
            .get("2", &version, now - Duration::seconds(1))
            .is_none()
    );
}

#[test]
fn usage_writes_external_writes_pricing_and_settings_each_invalidate() {
    for changed in [
        ReadVersion {
            changes: 1,
            ..version()
        },
        ReadVersion {
            data_version: 2,
            ..version()
        },
        ReadVersion {
            revision: 2,
            ..version()
        },
        ReadVersion {
            pricing: "v2".into(),
            ..version()
        },
        ReadVersion {
            official_month: (2026, 11),
            ..version()
        },
    ] {
        let mut cache = BillingReadCache::default();
        let now = Utc::now();
        cache.insert(&version(), status("a"), now);
        assert!(cache.get("a", &version(), now).is_some());
        assert!(cache.get("a", &changed, now).is_none());
    }
}

#[test]
fn quota_reset_deadline_expires_before_the_normal_cache_age() {
    let now = Utc::now();
    let reset = now + Duration::seconds(1);
    let mut value = status("timed");
    value.usage = Some(
        serde_json::from_value(serde_json::json!({
            "accountId":"timed", "providerId":"opencode", "availability":"available",
            "experimental":false, "freeCooldownUntil":reset.to_rfc3339(),
            "quotaWindows":[], "creditBalances":[], "syncState":null,
            "revision":1, "processGeneration":1, "pricingRevision":null
        }))
        .unwrap(),
    );
    let mut cache = BillingReadCache::default();
    cache.insert(&version(), value, now);
    assert!(cache.get("timed", &version(), now).is_some());
    assert!(cache.get("timed", &version(), reset).is_none());
}

//! Stored pricing rows still decode. URL helpers stay available for protocol pages.

use super::{
    ProviderPricingEvidence, ProviderPricingSnapshot, ProviderPricingValue,
    ProviderScopedPricingSnapshot, latest_provider_pricing_snapshot,
    store_provider_pricing_snapshot,
};
use crate::db::Database;
use crate::kernel::ids::OPENCODE_PROVIDER_ID;
use crate::kernel::pricing::PricingSnapshot;
use crate::provider::COMMAND_CODE_PROVIDER_ID;

const LEGACY_OPENCODE_GO: &str = r#"{
  "revision": "hist-rev",
  "activated_at": "2026-01-01T00:00:00Z",
  "document_updated_at": "2026-01-01T00:00:00Z",
  "source_url": "https://opencode.ai/docs/go/",
  "content_hash": "hist-hash",
  "limits": {"window_5h": 12.0, "window_week": 30.0, "window_month": 60.0},
  "models": [{
    "model_id": "glm-5.2",
    "display_name": "GLM-5.2",
    "input": 1.4,
    "output": 4.4,
    "cache_read": 0.26,
    "cache_write": null,
    "usage": 60.0,
    "quota_multiplier": 1.0,
    "min_input_tokens": null,
    "max_input_tokens": null,
    "adjustments": []
  }],
  "adjustment_policy_version": "local-v2"
}"#;

#[test]
fn legacy_opencode_go_json_decodes_without_rewriting_the_stored_row() {
    let legacy: PricingSnapshot = serde_json::from_str(LEGACY_OPENCODE_GO).unwrap();
    assert_eq!(legacy.models[0].model_id, "glm-5.2");
    assert_eq!(legacy.models[0].quota_multiplier, 1.0);
    let record = ProviderPricingSnapshot {
        provider_id: OPENCODE_PROVIDER_ID.to_string(),
        revision: legacy.revision.clone(),
        activated_at: legacy.activated_at.clone(),
        document_updated_at: Some(legacy.document_updated_at.clone()),
        source_url: legacy.source_url.clone(),
        content_hash: legacy.content_hash.clone(),
        snapshot_json: LEGACY_OPENCODE_GO.to_string(),
    };
    let loaded = ProviderScopedPricingSnapshot::from_storage_record(&record).unwrap();
    assert_eq!(record.snapshot_json, LEGACY_OPENCODE_GO);
    assert_eq!(loaded.provider_id(), OPENCODE_PROVIDER_ID);
    assert_eq!(loaded.revision(), "hist-rev");
    assert_eq!(loaded.evidence(), ProviderPricingEvidence::Verified);
    assert_eq!(loaded.values().len(), 1);
    assert_eq!(loaded.values()[0].model_id(), "glm-5.2");
    assert_eq!(loaded.values()[0].input_per_million(), Some(1.4));
    assert_eq!(loaded.values()[0].quota_multiplier(), Some(1.0));
}

#[test]
fn current_provider_snapshot_keeps_the_stored_multiplier() {
    let raw = r#"{
      "provider_id": "captured-provider",
      "revision": "hist-wire",
      "activated_at": "2030-01-01T00:00:00Z",
      "document_updated_at": null,
      "source_url": "",
      "content_hash": "",
      "evidence": "experimental",
      "values": [{
        "model_id": "captured-model",
        "display_name": "Captured",
        "input_per_million": null,
        "output_per_million": null,
        "cache_read_per_million": null,
        "cache_write_per_million": null,
        "plan_limit": 60.0,
        "model_allowance": 15.0,
        "quota_multiplier": 9.0,
        "paid_plan_price": null,
        "currency": null,
        "min_input_tokens": null,
        "max_input_tokens": null,
        "time_window": "always"
      }]
    }"#;
    let record = ProviderPricingSnapshot {
        provider_id: "captured-provider".into(),
        revision: "hist-wire".into(),
        activated_at: "2030-01-01T00:00:00Z".into(),
        document_updated_at: None,
        source_url: String::new(),
        content_hash: String::new(),
        snapshot_json: raw.into(),
    };
    let loaded = ProviderScopedPricingSnapshot::from_storage_record(&record).unwrap();
    assert_eq!(record.snapshot_json, raw);
    assert_eq!(loaded.values()[0].quota_multiplier(), Some(9.0));
    assert_eq!(loaded.values()[0].plan_limit(), Some(60.0));
    assert_eq!(loaded.values()[0].model_allowance(), Some(15.0));
}

#[test]
fn stored_provider_pricing_revision_stays_append_only() {
    let dir = std::env::temp_dir().join(format!("ocg-provider-pricing-{}", uuid::Uuid::new_v4()));
    let db = Database::open(dir.clone()).unwrap();
    let value = |name: &str| {
        ProviderPricingValue::new(
            "captured-model",
            name,
            None,
            None,
            None,
            None,
            Some(60.0),
            Some(15.0),
            None,
            None,
            None,
            None,
            super::PricingTimeWindow::Always,
        )
        .unwrap()
    };
    let snapshot = |name: &str| {
        ProviderScopedPricingSnapshot::new(
            COMMAND_CODE_PROVIDER_ID,
            "capture-1",
            "2030-01-01T00:00:00Z",
            None,
            "",
            "",
            ProviderPricingEvidence::Experimental,
            vec![value(name)],
        )
        .unwrap()
    };
    store_provider_pricing_snapshot(&db, &snapshot("first")).unwrap();
    store_provider_pricing_snapshot(&db, &snapshot("second")).unwrap();
    let loaded = latest_provider_pricing_snapshot(&db, COMMAND_CODE_PROVIDER_ID)
        .unwrap()
        .unwrap();
    assert_eq!(loaded.values()[0].display_name(), "first");
    assert_eq!(loaded.values()[0].quota_multiplier(), Some(4.0));
    drop(db);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn approved_host_redirect_stays_on_https_443() {
    let ok = reqwest::Url::parse("https://opencode.ai/docs/go/").unwrap();
    assert!(super::approved_https_host(&ok, "opencode.ai"));
    let explicit_port = reqwest::Url::parse("https://commandcode.ai:443/docs/provider").unwrap();
    assert!(super::approved_https_host(&explicit_port, "commandcode.ai"));
    for rejected in [
        "http://opencode.ai/docs/go/",
        "https://evil.example/docs/go/",
        "https://opencode.ai:8443/docs/go/",
    ] {
        let url = reqwest::Url::parse(rejected).unwrap();
        assert!(
            !super::approved_https_host(&url, "opencode.ai"),
            "{rejected}"
        );
    }
    assert_eq!(super::MAX_APPROVED_HOST_REDIRECTS, 5);
}

#[test]
fn html_table_helpers_read_a_protocol_endpoint_row() {
    let html = "<table><tr><th>Model</th><th>Model ID</th><th>Endpoint</th><th>AI SDK package</th></tr>\
<tr><td>GLM&nbsp;5.2</td><td>glm-5.2</td><td>/v1/chat</td><td>chat</td></tr></table>";
    let tables = super::extract_tables(html).unwrap();
    assert_eq!(tables.len(), 1);
    assert!(super::has_headers(
        &tables[0],
        &["model", "model id", "endpoint", "ai sdk package"]
    ));
    assert_eq!(tables[0][1][0], "GLM 5.2");
    assert_eq!(tables[0][1][1], "glm-5.2");
    assert_eq!(tables[0][1][2], "/v1/chat");
    assert_eq!(
        super::collapse_whitespace(&super::strip_tags("<p>keep <b>text</b></p>")),
        "keep text"
    );
}

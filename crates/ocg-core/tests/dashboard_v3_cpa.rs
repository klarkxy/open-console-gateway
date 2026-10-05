//! Historical CPA catalog residue and retired activation.

use chrono::{DateTime, Utc};
use ocg_core::dashboard_v3::{CpaIntegration, CpaModels};
use ocg_core::db::{CpaCatalogModel, CpaCatalogRecord, CpaIntegrationRecord};
use ocg_core::models::{Account, AccountSetupStep, AccountType};
use ocg_core::provider::{
    CPA_ACCOUNT_ID, CPA_ACCOUNT_NAME, CPA_PROVIDER_ID, CredentialKind, QuotaScope,
};
use ocg_core::state::CoreStateInner;
use reqwest::StatusCode;

#[path = "fixtures/dashboard_v3/harness.rs"]
mod harness;

use harness::start_loopback;

const HISTORICAL_SOURCE_URL: &str = "http://127.0.0.1:8317";
const RETIRED_CATALOG: &str = "dedicated CPA catalog is retired and cannot be changed";
const HISTORICAL_INFERENCE: &str = "synthetic-historical-inference";
const HISTORICAL_MANAGEMENT: &str = "synthetic-historical-management";

fn historical_refreshed_at() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2020-01-02T03:04:05Z")
        .unwrap()
        .with_timezone(&Utc)
}

fn historical_models() -> Vec<CpaCatalogModel> {
    vec![CpaCatalogModel {
        id: "gpt-5".into(),
        owned_by: Some("openai".into()),
        enabled: true,
    }]
}

fn seed_historical_residue(state: &CoreStateInner, refreshed_at: DateTime<Utc>) {
    let now = Utc::now();
    let account = Account {
        id: CPA_ACCOUNT_ID.to_string(),
        provider_id: CPA_PROVIDER_ID.to_string(),
        credential_kind: CredentialKind::ApiKey,
        quota_scope: QuotaScope::Key,
        name: CPA_ACCOUNT_NAME.to_string(),
        username: None,
        password_cipher: None,
        key_cipher: state.encrypt_key(HISTORICAL_INFERENCE).unwrap(),
        enabled: true,
        account_type: AccountType::Key,
        setup_step: AccountSetupStep::Ready,
        referral_code: None,
        purchase_date: String::new(),
        expires_on: String::new(),
        cooldown_until: None,
        cooldown_generic_until: None,
        cooldown_5h_until: None,
        cooldown_week_until: None,
        cooldown_month_until: None,
        cooldown_free_until: None,
        last_error: None,
        auth_error: None,
        notes: None,
        created_at: now,
        updated_at: now,
    };
    let management = state.encrypt_key(HISTORICAL_MANAGEMENT).unwrap();
    let db = state.db.lock();
    db.upsert_cpa_integration(&account, HISTORICAL_SOURCE_URL, &management)
        .unwrap();
    db.replace_cpa_model_catalog(&historical_models(), HISTORICAL_SOURCE_URL, refreshed_at)
        .unwrap();
}

fn stored_integration(state: &CoreStateInner) -> Option<CpaIntegrationRecord> {
    state.db.lock().cpa_integration().unwrap()
}

fn stored_catalog(state: &CoreStateInner) -> Option<CpaCatalogRecord> {
    state.db.lock().cpa_model_catalog().unwrap()
}

fn stored_account_key_cipher(state: &CoreStateInner) -> String {
    state
        .db
        .lock()
        .get_account(CPA_ACCOUNT_ID)
        .unwrap()
        .expect("historical CPA account")
        .key_cipher
}

fn stored_grant_bytes(
    state: &CoreStateInner,
) -> Vec<(String, String, bool, String, Vec<String>, Vec<String>, u64)> {
    state
        .db
        .lock()
        .list_inference_bindings()
        .unwrap()
        .into_iter()
        .map(|row| {
            (
                row.account_id,
                row.binding_id,
                row.enabled,
                format!("{:?}", row.model_scope),
                row.allowed_endpoint_ids,
                row.allowed_origins,
                row.credential_version,
            )
        })
        .collect()
}

#[tokio::test]
async fn cpa_model_catalog_get_returns_the_persisted_snapshot() {
    let harness = start_loopback("cpa-models-get").await;
    let (status, body) = harness
        .get_json(&format!(
            "{}/external-integrations/cpa/models",
            harness.v3_base
        ))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let empty: CpaModels = serde_json::from_value(body).unwrap();
    assert!(empty.models.is_empty());
    assert!(empty.source_url.is_none());

    let refreshed_at = historical_refreshed_at();
    seed_historical_residue(&harness.state, refreshed_at);
    let revision = harness.state.settings_revision();
    let generation = harness.state.process_generation();
    let integration = stored_integration(&harness.state);
    let catalog = stored_catalog(&harness.state);
    let account_key_cipher = stored_account_key_cipher(&harness.state);
    let grants = stored_grant_bytes(&harness.state);
    let stored = integration.clone().expect("historical CPA row");
    assert_eq!(stored.base_url, HISTORICAL_SOURCE_URL);
    assert_eq!(stored.account_id, CPA_ACCOUNT_ID);
    let historical_catalog = catalog.clone().expect("historical CPA catalog");
    assert_eq!(historical_catalog.models, historical_models());
    assert_eq!(historical_catalog.source_url, HISTORICAL_SOURCE_URL);
    assert_eq!(historical_catalog.refreshed_at, Some(refreshed_at));

    let refused = harness
        .state
        .activate_cpa_model_catalog(
            vec![CpaCatalogModel {
                id: "retired-must-not-land".into(),
                owned_by: Some("not-openai".into()),
                enabled: false,
            }],
            "http://192.0.2.8/retired",
            Utc::now(),
        )
        .expect_err("retired catalog activation must fail before any remote call");
    assert!(
        refused.to_string().contains(RETIRED_CATALOG),
        "activation must stay retired"
    );
    assert_eq!(stored_integration(&harness.state), integration);
    assert_eq!(stored_catalog(&harness.state), catalog);
    assert_eq!(
        stored_account_key_cipher(&harness.state),
        account_key_cipher
    );
    assert_eq!(stored_grant_bytes(&harness.state), grants);
    assert_eq!(harness.state.settings_revision(), revision);
    assert_eq!(harness.state.process_generation(), generation);

    let (status, body) = harness
        .get_json(&format!(
            "{}/external-integrations/cpa/models",
            harness.v3_base
        ))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let models: CpaModels = serde_json::from_value(body.clone()).unwrap();
    assert_eq!(models.models.len(), 1);
    assert_eq!(models.models[0].id, "gpt-5");
    assert_eq!(models.models[0].owned_by.as_deref(), Some("openai"));
    assert_eq!(models.source_url.as_deref(), Some(HISTORICAL_SOURCE_URL));
    let refreshed = refreshed_at.to_rfc3339();
    assert_eq!(models.refreshed_at.as_deref(), Some(refreshed.as_str()));
    assert_eq!(models.revision, revision);
    assert_eq!(models.process_generation, generation);
    let models_text = body.to_string();
    assert!(!models_text.contains(HISTORICAL_INFERENCE));
    assert!(!models_text.contains(HISTORICAL_MANAGEMENT));

    let (status, body) = harness
        .get_json(&format!("{}/external-integrations/cpa", harness.v3_base))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let integration_view: CpaIntegration = serde_json::from_value(body.clone()).unwrap();
    assert!(integration_view.legacy_migration_required);
    assert_ne!(integration_view.base_url, HISTORICAL_SOURCE_URL);
    assert!(!integration_view.base_url.contains("8317"));
    assert!(!integration_view.management_key_configured);
    assert!(!integration_view.inference_key_configured);
    assert!(integration_view.account_id.is_none());
    let integration_text = body.to_string();
    assert!(!integration_text.contains(HISTORICAL_INFERENCE));
    assert!(!integration_text.contains(HISTORICAL_MANAGEMENT));
    assert!(!integration_text.contains("8317"));
    harness.stop();
}

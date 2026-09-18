//! V3 characterization: dashboard CAS vs CLI/Tauri-shaped `Database` mutations.
//!
//! Tauri commands and the CLI binary are out of this crate. Their current
//! account writes go through `Database::create_account` / `update_account`
//! without `CoreState::bump_settings_revision`. Dashboard HTTP is the only
//! path that checks `expected_revision` and bumps the shared token.

use chrono::Utc;
use ocg_core::crypto::{KeyCipher, StaticKeyCipher};
use ocg_core::db::Database;
use ocg_core::gateway;
use ocg_core::models::{Account, AccountSetupStep, AccountType, AccountUpdate, ProxyMode};
use ocg_core::provider::{
    COMMAND_CODE_PROVIDER_ID, CUSTOM_PROVIDER_ID, OPENCODE_PROVIDER_ID, provider_allows_enablement,
};
use ocg_core::state::CoreStateInner;
use reqwest::StatusCode;
use serde_json::json;
use std::fs;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

const GATEWAY_KEY: &str = "gw-v3-control";

fn temp_data_dir(label: &str) -> PathBuf {
    let mut dir = std::env::temp_dir();
    dir.push(format!("ocg-v3-control-{}-{}", label, uuid::Uuid::new_v4()));
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn loopback_client() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .build()
        .expect("v3 control-plane client should build")
}

fn state(label: &str) -> Arc<CoreStateInner> {
    let dir = temp_data_dir(label);
    let db = Database::open(dir.clone()).unwrap();
    let cipher: Arc<dyn KeyCipher + Send + Sync> = Arc::new(StaticKeyCipher::new("v3-control"));
    let state = Arc::new(CoreStateInner::new(db, dir, cipher).unwrap());
    let mut config = state.config();
    config.gateway_key = GATEWAY_KEY.into();
    config.proxy_mode = ProxyMode::Direct;
    state.set_config(config).unwrap();
    state
}

fn go_account(state: &CoreStateInner, id: &str, enabled: bool) -> Account {
    let now = Utc::now();
    Account {
        id: id.to_string(),
        provider_id: OPENCODE_PROVIDER_ID.to_string(),

        credential_kind: ocg_core::provider::default_credential_kind(),
        quota_scope: ocg_core::provider::default_quota_scope(),
        name: id.to_string(),
        username: None,
        password_cipher: None,
        key_cipher: state.encrypt_key("sk-v3-go").unwrap(),
        enabled,
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
    }
}

#[tokio::test]
async fn dashboard_cas_is_not_shared_with_cli_or_tauri_shaped_db_writes() {
    let state = state("cas-split");
    let handle = gateway::start_gateway_on(state.clone(), SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .unwrap();
    let client = loopback_client();
    let base = format!("http://127.0.0.1:{}/dashboard/api/v4", handle.port);
    let revision_after_config = state.settings_revision();
    let generation = state.process_generation();

    let created: serde_json::Value = client
        .post(format!("{base}/accounts"))
        .json(&json!({
            "expectedRevision": revision_after_config,
            "processGeneration": generation,
            "providerId": OPENCODE_PROVIDER_ID,
            "name": "dashboard-go",
            "key": "sk-dashboard"
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let dashboard_revision = created["revision"].as_u64().unwrap();
    assert_eq!(dashboard_revision, revision_after_config + 1);
    assert_eq!(state.settings_revision(), dashboard_revision);
    assert_eq!(created["account"]["enabled"], true);

    let stale = client
        .post(format!("{base}/accounts"))
        .json(&json!({
            "expectedRevision": revision_after_config,
            "processGeneration": generation,
            "providerId": OPENCODE_PROVIDER_ID,
            "name": "stale-go",
            "key": "sk-stale"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(stale.status(), StatusCode::CONFLICT);
    assert_eq!(state.settings_revision(), dashboard_revision);

    let before_cli = state.settings_revision();
    state
        .db
        .lock()
        .create_account(&go_account(&state, "cli-go", true))
        .unwrap();
    assert_eq!(
        state.settings_revision(),
        before_cli,
        "CLI-shaped Database::create_account must not bump the dashboard CAS token"
    );

    let still_current = client
        .patch(format!("{base}/accounts/cli-go"))
        .json(&json!({
            "expectedRevision": before_cli,
            "processGeneration": state.process_generation(),
            "name": "cli-go-renamed"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        still_current.status(),
        StatusCode::OK,
        "dashboard CAS stays valid across CLI/Tauri-shaped db writes because those writes do not bump revision"
    );
    assert_eq!(state.settings_revision(), before_cli + 1);

    gateway::stop_gateway(handle);
    let _ = fs::remove_dir_all(state.data_dir());
}

#[test]
fn direct_db_creates_respect_catalog_enablement_without_dashboard_cas() {
    let state = state("mutation-shapes");
    let dir = state.data_dir();

    assert!(provider_allows_enablement(OPENCODE_PROVIDER_ID));
    assert!(provider_allows_enablement(CUSTOM_PROVIDER_ID));
    assert!(provider_allows_enablement(COMMAND_CODE_PROVIDER_ID));

    state
        .db
        .lock()
        .create_account(&go_account(&state, "cli-go", true))
        .expect("CLI key add persists an enabled ready Go account");

    let mut goat = go_account(&state, "tauri-goat", true);
    goat.provider_id = COMMAND_CODE_PROVIDER_ID.into();
    goat.credential_kind = ocg_core::provider::CredentialKind::ApiKey;
    state
        .db
        .lock()
        .create_account(&goat)
        .expect("catalog-routable GOAT may persist enabled=true at the Database layer");
    goat.id = "tauri-goat-disabled".into();
    goat.name = goat.id.clone();
    goat.enabled = false;
    state
        .db
        .lock()
        .create_account(&goat)
        .expect("disabled GOAT accounts persist through Database::create_account");

    let mut custom = go_account(&state, "tauri-custom", true);
    custom.provider_id = CUSTOM_PROVIDER_ID.into();
    state
        .db
        .lock()
        .create_account(&custom)
        .expect(
            "Tauri-shaped Custom create uses provider_allows_enablement and can persist enabled=true without dashboard verification",
        );
    let stored = state
        .db
        .lock()
        .get_account("tauri-custom")
        .unwrap()
        .unwrap();
    assert!(stored.enabled);
    let verification = state
        .db
        .lock()
        .account_verification_state("tauri-custom")
        .unwrap()
        .unwrap();
    assert_eq!(
        verification.status,
        ocg_core::provider::ConnectionVerificationStatus::Pending
    );

    let before = state.settings_revision();
    state
        .db
        .lock()
        .update_account(
            "cli-go",
            &AccountUpdate {
                name: Some("renamed".into()),
                username: None,
                password: None,
                key: None,
                enabled: None,
                referral_code: None,
                purchase_date: None,
                notes: None,
            },
            None,
            None,
        )
        .unwrap();
    assert_eq!(
        state.settings_revision(),
        before,
        "CLI/Tauri update_account must not bump settings_revision"
    );

    drop(state);
    let _ = fs::remove_dir_all(dir);
}

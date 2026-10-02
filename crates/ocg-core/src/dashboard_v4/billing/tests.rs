use super::*;
use crate::crypto::StaticKeyCipher;
use crate::dashboard_v3::MutationExpectation;
use crate::db::Database;
use crate::dynamic::DynamicProviderRuntime;
use crate::models::{
    Account, AccountCustomConfigInput, AccountModelCapabilityInput, AccountSetupStep, AccountType,
};
use crate::official_api::OfficialApiKind;
use crate::provider::{CUSTOM_PROVIDER_ID, CredentialKind, QuotaScope, UpstreamProtocolKind};
use crate::state::{CoreState, CoreStateInner};
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use chrono::{TimeZone, Utc};
use ocg_domain::dynamic::DynamicAuthKind;
use ocg_domain::provider::ProviderOrigin;
use std::sync::Arc;

fn state() -> (std::path::PathBuf, crate::state::CoreState) {
    let dir = std::env::temp_dir().join(format!("ocg-billing-receipt-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let db = Database::open(dir.clone()).unwrap();
    let opened = Arc::new(
        CoreStateInner::new(
            db,
            dir.clone(),
            Arc::new(StaticKeyCipher::new("billing-receipt")),
        )
        .unwrap(),
    );
    (dir, opened)
}

fn account(state: &CoreState, id: &str) {
    let now = Utc.with_ymd_and_hms(2026, 1, 15, 0, 0, 0).unwrap();
    state
        .db
        .lock()
        .create_account_with_contract(
            &Account {
                id: id.into(),
                provider_id: CUSTOM_PROVIDER_ID.into(),
                credential_kind: CredentialKind::ApiKey,
                quota_scope: QuotaScope::Key,
                name: id.into(),
                username: None,
                password_cipher: None,
                key_cipher: "cipher".into(),
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
            },
            Some(&AccountCustomConfigInput {
                endpoint_url: "https://example.test/v1/chat/completions".into(),
                upstream_protocol: crate::provider::UpstreamProtocolKind::ChatCompletions,
            }),
            &[AccountModelCapabilityInput {
                public_model: "model".into(),
                upstream_model: "model".into(),
                protocol: crate::provider::UpstreamProtocolKind::ChatCompletions,
                source: None,
            }],
        )
        .unwrap();
}

fn body(state: &CoreState, mut value: serde_json::Value) -> Bytes {
    let object = value.as_object_mut().unwrap();
    object.insert(
        "expectedRevision".into(),
        serde_json::json!(state.settings_revision()),
    );
    object.insert(
        "processGeneration".into(),
        serde_json::json!(state.process_generation()),
    );
    Bytes::from(serde_json::to_vec(&value).unwrap())
}

fn configuration() -> serde_json::Value {
    serde_json::json!({
        "name": "Personal",
        "currency": "CNY",
        "creditsPerCurrency": 1.0,
        "rates": [{
            "model": "model",
            "inputPerMillion": 10.0,
            "outputPerMillion": 20.0,
            "cacheReadPerMillion": 2.0,
            "cacheWritePerMillion": 10.0
        }],
        "monthly": null,
        "sourceUrl": null
    })
}

fn bucket(remaining: f64) -> serde_json::Value {
    serde_json::json!({
        "id": "initial",
        "kind": "manual",
        "label": "Current",
        "granted": 100.0,
        "remaining": remaining,
        "startsAt": "2020-01-01T00:00:00Z",
        "expiresAt": null
    })
}

fn remaining(status: &BillingStatus) -> f64 {
    status.credits.as_ref().unwrap().remaining
}

#[tokio::test]
async fn configure_calibrate_and_grant_return_the_committed_balance_and_revision() {
    let (dir, state) = state();
    account(&state, "credits");
    let before = state.settings_revision();

    let configured = configure(
        State(state.clone()),
        Path("credits".into()),
        body(
            &state,
            serde_json::json!({
                "configuration": configuration(),
                "initialBuckets": [bucket(75.0)]
            }),
        ),
    )
    .await
    .unwrap()
    .0;
    assert_eq!(remaining(&configured), 75.0);
    assert_eq!(
        configured.credits.as_ref().unwrap().configuration.name,
        "Personal"
    );
    assert_eq!(configured.revision, before + 1);
    assert_eq!(configured.revision, state.settings_revision());
    assert_eq!(
        configured.usage.as_ref().unwrap().revision,
        configured.revision
    );
    assert_eq!(configured.process_generation, state.process_generation());

    let calibrated = calibrate(
        State(state.clone()),
        Path("credits".into()),
        body(
            &state,
            serde_json::json!({
                "balances": [{ "bucketId": "initial", "remaining": 40.0 }]
            }),
        ),
    )
    .await
    .unwrap()
    .0;
    assert_eq!(remaining(&calibrated), 40.0);
    assert_eq!(calibrated.revision, before + 2);
    assert_eq!(calibrated.revision, state.settings_revision());

    let granted = grant(
        State(state.clone()),
        Path("credits".into()),
        body(
            &state,
            serde_json::json!({
                "label": "extra",
                "amount": 10.0,
                "expiresAt": null
            }),
        ),
    )
    .await
    .unwrap()
    .0;
    assert_eq!(remaining(&granted), 50.0);
    assert_eq!(granted.revision, before + 3);
    assert_eq!(granted.revision, state.settings_revision());
    assert_eq!(
        crate::db::billing::read_view_on(&state.db.lock().conn, "credits", Utc::now())
            .unwrap()
            .unwrap()
            .remaining,
        50.0
    );

    drop(state);
    std::fs::remove_dir_all(dir).ok();
}

#[tokio::test]
async fn projection_failure_rolls_back_the_uncommitted_meter_without_a_revision_bump() {
    let (dir, state) = state();
    account(&state, "credits");
    let _ = configure(
        State(state.clone()),
        Path("credits".into()),
        body(
            &state,
            serde_json::json!({
                "configuration": configuration(),
                "initialBuckets": [bucket(75.0)]
            }),
        ),
    )
    .await
    .unwrap();
    let revision = state.settings_revision();

    let failed = mutate(&state, "credits", &expectation(&state), |conn| {
        conn.execute(
            "UPDATE credentials SET credit_meter_json = '{' WHERE legacy_account_id = ?1",
            ["credits"],
        )?;
        Ok(())
    })
    .unwrap_err();
    assert_eq!(
        failed.into_response().status(),
        StatusCode::INTERNAL_SERVER_ERROR
    );
    assert_eq!(state.settings_revision(), revision);
    assert_eq!(
        crate::db::billing::read_view_on(&state.db.lock().conn, "credits", Utc::now())
            .unwrap()
            .unwrap()
            .remaining,
        75.0
    );

    drop(state);
    std::fs::remove_dir_all(dir).ok();
}

fn expectation(state: &CoreState) -> MutationExpectation {
    MutationExpectation {
        expected_revision: state.settings_revision(),
        process_generation: state.process_generation(),
    }
}

/// Same saved preset as `official_api::tests::runtime(OfficialApiKind::Deepseek)`.
fn deepseek_runtime() -> DynamicProviderRuntime {
    let at = Utc.with_ymd_and_hms(2026, 9, 17, 1, 0, 0).unwrap();
    DynamicProviderRuntime {
        preset_id: Some(OfficialApiKind::Deepseek.id().into()),
        id: "11111111-1111-1111-1111-111111111159".into(),
        name: "Official fixture".into(),
        endpoint_url: "https://api.deepseek.com/chat/completions".into(),
        upstream_protocol: UpstreamProtocolKind::ChatCompletions,
        auth_kind: DynamicAuthKind::Bearer,
        mappings: Vec::new(),
        created_at: at,
        updated_at: at,
        origin: ProviderOrigin::Preset,
        offering: "api".into(),
    }
}

fn official_deepseek() -> (std::path::PathBuf, CoreState, String) {
    let dir = std::env::temp_dir().join(format!("ocg-billing-official-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let db = Database::open(dir.clone()).unwrap();
    let runtime = deepseek_runtime();
    let provider_id = runtime.id.clone();
    db.create_dynamic_provider_definition(&runtime).unwrap();
    let state = Arc::new(
        CoreStateInner::new(
            db,
            dir.clone(),
            Arc::new(StaticKeyCipher::new("billing-official")),
        )
        .unwrap(),
    );
    let now = Utc.with_ymd_and_hms(2026, 1, 15, 0, 0, 0).unwrap();
    state
        .db
        .lock()
        .create_account(&Account {
            id: "credits".into(),
            provider_id: provider_id.clone(),
            credential_kind: CredentialKind::ApiKey,
            quota_scope: QuotaScope::Key,
            name: "credits".into(),
            username: None,
            password_cipher: None,
            key_cipher: "cipher".into(),
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
        })
        .unwrap();
    (dir, state, provider_id)
}

fn insert_mismatched_official_price_snapshot(state: &CoreState, provider_id: &str) {
    let sheet = serde_json::json!({
        "kind": "deepseek",
        "revision": "sheet-rev",
        "sourceUrl": "https://api-docs.deepseek.com/quick_start/pricing/",
        "observedAt": "2026-01-01T00:00:00Z",
        "validUntil": "2026-02-01T00:00:00Z",
        "rows": []
    });
    state
        .db
        .lock()
        .conn
        .execute(
            "INSERT INTO provider_pricing_snapshots
             (provider_id, revision, activated_at, document_updated_at, source_url, content_hash, snapshot_json)
             VALUES (?1, 'stored-rev', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z', ?2, 'stored', ?3)",
            rusqlite::params![
                provider_id,
                "https://api-docs.deepseek.com/quick_start/pricing/",
                sheet.to_string(),
            ],
        )
        .unwrap();
}

async fn configure_credits(state: &CoreState) -> BillingStatus {
    configure(
        State(state.clone()),
        Path("credits".into()),
        body(
            state,
            serde_json::json!({
                "configuration": configuration(),
                "initialBuckets": [bucket(75.0)]
            }),
        ),
    )
    .await
    .unwrap()
    .0
}

#[tokio::test]
async fn active_credits_ignore_a_failed_official_cash_projection() {
    let (dir, state, provider_id) = official_deepseek();
    insert_mismatched_official_price_snapshot(&state, &provider_id);
    let before = state.settings_revision();

    let configured = configure_credits(&state).await;
    assert_eq!(remaining(&configured), 75.0);
    assert_eq!(configured.model, BillingModel::Credits);
    assert!(configured.cash.is_none());
    assert_eq!(configured.revision, before + 1);
    assert_eq!(configured.revision, state.settings_revision());

    let read = status(&state, "credits").unwrap();
    assert_eq!(read.model, BillingModel::Credits);
    assert!(read.cash.is_none());
    assert_eq!(remaining(&read), 75.0);

    drop(state);
    std::fs::remove_dir_all(dir).ok();
}

#[tokio::test]
async fn disabling_credits_rolls_back_when_official_cash_projection_fails() {
    let (dir, state, provider_id) = official_deepseek();
    let configured = configure_credits(&state).await;
    assert_eq!(configured.model, BillingModel::Credits);
    insert_mismatched_official_price_snapshot(&state, &provider_id);
    let revision = state.settings_revision();

    let failed = disable(
        State(state.clone()),
        Path("credits".into()),
        body(&state, serde_json::json!({})),
    )
    .await
    .unwrap_err();
    assert_eq!(
        failed.into_response().status(),
        StatusCode::INTERNAL_SERVER_ERROR
    );
    assert_eq!(state.settings_revision(), revision);
    assert_eq!(
        crate::db::billing::read_view_on(&state.db.lock().conn, "credits", Utc::now())
            .unwrap()
            .unwrap()
            .remaining,
        75.0
    );
    let read = status(&state, "credits").unwrap();
    assert_eq!(read.model, BillingModel::Credits);
    assert!(read.cash.is_none());
    assert_eq!(remaining(&read), 75.0);

    drop(state);
    std::fs::remove_dir_all(dir).ok();
}

#[tokio::test]
async fn disabling_credits_on_an_official_provider_returns_the_cash_receipt() {
    let (dir, state, _) = official_deepseek();
    let before = state.settings_revision();
    let configured = configure_credits(&state).await;
    assert_eq!(configured.revision, before + 1);
    assert!(configured.cash.is_none());

    let disabled = disable(
        State(state.clone()),
        Path("credits".into()),
        body(&state, serde_json::json!({})),
    )
    .await
    .unwrap()
    .0;
    assert_eq!(disabled.model, BillingModel::Cash);
    assert!(disabled.credits.is_none());
    let cash = disabled.cash.as_ref().unwrap();
    assert_eq!(cash.kind, OfficialApiKind::Deepseek);
    assert_eq!(cash.account_id, "credits");
    assert_eq!(disabled.revision, before + 2);
    assert_eq!(disabled.revision, state.settings_revision());
    assert_eq!(cash.revision, disabled.revision);
    assert!(
        crate::db::billing::read_view_on(&state.db.lock().conn, "credits", Utc::now())
            .unwrap()
            .is_none()
    );

    drop(state);
    std::fs::remove_dir_all(dir).ok();
}

#[tokio::test]
async fn cached_reads_see_usage_writes_without_a_settings_revision_and_external_writes() {
    let (dir, state) = state();
    account(&state, "credits");
    let _ = configure(
        State(state.clone()),
        Path("credits".into()),
        body(
            &state,
            serde_json::json!({
                "configuration": configuration(), "initialBuckets": [bucket(75.0)]
            }),
        ),
    )
    .await
    .unwrap();
    let first = status(&state, "credits").unwrap();
    let second = status(&state, "credits").unwrap();
    assert_eq!(
        first.credits.as_ref().unwrap().estimated_at,
        second.credits.as_ref().unwrap().estimated_at
    );
    let revision = state.settings_revision();
    storage::grant_on(
        &state.db.lock().conn,
        "credits",
        "local settlement".into(),
        5.0,
        None,
        Utc::now(),
    )
    .unwrap();
    assert_eq!(state.settings_revision(), revision);
    assert_eq!(remaining(&status(&state, "credits").unwrap()), 80.0);
    let path = state.db.lock().conn.path().unwrap().to_string();
    let external = rusqlite::Connection::open(path).unwrap();
    storage::grant_on(
        &external,
        "credits",
        "other connection".into(),
        7.0,
        None,
        Utc::now(),
    )
    .unwrap();
    assert_eq!(remaining(&status(&state, "credits").unwrap()), 87.0);
    external
        .execute(
            "DELETE FROM credentials WHERE legacy_account_id = 'credits'",
            [],
        )
        .unwrap();
    assert!(
        status(&state, "credits").is_err(),
        "deleted credentials cannot return a cached balance"
    );
    drop(external);
    drop(state);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn batch_reads_isolate_errors_deduplicate_and_enforce_a_bound() {
    let (dir, state) = state();
    account(&state, "valid");
    let batch = snapshots(
        State(state.clone()),
        Bytes::from_static(br#"{"accountIds":["valid","missing","valid"]}"#),
    )
    .await
    .unwrap()
    .0;
    assert_eq!(batch.statuses.len(), 1);
    assert_eq!(batch.statuses[0].account_id, "valid");
    assert_eq!(batch.errors.len(), 1);
    assert!(batch.errors.contains_key("missing"));
    let too_many = serde_json::json!({"accountIds": vec!["valid"; 65]});
    assert!(
        snapshots(State(state.clone()), Bytes::from(too_many.to_string()))
            .await
            .is_err()
    );
    drop(state);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn cache_expires_when_a_not_yet_visible_credit_bucket_becomes_active() {
    let (dir, state) = state();
    account(&state, "future-credits");
    let start = Utc::now() + chrono::Duration::seconds(10);
    let mut future = bucket(25.0);
    future["startsAt"] = serde_json::json!(start);
    let _ = configure(
        State(state.clone()),
        Path("future-credits".into()),
        body(
            &state,
            serde_json::json!({
                "configuration": configuration(), "initialBuckets": [future]
            }),
        ),
    )
    .await
    .unwrap();
    let snapshot = status(&state, "future-credits").unwrap();
    assert!(snapshot.credits.unwrap().buckets.is_empty());
    let version =
        super::super::billing_cache::ReadVersion::capture(&state, &state.db.lock()).unwrap();
    assert!(
        state
            .billing_cache
            .lock()
            .get("future-credits", &version, start)
            .is_none()
    );
    drop(state);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn malformed_batch_bodies_do_not_mutate_control_or_billing_state() {
    let (dir, state) = state();
    let revision = state.settings_revision();
    for input in [
        r#"null"#,
        r#"{}"#,
        r#"{"accountIds":[null]}"#,
        r#"{"accountIds":[],"unknown":true}"#,
    ] {
        let error = snapshots(State(state.clone()), Bytes::from(input.to_string()))
            .await
            .unwrap_err();
        assert_eq!(error.envelope().code, "invalidJson");
        assert_eq!(state.settings_revision(), revision);
    }
    drop(state);
    let _ = std::fs::remove_dir_all(dir);
}

use super::*;
use crate::crypto::StaticKeyCipher;
use crate::dashboard_v3::MutationExpectation;
use crate::dashboard_v4::types::{
    AuthSchemeDto, DestinationModelPatch, DestinationPatchRequest, ProtocolDto,
};
use crate::db::Database;
use crate::dynamic::DynamicProviderRuntime;
use crate::models::{
    Account, AccountCustomConfigInput, AccountModelCapabilityInput, AccountSetupStep, AccountType,
};
use crate::provider::{CUSTOM_PROVIDER_ID, CredentialKind, ProviderOrigin, QuotaScope};
use crate::state::{CoreState, CoreStateInner};
use axum::Router;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::routing::any;
use chrono::Utc;
use ocg_domain::dynamic::{DynamicAuthKind, DynamicModelMapping};
use serde_json::{Value, json};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

fn open_state(tag: &str) -> (std::path::PathBuf, CoreState) {
    let dir = std::env::temp_dir().join(format!(
        "ocg-platform-observation-{tag}-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let db = Database::open(dir.clone()).unwrap();
    let state = Arc::new(
        CoreStateInner::new(db, dir.clone(), Arc::new(StaticKeyCipher::new(tag))).unwrap(),
    );
    (dir, state)
}

fn bytes(value: impl serde::Serialize) -> Bytes {
    Bytes::from(serde_json::to_vec(&value).unwrap())
}

async fn create_parent(
    state: &CoreState,
    name: &str,
    base_url: &str,
    credential: Option<&str>,
) -> String {
    let mut payload = json!({
        "kind": "new_api",
        "name": name,
        "baseUrl": base_url,
        "expectedRevision": state.settings_revision(),
        "processGeneration": state.process_generation(),
    });
    if let Some(credential) = credential {
        payload["userCredential"] = json!(credential);
    }
    let created = create(State(state.clone()), bytes(payload))
        .await
        .unwrap()
        .0;
    created.accounts[0].id.clone()
}

fn link_version(state: &CoreState, account_id: &str) -> i64 {
    state
        .db
        .lock()
        .conn
        .query_row(
            "SELECT COALESCE(link_version, 0) FROM credentials WHERE legacy_account_id = ?1",
            [account_id],
            |row| row.get(0),
        )
        .unwrap()
}

fn key_cipher(state: &CoreState, account_id: &str) -> String {
    state
        .db
        .lock()
        .conn
        .query_row(
            "SELECT key_cipher FROM credentials WHERE legacy_account_id = ?1",
            [account_id],
            |row| row.get(0),
        )
        .unwrap()
}

async fn local_site(
    on_request: Option<Arc<dyn Fn() + Send + Sync>>,
) -> (String, tokio::sync::oneshot::Sender<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = Router::new().fallback(any(move || {
        let on_request = on_request.clone();
        async move {
            if let Some(on_request) = on_request {
                on_request();
            }
            axum::Json(json!({"success": true, "data": {}}))
        }
    }));
    let (stop, stop_rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                let _ = stop_rx.await;
            })
            .await;
    });
    (format!("http://{address}"), stop)
}

#[tokio::test]
async fn parent_refresh_keeps_configuration_revision_and_advances_platform_version() {
    let (dir, state) = open_state("parent");
    let id = create_parent(&state, "Site", "https://platform.example.test", None).await;
    let revision = state.settings_revision();
    let before = view(&state).unwrap().accounts[0].version;

    let refreshed = refresh(
        State(state.clone()),
        Path(id),
        bytes(json!({
            "expectedRevision": revision,
            "processGeneration": state.process_generation(),
        })),
    )
    .await
    .unwrap()
    .0;

    assert_eq!(state.settings_revision(), revision);
    assert_eq!(refreshed.revision, revision);
    let parent = refreshed
        .accounts
        .iter()
        .find(|account| account.name == "Site")
        .unwrap();
    assert!(parent.version > before);
    assert!(
        parent
            .snapshot
            .as_ref()
            .is_some_and(|snapshot| snapshot.stale)
    );
    assert_eq!(parent.base_url, "https://platform.example.test");

    drop(state);
    std::fs::remove_dir_all(dir).ok();
}

#[tokio::test]
async fn key_refresh_keeps_configuration_revision_and_advances_link_version() {
    let (base_url, stop) = local_site(None).await;
    let (dir, state) = open_state("key");
    let parent_id = create_parent(&state, "Site", &base_url, None).await;
    let account_id = "linked-key";
    insert_custom_key(&state, account_id);
    let cipher_before = key_cipher(&state, account_id);
    let _ = link(
        State(state.clone()),
        Path(account_id.into()),
        bytes(json!({
            "platformAccountId": parent_id,
            "group": {"id": "default", "platform": null, "autoGroups": [], "verified": false},
            "expectedRevision": state.settings_revision(),
            "processGeneration": state.process_generation(),
        })),
    )
    .await
    .unwrap();
    let revision = state.settings_revision();
    let version_before = link_version(&state, account_id);

    let refreshed = refresh(
        State(state.clone()),
        Path(parent_id),
        bytes(json!({
            "accountId": account_id,
            "expectedRevision": revision,
            "processGeneration": state.process_generation(),
        })),
    )
    .await
    .unwrap()
    .0;

    assert_eq!(state.settings_revision(), revision);
    assert_eq!(refreshed.revision, revision);
    assert_eq!(link_version(&state, account_id), version_before + 1);
    assert_eq!(key_cipher(&state, account_id), cipher_before);
    assert!(
        refreshed
            .links
            .iter()
            .any(|link| link.account_id == account_id && link.snapshot.is_some())
    );

    let _ = stop.send(());
    drop(state);
    std::fs::remove_dir_all(dir).ok();
}

#[tokio::test]
async fn changed_parent_or_key_identity_rejects_the_in_flight_observation() {
    let (dir, state) = open_state("reject");
    let parent_id = Arc::new(std::sync::Mutex::new(String::new()));
    let hook_id = parent_id.clone();
    let hook_state = state.clone();
    let once = Arc::new(AtomicBool::new(false));
    let (parent_url, parent_stop) = local_site(Some(Arc::new(move || {
        if once.swap(true, Ordering::SeqCst) {
            return;
        }
        let id = hook_id.lock().unwrap().clone();
        hook_state
            .db
            .lock()
            .update_platform_account(&id, "Renamed site", None)
            .unwrap();
    })))
    .await;
    let id = create_parent(&state, "Site", &parent_url, Some("7:local-token")).await;
    *parent_id.lock().unwrap() = id.clone();
    let revision = state.settings_revision();
    let rejected = refresh(
        State(state.clone()),
        Path(id.clone()),
        bytes(json!({
            "expectedRevision": revision,
            "processGeneration": state.process_generation(),
        })),
    )
    .await
    .unwrap_err();
    assert_eq!(rejected.body.code, "conflict");
    assert!(rejected.body.message.contains("changed during refresh"));
    assert_eq!(state.settings_revision(), revision);
    let _ = parent_stop.send(());
    drop(state);
    std::fs::remove_dir_all(dir).ok();
    let _ = parent_url;
}

#[tokio::test]
async fn key_identity_change_during_refresh_rejects_the_observation() {
    let (dir, state) = open_state("key-reject");
    let once = Arc::new(AtomicBool::new(false));
    let hook_state = state.clone();
    let (key_url, key_stop) = local_site(Some(Arc::new(move || {
        if once.swap(true, Ordering::SeqCst) {
            return;
        }
        let changed = hook_state
            .db
            .lock()
            .conn
            .execute(
                "UPDATE credentials SET key_cipher = 'rotated-cipher' WHERE legacy_account_id = 'linked-key'",
                [],
            )
            .unwrap();
        assert_eq!(changed, 1, "linked key row must exist before the observation returns");
    })))
    .await;
    let key_parent = create_parent(&state, "Keys", &key_url, None).await;
    insert_custom_key(&state, "linked-key");
    let _ = link(
        State(state.clone()),
        Path("linked-key".into()),
        bytes(json!({
            "platformAccountId": key_parent,
            "group": {"id": "default", "platform": null, "autoGroups": [], "verified": false},
            "expectedRevision": state.settings_revision(),
            "processGeneration": state.process_generation(),
        })),
    )
    .await
    .unwrap();
    let revision = state.settings_revision();
    let version_before = link_version(&state, "linked-key");
    let rejected = refresh(
        State(state.clone()),
        Path(key_parent),
        bytes(json!({
            "accountId": "linked-key",
            "expectedRevision": revision,
            "processGeneration": state.process_generation(),
        })),
    )
    .await
    .unwrap_err();
    assert_eq!(rejected.body.code, "conflict");
    assert_eq!(state.settings_revision(), revision);
    assert_eq!(link_version(&state, "linked-key"), version_before);
    let _ = key_stop.send(());
    drop(state);
    std::fs::remove_dir_all(dir).ok();
}

fn insert_custom_key(state: &CoreState, id: &str) {
    let now = Utc::now();
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
                key_cipher: state.encrypt_key("sk-local-test").unwrap(),
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

#[tokio::test]
async fn refresh_keeps_an_edit_token_usable_for_alias_and_destination_writes() {
    let (dir, state) = open_state("token");
    let _ = create_parent(&state, "Site", "https://platform.example.test", None).await;
    let revision = state.settings_revision();
    let _ = refresh(
        State(state.clone()),
        Path(view(&state).unwrap().accounts[0].id.clone()),
        bytes(json!({
            "expectedRevision": revision,
            "processGeneration": state.process_generation(),
        })),
    )
    .await
    .unwrap();
    assert_eq!(state.settings_revision(), revision);

    state.set_dashboard_local_mode(true);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = crate::dashboard_v4::api_router(state.clone()).with_state(state.clone());
    let (stop, stop_rx) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                let _ = stop_rx.await;
            })
            .await;
    });
    let client = reqwest::Client::new();
    let alias = client
        .patch(format!("http://{address}/alias-publication"))
        .json(&json!({
            "expectedRevision": revision,
            "processGeneration": state.process_generation(),
            "publicModel": "Alias-Token",
            "published": false
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(alias.status(), reqwest::StatusCode::OK);
    let alias: Value = alias.json().await.unwrap();
    assert_eq!(alias["revision"]["revision"], json!(revision + 1));
    assert!(
        alias["unpublished"]
            .as_array()
            .unwrap()
            .iter()
            .any(|name| name == "alias-token")
    );
    let _ = stop.send(());

    drop(state);
    std::fs::remove_dir_all(dir).ok();

    let dir =
        std::env::temp_dir().join(format!("ocg-platform-destination-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let db = Database::open(dir.clone()).unwrap();
    let now = Utc::now();
    db.create_dynamic_provider_definition(&DynamicProviderRuntime {
        preset_id: None,
        id: "observation-destination".into(),
        name: "Before".into(),
        endpoint_url: "https://before.example/v1".into(),
        upstream_protocol: ocg_domain::catalog::UpstreamProtocolKind::ChatCompletions,
        auth_kind: DynamicAuthKind::Bearer,
        mappings: vec![DynamicModelMapping {
            public_model: "public-before".into(),
            upstream_model: "upstream-before".into(),
            upstream_override: None,
        }],
        created_at: now,
        updated_at: now,
        origin: ProviderOrigin::Custom,
        offering: "api".into(),
    })
    .unwrap();
    let state = Arc::new(
        CoreStateInner::new(
            db,
            dir.clone(),
            Arc::new(StaticKeyCipher::new("destination-token")),
        )
        .unwrap(),
    );
    let _ = create_parent(&state, "Site", "https://platform.example.test", None).await;
    let revision = state.settings_revision();
    let _ = refresh(
        State(state.clone()),
        Path(view(&state).unwrap().accounts[0].id.clone()),
        bytes(json!({
            "expectedRevision": revision,
            "processGeneration": state.process_generation(),
        })),
    )
    .await
    .unwrap();
    state.set_dashboard_local_mode(true);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = crate::dashboard_v4::api_router(state.clone()).with_state(state.clone());
    let (stop, stop_rx) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                let _ = stop_rx.await;
            })
            .await;
    });
    let destination_id =
        ocg_domain::destination::destination_id_for_dynamic("observation-destination");
    let response = reqwest::Client::new()
        .patch(format!("http://{address}/destinations/{destination_id}"))
        .json(&DestinationPatchRequest {
            expectation: MutationExpectation {
                expected_revision: revision,
                process_generation: state.process_generation(),
            },
            name: "After".into(),
            endpoint_url: "https://after.example/v1".into(),
            upstream_protocol: ProtocolDto::ChatCompletions,
            protocol_routes: None,
            auth_scheme: AuthSchemeDto::Bearer,
            models: vec![DestinationModelPatch {
                public_model: "public-before".into(),
                upstream_model: "upstream-before".into(),
                upstream_override: None,
                enabled: None,
                protocols: None,
                preferred: None,
            }],
            authorize_credential_ids: Vec::new(),
            enabled: None,
        })
        .send()
        .await
        .unwrap();
    let status = response.status();
    let patched: Value = response.json().await.unwrap();
    assert_eq!(status, reqwest::StatusCode::OK, "{patched}");
    assert_eq!(patched["destination"]["name"], "After");
    assert!(patched["revision"]["revision"].as_u64().unwrap() > revision);
    let _ = stop.send(());

    drop(state);
    std::fs::remove_dir_all(dir).ok();
}

#[tokio::test]
async fn configuration_writes_still_advance_settings_revision() {
    let (dir, state) = open_state("config");
    let initial = state.settings_revision();
    let id = create_parent(&state, "Site", "https://platform.example.test", None).await;
    assert_eq!(state.settings_revision(), initial + 1);

    let _ = update(
        State(state.clone()),
        Path(id.clone()),
        bytes(json!({
            "name": "Renamed",
            "expectedRevision": state.settings_revision(),
            "processGeneration": state.process_generation(),
        })),
    )
    .await
    .unwrap();
    assert_eq!(state.settings_revision(), initial + 2);

    insert_custom_key(&state, "linked-key");
    let _ = link(
        State(state.clone()),
        Path("linked-key".into()),
        bytes(json!({
            "platformAccountId": id,
            "group": {"id": "default", "platform": null, "autoGroups": [], "verified": false},
            "expectedRevision": state.settings_revision(),
            "processGeneration": state.process_generation(),
        })),
    )
    .await
    .unwrap();
    assert_eq!(state.settings_revision(), initial + 3);

    let _ = unlink(
        State(state.clone()),
        Path("linked-key".into()),
        bytes(json!({
            "expectedRevision": state.settings_revision(),
            "processGeneration": state.process_generation(),
        })),
    )
    .await
    .unwrap();
    assert_eq!(state.settings_revision(), initial + 4);

    let _ = delete(
        State(state.clone()),
        Path(id),
        bytes(json!({
            "expectedRevision": state.settings_revision(),
            "processGeneration": state.process_generation(),
        })),
    )
    .await
    .unwrap();
    assert_eq!(state.settings_revision(), initial + 5);

    drop(state);
    std::fs::remove_dir_all(dir).ok();
}

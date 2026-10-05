use super::super::types::ProtocolDto;
use super::*;
use crate::crypto::{KeyCipher, StaticKeyCipher};
use crate::db::Database;
use crate::dynamic::DynamicProviderRuntime;
use crate::models::{Account, AccountSetupStep, AccountType, ProxyMode};
use crate::provider::{CredentialKind, ProviderOrigin, QuotaScope, UpstreamProtocolKind};
use crate::state::CoreStateInner;
use axum::body::Bytes;
use axum::extract::{Path, State};
use chrono::Utc;
use ocg_domain::destination::{
    AuthScheme, CatalogModel, HttpProtocolRoute, Protocol, destination_id_for_dynamic,
};
use ocg_domain::dynamic::{DynamicAuthKind, DynamicModelMapping, DynamicModelUpstreamOverride};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{mpsc, oneshot};

const SECRET: &str = "refresh-secret-must-not-leak";

struct Fixture {
    state: Option<CoreState>,
    dir: PathBuf,
    provider_id: String,
    account_id: String,
    cipher: Arc<StaticKeyCipher>,
}

impl Fixture {
    fn destination_id(&self) -> String {
        destination_id_for_dynamic(&self.provider_id)
    }

    fn expectation(&self) -> MutationExpectation {
        MutationExpectation {
            expected_revision: self.state.as_ref().unwrap().settings_revision(),
            process_generation: self.state.as_ref().unwrap().process_generation(),
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.state.take();
        std::fs::remove_dir_all(&self.dir).ok();
    }
}

fn fixture(
    label: &str,
    endpoint: &str,
    auth_kind: DynamicAuthKind,
    with_key: bool,
    stepfun: bool,
) -> Fixture {
    let dir = std::env::temp_dir().join(format!(
        "ocg-destination-catalog-{label}-{}",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let db = Database::open(dir.clone()).unwrap();
    let cipher = Arc::new(StaticKeyCipher::new("destination-catalog-test"));
    let provider_id = if stepfun {
        "stepfun-plan"
    } else {
        "generic-http"
    }
    .to_string();
    let account_id = format!("{label}-account");
    let now = Utc::now();
    db.create_dynamic_provider_definition(&DynamicProviderRuntime {
        preset_id: stepfun.then(|| "stepfun-plan".into()),
        id: provider_id.clone(),
        name: if stepfun {
            "StepFun Plan"
        } else {
            "Generic HTTP"
        }
        .into(),
        endpoint_url: endpoint.into(),
        upstream_protocol: UpstreamProtocolKind::ChatCompletions,
        auth_kind,
        mappings: vec![DynamicModelMapping {
            public_model: "alias-keep".into(),
            upstream_model: "old-upstream".into(),
            upstream_override: Some(DynamicModelUpstreamOverride {
                protocol: UpstreamProtocolKind::Messages,
                endpoint_url: "https://override.example/messages".into(),
            }),
        }],
        created_at: now,
        updated_at: now,
        origin: ProviderOrigin::Custom,
        offering: if stepfun { "plan" } else { "api" }.into(),
    })
    .unwrap();
    let destination_id = destination_id_for_dynamic(&provider_id);
    if with_key {
        db.create_account(&Account {
            id: account_id.clone(),
            provider_id: provider_id.clone(),
            credential_kind: CredentialKind::ApiKey,
            quota_scope: QuotaScope::Key,
            name: account_id.clone(),
            username: None,
            password_cipher: None,
            key_cipher: cipher.encrypt(SECRET).unwrap(),
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
    }
    crate::db::destination_store::replace_destination_catalog(
        &db.conn,
        &destination_id,
        &[CatalogModel {
            public_model: "alias-keep".into(),
            upstream_model: "old-upstream".into(),
            protocols: vec![Protocol::ChatCompletions],
            preferred: Some(Protocol::ChatCompletions),
            enabled: false,
            upstream_override: Some(DynamicModelUpstreamOverride {
                protocol: UpstreamProtocolKind::Messages,
                endpoint_url: "https://override.example/messages".into(),
            }),
        }],
    )
    .unwrap();
    let state: CoreState = Arc::new(CoreStateInner::new(db, dir.clone(), cipher.clone()).unwrap());
    let mut config = state.config();
    config.proxy_mode = ProxyMode::Direct;
    state.set_config(config).unwrap();
    Fixture {
        state: Some(state),
        dir,
        provider_id,
        account_id,
        cipher,
    }
}

async fn start_models_upstream(
    responses: Vec<String>,
) -> (
    String,
    mpsc::UnboundedReceiver<String>,
    tokio::task::JoinHandle<()>,
) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!(
        "http://{}/v1/chat/completions",
        listener.local_addr().unwrap()
    );
    let (requests_tx, requests_rx) = mpsc::unbounded_channel();
    let task = tokio::spawn(async move {
        for body in responses {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            let mut request = vec![0_u8; 8192];
            let count = stream.read(&mut request).await.unwrap_or(0);
            let _ = requests_tx.send(String::from_utf8_lossy(&request[..count]).to_string());
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len(),
            );
            let _ = stream.write_all(response.as_bytes()).await;
        }
    });
    (endpoint, requests_rx, task)
}

async fn refresh_once(fixture: &Fixture) -> Result<DestinationCatalogRefreshResult, V3ApiError> {
    let state = fixture.state.as_ref().unwrap().clone();
    refresh(
        State(state),
        Path(fixture.destination_id()),
        Bytes::from(serde_json::to_vec(&fixture.expectation()).unwrap()),
    )
    .await
    .map(|value| value.0)
}

fn stored_catalog(fixture: &Fixture) -> Vec<CatalogModel> {
    crate::db::destination_store::load_destination_catalog(
        &fixture.state.as_ref().unwrap().db.lock().conn,
        &fixture.destination_id(),
    )
    .unwrap()
}

fn install_chat_and_messages_routes(fixture: &Fixture) {
    let chat_url = fixture
        .state
        .as_ref()
        .unwrap()
        .db
        .lock()
        .conn
        .query_row(
            "SELECT base_url FROM destinations WHERE id = ?1",
            [fixture.destination_id()],
            |row| row.get::<_, String>(0),
        )
        .unwrap();
    let origin = chat_url
        .strip_suffix("/v1/chat/completions")
        .expect("fixture has the chat completion endpoint");
    let routes = vec![
        HttpProtocolRoute {
            protocol: Protocol::ChatCompletions,
            endpoint_url: chat_url.clone(),
            auth_scheme: AuthScheme::Bearer,
        },
        HttpProtocolRoute {
            protocol: Protocol::Messages,
            endpoint_url: format!("{origin}/anthropic/v1/messages"),
            auth_scheme: AuthScheme::XApiKey,
        },
    ];
    let protocols: Vec<_> = routes.iter().map(|route| route.protocol).collect();
    let state = fixture.state.as_ref().unwrap();
    state
        .db
        .lock()
        .conn
        .execute(
            "UPDATE destinations SET protocol_routes_json = ?2, protocols_json = ?3 WHERE id = ?1",
            rusqlite::params![
                fixture.destination_id(),
                serde_json::to_string(&routes).unwrap(),
                serde_json::to_string(&protocols).unwrap(),
            ],
        )
        .unwrap();
}

fn replace_catalog(fixture: &Fixture, catalog: &[CatalogModel]) {
    crate::db::destination_store::replace_destination_catalog(
        &fixture.state.as_ref().unwrap().db.lock().conn,
        &fixture.destination_id(),
        catalog,
    )
    .unwrap();
}

fn add_multi_route_model(fixture: &Fixture) {
    let mut catalog = stored_catalog(fixture);
    catalog.push(CatalogModel {
        public_model: "multi-model".into(),
        upstream_model: "multi-upstream".into(),
        protocols: vec![Protocol::ChatCompletions, Protocol::Messages],
        preferred: Some(Protocol::ChatCompletions),
        enabled: true,
        upstream_override: None,
    });
    replace_catalog(fixture, &catalog);
}

fn message_endpoint_id(fixture: &Fixture) -> String {
    let state = fixture.state.as_ref().unwrap();
    let db = state.db.lock();
    let snapshot = RoutingSnapshot::load(&db).unwrap();
    let destination = snapshot
        .projection
        .destinations
        .iter()
        .find(|row| row.id == fixture.destination_id())
        .unwrap();
    let model = destination
        .catalog
        .iter()
        .find(|row| row.public_model == "multi-model")
        .unwrap();
    let credential = snapshot
        .credentials
        .iter()
        .find(|row| row.destination_id == fixture.destination_id())
        .unwrap();
    crate::gateway::materialize::endpoint_id_for_target(
        credential,
        destination,
        model,
        crate::gateway::protocol::ApiFormat::Messages,
    )
    .unwrap()
}

fn grant_chat_model(fixture: &Fixture, public_model: &str) {
    let state = fixture.state.as_ref().unwrap();
    let db = state.db.lock();
    let snapshot = RoutingSnapshot::load(&db).unwrap();
    let destination = snapshot
        .projection
        .destinations
        .iter()
        .find(|row| row.id == fixture.destination_id())
        .unwrap();
    let model = destination
        .catalog
        .iter()
        .find(|row| row.public_model == public_model)
        .unwrap();
    let credential = snapshot
        .credentials
        .iter()
        .find(|row| row.id == fixture.account_id)
        .unwrap();
    let endpoint_id = crate::gateway::materialize::endpoint_id_for_target(
        credential,
        destination,
        model,
        crate::gateway::protocol::ApiFormat::ChatCompletions,
    )
    .unwrap();
    db.conn
        .execute(
            "INSERT OR IGNORE INTO credential_grants (credential_id, kind, value)
             SELECT id, 'endpoint_id', ?2 FROM credentials WHERE legacy_account_id = ?1",
            rusqlite::params![fixture.account_id, endpoint_id],
        )
        .unwrap();
}

fn grant_alias_keep_override(fixture: &Fixture) {
    let state = fixture.state.as_ref().unwrap();
    let db = state.db.lock();
    let snapshot = RoutingSnapshot::load(&db).unwrap();
    let destination = snapshot
        .projection
        .destinations
        .iter()
        .find(|row| row.id == fixture.destination_id())
        .unwrap();
    let model = destination
        .catalog
        .iter()
        .find(|row| row.public_model == "alias-keep")
        .unwrap();
    let credential = snapshot
        .credentials
        .iter()
        .find(|row| row.id == fixture.account_id)
        .unwrap();
    let endpoint_id = crate::gateway::materialize::endpoint_id_for_target(
        credential,
        destination,
        model,
        crate::gateway::protocol::ApiFormat::Messages,
    )
    .unwrap();
    db.conn
        .execute(
            "INSERT OR IGNORE INTO credential_grants (credential_id, kind, value)
             SELECT id, 'endpoint_id', ?2 FROM credentials WHERE legacy_account_id = ?1",
            rusqlite::params![fixture.account_id, endpoint_id],
        )
        .unwrap();
    db.conn
        .execute(
            "INSERT OR IGNORE INTO credential_grants (credential_id, kind, value)
             SELECT id, 'origin', ?2 FROM credentials WHERE legacy_account_id = ?1",
            rusqlite::params![fixture.account_id, "https://override.example"],
        )
        .unwrap();
}

fn grant_message_endpoint(fixture: &Fixture) {
    let endpoint_id = message_endpoint_id(fixture);
    fixture
        .state
        .as_ref()
        .unwrap()
        .db
        .lock()
        .conn
        .execute(
            "INSERT OR IGNORE INTO credential_grants (credential_id, kind, value)
             SELECT id, 'endpoint_id', ?2 FROM credentials WHERE legacy_account_id = ?1",
            rusqlite::params![fixture.account_id, endpoint_id],
        )
        .unwrap();
}

async fn update_once(
    fixture: &Fixture,
    updates: Vec<DestinationCatalogModelUpdate>,
    remove_models: Vec<String>,
) -> Result<DestinationPatchResult, super::super::destinations::DestinationsError> {
    update(
        State(fixture.state.as_ref().unwrap().clone()),
        Path(fixture.destination_id()),
        Bytes::from(
            serde_json::to_vec(&DestinationCatalogUpdate {
                expectation: fixture.expectation(),
                updates,
                remove_models,
            })
            .unwrap(),
        ),
    )
    .await
    .map(|value| value.0)
}

async fn test_once(
    fixture: &Fixture,
    expectation: MutationExpectation,
) -> Result<DestinationModelTestResult, V3ApiError> {
    test_public(fixture, expectation, "multi-model", ProtocolDto::Messages).await
}

async fn test_public(
    fixture: &Fixture,
    expectation: MutationExpectation,
    public_model: &str,
    protocol: ProtocolDto,
) -> Result<DestinationModelTestResult, V3ApiError> {
    test_model(
        State(fixture.state.as_ref().unwrap().clone()),
        Path(fixture.destination_id()),
        Bytes::from(
            serde_json::to_vec(&DestinationModelTestRequest {
                expectation,
                public_model: public_model.into(),
                protocol,
            })
            .unwrap(),
        ),
    )
    .await
    .map(|value| value.0)
}

fn error_text(error: &V3ApiError) -> String {
    format!("{error:?}")
}

async fn assert_silent(requests: &mut mpsc::UnboundedReceiver<String>) {
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(100), requests.recv())
            .await
            .is_err(),
        "model test must not open a provider connection"
    );
}

fn credential_admission_row(fixture: &Fixture) -> (i64, String, String) {
    fixture
        .state
        .as_ref()
        .unwrap()
        .db
        .lock()
        .conn
        .query_row(
            "SELECT enabled, account_type, setup_step FROM credentials WHERE legacy_account_id = ?1",
            [&fixture.account_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap()
}

fn verification_stamp(fixture: &Fixture) -> (String, Option<String>, String) {
    fixture
        .state
        .as_ref()
        .unwrap()
        .db
        .lock()
        .conn
        .query_row(
            "SELECT verification_status, connection_verified_at, setup_step
             FROM credentials WHERE legacy_account_id = ?1",
            [&fixture.account_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap()
}

fn set_credential_column(fixture: &Fixture, sql: &str, value: &str) {
    fixture
        .state
        .as_ref()
        .unwrap()
        .db
        .lock()
        .conn
        .execute(sql, rusqlite::params![fixture.account_id, value])
        .unwrap();
}

fn insert_keyed_account(
    fixture: &Fixture,
    account_type: AccountType,
    step: AccountSetupStep,
    enabled: bool,
    key: &str,
) {
    let now = Utc::now();
    fixture
        .state
        .as_ref()
        .unwrap()
        .db
        .lock()
        .create_account(&Account {
            id: fixture.account_id.clone(),
            provider_id: fixture.provider_id.clone(),
            credential_kind: CredentialKind::ApiKey,
            quota_scope: QuotaScope::Key,
            name: fixture.account_id.clone(),
            username: None,
            password_cipher: None,
            key_cipher: fixture.cipher.encrypt(key).unwrap(),
            enabled,
            account_type,
            setup_step: step,
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
}

fn assert_forwarded_pin_refusal(text: &str) {
    assert!(text.contains("preconditionFailed"), "{text}");
    assert!(
        text.contains("validated protocol pin is not enforced"),
        "{text}"
    );
    assert!(!text.contains("save an enabled"), "{text}");
}

fn assert_local_model_refusal(text: &str) {
    assert!(
        text.contains("save an enabled credential") || text.contains("keyless credential"),
        "{text}"
    );
    assert!(!text.contains("validated protocol pin"), "{text}");
}

fn use_local_chat_model(fixture: &Fixture) {
    replace_catalog(
        fixture,
        &[CatalogModel {
            public_model: "local-model".into(),
            upstream_model: "local-upstream".into(),
            protocols: vec![Protocol::ChatCompletions],
            preferred: Some(Protocol::ChatCompletions),
            enabled: false,
            upstream_override: None,
        }],
    );
}

#[tokio::test]
async fn refresh_imports_stepfun_and_generic_http_models_without_replacing_saved_routes() {
    for (label, stepfun) in [("stepfun", true), ("generic", false)] {
        let (endpoint, mut requests, task) = start_models_upstream(vec![
            format!(
                r#"{{"data":[{{"id":"old-upstream"}},{{"id":"fresh-model"}},{{"id":"{SECRET}"}}]}}"#
            ),
            r#"{"data":[{"id":"fresh-model"}]}"#.into(),
        ])
        .await;
        let fixture = fixture(label, &endpoint, DynamicAuthKind::Bearer, true, stepfun);

        let first = refresh_once(&fixture).await.unwrap();
        assert_eq!(first.added_count, 1);
        assert_eq!(first.destination.catalog.len(), 2);
        assert!(!serde_json::to_string(&first).unwrap().contains(SECRET));
        let request = requests.recv().await.unwrap();
        assert!(
            request.starts_with("GET /v1/models HTTP/1.1\r\n"),
            "{request}"
        );
        assert!(
            request
                .to_ascii_lowercase()
                .contains(&format!("authorization: bearer {SECRET}").to_ascii_lowercase()),
            "{request}"
        );

        let catalog = stored_catalog(&fixture);
        let old = catalog
            .iter()
            .find(|model| model.public_model == "alias-keep")
            .unwrap();
        assert_eq!(old.upstream_model, "old-upstream");
        assert!(!old.enabled);
        assert_eq!(
            old.upstream_override.as_ref().unwrap().endpoint_url,
            "https://override.example/messages"
        );
        let fresh = catalog
            .iter()
            .find(|model| model.upstream_model == "fresh-model")
            .unwrap();
        assert_eq!(fresh.public_model, "fresh-model");
        assert!(fresh.enabled, "discovery must default new models on");
        assert!(catalog.iter().all(|model| model.upstream_model != SECRET));

        let second = refresh_once(&fixture).await.unwrap();
        assert_eq!(second.added_count, 0, "same inventory must be idempotent");
        assert_eq!(stored_catalog(&fixture), catalog);
        let _ = requests.recv().await.unwrap();
        task.await.unwrap();
    }
}

#[tokio::test]
async fn refresh_honors_no_auth_and_x_api_key_destinations() {
    let (endpoint, mut requests, task) = start_models_upstream(vec![
        r#"{"data":[{"id":"anonymous-model"}]}"#.into(),
        r#"{"data":[{"id":"x-key-model"}]}"#.into(),
    ])
    .await;
    let anonymous = fixture("anonymous", &endpoint, DynamicAuthKind::None, false, false);
    let x_key = fixture("x-key", &endpoint, DynamicAuthKind::XApiKey, true, false);

    assert_eq!(refresh_once(&anonymous).await.unwrap().added_count, 1);
    let anonymous_request = requests.recv().await.unwrap();
    assert!(
        !anonymous_request
            .to_ascii_lowercase()
            .contains("authorization:")
    );
    assert!(
        !anonymous_request
            .to_ascii_lowercase()
            .contains("x-api-key:")
    );

    assert_eq!(refresh_once(&x_key).await.unwrap().added_count, 1);
    let x_key_request = requests.recv().await.unwrap();
    assert!(
        x_key_request
            .to_ascii_lowercase()
            .contains(&format!("x-api-key: {SECRET}").to_ascii_lowercase())
    );
    assert!(
        !x_key_request
            .to_ascii_lowercase()
            .contains("authorization:")
    );
    task.await.unwrap();
}

#[tokio::test]
async fn refresh_refuses_missing_or_unauthorized_keys_before_outbound_io() {
    for (label, with_key, revoke_grants) in
        [("missing", false, false), ("unauthorized", true, true)]
    {
        let (endpoint, mut requests, task) =
            start_models_upstream(vec![r#"{"data":[{"id":"must-not-arrive"}]}"#.into()]).await;
        let fixture = fixture(label, &endpoint, DynamicAuthKind::Bearer, with_key, false);
        if revoke_grants {
            fixture
                .state
                .as_ref()
                .unwrap()
                .db
                .lock()
                .conn
                .execute(
                    "DELETE FROM credential_grants
                     WHERE credential_id = (SELECT id FROM credentials WHERE legacy_account_id = ?1)
                       AND kind = 'endpoint_id'",
                    [&fixture.account_id],
                )
                .unwrap();
            let surviving_origins: i64 = fixture
                .state
                .as_ref()
                .unwrap()
                .db
                .lock()
                .conn
                .query_row(
                    "SELECT COUNT(*) FROM credential_grants
                     WHERE credential_id = (SELECT id FROM credentials WHERE legacy_account_id = ?1)
                       AND kind = 'origin'",
                    [&fixture.account_id],
                    |row| row.get(0),
                )
                .unwrap();
            assert!(
                surviving_origins > 0,
                "origin grant must remain for this refusal"
            );
        }
        assert!(refresh_once(&fixture).await.is_err());
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(100), requests.recv())
                .await
                .is_err()
        );
        task.abort();
    }
}

#[tokio::test]
async fn refresh_keeps_last_catalog_after_upstream_error_or_empty_inventory() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!(
        "http://{}/v1/chat/completions",
        listener.local_addr().unwrap()
    );
    let task = tokio::spawn(async move {
        for (status, body) in [
            ("500 Internal Server Error", "failure"),
            ("200 OK", r#"{"data":[]}"#),
        ] {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            let mut request = [0_u8; 4096];
            let _ = stream.read(&mut request).await;
            let response = format!(
                "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes()).await;
        }
    });
    let fixture = fixture("retain", &endpoint, DynamicAuthKind::Bearer, true, false);
    let before = stored_catalog(&fixture);
    assert!(refresh_once(&fixture).await.is_err());
    assert_eq!(stored_catalog(&fixture), before);
    assert!(refresh_once(&fixture).await.is_err());
    assert_eq!(stored_catalog(&fixture), before);
    task.await.unwrap();
}

#[tokio::test]
async fn catalog_update_persists_multi_protocol_controls_and_removals() {
    let (endpoint, _requests, task) =
        start_models_upstream(vec![r#"{"data":[{"id":"unused"}]}"#.into()]).await;
    let fixture = fixture(
        "catalog-update",
        &endpoint,
        DynamicAuthKind::Bearer,
        true,
        false,
    );
    install_chat_and_messages_routes(&fixture);
    add_multi_route_model(&fixture);

    let updated = update_once(
        &fixture,
        vec![DestinationCatalogModelUpdate {
            public_model: "multi-model".into(),
            enabled: Some(true),
            protocols: Some(vec![ProtocolDto::Messages]),
            preferred: Some(ProtocolDto::Messages),
        }],
        Vec::new(),
    )
    .await
    .unwrap();
    let model = updated
        .destination
        .catalog
        .iter()
        .find(|row| row.public_model == "multi-model")
        .unwrap();
    assert!(model.enabled);
    assert_eq!(model.protocols, vec![ProtocolDto::Messages]);
    assert_eq!(model.preferred, Some(ProtocolDto::Messages));

    let reloaded = RoutingSnapshot::load(&fixture.state.as_ref().unwrap().db.lock()).unwrap();
    let reloaded_model = reloaded
        .projection
        .destinations
        .iter()
        .find(|row| row.id == fixture.destination_id())
        .unwrap()
        .catalog
        .iter()
        .find(|row| row.public_model == "multi-model")
        .unwrap();
    assert_eq!(reloaded_model.protocols, vec![Protocol::Messages]);
    assert_eq!(reloaded_model.preferred, Some(Protocol::Messages));
    assert!(reloaded_model.enabled);

    update_once(&fixture, Vec::new(), vec!["alias-keep".into()])
        .await
        .unwrap();
    let after_remove = stored_catalog(&fixture);
    assert!(
        after_remove
            .iter()
            .any(|row| row.public_model == "multi-model")
    );
    assert!(
        !after_remove
            .iter()
            .any(|row| row.public_model == "alias-keep")
    );
    task.abort();
}

#[tokio::test]
async fn catalog_update_rejects_invalid_models_protocols_and_stale_cas_without_writing() {
    let (endpoint, _requests, task) =
        start_models_upstream(vec![r#"{"data":[{"id":"unused"}]}"#.into()]).await;
    let fixture = fixture(
        "catalog-refuse",
        &endpoint,
        DynamicAuthKind::Bearer,
        true,
        false,
    );
    install_chat_and_messages_routes(&fixture);
    add_multi_route_model(&fixture);
    let before = stored_catalog(&fixture);

    for updates in [
        vec![DestinationCatalogModelUpdate {
            public_model: "unknown".into(),
            enabled: Some(true),
            protocols: None,
            preferred: None,
        }],
        vec![
            DestinationCatalogModelUpdate {
                public_model: "multi-model".into(),
                enabled: Some(false),
                protocols: None,
                preferred: None,
            },
            DestinationCatalogModelUpdate {
                public_model: "MULTI-MODEL".into(),
                enabled: Some(true),
                protocols: None,
                preferred: None,
            },
        ],
        vec![DestinationCatalogModelUpdate {
            public_model: "multi-model".into(),
            enabled: Some(true),
            protocols: Some(vec![ProtocolDto::Responses]),
            preferred: Some(ProtocolDto::Responses),
        }],
        vec![DestinationCatalogModelUpdate {
            public_model: "alias-keep".into(),
            enabled: Some(true),
            protocols: Some(vec![ProtocolDto::ChatCompletions]),
            preferred: Some(ProtocolDto::ChatCompletions),
        }],
    ] {
        assert!(update_once(&fixture, updates, Vec::new()).await.is_err());
        assert_eq!(stored_catalog(&fixture), before);
    }
    assert!(
        update_once(&fixture, Vec::new(), vec!["missing".into()])
            .await
            .is_err()
    );
    assert_eq!(stored_catalog(&fixture), before);

    let stale = fixture.expectation();
    fixture.state.as_ref().unwrap().bump_settings_revision();
    let stale_result = update(
        State(fixture.state.as_ref().unwrap().clone()),
        Path(fixture.destination_id()),
        Bytes::from(
            serde_json::to_vec(&DestinationCatalogUpdate {
                expectation: stale,
                updates: vec![DestinationCatalogModelUpdate {
                    public_model: "multi-model".into(),
                    enabled: Some(false),
                    protocols: None,
                    preferred: None,
                }],
                remove_models: Vec::new(),
            })
            .unwrap(),
        ),
    )
    .await;
    assert!(stale_result.is_err());
    assert_eq!(stored_catalog(&fixture), before);
    task.abort();
}

#[tokio::test]
async fn model_test_exact_persisted_credential_does_not_direct_post_or_mark_success() {
    let (endpoint, mut requests, task) = start_models_upstream(vec![
        r#"{"type":"message","role":"assistant","content":[]}"#.into(),
    ])
    .await;
    let fixture = fixture(
        "model-test",
        &endpoint,
        DynamicAuthKind::Bearer,
        true,
        false,
    );
    install_chat_and_messages_routes(&fixture);
    add_multi_route_model(&fixture);
    grant_message_endpoint(&fixture);
    let before = stored_catalog(&fixture);
    let stamp = verification_stamp(&fixture);

    let error = test_once(&fixture, fixture.expectation())
        .await
        .expect_err("a refused pin is not protocol success");
    let text = error_text(&error);
    assert_forwarded_pin_refusal(&text);
    assert!(!text.contains(SECRET), "{text}");
    assert_silent(&mut requests).await;
    assert_eq!(
        stored_catalog(&fixture),
        before,
        "probe must not alter switches"
    );
    assert_eq!(verification_stamp(&fixture), stamp);
    let model = stored_catalog(&fixture)
        .into_iter()
        .find(|row| row.public_model == "multi-model")
        .unwrap();
    assert!(model.enabled);
    assert_eq!(model.preferred, Some(Protocol::ChatCompletions));
    task.abort();
}

#[tokio::test]
async fn model_test_keyless_pending_and_refused_rows_send_nothing() {
    let (endpoint, mut requests, task) =
        start_models_upstream(vec![r#"{"id":"ok","choices":[]}"#.into()]).await;

    let missing = fixture(
        "keyless-missing",
        &endpoint,
        DynamicAuthKind::None,
        false,
        false,
    );
    use_local_chat_model(&missing);
    let missing_error = test_public(
        &missing,
        missing.expectation(),
        "local-model",
        ProtocolDto::ChatCompletions,
    )
    .await
    .expect_err("no persisted keyless row");
    assert_local_model_refusal(&error_text(&missing_error));

    let keyed_none = fixture(
        "keyless-keyed",
        &endpoint,
        DynamicAuthKind::None,
        false,
        false,
    );
    use_local_chat_model(&keyed_none);
    let keyed_now = Utc::now();
    keyed_none
        .state
        .as_ref()
        .unwrap()
        .db
        .lock()
        .create_account(&Account {
            id: keyed_none.account_id.clone(),
            provider_id: keyed_none.provider_id.clone(),
            credential_kind: CredentialKind::None,
            quota_scope: QuotaScope::Key,
            name: keyed_none.account_id.clone(),
            username: None,
            password_cipher: None,
            key_cipher: keyed_none.cipher.encrypt(SECRET).unwrap(),
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
            created_at: keyed_now,
            updated_at: keyed_now,
        })
        .unwrap();
    grant_chat_model(&keyed_none, "local-model");
    let keyed_error = test_public(
        &keyed_none,
        keyed_none.expectation(),
        "local-model",
        ProtocolDto::ChatCompletions,
    )
    .await
    .expect_err("a keyed row is not a keyless binding");
    let keyed_text = error_text(&keyed_error);
    assert_local_model_refusal(&keyed_text);
    assert!(!keyed_text.contains(SECRET), "{keyed_text}");

    let keyless = fixture(
        "keyless-real",
        &endpoint,
        DynamicAuthKind::None,
        false,
        false,
    );
    use_local_chat_model(&keyless);
    let now = Utc::now();
    keyless
        .state
        .as_ref()
        .unwrap()
        .db
        .lock()
        .create_account(&Account {
            id: keyless.account_id.clone(),
            provider_id: keyless.provider_id.clone(),
            credential_kind: CredentialKind::None,
            quota_scope: QuotaScope::Key,
            name: keyless.account_id.clone(),
            username: None,
            password_cipher: None,
            key_cipher: String::new(),
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
    grant_chat_model(&keyless, "local-model");
    let keyless_before = stored_catalog(&keyless);
    let keyless_error = test_public(
        &keyless,
        keyless.expectation(),
        "local-model",
        ProtocolDto::ChatCompletions,
    )
    .await
    .expect_err("unavailable host refuses the forwarded keyless row");
    // Refusal evidence only. Empty-material admission belongs to the route
    // projection pin, which this test does not claim to satisfy.
    assert_forwarded_pin_refusal(&error_text(&keyless_error));
    assert_eq!(stored_catalog(&keyless), keyless_before);
    let (status, verified_at, step) = verification_stamp(&keyless);
    assert_ne!(status, "verified");
    assert!(verified_at.is_none());
    assert_eq!(step, "ready");

    let pending = fixture(
        "managed-pending",
        &endpoint,
        DynamicAuthKind::Bearer,
        false,
        false,
    );
    install_chat_and_messages_routes(&pending);
    add_multi_route_model(&pending);
    insert_keyed_account(
        &pending,
        AccountType::Managed,
        AccountSetupStep::KeyVerification,
        false,
        SECRET,
    );
    grant_message_endpoint(&pending);
    let pending_catalog = stored_catalog(&pending);
    let pending_error = test_once(&pending, pending.expectation())
        .await
        .expect_err("managed pending still stops at the unavailable pin");
    let pending_text = error_text(&pending_error);
    assert_forwarded_pin_refusal(&pending_text);
    assert!(!pending_text.contains(SECRET), "{pending_text}");
    assert_eq!(stored_catalog(&pending), pending_catalog);
    let (pending_enabled, pending_type, pending_step) = credential_admission_row(&pending);
    assert_eq!(pending_enabled, 0);
    assert_eq!(pending_type, "managed");
    assert_eq!(pending_step, "key_verification");
    let (status, verified_at, step) = verification_stamp(&pending);
    assert_ne!(status, "verified");
    assert!(verified_at.is_none());
    assert_eq!(step, "key_verification");

    let substitute = fixture(
        "ordinary-key-verification",
        &endpoint,
        DynamicAuthKind::Bearer,
        true,
        false,
    );
    install_chat_and_messages_routes(&substitute);
    add_multi_route_model(&substitute);
    grant_message_endpoint(&substitute);
    set_credential_column(
        &substitute,
        "UPDATE credentials SET setup_step = ?2 WHERE legacy_account_id = ?1",
        "key_verification",
    );
    let substitute_text = error_text(
        &test_once(&substitute, substitute.expectation())
            .await
            .expect_err("an enabled ordinary key_verification row is not the managed candidate"),
    );
    assert_local_model_refusal(&substitute_text);
    assert_eq!(verification_stamp(&substitute).2, "key_verification");

    let incomplete = fixture(
        "incomplete-payment",
        &endpoint,
        DynamicAuthKind::Bearer,
        true,
        false,
    );
    install_chat_and_messages_routes(&incomplete);
    add_multi_route_model(&incomplete);
    grant_message_endpoint(&incomplete);
    set_credential_column(
        &incomplete,
        "UPDATE credentials SET setup_step = ?2 WHERE legacy_account_id = ?1",
        "payment",
    );
    let incomplete_error = test_once(&incomplete, incomplete.expectation())
        .await
        .expect_err("incomplete setup refuses before the pin");
    assert_local_model_refusal(&error_text(&incomplete_error));
    assert_eq!(verification_stamp(&incomplete).2, "payment");

    let disabled = fixture(
        "disabled-ready",
        &endpoint,
        DynamicAuthKind::Bearer,
        true,
        false,
    );
    install_chat_and_messages_routes(&disabled);
    add_multi_route_model(&disabled);
    grant_message_endpoint(&disabled);
    disabled
        .state
        .as_ref()
        .unwrap()
        .db
        .lock()
        .conn
        .execute(
            "UPDATE credentials SET enabled = 0 WHERE legacy_account_id = ?1",
            [&disabled.account_id],
        )
        .unwrap();
    assert_local_model_refusal(&error_text(
        &test_once(&disabled, disabled.expectation())
            .await
            .expect_err("disabled Ready refuses before the pin"),
    ));

    assert_silent(&mut requests).await;
    task.abort();
}

#[tokio::test]
async fn model_test_binding_and_ordinary_disabled_ready_refuse_before_send() {
    let (endpoint, mut requests, task) =
        start_models_upstream(vec![r#"{"id":"ok","choices":[]}"#.into()]).await;

    let disabled_binding = fixture(
        "binding-disabled",
        &endpoint,
        DynamicAuthKind::Bearer,
        true,
        false,
    );
    install_chat_and_messages_routes(&disabled_binding);
    add_multi_route_model(&disabled_binding);
    grant_message_endpoint(&disabled_binding);
    let binding_catalog = stored_catalog(&disabled_binding);
    disabled_binding
        .state
        .as_ref()
        .unwrap()
        .db
        .lock()
        .conn
        .execute(
            "UPDATE credentials SET binding_enabled = 0 WHERE legacy_account_id = ?1",
            [&disabled_binding.account_id],
        )
        .unwrap();
    assert_local_model_refusal(&error_text(
        &test_once(&disabled_binding, disabled_binding.expectation())
            .await
            .expect_err("a disabled binding refuses before the pin"),
    ));
    assert_eq!(stored_catalog(&disabled_binding), binding_catalog);
    assert_eq!(verification_stamp(&disabled_binding).2, "ready");

    let missing_binding = fixture(
        "binding-missing",
        &endpoint,
        DynamicAuthKind::Bearer,
        true,
        false,
    );
    install_chat_and_messages_routes(&missing_binding);
    add_multi_route_model(&missing_binding);
    grant_message_endpoint(&missing_binding);
    missing_binding
        .state
        .as_ref()
        .unwrap()
        .db
        .lock()
        .conn
        .execute(
            "UPDATE credentials SET binding_id = '' WHERE legacy_account_id = ?1",
            [&missing_binding.account_id],
        )
        .unwrap();
    assert_local_model_refusal(&error_text(
        &test_once(&missing_binding, missing_binding.expectation())
            .await
            .expect_err("an empty binding refuses before the pin"),
    ));

    let enabled_managed = fixture(
        "managed-enabled-pending",
        &endpoint,
        DynamicAuthKind::Bearer,
        false,
        false,
    );
    install_chat_and_messages_routes(&enabled_managed);
    add_multi_route_model(&enabled_managed);
    insert_keyed_account(
        &enabled_managed,
        AccountType::Managed,
        AccountSetupStep::KeyVerification,
        true,
        SECRET,
    );
    grant_message_endpoint(&enabled_managed);
    let enabled_managed_text = error_text(
        &test_once(&enabled_managed, enabled_managed.expectation())
            .await
            .expect_err("an enabled managed key_verification row is outside the narrow candidate"),
    );
    assert_local_model_refusal(&enabled_managed_text);
    assert!(
        !enabled_managed_text.contains(SECRET),
        "{enabled_managed_text}"
    );
    let (enabled, account_type, step) = credential_admission_row(&enabled_managed);
    assert_eq!(enabled, 1);
    assert_eq!(account_type, "managed");
    assert_eq!(step, "key_verification");

    let disabled_ready = fixture(
        "ordinary-disabled-ready",
        &endpoint,
        DynamicAuthKind::Bearer,
        true,
        false,
    );
    install_chat_and_messages_routes(&disabled_ready);
    add_multi_route_model(&disabled_ready);
    grant_message_endpoint(&disabled_ready);
    disabled_ready
        .state
        .as_ref()
        .unwrap()
        .db
        .lock()
        .conn
        .execute(
            "UPDATE credentials SET enabled = 0 WHERE legacy_account_id = ?1",
            [&disabled_ready.account_id],
        )
        .unwrap();
    assert_local_model_refusal(&error_text(
        &test_once(&disabled_ready, disabled_ready.expectation())
            .await
            .expect_err("ordinary disabled Ready refuses before the pin"),
    ));
    let (enabled, account_type, step) = credential_admission_row(&disabled_ready);
    assert_eq!(enabled, 0);
    assert_eq!(account_type, "key");
    assert_eq!(step, "ready");

    assert_silent(&mut requests).await;
    task.abort();
}

#[tokio::test]
async fn model_test_scope_protocol_and_stale_cas_send_nothing() {
    let (endpoint, mut requests, task) =
        start_models_upstream(vec![r#"{"id":"ok","choices":[]}"#.into()]).await;
    let fixture = fixture(
        "model-test-fence",
        &endpoint,
        DynamicAuthKind::Bearer,
        true,
        false,
    );
    install_chat_and_messages_routes(&fixture);
    add_multi_route_model(&fixture);
    grant_message_endpoint(&fixture);
    let before = stored_catalog(&fixture);

    let wrong_protocol = test_public(
        &fixture,
        fixture.expectation(),
        "multi-model",
        ProtocolDto::Responses,
    )
    .await
    .expect_err("unconfigured protocol");
    assert!(
        error_text(&wrong_protocol).contains("invalidRequest"),
        "{}",
        error_text(&wrong_protocol)
    );

    let scope = serde_json::to_string(&ocg_domain::credential::ModelScope::Only {
        models: vec!["other-model".into()],
    })
    .unwrap();
    set_credential_column(
        &fixture,
        "UPDATE credentials SET scope_json = ?2 WHERE legacy_account_id = ?1",
        &scope,
    );
    let scoped = test_once(&fixture, fixture.expectation())
        .await
        .expect_err("model scope miss");
    assert_local_model_refusal(&error_text(&scoped));
    set_credential_column(
        &fixture,
        "UPDATE credentials SET scope_json = ?2 WHERE legacy_account_id = ?1",
        &serde_json::to_string(&ocg_domain::credential::ModelScope::All).unwrap(),
    );

    let stale = fixture.expectation();
    fixture.state.as_ref().unwrap().bump_settings_revision();
    let stale_error = test_once(&fixture, stale).await.expect_err("stale CAS");
    assert!(
        error_text(&stale_error).contains("revisionConflict"),
        "{}",
        error_text(&stale_error)
    );
    assert_eq!(stored_catalog(&fixture), before);
    assert_silent(&mut requests).await;
    task.abort();
}

#[tokio::test]
async fn model_test_post_send_fence_rejects_a_rotated_credential_without_a_provider_send() {
    let (endpoint, mut requests, task) = start_models_upstream(vec![
        r#"{"type":"message","role":"assistant","content":[]}"#.into(),
    ])
    .await;
    let fixture = fixture(
        "model-test-rotate",
        &endpoint,
        DynamicAuthKind::Bearer,
        true,
        false,
    );
    install_chat_and_messages_routes(&fixture);
    add_multi_route_model(&fixture);
    grant_message_endpoint(&fixture);
    let before = stored_catalog(&fixture);
    let state = fixture.state.as_ref().unwrap().clone();
    let expectation = fixture.expectation();
    let selected = {
        let _settings = state.settings_update.lock();
        prepare_destination_model_test(
            &state,
            &fixture.destination_id(),
            &expectation,
            "multi-model",
            Protocol::Messages,
        )
        .unwrap()
    };
    state
        .db
        .lock()
        .rotate_account_credential(
            &fixture.account_id,
            &fixture.cipher.encrypt("rotated-secret").unwrap(),
        )
        .unwrap();
    let fenced = {
        let _settings = state.settings_update.lock();
        recheck_model_test_fence(&state, &fixture.destination_id(), &expectation, &selected)
    };
    assert!(
        fenced.is_err(),
        "rotated material must not pass the old fence"
    );
    assert_eq!(stored_catalog(&fixture), before);
    assert_silent(&mut requests).await;
    task.abort();
}

#[tokio::test]
async fn model_test_refuses_revoked_selected_endpoint_or_origin_without_outbound_io() {
    for revoke in ["endpoint_id", "origin"] {
        let (endpoint, mut requests, task) = start_models_upstream(vec![
            r#"{"type":"message","role":"assistant","content":[]}"#.into(),
        ])
        .await;
        let fixture = fixture(
            &format!("model-test-revoked-{revoke}"),
            &endpoint,
            DynamicAuthKind::Bearer,
            true,
            false,
        );
        install_chat_and_messages_routes(&fixture);
        add_multi_route_model(&fixture);
        grant_message_endpoint(&fixture);
        let endpoint_id = message_endpoint_id(&fixture);
        {
            let db = fixture.state.as_ref().unwrap().db.lock();
            let credential_id: String = db
                .conn
                .query_row(
                    "SELECT id FROM credentials WHERE legacy_account_id = ?1",
                    [&fixture.account_id],
                    |row| row.get(0),
                )
                .unwrap();
            if revoke == "endpoint_id" {
                let origins: i64 = db.conn.query_row(
                    "SELECT COUNT(*) FROM credential_grants WHERE credential_id = ?1 AND kind = 'origin'",
                    [&credential_id],
                    |row| row.get(0),
                )
                .unwrap();
                assert!(origins > 0);
                db.conn.execute(
                    "DELETE FROM credential_grants WHERE credential_id = ?1 AND kind = 'endpoint_id' AND value = ?2",
                    rusqlite::params![credential_id, endpoint_id],
                )
                .unwrap();
            } else {
                let endpoints: i64 = db.conn.query_row(
                    "SELECT COUNT(*) FROM credential_grants WHERE credential_id = ?1 AND kind = 'endpoint_id' AND value = ?2",
                    rusqlite::params![credential_id, endpoint_id],
                    |row| row.get(0),
                )
                .unwrap();
                assert_eq!(endpoints, 1);
                db.conn.execute(
                    "DELETE FROM credential_grants WHERE credential_id = ?1 AND kind = 'origin'",
                    [&credential_id],
                )
                .unwrap();
            }
        }

        let revoked = test_once(&fixture, fixture.expectation())
            .await
            .expect_err("revoked grant");
        assert_local_model_refusal(&error_text(&revoked));
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(100), requests.recv())
                .await
                .is_err(),
            "revoked {revoke} grant must reject before outbound I/O"
        );
        task.abort();
    }
}

#[tokio::test]
async fn refresh_rechecks_cas_and_credential_identity_after_awaiting_upstream() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!(
        "http://{}/v1/chat/completions",
        listener.local_addr().unwrap()
    );
    let (arrived_tx, arrived_rx) = oneshot::channel();
    let (release_tx, release_rx) = oneshot::channel();
    let (unexpected_tx, mut unexpected_rx) = mpsc::unbounded_channel();
    let task = tokio::spawn(async move {
        let Ok((mut stream, _)) = listener.accept().await else {
            return;
        };
        let mut request = [0_u8; 4096];
        let _ = stream.read(&mut request).await;
        let _ = arrived_tx.send(());
        let _ = release_rx.await;
        let body = r#"{"data":[{"id":"late-model"}]}"#;
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = stream.write_all(response.as_bytes()).await;
        let Ok((mut unexpected, _)) = listener.accept().await else {
            return;
        };
        let mut request = [0_u8; 4096];
        let _ = unexpected.read(&mut request).await;
        let _ = unexpected_tx.send(());
    });
    let fixture = fixture("race", &endpoint, DynamicAuthKind::Bearer, true, false);
    let before = stored_catalog(&fixture);
    let state = fixture.state.as_ref().unwrap().clone();
    let id = fixture.destination_id();
    let expectation = fixture.expectation();
    let refresh_task = tokio::spawn(async move {
        refresh(
            State(state),
            Path(id),
            Bytes::from(serde_json::to_vec(&expectation).unwrap()),
        )
        .await
    });
    arrived_rx.await.unwrap();
    fixture
        .state
        .as_ref()
        .unwrap()
        .db
        .lock()
        .rotate_account_credential(
            &fixture.account_id,
            &fixture.cipher.encrypt("rotated-secret").unwrap(),
        )
        .unwrap();
    let _ = release_tx.send(());
    assert!(refresh_task.await.unwrap().is_err());
    assert_eq!(stored_catalog(&fixture), before);

    let stale = MutationExpectation {
        expected_revision: fixture.state.as_ref().unwrap().settings_revision() - 1,
        process_generation: fixture.state.as_ref().unwrap().process_generation(),
    };
    let result = refresh(
        State(fixture.state.as_ref().unwrap().clone()),
        Path(fixture.destination_id()),
        Bytes::from(serde_json::to_vec(&stale).unwrap()),
    )
    .await;
    assert!(
        result.is_err(),
        "stale CAS must be rejected before refreshing"
    );
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(100), unexpected_rx.recv())
            .await
            .is_err(),
        "stale CAS must reject before it sends an upstream request"
    );
    task.abort();
}

#[test]
fn refresh_receipt_stays_with_its_commit_when_a_later_mutation_advances_revision() {
    let fixture = fixture(
        "receipt",
        "https://catalog.invalid/v1/chat/completions",
        DynamicAuthKind::Bearer,
        true,
        false,
    );
    let state = fixture.state.as_ref().unwrap().clone();
    let id = fixture.destination_id();
    let cas = crate::account_control::MutationCas {
        expected_revision: state.settings_revision(),
        process_generation: state.process_generation(),
    };
    let stale_prepared = crate::account_control::prepare_catalog_refresh(&state, &id, cas).unwrap();
    let prepared = crate::account_control::prepare_catalog_refresh(&state, &id, cas).unwrap();
    let metadata = std::collections::BTreeMap::new();
    let models = vec!["fresh-model".to_string()];
    arm_catalog_refresh_snapshot_interpose();
    let receipt =
        finish_catalog_refresh(&state, &id, cas, prepared, &models, &metadata, false).unwrap();
    assert_eq!(receipt.revision.revision + 1, state.settings_revision());
    assert_eq!(
        receipt.revision.process_generation,
        state.process_generation()
    );
    assert!(
        receipt
            .destination
            .catalog
            .iter()
            .any(|model| model.public_model == "fresh-model")
    );
    assert_eq!(
        stored_catalog(&fixture)
            .iter()
            .map(|model| model.public_model.clone())
            .collect::<Vec<_>>(),
        receipt
            .destination
            .catalog
            .iter()
            .map(|model| model.public_model.clone())
            .collect::<Vec<_>>()
    );
    let stale = crate::account_control::commit_catalog_refresh(
        &state,
        &id,
        cas,
        stale_prepared,
        &models,
        &metadata,
    )
    .unwrap_err();
    assert!(
        matches!(
            stale,
            crate::account_control::CatalogRefreshError::Conflict(message)
                if message == "revision conflict"
        ),
        "stale CAS must reject the second commit"
    );
    assert_eq!(
        stored_catalog(&fixture)
            .iter()
            .map(|model| model.public_model.clone())
            .collect::<Vec<_>>(),
        receipt
            .destination
            .catalog
            .iter()
            .map(|model| model.public_model.clone())
            .collect::<Vec<_>>()
    );
}

/// Skip-spawn owned plane. The empty task fills `GatewayHandle`; it is not a child process.
struct OwnedApplyGuard {
    _shutdown_rx: tokio::sync::oneshot::Receiver<()>,
}

impl OwnedApplyGuard {
    fn arm(state: &CoreState) -> Self {
        crate::cpa_execution::set_skip_spawn(true);
        crate::cpa_execution::set_fail_ready(false);
        crate::cpa_execution::set_test_password(Some("synthetic-management".to_string()));
        crate::cpa_execution::set_before_apply_commit(None);
        crate::cpa_execution::set_artifact_dir(
            state,
            crate::cpa_execution::documented_runtime_dir(),
        );
        let (shutdown, shutdown_rx) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async {});
        *state.gateway.lock() = Some(crate::gateway_runtime::GatewayHandle {
            port: 9,
            listen_addr: "127.0.0.1:9".parse().unwrap(),
            dashboard_is_local: true,
            shutdown,
            task,
        });
        Self {
            _shutdown_rx: shutdown_rx,
        }
    }
}

impl Drop for OwnedApplyGuard {
    fn drop(&mut self) {
        crate::cpa_execution::set_skip_spawn(false);
        crate::cpa_execution::set_fail_ready(false);
        crate::cpa_execution::set_test_password(None);
        crate::cpa_execution::set_before_apply_commit(None);
    }
}

fn projection_catalog_fixture(label: &str) -> Fixture {
    fixture(
        label,
        "https://projection.example/v1/chat/completions",
        DynamicAuthKind::Bearer,
        true,
        false,
    )
}

fn alias_keep_enabled(fixture: &Fixture) -> bool {
    stored_catalog(fixture)
        .into_iter()
        .find(|model| model.public_model == "alias-keep")
        .unwrap()
        .enabled
}

async fn started_plane(state: &CoreState) -> crate::cpa_execution::ExecutionReport {
    let started =
        crate::cpa_execution::start(state, state.settings_revision(), state.process_generation())
            .await
            .expect("owned plane start");
    assert_eq!(started.apply_status, "applied");
    assert!(started.desired_running);
    started
}

#[tokio::test(flavor = "current_thread")]
async fn catalog_update_keeps_the_saved_receipt_and_applies_enabled() {
    let fixture = projection_catalog_fixture("projection-catalog");
    let state = fixture.state.as_ref().unwrap().clone();
    let _plane = OwnedApplyGuard::arm(&state);
    let started = started_plane(&state).await;
    grant_alias_keep_override(&fixture);
    let saved = update_once(
        &fixture,
        vec![DestinationCatalogModelUpdate {
            public_model: "alias-keep".into(),
            enabled: Some(true),
            protocols: None,
            preferred: None,
        }],
        Vec::new(),
    )
    .await
    .expect("catalog update");
    assert_eq!(saved.revision.revision, state.settings_revision());
    assert_eq!(
        saved.revision.process_generation,
        state.process_generation()
    );
    assert!(
        saved
            .destination
            .catalog
            .iter()
            .any(|model| { model.public_model == "alias-keep" && model.enabled })
    );
    assert!(alias_keep_enabled(&fixture));
    let applied = crate::cpa_execution::execution_report(&state);
    assert_eq!(applied.desired_revision, started.desired_revision + 1);
    assert_eq!(applied.applied_revision, applied.desired_revision);
    assert_eq!(applied.apply_status, "applied");
    assert!(state.settings_update.try_lock().is_some());
}

#[tokio::test(flavor = "current_thread")]
async fn catalog_update_stale_cas_does_not_apply() {
    let fixture = projection_catalog_fixture("projection-catalog-stale");
    let state = fixture.state.as_ref().unwrap().clone();
    let _plane = OwnedApplyGuard::arm(&state);
    let started = started_plane(&state).await;
    let revision = state.settings_revision();
    let error = update(
        State(state.clone()),
        Path(fixture.destination_id()),
        Bytes::from(
            serde_json::to_vec(&DestinationCatalogUpdate {
                expectation: MutationExpectation {
                    expected_revision: revision + 1,
                    process_generation: state.process_generation(),
                },
                updates: vec![DestinationCatalogModelUpdate {
                    public_model: "alias-keep".into(),
                    enabled: Some(true),
                    protocols: None,
                    preferred: None,
                }],
                remove_models: Vec::new(),
            })
            .unwrap(),
        ),
    )
    .await
    .expect_err("stale catalog update");
    assert!(format!("{error:?}").contains("revisionConflict"));
    assert!(!alias_keep_enabled(&fixture));
    assert_eq!(state.settings_revision(), revision);
    let after = crate::cpa_execution::execution_report(&state);
    assert_eq!(after.desired_revision, started.desired_revision);
    assert_eq!(after.applied_revision, started.applied_revision);
    assert_eq!(after.apply_status, started.apply_status);
    assert!(state.settings_update.try_lock().is_some());
}

#[tokio::test(flavor = "current_thread")]
async fn catalog_update_empty_input_does_not_apply() {
    let fixture = projection_catalog_fixture("projection-catalog-empty");
    let state = fixture.state.as_ref().unwrap().clone();
    let _plane = OwnedApplyGuard::arm(&state);
    let started = started_plane(&state).await;
    let revision = state.settings_revision();
    let error = update_once(&fixture, Vec::new(), Vec::new())
        .await
        .expect_err("empty catalog update");
    assert!(format!("{error:?}").contains("catalog update is empty"));
    assert!(!alias_keep_enabled(&fixture));
    assert_eq!(state.settings_revision(), revision);
    let after = crate::cpa_execution::execution_report(&state);
    assert_eq!(after.desired_revision, started.desired_revision);
    assert_eq!(after.applied_revision, started.applied_revision);
    assert_eq!(after.apply_status, started.apply_status);
    assert!(state.settings_update.try_lock().is_some());
}

fn plant_malformed_quota_policy(state: &CoreState) {
    state
        .db
        .lock()
        .conn
        .execute(
            "INSERT OR REPLACE INTO settings (key, value) VALUES (?1, ?2)",
            rusqlite::params![crate::cpa_policy::SETTINGS_KEY, "{"],
        )
        .unwrap();
}

struct ReceiptFailGuard;

impl ReceiptFailGuard {
    fn arm() -> Self {
        super::super::destinations::fail_next_mutation_receipt();
        Self
    }
}

impl Drop for ReceiptFailGuard {
    fn drop(&mut self) {
        super::super::destinations::clear_mutation_receipt_failure();
    }
}

#[tokio::test(flavor = "current_thread")]
async fn catalog_update_malformed_quota_policy_refuses_before_commit() {
    let fixture = projection_catalog_fixture("projection-quota-refuse");
    let state = fixture.state.as_ref().unwrap().clone();
    let _plane = OwnedApplyGuard::arm(&state);
    let started = started_plane(&state).await;
    let revision = state.settings_revision();
    plant_malformed_quota_policy(&state);
    let error = update_once(
        &fixture,
        vec![DestinationCatalogModelUpdate {
            public_model: "alias-keep".into(),
            enabled: Some(true),
            protocols: None,
            preferred: None,
        }],
        Vec::new(),
    )
    .await
    .expect_err("malformed quota policy");
    assert!(format!("{error:?}").contains("official quota policy could not be read"));
    assert!(!alias_keep_enabled(&fixture));
    assert_eq!(state.settings_revision(), revision);
    let after = crate::cpa_execution::execution_report(&state);
    assert_eq!(after.desired_revision, started.desired_revision);
    assert_eq!(after.applied_revision, started.applied_revision);
    assert_eq!(after.apply_status, started.apply_status);
    assert!(state.settings_update.try_lock().is_some());
}

#[tokio::test(flavor = "current_thread")]
async fn catalog_update_receipt_failure_after_commit_still_applies() {
    let fixture = projection_catalog_fixture("projection-receipt-fail");
    let state = fixture.state.as_ref().unwrap().clone();
    let _plane = OwnedApplyGuard::arm(&state);
    let started = started_plane(&state).await;
    grant_alias_keep_override(&fixture);
    let revision = state.settings_revision();
    let _receipt_fail = ReceiptFailGuard::arm();
    let error = update_once(
        &fixture,
        vec![DestinationCatalogModelUpdate {
            public_model: "alias-keep".into(),
            enabled: Some(true),
            protocols: None,
            preferred: None,
        }],
        Vec::new(),
    )
    .await
    .expect_err("receipt read");
    assert!(format!("{error:?}").contains("official quota policy could not be read"));
    assert!(alias_keep_enabled(&fixture));
    assert!(state.settings_revision() > revision);
    let applied = crate::cpa_execution::execution_report(&state);
    assert_eq!(applied.desired_revision, started.desired_revision + 1);
    assert_eq!(applied.applied_revision, applied.desired_revision);
    assert_eq!(applied.apply_status, "applied");
    assert!(state.settings_update.try_lock().is_some());
}

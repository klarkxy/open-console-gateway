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
    test_model(
        State(fixture.state.as_ref().unwrap().clone()),
        Path(fixture.destination_id()),
        Bytes::from(
            serde_json::to_vec(&DestinationModelTestRequest {
                expectation,
                public_model: "multi-model".into(),
                protocol: ProtocolDto::Messages,
            })
            .unwrap(),
        ),
    )
    .await
    .map(|value| value.0)
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
async fn model_test_uses_selected_messages_route_and_preserves_catalog_switches() {
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

    let result = test_once(&fixture, fixture.expectation()).await.unwrap();
    assert!(
        result.ok,
        "model test should accept a protocol-valid response"
    );
    assert_eq!(result.protocol, ProtocolDto::Messages);
    let request = requests.recv().await.unwrap();
    assert!(
        request.starts_with("POST /anthropic/v1/messages HTTP/1.1\r\n"),
        "{request}"
    );
    assert!(
        request
            .to_ascii_lowercase()
            .contains(&format!("x-api-key: {SECRET}").to_ascii_lowercase()),
        "{request}"
    );
    assert!(!request.to_ascii_lowercase().contains("authorization:"));
    assert_eq!(
        stored_catalog(&fixture),
        before,
        "probe must not alter switches"
    );
    task.await.unwrap();
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
            let origins: i64 = db
                .conn
                .query_row(
                    "SELECT COUNT(*) FROM credential_grants WHERE credential_id = ?1 AND kind = 'origin'",
                    [&credential_id],
                    |row| row.get(0),
                )
                .unwrap();
            assert!(origins > 0);
            db.conn
                .execute(
                    "DELETE FROM credential_grants WHERE credential_id = ?1 AND kind = 'endpoint_id' AND value = ?2",
                    rusqlite::params![credential_id, endpoint_id],
                )
                .unwrap();
        } else {
            let endpoints: i64 = db
                .conn
                .query_row(
                    "SELECT COUNT(*) FROM credential_grants WHERE credential_id = ?1 AND kind = 'endpoint_id' AND value = ?2",
                    rusqlite::params![credential_id, endpoint_id],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(endpoints, 1);
            db.conn
                .execute(
                    "DELETE FROM credential_grants WHERE credential_id = ?1 AND kind = 'origin'",
                    [&credential_id],
                )
                .unwrap();
        }
        drop(db);

        assert!(test_once(&fixture, fixture.expectation()).await.is_err());
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

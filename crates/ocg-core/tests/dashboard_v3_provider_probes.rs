//! Dashboard V3 provider protocol probes: auth, CAS, zero-call gates, shared
//! transport, persistence, and retired V2 paths.

use axum::Router;
use axum::body::Bytes;
use axum::extract::OriginalUri;
use axum::http::{HeaderMap, Method as HttpMethod};
use axum::routing::any;
use ocg_core::dashboard_v3::{
    AccountUpstreamProtocol, ERROR_INTERNAL, ERROR_INVALID_JSON, ERROR_INVALID_REQUEST,
    ERROR_MISSING_EXPECTED_REVISION, ERROR_NOT_FOUND, ERROR_REVISION_CONFLICT, ERROR_UNAUTHORIZED,
    OfficialProtocolBaseline, ProtocolProbeResponse, install_official_protocol_fetch_for_tests,
    install_official_protocol_fetch_unavailable_for_tests,
};
use ocg_core::gateway::provider_adapter::install_goat_loopback_route_for_test;
use ocg_core::models::{ProxyListDirection, ProxyMode};
use ocg_core::provider::{
    COMMAND_CODE_PROVIDER_ID, CUSTOM_PROVIDER_ID, KIMI_PROVIDER_ID, MINIMAX_PROVIDER_ID,
    OPENCODE_PROVIDER_ID, OPENCODE_ZEN_FREE_PROVIDER_ID, UpstreamProtocolKind,
};
use ocg_core::provider_contracts::{
    CATALOG_SOURCE_OPENCODE_MODELS, ContractEvidenceSource, ContractScope, PersistedModelProtocol,
    ProbeResultKind, ProtocolOverrideState,
};
use reqwest::{Method, StatusCode};
use serde_json::{Map, Value, json};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[path = "fixtures/dashboard_v3/harness.rs"]
mod harness;

use harness::{V3Harness, start_loopback, start_public};

fn seed_probe_catalogs(harness: &V3Harness) {
    let now = chrono::Utc::now();
    {
        let db = harness.state.db.lock();
        for (provider_id, models, source) in [
            (
                OPENCODE_PROVIDER_ID,
                &["grok-4.5", "glm-5.2", "glm-5.3", "future-go-model"][..],
                CATALOG_SOURCE_OPENCODE_MODELS,
            ),
            (
                OPENCODE_ZEN_FREE_PROVIDER_ID,
                &["mimo-v2.5-free"][..],
                "official_zen",
            ),
            (
                MINIMAX_PROVIDER_ID,
                &["MiniMax-M3"][..],
                "minimax_cn_get_models",
            ),
            (KIMI_PROVIDER_ID, &["kimi-k3"][..], "kimi_cn_get_models"),
            (
                COMMAND_CODE_PROVIDER_ID,
                &[
                    "claude-fable-5",
                    "deepseek/deepseek-v4-flash",
                    "claude-sonnet-5",
                ][..],
                "command_code_get_models",
            ),
        ] {
            db.set_contract_catalog(
                &ContractScope::provider(provider_id),
                &models
                    .iter()
                    .map(|model| (*model).to_string())
                    .collect::<Vec<_>>(),
                Some(now),
                source,
                "https://example.test/models",
                now,
            )
            .unwrap();
        }
        db.apply_official_protocol_baseline(
            &ContractScope::provider(OPENCODE_PROVIDER_ID),
            &[
                "grok-4.5".to_string(),
                "glm-5.2".to_string(),
                "glm-5.3".to_string(),
                "future-go-model".to_string(),
            ],
            &OfficialProtocolBaseline::mapped([
                ("grok-4.5", UpstreamProtocolKind::Responses),
                ("glm-5.2", UpstreamProtocolKind::ChatCompletions),
                ("glm-5.3", UpstreamProtocolKind::ChatCompletions),
            ]),
            now,
        )
        .unwrap();
    }
    harness.state.reload_provider_contracts().unwrap();
}

async fn start_probes(name: &str) -> V3Harness {
    let harness = start_loopback(name).await;
    seed_probe_catalogs(&harness);
    harness
}

const GO_KEY: &str = "sk-probe-secret-key";
const CUSTOM_KEY: &str = "custom-x-api-key";
#[path = "fixtures/probe_response.rs"]
mod probe_response;
const SUCCESS_BODY: &str = probe_response::CHAT;

#[derive(Clone, Debug)]
struct CapturedProbe {
    method: String,
    path: String,
    authorization: Option<String>,
    x_api_key: Option<String>,
    cookie: Option<String>,
    opencode_session: Option<String>,
    body: String,
}

struct ProbeOrigin {
    url: String,
    calls: Arc<Mutex<Vec<CapturedProbe>>>,
    _stop: tokio::sync::oneshot::Sender<()>,
}

impl ProbeOrigin {
    fn call_count(&self) -> usize {
        self.calls.lock().unwrap().len()
    }
}

fn header_value(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
}

async fn start_probe_origin(status: StatusCode, body: &str, delay: Duration) -> ProbeOrigin {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let calls_for_handler = calls.clone();
    let body = body.to_string();
    let app = Router::new().fallback(any(
        move |method: HttpMethod, uri: OriginalUri, headers: HeaderMap, payload: Bytes| {
            let calls = calls_for_handler.clone();
            // Model the real Go rejection that a generic 200 fixture missed.
            let missing_session = status.is_success()
                && !uri.0.path().starts_with("/provider/")
                && (header_value(&headers, "authorization").as_deref() == Some(&format!("Bearer {GO_KEY}"))
                    || header_value(&headers, "x-api-key").as_deref() == Some(GO_KEY))
                && !headers.contains_key("x-opencode-session");
            let too_small = uri.0.path().ends_with("/responses")
                && serde_json::from_slice::<Value>(&payload).ok()
                    .and_then(|value| value["max_output_tokens"].as_u64())
                    .is_some_and(|tokens| tokens < 16);
            let status = if missing_session || too_small { StatusCode::BAD_REQUEST } else { status };
            let body = if missing_session || too_small {
                r#"{"error":{"message":"missing session identity or invalid Responses token budget"}}"#.to_string()
            } else if body == SUCCESS_BODY {
                probe_response::for_path(uri.0.path()).to_string()
            } else {
                body.clone()
            };
            async move {
                if !delay.is_zero() {
                    tokio::time::sleep(delay).await;
                }
                calls.lock().unwrap().push(CapturedProbe {
                    method: method.to_string(),
                    path: uri.0.path().to_string(),
                    authorization: header_value(&headers, "authorization"),
                    x_api_key: header_value(&headers, "x-api-key"),
                    cookie: header_value(&headers, "cookie"),
                    opencode_session: header_value(&headers, "x-opencode-session"),
                    body: String::from_utf8_lossy(&payload).into_owned(),
                });
                (
                    status,
                    [(axum::http::header::CONTENT_TYPE, "application/json")],
                    body,
                )
            }
        },
    ));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (stop, shutdown) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                let _ = shutdown.await;
            })
            .await
            .ok();
    });
    ProbeOrigin {
        url: format!("http://{addr}"),
        calls,
        _stop: stop,
    }
}

async fn start_fallback_probe_origin(failing_key: &str) -> ProbeOrigin {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let calls_for_handler = calls.clone();
    let failing_bearer = format!("Bearer {failing_key}");
    let app = Router::new().fallback(any(
        move |method: HttpMethod, uri: OriginalUri, headers: HeaderMap, payload: Bytes| {
            let calls = calls_for_handler.clone();
            let failing_bearer = failing_bearer.clone();
            async move {
                let authorization = header_value(&headers, "authorization");
                calls.lock().unwrap().push(CapturedProbe {
                    method: method.to_string(),
                    path: uri.0.path().to_string(),
                    authorization: authorization.clone(),
                    x_api_key: header_value(&headers, "x-api-key"),
                    cookie: header_value(&headers, "cookie"),
                    opencode_session: header_value(&headers, "x-opencode-session"),
                    body: String::from_utf8_lossy(&payload).into_owned(),
                });
                if authorization.as_deref() == Some(failing_bearer.as_str()) {
                    (
                        StatusCode::UNAUTHORIZED,
                        [(axum::http::header::CONTENT_TYPE, "application/json")],
                        r#"{"error":{"message":"account unavailable"}}"#,
                    )
                } else {
                    (
                        StatusCode::OK,
                        [(axum::http::header::CONTENT_TYPE, "application/json")],
                        probe_response::for_path(uri.0.path()),
                    )
                }
            }
        },
    ));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (stop, shutdown) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                let _ = shutdown.await;
            })
            .await
            .ok();
    });
    ProbeOrigin {
        url: format!("http://{addr}"),
        calls,
        _stop: stop,
    }
}

fn cas(harness: &V3Harness, patch: Value) -> Value {
    let mut body = match patch {
        Value::Object(map) => map,
        _ => Map::new(),
    };
    body.insert(
        "expectedRevision".into(),
        json!(harness.state.settings_revision()),
    );
    body.insert(
        "processGeneration".into(),
        json!(harness.state.process_generation()),
    );
    Value::Object(body)
}

async fn send_json(
    harness: &V3Harness,
    method: Method,
    path: &str,
    body: &Value,
) -> (StatusCode, Value) {
    let response = harness
        .client
        .request(method, format!("{}{path}", harness.v3_base))
        .json(body)
        .send()
        .await
        .unwrap();
    let status = response.status();
    let body = response.json().await.unwrap_or(Value::Null);
    (status, body)
}

async fn send_raw(harness: &V3Harness, path: &str, body: &str) -> (StatusCode, Value) {
    let response = harness
        .client
        .post(format!("{}{path}", harness.v3_base))
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(body.to_string())
        .send()
        .await
        .unwrap();
    let status = response.status();
    let body = response.json().await.unwrap_or(Value::Null);
    (status, body)
}

fn probe_path(provider_id: &str) -> String {
    format!("/providers/{provider_id}/protocol-probes")
}

fn static_reset_path() -> String {
    static_reset_path_for(OPENCODE_PROVIDER_ID)
}

fn static_reset_path_for(provider_id: &str) -> String {
    format!("/provider-contracts/provider/{provider_id}/model-protocols/reset-static")
}

fn assert_v3_error(body: &Value, code: &str) {
    assert_eq!(body["code"], code, "{body}");
    assert!(body.get("message").and_then(Value::as_str).is_some());
    assert!(body.as_object().unwrap().contains_key("currentRevision"));
    assert!(body.as_object().unwrap().contains_key("processGeneration"));
    assert!(body.get("current_revision").is_none());
}

fn json_field_names(value: &Value) -> Vec<&str> {
    match value {
        Value::Object(map) => {
            let mut names: Vec<&str> = map.keys().map(String::as_str).collect();
            names.extend(map.values().flat_map(json_field_names));
            names
        }
        Value::Array(items) => items.iter().flat_map(json_field_names).collect(),
        _ => Vec::new(),
    }
}

fn json_string_values(value: &Value) -> Vec<&str> {
    match value {
        Value::String(text) => vec![text.as_str()],
        Value::Array(items) => items.iter().flat_map(json_string_values).collect(),
        Value::Object(map) => map.values().flat_map(json_string_values).collect(),
        _ => Vec::new(),
    }
}

fn assert_secret_free(body: &Value, secrets: &[&str]) {
    for name in json_field_names(body) {
        assert!(
            !matches!(
                name,
                "key"
                    | "password"
                    | "passwordCipher"
                    | "keyCipher"
                    | "gatewayKey"
                    | "gateway_key"
                    | "primaryKey"
                    | "primary_key"
                    | "referralCode"
                    | "referral_code"
                    | "cipher"
                    | "apiKey"
                    | "api_key"
                    | "token"
                    | "secret"
            ),
            "probe payload leaked field {name}: {body}"
        );
    }
    let encoded = body.to_string();
    for secret in secrets {
        assert!(
            !encoded.contains(secret),
            "probe payload leaked credential {secret}: {body}"
        );
    }
    for value in json_string_values(body) {
        for secret in secrets {
            assert!(
                !value.contains(secret),
                "probe payload leaked credential {secret}: {body}"
            );
        }
    }
}

fn parse_probe(body: &Value) -> ProtocolProbeResponse {
    serde_json::from_value(body.clone()).unwrap_or_else(|_| panic!("ProtocolProbeResponse: {body}"))
}

async fn create_go_account(harness: &V3Harness) -> String {
    create_go_account_with(harness, "Go probe", GO_KEY).await
}

async fn create_go_account_with(harness: &V3Harness, name: &str, key: &str) -> String {
    let (status, created) = send_json(
        harness,
        Method::POST,
        "/accounts",
        &cas(harness, json!({ "name": name, "key": key })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let id = created["account"]["id"]
        .as_str()
        .expect("created Go account id")
        .to_string();
    harness.enable_account(&id);
    id
}

async fn create_goat_account(harness: &V3Harness) -> String {
    let (status, created) = send_json(
        harness,
        Method::POST,
        "/accounts",
        &cas(
            harness,
            json!({
                "name": "GOAT probe",
                "key": GO_KEY,
                "providerId": COMMAND_CODE_PROVIDER_ID,
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let id = created["account"]["id"]
        .as_str()
        .expect("created GOAT account id")
        .to_string();
    harness.enable_account(&id);
    id
}

fn point_upstream(harness: &V3Harness, base_url: &str) {
    let mut config = harness.state.config();
    config.upstream_base_url = base_url.to_string();
    config.proxy_mode = ProxyMode::Direct;
    config.non_stream_timeout_secs = 5;
    harness.state.set_config(config).unwrap();
}

fn go_scope() -> ContractScope {
    ContractScope::provider(OPENCODE_PROVIDER_ID)
}

fn open_sqlite(harness: &V3Harness) -> rusqlite::Connection {
    let conn = rusqlite::Connection::open(harness.dir.join("data.sqlite")).unwrap();
    conn.busy_timeout(Duration::from_secs(5)).unwrap();
    conn
}

fn go_scope_revision(harness: &V3Harness) -> Option<u64> {
    harness
        .state
        .db
        .lock()
        .load_persisted_scope(&go_scope())
        .unwrap()
        .map(|row| row.revision)
}

fn load_go_evidence(
    harness: &V3Harness,
    protocol: UpstreamProtocolKind,
) -> Option<PersistedModelProtocol> {
    harness
        .state
        .db
        .lock()
        .load_model_protocol(&go_scope(), "grok-4.5", protocol)
        .unwrap()
}

#[tokio::test]
async fn protocol_probes_require_the_v3_session() {
    let harness = start_public("probes-auth").await;
    let origin = start_probe_origin(StatusCode::OK, SUCCESS_BODY, Duration::ZERO).await;
    point_upstream(&harness, &origin.url);
    let (status, body) = send_json(
        &harness,
        Method::POST,
        &probe_path(OPENCODE_PROVIDER_ID),
        &cas(
            &harness,
            json!({
                "accountId": "acct-1",
                "modelId": "grok-4.5",
                "protocols": ["responses"]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    assert_v3_error(&body, ERROR_UNAUTHORIZED);
    assert_eq!(body["currentRevision"], Value::Null);
    assert_eq!(body["processGeneration"], Value::Null);
    assert_eq!(origin.call_count(), 0);
    harness.stop();
}

#[tokio::test]
async fn opencode_static_protocol_reset_requires_the_v3_session() {
    let harness = start_public("static-protocol-reset-auth").await;
    let (status, body) = send_json(
        &harness,
        Method::POST,
        &static_reset_path(),
        &cas(&harness, json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    assert_v3_error(&body, ERROR_UNAUTHORIZED);
    harness.stop();
}

#[tokio::test]
async fn opencode_static_protocol_reset_is_cas_protected_and_restores_current_catalog_deterministically()
 {
    let harness = start_probes("static-protocol-reset").await;
    let scope = go_scope();
    let models = vec!["grok-4.5".to_string(), "future-go-model".to_string()];
    let now = chrono::Utc::now();
    harness
        .state
        .db
        .lock()
        .set_contract_catalog(
            &scope,
            &models,
            Some(now),
            CATALOG_SOURCE_OPENCODE_MODELS,
            "https://example.test/models",
            now,
        )
        .unwrap();
    harness.state.reload_provider_contracts().unwrap();
    let _docs =
        install_official_protocol_fetch_for_tests(harness.state.process_generation(), |_| {
            OfficialProtocolBaseline::mapped([("grok-4.5", UpstreamProtocolKind::Responses)])
        });

    let stale = json!({
        "expectedRevision": harness.state.settings_revision().saturating_sub(1),
        "processGeneration": harness.state.process_generation() });
    let (status, body) = send_json(&harness, Method::POST, &static_reset_path(), &stale).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_v3_error(&body, ERROR_REVISION_CONFLICT);

    let (status, body) = send_json(
        &harness,
        Method::PUT,
        "/provider-contracts/provider/opencode/model-protocol-overrides",
        &cas(
            &harness,
            json!({"overrides": [
                {"modelId": "grok-4.5", "protocol": "chat_completions", "state": "force_on"},
                {"modelId": "future-go-model", "protocol": "chat_completions", "state": "force_on"}
            ]}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let before = harness.state.settings_revision();
    let (status, reset) = send_json(
        &harness,
        Method::POST,
        &static_reset_path(),
        &cas(&harness, json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{reset}");
    assert_eq!(harness.state.settings_revision(), before + 1);
    let opencode = reset["providers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|provider| provider["providerId"] == OPENCODE_PROVIDER_ID)
        .unwrap();
    assert_eq!(opencode["staticProtocolSnapshotDate"], "2026-09-06");
    assert_eq!(opencode["catalog"]["models"], json!(models));
    let grok = opencode["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|model| model["modelId"] == "grok-4.5")
        .unwrap();
    assert_eq!(grok["protocols"]["responses"]["override"], "auto");
    assert_eq!(grok["protocols"]["responses"]["enabled"], true);
    assert_eq!(
        grok["protocols"]["chat_completions"]["override"],
        "force_off"
    );
    assert_eq!(grok["protocols"]["messages"]["override"], "force_off");
    let future = opencode["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|model| model["modelId"] == "future-go-model")
        .unwrap();
    assert_eq!(future["protocols"]["chat_completions"]["override"], "auto");
    assert_eq!(future["protocols"]["chat_completions"]["enabled"], false);
    assert_eq!(future["protocols"]["responses"]["enabled"], false);
    assert_eq!(future["protocols"]["messages"]["enabled"], false);
    let (status, repeat) = send_json(
        &harness,
        Method::POST,
        &static_reset_path(),
        &cas(&harness, json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{repeat}");
    let repeated_opencode = repeat["providers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|provider| provider["providerId"] == OPENCODE_PROVIDER_ID)
        .unwrap();
    assert_eq!(repeated_opencode["catalog"], opencode["catalog"]);
    assert_eq!(repeated_opencode["models"], opencode["models"]);
    harness.stop();
}

#[tokio::test]
async fn opencode_static_protocol_reset_does_not_mutate_when_official_docs_are_unavailable() {
    let harness = start_probes("static-protocol-reset-docs-fallback").await;
    let scope = go_scope();
    let models = vec!["grok-4.5".to_string()];
    let now = chrono::Utc::now();
    harness
        .state
        .db
        .lock()
        .set_contract_catalog(
            &scope,
            &models,
            Some(now),
            CATALOG_SOURCE_OPENCODE_MODELS,
            "https://example.test/models",
            now,
        )
        .unwrap();
    harness.state.reload_provider_contracts().unwrap();
    let _docs =
        install_official_protocol_fetch_unavailable_for_tests(harness.state.process_generation());
    let before = harness.state.settings_revision();
    let before_contracts = harness.state.provider_contracts();
    let before_scope = go_scope_revision(&harness);
    let (status, reset) = send_json(
        &harness,
        Method::POST,
        &static_reset_path(),
        &cas(&harness, json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{reset}");
    assert_v3_error(&reset, ERROR_INTERNAL);
    assert!(
        reset["message"]
            .as_str()
            .unwrap()
            .contains("without an official document")
    );
    assert_eq!(harness.state.settings_revision(), before);
    assert_eq!(go_scope_revision(&harness), before_scope);
    assert_eq!(
        before_contracts.as_ref(),
        harness.state.provider_contracts().as_ref()
    );
    harness.stop();
}

#[tokio::test]
async fn zen_static_protocol_reset_uses_go_docs_and_keeps_unknown_protocols_disabled() {
    let harness = start_probes("zen-static-protocol-reset").await;
    let scope = ContractScope::provider(OPENCODE_ZEN_FREE_PROVIDER_ID);
    let models = vec![
        "mimo-v2.5-free".to_string(),
        "grok-4.6-free".to_string(),
        "future-free".to_string(),
    ];
    let now = chrono::Utc::now();
    harness
        .state
        .db
        .lock()
        .set_contract_catalog(
            &scope,
            &models,
            Some(now),
            "official_zen",
            "https://example.test/zen",
            now,
        )
        .unwrap();
    harness.state.reload_provider_contracts().unwrap();
    let _docs =
        install_official_protocol_fetch_for_tests(harness.state.process_generation(), |_| {
            OfficialProtocolBaseline::mapped([
                ("mimo-v2.5", UpstreamProtocolKind::ChatCompletions),
                ("grok-4.6", UpstreamProtocolKind::Responses),
            ])
        });
    let stale = json!({"expectedRevision": harness.state.settings_revision().saturating_sub(1), "processGeneration": harness.state.process_generation()});
    let (status, stale_body) = send_json(
        &harness,
        Method::POST,
        &static_reset_path_for(OPENCODE_ZEN_FREE_PROVIDER_ID),
        &stale,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{stale_body}");
    let (status, reset) = send_json(
        &harness,
        Method::POST,
        &static_reset_path_for(OPENCODE_ZEN_FREE_PROVIDER_ID),
        &cas(&harness, json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{reset}");
    let zen = reset["providers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|provider| provider["providerId"] == OPENCODE_ZEN_FREE_PROVIDER_ID)
        .unwrap();
    assert_eq!(zen["staticProtocolSnapshotDate"], "2026-09-06");
    let official = zen["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|model| model["modelId"] == "mimo-v2.5-free")
        .unwrap();
    assert_eq!(
        official["protocols"]["chat_completions"]["override"],
        "auto"
    );
    assert_eq!(official["protocols"]["chat_completions"]["enabled"], true);
    assert!(official["protocols"]["responses"].is_null());
    assert!(official["protocols"]["messages"].is_null());
    let grok = zen["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|model| model["modelId"] == "grok-4.6-free")
        .unwrap();
    assert_eq!(grok["protocols"]["responses"]["override"], "auto");
    assert_eq!(grok["protocols"]["responses"]["enabled"], true);
    assert_eq!(
        grok["protocols"]["chat_completions"]["override"],
        "force_off"
    );
    assert!(grok["protocols"]["messages"].is_null());
    let future = zen["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|model| model["modelId"] == "future-free")
        .unwrap();
    assert_eq!(future["protocols"]["chat_completions"]["override"], "auto");
    assert_eq!(future["protocols"]["chat_completions"]["enabled"], false);
    assert!(future["protocols"]["responses"].is_null());
    assert!(future["protocols"]["messages"].is_null());
    harness.stop();
}

#[tokio::test]
async fn goat_static_protocol_reset_enables_documented_family_and_preserves_explicit_off() {
    let harness = start_probes("goat-static-protocol-reset").await;
    let scope = ContractScope::provider(COMMAND_CODE_PROVIDER_ID);
    let models = vec![
        "claude-fable-5".to_string(),
        "stealth/ox-alpha".to_string(),
        "future-goat-model".to_string(),
    ];
    let now = chrono::Utc::now();
    harness
        .state
        .db
        .lock()
        .set_contract_catalog(
            &scope,
            &models,
            Some(now),
            "command_code_get_models",
            "https://example.test/goat",
            now,
        )
        .unwrap();
    harness.state.reload_provider_contracts().unwrap();
    let _docs =
        install_official_protocol_fetch_for_tests(harness.state.process_generation(), |_| {
            OfficialProtocolBaseline::FamilyRule
        });
    let (status, reset) = send_json(
        &harness,
        Method::POST,
        &static_reset_path_for(COMMAND_CODE_PROVIDER_ID),
        &cas(&harness, json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{reset}");
    let goat = reset["providers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|provider| provider["providerId"] == COMMAND_CODE_PROVIDER_ID)
        .unwrap();
    assert_eq!(goat["staticProtocolSnapshotDate"], "2026-09-06");
    let fable = goat["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|model| model["modelId"] == "claude-fable-5")
        .unwrap();
    assert_eq!(fable["protocols"]["messages"]["override"], "auto");
    assert_eq!(fable["protocols"]["messages"]["source"], "static");
    assert_eq!(fable["protocols"]["messages"]["available"], true);
    assert_eq!(fable["protocols"]["messages"]["enabled"], true);
    assert!(fable["protocols"]["messages"]["verifiedAt"].is_null());
    assert!(fable["protocols"]["chat_completions"].is_null());
    assert!(fable["protocols"]["responses"].is_null());
    let stealth = goat["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|model| model["modelId"] == "stealth/ox-alpha")
        .unwrap();
    assert!(stealth["protocols"]["chat_completions"].is_null());
    assert!(stealth["protocols"]["responses"].is_null());
    assert!(stealth["protocols"]["messages"].is_null());
    let future = goat["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|model| model["modelId"] == "future-goat-model")
        .unwrap();
    assert_eq!(future["protocols"]["chat_completions"]["override"], "auto");
    assert_eq!(future["protocols"]["chat_completions"]["source"], "static");
    assert_eq!(future["protocols"]["chat_completions"]["enabled"], true);
    assert!(future["protocols"]["responses"].is_null());
    assert!(future["protocols"]["messages"].is_null());
    let (status, overridden) = send_json(&harness, Method::PUT, "/provider-contracts/provider/command-code/model-protocol-overrides", &cas(&harness, json!({"overrides":[{"modelId":"future-goat-model","protocol":"chat_completions","state":"force_off"}]}))).await;
    assert_eq!(status, StatusCode::OK, "{overridden}");
    let goat = overridden["providers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|provider| provider["providerId"] == COMMAND_CODE_PROVIDER_ID)
        .unwrap();
    let future = goat["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|model| model["modelId"] == "future-goat-model")
        .unwrap();
    assert_eq!(future["protocols"]["chat_completions"]["enabled"], false);
    harness.stop();
}

#[tokio::test]
async fn fixed_provider_resets_restore_each_documented_protocol() {
    let harness = start_probes("fixed-provider-official-protocol-reset").await;
    let now = chrono::Utc::now();
    for (provider_id, model_id) in [
        (MINIMAX_PROVIDER_ID, "MiniMax-New"),
        (KIMI_PROVIDER_ID, "kimi-new"),
    ] {
        harness
            .state
            .db
            .lock()
            .set_contract_catalog(
                &ContractScope::provider(provider_id),
                &[model_id.to_string()],
                Some(now),
                "provider_get_models",
                "https://example.test/models",
                now,
            )
            .unwrap();
    }
    harness.state.reload_provider_contracts().unwrap();

    for (provider_id, model_id) in [
        (MINIMAX_PROVIDER_ID, "MiniMax-New"),
        (KIMI_PROVIDER_ID, "kimi-new"),
    ] {
        let (status, reset) = send_json(
            &harness,
            Method::POST,
            &static_reset_path_for(provider_id),
            &cas(&harness, json!({})),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{reset}");
        let provider = reset["providers"]
            .as_array()
            .unwrap()
            .iter()
            .find(|provider| provider["providerId"] == provider_id)
            .unwrap();
        assert_eq!(provider["staticProtocolSnapshotDate"], "2026-09-06");
        let model = provider["models"]
            .as_array()
            .unwrap()
            .iter()
            .find(|model| model["modelId"] == model_id)
            .unwrap();
        assert_eq!(model["protocols"]["chat_completions"]["override"], "auto");
        assert_eq!(model["protocols"]["chat_completions"]["source"], "static");
        assert_eq!(model["protocols"]["chat_completions"]["enabled"], true);
        assert_eq!(model["protocols"]["messages"]["override"], "auto");
        assert_eq!(model["protocols"]["messages"]["source"], "static");
        assert_eq!(model["protocols"]["messages"]["enabled"], true);
        if provider_id == MINIMAX_PROVIDER_ID {
            assert_eq!(model["protocols"]["responses"]["override"], "auto");
            assert_eq!(model["protocols"]["responses"]["source"], "static");
            assert_eq!(model["protocols"]["responses"]["enabled"], true);
        } else {
            assert!(model["protocols"]["responses"].is_null());
        }
    }
    harness.stop();
}

#[tokio::test]
async fn fixed_provider_overrides_accept_minimax_responses_and_reject_kimi_responses() {
    let harness = start_probes("fixed-provider-override-ceiling").await;
    let (status, rejected) = send_json(
        &harness,
        Method::PUT,
        "/provider-contracts/provider/kimi/model-protocol-overrides",
        &cas(
            &harness,
            json!({"overrides":[{
                "modelId":"kimi-k3",
                "protocol":"responses",
                "state":"force_on"
            }]}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{rejected}");
    assert_v3_error(&rejected, ERROR_INVALID_REQUEST);

    let (status, accepted) = send_json(
        &harness,
        Method::PUT,
        "/provider-contracts/provider/minimax/model-protocol-overrides",
        &cas(
            &harness,
            json!({"overrides":[{
                "modelId":"MiniMax-M3",
                "protocol":"responses",
                "state":"force_on"
            }]}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{accepted}");
    harness.stop();
}

#[tokio::test]
async fn protocol_probes_require_cas_and_reject_stale_tokens_with_zero_upstream() {
    let harness = start_probes("probes-cas").await;
    let origin = start_probe_origin(StatusCode::OK, SUCCESS_BODY, Duration::ZERO).await;
    point_upstream(&harness, &origin.url);
    let account_id = create_go_account(&harness).await;
    let before = harness.state.settings_revision();
    let path = probe_path(OPENCODE_PROVIDER_ID);

    let (status, missing) = send_raw(
        &harness,
        &path,
        &json!({
            "processGeneration": harness.state.process_generation(),
            "accountId": account_id,
            "modelId": "grok-4.5",
            "protocols": ["responses"]
        })
        .to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{missing}");
    assert_v3_error(&missing, ERROR_MISSING_EXPECTED_REVISION);
    assert_eq!(harness.state.settings_revision(), before);

    let (status, stale) = send_json(
        &harness,
        Method::POST,
        &path,
        &json!({
            "expectedRevision": 1,
            "processGeneration": harness.state.process_generation(),
            "accountId": account_id,
            "modelId": "grok-4.5",
            "protocols": ["responses"]
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{stale}");
    assert_v3_error(&stale, ERROR_REVISION_CONFLICT);
    assert_eq!(harness.state.settings_revision(), before);
    assert_eq!(origin.call_count(), 0);
    harness.stop();
}

#[tokio::test]
async fn protocol_probes_zero_call_gates_do_not_touch_upstream() {
    let harness = start_probes("probes-zero-call").await;
    let origin = start_probe_origin(StatusCode::OK, SUCCESS_BODY, Duration::ZERO).await;
    point_upstream(&harness, &origin.url);
    let account_id = create_go_account(&harness).await;
    let before = harness.state.settings_revision();

    let (status, duplicate) = send_json(
        &harness,
        Method::POST,
        &probe_path(OPENCODE_PROVIDER_ID),
        &cas(
            &harness,
            json!({
                "accountId": account_id,
                "modelId": "grok-4.5",
                "protocols": ["responses", "chat_completions", "responses"]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{duplicate}");
    assert_v3_error(&duplicate, ERROR_INVALID_REQUEST);

    let (status, empty) = send_json(
        &harness,
        Method::POST,
        &probe_path(OPENCODE_PROVIDER_ID),
        &cas(
            &harness,
            json!({
                "accountId": account_id,
                "modelId": "grok-4.5",
                "protocols": []
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{empty}");
    assert_v3_error(&empty, ERROR_INVALID_REQUEST);

    let (status, blank_model) = send_json(
        &harness,
        Method::POST,
        &probe_path(OPENCODE_PROVIDER_ID),
        &cas(
            &harness,
            json!({
                "accountId": account_id,
                "modelId": "  ",
                "protocols": ["responses"]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{blank_model}");
    assert_v3_error(&blank_model, ERROR_INVALID_REQUEST);

    let (status, unknown_protocol) = send_raw(
        &harness,
        &probe_path(OPENCODE_PROVIDER_ID),
        &cas(
            &harness,
            json!({
                "accountId": account_id,
                "modelId": "grok-4.5",
                "protocols": ["gemini"]
            }),
        )
        .to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{unknown_protocol}");
    assert_v3_error(&unknown_protocol, ERROR_INVALID_JSON);

    let (status, custom) = send_json(
        &harness,
        Method::POST,
        &probe_path(CUSTOM_PROVIDER_ID),
        &cas(
            &harness,
            json!({
                "modelId": "org/model",
                "protocols": ["chat_completions"]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{custom}");
    assert_v3_error(&custom, ERROR_INVALID_REQUEST);

    for (provider_id, model_id) in [
        (MINIMAX_PROVIDER_ID, "MiniMax-M3"),
        (KIMI_PROVIDER_ID, "kimi-for-coding"),
    ] {
        let (status, missing_account) = send_json(
            &harness,
            Method::POST,
            &probe_path(provider_id),
            &cas(
                &harness,
                json!({
                    "modelId": model_id,
                    "protocols": ["chat_completions", "responses", "messages"]
                }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{missing_account}");
        assert_v3_error(&missing_account, ERROR_INVALID_REQUEST);
    }

    let (status, unknown_provider) = send_json(
        &harness,
        Method::POST,
        "/providers/not-a-provider/protocol-probes",
        &cas(
            &harness,
            json!({
                "accountId": account_id,
                "modelId": "grok-4.5",
                "protocols": ["responses"]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{unknown_provider}");
    assert_v3_error(&unknown_provider, ERROR_NOT_FOUND);

    assert_eq!(origin.call_count(), 0);
    assert_eq!(harness.state.settings_revision(), before);
    harness.stop();
}

#[tokio::test]
async fn go_protocol_probes_send_one_admin_post_per_protocol_with_correct_path_and_auth() {
    let harness = start_probes("probes-go-n").await;
    let origin = start_probe_origin(StatusCode::OK, SUCCESS_BODY, Duration::ZERO).await;
    point_upstream(&harness, &origin.url);
    create_go_account(&harness).await;
    let before = harness.state.settings_revision();

    let (status, body) = send_json(
        &harness,
        Method::POST,
        &probe_path(OPENCODE_PROVIDER_ID),
        &cas(
            &harness,
            json!({
                "modelId": "grok-4.5",
                "protocols": ["chat_completions", "responses", "messages"]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let parsed = parse_probe(&body);
    assert_eq!(parsed.account_id, None);
    assert_eq!(parsed.provider_id, OPENCODE_PROVIDER_ID);
    assert_eq!(parsed.model_id, "grok-4.5");
    assert_eq!(parsed.results.len(), 3);
    assert!(
        parsed
            .results
            .iter()
            .all(|result| result.success && !result.skipped)
    );
    assert!(parsed.contract.is_some());
    assert_eq!(parsed.revision, before + 1);
    assert_eq!(harness.state.settings_revision(), before + 1);
    assert_secret_free(&body, &[GO_KEY]);

    let calls = origin.calls.lock().unwrap().clone();
    assert_eq!(calls.len(), 3, "{calls:?}");
    assert!(calls.iter().all(|call| call.method == "POST"));
    assert!(calls.iter().all(|call| call.body.contains("grok-4.5")));
    assert_eq!(calls[0].path, "/v1/chat/completions");
    assert_eq!(
        calls[0].authorization.as_deref(),
        Some("Bearer sk-probe-secret-key")
    );
    assert!(calls[0].x_api_key.is_none());
    assert_eq!(calls[1].path, "/v1/responses");
    assert_eq!(
        calls[1].authorization.as_deref(),
        Some("Bearer sk-probe-secret-key")
    );
    assert!(calls[1].x_api_key.is_none());
    assert_eq!(calls[2].path, "/v1/messages");
    assert!(calls[2].authorization.is_none());
    assert_eq!(calls[2].x_api_key.as_deref(), Some(GO_KEY));
    harness.stop();
}

#[tokio::test]
async fn goat_protocol_probes_use_only_each_models_sealed_native_family_path() {
    let harness = start_probes("probes-goat-native-family").await;
    let origin = start_probe_origin(StatusCode::OK, SUCCESS_BODY, Duration::ZERO).await;
    let account_id = create_goat_account(&harness).await;
    let _route = install_goat_loopback_route_for_test(&account_id, &origin.url).unwrap();
    let scope = ContractScope::provider(COMMAND_CODE_PROVIDER_ID);
    let now = chrono::Utc::now();
    harness
        .state
        .db
        .lock()
        .set_contract_catalog(
            &scope,
            &[
                "deepseek/deepseek-v4-flash".to_string(),
                "claude-sonnet-5".to_string(),
            ],
            Some(now),
            "command_code_get_models",
            "https://example.test/goat",
            now,
        )
        .unwrap();
    harness.state.reload_provider_contracts().unwrap();

    for (model_id, expected_protocols) in [
        (
            "deepseek/deepseek-v4-flash",
            vec![
                AccountUpstreamProtocol::ChatCompletions,
                AccountUpstreamProtocol::Responses,
            ],
        ),
        ("claude-sonnet-5", vec![AccountUpstreamProtocol::Messages]),
    ] {
        let previously_enabled = harness
            .state
            .provider_contracts()
            .scope(&scope)
            .and_then(|scope| scope.model(model_id))
            .unwrap()
            .enabled_protocols();
        let (status, body) = send_json(
            &harness,
            Method::POST,
            &probe_path(COMMAND_CODE_PROVIDER_ID),
            &cas(
                &harness,
                json!({
                    "modelId": model_id,
                    "protocols": ["chat_completions", "responses", "messages"]
                }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let parsed = parse_probe(&body);
        assert_eq!(parsed.results.len(), expected_protocols.len(), "{body}");
        assert_eq!(
            parsed
                .results
                .iter()
                .map(|result| result.protocol)
                .collect::<Vec<_>>(),
            expected_protocols
        );
        assert!(parsed.results.iter().all(|result| result.success));
        let contract = parsed
            .contract
            .expect("probe returns the updated model contract");
        for expected_protocol in expected_protocols {
            let evidence = match expected_protocol {
                AccountUpstreamProtocol::ChatCompletions => {
                    contract.protocols.chat_completions.as_ref()
                }
                AccountUpstreamProtocol::Responses => contract.protocols.responses.as_ref(),
                AccountUpstreamProtocol::Messages => contract.protocols.messages.as_ref(),
            }
            .expect("probed family protocol");
            assert!(evidence.available);
            assert_eq!(
                evidence.enabled,
                previously_enabled.contains(&expected_protocol.into())
            );
        }
        assert_secret_free(&body, &[GO_KEY]);
    }

    let calls = origin.calls.lock().unwrap().clone();
    assert_eq!(calls.len(), 3, "{calls:?}");
    assert_eq!(calls[0].path, "/provider/v1/chat/completions");
    assert_eq!(calls[1].path, "/provider/v1/responses");
    assert_eq!(calls[2].path, "/provider/v1/messages");
    assert!(calls.iter().all(|call| {
        call.authorization.as_deref() == Some("Bearer sk-probe-secret-key")
            && call.x_api_key.is_none()
            && call.cookie.is_none()
            && call.opencode_session.is_none()
    }));
    harness.stop();
}

#[tokio::test]
async fn protocol_probe_falls_back_to_the_next_eligible_account() {
    const BAD_KEY: &str = "sk-probe-first-unavailable";
    let harness = start_probes("probes-account-fallback").await;
    let origin = start_fallback_probe_origin(BAD_KEY).await;
    point_upstream(&harness, &origin.url);
    let first_id = create_go_account_with(&harness, "First unavailable", BAD_KEY).await;
    let second_id = create_go_account_with(&harness, "Second available", GO_KEY).await;

    let (status, body) = send_json(
        &harness,
        Method::POST,
        &probe_path(OPENCODE_PROVIDER_ID),
        &cas(
            &harness,
            json!({
                "modelId": "grok-4.5",
                "protocols": ["responses"]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let parsed = parse_probe(&body);
    assert_eq!(parsed.account_id, None);
    assert!(parsed.results[0].success, "{body}");
    assert_eq!(parsed.results[0].error, None);

    let calls = origin.calls.lock().unwrap().clone();
    assert_eq!(calls.len(), 2, "{calls:?}");
    assert_eq!(
        calls[0].authorization.as_deref(),
        Some("Bearer sk-probe-first-unavailable")
    );
    assert_eq!(
        calls[1].authorization.as_deref(),
        Some("Bearer sk-probe-secret-key")
    );

    let db = harness.state.db.lock();
    let mut logs: Vec<_> = db
        .list_forward_logs(100)
        .unwrap()
        .into_iter()
        .filter(|row| {
            row.diagnostic
                .as_ref()
                .and_then(|value| value["event"].as_str())
                == Some("protocol_probe")
        })
        .collect();
    let runtime_logs = db.list_gateway_logs(100).unwrap();
    drop(db);
    logs.sort_by_key(|row| row.attempt);
    assert_eq!(logs.len(), 2, "{logs:?}");
    assert_eq!(logs[0].account_id, first_id);
    assert_eq!(logs[0].status, "error");
    assert_eq!(logs[0].http_status, Some(401));
    assert_eq!(logs[1].account_id, second_id);
    assert_eq!(logs[1].status, "success");
    assert_eq!(logs[1].http_status, Some(200));
    assert_eq!(logs[0].request_id, logs[1].request_id);
    assert_eq!(logs[0].attempt, Some(1));
    assert_eq!(logs[1].attempt, Some(2));
    assert!(
        runtime_logs
            .iter()
            .all(|row| row.category != "protocol_probe")
    );

    let stored = harness
        .state
        .provider_contracts()
        .scope(&go_scope())
        .and_then(|scope| scope.model("grok-4.5").cloned())
        .unwrap();
    assert_eq!(
        stored.protocols.get("responses").unwrap().r#override,
        ProtocolOverrideState::Auto
    );
    harness.stop();
}

#[tokio::test]
async fn protocol_probe_without_eligible_accounts_is_a_zero_call_rejection() {
    let harness = start_probes("probes-no-eligible-account").await;
    let origin = start_probe_origin(StatusCode::OK, SUCCESS_BODY, Duration::ZERO).await;
    point_upstream(&harness, &origin.url);
    let before = harness.state.settings_revision();

    let (status, body) = send_json(
        &harness,
        Method::POST,
        &probe_path(OPENCODE_PROVIDER_ID),
        &cas(
            &harness,
            json!({
                "modelId": "grok-4.5",
                "protocols": ["responses"]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_v3_error(&body, ERROR_INVALID_REQUEST);
    assert_eq!(origin.call_count(), 0);
    assert_eq!(harness.state.settings_revision(), before);
    assert!(
        harness
            .state
            .db
            .lock()
            .list_forward_logs(100)
            .unwrap()
            .is_empty()
    );
    harness.stop();
}

#[tokio::test]
async fn zen_protocol_probe_omits_auth_and_selects_the_singleton_internally() {
    let harness = start_probes("probes-zen").await;
    let origin = start_probe_origin(StatusCode::OK, SUCCESS_BODY, Duration::ZERO).await;
    point_upstream(&harness, &format!("{}/zen/go", origin.url));
    let (status, body) = send_json(
        &harness,
        Method::POST,
        &probe_path(OPENCODE_ZEN_FREE_PROVIDER_ID),
        &cas(
            &harness,
            json!({
                "modelId": "mimo-v2.5-free",
                "protocols": ["chat_completions"]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let parsed = parse_probe(&body);
    assert_eq!(parsed.account_id, None);
    assert_eq!(parsed.provider_id, OPENCODE_ZEN_FREE_PROVIDER_ID);
    assert!(parsed.results[0].success);
    assert_eq!(origin.call_count(), 1);
    let call = origin.calls.lock().unwrap()[0].clone();
    assert_eq!(call.method, "POST");
    assert_eq!(call.path, "/zen/v1/chat/completions");
    assert!(call.authorization.is_none(), "{call:?}");
    assert!(call.x_api_key.is_none(), "{call:?}");
    harness.stop();
}

#[tokio::test]
async fn model_outside_provider_catalog_is_rejected_without_bump_or_upstream() {
    let harness = start_probes("probes-ceiling").await;
    let origin = start_probe_origin(StatusCode::OK, SUCCESS_BODY, Duration::ZERO).await;
    point_upstream(&harness, &origin.url);
    let account_id = create_go_account(&harness).await;
    let before = harness.state.settings_revision();

    let (status, body) = send_json(
        &harness,
        Method::POST,
        &probe_path(OPENCODE_PROVIDER_ID),
        &cas(
            &harness,
            json!({
                "accountId": account_id,
                "modelId": "not-a-known-model",
                "protocols": ["chat_completions", "responses"]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], ERROR_INVALID_REQUEST);
    assert_eq!(harness.state.settings_revision(), before);
    assert_eq!(origin.call_count(), 0);
    harness.stop();
}

#[tokio::test]
async fn fetched_catalog_model_probes_all_protocols_and_writes_request_logs() {
    let harness = start_probes("probes-fetched-model").await;
    let origin = start_probe_origin(StatusCode::OK, SUCCESS_BODY, Duration::ZERO).await;
    point_upstream(&harness, &origin.url);
    let account_id = create_go_account(&harness).await;
    let now = chrono::Utc::now();
    harness
        .state
        .db
        .lock()
        .set_contract_catalog(
            &go_scope(),
            &["future-go-model".to_string()],
            Some(now),
            CATALOG_SOURCE_OPENCODE_MODELS,
            "http://127.0.0.1/provider/v1/models",
            now,
        )
        .unwrap();
    harness.state.reload_provider_contracts().unwrap();

    let (status, body) = send_json(
        &harness,
        Method::POST,
        &probe_path(OPENCODE_PROVIDER_ID),
        &cas(
            &harness,
            json!({
                "accountId": account_id,
                "modelId": "future-go-model",
                "protocols": ["chat_completions", "responses", "messages"]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let parsed = parse_probe(&body);
    assert_eq!(parsed.results.len(), 3);
    assert!(
        parsed
            .results
            .iter()
            .all(|result| result.success && !result.skipped),
        "{body}"
    );
    assert_eq!(origin.call_count(), 3);

    let db = harness.state.db.lock();
    let logs: Vec<_> = db
        .list_forward_logs(100)
        .unwrap()
        .into_iter()
        .filter(|row| {
            row.diagnostic
                .as_ref()
                .and_then(|value| value["event"].as_str())
                == Some("protocol_probe")
        })
        .collect();
    let runtime_logs = db.list_gateway_logs(100).unwrap();
    drop(db);
    assert_eq!(logs.len(), 3, "{logs:?}");
    assert!(logs.iter().all(|row| {
        row.model == "future-go-model"
            && row.account_id == account_id
            && row.provider_id.as_deref() == Some(OPENCODE_PROVIDER_ID)
            && row.status == "success"
            && row.http_status == Some(200)
            && row.cost_state == "not_applicable"
            && row.prompt_tokens == 0
            && row.completion_tokens == 0
            && row.client_key_id.is_none()
    }));
    let request_ids: std::collections::HashSet<_> =
        logs.iter().map(|row| row.request_id.as_deref()).collect();
    assert_eq!(
        request_ids.len(),
        1,
        "one request id groups the probe batch"
    );
    assert!(request_ids.iter().next().unwrap().is_some());
    let mut attempts: Vec<_> = logs.iter().filter_map(|row| row.attempt).collect();
    attempts.sort_unstable();
    assert_eq!(attempts, vec![1, 2, 3]);
    assert!(
        runtime_logs
            .iter()
            .all(|row| row.category != "protocol_probe"),
        "request-related probes must not enter runtime logs: {runtime_logs:?}"
    );
    harness.stop();
}

#[tokio::test]
async fn removed_and_zen_owned_go_catalog_models_cannot_be_probed() {
    let harness = start_probes("probes-current-catalog-only").await;
    let origin = start_probe_origin(StatusCode::OK, SUCCESS_BODY, Duration::ZERO).await;
    point_upstream(&harness, &origin.url);
    let account_id = create_go_account(&harness).await;
    let now = chrono::Utc::now();
    harness
        .state
        .db
        .lock()
        .set_contract_catalog(
            &go_scope(),
            &["future-go-model".to_string()],
            Some(now),
            CATALOG_SOURCE_OPENCODE_MODELS,
            "http://127.0.0.1/provider/v1/models",
            now,
        )
        .unwrap();
    harness.state.reload_provider_contracts().unwrap();
    let (status, body) = send_json(
        &harness,
        Method::POST,
        &probe_path(OPENCODE_PROVIDER_ID),
        &cas(
            &harness,
            json!({
                "accountId": account_id,
                "modelId": "future-go-model",
                "protocols": ["chat_completions"]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(origin.call_count(), 1);

    let refreshed = chrono::Utc::now();
    harness
        .state
        .db
        .lock()
        .set_contract_catalog(
            &go_scope(),
            &["glm-5.3".to_string()],
            Some(refreshed),
            CATALOG_SOURCE_OPENCODE_MODELS,
            "http://127.0.0.1/provider/v1/models",
            refreshed,
        )
        .unwrap();
    harness.state.reload_provider_contracts().unwrap();
    assert!(
        harness
            .state
            .db
            .lock()
            .load_model_protocol(
                &go_scope(),
                "future-go-model",
                UpstreamProtocolKind::ChatCompletions,
            )
            .unwrap()
            .is_some(),
        "historical evidence remains persisted for this admission regression"
    );
    let (status, body) = send_json(
        &harness,
        Method::POST,
        &probe_path(OPENCODE_PROVIDER_ID),
        &cas(
            &harness,
            json!({
                "accountId": account_id,
                "modelId": "future-go-model",
                "protocols": ["chat_completions"]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(origin.call_count(), 1);

    let legacy = chrono::Utc::now();
    harness
        .state
        .db
        .lock()
        .set_contract_catalog(
            &go_scope(),
            &["hy3-free".to_string()],
            Some(legacy),
            CATALOG_SOURCE_OPENCODE_MODELS,
            "http://127.0.0.1/provider/v1/models",
            legacy,
        )
        .unwrap();
    harness.state.reload_provider_contracts().unwrap();
    let (status, body) = send_json(
        &harness,
        Method::POST,
        &probe_path(OPENCODE_PROVIDER_ID),
        &cas(
            &harness,
            json!({
                "accountId": account_id,
                "modelId": "hy3-free",
                "protocols": ["chat_completions"]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(origin.call_count(), 1);
    harness.stop();
}

#[tokio::test]
async fn s04_probe_errors_and_logs_redact_secrets() {
    let harness = start_probes("probes-failure").await;
    let origin = start_probe_origin(
        StatusCode::INTERNAL_SERVER_ERROR,
        &format!(r#"{{"error":"leaked {GO_KEY}"}}"#),
        Duration::ZERO,
    )
    .await;
    point_upstream(&harness, &origin.url);
    let account_id = create_go_account(&harness).await;
    let before = harness.state.settings_revision();

    let (status, body) = send_json(
        &harness,
        Method::POST,
        &probe_path(OPENCODE_PROVIDER_ID),
        &cas(
            &harness,
            json!({
                "accountId": account_id,
                "modelId": "grok-4.5",
                "protocols": ["chat_completions"]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let parsed = parse_probe(&body);
    assert!(!parsed.results[0].success);
    assert!(!parsed.results[0].skipped);
    assert!(parsed.results[0].error.is_some());
    assert_eq!(parsed.revision, before + 1);
    assert_secret_free(&body, &[GO_KEY]);
    let stored = harness
        .state
        .provider_contracts()
        .scope(&ContractScope::provider(OPENCODE_PROVIDER_ID))
        .and_then(|scope| scope.model("grok-4.5").cloned())
        .unwrap();
    let chat = stored.protocols.get("chat_completions").unwrap();
    assert!(!chat.available);
    assert_eq!(chat.last_probe_result, Some(ProbeResultKind::Failure));
    assert_eq!(origin.call_count(), 1);
    let db = harness.state.db.lock();
    let log = db
        .list_forward_logs(100)
        .unwrap()
        .into_iter()
        .find(|row| {
            row.diagnostic
                .as_ref()
                .and_then(|value| value["event"].as_str())
                == Some("protocol_probe")
        })
        .expect("failed probe writes a request log");
    let runtime_logs = db.list_gateway_logs(100).unwrap();
    drop(db);
    assert_eq!(log.status, "error");
    assert_eq!(log.http_status, Some(500));
    assert_eq!(log.error_stage.as_deref(), Some("protocol_probe"));
    assert_secret_free(&serde_json::to_value(&log).unwrap(), &[GO_KEY]);
    assert!(
        runtime_logs
            .iter()
            .all(|row| row.category != "protocol_probe"),
        "failed request-related probes must not enter runtime logs: {runtime_logs:?}"
    );
    harness.stop();
}

#[tokio::test]
async fn connection_test_success_and_failure_never_change_protocol_overrides() {
    let harness = start_probes("probes-write-overrides").await;
    let ok_origin = start_probe_origin(StatusCode::OK, SUCCESS_BODY, Duration::ZERO).await;
    point_upstream(&harness, &ok_origin.url);
    let account_id = create_go_account(&harness).await;

    // Success records reachability, not a routing configuration change.
    let (status, body) = send_json(
        &harness,
        Method::POST,
        &probe_path(OPENCODE_PROVIDER_ID),
        &cas(
            &harness,
            json!({
                "accountId": account_id,
                "modelId": "grok-4.5",
                "protocols": ["chat_completions", "responses"]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let stored = harness
        .state
        .provider_contracts()
        .scope(&go_scope())
        .and_then(|scope| scope.model("grok-4.5").cloned())
        .unwrap();
    for protocol in ["chat_completions", "responses"] {
        let row = stored.protocols.get(protocol).unwrap();
        assert_eq!(row.r#override, ProtocolOverrideState::Auto, "{protocol}");
        assert_eq!(row.enabled, protocol == "responses", "{protocol}");
        assert_eq!(row.last_probe_result, Some(ProbeResultKind::Success));
    }

    // A failed account-level attempt records evidence but never pins a shared
    // provider protocol force_off.
    let fail_origin = start_probe_origin(
        StatusCode::INTERNAL_SERVER_ERROR,
        r#"{"error":"down"}"#,
        Duration::ZERO,
    )
    .await;
    point_upstream(&harness, &fail_origin.url);
    let (status, body) = send_json(
        &harness,
        Method::POST,
        &probe_path(OPENCODE_PROVIDER_ID),
        &cas(
            &harness,
            json!({
                "accountId": account_id,
                "modelId": "grok-4.5",
                "protocols": ["messages"]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let parsed = parse_probe(&body);
    assert!(!parsed.results[0].success);
    assert!(!parsed.results[0].skipped);
    let stored = harness
        .state
        .provider_contracts()
        .scope(&go_scope())
        .and_then(|scope| scope.model("grok-4.5").cloned())
        .unwrap();
    let messages = stored.protocols.get("messages").unwrap();
    assert_eq!(messages.r#override, ProtocolOverrideState::Auto);
    assert!(!messages.enabled);
    assert_eq!(
        stored.protocols.get("chat_completions").unwrap().r#override,
        ProtocolOverrideState::Auto
    );

    // No hidden override rows were written to persistence either.
    let conn = open_sqlite(&harness);
    let mut statement = conn
        .prepare(
            "SELECT protocol, state FROM provider_contract_model_protocol_overrides
             WHERE scope_kind = 'provider' AND scope_id = ?1 AND model_id = 'grok-4.5'
             ORDER BY protocol",
        )
        .unwrap();
    let rows: Vec<(String, String)> = statement
        .query_map([OPENCODE_PROVIDER_ID], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert!(rows.is_empty(), "{rows:?}");
    drop(statement);
    drop(conn);
    harness.stop();
}

#[tokio::test]
async fn successful_test_records_observation_and_does_not_forward_dashboard_headers() {
    let harness = start_probes("probes-success-headers").await;
    let origin = start_probe_origin(StatusCode::OK, SUCCESS_BODY, Duration::ZERO).await;
    point_upstream(&harness, &origin.url);
    let account_id = create_go_account(&harness).await;
    let before = harness.state.settings_revision();
    let payload = cas(
        &harness,
        json!({
            "accountId": account_id,
            "modelId": "grok-4.5",
            "protocols": ["chat_completions"]
        }),
    );
    let response = harness
        .client
        .post(format!(
            "{}{}",
            harness.v3_base,
            probe_path(OPENCODE_PROVIDER_ID)
        ))
        .header(
            reqwest::header::COOKIE,
            "ocg_dashboard_session=should-not-leak",
        )
        .header(reqwest::header::AUTHORIZATION, "Bearer dashboard-token")
        .json(&payload)
        .send()
        .await
        .unwrap();
    let status = response.status();
    let body: Value = response.json().await.unwrap();
    assert_eq!(status, StatusCode::OK, "{body}");
    let parsed = parse_probe(&body);
    assert!(parsed.results[0].success);
    let contract = parsed
        .contract
        .expect("test should return the model contract");
    assert!(
        contract
            .protocols
            .chat_completions
            .as_ref()
            .is_some_and(|row| !row.enabled)
    );
    assert_eq!(parsed.revision, before + 1);
    let call = origin.calls.lock().unwrap()[0].clone();
    assert!(call.cookie.is_none(), "{call:?}");
    assert_eq!(
        call.authorization.as_deref(),
        Some("Bearer sk-probe-secret-key")
    );
    assert_ne!(
        call.authorization.as_deref(),
        Some("Bearer dashboard-token")
    );
    harness.stop();
}

#[tokio::test]
async fn protocol_probes_use_the_default_proxy_leg_not_the_model_exception() {
    let harness = start_probes("probes-proxy-leg").await;
    let origin = start_probe_origin(StatusCode::OK, SUCCESS_BODY, Duration::ZERO).await;
    let account_id = create_go_account(&harness).await;
    let mut config = harness.state.config();
    config.upstream_base_url = origin.url.clone();
    config.proxy_mode = ProxyMode::List;
    config.proxy_list_direction = ProxyListDirection::Whitelist;
    config.proxy_list_models = vec!["grok-4.5".into()];
    config.proxy_url = "http://127.0.0.1:1".into();
    config.non_stream_timeout_secs = 5;
    harness.state.set_config(config).unwrap();

    let (status, body) = send_json(
        &harness,
        Method::POST,
        &probe_path(OPENCODE_PROVIDER_ID),
        &cas(
            &harness,
            json!({
                "accountId": account_id,
                "modelId": "grok-4.5",
                "protocols": ["responses"]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let parsed = parse_probe(&body);
    assert!(
        parsed.results[0].success,
        "default-leg whitelist is direct; model-exception proxy would fail: {:?}",
        parsed.results[0].error
    );
    assert_eq!(origin.call_count(), 1);
    harness.stop();
}

#[tokio::test]
async fn cas_change_during_outbound_rejects_probe_commit() {
    let harness = start_probes("probes-cas-during").await;
    let origin = start_probe_origin(StatusCode::OK, SUCCESS_BODY, Duration::from_millis(400)).await;
    point_upstream(&harness, &origin.url);
    let account_id = create_go_account(&harness).await;
    let before = harness.state.settings_revision();
    let start_scope = go_scope_revision(&harness);
    assert!(
        start_scope.is_some(),
        "seeded Go catalog persists a contract scope"
    );
    let payload = cas(
        &harness,
        json!({
            "accountId": account_id,
            "modelId": "grok-4.5",
            "protocols": ["chat_completions"]
        }),
    );
    let client = harness.client.clone();
    let url = format!("{}{}", harness.v3_base, probe_path(OPENCODE_PROVIDER_ID));
    let pending = tokio::spawn(async move {
        let response = client.post(url).json(&payload).send().await.unwrap();
        let status = response.status();
        let body = response.json().await.unwrap_or(Value::Null);
        (status, body)
    });
    tokio::time::sleep(Duration::from_millis(120)).await;
    let mid = harness.state.bump_settings_revision();
    assert_eq!(mid, before + 1);
    let (status, body) = pending.await.unwrap();
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], ERROR_REVISION_CONFLICT);
    assert_eq!(harness.state.settings_revision(), mid);
    assert_eq!(go_scope_revision(&harness), start_scope);
    assert_eq!(origin.call_count(), 1);
    let db = harness.state.db.lock();
    let request_logs = db.list_forward_logs(100).unwrap();
    let runtime_logs = db.list_gateway_logs(100).unwrap();
    drop(db);
    assert!(
        request_logs.iter().any(|row| {
            row.diagnostic
                .as_ref()
                .and_then(|value| value["event"].as_str())
                == Some("protocol_probe")
        }),
        "the real upstream attempt remains visible even when its stale result is not committed"
    );
    assert!(
        runtime_logs
            .iter()
            .all(|row| row.category != "protocol_probe"),
        "request-related probes must not enter runtime logs: {runtime_logs:?}"
    );
    harness.stop();
}

#[tokio::test]
async fn two_protocol_success_stores_both_rows_and_bumps_nested_scope_once() {
    let harness = start_probes("probes-batch-success").await;
    let origin = start_probe_origin(StatusCode::OK, SUCCESS_BODY, Duration::ZERO).await;
    point_upstream(&harness, &origin.url);
    let account_id = create_go_account(&harness).await;
    let before = harness.state.settings_revision();
    let start_scope = go_scope_revision(&harness);
    assert!(
        start_scope.is_some(),
        "seeded Go catalog persists a contract scope"
    );

    let (status, body) = send_json(
        &harness,
        Method::POST,
        &probe_path(OPENCODE_PROVIDER_ID),
        &cas(
            &harness,
            json!({
                "accountId": account_id,
                "modelId": "grok-4.5",
                "protocols": ["chat_completions", "responses"]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let parsed = parse_probe(&body);
    assert_eq!(parsed.results.len(), 2);
    assert!(
        parsed
            .results
            .iter()
            .all(|result| result.success && !result.skipped)
    );
    assert_eq!(parsed.revision, before + 1);
    assert_eq!(harness.state.settings_revision(), before + 1);
    assert_eq!(
        go_scope_revision(&harness),
        start_scope.map(|revision| revision + 1)
    );
    assert!(load_go_evidence(&harness, UpstreamProtocolKind::ChatCompletions).is_some());
    assert!(load_go_evidence(&harness, UpstreamProtocolKind::Responses).is_some());
    let stored = harness
        .state
        .provider_contracts()
        .scope(&go_scope())
        .and_then(|scope| scope.model("grok-4.5").cloned())
        .unwrap();
    assert!(!stored.protocols["chat_completions"].enabled);
    assert!(stored.protocols["responses"].available);
    assert_eq!(origin.call_count(), 2);
    harness.stop();
}

#[tokio::test]
async fn two_protocol_batch_rolls_back_when_second_observation_write_fails() {
    let harness = start_probes("probes-batch-fault").await;
    let origin = start_probe_origin(StatusCode::OK, SUCCESS_BODY, Duration::ZERO).await;
    point_upstream(&harness, &origin.url);
    let account_id = create_go_account(&harness).await;
    let before = harness.state.settings_revision();
    let before_contracts = harness.state.provider_contracts();
    let start_scope = go_scope_revision(&harness);
    assert!(
        start_scope.is_some(),
        "seeded Go catalog persists a contract scope"
    );

    let conn = open_sqlite(&harness);
    conn.execute_batch(
        "CREATE TRIGGER fail_second_probe_observation_write
         BEFORE INSERT ON provider_contract_model_protocols
         WHEN NEW.protocol = 'responses'
         BEGIN
             SELECT RAISE(ABORT, 'injected second observation write failure');
         END;",
    )
    .unwrap();

    let (status, body) = send_json(
        &harness,
        Method::POST,
        &probe_path(OPENCODE_PROVIDER_ID),
        &cas(
            &harness,
            json!({
                "accountId": account_id,
                "modelId": "grok-4.5",
                "protocols": ["chat_completions", "responses"]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
    assert_v3_error(&body, ERROR_INTERNAL);
    assert_eq!(origin.call_count(), 2);
    assert_eq!(harness.state.settings_revision(), before);
    assert_eq!(go_scope_revision(&harness), start_scope);
    assert!(load_go_evidence(&harness, UpstreamProtocolKind::ChatCompletions).is_none());
    let responses = load_go_evidence(&harness, UpstreamProtocolKind::Responses)
        .expect("rolled-back probe must keep the official-docs Responses row");
    assert_eq!(responses.source, ContractEvidenceSource::Static);
    assert_eq!(responses.last_probe_result, None);
    assert_eq!(
        before_contracts.as_ref(),
        harness.state.provider_contracts().as_ref()
    );
    drop(conn);
    harness.stop();
}

#[tokio::test]
async fn probe_commit_advances_global_revision_before_reload_failure() {
    let harness = start_probes("probes-reload-fail").await;
    let origin = start_probe_origin(StatusCode::OK, SUCCESS_BODY, Duration::ZERO).await;
    point_upstream(&harness, &origin.url);
    let account_id = create_go_account(&harness).await;
    let before = harness.state.settings_revision();
    let before_contracts = harness.state.provider_contracts();
    let start_scope = go_scope_revision(&harness);
    assert!(
        start_scope.is_some(),
        "seeded Go catalog persists a contract scope"
    );

    let conn = open_sqlite(&harness);
    conn.execute_batch(
        "CREATE TRIGGER corrupt_probe_evidence_post_commit
         AFTER INSERT ON provider_contract_model_protocols
         BEGIN
             UPDATE provider_contract_model_protocols
                SET source = 'invalid-after-commit'
              WHERE scope_kind = NEW.scope_kind
                AND scope_id = NEW.scope_id
                AND model_id = NEW.model_id
                AND protocol = NEW.protocol;
         END;",
    )
    .unwrap();

    let (status, body) = send_json(
        &harness,
        Method::POST,
        &probe_path(OPENCODE_PROVIDER_ID),
        &cas(
            &harness,
            json!({
                "accountId": account_id,
                "modelId": "grok-4.5",
                "protocols": ["chat_completions"]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
    assert_v3_error(&body, ERROR_INTERNAL);
    assert_eq!(origin.call_count(), 1);
    assert_eq!(harness.state.settings_revision(), before + 1);
    assert_eq!(
        go_scope_revision(&harness),
        start_scope.map(|revision| revision + 1)
    );
    let stored_source: String = conn
        .query_row(
            "SELECT source FROM provider_contract_model_protocols
             WHERE scope_kind = 'provider' AND scope_id = ?1
               AND model_id = 'grok-4.5' AND protocol = 'chat_completions'",
            [OPENCODE_PROVIDER_ID],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(stored_source, "invalid-after-commit");
    assert_eq!(
        before_contracts.as_ref(),
        harness.state.provider_contracts().as_ref()
    );
    drop(conn);
    harness.stop();
}

#[tokio::test]
async fn static_reset_advances_global_revision_before_reload_failure() {
    let harness = start_probes("static-reset-reload-fail").await;
    let now = chrono::Utc::now();
    harness
        .state
        .db
        .lock()
        .set_contract_catalog(
            &ContractScope::provider(KIMI_PROVIDER_ID),
            &["kimi-for-coding".to_string()],
            Some(now),
            "provider_get_models",
            "https://example.test/models",
            now,
        )
        .unwrap();
    harness.state.reload_provider_contracts().unwrap();
    let _docs =
        install_official_protocol_fetch_for_tests(harness.state.process_generation(), |_| {
            OfficialProtocolBaseline::mapped([("grok-4.5", UpstreamProtocolKind::Responses)])
        });
    let before = harness.state.settings_revision();
    let before_contracts = harness.state.provider_contracts();
    let conn = open_sqlite(&harness);
    // Protocol controls now read all evidence inside the reset transaction.
    // Corrupt only after that read, while its final destination catalog writes
    // are running, so the durable commit precedes the reload failure.
    conn.execute_batch(&format!(
        "CREATE TRIGGER corrupt_static_reset_evidence_before_reload
         AFTER INSERT ON destination_models
         WHEN NEW.destination_id = (
             SELECT id FROM destinations
              WHERE legacy_kind = 'builtin' AND legacy_id = '{OPENCODE_PROVIDER_ID}'
         )
         BEGIN
             INSERT OR REPLACE INTO provider_contract_model_protocols
             (scope_kind, scope_id, model_id, protocol, source)
             VALUES ('provider', '{KIMI_PROVIDER_ID}', 'kimi-for-coding',
                     'chat_completions', 'invalid-before-reload');
         END;"
    ))
    .unwrap();

    let (status, body) = send_json(
        &harness,
        Method::POST,
        &static_reset_path(),
        &cas(&harness, json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
    assert_v3_error(&body, ERROR_INTERNAL);
    assert_eq!(harness.state.settings_revision(), before + 1);
    let stored_source: String = conn
        .query_row(
            "SELECT source FROM provider_contract_model_protocols
             WHERE scope_kind = 'provider' AND scope_id = ?1
             LIMIT 1",
            [KIMI_PROVIDER_ID],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(stored_source, "invalid-before-reload");
    let go_override_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM provider_contract_model_protocol_overrides
             WHERE scope_kind = 'provider' AND scope_id = ?1",
            [OPENCODE_PROVIDER_ID],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        go_override_count > 0,
        "the reset transaction must be durable"
    );
    assert_eq!(
        before_contracts.as_ref(),
        harness.state.provider_contracts().as_ref()
    );
    drop(conn);
    harness.stop();
}

#[tokio::test]
async fn static_reset_rolls_back_without_revision_bump_when_evidence_is_invalid() {
    let harness = start_probes("static-reset-invalid-evidence").await;
    let now = chrono::Utc::now();
    harness
        .state
        .db
        .lock()
        .set_contract_catalog(
            &ContractScope::provider(KIMI_PROVIDER_ID),
            &["kimi-for-coding".to_string()],
            Some(now),
            "provider_get_models",
            "https://example.test/models",
            now,
        )
        .unwrap();
    harness.state.reload_provider_contracts().unwrap();
    // Supply valid official docs so the fault occurs while applying controls.
    let _docs =
        install_official_protocol_fetch_for_tests(harness.state.process_generation(), |_| {
            OfficialProtocolBaseline::mapped([("grok-4.5", UpstreamProtocolKind::Responses)])
        });
    let before = harness.state.settings_revision();
    let before_contracts = harness.state.provider_contracts();
    let before_scope = go_scope_revision(&harness);
    assert!(before_scope.is_some());
    let conn = open_sqlite(&harness);
    let before_override_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM provider_contract_model_protocol_overrides
         WHERE scope_kind = 'provider' AND scope_id = ?1",
            [OPENCODE_PROVIDER_ID],
            |row| row.get(0),
        )
        .unwrap();
    conn.execute(
        "INSERT OR REPLACE INTO provider_contract_model_protocols
         (scope_kind, scope_id, model_id, protocol, source)
         VALUES ('provider', ?1, 'kimi-for-coding', 'chat_completions', 'invalid-before-reload')",
        [KIMI_PROVIDER_ID],
    )
    .unwrap();

    let (status, body) = send_json(
        &harness,
        Method::POST,
        &static_reset_path(),
        &cas(&harness, json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
    assert_v3_error(&body, ERROR_INTERNAL);
    assert_eq!(harness.state.settings_revision(), before);
    assert_eq!(go_scope_revision(&harness), before_scope);
    let stored_source: String = conn
        .query_row(
            "SELECT source FROM provider_contract_model_protocols
             WHERE scope_kind = 'provider' AND scope_id = ?1
             LIMIT 1",
            [KIMI_PROVIDER_ID],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(stored_source, "invalid-before-reload");
    let go_override_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM provider_contract_model_protocol_overrides
             WHERE scope_kind = 'provider' AND scope_id = ?1",
            [OPENCODE_PROVIDER_ID],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        go_override_count, before_override_count,
        "the failed reset must roll back its protocol-control writes"
    );
    assert_eq!(
        before_contracts.as_ref(),
        harness.state.provider_contracts().as_ref()
    );
    drop(conn);
    harness.stop();
}

#[tokio::test]
async fn retired_account_owned_probes_do_not_call_upstream() {
    let harness = start_probes("probes-v2-retired").await;
    let origin = start_probe_origin(StatusCode::OK, SUCCESS_BODY, Duration::ZERO).await;
    point_upstream(&harness, &origin.url);
    let account_id = create_go_account(&harness).await;

    harness
        .assert_v2_path_removed(
            Method::POST,
            &format!("/accounts/{account_id}/protocol-probes"),
            Some(json!({
                "model_id": "grok-4.5",
                "protocols": ["chat_completions"]
            })),
        )
        .await;
    assert_eq!(origin.call_count(), 0);

    let (status, custom) = send_json(
        &harness,
        Method::POST,
        "/accounts",
        &cas(
            &harness,
            json!({
                "name": "Custom probe",
                "key": CUSTOM_KEY,
                "providerId": CUSTOM_PROVIDER_ID,
                "customConfig": {
                    "endpointUrl": format!("{}/chat/completions", origin.url.trim_end_matches('/')),
                    "upstreamProtocol": "chat_completions"
                },
                "modelCapabilities": [{
                    "modelId": "org/model",
                    "protocol": "chat_completions"
                }]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{custom}");
    let custom_id = custom["account"]["id"].as_str().unwrap().to_string();
    let initially_enabled = custom["account"]["enabled"].as_bool().unwrap();
    harness
        .assert_v2_path_removed(
            Method::POST,
            &format!("/accounts/{custom_id}/protocol-probes"),
            Some(json!({
                "model_id": "org/model",
                "protocols": ["chat_completions"]
            })),
        )
        .await;
    assert_eq!(origin.call_count(), 0);
    let stored = harness
        .state
        .db
        .lock()
        .get_account(&custom_id)
        .unwrap()
        .unwrap();
    assert_eq!(stored.provider_id, CUSTOM_PROVIDER_ID);
    assert_eq!(stored.enabled, initially_enabled);
    harness.stop();
}

fn seed_zen_official(harness: &V3Harness, pairs: &[(&str, UpstreamProtocolKind)]) {
    let now = chrono::Utc::now();
    let models: Vec<String> = pairs
        .iter()
        .map(|(model, _)| (*model).to_string())
        .collect();
    let scope = ContractScope::provider(OPENCODE_ZEN_FREE_PROVIDER_ID);
    {
        let db = harness.state.db.lock();
        db.set_contract_catalog(
            &scope,
            &models,
            Some(now),
            "official_zen",
            "https://example.test/zen",
            now,
        )
        .unwrap();
        db.apply_official_protocol_baseline(
            &scope,
            &models,
            &OfficialProtocolBaseline::mapped(pairs.iter().map(|(model, protocol)| {
                (model.strip_suffix("-free").unwrap_or(model), *protocol)
            })),
            now,
        )
        .unwrap();
    }
    harness.state.reload_provider_contracts().unwrap();
}

#[tokio::test]
async fn zen_structural_ceiling_rejects_responses_until_official_static_evidence() {
    let harness = start_probes("zen-protocol-ceiling").await;
    let now = chrono::Utc::now();
    let scope = ContractScope::provider(OPENCODE_ZEN_FREE_PROVIDER_ID);
    harness
        .state
        .db
        .lock()
        .set_contract_catalog(
            &scope,
            &["review-model-free".to_string()],
            Some(now),
            "official_zen",
            "https://example.test/zen",
            now,
        )
        .unwrap();
    harness.state.reload_provider_contracts().unwrap();

    let overrides_path = format!(
        "/provider-contracts/provider/{OPENCODE_ZEN_FREE_PROVIDER_ID}/model-protocol-overrides"
    );
    let (status, rejected) = send_json(
        &harness,
        Method::PUT,
        &overrides_path,
        &cas(
            &harness,
            json!({
                "overrides": [{
                    "modelId": "review-model-free",
                    "protocol": "responses",
                    "state": "force_on",
                    "preferred": true
                }]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{rejected}");
    assert_v3_error(&rejected, ERROR_INVALID_REQUEST);

    let (status, unprobeable) = send_json(
        &harness,
        Method::POST,
        &probe_path(OPENCODE_ZEN_FREE_PROVIDER_ID),
        &cas(
            &harness,
            json!({
                "modelId": "review-model-free",
                "protocols": ["responses"]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{unprobeable}");
    assert_v3_error(&unprobeable, ERROR_INVALID_REQUEST);
    harness.stop();
}

#[tokio::test]
async fn zen_official_static_responses_and_messages_are_writable_and_probeable() {
    let harness = start_probes("zen-official-protocols").await;
    seed_zen_official(
        &harness,
        &[
            ("review-model-free", UpstreamProtocolKind::Responses),
            ("messages-model-free", UpstreamProtocolKind::Messages),
        ],
    );
    let origin = start_probe_origin(StatusCode::OK, SUCCESS_BODY, Duration::ZERO).await;
    point_upstream(&harness, &format!("{}/zen/go", origin.url));

    let overrides_path = format!(
        "/provider-contracts/provider/{OPENCODE_ZEN_FREE_PROVIDER_ID}/model-protocol-overrides"
    );
    let (status, responses_saved) = send_json(
        &harness,
        Method::PUT,
        &overrides_path,
        &cas(
            &harness,
            json!({
                "overrides": [
                    {
                        "modelId": "review-model-free",
                        "protocol": "responses",
                        "state": "force_on",
                        "preferred": true
                    },
                    {
                        "modelId": "review-model-free",
                        "protocol": "chat_completions",
                        "state": "force_off"
                    }
                ]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{responses_saved}");
    let (status, messages_saved) = send_json(
        &harness,
        Method::PUT,
        &overrides_path,
        &cas(
            &harness,
            json!({
                "overrides": [
                    {
                        "modelId": "messages-model-free",
                        "protocol": "messages",
                        "state": "force_on",
                        "preferred": true
                    }
                ]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{messages_saved}");

    let zen = harness
        .state
        .provider_contracts()
        .scope(&ContractScope::provider(OPENCODE_ZEN_FREE_PROVIDER_ID))
        .cloned()
        .unwrap();
    let responses = zen.model("review-model-free").unwrap();
    assert!(responses.protocols["responses"].enabled);
    assert_eq!(
        responses.preferred_protocol,
        UpstreamProtocolKind::Responses
    );
    let messages = zen.model("messages-model-free").unwrap();
    assert!(messages.protocols["messages"].enabled);
    assert_eq!(messages.preferred_protocol, UpstreamProtocolKind::Messages);

    let (status, probed_responses) = send_json(
        &harness,
        Method::POST,
        &probe_path(OPENCODE_ZEN_FREE_PROVIDER_ID),
        &cas(
            &harness,
            json!({
                "modelId": "review-model-free",
                "protocols": ["responses"]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{probed_responses}");
    let parsed = parse_probe(&probed_responses);
    assert_eq!(parsed.results.len(), 1);
    assert_eq!(
        parsed.results[0].protocol,
        AccountUpstreamProtocol::Responses
    );
    assert!(parsed.results[0].success);

    let (status, probed_messages) = send_json(
        &harness,
        Method::POST,
        &probe_path(OPENCODE_ZEN_FREE_PROVIDER_ID),
        &cas(
            &harness,
            json!({
                "modelId": "messages-model-free",
                "protocols": ["messages"]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{probed_messages}");
    let parsed = parse_probe(&probed_messages);
    assert_eq!(parsed.results.len(), 1);
    assert_eq!(
        parsed.results[0].protocol,
        AccountUpstreamProtocol::Messages
    );
    assert!(parsed.results[0].success);
    harness.stop();
}

#[tokio::test]
async fn zen_probe_observed_evidence_does_not_expand_sealed_admission() {
    let harness = start_probes("zen-probe-evidence-ceiling").await;
    seed_zen_official(
        &harness,
        &[("review-model-free", UpstreamProtocolKind::Responses)],
    );
    let now = chrono::Utc::now();
    harness
        .state
        .db
        .lock()
        .upsert_model_protocol(&PersistedModelProtocol {
            scope: ContractScope::provider(OPENCODE_ZEN_FREE_PROVIDER_ID),
            model_id: "review-model-free".into(),
            protocol: UpstreamProtocolKind::Messages,
            source: ContractEvidenceSource::ProbeObserved,
            verified_at: Some(now),
            observed_at: Some(now),
            last_probe_result: Some(ProbeResultKind::Success),
            last_probe_at: Some(now),
            last_probe_error: None,
        })
        .unwrap();
    harness.state.reload_provider_contracts().unwrap();

    let overrides_path = format!(
        "/provider-contracts/provider/{OPENCODE_ZEN_FREE_PROVIDER_ID}/model-protocol-overrides"
    );
    let (status, rejected) = send_json(
        &harness,
        Method::PUT,
        &overrides_path,
        &cas(
            &harness,
            json!({
                "overrides": [{
                    "modelId": "review-model-free",
                    "protocol": "messages",
                    "state": "force_on"
                }]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{rejected}");
    assert_v3_error(&rejected, ERROR_INVALID_REQUEST);

    let (status, unprobeable) = send_json(
        &harness,
        Method::POST,
        &probe_path(OPENCODE_ZEN_FREE_PROVIDER_ID),
        &cas(
            &harness,
            json!({
                "modelId": "review-model-free",
                "protocols": ["messages"]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{unprobeable}");
    assert_v3_error(&unprobeable, ERROR_INVALID_REQUEST);
    let model = harness
        .state
        .provider_contracts()
        .scope(&ContractScope::provider(OPENCODE_ZEN_FREE_PROVIDER_ID))
        .and_then(|contract| contract.model("review-model-free").cloned())
        .unwrap();
    assert!(model.protocols.contains_key("responses"));
    assert!(!model.protocols.contains_key("messages"));
    harness.stop();
}

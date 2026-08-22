//! Dashboard V3 provider protocol probes: auth, CAS, zero-call gates, shared
//! transport, persistence, and V2 coexistence.

use axum::Router;
use axum::body::Bytes;
use axum::extract::OriginalUri;
use axum::http::{HeaderMap, Method as HttpMethod};
use axum::routing::any;
use ocg_core::dashboard_v3::{
    ERROR_INTERNAL, ERROR_INVALID_JSON, ERROR_INVALID_REQUEST, ERROR_MISSING_EXPECTED_REVISION,
    ERROR_NOT_FOUND, ERROR_NOT_IMPLEMENTED, ERROR_REVISION_CONFLICT, ERROR_UNAUTHORIZED,
    ProtocolProbeResponse,
};
use ocg_core::db::CURRENT_SCHEMA_VERSION;
use ocg_core::models::{ProxyListDirection, ProxyMode};
use ocg_core::provider::{
    COMMAND_CODE_PROVIDER_ID, CUSTOM_API_OFFERING_ID, CUSTOM_PROVIDER_ID, OPENCODE_PROVIDER_ID,
    OPENCODE_ZEN_FREE_PROVIDER_ID, SCNET_PROVIDER_ID, UpstreamProtocolKind, ZEN_FREE_ACCOUNT_ID,
};
use ocg_core::provider_contracts::{ContractScope, PersistedModelProtocol, ProbeResultKind};
use reqwest::{Method, StatusCode};
use serde_json::{Map, Value, json};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[path = "fixtures/dashboard_v3/harness.rs"]
mod harness;

use harness::{V3Harness, start_loopback, start_public};

const GO_KEY: &str = "sk-probe-secret-key";
const CUSTOM_KEY: &str = "custom-x-api-key";
const SUCCESS_BODY: &str = r#"{"id":"ok","object":"json"}"#;

#[derive(Clone, Debug)]
struct CapturedProbe {
    method: String,
    path: String,
    authorization: Option<String>,
    x_api_key: Option<String>,
    cookie: Option<String>,
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
            let body = body.clone();
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
    let (status, created) = send_json(
        harness,
        Method::POST,
        "/accounts",
        &cas(harness, json!({ "name": "Go probe", "key": GO_KEY })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    created["account"]["id"]
        .as_str()
        .expect("created Go account id")
        .to_string()
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

#[test]
fn dashboard_v3_schema_version_stays_at_v26() {
    assert_eq!(CURRENT_SCHEMA_VERSION, 26);
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
async fn protocol_probes_require_cas_and_reject_stale_tokens_with_zero_upstream() {
    let harness = start_loopback("probes-cas").await;
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
    let harness = start_loopback("probes-zero-call").await;
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
    assert!(duplicate["message"].as_str().unwrap().contains("duplicate"));

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
    assert!(
        custom["message"]
            .as_str()
            .unwrap()
            .to_ascii_lowercase()
            .contains("account-owned")
    );

    let (status, goat) = send_json(
        &harness,
        Method::POST,
        &probe_path(COMMAND_CODE_PROVIDER_ID),
        &cas(
            &harness,
            json!({
                "modelId": "deepseek/deepseek-v4-flash",
                "protocols": ["chat_completions"]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_IMPLEMENTED, "{goat}");
    assert_v3_error(&goat, ERROR_NOT_IMPLEMENTED);

    let (status, scnet) = send_json(
        &harness,
        Method::POST,
        &probe_path(SCNET_PROVIDER_ID),
        &cas(
            &harness,
            json!({
                "modelId": "DeepSeek-V3",
                "protocols": ["chat_completions"]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_IMPLEMENTED, "{scnet}");
    assert_v3_error(&scnet, ERROR_NOT_IMPLEMENTED);

    let (status, missing_account) = send_json(
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
    assert_eq!(status, StatusCode::BAD_REQUEST, "{missing_account}");
    assert_v3_error(&missing_account, ERROR_INVALID_REQUEST);

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

    let (status, unknown_account) = send_json(
        &harness,
        Method::POST,
        &probe_path(OPENCODE_PROVIDER_ID),
        &cas(
            &harness,
            json!({
                "accountId": "missing-account",
                "modelId": "grok-4.5",
                "protocols": ["responses"]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{unknown_account}");
    assert_v3_error(&unknown_account, ERROR_NOT_FOUND);

    assert_eq!(origin.call_count(), 0);
    assert_eq!(harness.state.settings_revision(), before);
    harness.stop();
}

#[tokio::test]
async fn go_protocol_probes_send_one_admin_post_per_protocol_with_correct_path_and_auth() {
    let harness = start_loopback("probes-go-n").await;
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
                "modelId": "grok-4.5",
                "protocols": ["chat_completions", "responses", "messages"]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let parsed = parse_probe(&body);
    assert_eq!(parsed.account_id, account_id);
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
async fn zen_protocol_probe_omits_auth_and_rejects_the_wrong_singleton_id() {
    let harness = start_loopback("probes-zen").await;
    let origin = start_probe_origin(StatusCode::OK, SUCCESS_BODY, Duration::ZERO).await;
    point_upstream(&harness, &format!("{}/zen/go", origin.url));
    let before = harness.state.settings_revision();

    let (status, wrong) = send_json(
        &harness,
        Method::POST,
        &probe_path(OPENCODE_ZEN_FREE_PROVIDER_ID),
        &cas(
            &harness,
            json!({
                "accountId": "not-zen",
                "modelId": "hy3-free",
                "protocols": ["chat_completions"]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{wrong}");
    assert_v3_error(&wrong, ERROR_INVALID_REQUEST);
    assert_eq!(origin.call_count(), 0);
    assert_eq!(harness.state.settings_revision(), before);

    let (status, body) = send_json(
        &harness,
        Method::POST,
        &probe_path(OPENCODE_ZEN_FREE_PROVIDER_ID),
        &cas(
            &harness,
            json!({
                "modelId": "hy3-free",
                "protocols": ["chat_completions"]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let parsed = parse_probe(&body);
    assert_eq!(parsed.account_id, ZEN_FREE_ACCOUNT_ID);
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
async fn unknown_model_ceiling_skip_returns_200_without_bump_or_upstream() {
    let harness = start_loopback("probes-ceiling").await;
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
    assert_eq!(status, StatusCode::OK, "{body}");
    let parsed = parse_probe(&body);
    assert_eq!(parsed.results.len(), 2);
    assert!(
        parsed
            .results
            .iter()
            .all(|result| result.skipped && !result.success)
    );
    assert!(parsed.contract.is_none());
    assert_eq!(parsed.revision, before);
    assert_eq!(harness.state.settings_revision(), before);
    assert_eq!(origin.call_count(), 0);
    harness.stop();
}

#[tokio::test]
async fn transport_failure_returns_200_persists_observation_and_redacts_secrets() {
    let harness = start_loopback("probes-failure").await;
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
    harness.stop();
}

#[tokio::test]
async fn successful_probe_adds_contract_and_does_not_forward_dashboard_headers() {
    let harness = start_loopback("probes-success-headers").await;
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
    let contract = parsed.contract.expect("success should add a contract");
    assert!(
        contract
            .protocols
            .chat_completions
            .as_ref()
            .is_some_and(|row| row.available)
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
    let harness = start_loopback("probes-proxy-leg").await;
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
async fn cas_change_during_outbound_still_persists_and_bumps_once() {
    let harness = start_loopback("probes-cas-during").await;
    let origin = start_probe_origin(StatusCode::OK, SUCCESS_BODY, Duration::from_millis(400)).await;
    point_upstream(&harness, &origin.url);
    let account_id = create_go_account(&harness).await;
    let before = harness.state.settings_revision();
    let generation = harness.state.process_generation();
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
    assert_eq!(status, StatusCode::OK, "{body}");
    let parsed = parse_probe(&body);
    assert!(parsed.results[0].success);
    assert_eq!(parsed.revision, mid + 1);
    assert_eq!(parsed.process_generation, generation);
    assert_eq!(harness.state.settings_revision(), mid + 1);
    assert_eq!(origin.call_count(), 1);
    harness.stop();
}

#[tokio::test]
async fn two_protocol_success_stores_both_rows_and_bumps_nested_scope_once() {
    let harness = start_loopback("probes-batch-success").await;
    let origin = start_probe_origin(StatusCode::OK, SUCCESS_BODY, Duration::ZERO).await;
    point_upstream(&harness, &origin.url);
    let account_id = create_go_account(&harness).await;
    let before = harness.state.settings_revision();
    assert_eq!(go_scope_revision(&harness), None);

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
    assert_eq!(go_scope_revision(&harness), Some(2));
    assert!(load_go_evidence(&harness, UpstreamProtocolKind::ChatCompletions).is_some());
    assert!(load_go_evidence(&harness, UpstreamProtocolKind::Responses).is_some());
    let stored = harness
        .state
        .provider_contracts()
        .scope(&go_scope())
        .and_then(|scope| scope.model("grok-4.5").cloned())
        .unwrap();
    assert!(stored.protocols["chat_completions"].available);
    assert!(stored.protocols["responses"].available);
    assert_eq!(origin.call_count(), 2);
    harness.stop();
}

#[tokio::test]
async fn two_protocol_batch_rolls_back_when_second_observation_write_fails() {
    let harness = start_loopback("probes-batch-fault").await;
    let origin = start_probe_origin(StatusCode::OK, SUCCESS_BODY, Duration::ZERO).await;
    point_upstream(&harness, &origin.url);
    let account_id = create_go_account(&harness).await;
    let before = harness.state.settings_revision();
    let before_contracts = harness.state.provider_contracts();
    assert_eq!(go_scope_revision(&harness), None);

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
    assert_eq!(go_scope_revision(&harness), None);
    assert!(load_go_evidence(&harness, UpstreamProtocolKind::ChatCompletions).is_none());
    assert!(load_go_evidence(&harness, UpstreamProtocolKind::Responses).is_none());
    assert_eq!(
        before_contracts.as_ref(),
        harness.state.provider_contracts().as_ref()
    );
    drop(conn);
    harness.stop();
}

#[tokio::test]
async fn probe_commit_advances_global_revision_before_reload_failure() {
    let harness = start_loopback("probes-reload-fail").await;
    let origin = start_probe_origin(StatusCode::OK, SUCCESS_BODY, Duration::ZERO).await;
    point_upstream(&harness, &origin.url);
    let account_id = create_go_account(&harness).await;
    let before = harness.state.settings_revision();
    let before_contracts = harness.state.provider_contracts();
    assert_eq!(go_scope_revision(&harness), None);

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
    assert_eq!(go_scope_revision(&harness), Some(2));
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
async fn v2_duplicate_custom_and_ceiling_probes_coexist() {
    let harness = start_loopback("probes-v2-coexist").await;
    let origin = start_probe_origin(StatusCode::OK, SUCCESS_BODY, Duration::ZERO).await;
    point_upstream(&harness, &origin.url);
    let account_id = create_go_account(&harness).await;

    let duplicate = harness
        .client
        .post(format!(
            "{}/accounts/{account_id}/protocol-probes",
            harness.v2_base
        ))
        .json(&json!({
            "model_id": "grok-4.5",
            "protocols": ["chat_completions", "responses", "chat_completions"]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(duplicate.status(), StatusCode::BAD_REQUEST);
    let duplicate_body: Value = duplicate.json().await.unwrap();
    assert!(
        duplicate_body.to_string().contains("duplicate"),
        "{duplicate_body}"
    );
    assert_eq!(origin.call_count(), 0);

    let ceiling = harness
        .client
        .post(format!(
            "{}/accounts/{account_id}/protocol-probes",
            harness.v2_base
        ))
        .json(&json!({
            "model_id": "not-a-known-model",
            "protocols": ["chat_completions"]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(ceiling.status(), StatusCode::OK);
    let ceiling_body: Value = ceiling.json().await.unwrap();
    assert_eq!(ceiling_body["results"][0]["skipped"], true);
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
                "offeringId": CUSTOM_API_OFFERING_ID,
                "customConfig": {
                    "baseUrl": origin.url,
                    "upstreamProtocol": "chat_completions",
                    "authScheme": "x-api-key"
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
    let custom_probe = harness
        .client
        .post(format!(
            "{}/accounts/{custom_id}/protocol-probes",
            harness.v2_base
        ))
        .json(&json!({
            "model_id": "org/model",
            "protocols": ["chat_completions"]
        }))
        .send()
        .await
        .unwrap();
    let custom_status = custom_probe.status();
    let custom_body: Value = custom_probe.json().await.unwrap();
    assert_eq!(custom_status, StatusCode::OK, "{custom_body}");
    assert_eq!(custom_body["results"][0]["success"], true);
    let call = origin.calls.lock().unwrap().last().cloned().unwrap();
    assert_eq!(call.x_api_key.as_deref(), Some(CUSTOM_KEY));
    assert!(call.authorization.is_none(), "{call:?}");
    assert_eq!(CURRENT_SCHEMA_VERSION, 26);
    harness.stop();
}

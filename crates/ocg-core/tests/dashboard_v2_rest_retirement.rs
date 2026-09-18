//! Dashboard V2 REST retirement: authenticated 410 tombstone, 401 before
//! tombstone, and excluded route families.

use ocg_core::crypto::{KeyCipher, StaticKeyCipher};
use ocg_core::db::Database;
use ocg_core::gateway;
use ocg_core::host_router::{
    DASHBOARD_V2_REMOVED_CODE, DASHBOARD_V2_REMOVED_MESSAGE, DASHBOARD_V3_REMOVED_CODE,
    DASHBOARD_V3_REMOVED_MESSAGE,
};
use ocg_core::state::CoreStateInner;
use reqwest::StatusCode;
use serde_json::{Value, json};
use std::fs;
use std::net::SocketAddr;
use std::sync::Arc;

fn temp_state(label: &str) -> Arc<CoreStateInner> {
    let mut dir = std::env::temp_dir();
    dir.push(format!(
        "ocg-v2-rest-retirement-{label}-{}",
        uuid::Uuid::new_v4()
    ));
    fs::create_dir_all(&dir).unwrap();
    let db = Database::open(dir.clone()).unwrap();
    let cipher: Arc<dyn KeyCipher + Send + Sync> = Arc::new(StaticKeyCipher::new("v2-retirement"));
    Arc::new(CoreStateInner::new(db, dir, cipher).unwrap())
}

fn loopback_client() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .build()
        .expect("retirement test client should build")
}

fn v2(port: u16, path: &str) -> String {
    format!("http://127.0.0.1:{port}/dashboard/api{path}")
}

fn tombstone_body(body: &Value) {
    assert_eq!(
        body,
        &json!({
            "code": DASHBOARD_V2_REMOVED_CODE,
            "message": DASHBOARD_V2_REMOVED_MESSAGE })
    );
}

fn v3_tombstone_body(body: &Value) {
    assert_eq!(
        body,
        &json!({
            "code": DASHBOARD_V3_REMOVED_CODE,
            "message": DASHBOARD_V3_REMOVED_MESSAGE })
    );
}

async fn json_status(
    client: &reqwest::Client,
    method: reqwest::Method,
    url: String,
    body: Option<Value>,
) -> (StatusCode, Value, String) {
    let mut request = client.request(method, url);
    if let Some(body) = body {
        request = request.json(&body);
    }
    let response = request.send().await.unwrap();
    let status = response.status();
    let text = response.text().await.unwrap_or_default();
    let json = serde_json::from_str(&text).unwrap_or(Value::Null);
    (status, json, text)
}

async fn start_session_protected(state: Arc<CoreStateInner>) -> ocg_core::state::GatewayHandle {
    #[cfg(windows)]
    let addr = SocketAddr::from(([127, 0, 0, 1], 0));
    #[cfg(not(windows))]
    let addr = SocketAddr::from(([0, 0, 0, 0], 0));

    let handle = gateway::start_gateway_on(state.clone(), addr)
        .await
        .unwrap();
    #[cfg(windows)]
    state.set_dashboard_local_mode(false);
    handle
}

#[tokio::test]
async fn authenticated_legacy_get_post_and_unknown_rest_return_exact_410() {
    let state = temp_state("loopback-410");
    let handle = gateway::start_gateway_on(state, SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .unwrap();
    let client = loopback_client();
    let port = handle.port;

    for (method, path, body) in [
        (reqwest::Method::GET, "/settings", None),
        (
            reqwest::Method::POST,
            "/accounts",
            Some(json!({ "name": "retired", "key": "sk-retired" })),
        ),
        (reqwest::Method::GET, "/does-not-exist", None),
        (reqwest::Method::POST, "/not-a-v2-route", Some(json!({}))),
        (reqwest::Method::PUT, "/providers/catalog", Some(json!({}))),
    ] {
        let (status, json, text) = json_status(&client, method.clone(), v2(port, path), body).await;
        assert_eq!(
            status,
            StatusCode::GONE,
            "{method} {path} should be the V2 tombstone, got {status} {text}"
        );
        tombstone_body(&json);
    }

    gateway::stop_gateway(handle);
}

#[tokio::test]
async fn anonymous_legacy_rest_is_401_before_the_tombstone() {
    let state = temp_state("public-401");
    let handle = start_session_protected(state).await;
    let client = loopback_client();
    let port = handle.port;

    for (method, path, body) in [
        (reqwest::Method::GET, "/settings", None),
        (
            reqwest::Method::POST,
            "/accounts",
            Some(json!({ "name": "retired", "key": "sk-retired" })),
        ),
        (reqwest::Method::GET, "/does-not-exist", None),
        (reqwest::Method::GET, "/auth/status/", None),
        (reqwest::Method::POST, "/auth/register/", Some(json!({}))),
        (reqwest::Method::POST, "/auth/login/", Some(json!({}))),
        (reqwest::Method::POST, "/auth/logout/", None),
        (
            reqwest::Method::GET,
            "/browser/sessions/opaque-token/ws/",
            None,
        ),
    ] {
        let (status, json, text) = json_status(&client, method.clone(), v2(port, path), body).await;
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "{method} {path} must 401 before the tombstone, got {status} {text}"
        );
        assert!(
            text.is_empty(),
            "anonymous V2 401 must stay an empty body, got {text}"
        );
        assert_eq!(json, Value::Null);
        assert_ne!(json["code"], DASHBOARD_V2_REMOVED_CODE);
    }

    gateway::stop_gateway(handle);
}

#[tokio::test]
async fn v2_auth_endpoints_remain_available() {
    let state = temp_state("auth-live");
    let handle = start_session_protected(state).await;
    let client = loopback_client();
    let port = handle.port;

    let (status, body, text) = json_status(
        &client,
        reqwest::Method::GET,
        v2(port, "/auth/status"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert_eq!(body["local"], false);
    assert_eq!(body["initialized"], false);
    assert_eq!(body["authenticated"], false);
    assert_ne!(body["code"], DASHBOARD_V2_REMOVED_CODE);

    let register = client
        .post(v2(port, "/auth/register"))
        .json(&json!({ "username": "admin", "password": "password123" }))
        .send()
        .await
        .unwrap();
    assert_eq!(register.status(), StatusCode::CREATED);
    let cookie = register
        .headers()
        .get(reqwest::header::SET_COOKIE)
        .unwrap()
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();
    let register_body: Value = register.json().await.unwrap();
    assert_eq!(register_body, json!({ "ok": true }));

    let (status, json, text) =
        json_status(&client, reqwest::Method::GET, v2(port, "/settings"), None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{text}");
    assert_ne!(json["code"], DASHBOARD_V2_REMOVED_CODE);

    let authorized = client
        .get(v2(port, "/settings"))
        .header(reqwest::header::COOKIE, &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(authorized.status(), StatusCode::GONE);
    tombstone_body(&authorized.json().await.unwrap());

    let logout = client
        .post(v2(port, "/auth/logout"))
        .header(reqwest::header::COOKIE, &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(logout.status(), StatusCode::NO_CONTENT);
    let login = client
        .post(v2(port, "/auth/login"))
        .json(&json!({ "username": "admin", "password": "password123" }))
        .send()
        .await
        .unwrap();
    assert_eq!(login.status(), StatusCode::OK);

    gateway::stop_gateway(handle);
}

#[tokio::test]
async fn browser_websocket_keeps_independent_error_shape() {
    let state = temp_state("browser-ws");
    let handle = gateway::start_gateway_on(state, SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .unwrap();
    let client = loopback_client();
    let port = handle.port;
    let url = v2(port, "/browser/sessions/opaque-token/ws");

    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(reqwest::header::CONNECTION, "Upgrade".parse().unwrap());
    headers.insert(reqwest::header::UPGRADE, "websocket".parse().unwrap());
    headers.insert("Sec-WebSocket-Version", "13".parse().unwrap());
    headers.insert(
        "Sec-WebSocket-Key",
        "dGhlIHNhbXBsZSBub25jZQ==".parse().unwrap(),
    );
    let missing_origin = client
        .get(&url)
        .headers(headers.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(missing_origin.status(), StatusCode::BAD_REQUEST);
    let body: Value = missing_origin.json().await.unwrap();
    assert_eq!(body["error"], "browser WebSocket Origin is required");
    assert_ne!(body["code"], DASHBOARD_V2_REMOVED_CODE);

    let public = start_session_protected(temp_state("browser-ws-anon")).await;
    let anon = client
        .get(v2(public.port, "/browser/sessions/opaque-token/ws"))
        .send()
        .await
        .unwrap();
    assert_eq!(anon.status(), StatusCode::UNAUTHORIZED);
    let anon_text = anon.text().await.unwrap();
    assert!(
        anon_text.is_empty(),
        "anonymous browser WS must stay the V2 empty 401, got {anon_text}"
    );

    gateway::stop_gateway(handle);
    gateway::stop_gateway(public);
}

#[tokio::test]
async fn static_inference_and_v4_families_are_not_captured() {
    let state = temp_state("excluded");
    let handle = gateway::start_gateway_on(state, SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .unwrap();
    let client = loopback_client();
    let port = handle.port;
    let root = format!("http://127.0.0.1:{port}");

    for path in [
        "/dashboard/api/v3",
        "/dashboard/api/v3/contract",
        "/dashboard/api/v3/accounts",
        "/dashboard/api/v3/settings",
        "/dashboard/api/v3/auth/status",
        "/dashboard/api/v3/browser/sessions/opaque-token/ws",
    ] {
        let (status, body, text) =
            json_status(&client, reqwest::Method::GET, format!("{root}{path}"), None).await;
        assert_eq!(
            status,
            StatusCode::GONE,
            "{path} should be the V3 tombstone, got {status} {text}"
        );
        v3_tombstone_body(&body);
        assert_ne!(body["code"], DASHBOARD_V2_REMOVED_CODE);
    }

    let (status, body, text) = json_status(
        &client,
        reqwest::Method::GET,
        format!("{root}/dashboard/api/v4/contract"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert!(body.get("revision").is_some(), "{body}");
    assert_ne!(body["code"], DASHBOARD_V2_REMOVED_CODE);
    assert_ne!(body["code"], DASHBOARD_V3_REMOVED_CODE);

    let v4_accounts = client
        .get(format!("{root}/dashboard/api/v4/account-records"))
        .send()
        .await
        .unwrap();
    assert_eq!(v4_accounts.status(), StatusCode::OK);
    let v4_accounts: Value = v4_accounts.json().await.unwrap();
    assert_ne!(v4_accounts["code"], DASHBOARD_V2_REMOVED_CODE);
    assert_ne!(v4_accounts["code"], DASHBOARD_V3_REMOVED_CODE);

    for path in [
        "/v1/models",
        "/v1/chat/completions",
        "/v1beta/models/minimax-m3:generateContent",
    ] {
        let response = client.get(format!("{root}{path}")).send().await.unwrap();
        assert_ne!(
            response.status(),
            StatusCode::GONE,
            "{path} must not be captured by the V2 REST tombstone"
        );
        let text = response.text().await.unwrap_or_default();
        assert!(
            !text.contains(DASHBOARD_V2_REMOVED_CODE),
            "{path} leaked the V2 tombstone: {text}"
        );
        assert!(
            !text.contains(DASHBOARD_V3_REMOVED_CODE),
            "{path} leaked the V3 tombstone: {text}"
        );
    }

    for path in ["/dashboard", "/dashboard/", "/dashboard/assets/app.js"] {
        let response = client.get(format!("{root}{path}")).send().await.unwrap();
        assert_ne!(
            response.status(),
            StatusCode::GONE,
            "{path} must stay a dashboard asset/index route"
        );
        let text = response.text().await.unwrap_or_default();
        assert!(
            !text.contains(DASHBOARD_V2_REMOVED_CODE),
            "{path} leaked the V2 tombstone: {text}"
        );
        assert!(
            !text.contains(DASHBOARD_V3_REMOVED_CODE),
            "{path} leaked the V3 tombstone: {text}"
        );
    }

    gateway::stop_gateway(handle);
}

#[tokio::test]
async fn anonymous_v3_rest_is_401_before_the_tombstone() {
    let state = temp_state("v3-public-401");
    let handle = start_session_protected(state).await;
    let client = loopback_client();
    let port = handle.port;
    let root = format!("http://127.0.0.1:{port}");

    for path in [
        "/dashboard/api/v3",
        "/dashboard/api/v3/contract",
        "/dashboard/api/v3/accounts",
        "/dashboard/api/v3/settings",
    ] {
        let (status, json, text) =
            json_status(&client, reqwest::Method::GET, format!("{root}{path}"), None).await;
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "{path} must 401 before the V3 tombstone, got {status} {text}"
        );
        assert!(
            text.is_empty(),
            "anonymous V3 401 must stay an empty body, got {text}"
        );
        assert_eq!(json, Value::Null);
        assert_ne!(json["code"], DASHBOARD_V3_REMOVED_CODE);
        assert_ne!(json["code"], DASHBOARD_V2_REMOVED_CODE);
    }

    gateway::stop_gateway(handle);
}

#[tokio::test]
async fn similar_looking_paths_cannot_bypass_the_tombstone() {
    let state = temp_state("lookalike-410");
    let handle = gateway::start_gateway_on(state, SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .unwrap();
    let client = loopback_client();
    let port = handle.port;

    for (method, path) in [
        (reqwest::Method::GET, "/auth"),
        (reqwest::Method::GET, "/auth/"),
        (reqwest::Method::GET, "/auth/status/"),
        (reqwest::Method::GET, "/auth/status//"),
        (reqwest::Method::GET, "/auth//status"),
        (reqwest::Method::GET, "/auth/status/extra"),
        (reqwest::Method::POST, "/auth/register/extra"),
        (reqwest::Method::POST, "/auth/login/now"),
        (reqwest::Method::POST, "/auth/logout/now"),
        (reqwest::Method::GET, "/auth/statusx"),
        (reqwest::Method::GET, "/authentication/status"),
        (reqwest::Method::GET, "/v2/auth/status"),
        (reqwest::Method::GET, "/browser/sessions//ws"),
        (reqwest::Method::GET, "/browser/sessions/tok"),
        (reqwest::Method::GET, "/browser/sessions/tok/websocket"),
        (reqwest::Method::GET, "/browser/sessions/tok/ws/extra"),
        (reqwest::Method::GET, "/browser/sessions/tok/ws/"),
        (reqwest::Method::GET, "/browser/sessions/tok//ws"),
        (reqwest::Method::GET, "/browser/session/tok/ws"),
        (reqwest::Method::GET, "/auth%2Fstatus"),
        (reqwest::Method::GET, "/auth/%73tatus"),
        (reqwest::Method::GET, "/browser/sessions/tok/%77s"),
        (reqwest::Method::GET, "/v3accounts"),
        (reqwest::Method::GET, "/V3/accounts"),
        (reqwest::Method::GET, "/v3-contract"),
    ] {
        let (status, json, text) = json_status(&client, method.clone(), v2(port, path), None).await;
        assert_eq!(
            status,
            StatusCode::GONE,
            "{method} {path} must stay the V2 tombstone, got {status} {text}"
        );
        tombstone_body(&json);
    }

    gateway::stop_gateway(handle);
}

#[tokio::test]
async fn only_exact_auth_and_nonempty_browser_ws_are_preserved() {
    let state = temp_state("exact-preserved");
    let handle = gateway::start_gateway_on(state, SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .unwrap();
    let client = loopback_client();
    let port = handle.port;

    let (status, body, text) = json_status(
        &client,
        reqwest::Method::GET,
        v2(port, "/auth/status"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert_eq!(body["local"], true);
    assert_ne!(body["code"], DASHBOARD_V2_REMOVED_CODE);

    for path in [
        "/auth/status/",
        "/auth/logout/",
        "/auth/status/extra",
        "/browser/sessions//ws",
        "/browser/sessions/opaque-token/ws/",
        "/browser/sessions/tok/ws/extra",
    ] {
        let (status, json, text) =
            json_status(&client, reqwest::Method::GET, v2(port, path), None).await;
        assert_eq!(
            status,
            StatusCode::GONE,
            "{path} must not inherit the preserved-family exemption, got {status} {text}"
        );
        tombstone_body(&json);
    }

    gateway::stop_gateway(handle);
}

#[tokio::test]
async fn exact_preserved_paths_keep_method_semantics_without_mutation() {
    let state = temp_state("exact-methods");
    let handle = gateway::start_gateway_on(state.clone(), SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .unwrap();
    let client = loopback_client();
    let port = handle.port;

    let before_status = client
        .get(v2(port, "/auth/status"))
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();
    assert_eq!(before_status["initialized"], false);

    for (method, path) in [
        (reqwest::Method::POST, "/auth/status"),
        (reqwest::Method::GET, "/auth/register"),
        (reqwest::Method::GET, "/auth/login"),
        (reqwest::Method::GET, "/auth/logout"),
        (reqwest::Method::POST, "/browser/sessions/opaque-token/ws"),
    ] {
        let response = client
            .request(method.clone(), v2(port, path))
            .json(&json!({ "username": "admin", "password": "password123" }))
            .send()
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::METHOD_NOT_ALLOWED,
            "{method} {path} must retain its own method contract"
        );
        assert!(
            !response
                .text()
                .await
                .unwrap()
                .contains(DASHBOARD_V2_REMOVED_CODE)
        );
    }

    let after_status = client
        .get(v2(port, "/auth/status"))
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();
    assert_eq!(after_status["initialized"], false);

    let revision_before = state.settings_revision();
    let gateway_key_before = state.config().gateway_key;
    let account_ids_before = state
        .db
        .lock()
        .list_accounts()
        .unwrap()
        .into_iter()
        .map(|account| account.id)
        .collect::<Vec<_>>();
    for (method, path, body) in [
        (
            reqwest::Method::POST,
            "/settings",
            json!({ "gateway_key": "ocg-mutation-bypass" }),
        ),
        (
            reqwest::Method::POST,
            "/settings/regenerate-gateway-key",
            json!({}),
        ),
        (
            reqwest::Method::POST,
            "/accounts",
            json!({ "name": "bypass", "key": "sk-bypass" }),
        ),
        (
            reqwest::Method::PUT,
            "/accounts/nonexistent",
            json!({ "name": "bypass" }),
        ),
        (
            reqwest::Method::PATCH,
            "/accounts/nonexistent/setup",
            json!({ "step": "ready" }),
        ),
        (reqwest::Method::DELETE, "/accounts/nonexistent", json!({})),
    ] {
        let (status, json, text) =
            json_status(&client, method.clone(), v2(port, path), Some(body)).await;
        assert_eq!(status, StatusCode::GONE, "{method} {path}: {text}");
        tombstone_body(&json);
    }
    assert_eq!(state.settings_revision(), revision_before);
    assert_eq!(state.config().gateway_key, gateway_key_before);
    let account_ids_after = state
        .db
        .lock()
        .list_accounts()
        .unwrap()
        .into_iter()
        .map(|account| account.id)
        .collect::<Vec<_>>();
    assert_eq!(account_ids_after, account_ids_before);

    gateway::stop_gateway(handle);
}

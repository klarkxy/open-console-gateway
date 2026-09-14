use ocg_core::crypto::{KeyCipher, StaticKeyCipher};
use ocg_core::db::Database;
use ocg_core::gateway;
use ocg_core::host_router::{DASHBOARD_V2_REMOVED_CODE, DASHBOARD_V2_REMOVED_MESSAGE};
use ocg_core::models::RoutingMode;
use ocg_core::provider::ZEN_FREE_ACCOUNT_ID;
use ocg_core::state::CoreStateInner;
use reqwest::StatusCode;
use serde_json::json;
use std::fs;
use std::net::SocketAddr;
use std::sync::Arc;

fn state(label: &str) -> Arc<CoreStateInner> {
    let mut dir = std::env::temp_dir();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    dir.push(format!("ocg-auth-test-{label}-{nanos}"));
    fs::create_dir_all(&dir).unwrap();
    let db = Database::open(dir.clone()).unwrap();
    let cipher: Arc<dyn KeyCipher + Send + Sync> = Arc::new(StaticKeyCipher::new("test"));
    Arc::new(CoreStateInner::new(db, dir, cipher).unwrap())
}

/// Every request in this suite targets loopback listeners; never route them
/// through an ambient system/environment proxy (which aborts such
/// connections on some machines).
fn loopback_client() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .build()
        .expect("test client should build")
}

fn v3_url(port: u16, path: &str) -> String {
    format!("http://127.0.0.1:{port}/dashboard/api/v3{path}")
}

fn cas(state: &CoreStateInner, extra: serde_json::Value) -> serde_json::Value {
    let mut body = extra.as_object().cloned().unwrap_or_default();
    body.insert("expectedRevision".into(), json!(state.settings_revision()));
    body.insert(
        "processGeneration".into(),
        json!(state.process_generation()),
    );
    serde_json::Value::Object(body)
}

async fn assert_v2_removed(response: reqwest::Response) {
    assert_eq!(response.status(), StatusCode::GONE);
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(
        body,
        json!({
            "code": DASHBOARD_V2_REMOVED_CODE,
            "message": DASHBOARD_V2_REMOVED_MESSAGE })
    );
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
async fn public_dashboard_uses_first_registration_and_session_cookie() {
    let state = state("public");
    let handle = start_session_protected(state.clone()).await;
    let base = format!("http://127.0.0.1:{}/dashboard/api", handle.port);
    let v3 = format!("{base}/v3");
    let client = loopback_client();

    let status = client
        .get(format!("{base}/auth/status"))
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    assert_eq!(status["local"], false);
    assert_eq!(status["initialized"], false);
    assert_eq!(status["authenticated"], false);

    let response = client
        .post(format!("{base}/auth/register"))
        .json(&json!({ "username": "admin", "password": "password123" }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let cookie = response
        .headers()
        .get(reqwest::header::SET_COOKIE)
        .unwrap()
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();

    assert_eq!(
        client
            .get(format!("{v3}/settings"))
            .header(reqwest::header::COOKIE, &cookie)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    assert_v2_removed(
        client
            .get(format!("{base}/settings"))
            .header(reqwest::header::COOKIE, &cookie)
            .send()
            .await
            .unwrap(),
    )
    .await;
    let reordered = client
        .put(format!("{v3}/accounts/order"))
        .header(reqwest::header::COOKIE, &cookie)
        .json(&cas(&state, json!({ "accountIds": [ZEN_FREE_ACCOUNT_ID] })))
        .send()
        .await
        .unwrap();
    assert_eq!(reordered.status(), StatusCode::OK);
    let reordered = reordered.json::<serde_json::Value>().await.unwrap();
    let reordered_ids = reordered["accounts"]
        .as_array()
        .expect("reorder response should be an account list")
        .iter()
        .map(|account| {
            account["id"]
                .as_str()
                .expect("account id should be a string")
        })
        .collect::<Vec<_>>();
    assert_eq!(reordered_ids, [ZEN_FREE_ACCOUNT_ID]);
    let application_models = client
        .get(format!("{v3}/application-models"))
        .header(reqwest::header::COOKIE, &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(application_models.status(), StatusCode::OK);
    assert!(
        application_models
            .json::<serde_json::Value>()
            .await
            .unwrap()
            .get("models")
            .and_then(serde_json::Value::as_array)
            .is_some(),
        "authenticated application-models must return a local models array"
    );

    assert_eq!(
        client
            .post(format!("{base}/auth/login"))
            .json(&json!({ "username": "admin", "password": "wrong-password" }))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        client
            .post(format!("{base}/auth/register"))
            .json(&json!({ "username": "other", "password": "password456" }))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::CONFLICT
    );

    assert_eq!(
        client
            .post(format!("{base}/auth/logout"))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        client
            .post(format!("{base}/auth/logout"))
            .header(reqwest::header::COOKIE, &cookie)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        client
            .get(format!("{base}/settings"))
            .header(reqwest::header::COOKIE, &cookie)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );

    let login = client
        .post(format!("{base}/auth/login"))
        .json(&json!({ "username": "admin", "password": "password123" }))
        .send()
        .await
        .unwrap();
    assert_eq!(login.status(), StatusCode::OK);
    let replacement_cookie = login
        .headers()
        .get(reqwest::header::SET_COOKIE)
        .unwrap()
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();
    assert_ne!(replacement_cookie, cookie);
    assert_eq!(
        client
            .get(format!("{v3}/settings"))
            .header(reqwest::header::COOKIE, &replacement_cookie)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );

    gateway::stop_gateway(handle);
}

#[tokio::test]
async fn local_connection_rejects_rebinding_and_cross_origin_requests() {
    let state = state("local-authority");
    let handle = gateway::start_gateway_on(state.clone(), SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .unwrap();
    let client = loopback_client();
    let host = format!("127.0.0.1:{}", handle.port);
    let origin = format!("http://{host}");
    for (request_host, request_origin, expected) in [
        (host.as_str(), None, StatusCode::OK),
        (host.as_str(), Some(origin.as_str()), StatusCode::OK),
        (
            "localhost:30001",
            Some("http://localhost:30001"),
            StatusCode::OK,
        ),
        (
            "attacker.invalid",
            Some("http://attacker.invalid"),
            StatusCode::FORBIDDEN,
        ),
        ("attacker.invalid", None, StatusCode::FORBIDDEN),
        (
            host.as_str(),
            Some("http://attacker.invalid"),
            StatusCode::FORBIDDEN,
        ),
        (host.as_str(), Some("null"), StatusCode::FORBIDDEN),
    ] {
        let mut request = client
            .get(v3_url(handle.port, "/connection"))
            .header("host", request_host);
        if let Some(value) = request_origin {
            request = request.header("origin", value);
        }
        let response = request.send().await.unwrap();
        assert_eq!(
            response.status(),
            expected,
            "host={request_host}, origin={request_origin:?}"
        );
        let body = response.text().await.unwrap();
        assert_eq!(
            body.contains(&state.config().gateway_key),
            expected == StatusCode::OK
        );
    }
    let response = client
        .get(v3_url(handle.port, "/connection"))
        .header("sec-fetch-site", "cross-site")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    for path in [
        "/dashboard/api/auth/register",
        "/dashboard/api/v3/auth/register",
    ] {
        let response = client
            .post(format!("http://{host}{path}"))
            .header("host", "attacker.invalid")
            .header("origin", "http://attacker.invalid")
            .json(&cas(
                &state,
                json!({"username": "attacker", "password": "password123"}),
            ))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN, "{path}");
    }
    let response = client
        .get(v3_url(handle.port, "/connection"))
        .header("host", "attacker.invalid")
        .header(
            "cookie",
            format!(
                "ocg_dashboard_session={}",
                state.dashboard_session_token.lock()
            ),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    gateway::stop_gateway(handle);
}

#[tokio::test]
async fn loopback_dashboard_skips_login() {
    let state = state("local");
    let handle = gateway::start_gateway_on(state, SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .unwrap();
    let base = format!("http://127.0.0.1:{}/dashboard/api", handle.port);
    let client = loopback_client();

    let status = client
        .get(format!("{base}/auth/status"))
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    assert_eq!(status["local"], true);
    assert_eq!(status["authenticated"], true);
    assert_eq!(
        client
            .get(v3_url(handle.port, "/settings"))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    assert_v2_removed(client.get(format!("{base}/settings")).send().await.unwrap()).await;

    gateway::stop_gateway(handle);
}

#[tokio::test]
async fn loopback_settings_trim_and_require_gateway_key() {
    let state = state("settings-key");
    let handle = gateway::start_gateway_on(state.clone(), SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .unwrap();
    let url = v3_url(handle.port, "/settings");
    let client = loopback_client();
    let primary_before = state.config().gateway_key.clone();

    assert_eq!(
        client
            .put(&url)
            .json(&cas(
                &state,
                json!({
                    "clientRootUrl": "  http://192.168.1.20:9042/proxy/v1/  ",
                    "connectTimeoutSecs": 12,
                    "nonStreamTimeoutSecs": 345,
                    "streamIdleTimeoutSecs": 678
                })
            ))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    let saved = state.config();
    assert_eq!(saved.gateway_key, primary_before);
    assert_eq!(saved.client_root_url, "http://192.168.1.20:9042/proxy");
    assert_eq!(saved.connect_timeout_secs, 12);
    assert_eq!(saved.non_stream_timeout_secs, 345);
    assert_eq!(saved.stream_idle_timeout_secs, 678);
    let roundtrip = client
        .get(&url)
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    assert_eq!(roundtrip["connectTimeoutSecs"], 12);
    assert_eq!(roundtrip["nonStreamTimeoutSecs"], 345);
    assert_eq!(roundtrip["streamIdleTimeoutSecs"], 678);
    assert_eq!(roundtrip["clientRootUrl"], "http://192.168.1.20:9042/proxy");

    let before = state.db.lock().list_sub_gateway_keys().unwrap();
    let forged = cas(
        &state,
        json!({
            "connectTimeoutSecs": 12,
            "gatewayKeys": [{
                "id": "forged",
                "name": "Forged",
                "key": "ocg-forged",
                "enabled": true
            }]
        }),
    );
    assert_eq!(
        client
            .put(&url)
            .json(&forged)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::BAD_REQUEST,
        "V3 settings reject Key material"
    );
    assert_eq!(state.config().gateway_key, primary_before);
    assert_eq!(
        state.db.lock().list_sub_gateway_keys().unwrap(),
        before,
        "settings updates cannot create, modify, or remove sub keys"
    );

    for client_root_url in [
        "ocg.example.com",
        "ftp://ocg.example.com",
        "https://user:secret@ocg.example.com",
        "https://ocg.example.com?node=one",
        "https://ocg.example.com#settings",
        "https://ocg.example.com/v1/chat/completions",
    ] {
        assert_eq!(
            client
                .put(&url)
                .json(&cas(&state, json!({ "clientRootUrl": client_root_url })))
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::BAD_REQUEST,
            "{client_root_url}"
        );
        assert_eq!(
            state.config().client_root_url,
            "http://192.168.1.20:9042/proxy"
        );
    }

    for (field, value) in [
        ("connectTimeoutSecs", 0),
        ("connectTimeoutSecs", 301),
        ("nonStreamTimeoutSecs", 0),
        ("nonStreamTimeoutSecs", 3_601),
        ("streamIdleTimeoutSecs", 0),
        ("streamIdleTimeoutSecs", 3_601),
    ] {
        let mut extra = serde_json::Map::new();
        extra.insert(field.to_string(), json!(value));
        assert_eq!(
            client
                .put(&url)
                .json(&cas(&state, serde_json::Value::Object(extra)))
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::BAD_REQUEST,
            "{field}={value}"
        );
        let unchanged = state.config();
        assert_eq!(unchanged.connect_timeout_secs, 12);
        assert_eq!(unchanged.non_stream_timeout_secs, 345);
        assert_eq!(unchanged.stream_idle_timeout_secs, 678);
    }

    gateway::stop_gateway(handle);
}

#[tokio::test]
async fn loopback_settings_round_trip_routing_modes_and_reject_unknown_values() {
    let state = state("settings-routing");
    let handle = gateway::start_gateway_on(state.clone(), SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .unwrap();
    let url = v3_url(handle.port, "/settings");
    let client = loopback_client();

    for mode in [
        RoutingMode::StrictPriority,
        RoutingMode::StickyGlobal,
        RoutingMode::RoundRobin,
    ] {
        let sticky = mode != RoutingMode::StrictPriority;
        let response = client
            .put(&url)
            .json(&cas(
                &state,
                json!({
                    "routingMode": mode,
                    "conversationSticky": sticky
                }),
            ))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let loaded = client
            .get(&url)
            .send()
            .await
            .unwrap()
            .json::<serde_json::Value>()
            .await
            .unwrap();
        assert_eq!(loaded["routingMode"], serde_json::to_value(mode).unwrap());
        assert_eq!(loaded["conversationSticky"], sticky);
    }

    let before = state.config();
    let before_revision = state.settings_revision();
    let response = client
        .put(&url)
        .json(&cas(&state, json!({ "routingMode": "weighted-random" })))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(state.config().routing_mode, before.routing_mode);
    assert_eq!(
        state.config().conversation_sticky,
        before.conversation_sticky
    );
    assert_eq!(state.settings_revision(), before_revision);

    gateway::stop_gateway(handle);
}

#[tokio::test]
async fn loopback_settings_reject_stale_revision_after_key_regeneration() {
    let state = state("settings-stale-revision");
    let handle = gateway::start_gateway_on(state.clone(), SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .unwrap();
    let url = v3_url(handle.port, "/settings");
    let client = loopback_client();
    let loaded = client
        .get(&url)
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    let stale_revision = loaded["revision"].as_u64().unwrap();
    let stale_timeout = loaded["connectTimeoutSecs"].as_u64().unwrap();

    let regenerated = client
        .post(v3_url(handle.port, "/keys/primary/regenerate"))
        .json(&cas(&state, json!({})))
        .send()
        .await
        .unwrap();
    assert_eq!(regenerated.status(), StatusCode::OK);
    let regenerated = regenerated.json::<serde_json::Value>().await.unwrap();
    assert_ne!(regenerated["revision"].as_u64().unwrap(), stale_revision);
    let connection = client
        .get(v3_url(handle.port, "/connection"))
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    let regenerated_key = connection["primaryKey"].as_str().unwrap().to_string();

    let stale_update = client
        .put(&url)
        .json(&json!({
            "expectedRevision": stale_revision,
            "processGeneration": state.process_generation(),
            "connectTimeoutSecs": stale_timeout + 1
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(stale_update.status(), StatusCode::CONFLICT);
    assert_eq!(state.config().gateway_key, regenerated_key);
    assert_eq!(state.config().connect_timeout_secs, stale_timeout);

    gateway::stop_gateway(handle);
}

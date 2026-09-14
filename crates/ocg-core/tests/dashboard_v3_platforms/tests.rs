use super::harness::{V3Harness, start_loopback};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use std::collections::HashSet;

#[derive(Clone, Default)]
struct MockPlatform {
    fail_prices: std::sync::Arc<std::sync::atomic::AtomicBool>,
    calls: std::sync::Arc<std::sync::Mutex<Vec<(String, String)>>>,
}

async fn mock_platform(
    axum::extract::State(state): axum::extract::State<MockPlatform>,
    uri: axum::http::Uri,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let auth = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    state
        .calls
        .lock()
        .unwrap()
        .push((uri.path().to_string(), auth.clone()));
    let body = match uri.path() {
        "/api/status" => json!({"success":true,"data":{"quota_per_unit":500000}}),
        "/api/user/self" => json!({"success":true,"data":{"group":"default","quota":1000}}),
        "/api/user/self/groups" => {
            json!({"success":true,"data":{"first":{"ratio":1},"second":{"ratio":3}}})
        }
        "/api/subscription/self" => json!({"success":true,"data":{"subscriptions":[]}}),
        "/api/token/auto-groups" => json!({"success":true,"data":{"groups":[]}}),
        "/v1/models" => {
            json!({"object":"list","data":[{"id":"platform-e2e-model","object":"model"}]})
        }
        "/api/usage/token/" => {
            json!({"code":true,"data":{"object":"token_usage","total_used":25,"total_available":1000,"total_granted":1025,"unlimited_quota":false,"model_limits_enabled":false}})
        }
        "/api/pricing" => {
            if state.fail_prices.load(std::sync::atomic::Ordering::SeqCst) {
                return (axum::http::StatusCode::BAD_GATEWAY, "upstream unavailable")
                    .into_response();
            }
            json!({"success":true,"data":[{"model_name":"platform-e2e-model","quota_type":0,"model_ratio":2,"completion_ratio":2,"enable_groups":["first","second"]}],"group_ratio":{"first":1,"second":3}})
        }
        "/v1/chat/completions" => {
            if auth == "Bearer first-key" {
                return (
                    axum::http::StatusCode::TOO_MANY_REQUESTS,
                    axum::Json(json!({"error":{"message":"try another Key"}})),
                )
                    .into_response();
            }
            return ([("content-type","text/event-stream")],"data: {\"id\":\"mock\",\"object\":\"chat.completion.chunk\",\"model\":\"platform-e2e-model\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hello\"},\"finish_reason\":null}]}\n\ndata: {\"id\":\"mock\",\"object\":\"chat.completion.chunk\",\"choices\":[],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":5,\"total_tokens\":15}}\n\ndata: [DONE]\n\n").into_response();
        }
        _ => return axum::http::StatusCode::NOT_FOUND.into_response(),
    };
    axum::Json(body).into_response()
}

#[tokio::test]
async fn platform_refresh_fallback_stream_and_stale_price_end_to_end() {
    use std::sync::atomic::Ordering;
    let mock = MockPlatform::default();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let app = axum::Router::new()
        .fallback(mock_platform)
        .with_state(mock.clone());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let h = start_loopback("platform-stream").await;
    let (status, parent) = send(
        &h,
        Method::POST,
        "/platform-accounts",
        cas(
            &h,
            json!({"kind":"new_api","name":"Local platform","baseUrl":origin,"userCredential":"user-metadata-key"}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{parent}");
    let id = parent["accounts"][0]["id"].as_str().unwrap();
    let mut keys = Vec::new();
    for group in ["first", "second"] {
        let (status,key)=send(&h,Method::POST,"/accounts",cas(&h,json!({"name":group,"providerId":"custom","key":format!("{group}-key"),"customConfig":{"endpointUrl":format!("{origin}/v1/chat/completions"),"upstreamProtocol":"chat_completions"},"modelCapabilities":[{"publicModel":"platform-e2e-model","upstreamModel":"platform-e2e-model","protocol":"chat_completions"}]}))).await;
        assert_eq!(status, StatusCode::OK, "{key}");
        let key_id = key["account"]["id"].as_str().unwrap().to_string();
        let (status, enabled) = send(
            &h,
            Method::POST,
            &format!("/accounts/{key_id}/toggle"),
            cas(&h, json!({})),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{enabled}");
        let (status,linked)=send(&h,Method::PUT,&format!("/accounts/{key_id}/platform-link"),cas(&h,json!({"platformAccountId":id,"group":{"id":group,"platform":null,"autoGroups":[],"verified":false}}))).await;
        assert_eq!(status, StatusCode::OK, "{linked}");
        let (status, refreshed) = send(
            &h,
            Method::POST,
            &format!("/platform-accounts/{id}/refresh"),
            cas(&h, json!({"accountId":key_id})),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{refreshed}");
        let link = refreshed["links"]
            .as_array()
            .unwrap()
            .iter()
            .find(|l| l["accountId"] == key_id)
            .unwrap();
        assert_eq!(link["snapshot"]["stale"], false, "{link}");
        keys.push(key_id);
    }
    let response=h.client.post(format!("http://127.0.0.1:{}/v1/chat/completions",h.handle.port)).bearer_auth(h.state.config().gateway_key).json(&json!({"model":"platform-e2e-model","stream":true,"messages":[{"role":"user","content":"hello"}]})).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let stream = response.text().await.unwrap();
    assert!(
        stream.contains("hello") && stream.contains("[DONE]"),
        "{stream}"
    );
    let (_, logs) = send(&h, Method::GET, "/logs/forward", json!({})).await;
    let encoded = logs.to_string();
    assert!(!encoded.contains("first-key") && !encoded.contains("second-key"));
    let attempts = mock
        .calls
        .lock()
        .unwrap()
        .iter()
        .filter(|(p, _)| p == "/v1/chat/completions")
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(
        attempts.iter().map(|(_, a)| a.as_str()).collect::<Vec<_>>(),
        ["Bearer first-key", "Bearer second-key"]
    );
    let rows = logs["items"].as_array().expect("forward log items");
    let successful = rows.iter().find(|l| l["accountId"] == keys[1]).unwrap();
    assert_eq!(successful["nativeCostCurrency"], "USD", "{successful}");
    assert!(
        (successful["nativeCostValue"].as_f64().unwrap() - 0.00024).abs() < 1e-12,
        "{successful}"
    );
    assert!(successful["rawCostUsd"].is_null() && successful["quotaDebit"].is_null());
    mock.fail_prices.store(true, Ordering::SeqCst);
    let (_, stale) = send(
        &h,
        Method::POST,
        &format!("/platform-accounts/{id}/refresh"),
        cas(&h, json!({"accountId":keys[1]})),
    )
    .await;
    let link = stale["links"]
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["accountId"] == keys[1])
        .unwrap();
    assert_eq!(link["snapshot"]["stale"], true);
    assert!(!link["snapshot"]["prices"].as_array().unwrap().is_empty());
    server.abort();
}

fn cas(h: &V3Harness, mut body: Value) -> Value {
    body["expectedRevision"] = json!(h.state.settings_revision());
    body["processGeneration"] = json!(h.state.process_generation());
    body
}
async fn send(h: &V3Harness, method: Method, path: &str, body: Value) -> (StatusCode, Value) {
    let r = h
        .client
        .request(method, format!("{}{path}", h.v3_base))
        .json(&body)
        .send()
        .await
        .unwrap();
    let status = r.status();
    (status, r.json().await.unwrap())
}

fn migration_gate() -> &'static tokio::sync::Mutex<()> {
    static GATE: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
    GATE.get_or_init(|| tokio::sync::Mutex::new(()))
}

async fn export_import(source: &V3Harness, target: &V3Harness, password: &str) {
    let _gate = migration_gate().lock().await;
    let (status, export) = send(
        source,
        Method::POST,
        "/accounts/transfer/export",
        json!({"bundlePassword":password}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{export}");
    let (status, imported) = send(
        target,
        Method::POST,
        "/accounts/transfer/import",
        cas(
            target,
            json!({"password":password,"bundle":export["bundle"]}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{imported}");
}

#[tokio::test]
async fn platform_accounts_cas_link_and_v5_secret_free_roundtrip() {
    let source = start_loopback("platform-v5-source").await;
    let target = start_loopback("platform-v5-target").await;
    let (status,parent)=send(&source,Method::POST,"/platform-accounts",cas(&source,json!({"kind":"new_api","name":"New API","baseUrl":"https://platform.example","userCredential":"management-secret-test"}))).await;
    assert_eq!(status, StatusCode::OK, "{parent}");
    assert!(!parent.to_string().contains("management-secret-test"));
    let id = parent["accounts"][0]["id"].as_str().unwrap();
    assert_eq!(parent["accounts"][0]["hasUserCredential"], true);
    let stale = cas(&source, json!({"name":"stale"}));
    source.state.bump_settings_revision();
    let (status, _) = send(
        &source,
        Method::PUT,
        &format!("/platform-accounts/{id}"),
        stale,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (status,key)=send(&source,Method::POST,"/accounts",cas(&source,json!({"name":"Platform Key","providerId":"custom","key":"inference-key-test","customConfig":{"endpointUrl":"https://old.example/v1/chat/completions","upstreamProtocol":"chat_completions"},"modelCapabilities":[{"publicModel":"model-a","upstreamModel":"model-a","protocol":"chat_completions"}]}))).await;
    assert_eq!(status, StatusCode::OK, "{key}");
    let key_id = key["account"]["id"].as_str().unwrap();
    let group = json!({"id":"default","platform":null,"autoGroups":[],"verified":true});
    let (status, linked) = send(
        &source,
        Method::PUT,
        &format!("/accounts/{key_id}/platform-link"),
        cas(&source, json!({"platformAccountId":id,"group":group})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{linked}");
    assert_eq!(linked["links"][0]["group"]["verified"], false);
    let (status, _) = send(
        &source,
        Method::DELETE,
        &format!("/platform-accounts/{id}"),
        cas(&source, json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    export_import(&source, &target, "platform-bundle-password").await;
    let (_, loaded) = send(&target, Method::GET, "/platform-accounts", json!({})).await;
    assert_eq!(loaded["accounts"][0]["id"], id);
    assert_eq!(loaded["accounts"][0]["hasUserCredential"], false);
    assert!(loaded["accounts"][0]["snapshot"].is_null());
    assert_eq!(loaded["links"][0]["accountId"], key_id);
    assert_eq!(loaded["links"][0]["group"]["verified"], false);
    let (status, _) = send(
        &source,
        Method::DELETE,
        &format!("/accounts/{key_id}/platform-link"),
        cas(&source, json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    export_import(&source, &target, "platform-bundle-password").await;
    assert!(
        target
            .state
            .db
            .lock()
            .list_platform_links()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        target
            .state
            .db
            .lock()
            .account_custom_config(key_id)
            .unwrap()
            .unwrap()
            .endpoint_url,
        "https://platform.example/v1/chat/completions"
    );
    let (status, _) = send(
        &target,
        Method::DELETE,
        &format!("/accounts/{key_id}/platform-link"),
        cas(&target, json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        target
            .state
            .db
            .lock()
            .get_account(key_id)
            .unwrap()
            .is_some()
    );
    let (status, _) = send(
        &target,
        Method::DELETE,
        &format!("/platform-accounts/{id}"),
        cas(&target, json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

/// One local New API or Sub2API installation. Kind is a type; many site
/// instances of that type coexist, each with its own origin and catalog.
#[derive(Clone)]
struct IsolatedSite {
    kind: &'static str,
    model: &'static str,
    groups: &'static [&'static str],
    hits: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
}

async fn isolated_site(
    axum::extract::State(state): axum::extract::State<IsolatedSite>,
    uri: axum::http::Uri,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    state.hits.lock().unwrap().push(uri.path().to_string());
    let groups = state.groups;
    let model = state.model;
    let body = match (state.kind, uri.path()) {
        ("new_api", "/api/status") => json!({"success":true,"data":{"quota_per_unit":500000}}),
        ("new_api", "/api/user/self") => {
            json!({"success":true,"data":{"group":groups[0],"quota":1000}})
        }
        ("new_api", "/api/user/self/groups") => {
            let data = groups
                .iter()
                .map(|group| (group.to_string(), json!({"ratio":1})))
                .collect::<serde_json::Map<_, _>>();
            json!({"success":true,"data":data})
        }
        ("new_api", "/api/subscription/self") => {
            json!({"success":true,"data":{"subscriptions":[]}})
        }
        ("new_api", "/api/token/auto-groups") => json!({"success":true,"data":{"groups":[]}}),
        ("new_api", "/api/usage/token/") => {
            json!({"code":true,"data":{"object":"token_usage","total_used":1,"total_available":2,"total_granted":3,"unlimited_quota":false,"model_limits_enabled":false}})
        }
        ("new_api", "/api/pricing") => {
            let ratio = groups
                .iter()
                .map(|group| (group.to_string(), json!(1)))
                .collect::<serde_json::Map<_, _>>();
            json!({"success":true,"data":[{"model_name":model,"quota_type":0,"model_ratio":1,"completion_ratio":1,"enable_groups":groups}],"group_ratio":ratio})
        }
        ("sub2api", "/api/v1/user/profile") => json!({"code":0,"data":{"id":1,"balance":10.0}}),
        ("sub2api", "/api/v1/subscriptions/summary") => {
            json!({"code":0,"data":{"active_count":0,"subscriptions":[]}})
        }
        ("sub2api", "/api/v1/groups/available") => {
            let data = groups
                .iter()
                .map(|name| json!({"name":name,"platform":name}))
                .collect::<Vec<_>>();
            json!({"code":0,"data":data})
        }
        ("sub2api", "/v1/usage") => {
            json!({"mode":"unrestricted","remaining":10.0,"balance":10.0,"unit":"USD"})
        }
        ("sub2api", "/v1/sub2api/billing") => {
            json!({"object":"sub2api.key_billing","schema_version":1,"billing_scope":"token","effective_rate_multiplier":1.0,"peak_rate_enabled":false})
        }
        ("sub2api", "/api/v1/model-plaza") => json!({"code":0,"data":{"groups":[]}}),
        (_, "/v1/models") => json!({"object":"list","data":[{"id":model,"object":"model"}]}),
        _ => return axum::http::StatusCode::NOT_FOUND.into_response(),
    };
    axum::Json(body).into_response()
}

async fn spawn_isolated_site(
    kind: &'static str,
    model: &'static str,
    groups: &'static [&'static str],
) -> (String, IsolatedSite, tokio::task::JoinHandle<()>) {
    let site = IsolatedSite {
        kind,
        model,
        groups,
        hits: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let app = axum::Router::new()
        .fallback(isolated_site)
        .with_state(site.clone());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (origin, site, server)
}

fn hit_count(site: &IsolatedSite) -> usize {
    site.hits.lock().unwrap().len()
}

fn account_ids(view: &Value) -> Vec<String> {
    view["accounts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["id"].as_str().unwrap().to_string())
        .collect()
}

fn parent<'a>(view: &'a Value, id: &str) -> &'a Value {
    view["accounts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == id)
        .unwrap_or_else(|| panic!("missing platform account {id} in {view}"))
}

fn link_of<'a>(view: &'a Value, account_id: &str) -> &'a Value {
    view["links"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["accountId"] == account_id)
        .unwrap_or_else(|| panic!("missing link {account_id} in {view}"))
}

fn snapshot_ids(snapshot: &Value, field: &str, key: &str) -> Vec<String> {
    snapshot
        .get(field)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|row| row[key].as_str().map(str::to_string))
        .collect()
}

fn endpoint_of(h: &V3Harness, account_id: &str) -> String {
    h.state
        .db
        .lock()
        .account_custom_config(account_id)
        .unwrap()
        .unwrap()
        .endpoint_url
}

async fn create_parent(
    h: &V3Harness,
    kind: &str,
    name: &str,
    origin: &str,
    credential: &str,
) -> String {
    let (_, before) = send(h, Method::GET, "/platform-accounts", json!({})).await;
    let seen: HashSet<String> = account_ids(&before).into_iter().collect();
    let (status, body) = send(
        h,
        Method::POST,
        "/platform-accounts",
        cas(
            h,
            json!({"kind":kind,"name":name,"baseUrl":origin,"userCredential":credential}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let created: Vec<_> = account_ids(&body)
        .into_iter()
        .filter(|id| !seen.contains(id))
        .collect();
    assert_eq!(created.len(), 1, "{body}");
    let id = created[0].clone();
    let row = parent(&body, &id);
    assert!(uuid::Uuid::parse_str(&id).is_ok(), "{id}");
    assert_eq!(row["kind"], kind, "{row}");
    assert_eq!(row["name"], name, "{row}");
    assert_eq!(row["baseUrl"], origin, "{row}");
    id
}

async fn create_custom(h: &V3Harness, name: &str, key: &str, origin: &str) -> String {
    let (status, body) = send(
        h,
        Method::POST,
        "/accounts",
        cas(
            h,
            json!({
                "name":name,
                "providerId":"custom",
                "key":key,
                "customConfig":{"endpointUrl":format!("{origin}/v1/chat/completions"),"upstreamProtocol":"chat_completions"},
                "modelCapabilities":[{"publicModel":"placeholder","upstreamModel":"placeholder","protocol":"chat_completions"}]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["account"]["id"].as_str().unwrap().to_string()
}

async fn link_key(h: &V3Harness, account_id: &str, parent_id: &str, group: &str) -> Value {
    let (status, body) = send(
        h,
        Method::PUT,
        &format!("/accounts/{account_id}/platform-link"),
        cas(
            h,
            json!({"platformAccountId":parent_id,"group":{"id":group,"platform":null,"autoGroups":[],"verified":false}}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body
}

async fn refresh_parent(h: &V3Harness, parent_id: &str) -> Value {
    let (status, body) = send(
        h,
        Method::POST,
        &format!("/platform-accounts/{parent_id}/refresh"),
        cas(h, json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body
}

async fn refresh_key(h: &V3Harness, parent_id: &str, account_id: &str) -> Value {
    let (status, body) = send(
        h,
        Method::POST,
        &format!("/platform-accounts/{parent_id}/refresh"),
        cas(h, json!({"accountId":account_id})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body
}

#[tokio::test]
async fn same_kind_site_instances_keep_independent_refresh_and_links() {
    let (new_a_url, new_a_site, new_a_server) =
        spawn_isolated_site("new_api", "site-new-a-model", &["default", "east"]).await;
    let (new_b_url, new_b_site, new_b_server) =
        spawn_isolated_site("new_api", "site-new-b-model", &["default", "west"]).await;
    let (sub_a_url, sub_a_site, sub_a_server) =
        spawn_isolated_site("sub2api", "site-sub2-a-model", &["claude"]).await;
    let (sub_b_url, sub_b_site, sub_b_server) =
        spawn_isolated_site("sub2api", "site-sub2-b-model", &["openai"]).await;

    let source = start_loopback("platform-same-kind-source").await;
    let new_a = create_parent(&source, "new_api", "New API", &new_a_url, "new-a-user").await;
    let new_b = create_parent(&source, "new_api", "New API", &new_b_url, "new-b-user").await;
    let sub_a = create_parent(&source, "sub2api", "Sub2API", &sub_a_url, "sub-a-user").await;
    let sub_b = create_parent(&source, "sub2api", "Sub2API", &sub_b_url, "sub-b-user").await;

    let (_, listed) = send(&source, Method::GET, "/platform-accounts", json!({})).await;
    let ids = account_ids(&listed);
    assert_eq!(ids.len(), 4, "{listed}");
    let unique: HashSet<_> = ids.iter().cloned().collect();
    assert_eq!(unique.len(), 4, "{listed}");
    assert_eq!(parent(&listed, &new_a)["name"], "New API");
    assert_eq!(parent(&listed, &new_b)["name"], "New API");
    assert_eq!(parent(&listed, &sub_a)["name"], "Sub2API");
    assert_eq!(parent(&listed, &sub_b)["name"], "Sub2API");
    assert_ne!(
        parent(&listed, &new_a)["baseUrl"],
        parent(&listed, &new_b)["baseUrl"]
    );
    assert_ne!(
        parent(&listed, &sub_a)["baseUrl"],
        parent(&listed, &sub_b)["baseUrl"]
    );

    let new_a_key = create_custom(&source, "new-a-key", "new-a-secret", &new_a_url).await;
    let new_b_key = create_custom(&source, "new-b-key", "new-b-secret", &new_b_url).await;
    let sub_a_key = create_custom(&source, "sub-a-key", "sub-a-secret", &sub_a_url).await;
    let sub_b_key = create_custom(&source, "sub-b-key", "sub-b-secret", &sub_b_url).await;
    let linked = link_key(&source, &new_a_key, &new_a, "east").await;
    link_key(&source, &new_b_key, &new_b, "west").await;
    link_key(&source, &sub_a_key, &sub_a, "claude").await;
    link_key(&source, &sub_b_key, &sub_b, "openai").await;
    assert_eq!(link_of(&linked, &new_a_key)["platformAccountId"], new_a);
    assert_eq!(
        endpoint_of(&source, &new_a_key),
        format!("{new_a_url}/v1/chat/completions")
    );
    assert_eq!(
        endpoint_of(&source, &new_b_key),
        format!("{new_b_url}/v1/chat/completions")
    );
    assert_eq!(
        endpoint_of(&source, &sub_a_key),
        format!("{sub_a_url}/v1/chat/completions")
    );
    assert_eq!(
        endpoint_of(&source, &sub_b_key),
        format!("{sub_b_url}/v1/chat/completions")
    );

    let after_a = refresh_parent(&source, &new_a).await;
    assert!(
        snapshot_ids(&parent(&after_a, &new_a)["snapshot"], "groups", "id")
            .contains(&"east".into()),
        "{after_a}"
    );
    assert!(
        !snapshot_ids(&parent(&after_a, &new_a)["snapshot"], "groups", "id")
            .contains(&"west".into())
    );
    assert!(parent(&after_a, &new_b)["snapshot"].is_null(), "{after_a}");
    assert!(parent(&after_a, &sub_a)["snapshot"].is_null(), "{after_a}");
    assert!(parent(&after_a, &sub_b)["snapshot"].is_null(), "{after_a}");
    assert_eq!(hit_count(&new_b_site), 0);
    assert_eq!(hit_count(&sub_a_site), 0);
    assert_eq!(hit_count(&sub_b_site), 0);

    refresh_parent(&source, &new_b).await;
    refresh_parent(&source, &sub_a).await;
    let parents = refresh_parent(&source, &sub_b).await;
    let new_a_groups = snapshot_ids(&parent(&parents, &new_a)["snapshot"], "groups", "id");
    let new_b_groups = snapshot_ids(&parent(&parents, &new_b)["snapshot"], "groups", "id");
    assert!(
        new_a_groups.contains(&"east".into()) && !new_a_groups.contains(&"west".into()),
        "{parents}"
    );
    assert!(
        new_b_groups.contains(&"west".into()) && !new_b_groups.contains(&"east".into()),
        "{parents}"
    );
    assert_eq!(
        snapshot_ids(&parent(&parents, &sub_a)["snapshot"], "groups", "id"),
        ["claude"]
    );
    assert_eq!(
        snapshot_ids(&parent(&parents, &sub_b)["snapshot"], "groups", "id"),
        ["openai"]
    );

    let new_a_hits = hit_count(&new_a_site);
    let new_b_hits = hit_count(&new_b_site);
    let key_a = refresh_key(&source, &new_a, &new_a_key).await;
    assert_eq!(
        snapshot_ids(&link_of(&key_a, &new_a_key)["snapshot"], "models", "id"),
        ["site-new-a-model"]
    );
    assert!(link_of(&key_a, &new_b_key)["snapshot"].is_null(), "{key_a}");
    assert!(link_of(&key_a, &sub_a_key)["snapshot"].is_null(), "{key_a}");
    assert!(hit_count(&new_a_site) > new_a_hits);
    assert_eq!(hit_count(&new_b_site), new_b_hits);
    assert_eq!(
        snapshot_ids(&parent(&key_a, &new_b)["snapshot"], "groups", "id"),
        new_b_groups
    );

    let new_a_hits = hit_count(&new_a_site);
    let (status, rejected) = send(
        &source,
        Method::POST,
        &format!("/platform-accounts/{new_b}/refresh"),
        cas(&source, json!({"accountId":new_a_key})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{rejected}");
    assert_eq!(hit_count(&new_b_site), new_b_hits);
    assert_eq!(hit_count(&new_a_site), new_a_hits);

    refresh_key(&source, &new_b, &new_b_key).await;
    refresh_key(&source, &sub_a, &sub_a_key).await;
    let keys = refresh_key(&source, &sub_b, &sub_b_key).await;
    assert_eq!(
        snapshot_ids(&link_of(&keys, &new_a_key)["snapshot"], "models", "id"),
        ["site-new-a-model"]
    );
    assert_eq!(
        snapshot_ids(&link_of(&keys, &new_b_key)["snapshot"], "models", "id"),
        ["site-new-b-model"]
    );
    assert_eq!(
        snapshot_ids(&link_of(&keys, &sub_a_key)["snapshot"], "models", "id"),
        ["site-sub2-a-model"]
    );
    assert_eq!(
        snapshot_ids(&link_of(&keys, &sub_b_key)["snapshot"], "models", "id"),
        ["site-sub2-b-model"]
    );
    assert_eq!(
        snapshot_ids(&parent(&keys, &new_a)["snapshot"], "groups", "id"),
        new_a_groups
    );
    assert_eq!(
        endpoint_of(&source, &new_a_key),
        format!("{new_a_url}/v1/chat/completions")
    );
    assert_eq!(
        endpoint_of(&source, &new_b_key),
        format!("{new_b_url}/v1/chat/completions")
    );

    let (status, renamed) = send(
        &source,
        Method::PUT,
        &format!("/platform-accounts/{new_a}"),
        cas(&source, json!({"name":"New API East"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{renamed}");
    assert_eq!(parent(&renamed, &new_a)["name"], "New API East");
    assert_eq!(parent(&renamed, &new_b)["name"], "New API");
    assert_eq!(parent(&renamed, &new_a)["baseUrl"], new_a_url);
    assert_eq!(
        snapshot_ids(&link_of(&renamed, &new_b_key)["snapshot"], "models", "id"),
        ["site-new-b-model"]
    );

    let (status, cleared) = send(
        &source,
        Method::PUT,
        &format!("/platform-accounts/{new_a}"),
        cas(&source, json!({"name":"New API East","userCredential":""})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{cleared}");
    assert!(parent(&cleared, &new_a)["snapshot"].is_null(), "{cleared}");
    assert!(
        link_of(&cleared, &new_a_key)["snapshot"].is_null(),
        "{cleared}"
    );
    assert_eq!(parent(&cleared, &new_b)["name"], "New API");
    assert!(
        parent(&cleared, &new_b)["snapshot"].is_object(),
        "{cleared}"
    );
    assert_eq!(
        snapshot_ids(&link_of(&cleared, &new_b_key)["snapshot"], "models", "id"),
        ["site-new-b-model"]
    );
    assert_eq!(
        snapshot_ids(&link_of(&cleared, &sub_a_key)["snapshot"], "models", "id"),
        ["site-sub2-a-model"]
    );

    let target = start_loopback("platform-same-kind-target").await;
    export_import(&source, &target, "platform-bundle-password").await;
    let (_, loaded) = send(&target, Method::GET, "/platform-accounts", json!({})).await;
    assert_eq!(account_ids(&loaded).len(), 4, "{loaded}");
    for (id, kind, name, url) in [
        (
            new_a.as_str(),
            "new_api",
            "New API East",
            new_a_url.as_str(),
        ),
        (new_b.as_str(), "new_api", "New API", new_b_url.as_str()),
        (sub_a.as_str(), "sub2api", "Sub2API", sub_a_url.as_str()),
        (sub_b.as_str(), "sub2api", "Sub2API", sub_b_url.as_str()),
    ] {
        let row = parent(&loaded, id);
        assert_eq!(row["kind"], kind, "{row}");
        assert_eq!(row["name"], name, "{row}");
        assert_eq!(row["baseUrl"], url, "{row}");
        assert_eq!(row["hasUserCredential"], false, "{row}");
        assert!(row["snapshot"].is_null(), "{row}");
    }
    assert_eq!(link_of(&loaded, &new_a_key)["platformAccountId"], new_a);
    assert_eq!(link_of(&loaded, &new_b_key)["platformAccountId"], new_b);
    assert_eq!(link_of(&loaded, &sub_a_key)["platformAccountId"], sub_a);
    assert_eq!(link_of(&loaded, &sub_b_key)["platformAccountId"], sub_b);
    assert!(link_of(&loaded, &new_b_key)["snapshot"].is_null());
    assert_eq!(
        endpoint_of(&target, &new_a_key),
        format!("{new_a_url}/v1/chat/completions")
    );
    assert_eq!(
        endpoint_of(&target, &new_b_key),
        format!("{new_b_url}/v1/chat/completions")
    );
    assert_eq!(
        endpoint_of(&target, &sub_a_key),
        format!("{sub_a_url}/v1/chat/completions")
    );
    assert_eq!(
        endpoint_of(&target, &sub_b_key),
        format!("{sub_b_url}/v1/chat/completions")
    );

    let rediscovered = refresh_key(&target, &new_a, &new_a_key).await;
    assert_eq!(
        snapshot_ids(
            &link_of(&rediscovered, &new_a_key)["snapshot"],
            "models",
            "id"
        ),
        ["site-new-a-model"]
    );
    assert!(
        link_of(&rediscovered, &new_b_key)["snapshot"].is_null(),
        "{rediscovered}"
    );
    let rediscovered = refresh_key(&target, &new_b, &new_b_key).await;
    assert_eq!(
        snapshot_ids(
            &link_of(&rediscovered, &new_b_key)["snapshot"],
            "models",
            "id"
        ),
        ["site-new-b-model"]
    );
    assert_eq!(
        snapshot_ids(
            &link_of(&rediscovered, &new_a_key)["snapshot"],
            "models",
            "id"
        ),
        ["site-new-a-model"]
    );

    let (status, blocked) = send(
        &source,
        Method::DELETE,
        &format!("/platform-accounts/{new_b}"),
        cas(&source, json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{blocked}");
    let (status, _) = send(
        &source,
        Method::DELETE,
        &format!("/accounts/{new_a_key}/platform-link"),
        cas(&source, json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = send(
        &source,
        Method::DELETE,
        &format!("/platform-accounts/{new_a}"),
        cas(&source, json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (_, remaining) = send(&source, Method::GET, "/platform-accounts", json!({})).await;
    assert_eq!(account_ids(&remaining).len(), 3, "{remaining}");
    assert!(
        remaining["accounts"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["id"] != new_a)
    );
    assert_eq!(parent(&remaining, &new_b)["name"], "New API");
    assert_eq!(
        snapshot_ids(&link_of(&remaining, &new_b_key)["snapshot"], "models", "id"),
        ["site-new-b-model"]
    );
    assert_eq!(
        snapshot_ids(&link_of(&remaining, &sub_a_key)["snapshot"], "models", "id"),
        ["site-sub2-a-model"]
    );
    assert_eq!(
        endpoint_of(&source, &new_b_key),
        format!("{new_b_url}/v1/chat/completions")
    );
    assert_eq!(
        endpoint_of(&source, &new_a_key),
        format!("{new_a_url}/v1/chat/completions")
    );

    new_a_server.abort();
    new_b_server.abort();
    sub_a_server.abort();
    sub_b_server.abort();
}

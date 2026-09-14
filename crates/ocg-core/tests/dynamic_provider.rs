//! Dynamic Provider persistence, V3 control plane, and routing snapshot tests.

use chrono::Utc;
use ocg_core::dashboard_v3::{ERROR_BUILTIN_PROVIDER_IMMUTABLE, ERROR_INVALID_REQUEST};
use ocg_core::models::ProxyMode;
use ocg_core::provider::{COMMAND_CODE_PROVIDER_ID, CUSTOM_PROVIDER_ID, OPENCODE_PROVIDER_ID};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet, VecDeque};
use std::time::Duration;

#[path = "fixtures/dynamic_protocols.rs"]
mod dynamic_protocols;
#[allow(dead_code)]
#[path = "fixtures/fake_upstream.rs"]
mod fake_upstream;
#[path = "fixtures/dashboard_v3/harness.rs"]
mod harness;

use fake_upstream::{FakeReply, start_fake_upstream, start_fake_upstream_with_delay};
use harness::{V3Harness, start_loopback};

const CHAT_OK: &str = r#"{"id":"ok","object":"chat.completion","model":"vendor/opus","choices":[{"index":0,"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":1}}"#;

#[tokio::test]
async fn discovered_model_probes_and_routes_all_three_client_formats_with_streaming() {
    for (upstream_protocol, suffix, auth, reply, stream_reply) in [
        (
            "chat_completions",
            "chat/completions",
            "bearer",
            CHAT_OK,
            dynamic_protocols::CHAT_STREAM,
        ),
        (
            "responses",
            "responses",
            "bearer",
            dynamic_protocols::RESPONSES,
            dynamic_protocols::RESPONSES_STREAM,
        ),
        (
            "messages",
            "messages",
            "x-api-key",
            dynamic_protocols::MESSAGES,
            dynamic_protocols::MESSAGES_STREAM,
        ),
    ] {
        let mut queue = VecDeque::from([
            FakeReply {
                status: 200,
                body: r#"{"data":[{"id":"vendor/opus"}],"has_more":true,"last_id":"vendor/opus"}"#,
            },
            FakeReply {
                status: 200,
                body: r#"{"data":[{"id":"vendor/second"}],"has_more":false}"#,
            },
            FakeReply {
                status: 200,
                body: reply,
            },
        ]);
        for _ in 0..3 {
            queue.push_back(FakeReply {
                status: 200,
                body: reply,
            });
            queue.push_back(FakeReply {
                status: 200,
                body: stream_reply,
            });
        }
        let (upstream, calls, _stop) =
            start_fake_upstream(HashMap::from([("sk-matrix".into(), queue)])).await;
        let harness = start_loopback(&format!("dyn-matrix-{upstream_protocol}")).await;
        let mut config = harness.state.config();
        config.proxy_mode = ProxyMode::Direct;
        harness.state.set_config(config).unwrap();
        let endpoint = format!("{upstream}/tenant/api/v1/{suffix}");
        let before = harness.state.settings_revision();
        let (status, discovered) = send_json(&harness, Method::POST, "/providers/models/discover", &json!({
            "endpointUrl": endpoint, "upstreamProtocol": upstream_protocol, "authKind": auth, "key": "sk-matrix"
        })).await;
        assert_eq!(status, StatusCode::OK, "{discovered}");
        assert_eq!(
            discovered["models"],
            json!(["vendor/opus", "vendor/second"])
        );
        assert_eq!(discovered["truncated"], false);
        let (status, probed) = send_json(&harness, Method::POST, "/providers/test", &json!({
            "endpointUrl": endpoint, "upstreamProtocol": upstream_protocol, "authKind": auth, "key": "sk-matrix",
            "publicModel": "lab-opus", "upstreamModel": discovered["models"][0]
        })).await;
        assert_eq!(status, StatusCode::OK, "{probed}");
        assert_eq!(probed["ok"], true, "{probed}");
        assert_eq!(
            harness.state.settings_revision(),
            before,
            "discovery/probes must not save routing"
        );
        let (status, created) = send_json(
            &harness,
            Method::POST,
            "/providers",
            &cas(
                &harness,
                create_body(
                    "matrix",
                    &endpoint,
                    upstream_protocol,
                    auth,
                    Some("sk-matrix"),
                ),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{created}");
        let provider_id = created["provider"]["id"].as_str().unwrap().to_string();
        enable_accounts_for_provider(&harness, &provider_id);
        assert!(
            listed_gateway_model_ids(&harness)
                .await
                .contains(&"lab-opus".to_string())
        );
        for client_protocol in ["chat/completions", "responses", "messages"] {
            for stream in [false, true] {
                let mut body = match client_protocol {
                    "responses" => {
                        json!({"model":"lab-opus","input":"ping","store":false,"max_output_tokens":32,
                        "tools":[{"type":"function","name":"lookup","parameters":{"type":"object","properties":{}}}]})
                    }
                    "messages" => {
                        json!({"model":"lab-opus","messages":[{"role":"user","content":"ping"}],"max_tokens":32,
                        "tools":[{"name":"lookup","input_schema":{"type":"object","properties":{}}}]})
                    }
                    _ => {
                        json!({"model":"lab-opus","messages":[{"role":"user","content":"ping"}],"max_tokens":32,
                        "tools":[{"type":"function","function":{"name":"lookup","parameters":{"type":"object","properties":{}}}}]})
                    }
                };
                body["stream"] = json!(stream);
                let response = harness
                    .client
                    .post(format!(
                        "http://127.0.0.1:{}/v1/{client_protocol}",
                        harness.handle.port
                    ))
                    .bearer_auth(&harness.state.config().gateway_key)
                    .header("cookie", "session=must-not-leak")
                    .json(&body)
                    .send()
                    .await
                    .unwrap();
                let status = response.status();
                let result = response.text().await.unwrap();
                assert_eq!(
                    status,
                    StatusCode::OK,
                    "{upstream_protocol} -> {client_protocol} stream={stream}: {result}"
                );
                assert!(!result.contains("sk-matrix"));
                if stream {
                    assert!(result.contains("hello"), "{result}");
                    let terminal = match client_protocol {
                        "responses" => "response.completed",
                        "messages" => "message_stop",
                        _ => "[DONE]",
                    };
                    assert!(result.contains(terminal), "{result}");
                } else {
                    let result: Value = serde_json::from_str(&result).unwrap();
                    let field = match client_protocol {
                        "responses" => "output",
                        "messages" => "content",
                        _ => "choices",
                    };
                    assert!(result[field].is_array(), "{result}");
                }
            }
        }
        let calls = calls.lock().unwrap();
        assert_eq!(calls.len(), 9);
        assert_eq!(calls[0].path, "/tenant/api/v1/models");
        assert_eq!(calls[1].path, "/tenant/api/v1/models");
        for call in calls.iter().skip(2) {
            assert_eq!(call.path, format!("/tenant/api/v1/{suffix}"));
            assert_eq!(
                serde_json::from_str::<Value>(&call.body).unwrap()["model"],
                "vendor/opus"
            );
            assert!(call.cookie.is_none());
            if auth == "bearer" {
                assert_eq!(call.authorization.as_deref(), Some("Bearer sk-matrix"));
                assert!(call.x_api_key.is_none());
            } else {
                assert_eq!(call.x_api_key.as_deref(), Some("sk-matrix"));
                assert!(call.authorization.is_none());
                assert!(call.anthropic_version.is_some());
            }
        }
        for (index, call) in calls.iter().skip(3).enumerate() {
            let sent: Value = serde_json::from_str(&call.body).unwrap();
            assert_eq!(sent["stream"], index % 2 == 1, "{sent}");
            if upstream_protocol == "responses" {
                assert!(
                    sent["input"].is_array() || sent["input"].is_string(),
                    "{sent}"
                );
                assert!(
                    sent["max_output_tokens"]
                        .as_u64()
                        .is_some_and(|value| value > 0),
                    "{sent}"
                );
            } else {
                assert!(
                    sent["messages"]
                        .as_array()
                        .is_some_and(|rows| !rows.is_empty()),
                    "{sent}"
                );
                assert!(
                    sent["max_tokens"].as_u64().is_some_and(|value| value > 0),
                    "{sent}"
                );
            }
            assert!(sent.to_string().contains("ping"), "{sent}");
            assert!(
                sent["tools"]
                    .as_array()
                    .is_some_and(|tools| !tools.is_empty()),
                "{sent}"
            );
            assert!(sent["tools"].to_string().contains("lookup"), "{sent}");
        }
        drop(calls);
        harness.stop();
    }
}

fn cas(harness: &V3Harness, patch: Value) -> Value {
    let mut body = patch.as_object().cloned().unwrap_or_default();
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

#[tokio::test]
async fn preset_provenance_survives_save_edit_and_can_be_cleared() {
    let harness = start_loopback("dyn-preset-provenance").await;
    let mut draft = create_body(
        "Renamed Azure",
        "https://resource.example/openai/v1/responses",
        "responses",
        "bearer",
        Some("sk-placeholder"),
    );
    draft["presetId"] = json!("azure-openai");
    let (status, created) = send_json(
        &harness,
        Method::POST,
        "/providers",
        &cas(&harness, draft.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    assert_eq!(created["provider"]["presetId"], "azure-openai");
    let id = created["provider"]["id"].as_str().unwrap();
    let (_, accounts) = send_json(&harness, Method::GET, "/accounts", &Value::Null).await;
    let account = accounts["accounts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|account| account["providerId"] == id)
        .unwrap();
    assert_eq!(account["purchaseDate"], "");
    assert_eq!(account["expiresOn"], "");
    let (_, second) = send_json(
        &harness,
        Method::POST,
        "/accounts",
        &cas(
            &harness,
            json!({"providerId":id,"name":"Second Azure Key","key":"sk-second"}),
        ),
    )
    .await;
    assert_eq!(second["account"]["purchaseDate"], "");
    assert_eq!(second["account"]["expiresOn"], "");
    let path = format!("/providers/{id}");
    let (status, loaded) = send_json(&harness, Method::GET, &path, &Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{loaded}");
    assert_eq!(loaded["presetId"], "azure-openai");
    assert_eq!(
        harness
            .state
            .db
            .lock()
            .get_dynamic_provider(id)
            .unwrap()
            .unwrap()
            .preset_id
            .as_deref(),
        Some("azure-openai")
    );
    draft.as_object_mut().unwrap().remove("key");
    draft.as_object_mut().unwrap().remove("presetId");
    draft["name"] = json!("Still Azure");
    let (status, edited) = send_json(
        &harness,
        Method::PATCH,
        &path,
        &cas(&harness, draft.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{edited}");
    assert_eq!(
        edited["provider"]["presetId"], "azure-openai",
        "legacy callers preserve provenance"
    );
    draft["presetId"] = json!("bad/id");
    let before = harness.state.settings_revision();
    let (status, _) = send_json(
        &harness,
        Method::PATCH,
        &path,
        &cas(&harness, draft.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(harness.state.settings_revision(), before);
    draft["presetId"] = json!("");
    let (status, cleared) = send_json(&harness, Method::PATCH, &path, &cas(&harness, draft)).await;
    assert_eq!(status, StatusCode::OK, "{cleared}");
    assert!(cleared["provider"].get("presetId").is_none());
    harness.stop();
}

#[tokio::test]
async fn draft_protocol_probe_rejects_wrong_shapes_and_error_objects_without_saving() {
    let harness = start_loopback("dyn-probe-false-success").await;
    let mut config = harness.state.config();
    config.proxy_mode = ProxyMode::Direct;
    harness.state.set_config(config).unwrap();
    let before = harness.state.settings_revision();
    for (protocol, body) in [
        ("chat_completions", r#"{"ok":true}"#),
        ("responses", CHAT_OK),
        ("responses", r#"{"output":[]}"#),
        ("messages", CHAT_OK),
        (
            "chat_completions",
            r#"{"error":{"message":"sk-secret-do-not-echo"}}"#,
        ),
        ("responses", r#"{"status":"failed","output":[]}"#),
    ] {
        let (upstream, calls, _stop) = start_fake_upstream(HashMap::from([(
            "sk-secret-do-not-echo".into(),
            VecDeque::from([FakeReply { status: 200, body }]),
        )]))
        .await;
        let (status, tested) = send_json(&harness, Method::POST, "/providers/test", &json!({
            "endpointUrl": upstream, "upstreamProtocol": protocol, "authKind":"bearer", "key":"sk-secret-do-not-echo",
            "publicModel":"lab-opus", "upstreamModel":"vendor/opus"
        })).await;
        assert_eq!(status, StatusCode::OK, "{tested}");
        assert_eq!(tested["ok"], false, "{tested}");
        assert!(tested["error"].is_string());
        assert!(!tested.to_string().contains("sk-secret-do-not-echo"));
        assert_eq!(calls.lock().unwrap().len(), 1);
        assert_eq!(harness.state.settings_revision(), before);
    }
    harness.stop();
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
    let parsed = response.json().await.unwrap_or(Value::Null);
    (status, parsed)
}

fn create_body(name: &str, endpoint: &str, protocol: &str, auth: &str, key: Option<&str>) -> Value {
    let mut body = json!({
        "name": name,
        "endpointUrl": endpoint,
        "upstreamProtocol": protocol,
        "authKind": auth,
        "models": [{
            "publicModel": "lab-opus",
            "upstreamModel": "vendor/opus"
        }]
    });
    if let Some(key) = key {
        body["key"] = json!(key);
    }
    body
}

async fn chat_completion(harness: &V3Harness, model: &str) -> (StatusCode, String) {
    let response = harness
        .client
        .post(format!(
            "http://127.0.0.1:{}/v1/chat/completions",
            harness.handle.port
        ))
        .header(
            reqwest::header::AUTHORIZATION,
            format!("Bearer {}", harness.state.config().gateway_key),
        )
        .json(&json!({
            "model": model,
            "messages": [{"role": "user", "content": "ping"}],
            "max_tokens": 1
        }))
        .send()
        .await
        .unwrap();
    (response.status(), response.text().await.unwrap())
}

async fn listed_gateway_model_ids(harness: &V3Harness) -> Vec<String> {
    let models = harness
        .client
        .get(format!(
            "http://127.0.0.1:{}/v1/models",
            harness.handle.port
        ))
        .header(
            reqwest::header::AUTHORIZATION,
            format!("Bearer {}", harness.state.config().gateway_key),
        )
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();
    models["data"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|item| item["id"].as_str().map(str::to_string))
        .collect()
}

#[tokio::test]
async fn create_patch_delete_and_cas_conflict() {
    let harness = start_loopback("dyn-cas").await;
    let (status, created) = send_json(
        &harness,
        Method::POST,
        "/providers",
        &cas(
            &harness,
            create_body(
                "Lab",
                "http://127.0.0.1:9",
                "chat_completions",
                "bearer",
                Some("sk-lab"),
            ),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let provider_id = created["provider"]["id"].as_str().unwrap().to_string();
    assert!(created["provider"].get("key").is_none());
    let revision = created["revision"].as_u64().unwrap();

    let (status, conflict) = send_json(
        &harness,
        Method::PATCH,
        &format!("/providers/{provider_id}"),
        &json!({
            "expectedRevision": revision.saturating_sub(1),
            "processGeneration": harness.state.process_generation(),
            "name": "Hacked",
            "endpointUrl": "http://127.0.0.1:9",
            "upstreamProtocol": "chat_completions",
            "authKind": "bearer",
            "models": [{"publicModel": "lab-opus", "upstreamModel": "vendor/opus"}]
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{conflict}");
    let stored = harness
        .state
        .db
        .lock()
        .get_dynamic_provider(&provider_id)
        .unwrap()
        .unwrap();
    assert_eq!(stored.name, "Lab");

    let (status, deleted) = send_json(
        &harness,
        Method::DELETE,
        &format!("/providers/{provider_id}"),
        &cas(&harness, json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{deleted}");

    let accounts = harness.state.db.lock().list_accounts().unwrap();
    let account_id = accounts
        .iter()
        .find(|account| account.provider_id == provider_id)
        .unwrap()
        .id
        .clone();
    harness.state.db.lock().delete_account(&account_id).unwrap();
    let (status, ack) = send_json(
        &harness,
        Method::DELETE,
        &format!("/providers/{provider_id}"),
        &cas(&harness, json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{ack}");
    assert!(
        harness
            .state
            .db
            .lock()
            .get_dynamic_provider(&provider_id)
            .unwrap()
            .is_none()
    );

    let (status, builtin) = send_json(
        &harness,
        Method::DELETE,
        &format!("/providers/{OPENCODE_PROVIDER_ID}"),
        &cas(&harness, json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{builtin}");
    harness.stop();
}

#[tokio::test]
async fn protocols_auth_kinds_and_no_auth_singleton() {
    let harness = start_loopback("dyn-kinds").await;
    for (protocol, auth, key) in [
        ("chat_completions", "bearer", Some("sk-a")),
        ("responses", "x-api-key", Some("sk-b")),
        ("messages", "none", None),
    ] {
        let (status, body) = send_json(
            &harness,
            Method::POST,
            "/providers",
            &cas(
                &harness,
                create_body(
                    &format!("{protocol}-{auth}"),
                    "http://127.0.0.1:9",
                    protocol,
                    auth,
                    key,
                ),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        if auth == "none" {
            let provider_id = body["provider"]["id"].as_str().unwrap().to_string();
            let (status, second) = send_json(
                &harness,
                Method::POST,
                "/accounts",
                &cas(
                    &harness,
                    json!({
                        "name": "second",
                        "providerId": provider_id,
                        "key": "sk-should-fail"
                    }),
                ),
            )
            .await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{second}");
        }
    }
    harness.stop();
}

#[tokio::test]
async fn two_keyed_accounts_select_and_fallback() {
    let mut replies = HashMap::new();
    replies.insert(
        "sk-first".to_string(),
        VecDeque::from([FakeReply {
            status: 401,
            body: r#"{"error":{"message":"bad"}}"#,
        }]),
    );
    replies.insert(
        "sk-second".to_string(),
        VecDeque::from([FakeReply {
            status: 200,
            body: CHAT_OK,
        }]),
    );
    let (upstream, _calls, _stop) = start_fake_upstream(replies).await;
    let harness = start_loopback("dyn-fallback").await;
    let mut config = harness.state.config();
    config.proxy_mode = ProxyMode::Direct;
    harness.state.set_config(config).unwrap();
    let (status, created) = send_json(
        &harness,
        Method::POST,
        "/providers",
        &cas(
            &harness,
            create_body(
                "Fallback",
                &format!("{upstream}/v1"),
                "chat_completions",
                "bearer",
                Some("sk-first"),
            ),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let provider_id = created["provider"]["id"].as_str().unwrap().to_string();
    let (status, second) = send_json(
        &harness,
        Method::POST,
        "/accounts",
        &cas(
            &harness,
            json!({
                "name": "second",
                "providerId": provider_id,
                "key": "sk-second"
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{second}");
    let second_account_id = second["account"]["id"].as_str().unwrap().to_string();
    enable_accounts_for_provider(&harness, &provider_id);
    let (gw_status, body) = chat_completion(&harness, "lab-opus").await;
    assert_eq!(gw_status, StatusCode::OK, "{body}");
    let logs = harness.state.db.lock().list_forward_logs(8).unwrap();
    let attributed: Vec<_> = logs
        .iter()
        .filter(|row| row.provider_id.as_deref() == Some(provider_id.as_str()))
        .collect();
    assert!(
        attributed.iter().any(|row| {
            !row.account_id.is_empty()
                && (row.http_status == Some(401) || row.status.contains("error"))
        }),
        "expected first-account failure log: {logs:?}"
    );
    assert!(
        attributed.iter().any(|row| {
            !row.account_id.is_empty()
                && (row.http_status == Some(200) || row.status.contains("success"))
        }),
        "expected fallback success log: {logs:?}"
    );
    let account_ids: HashSet<_> = attributed
        .iter()
        .map(|row| row.account_id.as_str())
        .filter(|id| !id.is_empty())
        .collect();
    assert!(
        account_ids.len() >= 2 && account_ids.contains(second_account_id.as_str()),
        "expected log attribution across accounts, second={second_account_id}, logs={logs:?}"
    );
    harness.stop();
}

#[tokio::test]
async fn none_auth_dynamic_provider_forwards_without_upstream_auth() {
    let mut replies = HashMap::new();
    replies.insert(
        String::new(),
        VecDeque::from([FakeReply {
            status: 200,
            body: CHAT_OK,
        }]),
    );
    let (upstream, calls, _stop) = start_fake_upstream(replies).await;
    let harness = start_loopback("dyn-none-auth-forward").await;
    let mut config = harness.state.config();
    config.proxy_mode = ProxyMode::Direct;
    harness.state.set_config(config).unwrap();
    let (status, created) = send_json(
        &harness,
        Method::POST,
        "/providers",
        &cas(
            &harness,
            create_body(
                "OpenLab",
                &format!("{upstream}/v1"),
                "chat_completions",
                "none",
                None,
            ),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let provider_id = created["provider"]["id"].as_str().unwrap().to_string();
    enable_accounts_for_provider(&harness, &provider_id);

    let response = harness
        .client
        .post(format!(
            "http://127.0.0.1:{}/v1/chat/completions",
            harness.handle.port
        ))
        .header(
            reqwest::header::AUTHORIZATION,
            format!("Bearer {}", harness.state.config().gateway_key),
        )
        .header("x-opencode-session", "ses_client")
        .header("x-opencode-client", "cli")
        .header("x-opencode-request", "req_client")
        .header("x-opencode-project", "proj_client")
        .header("x-session-id", "ses_id")
        .header("x-session-affinity", "ses_aff")
        .json(&json!({
            "model": "lab-opus",
            "messages": [{"role": "user", "content": "ping"}],
            "max_tokens": 1
        }))
        .send()
        .await
        .unwrap();
    let status = response.status();
    let body = response.text().await.unwrap();
    assert_eq!(status, StatusCode::OK, "{body}");

    let captured = calls.lock().expect("fake call log").clone();
    assert_eq!(captured.len(), 1, "{captured:?}");
    let call = &captured[0];
    assert!(call.authorization.is_none(), "{call:?}");
    assert!(call.x_api_key.is_none(), "{call:?}");
    assert!(call.x_goog_api_key.is_none(), "{call:?}");
    assert!(call.key.is_empty(), "{call:?}");
    assert!(call.opencode_session.is_none(), "{call:?}");
    assert!(call.opencode_client.is_none(), "{call:?}");
    assert!(call.opencode_request.is_none(), "{call:?}");
    assert!(call.opencode_project.is_none(), "{call:?}");
    assert!(call.session_id.is_none(), "{call:?}");
    assert!(call.session_affinity.is_none(), "{call:?}");
    harness.stop();
}

#[tokio::test]
async fn raw_ambiguity_makes_zero_outbound_requests() {
    let (upstream, calls, _stop) = start_fake_upstream(HashMap::new()).await;
    let harness = start_loopback("dyn-ambiguous").await;
    let mut config = harness.state.config();
    config.proxy_mode = ProxyMode::Direct;
    harness.state.set_config(config).unwrap();
    for name in ["A", "B"] {
        let (status, created) = send_json(
            &harness,
            Method::POST,
            "/providers",
            &cas(
                &harness,
                json!({
                    "name": name,
                    "endpointUrl": format!("{upstream}/v1"),
                    "upstreamProtocol": "chat_completions",
                    "authKind": "bearer",
                    "key": format!("sk-{name}"),
                    "models": [{"publicModel": format!("{name}-pub"), "upstreamModel": "shared/raw"}]
                }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{created}");
    }
    let (status, body) = chat_completion(&harness, "shared/raw").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let parsed: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
    assert_eq!(parsed["error"]["type"], "ambiguous_model_id", "{body}");
    assert!(calls.lock().expect("fake call log").is_empty());
    let ids = listed_gateway_model_ids(&harness).await;
    assert!(
        !ids.iter().any(|id| id == "shared/raw"),
        "ambiguous raw id must stay unpublished: {ids:?}"
    );
    harness.stop();
}

#[tokio::test]
async fn raw_shaped_public_models_are_listed_under_public_name_only() {
    let mut replies = HashMap::new();
    replies.insert(
        "sk-lab".to_string(),
        VecDeque::from([FakeReply {
            status: 200,
            body: CHAT_OK,
        }]),
    );
    let (upstream, calls, _stop) = start_fake_upstream(replies).await;
    let harness = start_loopback("dyn-raw-public").await;
    let mut config = harness.state.config();
    config.proxy_mode = ProxyMode::Direct;
    harness.state.set_config(config).unwrap();
    let (status, created) = send_json(
        &harness,
        Method::POST,
        "/providers",
        &cas(
            &harness,
            json!({
                "name": "RawLab",
                "endpointUrl": format!("{upstream}/v1"),
                "upstreamProtocol": "chat_completions",
                "authKind": "bearer",
                "key": "sk-lab",
                "models": [
                    {"publicModel": "org/same", "upstreamModel": "org/same"},
                    {"publicModel": "org/public", "upstreamModel": "vendor/real"},
                    {"publicModel": "lab_model", "upstreamModel": "vendor/lab"},
                    {"publicModel": "lab model", "upstreamModel": "vendor/space"}
                ]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let provider_id = created["provider"]["id"].as_str().unwrap().to_string();
    enable_accounts_for_provider(&harness, &provider_id);

    let ids = listed_gateway_model_ids(&harness).await;
    for public in ["org/same", "org/public", "lab_model", "lab model"] {
        assert_eq!(
            ids.iter().filter(|id| **id == public).count(),
            1,
            "{public} missing or duplicated in {ids:?}"
        );
    }
    for leaked in ["vendor/real", "vendor/lab", "vendor/space"] {
        assert!(
            !ids.iter().any(|id| id == leaked),
            "differing upstream leaked into /v1/models: {leaked} in {ids:?}"
        );
    }

    let (status, body) = chat_completion(&harness, "org/public").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    {
        let outbound = calls.lock().expect("fake call log");
        assert_eq!(outbound.len(), 1, "{outbound:?}");
        assert!(
            outbound[0].body.contains("\"model\":\"vendor/real\""),
            "public!=upstream must forward the exact upstream id: {}",
            outbound[0].body
        );
        assert!(
            !outbound[0].body.contains("\"model\":\"org/public\""),
            "public name must not replace the upstream id: {}",
            outbound[0].body
        );
    }

    let (status, same_body) = chat_completion(&harness, "org/same").await;
    assert_eq!(status, StatusCode::OK, "{same_body}");
    harness.stop();
}

#[tokio::test]
async fn public_alias_aggregates_across_dynamic_providers() {
    let mut replies = HashMap::new();
    replies.insert(
        "sk-one".to_string(),
        VecDeque::from([FakeReply {
            status: 401,
            body: r#"{"error":{"message":"bad"}}"#,
        }]),
    );
    replies.insert(
        "sk-two".to_string(),
        VecDeque::from([FakeReply {
            status: 200,
            body: CHAT_OK,
        }]),
    );
    let (upstream, _calls, _stop) = start_fake_upstream(replies).await;
    let harness = start_loopback("dyn-aggregate").await;
    let mut config = harness.state.config();
    config.proxy_mode = ProxyMode::Direct;
    harness.state.set_config(config).unwrap();
    for (name, key) in [("One", "sk-one"), ("Two", "sk-two")] {
        let (status, created) = send_json(
            &harness,
            Method::POST,
            "/providers",
            &cas(
                &harness,
                json!({
                    "name": name,
                    "endpointUrl": format!("{upstream}/v1"),
                    "upstreamProtocol": "chat_completions",
                    "authKind": "bearer",
                    "key": key,
                    "models": [{"publicModel": "shared-opus", "upstreamModel": format!("vendor/{name}")}]
                }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{created}");
        let provider_id = created["provider"]["id"].as_str().unwrap().to_string();
        enable_accounts_for_provider(&harness, &provider_id);
    }
    let models = harness
        .client
        .get(format!(
            "http://127.0.0.1:{}/v1/models",
            harness.handle.port
        ))
        .header(
            reqwest::header::AUTHORIZATION,
            format!("Bearer {}", harness.state.config().gateway_key),
        )
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();
    let ids = models["data"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|item| item["id"].as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        ids.iter().filter(|id| **id == "shared-opus").count(),
        1,
        "{models}"
    );
    let (status, body) = chat_completion(&harness, "shared-opus").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    harness.stop();
}

#[tokio::test]
async fn discover_and_test_do_not_persist_keys_or_providers() {
    let mut replies = HashMap::new();
    replies.insert(
        "sk-probe".to_string(),
        VecDeque::from([
            FakeReply {
                status: 200,
                body: r#"{"data":[{"id":"discovered-1"}]}"#,
            },
            FakeReply {
                status: 200,
                body: CHAT_OK,
            },
        ]),
    );
    let (upstream, _calls, _stop) = start_fake_upstream(replies).await;
    let harness = start_loopback("dyn-probe").await;
    let mut config = harness.state.config();
    config.proxy_mode = ProxyMode::Direct;
    harness.state.set_config(config).unwrap();
    let before = harness
        .state
        .db
        .lock()
        .list_dynamic_providers()
        .unwrap()
        .len();
    let (status, discovered) = send_json(
        &harness,
        Method::POST,
        "/providers/models/discover",
        &json!({
            "endpointUrl": format!("{upstream}/v1"),
            "upstreamProtocol": "chat_completions",
            "authKind": "bearer",
            "key": "sk-probe"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{discovered}");
    assert!(discovered.get("key").is_none());
    assert!(!discovered.to_string().contains("sk-probe"));
    let (status, tested) = send_json(
        &harness,
        Method::POST,
        "/providers/test",
        &json!({
            "endpointUrl": format!("{upstream}/v1"),
            "upstreamProtocol": "chat_completions",
            "authKind": "bearer",
            "publicModel": "lab-opus",
            "upstreamModel": "vendor/opus",
            "key": "sk-probe"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{tested}");
    assert!(tested.get("key").is_none());
    assert!(!tested.to_string().contains("sk-probe"));
    assert_eq!(
        harness
            .state
            .db
            .lock()
            .list_dynamic_providers()
            .unwrap()
            .len(),
        before
    );
    harness.stop();
}

#[tokio::test]
async fn patch_clears_runtime_state_and_usage_is_unpriced() {
    let harness = start_loopback("dyn-patch").await;
    let (status, created) = send_json(
        &harness,
        Method::POST,
        "/providers",
        &cas(
            &harness,
            create_body(
                "PatchMe",
                "http://127.0.0.1:9",
                "chat_completions",
                "bearer",
                Some("sk-keep"),
            ),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let provider_id = created["provider"]["id"].as_str().unwrap().to_string();
    let account_id = harness
        .state
        .db
        .lock()
        .list_accounts()
        .unwrap()
        .into_iter()
        .find(|account| account.provider_id == provider_id)
        .unwrap()
        .id;
    {
        let db = harness.state.db.lock();
        db.set_account_auth_error(&account_id, Some("stale"))
            .unwrap();
        db.set_account_cooldown(
            &account_id,
            Some(Utc::now() + chrono::Duration::hours(1)),
            Some("boom"),
        )
        .unwrap();
    }
    let (status, renamed) = send_json(
        &harness,
        Method::PATCH,
        &format!("/providers/{provider_id}"),
        &cas(
            &harness,
            json!({
                "name": "Renamed",
                "endpointUrl": "http://127.0.0.1:9",
                "upstreamProtocol": "chat_completions",
                "authKind": "bearer",
                "models": [{"publicModel": "lab-opus", "upstreamModel": "vendor/opus"}]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{renamed}");
    let account = harness
        .state
        .db
        .lock()
        .get_account(&account_id)
        .unwrap()
        .unwrap();
    assert_eq!(account.auth_error.as_deref(), Some("stale"));
    assert_eq!(account.last_error.as_deref(), Some("boom"));
    assert!(account.cooldown_until.is_some());

    let (status, patched) = send_json(
        &harness,
        Method::PATCH,
        &format!("/providers/{provider_id}"),
        &cas(
            &harness,
            json!({
                "name": "Renamed",
                "endpointUrl": "http://127.0.0.1:10",
                "upstreamProtocol": "responses",
                "authKind": "bearer",
                "models": [{"publicModel": "lab-opus", "upstreamModel": "vendor/opus-2"}]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{patched}");
    let account = harness
        .state
        .db
        .lock()
        .get_account(&account_id)
        .unwrap()
        .unwrap();
    assert!(account.auth_error.is_none());
    assert!(account.last_error.is_none());
    assert!(account.cooldown_until.is_none());
    assert!(!account.key_cipher.is_empty());

    let (status, usage) = harness
        .get_json(&format!(
            "{}/accounts/{account_id}/provider-usage",
            harness.v3_base
        ))
        .await;
    assert_eq!(status, StatusCode::OK, "{usage}");
    assert_eq!(usage["availability"], "unavailable");
    let (status, pricing) = harness
        .get_json(&format!(
            "{}/providers/{provider_id}/pricing",
            harness.v3_base
        ))
        .await;
    assert_eq!(status, StatusCode::OK, "{pricing}");
    assert_eq!(pricing["availability"], "unpriced");
    harness.stop();
}

#[tokio::test]
async fn none_to_keyed_requires_replacement_key() {
    let harness = start_loopback("dyn-auth-swap").await;
    let (status, created) = send_json(
        &harness,
        Method::POST,
        "/providers",
        &cas(
            &harness,
            create_body(
                "OpenLab",
                "http://127.0.0.1:9",
                "chat_completions",
                "none",
                None,
            ),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let provider_id = created["provider"]["id"].as_str().unwrap().to_string();
    let (status, missing) = send_json(
        &harness,
        Method::PATCH,
        &format!("/providers/{provider_id}"),
        &cas(
            &harness,
            json!({
                "name": "OpenLab",
                "endpointUrl": "http://127.0.0.1:9",
                "upstreamProtocol": "chat_completions",
                "authKind": "bearer",
                "models": [{"publicModel": "lab-opus", "upstreamModel": "vendor/opus"}]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{missing}");
    let (status, patched) = send_json(
        &harness,
        Method::PATCH,
        &format!("/providers/{provider_id}"),
        &cas(
            &harness,
            json!({
                "name": "OpenLab",
                "endpointUrl": "http://127.0.0.1:9",
                "upstreamProtocol": "chat_completions",
                "authKind": "bearer",
                "key": "sk-now",
                "models": [{"publicModel": "lab-opus", "upstreamModel": "vendor/opus"}]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{patched}");
    let account = harness
        .state
        .db
        .lock()
        .list_accounts()
        .unwrap()
        .into_iter()
        .find(|account| account.provider_id == provider_id)
        .unwrap();
    assert!(!account.key_cipher.is_empty());
    harness.stop();
}

#[tokio::test]
async fn keyed_provider_update_rejects_key_and_does_not_fan_out() {
    let harness = start_loopback("dyn-no-key-fanout").await;
    let (status, created) = send_json(
        &harness,
        Method::POST,
        "/providers",
        &cas(
            &harness,
            create_body(
                "Keys",
                "http://127.0.0.1:9",
                "chat_completions",
                "bearer",
                Some("sk-first"),
            ),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let provider_id = created["provider"]["id"].as_str().unwrap().to_string();
    let (status, second) = send_json(
        &harness,
        Method::POST,
        "/accounts",
        &cas(
            &harness,
            json!({
                "name": "second",
                "providerId": provider_id,
                "key": "sk-second"
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{second}");
    let before = harness
        .state
        .db
        .lock()
        .list_accounts()
        .unwrap()
        .into_iter()
        .filter(|account| account.provider_id == provider_id)
        .map(|account| (account.id, account.key_cipher))
        .collect::<Vec<_>>();
    assert_eq!(before.len(), 2);
    let revision = harness.state.settings_revision();
    let (status, rejected) = send_json(
        &harness,
        Method::PATCH,
        &format!("/providers/{provider_id}"),
        &cas(
            &harness,
            json!({
                "name": "Keys",
                "endpointUrl": "http://127.0.0.1:9",
                "upstreamProtocol": "chat_completions",
                "authKind": "bearer",
                "key": "sk-fanout",
                "models": [{"publicModel": "lab-opus", "upstreamModel": "vendor/opus"}]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{rejected}");
    assert_eq!(rejected["code"], ERROR_INVALID_REQUEST, "{rejected}");
    assert_eq!(harness.state.settings_revision(), revision);
    let after = harness
        .state
        .db
        .lock()
        .list_accounts()
        .unwrap()
        .into_iter()
        .filter(|account| account.provider_id == provider_id)
        .map(|account| (account.id, account.key_cipher))
        .collect::<Vec<_>>();
    assert_eq!(after, before);
    let first = harness.state.decrypt_key(&before[0].1).unwrap();
    let second_key = harness.state.decrypt_key(&before[1].1).unwrap();
    assert_ne!(first, second_key);
    assert!(first == "sk-first" || second_key == "sk-first");
    assert!(first == "sk-second" || second_key == "sk-second");
    harness.stop();
}

#[tokio::test]
async fn in_flight_fallback_stops_after_provider_destination_changes() {
    let mut replies = HashMap::new();
    replies.insert(
        "sk-first".to_string(),
        VecDeque::from([FakeReply {
            status: 401,
            body: r#"{"error":{"message":"bad"}}"#,
        }]),
    );
    replies.insert(
        "sk-second".to_string(),
        VecDeque::from([FakeReply {
            status: 200,
            body: CHAT_OK,
        }]),
    );
    let (upstream, calls, _stop) =
        start_fake_upstream_with_delay(replies, Duration::from_millis(400)).await;
    let harness = start_loopback("dyn-snapshot").await;
    let mut config = harness.state.config();
    config.proxy_mode = ProxyMode::Direct;
    harness.state.set_config(config).unwrap();
    let (status, created) = send_json(
        &harness,
        Method::POST,
        "/providers",
        &cas(
            &harness,
            create_body(
                "Snap",
                &format!("{upstream}/v1"),
                "chat_completions",
                "bearer",
                Some("sk-first"),
            ),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let provider_id = created["provider"]["id"].as_str().unwrap().to_string();
    let (status, second) = send_json(
        &harness,
        Method::POST,
        "/accounts",
        &cas(
            &harness,
            json!({
                "name": "second",
                "providerId": provider_id,
                "key": "sk-second"
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{second}");
    enable_accounts_for_provider(&harness, &provider_id);

    let client = harness.client.clone();
    let port = harness.handle.port;
    let gateway_key = harness.state.config().gateway_key.clone();
    let pending = tokio::spawn(async move {
        client
            .post(format!("http://127.0.0.1:{port}/v1/chat/completions"))
            .header(
                reqwest::header::AUTHORIZATION,
                format!("Bearer {gateway_key}"),
            )
            .json(&json!({
                "model": "lab-opus",
                "messages": [{"role": "user", "content": "ping"}],
                "max_tokens": 1
            }))
            .send()
            .await
            .unwrap()
    });
    let started = tokio::time::Instant::now();
    loop {
        if !calls.lock().expect("fake call log").is_empty() {
            break;
        }
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "in-flight request never reached the original upstream"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let (status, patched) = send_json(
        &harness,
        Method::PATCH,
        &format!("/providers/{provider_id}"),
        &cas(
            &harness,
            json!({
                "name": "Snap",
                "endpointUrl": "http://127.0.0.1:1",
                "upstreamProtocol": "chat_completions",
                "authKind": "bearer",
                "models": [{"publicModel": "lab-opus", "upstreamModel": "vendor/opus"}]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{patched}");
    let response = pending.await.unwrap();
    let status = response.status();
    let body = response.text().await.unwrap();
    // Materialization remains frozen, but a later attempt must still pass
    // the live destination gate. It cannot send to the old destination or
    // silently retarget the captured request to the newly edited address.
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert!(
        body.contains("destination is not the current granted route"),
        "{body}"
    );
    assert_eq!(calls.lock().expect("fake call log").len(), 1);
    harness.stop();
}

#[tokio::test]
async fn custom_api_still_creates_account_owned_endpoint() {
    let harness = start_loopback("dyn-custom-regression").await;
    let (status, created) = send_json(
        &harness,
        Method::POST,
        "/accounts",
        &cas(
            &harness,
            json!({
                "name": "Custom",
                "providerId": CUSTOM_PROVIDER_ID,
                "key": "sk-custom",
                "customConfig": {
                    "endpointUrl": "http://127.0.0.1:9",
                    "upstreamProtocol": "chat_completions"
                },
                "modelCapabilities": [{
                    "publicModel": "home-model",
                    "upstreamModel": "home-model",
                    "protocol": "chat_completions"
                }]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    assert_eq!(created["account"]["providerId"], CUSTOM_PROVIDER_ID);
    let account_id = created["account"]["id"].as_str().unwrap();
    let custom = harness
        .state
        .db
        .lock()
        .account_custom_config(account_id)
        .unwrap()
        .unwrap();
    assert_eq!(custom.endpoint_url, "http://127.0.0.1:9");
    harness.stop();
}

#[tokio::test]
async fn keyed_create_without_a_key_saves_the_definition_only() {
    let harness = start_loopback("dyn-create-no-key").await;
    let (status, created) = send_json(
        &harness,
        Method::POST,
        "/providers",
        &cas(
            &harness,
            create_body(
                "Tencent Lab",
                "http://127.0.0.1:9",
                "chat_completions",
                "bearer",
                None,
            ),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let provider_id = created["provider"]["id"].as_str().unwrap().to_string();
    assert_eq!(created["provider"]["name"], "Tencent Lab");
    assert!(created["provider"].get("key").is_none());
    let accounts = harness.state.db.lock().list_accounts().unwrap();
    assert!(
        accounts
            .iter()
            .all(|account| account.provider_id != provider_id),
        "keyed create without a Key must not insert an account: {accounts:?}"
    );
    let (status, second) = send_json(
        &harness,
        Method::POST,
        "/accounts",
        &cas(
            &harness,
            json!({"providerId": provider_id, "name": "First Key", "key": "sk-later"}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{second}");
    assert_eq!(second["account"]["providerId"], provider_id);
    assert_eq!(second["account"]["name"], "First Key");
    harness.stop();
}

#[tokio::test]
async fn deleting_the_last_account_keeps_the_dynamic_provider_definition() {
    let harness = start_loopback("dyn-keep-after-delete").await;
    let endpoint = "http://127.0.0.1:9";
    let (status, created) = send_json(
        &harness,
        Method::POST,
        "/providers",
        &cas(
            &harness,
            create_body(
                "KeepMe",
                endpoint,
                "chat_completions",
                "bearer",
                Some("sk-keep"),
            ),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let provider_id = created["provider"]["id"].as_str().unwrap().to_string();
    assert_eq!(created["provider"]["endpointUrl"], endpoint);
    let account_id = account_id_for_provider(&harness, &provider_id).await;

    let (status, deleted) = send_json(
        &harness,
        Method::DELETE,
        &format!("/accounts/{account_id}"),
        &cas(&harness, json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{deleted}");
    assert!(
        harness
            .state
            .db
            .lock()
            .list_accounts()
            .unwrap()
            .iter()
            .all(|account| account.provider_id != provider_id)
    );

    let (status, loaded) = send_json(
        &harness,
        Method::GET,
        &format!("/providers/{provider_id}"),
        &Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{loaded}");
    assert_eq!(loaded["endpointUrl"], endpoint);
    assert_eq!(loaded["models"][0]["publicModel"], "lab-opus");
    assert_eq!(loaded["models"][0]["upstreamModel"], "vendor/opus");

    let (status, catalog) = send_json(&harness, Method::GET, "/providers", &Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{catalog}");
    assert!(
        catalog["entries"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["providerId"] == provider_id),
        "catalog must still list the definition: {catalog}"
    );

    let (status, second) = send_json(
        &harness,
        Method::POST,
        "/accounts",
        &cas(
            &harness,
            json!({
                "providerId": provider_id,
                "name": "Replacement Key",
                "key": "sk-later"
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{second}");
    assert_eq!(second["account"]["providerId"], provider_id);
    harness.stop();
}

fn enable_accounts_for_provider(harness: &V3Harness, provider_id: &str) {
    let ids: Vec<String> = harness
        .state
        .db
        .lock()
        .list_accounts()
        .unwrap()
        .into_iter()
        .filter(|account| account.provider_id == provider_id)
        .map(|account| account.id)
        .collect();
    for id in ids {
        harness.enable_account(&id);
    }
}

async fn account_id_for_provider(harness: &V3Harness, provider_id: &str) -> String {
    harness
        .state
        .db
        .lock()
        .list_accounts()
        .unwrap()
        .into_iter()
        .find(|account| account.provider_id == provider_id)
        .map(|account| account.id)
        .unwrap_or_else(|| panic!("missing account for {provider_id}"))
}

async fn toggle_account(harness: &V3Harness, account_id: &str) -> (StatusCode, Value) {
    send_json(
        harness,
        Method::POST,
        &format!("/accounts/{account_id}/toggle"),
        &cas(harness, json!({})),
    )
    .await
}

#[tokio::test]
async fn keyed_dynamic_account_can_disable_and_re_enable() {
    let harness = start_loopback("dyn-enable-keyed").await;
    let (status, created) = send_json(
        &harness,
        Method::POST,
        "/providers",
        &cas(
            &harness,
            create_body(
                "Lab",
                "http://127.0.0.1:9",
                "chat_completions",
                "bearer",
                Some("sk-lab"),
            ),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let provider_id = created["provider"]["id"].as_str().unwrap().to_string();
    let account_id = account_id_for_provider(&harness, &provider_id).await;
    let before = harness
        .state
        .db
        .lock()
        .get_account(&account_id)
        .unwrap()
        .unwrap();
    assert!(!before.enabled);
    let (status, enabled) = toggle_account(&harness, &account_id).await;
    assert_eq!(status, StatusCode::OK, "{enabled}");
    assert_eq!(enabled["account"]["enabled"], true);
    let mut revision = harness.state.settings_revision();

    let (status, disabled) = toggle_account(&harness, &account_id).await;
    assert_eq!(status, StatusCode::OK, "{disabled}");
    assert_eq!(disabled["account"]["enabled"], false);
    assert_eq!(harness.state.settings_revision(), revision + 1);
    revision = harness.state.settings_revision();
    let after_disable = harness
        .state
        .db
        .lock()
        .get_account(&account_id)
        .unwrap()
        .unwrap();
    assert_eq!(after_disable.provider_id, before.provider_id);
    assert_eq!(after_disable.credential_kind, before.credential_kind);
    assert_eq!(after_disable.key_cipher, before.key_cipher);

    let (status, enabled) = toggle_account(&harness, &account_id).await;
    assert_eq!(status, StatusCode::OK, "{enabled}");
    assert_eq!(enabled["account"]["enabled"], true);
    assert_eq!(harness.state.settings_revision(), revision + 1);
    let after_enable = harness
        .state
        .db
        .lock()
        .get_account(&account_id)
        .unwrap()
        .unwrap();
    assert_eq!(after_enable.provider_id, before.provider_id);
    assert_eq!(after_enable.credential_kind, before.credential_kind);
    assert_eq!(after_enable.key_cipher, before.key_cipher);
    harness.stop();
}

#[tokio::test]
async fn dynamic_none_auth_singleton_can_disable_and_re_enable_without_a_key() {
    let harness = start_loopback("dyn-enable-none").await;
    let (status, created) = send_json(
        &harness,
        Method::POST,
        "/providers",
        &cas(
            &harness,
            create_body(
                "OpenLab",
                "http://127.0.0.1:9",
                "chat_completions",
                "none",
                None,
            ),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let provider_id = created["provider"]["id"].as_str().unwrap().to_string();
    let account_id = account_id_for_provider(&harness, &provider_id).await;
    let stored = harness
        .state
        .db
        .lock()
        .get_account(&account_id)
        .unwrap()
        .unwrap();
    assert!(!stored.enabled);
    assert!(stored.key_cipher.is_empty());
    assert_eq!(
        stored.credential_kind,
        ocg_core::provider::CredentialKind::None
    );
    let (status, enabled) = toggle_account(&harness, &account_id).await;
    assert_eq!(status, StatusCode::OK, "{enabled}");
    assert_eq!(enabled["account"]["enabled"], true);
    let mut revision = harness.state.settings_revision();

    let (status, disabled) = toggle_account(&harness, &account_id).await;
    assert_eq!(status, StatusCode::OK, "{disabled}");
    assert_eq!(disabled["account"]["enabled"], false);
    assert_eq!(harness.state.settings_revision(), revision + 1);
    revision = harness.state.settings_revision();
    let after_disable = harness
        .state
        .db
        .lock()
        .get_account(&account_id)
        .unwrap()
        .unwrap();
    assert_eq!(after_disable.provider_id, stored.provider_id);
    assert_eq!(after_disable.credential_kind, stored.credential_kind);
    assert_eq!(after_disable.key_cipher, stored.key_cipher);
    assert!(after_disable.key_cipher.is_empty());

    let (status, enabled) = toggle_account(&harness, &account_id).await;
    assert_eq!(status, StatusCode::OK, "{enabled}");
    assert_eq!(enabled["account"]["enabled"], true);
    assert_eq!(harness.state.settings_revision(), revision + 1);
    let after_enable = harness
        .state
        .db
        .lock()
        .get_account(&account_id)
        .unwrap()
        .unwrap();
    assert_eq!(after_enable.provider_id, stored.provider_id);
    assert_eq!(after_enable.credential_kind, stored.credential_kind);
    assert_eq!(after_enable.key_cipher, stored.key_cipher);
    assert!(after_enable.key_cipher.is_empty());
    harness.stop();
}

#[tokio::test]
async fn inherited_chat_and_overridden_messages_use_effective_routes() {
    let mut queue = VecDeque::new();
    for body in [
        CHAT_OK,
        CHAT_OK,
        CHAT_OK,
        dynamic_protocols::MESSAGES,
        dynamic_protocols::MESSAGES,
        dynamic_protocols::MESSAGES,
        CHAT_OK,
        dynamic_protocols::MESSAGES,
    ] {
        queue.push_back(FakeReply { status: 200, body });
    }
    let (upstream, calls, _stop) =
        start_fake_upstream(HashMap::from([("sk-override".into(), queue)])).await;
    let harness = start_loopback("dyn-effective-route").await;
    let mut config = harness.state.config();
    config.proxy_mode = ProxyMode::Direct;
    harness.state.set_config(config).unwrap();
    let chat_endpoint = format!("{upstream}/v1");
    let messages_endpoint = format!("{upstream}/anthropic/v1/messages");
    let (status, created) = send_json(
        &harness,
        Method::POST,
        "/providers",
        &cas(
            &harness,
            json!({
                "name": "Lab",
                "endpointUrl": chat_endpoint,
                "upstreamProtocol": "chat_completions",
                "authKind": "bearer",
                "key": "sk-override",
                "models": [
                    {
                        "publicModel": "lab-chat",
                        "upstreamModel": "vendor/chat"
                    },
                    {
                        "publicModel": "lab-messages",
                        "upstreamModel": "vendor/messages",
                        "upstreamOverride": {
                            "protocol": "messages",
                            "endpointUrl": messages_endpoint
                        }
                    }
                ]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let provider_id = created["provider"]["id"].as_str().unwrap().to_string();
    enable_accounts_for_provider(&harness, &provider_id);
    let models = created["provider"]["models"].as_array().unwrap();
    assert!(models[0].get("upstreamOverride").is_none(), "{created}");
    assert_eq!(models[1]["upstreamOverride"]["protocol"], "messages");
    assert_eq!(
        models[1]["upstreamOverride"]["endpointUrl"],
        messages_endpoint
    );
    let listed = listed_gateway_model_ids(&harness).await;
    assert!(listed.contains(&"lab-chat".to_string()), "{listed:?}");
    assert!(listed.contains(&"lab-messages".to_string()), "{listed:?}");

    async fn request_all_formats(harness: &V3Harness, model: &str) -> Vec<(StatusCode, String)> {
        let mut results = Vec::new();
        for client_protocol in ["chat/completions", "responses", "messages"] {
            let body = match client_protocol {
                "responses" => json!({
                    "model": model,
                    "input": "ping",
                    "store": false,
                    "max_output_tokens": 32
                }),
                "messages" => json!({
                    "model": model,
                    "messages": [{"role": "user", "content": "ping"}],
                    "max_tokens": 32
                }),
                _ => json!({
                    "model": model,
                    "messages": [{"role": "user", "content": "ping"}],
                    "max_tokens": 32
                }),
            };
            let response = harness
                .client
                .post(format!(
                    "http://127.0.0.1:{}/v1/{client_protocol}",
                    harness.handle.port
                ))
                .bearer_auth(&harness.state.config().gateway_key)
                .json(&body)
                .send()
                .await
                .unwrap();
            results.push((response.status(), response.text().await.unwrap()));
        }
        results
    }

    for (status, body) in request_all_formats(&harness, "lab-chat").await {
        assert_eq!(status, StatusCode::OK, "{body}");
    }
    for (status, body) in request_all_formats(&harness, "lab-messages").await {
        assert_eq!(status, StatusCode::OK, "{body}");
    }

    let account_id = account_id_for_provider(&harness, &provider_id).await;
    for (model, protocol) in [
        ("lab-chat", "chat_completions"),
        ("lab-messages", "messages"),
    ] {
        let (status, tested) = send_json(
            &harness,
            Method::POST,
            &format!("/accounts/{account_id}/model-tests"),
            &json!({ "modelId": model }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{tested}");
        assert_eq!(tested["success"], true, "{tested}");
        assert_eq!(tested["protocol"], protocol, "{tested}");
        assert_eq!(tested["modelId"], model);
    }

    {
        let recorded = calls.lock().unwrap();
        assert_eq!(recorded.len(), 8, "{recorded:?}");
        for call in recorded
            .iter()
            .take(3)
            .chain(recorded.iter().skip(6).take(1))
        {
            assert_eq!(call.path, "/v1/chat/completions", "{call:?}");
            assert_eq!(
                serde_json::from_str::<Value>(&call.body).unwrap()["model"],
                "vendor/chat"
            );
            assert_eq!(call.authorization.as_deref(), Some("Bearer sk-override"));
            assert!(call.x_api_key.is_none(), "{call:?}");
        }
        for call in recorded
            .iter()
            .skip(3)
            .take(3)
            .chain(recorded.iter().skip(7).take(1))
        {
            assert_eq!(call.path, "/anthropic/v1/messages", "{call:?}");
            assert_eq!(
                serde_json::from_str::<Value>(&call.body).unwrap()["model"],
                "vendor/messages"
            );
            assert_eq!(
                call.authorization.as_deref(),
                Some("Bearer sk-override"),
                "auth stays supplier-owned on a Messages override"
            );
            assert!(call.x_api_key.is_none(), "{call:?}");
            assert!(call.anthropic_version.is_some(), "{call:?}");
        }
    }

    let (status, updated) = send_json(
        &harness,
        Method::PATCH,
        &format!("/providers/{provider_id}"),
        &cas(
            &harness,
            json!({
                "name": "Lab",
                "endpointUrl": chat_endpoint,
                "upstreamProtocol": "chat_completions",
                "authKind": "bearer",
                "models": [
                    {
                        "publicModel": "lab-chat",
                        "upstreamModel": "vendor/chat"
                    },
                    {
                        "publicModel": "lab-messages",
                        "upstreamModel": "vendor/messages"
                    }
                ]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{updated}");
    let cleared = &updated["provider"]["models"][1];
    assert_eq!(cleared["publicModel"], "lab-messages");
    assert!(cleared.get("upstreamOverride").is_none(), "{updated}");
    let (status, loaded) = send_json(
        &harness,
        Method::GET,
        &format!("/providers/{provider_id}"),
        &Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{loaded}");
    assert!(
        loaded["models"][1].get("upstreamOverride").is_none(),
        "{loaded}"
    );
    harness.stop();
}

#[tokio::test]
async fn get_builtin_provider_unifies_under_provider_definition_shape() {
    let harness = start_loopback("dyn-builtin-get").await;
    let (status, body) = send_json(
        &harness,
        Method::GET,
        &format!("/providers/{OPENCODE_PROVIDER_ID}"),
        &Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["origin"], "builtin");
    assert_eq!(body["editable"], false);
    assert_eq!(body["deletable"], false);
    assert!(body["endpointUrl"].is_null(), "{body}");
    assert!(body["upstreamProtocol"].is_null(), "{body}");
    assert!(body["authKind"].is_null(), "{body}");
    assert!(body["models"].as_array().unwrap().is_empty(), "{body}");
    assert!(body["presetId"].is_null(), "{body}");
    assert_eq!(body["id"], OPENCODE_PROVIDER_ID);
    let (status, second) = send_json(
        &harness,
        Method::GET,
        &format!("/providers/{COMMAND_CODE_PROVIDER_ID}"),
        &Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{second}");
    assert_eq!(second["origin"], "builtin");
    assert_eq!(second["editable"], false);
    assert_eq!(second["deletable"], false);
    assert!(second["endpointUrl"].is_null(), "{second}");
    harness.stop();
}

#[tokio::test]
async fn get_dynamic_provider_distinguishes_preset_and_custom_origins() {
    let harness = start_loopback("dyn-origin-get").await;
    let (status, preset) = send_json(
        &harness,
        Method::POST,
        "/providers",
        &cas(
            &harness,
            json!({
                "name": "Bailian Plan",
                "presetId": "bailian-coding",
                "endpointUrl": "https://example.com/v1",
                "upstreamProtocol": "chat_completions",
                "authKind": "bearer",
                "key": "sk-bailian",
                "models": [{"publicModel": "lab-opus", "upstreamModel": "vendor/opus"}]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{preset}");
    let preset_id = preset["provider"]["id"].as_str().unwrap().to_string();

    let (status, custom) = send_json(
        &harness,
        Method::POST,
        "/providers",
        &cas(
            &harness,
            create_body(
                "Custom Raw",
                "https://example.com/v1",
                "chat_completions",
                "bearer",
                Some("sk-custom"),
            ),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{custom}");
    let custom_id = custom["provider"]["id"].as_str().unwrap().to_string();

    let (status, preset_loaded) = send_json(
        &harness,
        Method::GET,
        &format!("/providers/{preset_id}"),
        &Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{preset_loaded}");
    assert_eq!(preset_loaded["origin"], "preset");
    assert_eq!(preset_loaded["editable"], true);
    assert_eq!(preset_loaded["deletable"], true);
    assert_eq!(preset_loaded["offering"], "plan");
    assert_eq!(preset_loaded["presetId"], "bailian-coding");
    assert_eq!(preset_loaded["endpointUrl"], "https://example.com/v1");
    assert_eq!(preset_loaded["upstreamProtocol"], "chat_completions");
    assert_eq!(preset_loaded["authKind"], "bearer");

    let (status, custom_loaded) = send_json(
        &harness,
        Method::GET,
        &format!("/providers/{custom_id}"),
        &Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{custom_loaded}");
    assert_eq!(custom_loaded["origin"], "custom");
    assert_eq!(custom_loaded["editable"], true);
    assert_eq!(custom_loaded["deletable"], true);
    assert_eq!(custom_loaded["offering"], "api");
    assert!(custom_loaded["presetId"].is_null(), "{custom_loaded}");
    harness.stop();
}

#[tokio::test]
async fn patch_and_delete_builtin_provider_returns_immutable_error() {
    let harness = start_loopback("dyn-builtin-immutable").await;
    let (status, patch) = send_json(
        &harness,
        Method::PATCH,
        &format!("/providers/{OPENCODE_PROVIDER_ID}"),
        &cas(
            &harness,
            json!({
                "name": "Renamed",
                "endpointUrl": "https://example.com/v1",
                "upstreamProtocol": "chat_completions",
                "authKind": "bearer",
                "models": [{"publicModel": "lab-opus", "upstreamModel": "vendor/opus"}]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{patch}");
    assert_eq!(patch["code"], ERROR_BUILTIN_PROVIDER_IMMUTABLE, "{patch}");
    assert!(patch["message"].is_string(), "{patch}");
    assert!(patch["currentRevision"].is_u64(), "{patch}");
    assert!(patch["processGeneration"].is_u64(), "{patch}");

    let (status, delete) = send_json(
        &harness,
        Method::DELETE,
        &format!("/providers/{OPENCODE_PROVIDER_ID}"),
        &cas(&harness, json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{delete}");
    assert_eq!(delete["code"], ERROR_BUILTIN_PROVIDER_IMMUTABLE, "{delete}");
    harness.stop();
}

#[tokio::test]
async fn catalog_entries_advertise_origin_and_mutability_flags_for_every_provider() {
    let harness = start_loopback("dyn-catalog-origin").await;
    let (status, _created) = send_json(
        &harness,
        Method::POST,
        "/providers",
        &cas(
            &harness,
            json!({
                "name": "Custom Lab",
                "endpointUrl": "https://example.com/v1",
                "upstreamProtocol": "chat_completions",
                "authKind": "bearer",
                "key": "sk-lab",
                "models": [{"publicModel": "lab-opus", "upstreamModel": "vendor/opus"}]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{_created}");

    let (status, body) = send_json(&harness, Method::GET, "/providers", &Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let entries = body["entries"].as_array().expect("entries array");
    assert!(!entries.is_empty(), "{body}");

    let mut seen_builtin = false;
    let mut seen_custom = false;
    for entry in entries {
        let origin = entry["origin"].as_str().expect("origin");
        let editable = entry["editable"].as_bool().expect("editable");
        let deletable = entry["deletable"].as_bool().expect("deletable");
        let offering = entry["offering"]
            .as_str()
            .unwrap_or_else(|| panic!("offering missing from {entry}"));
        assert!(
            matches!(offering, "plan" | "api"),
            "offering must be plan|api, got {offering} in {entry}"
        );
        match origin {
            "builtin" => {
                seen_builtin = true;
                assert!(!editable, "builtin entry must not be editable: {entry}");
                assert!(!deletable, "builtin entry must not be deletable: {entry}");
            }
            "custom" | "preset" => {
                seen_custom = true;
                assert!(editable, "dynamic entry must be editable: {entry}");
                assert!(deletable, "dynamic entry must be deletable: {entry}");
            }
            other => panic!("unexpected origin {other} in {entry}"),
        }
    }
    assert!(seen_builtin, "no builtin entries found in {body}");
    assert!(seen_custom, "no custom entries found in {body}");
    harness.stop();
}

#[tokio::test]
async fn catalog_entries_advertise_offering_per_builtin_and_preset_origin() {
    let harness = start_loopback("dyn-catalog-offering").await;
    let (status, plan_row) = send_json(
        &harness,
        Method::POST,
        "/providers",
        &cas(
            &harness,
            json!({
                "name": "Bailian Plan",
                "presetId": "bailian-coding",
                "endpointUrl": "https://example.com/v1",
                "upstreamProtocol": "chat_completions",
                "authKind": "bearer",
                "key": "sk-bailian",
                "models": [{"publicModel": "lab-opus", "upstreamModel": "vendor/opus"}]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{plan_row}");
    let plan_provider_id = plan_row["provider"]["id"].as_str().unwrap().to_string();

    let (status, custom_row) = send_json(
        &harness,
        Method::POST,
        "/providers",
        &cas(
            &harness,
            json!({
                "name": "Custom Lab",
                "endpointUrl": "https://example.com/v1",
                "upstreamProtocol": "chat_completions",
                "authKind": "bearer",
                "key": "sk-lab",
                "models": [{"publicModel": "lab-opus", "upstreamModel": "vendor/opus"}]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{custom_row}");
    let custom_provider_id = custom_row["provider"]["id"].as_str().unwrap().to_string();

    let (status, body) = send_json(&harness, Method::GET, "/providers", &Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let entries = body["entries"].as_array().expect("entries array");

    let by_id: HashMap<String, &Value> = entries
        .iter()
        .map(|entry| (entry["providerId"].as_str().unwrap().to_string(), entry))
        .collect();

    for (provider_id, expected_offering) in [
        (OPENCODE_PROVIDER_ID, "plan"),
        (COMMAND_CODE_PROVIDER_ID, "plan"),
        (ocg_core::provider::MINIMAX_PROVIDER_ID, "plan"),
        (ocg_core::provider::KIMI_PROVIDER_ID, "plan"),
        (ocg_core::provider::OLLAMA_PROVIDER_ID, "plan"),
        (ocg_core::provider::OPENCODE_ZEN_FREE_PROVIDER_ID, "api"),
        (CUSTOM_PROVIDER_ID, "api"),
    ] {
        let entry = by_id
            .get(provider_id)
            .unwrap_or_else(|| panic!("missing catalog entry for {provider_id}"));
        assert_eq!(
            entry["offering"].as_str(),
            Some(expected_offering),
            "{provider_id} entry: {entry}"
        );
    }

    let plan_entry = by_id
        .get(&plan_provider_id)
        .expect("plan preset catalog entry");
    assert_eq!(
        plan_entry["offering"].as_str(),
        Some("plan"),
        "{plan_entry}"
    );
    assert_eq!(
        plan_entry["origin"].as_str(),
        Some("preset"),
        "{plan_entry}"
    );

    let custom_entry = by_id
        .get(&custom_provider_id)
        .expect("custom catalog entry");
    assert_eq!(
        custom_entry["offering"].as_str(),
        Some("api"),
        "{custom_entry}"
    );
    assert_eq!(
        custom_entry["origin"].as_str(),
        Some("custom"),
        "{custom_entry}"
    );

    harness.stop();
}

#[tokio::test]
async fn dynamic_provider_offering_round_trips_for_preset_and_custom() {
    let harness = start_loopback("dyn-offering-roundtrip").await;
    let (status, plan_row) = send_json(
        &harness,
        Method::POST,
        "/providers",
        &cas(
            &harness,
            json!({
                "name": "Bailian Plan",
                "presetId": "bailian-coding",
                "endpointUrl": "https://example.com/v1",
                "upstreamProtocol": "chat_completions",
                "authKind": "bearer",
                "key": "sk-bailian",
                "models": [{"publicModel": "lab-opus", "upstreamModel": "vendor/opus"}]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{plan_row}");
    let plan_id = plan_row["provider"]["id"].as_str().unwrap().to_string();
    assert_eq!(plan_row["provider"]["offering"], "plan");
    assert_eq!(plan_row["provider"]["origin"], "preset");

    let (status, api_row) = send_json(
        &harness,
        Method::POST,
        "/providers",
        &cas(
            &harness,
            create_body(
                "API Lab",
                "https://example.com/v1",
                "chat_completions",
                "bearer",
                Some("sk-api"),
            ),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{api_row}");
    let api_id = api_row["provider"]["id"].as_str().unwrap().to_string();
    assert_eq!(api_row["provider"]["offering"], "api");
    assert_eq!(api_row["provider"]["origin"], "custom");

    let (status, plan_loaded) = send_json(
        &harness,
        Method::GET,
        &format!("/providers/{plan_id}"),
        &Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{plan_loaded}");
    assert_eq!(plan_loaded["offering"], "plan");
    assert_eq!(plan_loaded["origin"], "preset");
    assert_eq!(plan_loaded["presetId"], "bailian-coding");

    let (status, api_loaded) = send_json(
        &harness,
        Method::GET,
        &format!("/providers/{api_id}"),
        &Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{api_loaded}");
    assert_eq!(api_loaded["offering"], "api");
    assert_eq!(api_loaded["origin"], "custom");
    assert!(api_loaded["presetId"].is_null(), "{api_loaded}");
    harness.stop();
}

#[tokio::test]
async fn v3_create_is_configured_not_a_draft() {
    let harness = start_loopback("dyn-not-draft").await;
    let (status, created) = send_json(
        &harness,
        Method::POST,
        "/providers",
        &cas(
            &harness,
            json!({
                "name": "Live Lab",
                "endpointUrl": "https://live-lab.example/v1/chat/completions",
                "upstreamProtocol": "chat_completions",
                "authKind": "bearer",
                "models": [{
                    "publicModel": "lab-opus",
                    "upstreamModel": "vendor/opus"
                }]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let provider_id = created["provider"]["id"].as_str().unwrap().to_string();
    assert_eq!(
        harness
            .state
            .db
            .lock()
            .provider_is_onboarding_draft(&provider_id)
            .unwrap(),
        Some(false)
    );
    assert_eq!(harness.state.dynamic_providers().len(), 1);
    harness.stop();
}

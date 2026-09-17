//! Public financial endpoints: fixed source, selected Key, CAS and old evidence retention.
use chrono::{DateTime, Duration, Utc};
use ocg_core::official_api::{
    BALANCE_URL, DEEPSEEK_PRICING_URL, ZHIPU_PRICING_URL, install_official_api_endpoint_for_test,
};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use std::collections::{HashMap, VecDeque};
#[allow(dead_code)]
#[path = "fixtures/fake_upstream.rs"]
mod fake_upstream;
#[path = "fixtures/dashboard_v3/harness.rs"]
mod harness;
use fake_upstream::{FakeReply, start_fake_upstream, start_fake_upstream_with_delay};
use harness::{V3Harness, start_loopback, start_public};
const KEY: &str = "sk-official-test-only";
const BALANCE: &str = r#"{"is_available":true,"balance_infos":[{"currency":"CNY","total_balance":"12.5","granted_balance":"2.5","topped_up_balance":"10"},{"currency":"USD","total_balance":"2","granted_balance":"0","topped_up_balance":"2"}]}"#;
const DS_PRICES: &str = include_str!("fixtures/official-api/deepseek-pricing.html");
const GLM_PRICES: &str = include_str!("fixtures/official-api/zhipu-pricing.md");
fn now() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-09-17T01:00:00Z")
        .unwrap()
        .with_timezone(&Utc)
}
fn cas(h: &V3Harness) -> Value {
    json!({"expectedRevision":h.state.settings_revision(),"processGeneration":h.state.process_generation()})
}
async fn send(h: &V3Harness, method: Method, path: &str, body: Value) -> (StatusCode, Value) {
    let r = h
        .client
        .request(
            method,
            format!("http://127.0.0.1:{}/dashboard/api/v4{path}", h.handle.port),
        )
        .json(&body)
        .send()
        .await
        .unwrap();
    let status = r.status();
    let body = r.json::<Value>().await.unwrap();
    (status, body)
}
async fn create(h: &V3Harness, kind: &str) -> (String, String) {
    let (endpoint, model) = if kind == "deepseek" {
        (
            "https://api.deepseek.com/chat/completions",
            "deepseek-flash",
        )
    } else {
        (
            "https://open.bigmodel.cn/api/paas/v4/chat/completions",
            "glm-5.3",
        )
    };
    let mut body = cas(h);
    body.as_object_mut().unwrap().extend(json!({"presetId":kind,"name":"Official API fixture","endpointUrl":endpoint,"upstreamProtocol":"chat_completions","authKind":"bearer","key":KEY,"models":[{"publicModel":"official-model","upstreamModel":model}]}).as_object().unwrap().clone());
    let r = h
        .client
        .post(format!("{}/providers", h.v3_base))
        .json(&body)
        .send()
        .await
        .unwrap();
    let status = r.status();
    let body = r.json::<Value>().await.unwrap();
    assert_eq!(status, StatusCode::OK, "{body}");
    let provider = body["provider"]["id"].as_str().unwrap().to_string();
    let account = h
        .state
        .db
        .lock()
        .list_accounts()
        .unwrap()
        .into_iter()
        .find(|a| a.provider_id == provider)
        .unwrap()
        .id;
    (provider, account)
}
fn clock(h: &V3Harness, at: DateTime<Utc>) {
    h.state.usage_sync.set_clock_for_test(move || at);
}
fn assert_safe(body: &Value) {
    let text = body.to_string();
    assert!(!text.contains(KEY));
    assert!(!text.contains("keyCipher"));
    assert!(!text.contains("key_cipher"));
    assert!(!text.contains("Authorization"));
}

#[tokio::test]
async fn official_api_balance_refresh_is_selected_key_only_and_retains_evidence_on_failure() {
    let h = start_loopback("official-balance").await;
    clock(&h, now());
    let (provider, id) = create(&h, "deepseek").await;
    let (base, calls, _stop) = start_fake_upstream(HashMap::from([(
        KEY.into(),
        VecDeque::from([
            FakeReply {
                status: 200,
                body: BALANCE,
            },
            FakeReply {
                status: 500,
                body: KEY,
            },
        ]),
    )]))
    .await;
    let _guard = install_official_api_endpoint_for_test(
        h.state.process_generation(),
        BALANCE_URL,
        &format!("{base}/user/balance"),
    )
    .unwrap();
    let before = h.state.db.lock().get_account(&id).unwrap().unwrap();
    let path = format!("/accounts/{id}/official-api");
    let (status, body) = send(&h, Method::GET, &path, json!({})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["balances"], json!([]));
    assert_eq!(body["balanceAvailable"], true);
    assert!(calls.lock().unwrap().is_empty());
    let (status, body) = send(&h, Method::POST, &format!("{path}/balance"), cas(&h)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_safe(&body);
    assert_eq!(body["providerId"], provider);
    assert_eq!(body["balances"][0]["total"], 12.5);
    assert_eq!(body["balances"][0]["granted"], 2.5);
    assert_eq!(body["balances"][1]["currency"], "USD");
    assert_eq!(calls.lock().unwrap().len(), 1);
    let call = calls.lock().unwrap()[0].clone();
    assert_eq!(call.method, Method::GET);
    assert_eq!(call.path, "/user/balance");
    assert_eq!(
        call.authorization.as_deref(),
        Some(&*format!("Bearer {KEY}"))
    );
    assert!(call.cookie.is_none());
    assert!(call.body.is_empty());
    let (status, _) = send(&h, Method::POST, &format!("{path}/balance"), cas(&h)).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(calls.lock().unwrap().len(), 1);
    clock(&h, now() + Duration::seconds(16));
    let (status, error) = send(&h, Method::POST, &format!("{path}/balance"), cas(&h)).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert_safe(&error);
    let (_, after) = send(&h, Method::GET, &path, json!({})).await;
    assert_eq!(after["balances"], body["balances"]);
    let account = h.state.db.lock().get_account(&id).unwrap().unwrap();
    assert_eq!(account.cooldown_until, before.cooldown_until);
    assert_eq!(account.auth_error, before.auth_error);
    assert_eq!(account.enabled, before.enabled);
    h.state
        .db
        .lock()
        .update_account(
            &id,
            &Default::default(),
            Some(&h.state.encrypt_key("rotated").unwrap()),
            None,
        )
        .unwrap();
    let (_, after) = send(&h, Method::GET, &path, json!({})).await;
    assert_eq!(
        after["balances"],
        json!([]),
        "old-Key balance must not be presented for a new Key"
    );
    h.stop();
}

#[tokio::test]
async fn official_api_price_refresh_is_keyless_scoped_and_failure_keeps_previous_snapshot() {
    for (kind, source, fixture, currency) in [
        ("deepseek", DEEPSEEK_PRICING_URL, DS_PRICES, "USD"),
        ("zhipu", ZHIPU_PRICING_URL, GLM_PRICES, "CNY"),
    ] {
        let h = start_loopback("official-prices").await;
        clock(&h, now());
        let (provider, id) = create(&h, kind).await;
        let (base, calls, _stop) = start_fake_upstream(HashMap::from([(
            "".into(),
            VecDeque::from([
                FakeReply {
                    status: 200,
                    body: fixture,
                },
                FakeReply {
                    status: 200,
                    body: "schema changed",
                },
            ]),
        )]))
        .await;
        let _guard = install_official_api_endpoint_for_test(
            h.state.process_generation(),
            source,
            &format!("{base}/pricing"),
        )
        .unwrap();
        let path = format!("/providers/{provider}/official-api/pricing");
        let (_, seed) = send(&h, Method::GET, &path, json!({})).await;
        assert_eq!(seed["prices"]["rows"][0]["currency"], currency);
        assert!(calls.lock().unwrap().is_empty());
        let (status, fresh) = send(&h, Method::POST, &path, cas(&h)).await;
        assert_eq!(status, StatusCode::OK, "{fresh}");
        assert_safe(&fresh);
        assert_eq!(fresh["prices"]["sourceUrl"], source);
        assert_ne!(fresh["prices"]["revision"], seed["prices"]["revision"]);
        assert!(calls.lock().unwrap()[0].authorization.is_none());
        assert!(calls.lock().unwrap()[0].cookie.is_none());
        let (status, failed) = send(&h, Method::POST, &path, cas(&h)).await;
        assert_eq!(status, StatusCode::BAD_GATEWAY, "{failed}");
        let (_, persisted) = send(&h, Method::GET, &path, json!({})).await;
        assert_eq!(persisted["prices"], fresh["prices"]);
        if kind == "zhipu" {
            let (status, body) = send(
                &h,
                Method::POST,
                &format!("/accounts/{id}/official-api/balance"),
                cas(&h),
            )
            .await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
            assert_eq!(calls.lock().unwrap().len(), 2);
        }
        h.stop();
    }
}

#[tokio::test]
async fn official_api_stale_cas_and_revoked_destination_send_nothing() {
    let h = start_loopback("official-cas").await;
    let (_, id) = create(&h, "deepseek").await;
    let (base, calls, _stop) = start_fake_upstream(HashMap::from([(
        KEY.into(),
        VecDeque::from([FakeReply {
            status: 200,
            body: BALANCE,
        }]),
    )]))
    .await;
    let _guard = install_official_api_endpoint_for_test(
        h.state.process_generation(),
        BALANCE_URL,
        &format!("{base}/balance"),
    )
    .unwrap();
    let path = format!("/accounts/{id}/official-api/balance");
    for invalid in [
        json!({}),
        json!({"expectedRevision":0,"processGeneration":0}),
    ] {
        let (status, _) = send(&h, Method::POST, &path, invalid).await;
        assert!(status.is_client_error());
    }
    let binding = h
        .state
        .db
        .lock()
        .list_inference_bindings()
        .unwrap()
        .into_iter()
        .find(|b| b.account_id == id)
        .unwrap();
    let (status, body) = send(
        &h,
        Method::PATCH,
        &format!("/bindings/{}", binding.binding_id),
        {
            let mut v = cas(&h);
            v["allowedEndpointIds"] = json!([]);
            v["allowedOrigins"] = json!([]);
            v
        },
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = send(&h, Method::POST, &path, cas(&h)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(calls.lock().unwrap().is_empty());
    h.stop();
}

#[tokio::test]
async fn official_api_changed_key_during_io_rejects_late_balance() {
    let h = start_loopback("official-race").await;
    let (_, id) = create(&h, "deepseek").await;
    let (base, calls, _stop) = start_fake_upstream_with_delay(
        HashMap::from([(
            KEY.into(),
            VecDeque::from([FakeReply {
                status: 200,
                body: BALANCE,
            }]),
        )]),
        std::time::Duration::from_millis(150),
    )
    .await;
    let _guard = install_official_api_endpoint_for_test(
        h.state.process_generation(),
        BALANCE_URL,
        &format!("{base}/balance"),
    )
    .unwrap();
    let url = format!(
        "http://127.0.0.1:{}/dashboard/api/v4/accounts/{id}/official-api/balance",
        h.handle.port
    );
    let client = h.client.clone();
    let expectation = cas(&h);
    let task =
        tokio::spawn(async move { client.post(url).json(&expectation).send().await.unwrap() });
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while calls.lock().unwrap().is_empty() {
            tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        }
    })
    .await
    .unwrap();
    let encrypted = h.state.encrypt_key("new-key").unwrap();
    h.state
        .db
        .lock()
        .update_account(&id, &Default::default(), Some(&encrypted), None)
        .unwrap();
    h.state.bump_settings_revision();
    assert_eq!(task.await.unwrap().status(), StatusCode::CONFLICT);
    assert!(
        h.state
            .db
            .lock()
            .list_credit_balances(&id)
            .unwrap()
            .is_empty()
    );
    h.stop();
}

#[tokio::test]
async fn official_api_financial_routes_require_dashboard_session() {
    let h = start_public("official-auth").await;
    for (method, path) in [
        (Method::GET, "/accounts/id/official-api"),
        (Method::POST, "/accounts/id/official-api/balance"),
        (Method::GET, "/providers/id/official-api/pricing"),
        (Method::POST, "/providers/id/official-api/pricing"),
    ] {
        let (status, body) = send(&h, method, path, cas(&h)).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
        assert_safe(&body);
    }
    h.stop();
}

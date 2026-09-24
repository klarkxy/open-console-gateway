//! MiniMax official usage refresh must return and release its locks.

use ocg_core::dashboard_v3::install_plan_usage_target_for_tests;
use ocg_core::provider::{MINIMAX_PROVIDER_ID, ProviderAdapterKind};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[path = "fixtures/dashboard_v3/harness.rs"]
mod harness;

use harness::{V3Harness, start_loopback};

fn cas(harness: &V3Harness) -> Value {
    json!({
        "expectedRevision": harness.state.settings_revision(),
        "processGeneration": harness.state.process_generation()
    })
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

async fn serve_usage(body: &str) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let body = body.to_string();
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        let mut chunk = [0_u8; 1024];
        loop {
            let read = stream.read(&mut chunk).await.unwrap_or(0);
            if read == 0 {
                break;
            }
            request.extend_from_slice(&chunk[..read]);
            if request.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(response.as_bytes()).await.unwrap();
    });
    format!("http://{address}/v1/token_plan/remains")
}

#[tokio::test]
async fn minimax_refresh_returns_and_releases_the_settings_lock() {
    let harness = start_loopback("minimax-usage-refresh-locks").await;
    let (status, created) = send_json(
        &harness,
        Method::POST,
        "/accounts",
        &json!({
            "name": "MiniMax",
            "key": "sk-cp-minimax-refresh-secret",
            "providerId": MINIMAX_PROVIDER_ID,
            "purchaseDate": "2026-01-31",
            "expectedRevision": harness.state.settings_revision(),
            "processGeneration": harness.state.process_generation()
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let account_id = created["account"]["id"].as_str().unwrap();
    let url = serve_usage(
        r#"{"model_remains":[{"model_name":"MiniMax-M3","current_interval_total_count":100,"current_interval_usage_count":10,"current_interval_status":1,"remains_time":60000,"current_weekly_total_count":200,"current_weekly_usage_count":20,"current_weekly_status":1,"weekly_remains_time":120000}]}"#,
    )
    .await;
    let _target = install_plan_usage_target_for_tests(
        harness.state.process_generation(),
        ProviderAdapterKind::MiniMaxCn,
        url,
    );

    let (status, body) = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        send_json(
            &harness,
            Method::POST,
            &format!("/accounts/{account_id}/provider-usage"),
            &cas(&harness),
        ),
    )
    .await
    .expect("MiniMax usage refresh must return without re-locking settings");
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        body["quotaWindows"]
            .as_array()
            .is_some_and(|windows| !windows.is_empty()),
        "{body}"
    );
    assert!(!body.to_string().contains("sk-cp-minimax-refresh-secret"));

    let (settings_status, settings) = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        harness.get_json(&format!("{}/settings", harness.v3_base)),
    )
    .await
    .expect("settings read must not wait on a lock held by refresh");
    assert_eq!(settings_status, StatusCode::OK, "{settings}");
    harness.stop();
}

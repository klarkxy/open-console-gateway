//! Official read-only usage clients for sealed MiniMax CN and Kimi Code CN Plans.

use crate::http_client;
use crate::models::{AppConfig, QuotaWindow};
use crate::provider::{KIMI_CN_USAGE_URL, MINIMAX_CN_USAGE_URL, ProviderAdapterKind};
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use futures_util::StreamExt;
use serde_json::{Map, Value};
use std::time::Duration;

const MAX_BODY_BYTES: usize = 256 * 1024;
pub const MINIMAX_USAGE_SOURCE: &str = "minimax-cn-official";
pub const KIMI_USAGE_SOURCE: &str = "kimi-cn-official";

fn official_usage_target(
    adapter: ProviderAdapterKind,
) -> Result<(&'static str, &'static str), String> {
    match adapter {
        ProviderAdapterKind::MiniMaxCn => Ok((MINIMAX_CN_USAGE_URL, "MiniMax CN")),
        ProviderAdapterKind::KimiCn => Ok((KIMI_CN_USAGE_URL, "Kimi Code CN")),
        _ => Err("this Plan does not expose an official manual usage refresh".to_string()),
    }
}

pub async fn fetch(
    config: &AppConfig,
    adapter: ProviderAdapterKind,
    account_id: &str,
    key: &str,
) -> Result<Vec<QuotaWindow>, String> {
    let (url, _) = official_usage_target(adapter)?;
    fetch_from_url(config, adapter, account_id, key, url).await
}

async fn fetch_from_url(
    config: &AppConfig,
    adapter: ProviderAdapterKind,
    account_id: &str,
    key: &str,
    url: &str,
) -> Result<Vec<QuotaWindow>, String> {
    let (_, label) = official_usage_target(adapter)?;
    let key = key.trim();
    if key.is_empty() {
        return Err(format!("{label} usage refresh requires a stored Key"));
    }
    let client = http_client::configured_builder(config)
        .and_then(|builder| {
            builder
                .connect_timeout(Duration::from_secs(config.connect_timeout_secs))
                .redirect(http_client::no_redirect_policy())
                .build()
                .map_err(Into::into)
        })
        .map_err(|error| format!("failed to build {label} usage client: {error}"))?;
    let response = client
        .get(url)
        .bearer_auth(key)
        .header(reqwest::header::ACCEPT, "application/json")
        .timeout(Duration::from_secs(config.non_stream_timeout_secs))
        .send()
        .await
        .map_err(|error| format!("{label} usage request failed: {error}"))?;
    let status = response.status();
    let body = read_limited(response, label).await?;
    if !status.is_success() {
        return Err(format!(
            "{label} usage endpoint returned {}",
            status.as_u16()
        ));
    }
    let value: Value = serde_json::from_slice(&body)
        .map_err(|_| format!("{label} usage endpoint did not return JSON"))?;
    let now = Utc::now();
    match adapter {
        ProviderAdapterKind::MiniMaxCn => parse_minimax(account_id, &value, now),
        ProviderAdapterKind::KimiCn => parse_kimi(account_id, &value, now),
        _ => unreachable!("adapter checked above"),
    }
}

async fn read_limited(response: reqwest::Response, label: &str) -> Result<Vec<u8>, String> {
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| format!("{label} usage body failed: {error}"))?;
        if body.len().saturating_add(chunk.len()) > MAX_BODY_BYTES {
            return Err(format!(
                "{label} usage body exceeded {MAX_BODY_BYTES} bytes"
            ));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn parse_minimax(
    account_id: &str,
    value: &Value,
    now: DateTime<Utc>,
) -> Result<Vec<QuotaWindow>, String> {
    let remains = value
        .get("model_remains")
        .and_then(Value::as_array)
        .ok_or_else(|| "MiniMax CN usage response did not include model_remains".to_string())?;
    let mut rows = Vec::new();
    for item in remains {
        let Some(item) = item.as_object() else {
            continue;
        };
        let model = item
            .get("model_name")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("model");
        let current_total = number(item, "current_interval_total_count").unwrap_or(0.0);
        let weekly_total = number(item, "current_weekly_total_count").unwrap_or(0.0);
        let current_status = integer(item, "current_interval_status");
        let weekly_status = integer(item, "current_weekly_status");
        if current_total == 0.0
            && weekly_total == 0.0
            && current_status == Some(3)
            && weekly_status == Some(3)
        {
            continue;
        }
        rows.push(minimax_window(
            account_id,
            format!("minimax_current:{model}"),
            current_total,
            number(item, "current_interval_usage_count"),
            number(item, "current_interval_remaining_percent"),
            None,
            current_status,
            integer(item, "start_time"),
            integer(item, "end_time"),
            integer(item, "remains_time"),
            now,
        ));
        rows.push(minimax_window(
            account_id,
            format!("minimax_weekly:{model}"),
            weekly_total,
            number(item, "current_weekly_usage_count"),
            number(item, "current_weekly_remaining_percent"),
            integer(item, "weekly_boost_permille"),
            weekly_status,
            integer(item, "weekly_start_time"),
            integer(item, "weekly_end_time"),
            integer(item, "weekly_remains_time"),
            now,
        ));
    }
    if rows.is_empty() {
        return Err("MiniMax CN usage response contained no Token Plan windows".to_string());
    }
    Ok(rows)
}

#[allow(clippy::too_many_arguments)]
fn minimax_window(
    account_id: &str,
    window_kind: String,
    total: f64,
    remaining_count: Option<f64>,
    remaining_percent: Option<f64>,
    boost_permille: Option<i64>,
    status: Option<i64>,
    starts_at_ms: Option<i64>,
    ends_at_ms: Option<i64>,
    resets_in_ms: Option<i64>,
    now: DateTime<Utc>,
) -> QuotaWindow {
    let unlimited = status == Some(3);
    let (used, limit_value, unit) = if unlimited {
        (0.0, None, "unlimited")
    } else if total > 0.0 {
        (
            (total - remaining_count.unwrap_or(total)).clamp(0.0, total),
            Some(total),
            "request",
        )
    } else {
        let boost = boost_permille.unwrap_or(1000).max(0) as f64 / 1000.0;
        let ceiling = (boost * 100.0).clamp(100.0, 200.0);
        let remaining = (remaining_percent.unwrap_or(100.0) * boost).clamp(0.0, 200.0);
        (ceiling - remaining.min(ceiling), Some(ceiling), "percent")
    };
    let started_at = starts_at_ms.and_then(DateTime::<Utc>::from_timestamp_millis);
    let resets_at = ends_at_ms
        .and_then(DateTime::<Utc>::from_timestamp_millis)
        .or_else(|| {
            resets_in_ms.and_then(|millis| {
                now.checked_add_signed(ChronoDuration::milliseconds(millis.max(0)))
            })
        });
    QuotaWindow {
        account_id: account_id.to_string(),
        window_kind,
        used,
        limit_value,
        started_at,
        resets_at,
        calibration_offset: 0.0,
        unit: unit.to_string(),
        source: MINIMAX_USAGE_SOURCE.to_string(),
        observed_at: Some(now),
        updated_at: now,
    }
}

fn parse_kimi(
    account_id: &str,
    value: &Value,
    now: DateTime<Utc>,
) -> Result<Vec<QuotaWindow>, String> {
    let mut rows = Vec::new();
    if let Some(usage) = value.get("usage").and_then(Value::as_object)
        && let Some(row) = kimi_window(account_id, "kimi_usage".to_string(), usage, now)
    {
        rows.push(row);
    }
    if let Some(limits) = value.get("limits").and_then(Value::as_array) {
        for (index, item) in limits.iter().enumerate() {
            let Some(item) = item.as_object() else {
                continue;
            };
            let detail = item
                .get("detail")
                .and_then(Value::as_object)
                .unwrap_or(item);
            let window_kind = kimi_limit_kind(item, detail, index);
            if let Some(row) = kimi_window(account_id, window_kind, detail, now) {
                rows.push(row);
            }
        }
    }
    if rows.is_empty() {
        return Err("Kimi Code CN usage response contained no usage or limits".to_string());
    }
    Ok(rows)
}

fn kimi_limit_kind(item: &Map<String, Value>, detail: &Map<String, Value>, index: usize) -> String {
    let window = item.get("window").and_then(Value::as_object);
    let duration = window
        .and_then(|value| integer(value, "duration"))
        .or_else(|| integer(item, "duration"))
        .or_else(|| integer(detail, "duration"));
    let time_unit = window
        .and_then(|value| value.get("timeUnit"))
        .or_else(|| item.get("timeUnit"))
        .or_else(|| detail.get("timeUnit"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_ascii_uppercase();
    match (duration, time_unit.as_str()) {
        (Some(300), unit) if unit.contains("MINUTE") => "kimi_5h".to_string(),
        (Some(value), unit) if unit.contains("MINUTE") && value % 60 == 0 => {
            format!("kimi_{}h", value / 60)
        }
        (Some(value), unit) if unit.contains("HOUR") => format!("kimi_{value}h"),
        (Some(value), unit) if unit.contains("DAY") => format!("kimi_{value}d"),
        _ => format!("kimi_limit_{}", index + 1),
    }
}

fn kimi_window(
    account_id: &str,
    window_kind: String,
    data: &Map<String, Value>,
    now: DateTime<Utc>,
) -> Option<QuotaWindow> {
    let limit = number(data, "limit")?;
    let used = number(data, "used")
        .or_else(|| number(data, "remaining").map(|remaining| limit - remaining))
        .unwrap_or(0.0)
        .clamp(0.0, limit.max(0.0));
    let resets_at = ["reset_at", "resetAt", "reset_time", "resetTime"]
        .iter()
        .find_map(|key| data.get(*key).and_then(Value::as_str))
        .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
        .map(|value| value.with_timezone(&Utc))
        .or_else(|| {
            ["reset_in", "resetIn", "ttl", "window"]
                .iter()
                .find_map(|key| integer(data, key))
                .and_then(|seconds| now.checked_add_signed(ChronoDuration::seconds(seconds.max(0))))
        });
    Some(QuotaWindow {
        account_id: account_id.to_string(),
        window_kind,
        used,
        limit_value: Some(limit.max(0.0)),
        started_at: None,
        resets_at,
        calibration_offset: 0.0,
        unit: "request".to_string(),
        source: KIMI_USAGE_SOURCE.to_string(),
        observed_at: Some(now),
        updated_at: now,
    })
}

fn number(data: &Map<String, Value>, key: &str) -> Option<f64> {
    data.get(key)
        .and_then(|value| value.as_f64().or_else(|| value.as_str()?.parse().ok()))
}

fn integer(data: &Map<String, Value>, key: &str) -> Option<i64> {
    data.get(key)
        .and_then(|value| value.as_i64().or_else(|| value.as_str()?.parse().ok()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimax_remaining_counts_as_remaining_not_used() {
        let value = serde_json::json!({"model_remains":[{
            "model_name":"MiniMax-M3","current_interval_total_count":100,
            "current_interval_usage_count":96,"current_interval_status":1,"remains_time":60000,
            "current_weekly_total_count":200,"current_weekly_usage_count":150,
            "current_weekly_status":1,"weekly_remains_time":120000
        }]});
        let rows = parse_minimax("a", &value, Utc::now()).unwrap();
        assert_eq!(rows[0].used, 4.0);
        assert_eq!(rows[1].used, 50.0);
    }

    #[test]
    fn preserves_minimax_window_duration_and_boosted_weekly_percent() {
        let base = 1_800_000_000_000_i64;
        let models = [(2_i64, 1500_i64), (6, 2000), (12, 3000)]
            .into_iter()
            .map(|(hours, boost)| {
                serde_json::json!({
                    "model_name": format!("model-{hours}h"),
                    "start_time": base,
                    "end_time": base + hours * 60 * 60 * 1000,
                    "current_interval_total_count": 100,
                    "current_interval_usage_count": 100,
                    "current_interval_status": 1,
                    "current_weekly_total_count": 0,
                    "current_weekly_usage_count": 0,
                    "current_weekly_remaining_percent": 100,
                    "weekly_boost_permille": boost,
                    "current_weekly_status": 1
                })
            })
            .collect::<Vec<_>>();
        let rows = parse_minimax(
            "a",
            &serde_json::json!({"model_remains": models}),
            Utc::now(),
        )
        .unwrap();
        for (index, hours) in [2_i64, 6, 12].into_iter().enumerate() {
            let current = &rows[index * 2];
            assert_eq!(
                current.resets_at.unwrap() - current.started_at.unwrap(),
                ChronoDuration::hours(hours)
            );
        }
        assert_eq!(rows[1].limit_value, Some(150.0));
        assert_eq!(rows[1].used, 0.0);
        assert_eq!(rows[3].limit_value, Some(200.0));
        assert_eq!(rows[3].used, 0.0);
        assert_eq!(rows[5].limit_value, Some(200.0));
        assert_eq!(rows[5].used, 0.0);
    }

    #[test]
    fn parses_kimi_summary_and_limits() {
        let value = serde_json::json!({
            "usage":{"limit":100,"used":4},
            "limits":[{
                "window":{"duration":300,"timeUnit":"MINUTE"},
                "detail":{"limit":10,"remaining":7}
            }]
        });
        let rows = parse_kimi("a", &value, Utc::now()).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].used, 4.0);
        assert_eq!(rows[1].used, 3.0);
        assert_eq!(rows[1].window_kind, "kimi_5h");
    }

    #[test]
    fn official_usage_urls_are_the_fixed_production_constants() {
        assert_eq!(
            official_usage_target(ProviderAdapterKind::MiniMaxCn).unwrap(),
            (MINIMAX_CN_USAGE_URL, "MiniMax CN")
        );
        assert_eq!(
            official_usage_target(ProviderAdapterKind::KimiCn).unwrap(),
            (KIMI_CN_USAGE_URL, "Kimi Code CN")
        );
        assert!(official_usage_target(ProviderAdapterKind::OpenCodeGo).is_err());
    }

    #[derive(Clone)]
    struct CapturedUsageCall {
        method: String,
        path: String,
        authorization: Option<String>,
        accept: Option<String>,
    }

    async fn start_usage_origin(
        status: axum::http::StatusCode,
        body: &'static str,
        path: &str,
    ) -> (
        String,
        std::sync::Arc<std::sync::Mutex<Vec<CapturedUsageCall>>>,
    ) {
        use axum::Router;
        use axum::extract::OriginalUri;
        use axum::http::{HeaderMap, Method};
        use axum::routing::any;
        let calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let calls_for_handler = calls.clone();
        let app = Router::new().fallback(any(
            move |method: Method, uri: OriginalUri, headers: HeaderMap| {
                let calls = calls_for_handler.clone();
                async move {
                    calls.lock().unwrap().push(CapturedUsageCall {
                        method: method.to_string(),
                        path: uri.path().to_string(),
                        authorization: headers
                            .get(axum::http::header::AUTHORIZATION)
                            .and_then(|value| value.to_str().ok())
                            .map(str::to_string),
                        accept: headers
                            .get(axum::http::header::ACCEPT)
                            .and_then(|value| value.to_str().ok())
                            .map(str::to_string),
                    });
                    (status, body)
                }
            },
        ));
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (format!("http://{addr}{path}"), calls)
    }

    #[tokio::test]
    async fn minimax_usage_http_sends_bearer_get_and_parses() {
        let (url, calls) = start_usage_origin(
            axum::http::StatusCode::OK,
            r#"{"model_remains":[{"model_name":"MiniMax-M3","current_interval_total_count":100,"current_interval_usage_count":96,"current_interval_status":1,"current_weekly_total_count":200,"current_weekly_usage_count":150,"current_weekly_status":1}]}"#,
            "/v1/token_plan/remains",
        )
        .await;
        let rows = fetch_from_url(
            &AppConfig::default(),
            ProviderAdapterKind::MiniMaxCn,
            "acct-1",
            "sk-minimax",
            &url,
        )
        .await
        .unwrap();
        assert_eq!(rows[0].used, 4.0);
        assert_eq!(rows[0].source, MINIMAX_USAGE_SOURCE);
        let captured = calls.lock().unwrap();
        assert_eq!(captured.len(), 1);
        assert_eq!(captured[0].method, "GET");
        assert_eq!(captured[0].path, "/v1/token_plan/remains");
        assert_eq!(
            captured[0].authorization.as_deref(),
            Some("Bearer sk-minimax")
        );
        assert_eq!(captured[0].accept.as_deref(), Some("application/json"));
    }

    #[tokio::test]
    async fn kimi_usage_http_sends_bearer_get_and_parses() {
        let (url, calls) = start_usage_origin(
            axum::http::StatusCode::OK,
            r#"{"usage":{"limit":100,"used":4},"limits":[]}"#,
            "/coding/v1/usages",
        )
        .await;
        let rows = fetch_from_url(
            &AppConfig::default(),
            ProviderAdapterKind::KimiCn,
            "acct-1",
            "sk-kimi",
            &url,
        )
        .await
        .unwrap();
        assert_eq!(rows[0].used, 4.0);
        assert_eq!(rows[0].source, KIMI_USAGE_SOURCE);
        let captured = calls.lock().unwrap();
        assert_eq!(captured[0].method, "GET");
        assert_eq!(captured[0].path, "/coding/v1/usages");
        assert_eq!(captured[0].authorization.as_deref(), Some("Bearer sk-kimi"));
        assert_eq!(captured[0].accept.as_deref(), Some("application/json"));
    }

    #[tokio::test]
    async fn usage_http_non_2xx_fails_without_parsing() {
        let (url, _) = start_usage_origin(
            axum::http::StatusCode::BAD_GATEWAY,
            r#"{"error":"down"}"#,
            "/v1/token_plan/remains",
        )
        .await;
        let error = fetch_from_url(
            &AppConfig::default(),
            ProviderAdapterKind::MiniMaxCn,
            "acct-1",
            "sk-minimax",
            &url,
        )
        .await
        .unwrap_err();
        assert!(error.contains("502"), "{error}");
    }
}

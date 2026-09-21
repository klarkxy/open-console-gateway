//! Official Key-authenticated balance reads for destinations that publish one.
//!
//! New API / Sub2API remain on the platform snapshot reader. This leaf covers
//! Custom API and user-defined Provider accounts whose stored Endpoint host is
//! a known official balance origin. Unknown hosts are skipped; this never
//! probes arbitrary paths.

use crate::custom_http::{
    CustomUrlHost, HttpInferenceTransport, build_custom_http_client, inspect_custom_url,
    join_inference_endpoint,
};
use crate::models::{AppConfig, CreditBalance};
use crate::provider::UpstreamAuthScheme;
use chrono::Utc;
use reqwest::StatusCode;
use reqwest::header::{ACCEPT, HeaderMap, HeaderValue};
use serde_json::Value;
use std::time::Duration;

pub const DEEPSEEK_BALANCE_SOURCE: &str = "deepseek-official";
pub const MOONSHOT_BALANCE_SOURCE: &str = "moonshot-official";
pub const STEPFUN_BALANCE_SOURCE: &str = "stepfun-api-official";

const MAX_BODY_BYTES: usize = 256 * 1024;
const PATH_DEEPSEEK: &str = "user/balance";
const PATH_MOONSHOT: &str = "v1/users/me/balance";
const PATH_STEPFUN: &str = "v1/accounts";

#[cfg(test)]
#[path = "api_balance/tests.rs"]
mod tests;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BalanceKind {
    DeepSeek,
    Moonshot,
    StepFun,
}

#[derive(Debug, Clone)]
struct BalanceProbe {
    url: reqwest::Url,
    source: &'static str,
    kind: BalanceKind,
    unit_hint: &'static str,
}

pub fn is_official_balance_source(source: &str) -> bool {
    source == DEEPSEEK_BALANCE_SOURCE
        || source == MOONSHOT_BALANCE_SOURCE
        || source == STEPFUN_BALANCE_SOURCE
}

/// Exact official hosts only. Suffix matching is never used.
fn probe_kind(host: &str) -> Option<(BalanceKind, &'static str, &'static str, &'static str)> {
    match host {
        "api.deepseek.com" => Some((
            BalanceKind::DeepSeek,
            PATH_DEEPSEEK,
            DEEPSEEK_BALANCE_SOURCE,
            "cny",
        )),
        "api.moonshot.cn" => Some((
            BalanceKind::Moonshot,
            PATH_MOONSHOT,
            MOONSHOT_BALANCE_SOURCE,
            "cny",
        )),
        "api.moonshot.ai" => Some((
            BalanceKind::Moonshot,
            PATH_MOONSHOT,
            MOONSHOT_BALANCE_SOURCE,
            "usd",
        )),
        "api.stepfun.com" => Some((
            BalanceKind::StepFun,
            PATH_STEPFUN,
            STEPFUN_BALANCE_SOURCE,
            "cny",
        )),
        _ => None,
    }
}

/// Step Plan paths must be rejected on the original StepFun URL. Origin
/// stripping would otherwise classify `/step_plan/...` as API balance.
fn is_step_plan_path(path: &str) -> bool {
    path == "/step_plan" || path.starts_with("/step_plan/")
}

fn is_stepfun_api_balance_url(parsed: &reqwest::Url, host: &str) -> bool {
    host == "api.stepfun.com"
        && parsed.scheme() == "https"
        && parsed.port_or_known_default() == Some(443)
        && !is_step_plan_path(parsed.path())
}

fn parsed_endpoint(endpoint_url: &str) -> Result<reqwest::Url, String> {
    let parsed = reqwest::Url::parse(endpoint_url.trim())
        .map_err(|_| "balance endpoint URL is not a valid URL".to_string())?;
    inspect_custom_url(&parsed).map_err(|error| error.to_string())?;
    if let Some(host) = host_domain(&parsed)
        && host == "api.stepfun.com"
        && !is_stepfun_api_balance_url(&parsed, &host)
    {
        return Err("this destination does not expose an official balance endpoint".to_string());
    }
    Ok(parsed)
}

fn origin_of(endpoint_url: &str) -> Result<reqwest::Url, String> {
    let parsed = parsed_endpoint(endpoint_url)?;
    let mut origin = parsed;
    origin.set_path("");
    origin.set_query(None);
    origin.set_fragment(None);
    Ok(origin)
}

fn host_domain(url: &reqwest::Url) -> Option<String> {
    match inspect_custom_url(url).ok()?.host {
        CustomUrlHost::Domain(name) => Some(name.to_ascii_lowercase()),
        CustomUrlHost::Ip(_) => None,
    }
}

pub fn probe_from_endpoint(endpoint_url: &str) -> Option<()> {
    let origin = origin_of(endpoint_url).ok()?;
    let host = host_domain(&origin)?;
    probe_kind(&host).map(|_| ())
}

fn probe_for(endpoint_url: &str) -> Result<BalanceProbe, String> {
    let origin = origin_of(endpoint_url)?;
    let host = host_domain(&origin).ok_or_else(|| {
        "this destination does not expose an official balance endpoint".to_string()
    })?;
    let Some((kind, path, source, unit_hint)) = probe_kind(&host) else {
        return Err("this destination does not expose an official balance endpoint".to_string());
    };
    let url = join_inference_endpoint(origin.as_str(), path).map_err(|error| error.to_string())?;
    inspect_custom_url(&url).map_err(|error| error.to_string())?;
    Ok(BalanceProbe {
        url,
        source,
        kind,
        unit_hint,
    })
}

pub async fn fetch(
    config: &AppConfig,
    account_id: &str,
    key: &str,
    endpoint_url: &str,
) -> Result<Vec<CreditBalance>, String> {
    let probe = probe_for(endpoint_url)?;
    fetch_probe(config, account_id, key, probe).await
}

async fn fetch_probe(
    config: &AppConfig,
    account_id: &str,
    key: &str,
    probe: BalanceProbe,
) -> Result<Vec<CreditBalance>, String> {
    let key = key.trim();
    if key.is_empty() {
        return Err("balance refresh requires a stored Key".to_string());
    }
    let client = build_custom_http_client(config).map_err(|error| error.to_string())?;
    let mut headers = HeaderMap::new();
    headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
    let response = client
        .send_isolated(
            reqwest::Method::GET,
            probe.url.clone(),
            UpstreamAuthScheme::Bearer,
            key,
            headers,
            None,
            Some(Duration::from_secs(config.non_stream_timeout_secs)),
        )
        .await
        .map_err(|error| error.to_string())?;
    if response.status().is_redirection() {
        return Err("balance endpoint redirected".to_string());
    }
    let status = response.status();
    let body = HttpInferenceTransport::read_body_limited(response, MAX_BODY_BYTES)
        .await
        .map_err(|error| error.to_string())?;
    if status != StatusCode::OK {
        return Err(format!("balance endpoint returned {}", status.as_u16()));
    }
    let value: Value = serde_json::from_slice(&body)
        .map_err(|_| "balance endpoint did not return JSON".to_string())?;
    let now = Utc::now();
    match probe.kind {
        BalanceKind::DeepSeek => parse_deepseek(account_id, probe.source, &value, now),
        BalanceKind::Moonshot => {
            parse_moonshot(account_id, probe.source, probe.unit_hint, &value, now)
        }
        BalanceKind::StepFun => {
            parse_stepfun(account_id, probe.source, probe.unit_hint, &value, now)
        }
    }
}

fn json_f64(value: Option<&Value>) -> Option<f64> {
    let value = value?;
    let parsed = match value {
        Value::Null => None,
        Value::Number(number) => number.as_f64(),
        Value::String(text) => {
            let text = text.trim();
            if text.is_empty() {
                None
            } else {
                text.parse().ok()
            }
        }
        _ => None,
    };
    parsed.filter(|number| number.is_finite())
}

fn json_str(value: Option<&Value>) -> Option<&str> {
    value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn credit_row(
    account_id: &str,
    kind: String,
    amount: f64,
    unit: &str,
    source: &str,
    now: chrono::DateTime<Utc>,
) -> CreditBalance {
    CreditBalance {
        account_id: account_id.to_string(),
        balance_kind: kind,
        amount,
        unit: unit.to_ascii_lowercase(),
        source: source.to_string(),
        observed_at: Some(now),
        updated_at: now,
    }
}

fn parse_deepseek(
    account_id: &str,
    source: &str,
    value: &Value,
    now: chrono::DateTime<Utc>,
) -> Result<Vec<CreditBalance>, String> {
    let infos = value
        .get("balance_infos")
        .and_then(Value::as_array)
        .ok_or_else(|| "DeepSeek balance response did not include balance_infos".to_string())?;
    let mut rows = Vec::new();
    for item in infos {
        let Some(item) = item.as_object() else {
            continue;
        };
        let Some(amount) = json_f64(item.get("total_balance")) else {
            continue;
        };
        let unit = json_str(item.get("currency")).unwrap_or("cny");
        rows.push(credit_row(
            account_id,
            format!("available:{unit}"),
            amount,
            unit,
            source,
            now,
        ));
    }
    if rows.is_empty() {
        return Err("DeepSeek balance response had no usable total_balance".to_string());
    }
    Ok(rows)
}

fn parse_moonshot(
    account_id: &str,
    source: &str,
    unit_hint: &str,
    value: &Value,
    now: chrono::DateTime<Utc>,
) -> Result<Vec<CreditBalance>, String> {
    if json_f64(value.get("code")) != Some(0.0)
        && value.get("code").and_then(Value::as_i64) != Some(0)
    {
        return Err("Moonshot balance response was not successful".to_string());
    }
    let data = value.get("data").unwrap_or(value);
    let amount = json_f64(data.get("available_balance"))
        .ok_or_else(|| "Moonshot balance response did not include available_balance".to_string())?;
    Ok(vec![credit_row(
        account_id,
        "available".to_string(),
        amount,
        unit_hint,
        source,
        now,
    )])
}

fn parse_stepfun(
    account_id: &str,
    source: &str,
    unit_hint: &str,
    value: &Value,
    now: chrono::DateTime<Utc>,
) -> Result<Vec<CreditBalance>, String> {
    let amount = json_f64(value.get("balance"))
        .ok_or_else(|| "StepFun balance response did not include a finite balance".to_string())?;
    Ok(vec![credit_row(
        account_id,
        "balance".to_string(),
        amount,
        unit_hint,
        source,
        now,
    )])
}

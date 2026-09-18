//! Manual Command Code GOAT usage calibration.
//!
//! Command Code's public Provider API documentation does not publish an
//! account-usage route. Its official CLI currently reads the fixed first-party
//! endpoint below for `/usage`. OCG calls it only after an explicit dashboard
//! action, keeps redirects disabled, bounds the response, and treats the
//! response as a calibration baseline for the existing local estimator.

use crate::models::AppConfig;
use crate::provider::{
    COMMAND_CODE_GOAT_QUOTA_5H, COMMAND_CODE_GOAT_QUOTA_MONTH, COMMAND_CODE_GOAT_QUOTA_WEEK,
    COMMAND_CODE_GOAT_USAGE_URL,
};
use chrono::{DateTime, Utc};
use futures_util::StreamExt;
#[cfg(debug_assertions)]
use parking_lot::Mutex;
use reqwest::StatusCode;
use serde_json::Value;
use std::fmt;
use std::time::Duration;

const MAX_BODY_BYTES: usize = 64 * 1024;
const FIVE_HOUR_MAX_MINUTES: i64 = 5 * 60;
const WEEK_MAX_MINUTES: i64 = 7 * 24 * 60;
const LIMIT_EPSILON: f64 = 1e-6;

pub const COMMAND_CODE_GOAT_USAGE_SOURCE: &str = "official_command_code_usage";

#[cfg(debug_assertions)]
static COMMAND_CODE_USAGE_URL_OVERRIDES: Mutex<std::collections::BTreeMap<u64, String>> =
    Mutex::new(std::collections::BTreeMap::new());

/// Test-only guard for a process-generation-scoped loopback usage endpoint.
#[cfg(debug_assertions)]
pub struct CommandCodeUsageTargetGuard {
    process_generation: u64,
}

#[cfg(debug_assertions)]
impl Drop for CommandCodeUsageTargetGuard {
    fn drop(&mut self) {
        COMMAND_CODE_USAGE_URL_OVERRIDES
            .lock()
            .remove(&self.process_generation);
    }
}

/// Bind a loopback stand-in to one debug `CoreState` process generation.
#[cfg(debug_assertions)]
#[must_use]
pub fn install_command_code_usage_target_for_tests(
    process_generation: u64,
    url: impl Into<String>,
) -> CommandCodeUsageTargetGuard {
    let url = url.into();
    let mut overrides = COMMAND_CODE_USAGE_URL_OVERRIDES.lock();
    match parse_loopback_http_url(&url) {
        Some(canonical) => {
            overrides.insert(process_generation, canonical);
        }
        None => {
            overrides.remove(&process_generation);
        }
    }
    CommandCodeUsageTargetGuard { process_generation }
}

#[derive(Debug, Clone, PartialEq)]
pub struct CommandCodeUsageSnapshot {
    pub observed_at: DateTime<Utc>,
    pub rolling_percent: f64,
    pub weekly_percent: f64,
    pub monthly_percent: f64,
    pub rolling_resets_in_minutes: i64,
    pub weekly_resets_in_minutes: i64,
    pub earliest_resets_in_minutes: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandCodeUsageError {
    Unauthorized,
    Forbidden,
    RateLimited,
    Http(u16),
    Timeout,
    Network,
    Oversize,
    Schema,
    Plan,
    Window,
}

impl fmt::Display for CommandCodeUsageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unauthorized => f.write_str("Command Code usage returned HTTP 401"),
            Self::Forbidden => f.write_str("Command Code usage returned HTTP 403"),
            Self::RateLimited => f.write_str("Command Code usage returned HTTP 429"),
            Self::Http(status) => write!(f, "Command Code usage returned HTTP {status}"),
            Self::Timeout => f.write_str("Command Code usage request timed out"),
            Self::Network => f.write_str("Command Code usage request failed"),
            Self::Oversize => f.write_str("Command Code usage response exceeds 64 KiB"),
            Self::Schema => f.write_str("Command Code usage response has an invalid schema"),
            Self::Plan => f.write_str("Command Code usage does not match the GOAT plan limits"),
            Self::Window => f.write_str("Command Code usage window is out of range"),
        }
    }
}

impl std::error::Error for CommandCodeUsageError {}

pub async fn fetch_command_code_usage(
    config: &AppConfig,
    api_key: &str,
    process_generation: u64,
    now: impl FnOnce() -> DateTime<Utc>,
) -> Result<CommandCodeUsageSnapshot, CommandCodeUsageError> {
    #[cfg(debug_assertions)]
    let endpoint = COMMAND_CODE_USAGE_URL_OVERRIDES
        .lock()
        .get(&process_generation)
        .cloned();
    #[cfg(debug_assertions)]
    let endpoint = endpoint.as_deref().unwrap_or(COMMAND_CODE_GOAT_USAGE_URL);
    #[cfg(not(debug_assertions))]
    let endpoint = {
        let _ = process_generation;
        COMMAND_CODE_GOAT_USAGE_URL
    };
    fetch_command_code_usage_from(config, api_key, endpoint, now).await
}

pub(crate) async fn fetch_command_code_usage_from(
    config: &AppConfig,
    api_key: &str,
    endpoint: &str,
    now: impl FnOnce() -> DateTime<Utc>,
) -> Result<CommandCodeUsageSnapshot, CommandCodeUsageError> {
    let client = crate::http_client::configured_builder(config)
        .map_err(|_| CommandCodeUsageError::Network)?
        .connect_timeout(Duration::from_secs(config.connect_timeout_secs))
        .redirect(crate::http_client::no_redirect_policy())
        .timeout(Duration::from_secs(config.non_stream_timeout_secs))
        .build()
        .map_err(|_| CommandCodeUsageError::Network)?;

    let response = client
        .get(endpoint)
        .bearer_auth(api_key.trim())
        .header(reqwest::header::ACCEPT, "application/json")
        .send()
        .await
        .map_err(map_reqwest_error)?;

    match response.status() {
        StatusCode::OK => {
            let body = read_body_limited(response).await?;
            parse_command_code_usage_body(&body, now())
        }
        StatusCode::UNAUTHORIZED => Err(CommandCodeUsageError::Unauthorized),
        StatusCode::FORBIDDEN => Err(CommandCodeUsageError::Forbidden),
        StatusCode::TOO_MANY_REQUESTS => Err(CommandCodeUsageError::RateLimited),
        status => Err(CommandCodeUsageError::Http(status.as_u16())),
    }
}

async fn read_body_limited(response: reqwest::Response) -> Result<Vec<u8>, CommandCodeUsageError> {
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(map_reqwest_error)?;
        if body.len().saturating_add(chunk.len()) > MAX_BODY_BYTES {
            return Err(CommandCodeUsageError::Oversize);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn map_reqwest_error(error: reqwest::Error) -> CommandCodeUsageError {
    if error.is_timeout() {
        CommandCodeUsageError::Timeout
    } else {
        CommandCodeUsageError::Network
    }
}

fn parse_command_code_usage_body(
    bytes: &[u8],
    now: DateTime<Utc>,
) -> Result<CommandCodeUsageSnapshot, CommandCodeUsageError> {
    let value: Value = serde_json::from_slice(bytes).map_err(|_| CommandCodeUsageError::Schema)?;
    let credits = value
        .get("credits")
        .and_then(Value::as_object)
        .ok_or(CommandCodeUsageError::Schema)?;
    let limits = value
        .get("windowLimits")
        .and_then(Value::as_object)
        .ok_or(CommandCodeUsageError::Schema)?;
    if limits.get("limited").and_then(Value::as_bool) != Some(true) {
        return Err(CommandCodeUsageError::Plan);
    }

    let monthly_remaining = finite_non_negative(
        credits
            .get("monthlyCredits")
            .and_then(Value::as_f64)
            .ok_or(CommandCodeUsageError::Schema)?,
    )?;
    if monthly_remaining > COMMAND_CODE_GOAT_QUOTA_MONTH + LIMIT_EPSILON {
        return Err(CommandCodeUsageError::Plan);
    }

    let rolling = parse_window(
        limits
            .get("fiveHour")
            .ok_or(CommandCodeUsageError::Schema)?,
        COMMAND_CODE_GOAT_QUOTA_5H,
        FIVE_HOUR_MAX_MINUTES,
        now,
    )?;
    let weekly = parse_window(
        limits.get("weekly").ok_or(CommandCodeUsageError::Schema)?,
        COMMAND_CODE_GOAT_QUOTA_WEEK,
        WEEK_MAX_MINUTES,
        now,
    )?;
    let monthly_percent = ((COMMAND_CODE_GOAT_QUOTA_MONTH - monthly_remaining)
        / COMMAND_CODE_GOAT_QUOTA_MONTH
        * 100.0)
        .clamp(0.0, 100.0);

    Ok(CommandCodeUsageSnapshot {
        observed_at: now,
        rolling_percent: rolling.percent,
        weekly_percent: weekly.percent,
        monthly_percent,
        rolling_resets_in_minutes: rolling.resets_in_minutes,
        weekly_resets_in_minutes: weekly.resets_in_minutes,
        earliest_resets_in_minutes: rolling.resets_in_minutes.min(weekly.resets_in_minutes),
    })
}

struct ParsedWindow {
    percent: f64,
    resets_in_minutes: i64,
}

fn parse_window(
    value: &Value,
    expected_cap: f64,
    max_minutes: i64,
    now: DateTime<Utc>,
) -> Result<ParsedWindow, CommandCodeUsageError> {
    let object = value.as_object().ok_or(CommandCodeUsageError::Schema)?;
    let used = finite_non_negative(
        object
            .get("used")
            .and_then(Value::as_f64)
            .ok_or(CommandCodeUsageError::Schema)?,
    )?;
    let cap = finite_non_negative(
        object
            .get("cap")
            .and_then(Value::as_f64)
            .ok_or(CommandCodeUsageError::Schema)?,
    )?;
    if cap <= 0.0 || (cap - expected_cap).abs() > LIMIT_EPSILON {
        return Err(CommandCodeUsageError::Plan);
    }
    let reset_at = object
        .get("resetAt")
        .and_then(Value::as_i64)
        .and_then(DateTime::<Utc>::from_timestamp_millis)
        .ok_or(CommandCodeUsageError::Schema)?;
    let resets_in_minutes = bounded_resets_in_minutes(reset_at, now, max_minutes)?;
    Ok(ParsedWindow {
        percent: (used / cap * 100.0).clamp(0.0, 100.0),
        resets_in_minutes,
    })
}

fn finite_non_negative(value: f64) -> Result<f64, CommandCodeUsageError> {
    if value.is_finite() && value >= 0.0 {
        Ok(value)
    } else {
        Err(CommandCodeUsageError::Schema)
    }
}

fn bounded_resets_in_minutes(
    resets_at: DateTime<Utc>,
    now: DateTime<Utc>,
    max: i64,
) -> Result<i64, CommandCodeUsageError> {
    let minutes = ceil_minutes_until(resets_at, now);
    if minutes <= max {
        Ok(minutes)
    } else if minutes == max + 1 {
        Ok(max)
    } else {
        Err(CommandCodeUsageError::Window)
    }
}

fn ceil_minutes_until(resets_at: DateTime<Utc>, now: DateTime<Utc>) -> i64 {
    if resets_at <= now {
        return 0;
    }
    let millis = (resets_at - now).num_milliseconds();
    (millis + 59_999) / 60_000
}

#[cfg(debug_assertions)]
fn parse_loopback_http_url(url: &str) -> Option<String> {
    let parsed = reqwest::Url::parse(url.trim()).ok()?;
    if !matches!(parsed.scheme(), "http" | "https")
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return None;
    }
    matches!(parsed.host_str(), Some("localhost" | "127.0.0.1" | "::1")).then(|| parsed.to_string())
}

#[cfg(test)]
mod tests;

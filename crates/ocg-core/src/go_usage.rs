//! Official OpenCode Go usage endpoint client.
//!
//! Production URL: `https://opencode.ai/zen/go/v1/usage`.
//! The public Go docs have not listed this path yet. The request/response
//! contract is taken from the official source at commits
//! `2b8a5969e932c15083e599c82d34ce0268f81b9e` and
//! `d4704347465c1ee63d0c213ed00e648e7f0231c5`.

use crate::models::AppConfig;
use crate::usage_http::{
    UsageHttpError, WindowOutOfRange, bounded_resets_in_minutes, ceil_minutes_until,
    classify_transport, read_ok_body,
};
use chrono::{DateTime, Utc};
use serde_json::Value;
use std::fmt;
use std::time::Duration;

/// Production Go usage endpoint. Callers must not substitute another URL.
///
/// Canonical definition: [`crate::kernel::catalog::OPENCODE_GO_USAGE_URL`].
pub use crate::kernel::catalog::OPENCODE_GO_USAGE_URL as GO_USAGE_URL;

const MAX_BODY_BYTES: usize = 64 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const ROLLING_MAX_MINUTES: i64 = 300;
const WEEKLY_MAX_MINUTES: i64 = 10_080;

/// Official window `status`. Only these two values are accepted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoUsageWindowStatus {
    Ok,
    RateLimited,
}

/// Pure data snapshot used to calibrate local Go usage windows.
///
/// Official reset minutes are retained for quota evidence. Local monthly
/// calibration still follows `purchase_date`.
#[derive(Debug, Clone, PartialEq)]
pub struct GoUsageSnapshot {
    pub rolling_status: GoUsageWindowStatus,
    pub weekly_status: GoUsageWindowStatus,
    pub monthly_status: GoUsageWindowStatus,
    pub rolling_percent: f64,
    pub weekly_percent: f64,
    pub monthly_percent: f64,
    pub rolling_resets_in_minutes: i64,
    pub weekly_resets_in_minutes: i64,
    pub monthly_resets_in_minutes: i64,
    /// Earliest of the three official `resetsAt` values, in whole minutes from
    /// the parse clock. Used to schedule a post-reset reconciliation.
    pub earliest_resets_in_minutes: i64,
}

/// Typed failure for a Go usage fetch. Display never includes a key or body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoUsageError {
    Unauthorized,
    Forbidden,
    RateLimited,
    Http(u16),
    Timeout,
    Network,
    Oversize,
    Schema,
    Window,
}

impl fmt::Display for GoUsageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unauthorized => f.write_str("OpenCode Go usage returned HTTP 401"),
            Self::Forbidden => f.write_str("OpenCode Go usage returned HTTP 403"),
            Self::RateLimited => f.write_str("OpenCode Go usage returned HTTP 429"),
            Self::Http(status) => write!(f, "OpenCode Go usage returned HTTP {status}"),
            Self::Timeout => f.write_str("OpenCode Go usage request timed out"),
            Self::Network => f.write_str("OpenCode Go usage request failed"),
            Self::Oversize => f.write_str("OpenCode Go usage response exceeds 64 KiB"),
            Self::Schema => f.write_str("OpenCode Go usage response has an invalid schema"),
            Self::Window => f.write_str("OpenCode Go usage window is out of range"),
        }
    }
}

impl std::error::Error for GoUsageError {}

impl From<UsageHttpError> for GoUsageError {
    fn from(error: UsageHttpError) -> Self {
        match error {
            UsageHttpError::Unauthorized => Self::Unauthorized,
            UsageHttpError::Forbidden => Self::Forbidden,
            UsageHttpError::RateLimited => Self::RateLimited,
            UsageHttpError::Http(status) => Self::Http(status),
            UsageHttpError::Timeout => Self::Timeout,
            UsageHttpError::Network => Self::Network,
            UsageHttpError::Oversize => Self::Oversize,
        }
    }
}

impl From<WindowOutOfRange> for GoUsageError {
    fn from(_: WindowOutOfRange) -> Self {
        Self::Window
    }
}

/// Fetch the official Go usage snapshot for `api_key`.
///
/// Always uses [`GO_USAGE_URL`]. Tests that need a local server must call the
/// crate-internal endpoint seam instead of changing this function.
pub async fn fetch_go_usage(
    config: &AppConfig,
    api_key: &str,
) -> Result<GoUsageSnapshot, GoUsageError> {
    fetch_go_usage_from(config, api_key, GO_USAGE_URL).await
}

/// Internal endpoint seam for tests. Production code must call [`fetch_go_usage`].
pub(crate) async fn fetch_go_usage_from(
    config: &AppConfig,
    api_key: &str,
    endpoint: &str,
) -> Result<GoUsageSnapshot, GoUsageError> {
    let client = crate::http_client::configured_builder(config)
        .map_err(|_| GoUsageError::Network)?
        .redirect(crate::http_client::no_redirect_policy())
        .timeout(REQUEST_TIMEOUT)
        .build()
        .map_err(|_| GoUsageError::Network)?;

    let response = client
        .get(endpoint)
        .bearer_auth(api_key)
        .send()
        .await
        .map_err(|error| GoUsageError::from(classify_transport(&error)))?;

    let body = read_ok_body(response, MAX_BODY_BYTES)
        .await
        .map_err(GoUsageError::from)?;
    parse_go_usage_body(&body, Utc::now())
}

fn parse_go_usage_body(bytes: &[u8], now: DateTime<Utc>) -> Result<GoUsageSnapshot, GoUsageError> {
    let value: Value = serde_json::from_slice(bytes).map_err(|_| GoUsageError::Schema)?;
    let usage = value.get("usage").ok_or(GoUsageError::Schema)?;
    if !usage.is_object() {
        return Err(GoUsageError::Schema);
    }

    let rolling = parse_window(
        usage.get("rolling").ok_or(GoUsageError::Schema)?,
        now,
        Some(ROLLING_MAX_MINUTES),
    )?;
    let weekly = parse_window(
        usage.get("weekly").ok_or(GoUsageError::Schema)?,
        now,
        Some(WEEKLY_MAX_MINUTES),
    )?;
    let monthly = parse_window(usage.get("monthly").ok_or(GoUsageError::Schema)?, now, None)?;

    let rolling_resets_in_minutes = rolling
        .resets_in_minutes
        .expect("rolling minutes are computed");
    let weekly_resets_in_minutes = weekly
        .resets_in_minutes
        .expect("weekly minutes are computed");
    let monthly_resets_in_minutes = ceil_minutes_until(monthly.resets_at, now);
    let earliest_resets_in_minutes = rolling_resets_in_minutes
        .min(weekly_resets_in_minutes)
        .min(monthly_resets_in_minutes);

    Ok(GoUsageSnapshot {
        rolling_status: rolling.status,
        weekly_status: weekly.status,
        monthly_status: monthly.status,
        rolling_percent: rolling.percent,
        weekly_percent: weekly.percent,
        monthly_percent: monthly.percent,
        rolling_resets_in_minutes,
        weekly_resets_in_minutes,
        monthly_resets_in_minutes,
        earliest_resets_in_minutes,
    })
}

struct ParsedWindow {
    status: GoUsageWindowStatus,
    percent: f64,
    resets_at: DateTime<Utc>,
    resets_in_minutes: Option<i64>,
}

fn parse_window(
    value: &Value,
    now: DateTime<Utc>,
    max_minutes: Option<i64>,
) -> Result<ParsedWindow, GoUsageError> {
    let object = value.as_object().ok_or(GoUsageError::Schema)?;
    let status = match object.get("status").and_then(Value::as_str) {
        Some("ok") => GoUsageWindowStatus::Ok,
        Some("rate-limited") => GoUsageWindowStatus::RateLimited,
        _ => return Err(GoUsageError::Schema),
    };
    let percent = parse_percent(object.get("percent").ok_or(GoUsageError::Schema)?)?;
    let resets_at = object
        .get("resetsAt")
        .and_then(Value::as_str)
        .ok_or(GoUsageError::Schema)?;
    let resets_at = DateTime::parse_from_rfc3339(resets_at)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|_| GoUsageError::Schema)?;

    let resets_in_minutes = match max_minutes {
        Some(max) => {
            Some(bounded_resets_in_minutes(resets_at, now, max).map_err(GoUsageError::from)?)
        }
        None => None,
    };

    Ok(ParsedWindow {
        status,
        percent,
        resets_at,
        resets_in_minutes,
    })
}

fn parse_percent(value: &Value) -> Result<f64, GoUsageError> {
    let percent = match value {
        Value::Number(number) => number.as_f64().ok_or(GoUsageError::Schema)?,
        Value::String(text) if text.eq_ignore_ascii_case("nan") => {
            return Err(GoUsageError::Schema);
        }
        _ => return Err(GoUsageError::Schema),
    };
    if !percent.is_finite() || !(0.0..=100.0).contains(&percent) {
        return Err(GoUsageError::Schema);
    }
    Ok(percent)
}

#[cfg(test)]
mod tests;

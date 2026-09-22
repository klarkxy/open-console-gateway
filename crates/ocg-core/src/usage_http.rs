//! Shared bounded HTTP and window-minute helpers for official usage clients.
//!
//! OpenCode Go, Command Code GOAT, MiniMax CN, and Kimi Code CN keep their own
//! parsers, body caps, timeouts, success-status rules, and error types. This
//! module owns stream-capped body reads, timeout-versus-network classification,
//! the 401/403/429 HTTP mapping Go and Command Code already share, and
//! ceil-minute reset math with a `max + 1` tolerance. Secret-bearing callers
//! still disable redirects themselves. Plan usage keeps `2xx` success and
//! string errors; only Go and Command Code treat a successful body as exact
//! HTTP `200`.

use chrono::{DateTime, Utc};
use futures_util::StreamExt;
use reqwest::StatusCode;

/// Mapped failure for an exact-`200` usage GET after send succeeds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UsageHttpError {
    Unauthorized,
    Forbidden,
    RateLimited,
    Http(u16),
    Timeout,
    Network,
    Oversize,
}

/// Stream read failed or exceeded the caller-supplied cap.
///
/// Transport keeps the raw reqwest error so plan usage can format it into its
/// existing string messages.
#[derive(Debug)]
pub(crate) enum BoundedBodyError {
    Transport(reqwest::Error),
    Oversize,
}

/// Reset is more than one ceil-minute past the window maximum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct WindowOutOfRange;

pub(crate) fn classify_transport(error: &reqwest::Error) -> UsageHttpError {
    if error.is_timeout() {
        UsageHttpError::Timeout
    } else {
        UsageHttpError::Network
    }
}

pub(crate) fn classify_http_status(status: StatusCode) -> UsageHttpError {
    match status {
        StatusCode::UNAUTHORIZED => UsageHttpError::Unauthorized,
        StatusCode::FORBIDDEN => UsageHttpError::Forbidden,
        StatusCode::TOO_MANY_REQUESTS => UsageHttpError::RateLimited,
        other => UsageHttpError::Http(other.as_u16()),
    }
}

fn map_bounded_body_error(error: BoundedBodyError) -> UsageHttpError {
    match error {
        BoundedBodyError::Oversize => UsageHttpError::Oversize,
        BoundedBodyError::Transport(error) => classify_transport(&error),
    }
}

pub(crate) async fn read_body_limited(
    response: reqwest::Response,
    max_bytes: usize,
) -> Result<Vec<u8>, BoundedBodyError> {
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(BoundedBodyError::Transport)?;
        if body.len().saturating_add(chunk.len()) > max_bytes {
            return Err(BoundedBodyError::Oversize);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

/// Exact HTTP `200` then a stream-capped body. Other `2xx` stay caller-specific.
pub(crate) async fn read_ok_body(
    response: reqwest::Response,
    max_bytes: usize,
) -> Result<Vec<u8>, UsageHttpError> {
    if response.status() != StatusCode::OK {
        return Err(classify_http_status(response.status()));
    }
    read_body_limited(response, max_bytes)
        .await
        .map_err(map_bounded_body_error)
}

pub(crate) fn ceil_minutes_until(resets_at: DateTime<Utc>, now: DateTime<Utc>) -> i64 {
    if resets_at <= now {
        return 0;
    }
    let millis = (resets_at - now).num_milliseconds();
    if millis <= 0 {
        return 0;
    }
    (millis + 59_999) / 60_000
}

pub(crate) fn bounded_resets_in_minutes(
    resets_at: DateTime<Utc>,
    now: DateTime<Utc>,
    max: i64,
) -> Result<i64, WindowOutOfRange> {
    let minutes = ceil_minutes_until(resets_at, now);
    // Official expired windows return exactly `window` hours from server now.
    // Ceil-to-minutes plus sub-second skew is commonly max+1, not a new length.
    if minutes <= max {
        Ok(minutes)
    } else if minutes == max + 1 {
        Ok(max)
    } else {
        Err(WindowOutOfRange)
    }
}

#[cfg(test)]
mod tests;

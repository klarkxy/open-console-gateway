//! Authoritative Go limit prose and exact GOAT Plan-limit evidence.
//!
//! Official Go usage is parsed only through [`crate::go_usage::official_usage_facts`].
//! Inference text, async usage, percentages, and GOAT calibration do not create
//! or clear a Plan restriction.

use chrono::{DateTime, Utc};
use serde_json::Value;

use crate::go_usage::{GoUsageFacts, GoUsageWindowStatus, official_usage_facts};
use crate::models::UsageWindowKind;
use crate::upstream_limit::{parse_reset, parse_usage_limit_window};
use ocg_domain::ids::{COMMAND_CODE_PROVIDER_ID, OPENCODE_PROVIDER_ID};

use super::store::{EvidenceSource, Window};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Observation {
    Known(DateTime<Utc>),
    Unknown,
    Healthy,
    /// Present but not allowed to shorten or clear a stronger row.
    Ignore,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Evidence {
    pub source: EvidenceSource,
    pub windows: Vec<(Window, Observation)>,
}

impl Evidence {
    pub fn none() -> Self {
        Self {
            source: EvidenceSource::GoLimit,
            windows: Vec::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.windows.is_empty()
    }

    pub fn creates_restriction(&self) -> bool {
        self.windows.iter().any(|(_, observation)| {
            matches!(observation, Observation::Known(_) | Observation::Unknown)
        })
    }
}

/// Rejection-body evidence. Official `/usage` JSON is ignored here so a
/// successful inference or async usage payload cannot clear a Plan window.
pub fn rejection(provider_id: &str, body: &str, observed_at: DateTime<Utc>) -> Evidence {
    if provider_id == OPENCODE_PROVIDER_ID {
        return go_rejection(body, observed_at);
    }
    if provider_id == COMMAND_CODE_PROVIDER_ID {
        return goat_evidence(body, observed_at);
    }
    Evidence::none()
}

/// Official usage snapshot already accepted by the Go usage parser.
pub fn from_official_usage(facts: &GoUsageFacts, now: DateTime<Utc>) -> Evidence {
    Evidence {
        source: EvidenceSource::GoUsage,
        windows: vec![
            (
                Window::FiveHours,
                window_observation(facts.rolling.status, facts.rolling.resets_at, now),
            ),
            (
                Window::Week,
                window_observation(facts.weekly.status, facts.weekly.resets_at, now),
            ),
            (
                Window::Month,
                window_observation(facts.monthly.status, facts.monthly.resets_at, now),
            ),
        ],
    }
}

pub fn parse_official_usage(body: &[u8], fetched_at: DateTime<Utc>) -> Result<GoUsageFacts, ()> {
    official_usage_facts(body, fetched_at).map_err(|_| ())
}

fn window_observation(
    status: GoUsageWindowStatus,
    resets_at: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Observation {
    match status {
        GoUsageWindowStatus::Ok => Observation::Healthy,
        GoUsageWindowStatus::RateLimited if resets_at > now => Observation::Known(resets_at),
        GoUsageWindowStatus::RateLimited => Observation::Ignore,
    }
}

fn go_rejection(body: &str, now: DateTime<Utc>) -> Evidence {
    if usage_object(body) {
        return Evidence::none();
    }
    match limit_text(body, now) {
        Some(window) => Evidence {
            source: EvidenceSource::GoLimit,
            windows: vec![window],
        },
        None => Evidence::none(),
    }
}

fn usage_object(body: &str) -> bool {
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|value| value.get("usage").cloned())
        .is_some_and(|usage| usage.is_object())
}

fn goat_evidence(body: &str, now: DateTime<Utc>) -> Evidence {
    if calibration_body(body) {
        return Evidence::none();
    }
    match plan_error(body, now) {
        Some(window) => Evidence {
            source: EvidenceSource::GoatPlan,
            windows: vec![window],
        },
        None => Evidence::none(),
    }
}

fn limit_text(body: &str, now: DateTime<Utc>) -> Option<(Window, Observation)> {
    let window = map_window(parse_usage_limit_window(body)?);
    let observation = match parse_reset(body).and_then(|duration| now.checked_add_signed(duration))
    {
        Some(deadline) if deadline > now => Observation::Known(deadline),
        Some(_) => Observation::Unknown,
        None => Observation::Unknown,
    };
    Some((window, observation))
}

fn map_window(window: UsageWindowKind) -> Window {
    match window {
        UsageWindowKind::FiveHours => Window::FiveHours,
        UsageWindowKind::Week => Window::Week,
        UsageWindowKind::Month => Window::Month,
        UsageWindowKind::Free => Window::Free,
    }
}

fn calibration_body(body: &str) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(body) else {
        return false;
    };
    value.get("credits").and_then(Value::as_object).is_some()
        && value
            .get("windowLimits")
            .and_then(Value::as_object)
            .is_some()
}

/// Exact Command Code Plan-limit object. The window word and RFC3339 reset are
/// part of the message; `RATE_LIMITED` or `rate_limit_error` is required.
/// Other 429 text, including supplier rate limits, is not a Plan restriction.
fn plan_error(body: &str, now: DateTime<Utc>) -> Option<(Window, Observation)> {
    let value: Value = serde_json::from_str(body).ok()?;
    let error = value.get("error")?.as_object()?;
    let code = error.get("code").and_then(Value::as_str);
    let kind = error.get("type").and_then(Value::as_str);
    if code != Some("RATE_LIMITED") && kind != Some("rate_limit_error") {
        return None;
    }
    let message = error.get("message").and_then(Value::as_str)?;
    let rest = message.strip_prefix("You've reached your ")?;
    let (window_word, after) =
        rest.split_once(" usage limit for your plan. Your limit resets at ")?;
    let window = match window_word {
        "5-hour" => Window::FiveHours,
        "weekly" => Window::Week,
        "monthly" => Window::Month,
        _ => return None,
    };
    let stamp = after
        .split_whitespace()
        .next()
        .unwrap_or("")
        .trim_end_matches(|ch: char| ch == '.' || ch == ',');
    if stamp.is_empty() {
        return Some((window, Observation::Unknown));
    }
    match DateTime::parse_from_rfc3339(stamp) {
        Ok(deadline) => {
            let deadline = deadline.with_timezone(&Utc);
            if deadline > now {
                Some((window, Observation::Known(deadline)))
            } else {
                None
            }
        }
        Err(_) => Some((window, Observation::Unknown)),
    }
}

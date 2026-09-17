//! Command Code GOAT inference-window rate-limit parsing.
//!
//! The Provider API reports plan-window exhaustion as a standard 429 error
//! envelope. Unlike OpenCode Go, the exact reset is an RFC3339 timestamp in
//! the message. Keep this parser provider-specific so unrelated upstream 429s
//! cannot extend an account cooldown from lookalike text.

use crate::models::UsageWindowKind;
use chrono::{DateTime, Duration, Utc};
use serde_json::Value;

const CLOCK_SKEW_ALLOWANCE: Duration = Duration::minutes(5);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CommandCodeRateLimit {
    pub(crate) window: UsageWindowKind,
    pub(crate) resets_at: DateTime<Utc>,
}

pub(crate) fn parse_command_code_rate_limit(
    text: &str,
    observed_at: DateTime<Utc>,
) -> Option<CommandCodeRateLimit> {
    let value: Value = serde_json::from_str(text).ok()?;
    let error = value.get("error")?.as_object()?;
    let code_matches = error
        .get("code")
        .and_then(Value::as_str)
        .is_some_and(|code| code.eq_ignore_ascii_case("RATE_LIMITED"));
    let type_matches = error
        .get("type")
        .and_then(Value::as_str)
        .is_some_and(|kind| kind.eq_ignore_ascii_case("rate_limit_error"));
    if !code_matches && !type_matches {
        return None;
    }

    let message = error.get("message")?.as_str()?;
    let message_lower = message.to_ascii_lowercase();
    let (window, maximum) = if message_lower.contains("5-hour usage limit for your plan")
        || message_lower.contains("5 hour usage limit for your plan")
    {
        (UsageWindowKind::FiveHours, Duration::hours(5))
    } else if message_lower.contains("weekly usage limit for your plan") {
        (UsageWindowKind::Week, Duration::days(7))
    } else {
        return None;
    };

    let resets_at = parse_reset_timestamp(message, &message_lower)?;
    let remaining = resets_at.signed_duration_since(observed_at);
    if remaining <= Duration::zero() || remaining > maximum + CLOCK_SKEW_ALLOWANCE {
        return None;
    }

    Some(CommandCodeRateLimit { window, resets_at })
}

fn parse_reset_timestamp(message: &str, message_lower: &str) -> Option<DateTime<Utc>> {
    const MARKER: &str = "your limit resets at ";
    let start = message_lower.find(MARKER)? + MARKER.len();
    let candidate = message.get(start..)?.split_whitespace().next()?;
    let candidate = candidate.trim_end_matches(['.', ',', ';']);
    DateTime::parse_from_rfc3339(candidate)
        .ok()
        .map(|timestamp| timestamp.with_timezone(&Utc))
}

#[cfg(test)]
mod tests;

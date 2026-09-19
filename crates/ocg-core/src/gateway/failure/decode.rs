//! Small static error dialects. No database, selector, retries or state writes.
use super::{Cause, FailureFacts, RetryHint, Scope};
use crate::models::UsageWindowKind;
use crate::upstream_limit::{parse_reset, parse_usage_limit_window};
use chrono::{DateTime, Datelike, Duration, NaiveDateTime, Utc};
use ocg_gateway::classify::{ErrorProfile, ProviderErrorClass};
use serde_json::Value;

pub(crate) fn decode(
    class: ProviderErrorClass,
    body: &str,
    retry_after: Option<&str>,
    observed_at: DateTime<Utc>,
) -> Option<FailureFacts> {
    let mut facts = FailureFacts {
        cause: Cause::Unknown,
        scope: Scope::Unspecified,
        window: None,
        upstream_reset_at: None,
        retry_not_before: retry_after.and_then(|value| parse_retry_after(value, observed_at)),
        rule_id: "http.429.unknown",
        rule_version: 1,
    };
    match class {
        ProviderErrorClass::InsufficientCredits => {
            facts.cause = Cause::CreditsExhausted;
            facts.scope = Scope::QuotaPool;
            facts.rule_id = "structured.insufficient_credits";
        }
        ProviderErrorClass::RateLimited {
            profile: ErrorProfile::CommandCodeGoat,
        } => {
            if let Some(limit) = crate::command_code_rate_limit::parse_command_code_rate_limit(body, observed_at) {
                facts.cause = Cause::QuotaExhausted;
                facts.scope = Scope::QuotaPool;
                facts.window = Some(limit.window);
                facts.upstream_reset_at = Some(limit.resets_at);
                facts.rule_id = "goat.plan_window";
            } else if serde_json::from_str::<Value>(body).ok().is_some_and(|value| {
                value.pointer("/error/message").and_then(Value::as_str)
                    == Some("Upstream model provider is temporarily unavailable. Please try again in a moment.")
                    && value.pointer("/error/code").is_none()
            }) {
                facts.cause = Cause::Transient;
                facts.rule_id = "goat.upstream_transient";
            }
        }
        ProviderErrorClass::RateLimited {
            profile: ErrorProfile::OpenCodeGo,
        } => {
            // This dialect is used only for the sealed Go service, never for a
            // merely OpenAI-compatible endpoint. Prefer its message field.
            let json = serde_json::from_str::<Value>(body).ok();
            let message = json
                .as_ref()
                .and_then(|v| v.pointer("/error/message").or_else(|| v.get("message")))
                .and_then(Value::as_str)
                .unwrap_or(if json.is_some() { "" } else { body });
            let kind = json
                .as_ref()
                .and_then(|v| v.pointer("/error/type").or_else(|| v.get("type")))
                .and_then(Value::as_str);
            let allowed = kind.is_none_or(|kind| {
                matches!(kind, "error" | "GoUsageLimitError" | "FreeUsageLimitError")
            });
            if allowed && let Some(window) = parse_usage_limit_window(message) {
                facts.cause = Cause::QuotaExhausted;
                facts.scope = if window == UsageWindowKind::Free {
                    Scope::SharedFreeEgress
                } else {
                    Scope::QuotaPool
                };
                facts.window = Some(window);
                facts.upstream_reset_at = future_reset(message, observed_at);
                facts.rule_id = "go.usage_window";
            }
        }
        ProviderErrorClass::RateLimited {
            profile: ErrorProfile::ZenFree,
        } => {
            // Anonymous Free is an existing, declared shared-egress contract.
            // Never let misleading paid-plan words move this onto a Key.
            facts.cause = Cause::QuotaExhausted;
            facts.scope = Scope::SharedFreeEgress;
            facts.window = Some(UsageWindowKind::Free);
            facts.upstream_reset_at = future_reset(body, observed_at);
            facts.rule_id = "zen.free_egress";
        }
        ProviderErrorClass::RateLimited {
            profile: ErrorProfile::GenericHttp,
        } => {}
        _ => return None,
    }
    Some(facts)
}

fn future_reset(text: &str, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    let parsed = parse_reset(text).or_else(|| {
        let index = text.to_ascii_lowercase().find("retrying in")?;
        parse_reset(&format!(
            "Resets in {}",
            &text[index + "retrying in".len()..]
        ))
    })?;
    (parsed > Duration::zero())
        .then(|| now.checked_add_signed(parsed))
        .flatten()
}

/// RFC 9110 Retry-After: delay-seconds and all three HTTP-date forms.
/// A valid zero/past date means no additional wait, not malformed evidence.
/// No arbitrary 31-day cap silently turns a valid long wait into an early retry.
pub(crate) fn parse_retry_after(value: &str, now: DateTime<Utc>) -> Option<RetryHint> {
    let value = value.trim();
    if !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()) {
        let deadline = value
            .parse::<i64>()
            .ok()
            .and_then(Duration::try_seconds)
            .and_then(|delay| now.checked_add_signed(delay));
        return Some(
            deadline
                .map(RetryHint::Until)
                .unwrap_or(RetryHint::Unbounded),
        );
    }
    let date = DateTime::parse_from_rfc2822(value)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
        .or_else(|| {
            let mut dt = NaiveDateTime::parse_from_str(value, "%A, %d-%b-%y %H:%M:%S GMT").ok()?;
            if dt.year() > now.year() + 50 {
                dt = dt.with_year(dt.year() - 100)?;
            }
            Some(dt.and_utc())
        })
        .or_else(|| {
            NaiveDateTime::parse_from_str(value, "%a %b %e %H:%M:%S %Y")
                .ok()
                .map(|dt| dt.and_utc())
        })?;
    Some(RetryHint::Until(date.max(now)))
}

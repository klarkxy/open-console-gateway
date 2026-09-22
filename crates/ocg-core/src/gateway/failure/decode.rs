//! Small static error dialects. No database, selector, retries or state writes.
//!
//! Quota exhaustion is not inferred from upstream error prose. A 429 carries
//! Retry-After and a short temporary backoff; official usage is the Go quota
//! authority. Zen Free keeps its declared shared-egress scope without a window
//! parsed from the body.
use super::{Cause, FailureFacts, RetryHint, Scope};
use crate::models::UsageWindowKind;
use chrono::{DateTime, Datelike, Duration, NaiveDateTime, Utc};
use ocg_gateway::classify::{ErrorProfile, ProviderErrorClass};

/// Default Key-scoped wait when an upstream 429 has no usable Retry-After.
pub(crate) const TEMPORARY_429_SECS: i64 = 30;

pub(crate) fn decode(
    class: ProviderErrorClass,
    _body: &str,
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
        rule_version: 2,
    };
    match class {
        ProviderErrorClass::RateLimited {
            profile:
                ErrorProfile::OpenCodeGo | ErrorProfile::GenericHttp | ErrorProfile::CommandCodeGoat,
        } => {
            facts.cause = Cause::Transient;
            facts.rule_id = "http.429.temporary";
        }
        ProviderErrorClass::RateLimited {
            profile: ErrorProfile::ZenFree,
        } => {
            // Anonymous Free is an existing, declared shared-egress contract.
            // Never let misleading paid-plan words move this onto a Key, and
            // never copy a reset instant from error prose.
            facts.cause = Cause::Transient;
            facts.scope = Scope::SharedFreeEgress;
            facts.window = Some(UsageWindowKind::Free);
            facts.retry_not_before = Some(temporary_429_deadline(retry_after, observed_at));
            facts.rule_id = "zen.free_egress";
        }
        _ => return None,
    }
    Some(facts)
}

/// Short Key-scoped 429 wait: honor a valid future Retry-After, otherwise 30s.
/// Zero/past Retry-After is not extra wait, so the default applies. Unbounded
/// stays unbounded. Callers must still max() against an existing longer wait.
pub(crate) fn temporary_429_deadline(retry_after: Option<&str>, now: DateTime<Utc>) -> RetryHint {
    match retry_after.and_then(|value| parse_retry_after(value, now)) {
        Some(RetryHint::Unbounded) => RetryHint::Unbounded,
        Some(RetryHint::Until(at)) if at > now => RetryHint::Until(at),
        _ => RetryHint::Until(now + Duration::seconds(TEMPORARY_429_SECS)),
    }
}

/// Wall-clock form of [`temporary_429_deadline`] for persisted cooldown rows
/// that cannot represent Unbounded. The maximum timestamp preserves the wait
/// without inventing a named quota window.
pub(crate) fn temporary_429_until(retry_after: Option<&str>, now: DateTime<Utc>) -> DateTime<Utc> {
    match temporary_429_deadline(retry_after, now) {
        RetryHint::Until(at) => at,
        RetryHint::Unbounded => DateTime::<Utc>::MAX_UTC,
    }
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

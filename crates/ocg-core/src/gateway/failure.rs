//! Provider-independent facts and decisions for rejected inference attempts.
//! A decoder may report evidence. Only this policy chooses persistence and
//! scheduling. HTTP status, quota reset and local probe eligibility are distinct.
use crate::models::UsageWindowKind;
use chrono::{DateTime, Utc};
use serde::Serialize;

pub(crate) mod decode;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Cause {
    QuotaExhausted,
    CreditsExhausted,
    Transient,
    Unknown,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Scope {
    QuotaPool,
    SharedFreeEgress,
    Unspecified,
}

/// Valid but unrepresentably distant waits fail closed until operator reset.
/// They are not discarded or shortened to a convenient local cap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(tag = "kind", content = "at", rename_all = "snake_case")]
pub(crate) enum RetryHint {
    Until(DateTime<Utc>),
    Unbounded,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct FailureFacts {
    pub cause: Cause,
    pub scope: Scope,
    pub window: Option<UsageWindowKind>,
    pub upstream_reset_at: Option<DateTime<Utc>>,
    pub retry_not_before: Option<RetryHint>,
    pub rule_id: &'static str,
    pub rule_version: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FailureDecision {
    pub persist_reset: Option<(UsageWindowKind, DateTime<Utc>)>,
    pub wait_for_recovery: bool,
    pub retry_not_before: Option<RetryHint>,
    pub exhaust_free: bool,
}

impl FailureFacts {
    pub(crate) fn decide(&self) -> FailureDecision {
        let known_scope = matches!(self.scope, Scope::QuotaPool | Scope::SharedFreeEgress);
        let persist_reset = if known_scope && self.cause == Cause::QuotaExhausted {
            self.window.zip(self.upstream_reset_at)
        } else {
            None
        };
        FailureDecision {
            persist_reset,
            wait_for_recovery: known_scope
                && (self.cause == Cause::CreditsExhausted
                    || (self.cause == Cause::QuotaExhausted && persist_reset.is_none())),
            // This never replaces a separate plan reset. Both must be satisfied.
            retry_not_before: self.retry_not_before,
            exhaust_free: self.scope == Scope::SharedFreeEgress,
        }
    }
}

#[cfg(test)]
mod tests;

//! Per-Key quota exhaustion, recovery wait, and one-shot trial gating.
//!
//! Confirmed exhaustion is owned by credential id + version + epoch. Ordinary
//! cooldown columns stay untouched. Probing is process-local and is not
//! persisted.

use chrono::{DateTime, Duration, Utc};
use ocg_gateway::quota::{QuotaEvidence, QuotaReason, QuotaWindowKind};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub(crate) use ocg_gateway::quota::{
    MiniMaxEnvelope, minimax_envelope, recognize_quota, recognize_quota_in_sse,
};

const FIRST_UNKNOWN: Duration = Duration::minutes(15);
const SECOND_UNKNOWN: Duration = Duration::hours(1);
const CAPPED_UNKNOWN: Duration = Duration::hours(6);
const FAILED_TRIAL_RETRY: Duration = Duration::minutes(15);
const MAX_KNOWN_RESET: Duration = Duration::days(40);

/// Identity of one in-flight or recorded recovery episode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct QuotaEpisode {
    pub credential_id: String,
    pub account_id: String,
    pub credential_version: u64,
    pub epoch: u64,
    pub key_cipher: String,
}

/// Persisted recovery facts. Status is presentation-only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct PersistedQuotaRecovery {
    pub epoch: u64,
    pub reason: PersistedQuotaReason,
    pub windows: BTreeMap<PersistedQuotaWindow, Option<DateTime<Utc>>>,
    pub observed_at: DateTime<Utc>,
    pub next_retry_at: DateTime<Utc>,
    pub failure_count: u32,
    /// Proven no-reset quota errors. Unknown backoff advances only on these.
    #[serde(default)]
    unknown_hits: u32,
    /// Eligibility floor from no-reset windows; not a provider `resetsAt`.
    #[serde(default)]
    unknown_retry_at: Option<DateTime<Utc>>,
    /// 15m floor after timeout/cancel/5xx. Distinct from crash-safe `next_retry_at`.
    #[serde(default)]
    nonquota_floor_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PersistedQuotaReason {
    QuotaExhausted,
    InsufficientBalance,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PersistedQuotaWindow {
    FiveHours,
    Week,
    Month,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum QuotaPresentationStatus {
    Waiting,
    Ready,
    Probing,
}

/// Wire-facing recovery row after applying the probing overlay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct QuotaRecoveryView {
    pub status: QuotaPresentationStatus,
    pub reason: PersistedQuotaReason,
    pub window: PersistedQuotaWindow,
    pub observed_at: DateTime<Utc>,
    pub resets_at: Option<DateTime<Utc>>,
    pub next_retry_at: DateTime<Utc>,
    pub failure_count: u32,
}

impl From<QuotaReason> for PersistedQuotaReason {
    fn from(value: QuotaReason) -> Self {
        match value {
            QuotaReason::QuotaExhausted => Self::QuotaExhausted,
            QuotaReason::InsufficientBalance => Self::InsufficientBalance,
        }
    }
}

impl From<QuotaWindowKind> for PersistedQuotaWindow {
    fn from(value: QuotaWindowKind) -> Self {
        match value {
            QuotaWindowKind::FiveHours => Self::FiveHours,
            QuotaWindowKind::Week => Self::Week,
            QuotaWindowKind::Month => Self::Month,
            QuotaWindowKind::Unknown => Self::Unknown,
        }
    }
}

impl PersistedQuotaRecovery {
    pub(crate) fn from_evidence(
        previous: Option<&Self>,
        evidence: &QuotaEvidence,
        observed_at: DateTime<Utc>,
        episode: Option<&QuotaEpisode>,
    ) -> Self {
        let (epoch, matching_trial) = match (previous, episode) {
            (Some(prev), Some(captured)) if captured.epoch == prev.epoch => (prev.epoch, true),
            (Some(prev), _) => (prev.epoch.saturating_add(1), false),
            _ => (1, false),
        };
        let mut windows = previous.map(|row| row.windows.clone()).unwrap_or_default();
        let window = PersistedQuotaWindow::from(evidence.window);
        let resets_at = parse_evidence_reset(evidence, observed_at);
        windows
            .entry(window)
            .and_modify(|existing| *existing = later_instant(*existing, resets_at))
            .or_insert(resets_at);
        let failure_count = match previous {
            None => 1,
            Some(prev) if matching_trial => prev.failure_count.saturating_add(1),
            Some(prev) => prev.failure_count,
        };
        let mut unknown_hits = previous.map(|row| row.unknown_hits).unwrap_or(0);
        let mut unknown_retry_at = previous.and_then(|row| row.unknown_retry_at);
        if resets_at.is_none() {
            if matching_trial || unknown_hits == 0 {
                unknown_hits = unknown_hits.saturating_add(1);
            }
            let candidate = observed_at + unknown_backoff(unknown_hits);
            unknown_retry_at = later_instant(unknown_retry_at, Some(candidate));
        }
        let nonquota_floor_at = previous.and_then(|row| row.nonquota_floor_at);
        let next_retry_at =
            next_retry_for(&windows, unknown_retry_at, nonquota_floor_at, observed_at);
        Self {
            epoch,
            reason: merge_reason(previous.map(|row| row.reason), evidence.reason.into()),
            windows,
            observed_at,
            next_retry_at,
            failure_count,
            unknown_hits,
            unknown_retry_at,
            nonquota_floor_at,
        }
    }

    pub(crate) fn with_crash_safe_retry(&self, now: DateTime<Utc>) -> Self {
        let mut next = self.clone();
        let floor = now + FAILED_TRIAL_RETRY;
        let restriction = next_retry_for(
            &next.windows,
            next.unknown_retry_at,
            next.nonquota_floor_at,
            now,
        );
        next.next_retry_at = next.next_retry_at.max(restriction).max(floor);
        next
    }

    pub(crate) fn with_nonquota_trial_failure(&self, now: DateTime<Utc>) -> Self {
        let mut next = self.clone();
        let floor = now + FAILED_TRIAL_RETRY;
        next.nonquota_floor_at = later_instant(next.nonquota_floor_at, Some(floor));
        let restriction = next_retry_for(
            &next.windows,
            next.unknown_retry_at,
            next.nonquota_floor_at,
            now,
        );
        next.next_retry_at = next.next_retry_at.max(restriction).max(floor);
        next
    }

    pub(crate) fn due_at(&self, now: DateTime<Utc>) -> bool {
        self.next_retry_at <= now
    }

    pub(crate) fn present(&self, now: DateTime<Utc>, probing: bool) -> QuotaRecoveryView {
        let (window, resets_at) = present_window(&self.windows, now, self.unknown_retry_at);
        QuotaRecoveryView {
            status: if probing {
                QuotaPresentationStatus::Probing
            } else if self.next_retry_at > now {
                QuotaPresentationStatus::Waiting
            } else {
                QuotaPresentationStatus::Ready
            },
            reason: self.reason,
            window,
            observed_at: self.observed_at,
            resets_at,
            next_retry_at: self.next_retry_at,
            failure_count: self.failure_count,
        }
    }
}

fn merge_reason(
    previous: Option<PersistedQuotaReason>,
    incoming: PersistedQuotaReason,
) -> PersistedQuotaReason {
    match (previous, incoming) {
        (Some(PersistedQuotaReason::QuotaExhausted), _)
        | (_, PersistedQuotaReason::QuotaExhausted) => PersistedQuotaReason::QuotaExhausted,
        _ => incoming,
    }
}

fn later_instant(
    left: Option<DateTime<Utc>>,
    right: Option<DateTime<Utc>>,
) -> Option<DateTime<Utc>> {
    match (left, right) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    }
}

fn present_window(
    windows: &BTreeMap<PersistedQuotaWindow, Option<DateTime<Utc>>>,
    now: DateTime<Utc>,
    unknown_retry_at: Option<DateTime<Utc>>,
) -> (PersistedQuotaWindow, Option<DateTime<Utc>>) {
    let mut best: Option<(DateTime<Utc>, PersistedQuotaWindow, Option<DateTime<Utc>>)> = None;
    for (kind, resets) in windows {
        let (deadline, honest) = match *resets {
            Some(at) if at > now => (at, Some(at)),
            None => match unknown_retry_at.filter(|at| *at > now) {
                Some(at) => (at, None),
                None => continue,
            },
            Some(_) => continue,
        };
        best = Some(select_controlling(best, (deadline, *kind, honest)));
    }
    if let Some((_, kind, honest)) = best {
        return (kind, honest);
    }

    let mut latest_named: Option<(DateTime<Utc>, PersistedQuotaWindow)> = None;
    let mut named_without_reset: Option<PersistedQuotaWindow> = None;
    for (kind, resets) in windows {
        if *kind == PersistedQuotaWindow::Unknown {
            continue;
        }
        match *resets {
            Some(at) => {
                latest_named = Some(match latest_named {
                    Some((prev, prev_kind)) if prev > at || (prev == at && prev_kind > *kind) => {
                        (prev, prev_kind)
                    }
                    _ => (at, *kind),
                });
            }
            None => {
                named_without_reset =
                    Some(named_without_reset.map_or(*kind, |prev| prev.max(*kind)));
            }
        }
    }
    if let Some((at, kind)) = latest_named {
        return (kind, Some(at));
    }
    if let Some(kind) = named_without_reset {
        return (kind, None);
    }
    (PersistedQuotaWindow::Unknown, None)
}

fn select_controlling(
    current: Option<(DateTime<Utc>, PersistedQuotaWindow, Option<DateTime<Utc>>)>,
    candidate: (DateTime<Utc>, PersistedQuotaWindow, Option<DateTime<Utc>>),
) -> (DateTime<Utc>, PersistedQuotaWindow, Option<DateTime<Utc>>) {
    let Some(current) = current else {
        return candidate;
    };
    if candidate.0 != current.0 {
        return if candidate.0 > current.0 {
            candidate
        } else {
            current
        };
    }
    match (current.1, candidate.1) {
        (PersistedQuotaWindow::Unknown, other) if other != PersistedQuotaWindow::Unknown => {
            candidate
        }
        (other, PersistedQuotaWindow::Unknown) if other != PersistedQuotaWindow::Unknown => current,
        (left, right) if right > left => candidate,
        _ => current,
    }
}

fn next_retry_for(
    windows: &BTreeMap<PersistedQuotaWindow, Option<DateTime<Utc>>>,
    unknown_retry_at: Option<DateTime<Utc>>,
    nonquota_floor_at: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> DateTime<Utc> {
    let mut retry: Option<DateTime<Utc>> = None;
    for reset in windows.values() {
        if let Some(at) = *reset
            && at > now
        {
            retry = Some(retry.map_or(at, |prev| prev.max(at)));
        }
    }
    for at in [unknown_retry_at, nonquota_floor_at].into_iter().flatten() {
        if at > now {
            retry = Some(retry.map_or(at, |prev| prev.max(at)));
        }
    }
    retry.unwrap_or(now)
}

fn unknown_backoff(failure_count: u32) -> Duration {
    match failure_count {
        0 | 1 => FIRST_UNKNOWN,
        2 => SECOND_UNKNOWN,
        _ => CAPPED_UNKNOWN,
    }
}

fn parse_evidence_reset(
    evidence: &QuotaEvidence,
    observed_at: DateTime<Utc>,
) -> Option<DateTime<Utc>> {
    if let Some(text) = evidence.resets_at_rfc3339.as_deref()
        && let Ok(parsed) = DateTime::parse_from_rfc3339(text)
    {
        let at = parsed.with_timezone(&Utc);
        if trustworthy_deadline(at, observed_at) {
            return Some(at);
        }
    }
    if let Some(text) = evidence.resets_in_text.as_deref()
        && let Some(duration) = crate::upstream_limit::parse_reset(text)
    {
        let at = observed_at + duration;
        if trustworthy_deadline(at, observed_at) {
            return Some(at);
        }
    }
    None
}

fn trustworthy_deadline(at: DateTime<Utc>, observed_at: DateTime<Utc>) -> bool {
    let remaining = at.signed_duration_since(observed_at);
    remaining > Duration::zero() && remaining <= MAX_KNOWN_RESET
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum QuotaAcquire {
    NotInRecovery,
    Trial(QuotaEpisode),
    SkipWaiting,
    SkipProbing,
}

pub(crate) fn host_quota_evidence(
    status: u16,
    provider_id: &str,
    body: &str,
) -> Option<QuotaEvidence> {
    recognize_quota(status, provider_id, body)
}

pub(crate) fn host_quota_evidence_sse(
    status: u16,
    provider_id: &str,
    chunk: &str,
) -> Option<QuotaEvidence> {
    recognize_quota_in_sse(status, provider_id, chunk)
}

#[cfg(test)]
mod tests;

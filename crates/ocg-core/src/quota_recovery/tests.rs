use super::*;
use ocg_gateway::quota::{QuotaEvidence, QuotaReason, QuotaWindowKind};

fn at(value: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(value)
        .unwrap()
        .with_timezone(&Utc)
}

fn evidence(
    window: QuotaWindowKind,
    resets: Option<&str>,
    resets_in: Option<&str>,
) -> QuotaEvidence {
    QuotaEvidence {
        reason: QuotaReason::QuotaExhausted,
        window,
        resets_at_rfc3339: resets.map(str::to_string),
        resets_in_text: resets_in.map(str::to_string),
    }
}

fn trial(epoch: u64) -> QuotaEpisode {
    QuotaEpisode {
        credential_id: "c".into(),
        account_id: "a".into(),
        credential_version: 1,
        epoch,
        key_cipher: "k".into(),
    }
}

#[test]
fn unknown_backoff_is_15m_then_1h_then_6h() {
    let now = at("2026-09-20T00:00:00Z");
    let first = PersistedQuotaRecovery::from_evidence(
        None,
        &evidence(QuotaWindowKind::Unknown, None, None),
        now,
        None,
    );
    assert_eq!(first.failure_count, 1);
    assert_eq!(first.epoch, 1);
    assert_eq!(first.next_retry_at, now + Duration::minutes(15));

    let second = PersistedQuotaRecovery::from_evidence(
        Some(&first),
        &evidence(QuotaWindowKind::Unknown, None, None),
        now,
        Some(&trial(1)),
    );
    assert_eq!(second.epoch, 1);
    assert_eq!(second.failure_count, 2);
    assert_eq!(second.next_retry_at, now + Duration::hours(1));

    let third = PersistedQuotaRecovery::from_evidence(
        Some(&second),
        &evidence(QuotaWindowKind::Unknown, None, None),
        now,
        Some(&trial(1)),
    );
    assert_eq!(third.failure_count, 3);
    assert_eq!(third.next_retry_at, now + Duration::hours(6));
}

#[test]
fn concurrent_nontrial_evidence_does_not_advance_unknown_backoff() {
    let now = at("2026-09-20T00:00:00Z");
    let first = PersistedQuotaRecovery::from_evidence(
        None,
        &evidence(QuotaWindowKind::Unknown, None, None),
        now,
        None,
    );
    let second = PersistedQuotaRecovery::from_evidence(
        Some(&first),
        &evidence(QuotaWindowKind::Unknown, None, None),
        now + Duration::seconds(1),
        None,
    );
    let third = PersistedQuotaRecovery::from_evidence(
        Some(&second),
        &evidence(QuotaWindowKind::Unknown, None, None),
        now + Duration::seconds(2),
        None,
    );
    assert_eq!(first.failure_count, 1);
    assert_eq!(second.failure_count, 1);
    assert_eq!(third.failure_count, 1);
    assert_eq!(first.next_retry_at, now + Duration::minutes(15));
    assert_eq!(
        second.next_retry_at,
        now + Duration::seconds(1) + Duration::minutes(15)
    );
    assert_eq!(
        third.next_retry_at,
        now + Duration::seconds(2) + Duration::minutes(15)
    );
    assert!(third.epoch > first.epoch);
    assert_eq!(
        third.windows.get(&PersistedQuotaWindow::Unknown).copied(),
        Some(None)
    );
}

#[test]
fn known_deadline_waits_and_keeps_later_simultaneous_windows() {
    let now = at("2026-09-20T00:00:00Z");
    let five = PersistedQuotaRecovery::from_evidence(
        None,
        &evidence(
            QuotaWindowKind::FiveHours,
            Some("2026-09-20T05:00:00Z"),
            None,
        ),
        now,
        None,
    );
    assert_eq!(five.next_retry_at, at("2026-09-20T05:00:00Z"));
    let both = PersistedQuotaRecovery::from_evidence(
        Some(&five),
        &evidence(QuotaWindowKind::Week, Some("2026-09-27T00:00:00Z"), None),
        now,
        None,
    );
    assert_eq!(both.epoch, 2);
    assert_eq!(both.next_retry_at, at("2026-09-27T00:00:00Z"));
    assert_eq!(both.windows.len(), 2);
    let view = both.present(now, false);
    assert_eq!(view.status, QuotaPresentationStatus::Waiting);
    assert_eq!(view.window, PersistedQuotaWindow::Week);
    assert_eq!(view.resets_at, Some(at("2026-09-27T00:00:00Z")));
    let after_week = both.present(at("2026-09-27T00:00:01Z"), false);
    assert_eq!(after_week.status, QuotaPresentationStatus::Ready);
    assert_eq!(after_week.window, PersistedQuotaWindow::Week);
    assert_eq!(after_week.resets_at, Some(at("2026-09-27T00:00:00Z")));
}

#[test]
fn nonquota_trial_failure_does_not_advance_the_quota_step() {
    let now = at("2026-09-20T00:00:00Z");
    let first = PersistedQuotaRecovery::from_evidence(
        None,
        &evidence(QuotaWindowKind::Unknown, None, None),
        now,
        None,
    );
    let released = first.with_nonquota_trial_failure(now);
    assert_eq!(released.failure_count, 1);
    assert_eq!(released.next_retry_at, now + Duration::minutes(15));
}

#[test]
fn stale_episode_does_not_reuse_the_current_epoch() {
    let now = at("2026-09-20T00:00:00Z");
    let first = PersistedQuotaRecovery::from_evidence(
        None,
        &evidence(QuotaWindowKind::Unknown, None, None),
        now,
        None,
    );
    let later = PersistedQuotaRecovery::from_evidence(
        Some(&first),
        &evidence(QuotaWindowKind::Unknown, None, None),
        now,
        Some(&trial(0)),
    );
    assert_eq!(later.epoch, 2);
    assert_eq!(later.failure_count, 1);
    assert_eq!(later.next_retry_at, now + Duration::minutes(15));
}

#[test]
fn stale_epoch_evidence_cannot_advance_current_trial_count() {
    let now = at("2026-09-20T00:00:00Z");
    let first = PersistedQuotaRecovery::from_evidence(
        None,
        &evidence(QuotaWindowKind::Unknown, None, None),
        now,
        None,
    );
    let verified = PersistedQuotaRecovery::from_evidence(
        Some(&first),
        &evidence(QuotaWindowKind::Unknown, None, None),
        now,
        Some(&trial(1)),
    );
    assert_eq!(verified.epoch, 1);
    assert_eq!(verified.failure_count, 2);
    assert_eq!(verified.next_retry_at, now + Duration::hours(1));

    let stale = PersistedQuotaRecovery::from_evidence(
        Some(&verified),
        &evidence(QuotaWindowKind::Unknown, None, None),
        now,
        Some(&trial(0)),
    );
    assert_eq!(stale.epoch, 2);
    assert_eq!(stale.failure_count, 2);
    assert_eq!(stale.next_retry_at, now + Duration::hours(1));
}

#[test]
fn go_resets_in_text_becomes_a_known_deadline() {
    let now = at("2026-09-20T00:00:00Z");
    let weekly = PersistedQuotaRecovery::from_evidence(
        None,
        &evidence(
            QuotaWindowKind::Week,
            None,
            Some("Weekly usage limit reached. Resets in 3 days."),
        ),
        now,
        None,
    );
    assert_eq!(weekly.next_retry_at, now + Duration::days(3));
    assert_eq!(
        weekly.windows.get(&PersistedQuotaWindow::Week).copied(),
        Some(Some(now + Duration::days(3)))
    );
}

#[test]
fn unknown_then_known_keeps_the_longer_unknown_wait() {
    let now = at("2026-09-20T00:00:00Z");
    let unknown = PersistedQuotaRecovery::from_evidence(
        None,
        &evidence(QuotaWindowKind::Unknown, None, None),
        now,
        None,
    );
    assert_eq!(unknown.next_retry_at, now + Duration::minutes(15));
    let mixed = PersistedQuotaRecovery::from_evidence(
        Some(&unknown),
        &evidence(
            QuotaWindowKind::FiveHours,
            Some("2026-09-20T00:05:00Z"),
            None,
        ),
        now,
        None,
    );
    assert_eq!(mixed.windows.len(), 2);
    assert_eq!(mixed.failure_count, 1);
    assert_eq!(mixed.epoch, 2);
    assert_eq!(mixed.next_retry_at, now + Duration::minutes(15));
    assert_eq!(
        mixed.windows.get(&PersistedQuotaWindow::FiveHours).copied(),
        Some(Some(at("2026-09-20T00:05:00Z")))
    );
    let view = mixed.present(now, false);
    assert_eq!(view.status, QuotaPresentationStatus::Waiting);
    assert_eq!(view.window, PersistedQuotaWindow::Unknown);
    assert_eq!(view.resets_at, None);
}

#[test]
fn known_then_named_without_reset_keeps_the_unknown_backoff() {
    let now = at("2026-09-20T00:00:00Z");
    let known = PersistedQuotaRecovery::from_evidence(
        None,
        &evidence(
            QuotaWindowKind::FiveHours,
            Some("2026-09-20T00:05:00Z"),
            None,
        ),
        now,
        None,
    );
    assert_eq!(known.next_retry_at, at("2026-09-20T00:05:00Z"));
    let mixed = PersistedQuotaRecovery::from_evidence(
        Some(&known),
        &evidence(QuotaWindowKind::Month, None, None),
        now,
        None,
    );
    assert_eq!(mixed.windows.len(), 2);
    assert_eq!(mixed.failure_count, 1);
    assert_eq!(mixed.next_retry_at, now + Duration::minutes(15));
    assert_eq!(
        mixed.windows.get(&PersistedQuotaWindow::Month).copied(),
        Some(None)
    );
    let view = mixed.present(now, false);
    assert_eq!(view.window, PersistedQuotaWindow::Month);
    assert_eq!(view.resets_at, None);
    assert_eq!(view.status, QuotaPresentationStatus::Waiting);

    let second_unknown = PersistedQuotaRecovery::from_evidence(
        Some(&mixed),
        &evidence(QuotaWindowKind::Unknown, None, None),
        now,
        None,
    );
    assert_eq!(second_unknown.failure_count, 1);
    assert_eq!(second_unknown.next_retry_at, now + Duration::minutes(15));

    let after_trial = PersistedQuotaRecovery::from_evidence(
        Some(&second_unknown),
        &evidence(QuotaWindowKind::Unknown, None, None),
        now,
        Some(&trial(second_unknown.epoch)),
    );
    assert_eq!(after_trial.failure_count, 2);
    assert_eq!(after_trial.next_retry_at, now + Duration::hours(1));
}

#[test]
fn nonquota_floor_is_preserved_across_a_shorter_known_reset() {
    let now = at("2026-09-20T00:00:00Z");
    let known = PersistedQuotaRecovery::from_evidence(
        None,
        &evidence(
            QuotaWindowKind::FiveHours,
            Some("2026-09-20T00:05:00Z"),
            None,
        ),
        now,
        None,
    );
    let floored = known.with_nonquota_trial_failure(now);
    assert_eq!(floored.failure_count, 1);
    assert_eq!(floored.next_retry_at, now + Duration::minutes(15));
    let again = PersistedQuotaRecovery::from_evidence(
        Some(&floored),
        &evidence(
            QuotaWindowKind::FiveHours,
            Some("2026-09-20T00:05:00Z"),
            None,
        ),
        now,
        None,
    );
    assert_eq!(again.failure_count, 1);
    assert_eq!(again.next_retry_at, now + Duration::minutes(15));
    assert_eq!(
        again.windows.get(&PersistedQuotaWindow::FiveHours).copied(),
        Some(Some(at("2026-09-20T00:05:00Z")))
    );
}

#[test]
fn trial_outcome_preserves_manual_due_known_and_unknown_deadlines() {
    let now = at("2026-09-20T00:00:00Z");
    let unknown = PersistedQuotaRecovery::from_evidence(
        None,
        &evidence(QuotaWindowKind::Unknown, None, None),
        now,
        None,
    );
    let stepped = PersistedQuotaRecovery::from_evidence(
        Some(&unknown),
        &evidence(QuotaWindowKind::Unknown, None, None),
        now,
        Some(&trial(1)),
    );
    let capped = PersistedQuotaRecovery::from_evidence(
        Some(&stepped),
        &evidence(QuotaWindowKind::Unknown, None, None),
        now,
        Some(&trial(1)),
    );
    assert_eq!(capped.next_retry_at, now + Duration::hours(6));
    let weekly = PersistedQuotaRecovery::from_evidence(
        Some(&capped),
        &evidence(QuotaWindowKind::Week, Some("2026-09-27T00:00:00Z"), None),
        now,
        None,
    );
    let deadline = at("2026-09-27T00:00:00Z");
    assert_eq!(weekly.next_retry_at, deadline);
    let mut manual = weekly.clone();
    manual.next_retry_at = now;

    let crash_safe = manual.with_crash_safe_retry(now);
    assert_eq!(crash_safe.next_retry_at, deadline);
    assert_eq!(crash_safe.failure_count, weekly.failure_count);
    assert_eq!(crash_safe.unknown_hits, weekly.unknown_hits);
    assert_eq!(crash_safe.unknown_retry_at, weekly.unknown_retry_at);
    assert_eq!(crash_safe.nonquota_floor_at, None);

    let nonquota = manual.with_nonquota_trial_failure(now);
    assert_eq!(nonquota.next_retry_at, deadline);
    assert_eq!(nonquota.failure_count, weekly.failure_count);
    assert_eq!(nonquota.unknown_hits, weekly.unknown_hits);
    assert_eq!(nonquota.unknown_retry_at, weekly.unknown_retry_at);
    assert_eq!(
        nonquota.nonquota_floor_at,
        Some(now + Duration::minutes(15))
    );
}

#[test]
fn trial_outcome_on_expired_restriction_waits_fifteen_minutes() {
    let now = at("2026-09-20T00:00:00Z");
    let weekly = PersistedQuotaRecovery::from_evidence(
        None,
        &evidence(QuotaWindowKind::Week, Some("2026-09-27T00:00:00Z"), None),
        now,
        None,
    );
    let later = at("2026-09-27T00:00:01Z");
    let mut expired = weekly.clone();
    expired.next_retry_at = later;

    let crash_safe = expired.with_crash_safe_retry(later);
    assert_eq!(crash_safe.next_retry_at, later + Duration::minutes(15));
    assert_eq!(crash_safe.failure_count, weekly.failure_count);
    assert_eq!(crash_safe.unknown_hits, weekly.unknown_hits);
    assert_eq!(crash_safe.nonquota_floor_at, None);

    let nonquota = expired.with_nonquota_trial_failure(later);
    assert_eq!(nonquota.next_retry_at, later + Duration::minutes(15));
    assert_eq!(nonquota.failure_count, weekly.failure_count);
    assert_eq!(nonquota.unknown_hits, weekly.unknown_hits);
    assert_eq!(
        nonquota.nonquota_floor_at,
        Some(later + Duration::minutes(15))
    );
}

#[test]
fn crash_safe_floor_does_not_override_conclusive_known_reset() {
    let now = at("2026-09-20T00:00:00Z");
    let unknown = PersistedQuotaRecovery::from_evidence(
        None,
        &evidence(QuotaWindowKind::Unknown, None, None),
        now,
        None,
    );
    let due = now + Duration::minutes(15);
    let crash_safe = unknown.with_crash_safe_retry(due);
    assert_eq!(crash_safe.next_retry_at, due + Duration::minutes(15));
    let conclusive = PersistedQuotaRecovery::from_evidence(
        Some(&crash_safe),
        &evidence(
            QuotaWindowKind::FiveHours,
            Some("2026-09-20T00:20:00Z"),
            None,
        ),
        due,
        None,
    );
    assert_eq!(conclusive.next_retry_at, at("2026-09-20T00:20:00Z"));
    assert!(
        conclusive
            .windows
            .contains_key(&PersistedQuotaWindow::Unknown)
    );
    assert_eq!(
        conclusive
            .windows
            .get(&PersistedQuotaWindow::FiveHours)
            .copied(),
        Some(Some(at("2026-09-20T00:20:00Z")))
    );
}

#[test]
fn past_reset_is_not_an_active_restriction_for_a_new_known_deadline() {
    let now = at("2026-09-20T00:00:00Z");
    let week = PersistedQuotaRecovery::from_evidence(
        None,
        &evidence(QuotaWindowKind::Week, Some("2026-09-27T00:00:00Z"), None),
        now,
        None,
    );
    let due = at("2026-09-27T00:00:01Z");
    let five = PersistedQuotaRecovery::from_evidence(
        Some(&week),
        &evidence(
            QuotaWindowKind::FiveHours,
            Some("2026-09-27T05:00:01Z"),
            None,
        ),
        due,
        None,
    );
    assert_eq!(five.windows.len(), 2);
    assert_eq!(five.next_retry_at, at("2026-09-27T05:00:01Z"));
    let view = five.present(due, false);
    assert_eq!(view.status, QuotaPresentationStatus::Waiting);
    assert_eq!(view.window, PersistedQuotaWindow::FiveHours);
    assert_eq!(view.resets_at, Some(at("2026-09-27T05:00:01Z")));
}

#[test]
fn named_window_without_reset_stays_named_when_waiting_ready_or_probing() {
    let now = at("2026-09-20T00:00:00Z");
    let row = PersistedQuotaRecovery::from_evidence(
        None,
        &evidence(QuotaWindowKind::Month, None, None),
        now,
        None,
    );
    let waiting = row.present(now, false);
    assert_eq!(waiting.status, QuotaPresentationStatus::Waiting);
    assert_eq!(waiting.window, PersistedQuotaWindow::Month);
    assert_eq!(waiting.resets_at, None);
    assert_eq!(waiting.next_retry_at, now + Duration::minutes(15));

    let ready = row.present(now + Duration::hours(1), false);
    assert_eq!(ready.status, QuotaPresentationStatus::Ready);
    assert_eq!(ready.window, PersistedQuotaWindow::Month);
    assert_eq!(ready.resets_at, None);

    let probing = row.present(now + Duration::hours(1), true);
    assert_eq!(probing.status, QuotaPresentationStatus::Probing);
    assert_eq!(probing.window, PersistedQuotaWindow::Month);
    assert_eq!(probing.resets_at, None);
}

#[test]
fn probing_overlay_wins_presentation() {
    let now = at("2026-09-20T00:00:00Z");
    let row = PersistedQuotaRecovery::from_evidence(
        None,
        &evidence(QuotaWindowKind::Unknown, None, None),
        now,
        None,
    );
    assert_eq!(
        row.present(now + Duration::hours(1), true).status,
        QuotaPresentationStatus::Probing
    );
}

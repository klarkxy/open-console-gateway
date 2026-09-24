use super::*;
use crate::go_usage::GoUsageSnapshot;

fn snapshot(
    rolling: GoUsageWindowStatus,
    weekly: GoUsageWindowStatus,
    monthly: GoUsageWindowStatus,
) -> GoUsageSnapshot {
    GoUsageSnapshot {
        rolling_status: rolling,
        weekly_status: weekly,
        monthly_status: monthly,
        rolling_percent: 100.0,
        weekly_percent: 100.0,
        monthly_percent: 100.0,
        rolling_resets_in_minutes: 30,
        weekly_resets_in_minutes: 1440,
        monthly_resets_in_minutes: 43200,
        earliest_resets_in_minutes: 30,
    }
}
#[test]
fn percent_full_ok_status_is_not_exhaustion() {
    assert!(
        official_go_quota_evidence(
            &snapshot(
                GoUsageWindowStatus::Ok,
                GoUsageWindowStatus::Ok,
                GoUsageWindowStatus::Ok
            ),
            chrono::Utc::now()
        )
        .is_empty()
    );
}
#[test]
fn every_limited_window_keeps_its_own_reset_deadline() {
    let now = chrono::Utc::now();
    let evidence = official_go_quota_evidence(
        &snapshot(
            GoUsageWindowStatus::RateLimited,
            GoUsageWindowStatus::RateLimited,
            GoUsageWindowStatus::RateLimited,
        ),
        now,
    );
    assert_eq!(evidence.len(), 3);
    assert_eq!(evidence[0].window, QuotaWindowKind::FiveHours);
    assert_eq!(
        evidence[0].resets_at_rfc3339,
        Some((now + chrono::Duration::minutes(30)).to_rfc3339())
    );
    assert_eq!(evidence[1].window, QuotaWindowKind::Week);
    assert_eq!(
        evidence[1].resets_at_rfc3339,
        Some((now + chrono::Duration::days(1)).to_rfc3339())
    );
    assert_eq!(evidence[2].window, QuotaWindowKind::Month);
    assert_eq!(
        evidence[2].resets_at_rfc3339,
        Some((now + chrono::Duration::days(30)).to_rfc3339())
    );
}

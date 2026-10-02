use super::*;
use chrono::TimeZone;

fn at(text: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(text)
        .unwrap()
        .with_timezone(&Utc)
}

fn sentence(window: &str, reset: &str) -> String {
    format!(
        r#"{{"error":{{"code":"RATE_LIMITED","type":"rate_limit_error","message":"You've reached your {window} usage limit for your plan. Your limit resets at {reset}. Please wait for the window to reset or upgrade your plan to continue."}}}}"#
    )
}

#[test]
fn exact_sentences_map_only_the_three_known_windows() {
    let observed = at("2026-10-01T00:00:00Z");
    let reset = "2026-10-02T12:25:45.241Z";
    for (token, window) in [
        ("5-hour", UsageWindowKind::FiveHours),
        ("weekly", UsageWindowKind::Week),
        ("monthly", UsageWindowKind::Month),
    ] {
        let parsed = declared_plan_window(&sentence(token, reset), observed).unwrap();
        assert_eq!(parsed.0, window);
        assert_eq!(parsed.1, at(reset));
    }
}

#[test]
fn grammar_type_code_time_and_profile_misses_are_not_a_plan_window() {
    let observed = at("2026-10-01T00:00:00Z");
    let reset = "2026-10-02T12:25:45.241Z";
    let good = sentence("weekly", reset);
    assert!(declared_plan_window(&good, observed).is_some());
    for body in [
        good.replace("RATE_LIMITED", "rate_limited"),
        good.replace("rate_limit_error", "RateLimitError"),
        good.replace("weekly", "week"),
        good.replace("weekly", "daily"),
        sentence("weekly", "tomorrow"),
        sentence("weekly", "2026-10-01T00:00:00Z"),
        sentence("weekly", "2026-09-30T00:00:00Z"),
        good.trim_end_matches("}").to_string(),
        r#"{"error":{"message":"You've reached your weekly usage limit for your plan. Your limit resets at 2026-10-02T12:25:45.241Z. Please wait for the window to reset or upgrade your plan to continue.","type":"rate_limit_error"}}"#.to_string(),
        format!("{good} trailing"),
    ] {
        assert!(
            declared_plan_window(&body, observed).is_none(),
            "{body}"
        );
    }
    assert!(declared_plan_window(&good, at(reset)).is_none());
}

#[test]
fn stored_map_rejects_unknown_windows_and_bad_timestamps_and_keeps_the_later_deadline() {
    let later = at("2026-10-02T12:25:45.241Z");
    let earlier = at("2026-10-02T00:00:00Z");
    let mut map = GoatPlanCooldowns {
        week: Some(later),
        ..GoatPlanCooldowns::default()
    };
    assert!(!map.raise(UsageWindowKind::Week, earlier));
    assert!(map.raise(UsageWindowKind::FiveHours, earlier));
    assert_eq!(map.week, Some(later));
    assert_eq!(map.latest(), Some(later));
    let json = serde_json::to_string(&map).unwrap();
    let restored: GoatPlanCooldowns = serde_json::from_str(&json).unwrap();
    assert_eq!(restored, map);
    assert!(
        serde_json::from_str::<GoatPlanCooldowns>(r#"{"day":"2026-10-02T00:00:00Z"}"#).is_err()
    );
    assert!(serde_json::from_str::<GoatPlanCooldowns>(r#"{"week":"next tuesday"}"#).is_err());
    assert!(Utc.timestamp_opt(0, 0).single().is_some());
}

#[test]
fn overlay_keeps_the_later_instant_without_inventing_one() {
    let early = at("2026-10-02T00:00:00Z");
    let late = at("2026-10-02T12:25:45.241Z");
    assert_eq!(overlay_instant(Some(early), Some(late)), Some(late));
    assert_eq!(overlay_instant(Some(late), Some(early)), Some(late));
    assert_eq!(overlay_instant(None, Some(early)), Some(early));
    assert_eq!(overlay_instant(Some(early), None), Some(early));
    assert_eq!(overlay_instant(None, None), None);
}

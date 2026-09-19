use super::decode::{decode, parse_retry_after};
use super::*;
use chrono::Duration;
use ocg_gateway::classify::{ErrorProfile, ProviderErrorClass};

fn now() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-09-19T00:00:00Z")
        .unwrap()
        .with_timezone(&Utc)
}
fn rate(profile: ErrorProfile, body: &str, retry: Option<&str>) -> FailureFacts {
    decode(
        ProviderErrorClass::RateLimited { profile },
        body,
        retry,
        now(),
    )
    .unwrap()
}
const GOAT: &str = r#"{"error":{"code":"RATE_LIMITED","type":"rate_limit_error","message":"You've reached your weekly usage limit for your plan. Your limit resets at 2026-09-20T00:00:00Z."}}"#;

#[test]
fn unknown_and_transient_never_invent_account_cooldown() {
    for profile in [
        ErrorProfile::GenericHttp,
        ErrorProfile::CommandCodeGoat,
        ErrorProfile::OpenCodeGo,
    ] {
        for body in [
            "",
            "not json",
            "{}",
            r#"{"error":{"type":"rate_limit_error","message":"Upstream model provider is temporarily unavailable. Please try again in a moment."}}"#,
        ] {
            let f = rate(profile, body, None);
            assert_eq!(f.scope, Scope::Unspecified);
            assert_eq!(f.decide().persist_reset, None);
            assert!(!f.decide().wait_for_recovery);
        }
    }
}

#[test]
fn dialects_translate_to_the_same_policy_without_cross_provider_guessing() {
    let goat = rate(ErrorProfile::CommandCodeGoat, GOAT, None);
    let go = rate(
        ErrorProfile::OpenCodeGo,
        r#"{"error":{"type":"GoUsageLimitError","message":"Weekly usage limit reached. Resets in 1 day."}}"#,
        None,
    );
    assert_eq!(goat.decide(), go.decide());
    assert_eq!(
        goat.decide().persist_reset,
        Some((UsageWindowKind::Week, now() + Duration::days(1)))
    );
    for body in [GOAT, "Weekly usage limit reached. Resets in 1 day."] {
        let generic = rate(ErrorProfile::GenericHttp, body, None);
        assert_eq!(generic.cause, Cause::Unknown);
        assert!(!generic.decide().wait_for_recovery);
        assert_eq!(generic.decide().persist_reset, None);
    }
}

#[test]
fn retry_after_and_quota_reset_are_independent_constraints() {
    for delay in ["10", "172800"] {
        let f = rate(ErrorProfile::CommandCodeGoat, GOAT, Some(delay));
        let d = f.decide();
        assert_eq!(
            d.persist_reset,
            Some((UsageWindowKind::Week, now() + Duration::days(1)))
        );
        assert_eq!(
            d.retry_not_before,
            Some(RetryHint::Until(
                now() + Duration::seconds(delay.parse().unwrap())
            ))
        );
    }
    let f = rate(ErrorProfile::GenericHttp, "{}", Some("90"));
    assert_eq!(f.scope, Scope::Unspecified);
    assert_eq!(f.decide().persist_reset, None);
    assert!(!f.decide().wait_for_recovery);
    assert!(f.retry_not_before.is_some());
}

#[test]
fn known_exhaustion_without_reset_is_pending_not_a_fake_deadline() {
    let credit = decode(ProviderErrorClass::InsufficientCredits, "", None, now()).unwrap();
    assert!(credit.decide().wait_for_recovery);
    assert!(credit.decide().persist_reset.is_none());
    let go = rate(ErrorProfile::OpenCodeGo, "Weekly usage limit reached", None);
    assert!(go.decide().wait_for_recovery);
    assert_eq!(go.window, Some(UsageWindowKind::Week));
    assert!(go.upstream_reset_at.is_none());
    let free = rate(ErrorProfile::ZenFree, "5-hour usage limit reached", None);
    assert_eq!(free.scope, Scope::SharedFreeEgress);
    assert_eq!(free.window, Some(UsageWindowKind::Free));
    assert!(free.decide().exhaust_free);
    assert!(free.decide().wait_for_recovery);
    assert!(free.upstream_reset_at.is_none());
}

#[test]
fn other_errors_never_become_retryable_from_headers_or_lookalike_text() {
    for class in [
        ProviderErrorClass::ClientError,
        ProviderErrorClass::ServerError,
        ProviderErrorClass::UnauthorizedPassthrough,
    ] {
        assert!(decode(class, GOAT, Some("60"), now()).is_none());
    }
    for body in [
        r#"{"error":{"type":"ModelError","message":"Weekly usage limit reached. Resets in 1 day."}}"#,
        r#"{"error":{"message":"bad model"},"echo":"Weekly usage limit reached. Resets in 1 day."}"#,
    ] {
        assert!(
            rate(ErrorProfile::OpenCodeGo, body, None)
                .decide()
                .persist_reset
                .is_none()
        );
    }
}

#[test]
fn retry_after_handles_seconds_zero_long_overflow_dates_and_bad_values() {
    assert_eq!(parse_retry_after("0", now()), Some(RetryHint::Until(now())));
    assert_eq!(
        parse_retry_after(" 3456000 ", now()),
        Some(RetryHint::Until(now() + Duration::days(40)))
    );
    assert_eq!(
        parse_retry_after("999999999999999999999999999999999", now()),
        Some(RetryHint::Unbounded)
    );
    assert_eq!(
        parse_retry_after("9223372036854775807", now()),
        Some(RetryHint::Unbounded)
    );
    let past = DateTime::parse_from_rfc3339("1994-11-06T08:49:37Z")
        .unwrap()
        .with_timezone(&Utc);
    for value in [
        "Sun, 06 Nov 1994 08:49:37 GMT",
        "Sunday, 06-Nov-94 08:49:37 GMT",
        "Sun Nov  6 08:49:37 1994",
    ] {
        assert_eq!(
            parse_retry_after(value, past - Duration::seconds(20)),
            Some(RetryHint::Until(past)),
            "{value}"
        );
        assert_eq!(
            parse_retry_after(value, now()),
            Some(RetryHint::Until(now())),
            "{value}"
        );
    }
    for value in [
        "",
        "-1",
        "+30",
        "NaN",
        "1.5",
        "forever",
        "Wed, 99 Nov 2026 00:00:00 GMT",
    ] {
        assert_eq!(parse_retry_after(value, now()), None, "{value}");
    }
}

#[test]
fn malformed_huge_reset_text_cannot_panic_or_persist() {
    for body in [
        "Weekly usage limit reached. Resets in 9223372036854775807 days.",
        "Weekly usage limit reached. Resets in 0 min.",
    ] {
        let f = rate(ErrorProfile::OpenCodeGo, body, None);
        assert_eq!(f.upstream_reset_at, None);
        assert!(f.decide().persist_reset.is_none());
    }
}

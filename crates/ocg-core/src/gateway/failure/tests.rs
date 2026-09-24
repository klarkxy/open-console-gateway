use super::decode::{decode, openrouter_free_rejection, parse_retry_after};
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
fn error_prose_does_not_establish_quota_for_any_provider() {
    let goat = rate(ErrorProfile::CommandCodeGoat, GOAT, None);
    let go = rate(
        ErrorProfile::OpenCodeGo,
        r#"{"error":{"type":"GoUsageLimitError","message":"Weekly usage limit reached. Resets in 1 day."}}"#,
        None,
    );
    assert_eq!(goat.decide(), go.decide());
    assert_eq!(goat.decide().persist_reset, None);
    for body in [GOAT, "Weekly usage limit reached. Resets in 1 day."] {
        let generic = rate(ErrorProfile::GenericHttp, body, None);
        assert_eq!(generic.cause, Cause::Transient);
        assert!(!generic.decide().wait_for_recovery);
        assert_eq!(generic.decide().persist_reset, None);
    }
}

#[test]
fn retry_after_and_quota_reset_are_independent_constraints() {
    for delay in ["10", "172800"] {
        let f = rate(ErrorProfile::CommandCodeGoat, GOAT, Some(delay));
        let d = f.decide();
        assert_eq!(d.persist_reset, None);
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
    assert!(decode(ProviderErrorClass::InsufficientCredits, "", None, now()).is_none());
    let go = rate(ErrorProfile::OpenCodeGo, "Weekly usage limit reached", None);
    assert!(!go.decide().wait_for_recovery);
    assert_eq!(go.window, None);
    assert!(go.upstream_reset_at.is_none());
    let free = rate(ErrorProfile::ZenFree, "5-hour usage limit reached", None);
    assert_eq!(free.scope, Scope::SharedFreeEgress);
    assert_eq!(free.window, Some(UsageWindowKind::Free));
    assert!(free.decide().exhaust_free);
    assert!(free.decide().wait_for_recovery);
    assert!(free.upstream_reset_at.is_none());
}

#[test]
fn zen_free_rejection_waits_briefly_without_inventing_quota_exhaustion() {
    let facts = decode(
        ProviderErrorClass::FreeRejected,
        "Weekly usage limit reached. Resets in 1 day.",
        None,
        now(),
    )
    .unwrap();
    assert_eq!(facts.scope, Scope::SharedFreeEgress);
    assert_eq!(facts.window, None);
    assert_eq!(facts.upstream_reset_at, None);
    assert_eq!(facts.decide().persist_reset, None);
    assert!(facts.decide().wait_for_recovery);
    assert_eq!(
        facts.retry_not_before,
        Some(RetryHint::Until(now() + Duration::seconds(30)))
    );
}

#[test]
fn openrouter_free_rejection_is_temporary_and_has_no_persistent_quota_window() {
    let facts = openrouter_free_rejection(Some("90"), now());
    assert_eq!(facts.scope, Scope::QuotaPool);
    assert_eq!(facts.window, None);
    assert_eq!(facts.decide().persist_reset, None);
    assert!(!facts.decide().wait_for_recovery);
    assert_eq!(
        facts.retry_not_before,
        Some(RetryHint::Until(now() + Duration::seconds(90)))
    );
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

#[test]
fn diagnostic_serialization_preserves_window_and_evidence() {
    let value =
        serde_json::to_value(rate(ErrorProfile::CommandCodeGoat, GOAT, Some("90"))).unwrap();
    assert!(value["window"].is_null());
    assert_eq!(value["rule_id"], "http.429.temporary");
    assert_eq!(value["rule_version"], 2);
    assert!(value.get("body").is_none());
}
#[test]
fn echoed_limit_text_outside_message_is_not_account_evidence() {
    let f = rate(
        ErrorProfile::OpenCodeGo,
        r#"{"error":{},"echo":"Weekly usage limit reached. Resets in 1 day."}"#,
        None,
    );
    assert_eq!(f.scope, Scope::Unspecified);
    assert!(f.decide().persist_reset.is_none());
    assert!(!f.decide().wait_for_recovery);
}

#[test]
fn temporary_backoff_uses_valid_retry_after_or_thirty_seconds() {
    use super::decode::temporary_429_deadline;
    for value in [None, Some("bad"), Some("0")] {
        assert_eq!(
            temporary_429_deadline(value, now()),
            RetryHint::Until(now() + Duration::seconds(30))
        );
    }
    assert_eq!(
        temporary_429_deadline(Some("120"), now()),
        RetryHint::Until(now() + Duration::seconds(120))
    );
    assert_eq!(
        temporary_429_deadline(Some("99999999999999999999999999"), now()),
        RetryHint::Unbounded
    );
}

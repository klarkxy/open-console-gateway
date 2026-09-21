use super::*;
use ocg_domain::ids::{
    COMMAND_CODE_PROVIDER_ID, CPA_PROVIDER_ID, CUSTOM_PROVIDER_ID, KIMI_PROVIDER_ID,
    MINIMAX_PROVIDER_ID, OLLAMA_PROVIDER_ID, OPENCODE_PROVIDER_ID, OPENCODE_ZEN_FREE_PROVIDER_ID,
};

fn evidence(
    status: u16,
    provider: &str,
    body: &str,
) -> Option<(QuotaReason, QuotaWindowKind, Option<String>)> {
    recognize_quota(status, provider, body).map(|e| (e.reason, e.window, e.resets_at_rfc3339))
}

#[test]
fn kimi_only_explicit_403_messages_are_quota() {
    let five = r#"{"error":{"message":"You've reached your 5-hour usage limit"}}"#;
    let week = r#"{"error":{"message":"You've reached your weekly (7-day) usage limit"}}"#;
    let month =
        r#"{"error":{"message":"You've reached your monthly usage limit for this billing cycle"}}"#;
    assert_eq!(
        evidence(403, KIMI_PROVIDER_ID, five),
        Some((
            QuotaReason::QuotaExhausted,
            QuotaWindowKind::FiveHours,
            None
        ))
    );
    assert_eq!(
        evidence(403, KIMI_PROVIDER_ID, week),
        Some((QuotaReason::QuotaExhausted, QuotaWindowKind::Week, None))
    );
    assert_eq!(
        evidence(403, KIMI_PROVIDER_ID, month),
        Some((QuotaReason::QuotaExhausted, QuotaWindowKind::Month, None))
    );
    assert_eq!(
        evidence(
            403,
            KIMI_PROVIDER_ID,
            "You've reached your 5-hour usage limit"
        ),
        Some((
            QuotaReason::QuotaExhausted,
            QuotaWindowKind::FiveHours,
            None
        ))
    );
    for not_quota in [
        r#"{"error":{"message":"concurrency limit exceeded"}}"#,
        r#"{"error":{"message":"invalid api key"}}"#,
        r#"{"error":{"message":"You've reached your weekly usage limit for your plan"}}"#,
    ] {
        assert_eq!(
            evidence(403, KIMI_PROVIDER_ID, not_quota),
            None,
            "{not_quota}"
        );
    }
    assert_eq!(
        evidence(429, KIMI_PROVIDER_ID, five),
        None,
        "Kimi quota is 403-only"
    );
}

#[test]
fn go_usage_window_429_is_quota_and_credits_error_is_not() {
    let weekly = r#"{"type":"error","error":{"type":"GoUsageLimitError","message":"Weekly usage limit reached. Resets in 3 days."}}"#;
    let got = recognize_quota(429, OPENCODE_PROVIDER_ID, weekly).unwrap();
    assert_eq!(got.reason, QuotaReason::QuotaExhausted);
    assert_eq!(got.window, QuotaWindowKind::Week);
    assert!(got.resets_in_text.is_some());

    let credits =
        r#"{"type":"error","error":{"type":"CreditsError","message":"No active subscription"}}"#;
    assert_eq!(evidence(401, OPENCODE_PROVIDER_ID, credits), None);
    assert_eq!(evidence(429, OPENCODE_PROVIDER_ID, credits), None);

    let free = r#"{"type":"FreeUsageLimitError","message":"Free usage exceeded"}"#;
    assert_eq!(evidence(429, OPENCODE_PROVIDER_ID, free), None);

    assert_eq!(
        evidence(429, OPENCODE_PROVIDER_ID, "rate limit exceeded"),
        None
    );
}

#[test]
fn goat_plan_limit_is_quota_even_without_a_reset_timestamp() {
    let weekly = r#"{"error":{"code":"RATE_LIMITED","message":"You've reached your weekly usage limit for your plan. Your limit resets at 2026-09-08T09:56:18.379Z.","type":"rate_limit_error"}}"#;
    let got = recognize_quota(429, COMMAND_CODE_PROVIDER_ID, weekly).unwrap();
    assert_eq!(got.window, QuotaWindowKind::Week);
    assert_eq!(
        got.resets_at_rfc3339.as_deref(),
        Some("2026-09-08T09:56:18.379Z")
    );

    let missing_reset = r#"{"error":{"code":"RATE_LIMITED","message":"You've reached your 5-hour usage limit for your plan. Please wait.","type":"rate_limit_error"}}"#;
    let got = recognize_quota(429, COMMAND_CODE_PROVIDER_ID, missing_reset).unwrap();
    assert_eq!(got.window, QuotaWindowKind::FiveHours);
    assert_eq!(got.resets_at_rfc3339, None);

    let credits = r#"{"error":{"code":"BAD_REQUEST","type":"invalid_request_error","message":"You have insufficient credits to make this request."}}"#;
    assert_eq!(
        evidence(400, COMMAND_CODE_PROVIDER_ID, credits),
        Some((
            QuotaReason::InsufficientBalance,
            QuotaWindowKind::Unknown,
            None
        ))
    );
    assert_eq!(
        evidence(
            400,
            COMMAND_CODE_PROVIDER_ID,
            r#"{"error":{"code":"BAD_REQUEST","type":"invalid_request_error","message":"maximum context length exceeded"}}"#
        ),
        None
    );
    assert_eq!(
        evidence(429, COMMAND_CODE_PROVIDER_ID, "rate limited"),
        None
    );
}

#[test]
fn minimax_structured_codes_and_http_200_envelopes() {
    let balance = r#"{"base_resp":{"status_code":1008,"status_msg":"insufficient balance"}}"#;
    assert_eq!(
        evidence(200, MINIMAX_PROVIDER_ID, balance),
        Some((
            QuotaReason::InsufficientBalance,
            QuotaWindowKind::Unknown,
            None
        ))
    );
    let plan = r#"{"base_resp":{"status_code":2056,"status_msg":"token plan exhausted"}}"#;
    assert_eq!(
        evidence(200, MINIMAX_PROVIDER_ID, plan),
        Some((QuotaReason::QuotaExhausted, QuotaWindowKind::Unknown, None))
    );
    let temporary = r#"{"base_resp":{"status_code":1002,"status_msg":"busy"}}"#;
    assert_eq!(evidence(200, MINIMAX_PROVIDER_ID, temporary), None);
    assert_eq!(
        minimax_envelope(temporary),
        Some(MiniMaxEnvelope::Temporary)
    );
    assert_eq!(evidence(200, MINIMAX_PROVIDER_ID, r#"{"id":"ok"}"#), None);

    let sse = "event: error\ndata: {\"base_resp\":{\"status_code\":2056}}\n\n";
    assert_eq!(
        recognize_quota_in_sse(200, MINIMAX_PROVIDER_ID, sse).map(|e| e.window),
        Some(QuotaWindowKind::Unknown)
    );
}

#[test]
fn configurable_http_uses_strict_error_code_or_type_only() {
    let quota = r#"{"error":{"code":"insufficient_quota","message":"plan empty"}}"#;
    assert_eq!(
        evidence(429, CUSTOM_PROVIDER_ID, quota),
        Some((QuotaReason::QuotaExhausted, QuotaWindowKind::Unknown, None))
    );
    let balance = r#"{"error":{"type":"insufficient_balance"}}"#;
    assert_eq!(
        evidence(200, CUSTOM_PROVIDER_ID, balance),
        Some((
            QuotaReason::InsufficientBalance,
            QuotaWindowKind::Unknown,
            None
        ))
    );
    for not_quota in [
        r#"{"error":{"message":"You've reached your 5-hour usage limit"}}"#,
        r#"{"error":{"code":"rate_limit_exceeded"}}"#,
        r#"{"error":{"type":"usage_limit"}}"#,
        "insufficient_quota",
        r#"{"error":{"code":"insufficient_quota_exceeded"}}"#,
    ] {
        assert_eq!(
            evidence(429, CUSTOM_PROVIDER_ID, not_quota),
            None,
            "{not_quota}"
        );
    }
    let sse = "data: {\"error\":{\"code\":\"insufficient_quota\"}}\n\n";
    assert!(recognize_quota_in_sse(200, CUSTOM_PROVIDER_ID, sse).is_some());
}

#[test]
fn configurable_http_matches_code_or_type_independently() {
    assert_eq!(
        evidence(
            429,
            CUSTOM_PROVIDER_ID,
            r#"{"error":{"code":"429","type":"insufficient_quota"}}"#
        ),
        Some((QuotaReason::QuotaExhausted, QuotaWindowKind::Unknown, None))
    );
    assert_eq!(
        evidence(
            429,
            CUSTOM_PROVIDER_ID,
            r#"{"error":{"code":"insufficient_balance","type":"insufficient_quota"}}"#
        ),
        Some((QuotaReason::QuotaExhausted, QuotaWindowKind::Unknown, None))
    );
    assert_eq!(
        evidence(
            429,
            CUSTOM_PROVIDER_ID,
            r#"{"error":{"code":"insufficient_quota","type":"insufficient_balance"}}"#
        ),
        Some((QuotaReason::QuotaExhausted, QuotaWindowKind::Unknown, None))
    );
    assert_eq!(
        evidence(
            429,
            CUSTOM_PROVIDER_ID,
            r#"{"error":{"code":"insufficient_balance","type":"rate_limit_exceeded"}}"#
        ),
        Some((
            QuotaReason::InsufficientBalance,
            QuotaWindowKind::Unknown,
            None
        ))
    );
    assert_eq!(
        evidence(
            429,
            CUSTOM_PROVIDER_ID,
            r#"{"error":{"code":"429","type":"rate_limit_exceeded"}}"#
        ),
        None
    );
}

#[test]
fn zen_cpa_ollama_and_bare_statuses_never_establish_exhaustion() {
    let kimi = r#"{"error":{"message":"You've reached your 5-hour usage limit"}}"#;
    assert_eq!(evidence(403, OPENCODE_ZEN_FREE_PROVIDER_ID, kimi), None);
    assert_eq!(evidence(403, CPA_PROVIDER_ID, kimi), None);
    assert_eq!(
        evidence(
            429,
            OLLAMA_PROVIDER_ID,
            r#"{"error":{"code":"insufficient_quota"}}"#
        ),
        None
    );
    assert_eq!(evidence(403, KIMI_PROVIDER_ID, "{}"), None);
    assert_eq!(evidence(429, CUSTOM_PROVIDER_ID, "{}"), None);
    assert_eq!(evidence(429, OPENCODE_PROVIDER_ID, ""), None);
}

#[test]
fn sse_error_envelopes_on_http_200_use_provider_quota_rules() {
    let kimi_month = concat!(
        "event: error\n",
        "data: {\"error\":{\"message\":\"You've reached your monthly usage limit for this billing cycle\"}}\n\n",
    );
    assert_eq!(
        recognize_quota_in_sse(200, KIMI_PROVIDER_ID, kimi_month).map(|e| e.window),
        Some(QuotaWindowKind::Month)
    );
    assert_eq!(
        evidence(
            200,
            KIMI_PROVIDER_ID,
            r#"{"error":{"message":"You've reached your monthly usage limit for this billing cycle"}}"#
        ),
        None,
        "bare HTTP 200 JSON still requires 403"
    );

    let go_week = concat!(
        "event: error\n",
        "data: {\"type\":\"error\",\"error\":{\"type\":\"GoUsageLimitError\",\"message\":\"Weekly usage limit reached. Resets in 3 days.\"}}\n\n",
    );
    let go = recognize_quota_in_sse(200, OPENCODE_PROVIDER_ID, go_week).unwrap();
    assert_eq!(go.window, QuotaWindowKind::Week);
    assert!(go.resets_in_text.is_some());
    assert_eq!(
        evidence(
            200,
            OPENCODE_PROVIDER_ID,
            r#"{"type":"error","error":{"type":"GoUsageLimitError","message":"Weekly usage limit reached. Resets in 3 days."}}"#
        ),
        None
    );

    let goat_five = concat!(
        "event: error\n",
        "data: {\"error\":{\"code\":\"RATE_LIMITED\",\"message\":\"You've reached your 5-hour usage limit for your plan. Please wait.\",\"type\":\"rate_limit_error\"}}\n\n",
    );
    assert_eq!(
        recognize_quota_in_sse(200, COMMAND_CODE_PROVIDER_ID, goat_five).map(|e| e.window),
        Some(QuotaWindowKind::FiveHours)
    );
    assert_eq!(
        evidence(
            200,
            COMMAND_CODE_PROVIDER_ID,
            r#"{"error":{"code":"RATE_LIMITED","message":"You've reached your 5-hour usage limit for your plan. Please wait.","type":"rate_limit_error"}}"#
        ),
        None
    );
}

#[test]
fn sse_content_echo_and_non_quota_errors_are_not_exhaustion() {
    let echo = "data: {\"choices\":[{\"delta\":{\"content\":\"You've reached your monthly usage limit for this billing cycle\"}}]}\n\n";
    assert_eq!(recognize_quota_in_sse(200, KIMI_PROVIDER_ID, echo), None);

    let permission = concat!(
        "event: error\n",
        "data: {\"error\":{\"message\":\"invalid api key\"}}\n\n",
    );
    assert_eq!(
        recognize_quota_in_sse(200, KIMI_PROVIDER_ID, permission),
        None
    );

    let concurrency = concat!(
        "event: error\n",
        "data: {\"error\":{\"message\":\"concurrency limit exceeded\"}}\n\n",
    );
    assert_eq!(
        recognize_quota_in_sse(200, KIMI_PROVIDER_ID, concurrency),
        None
    );

    let credits = concat!(
        "event: error\n",
        "data: {\"type\":\"error\",\"error\":{\"type\":\"CreditsError\",\"message\":\"No active subscription\"}}\n\n",
    );
    assert_eq!(
        recognize_quota_in_sse(200, OPENCODE_PROVIDER_ID, credits),
        None
    );
    assert_eq!(
        recognize_quota_in_sse(429, OPENCODE_PROVIDER_ID, credits),
        None
    );
}

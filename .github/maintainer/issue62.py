from pathlib import Path

def replace(path, old, new):
    p=Path(path); s=p.read_text(); assert s.count(old)==1, (path,s.count(old),old[:100]); p.write_text(s.replace(old,new))

p=Path('crates/ocg-core/src/gateway/classify.rs'); s=p.read_text(); a=s.index('pub(crate) fn rate_limit_window_and_deadline('); b=s.index('\npub(crate) fn rate_limit_fallback',a)
s=s[:a]+'''/// None means this request may fall back, but there is no account cooldown evidence.
pub(crate) fn rate_limit_window_and_deadline(
    provider_id: &str,
    policy: RateLimitPolicy,
    text: &str,
    retry_after: Option<&str>,
    observed_at: DateTime<Utc>,
) -> Option<(Option<UsageWindowKind>, DateTime<Utc>)> {
    if provider_id == COMMAND_CODE_PROVIDER_ID
        && matches!(policy, RateLimitPolicy::GenericFiveMinute)
    {
        let limit = crate::command_code_rate_limit::parse_command_code_rate_limit(text, observed_at);
        // A valid upstream Retry-After wins. Retain a known window when present;
        // otherwise it is a generic upstream-advertised deadline, not a guess.
        if let Some(deadline) = retry_after.and_then(|value| parse_retry_after(value, observed_at)) {
            return Some((limit.map(|limit| limit.window), deadline));
        }
        return limit.map(|limit| (Some(limit.window), limit.resets_at));
    }
    let (window, cooldown) = rate_limit_window_and_cooldown(policy, text);
    Some((window, observed_at + cooldown))
}

fn parse_retry_after(value: &str, observed_at: DateTime<Utc>) -> Option<DateTime<Utc>> {
    let value = value.trim();
    let deadline = if !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()) {
        let seconds: i64 = value.parse().ok()?;
        observed_at.checked_add_signed(Duration::try_seconds(seconds)?)?
    } else {
        DateTime::parse_from_rfc2822(value).ok()?.with_timezone(&Utc)
    };
    let remaining = deadline.signed_duration_since(observed_at);
    (remaining > Duration::zero() && remaining <= Duration::days(31)).then_some(deadline)
}
''' + s[b:]
s=s.replace('''            body,
            observed_at,
        );''','''            body,
            None,
            observed_at,
        ).unwrap();''')
s=s.replace('fn goat_429_is_generic_and_ignores_go_limit_windows()', 'fn goat_429_without_account_evidence_does_not_cool_down()')
s=s.replace('''            let (window, cooldown) =
                rate_limit_window_and_cooldown(RateLimitPolicy::GenericFiveMinute, misleading_body);
            assert_eq!(window, None, "{misleading_body}");
            assert_eq!(cooldown, Duration::minutes(5), "{misleading_body}");''','''            assert_eq!(rate_limit_window_and_deadline(
                COMMAND_CODE_PROVIDER_ID, RateLimitPolicy::GenericFiveMinute,
                misleading_body, None, Utc::now()), None, "{misleading_body}");''')
p.write_text(s)
replace('crates/ocg-core/src/command_code_rate_limit.rs', '''    } else {
        return None;
    };

    let resets_at''', '''    } else if message_lower.contains("monthly usage limit for your plan") {
        (UsageWindowKind::Month, Duration::days(31))
    } else {
        return None;
    };

    let resets_at''')
replace('crates/ocg-core/src/gateway/forwarder.rs', '''                let observed_at = Utc::now();
                let (window, until) = rate_limit_window_and_deadline(
                    &account.provider_id,
                    policy,
                    &text,
                    observed_at,
                );
                let cooldown = until.signed_duration_since(observed_at);
                let sanitized = attempt_context.sanitize_upstream_error(&text);
                let error_message = format!(
                    "rate limited: {} (resets in {}s)",
                    sanitized,
                    cooldown.num_seconds()
                );''', '''                let observed_at = state.sample_gateway_clock().0;
                let cooldown = rate_limit_window_and_deadline(
                    &account.provider_id,
                    policy,
                    &text,
                    error_headers.get(reqwest::header::RETRY_AFTER).and_then(|value| value.to_str().ok()),
                    observed_at,
                );
                let window = cooldown.and_then(|(window, _)| window);
                let sanitized = attempt_context.sanitize_upstream_error(&text);
                let error_message = match cooldown {
                    Some((_, until)) => format!("rate limited: {} (resets in {}s)", sanitized, until.signed_duration_since(observed_at).num_seconds()),
                    None => format!("upstream temporarily rate limited: {sanitized}"),
                };''')
replace('crates/ocg-core/src/gateway/forwarder.rs', '''                    db.set_account_rate_limit_if_key_matches(
                        &account.id,
                        &account.key_cipher,
                        until,
                        &sanitized,
                        window,
                    )?;''','''                    if let Some((window, until)) = cooldown {
                        db.set_account_rate_limit_if_key_matches(
                            &account.id,
                            &account.key_cipher,
                            until,
                            &sanitized,
                            window,
                        )?;
                    }''')
replace('crates/ocg-core/tests/gateway_fallback.rs', '''    assert!(goat.cooldown_until.is_some());
    assert!(goat.cooldown_generic_until.is_some());''','''    assert!(goat.cooldown_until.is_none());
    assert!(goat.cooldown_generic_until.is_none());''')
p=Path('crates/ocg-core/src/gateway/classify.rs'); s=p.read_text(); pos=s.rfind('\n}')
s=s[:pos]+'''
    #[test]
    fn goat_retry_after_is_bounded_and_precedes_body_deadline() {
        let now = DateTime::parse_from_rfc3339("2026-09-17T12:00:00Z").unwrap().with_timezone(&Utc);
        let body = r#"{"error":{"code":"RATE_LIMITED","type":"rate_limit_error","message":"You've reached your monthly usage limit for your plan. Your limit resets at 2026-10-01T12:00:00Z."}}"#;
        for header in ["90", "Thu, 17 Sep 2026 12:01:30 GMT"] {
            assert_eq!(rate_limit_window_and_deadline(COMMAND_CODE_PROVIDER_ID, RateLimitPolicy::GenericFiveMinute, body, Some(header), now), Some((Some(UsageWindowKind::Month), now + Duration::seconds(90))));
        }
        for invalid in ["", "0", "-1", "+90", "forever", "999999999999999999999", "2764800", "Wed, 16 Sep 2026 12:00:00 GMT"] {
            assert_eq!(rate_limit_window_and_deadline(COMMAND_CODE_PROVIDER_ID, RateLimitPolicy::GenericFiveMinute, "{}", Some(invalid), now), None, "{invalid}");
        }
        assert_eq!(rate_limit_window_and_deadline(COMMAND_CODE_PROVIDER_ID, RateLimitPolicy::GenericFiveMinute, body, None, now).unwrap().0, Some(UsageWindowKind::Month));
    }
''' + s[pos:]; p.write_text(s)
Path('crates/ocg-core/tests/gateway_goat_transient.rs').write_text('''//! Temporary upstream failures must not mutate account availability or stickiness.
use axum::http::StatusCode;
use chrono::{Duration, Utc};
use ocg_core::gateway::provider_adapter::install_goat_loopback_route_for_test;
use ocg_core::models::RoutingMode;
use ocg_core::provider::{COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_ALIAS as MODEL, ZEN_FREE_ACCOUNT_ID};
#[path = "fixtures/gateway_fallback.rs"]
mod fixture;
use fixture::*;

const TRANSIENT: &str = r#"{"error":{"message":"Upstream model provider is temporarily unavailable. Please try again in a moment.","type":"rate_limit_error"}}"#;

#[tokio::test]
async fn goat_transient_falls_back_only_for_this_request_and_next_request_returns_to_a() {
    let p = PreparedFallback::routing(&[("key-a", &[reply(429, TRANSIENT), ok()]), ("key-b", &[ok()])], &["unused"], RoutingMode::StickyGlobal, false).await;
    let a = format!("goat-a-{}", uuid::Uuid::new_v4());
    let b = format!("goat-b-{}", uuid::Uuid::new_v4());
    create_goat_account(&p.state, "acct-1", &a, "key-a");
    create_goat_account(&p.state, "acct-1", &b, "key-b");
    let _a = install_goat_loopback_route_for_test(a.clone(), p.base_url.clone()).unwrap();
    let _b = install_goat_loopback_route_for_test(b.clone(), p.base_url.clone()).unwrap();
    p.state.db.lock().reorder_accounts(&[a.clone(), b.clone(), "acct-1".into(), ZEN_FREE_ACCOUNT_ID.into()]).unwrap();
    let h = p.bind().await;
    let before = h.account(&a);
    for _ in 0..2 {
        let (status, body) = h.protocol("/v1/chat/completions", MODEL).await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }
    assert_eq!(h.call_keys(), ["key-a", "key-b", "key-a"]);
    let after = h.account(&a);
    assert_eq!(after.cooldown_until, before.cooldown_until);
    assert_eq!(after.cooldown_generic_until, before.cooldown_generic_until);
    assert_eq!(after.last_error, before.last_error);
    assert_eq!(after.updated_at, before.updated_at);
    assert!(h.account(&b).cooldown_until.is_none());
    let logs = h.logs();
    let failed = logs.iter().find(|row| row.http_status == Some(429)).unwrap();
    assert_eq!(failed.account_id, a);
    let diagnostic = failed.diagnostic.as_ref().unwrap();
    assert_eq!(diagnostic["retry_action"], "try_next_account");
}

#[tokio::test]
async fn goat_retry_after_header_reaches_persisted_deadline() {
    let raw = format!("HTTP/1.1 429 Too Many Requests\\r\\nContent-Type: application/json\\r\\nRetry-After: 90\\r\\nContent-Length: {}\\r\\nConnection: close\\r\\n\\r\\n{}", TRANSIENT.len(), TRANSIENT).into_bytes();
    let (base, calls, stop) = start_raw_disconnect_upstream(raw).await;
    let (state, dir) = build_state(base.clone(), &["unused"]);
    let goat = prepare_goat(&state, "key-a", &[], true);
    let _route = install_goat_loopback_route_for_test(goat.clone(), base).unwrap();
    let h = FallbackHarness::from_parts(state, dir, Default::default(), Some(stop), Some(calls)).await;
    let before = Utc::now();
    let (status, _) = h.protocol("/v1/chat/completions", MODEL).await;
    assert_ne!(status, StatusCode::OK);
    let until = h.account(&goat).cooldown_generic_until.unwrap();
    assert!(until >= before + Duration::seconds(90));
    assert!(until <= Utc::now() + Duration::seconds(90));
    assert!(h.account(&goat).cooldown_week_until.is_none());
}
''')
for path, old, new in [
('docs/maintainer/runtime-invariants.md','malformed, expired, implausibly distant, or ordinary transient 429s retain the generic five-minute cooldown.', 'a valid bounded `Retry-After` takes precedence. Five-hour, weekly and monthly windows are recognized. Without a valid deadline, GOAT 429s only exclude the account from the current request and never write account cooldown or replace the global sticky target. Other Providers retain their existing cooldown rules.'),
('docs/maintainer/runtime-invariants.zh-CN.md','正文畸形、时间已过、距离异常或普通瞬时 429 仍使用通用 5 分钟冷却。','有效且有界的 `Retry-After` 优先。支持 5 小时、周和月窗口；没有有效截止时间的 GOAT 429 只在本次请求内排除账号，不写账号冷却、不改全局粘性目标。其他供应商保留原有冷却规则。')]:
    replace(path, old, new)
for path,text in [
('docs/user/routing.md','\n### GOAT transient 429 versus account exhaustion\n\nGOAT only persists an account cooldown when an upstream 429 supplies a valid bounded `Retry-After` (seconds or HTTP date), or a recognized plan window with an absolute reset time. Retry-After takes precedence. A temporary provider/model failure or an unknown response without a usable deadline only excludes the account from this request; the next request retains the global sticky target. Five-hour, weekly, and monthly reset windows are supported. Other Providers keep their existing policies.\n'),
('docs/user/routing.zh-CN.md','\n### GOAT 临时 429 与账号额度耗尽\n\nGOAT 仅在上游 429 给出有效且有界的 `Retry-After`（秒数或 HTTP 日期），或明确的套餐窗口与绝对重置时间时写入账号冷却；Retry-After 优先。临时供应商／模型故障以及没有有效截止时间的未知响应，只在本次请求中排除该账号，下次请求仍保留原全局粘性目标。支持 5 小时、周、月窗口；其他供应商的规则不变。\n')]:
    p=Path(path); s=p.read_text(); p.write_text(s.replace('\n## ',text+'\n## ',1))

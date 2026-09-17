from pathlib import Path

def replace(path, old, new):
    p = Path(path)
    s = p.read_text()
    assert s.count(old) == 1, (path, s.count(old), old[:100])
    p.write_text(s.replace(old, new))

replace('crates/ocg-gateway/src/classify.rs', '    ClientError,\n', '    ClientError,\n    /// Explicit account credit rejection with no advertised recovery time.\n    InsufficientCredits,\n')
replace('crates/ocg-gateway/src/classify.rs', '''    } else {
        base
    }
}

fn response_has_error_type''', '''    } else if status == 400
        && !anonymous
        && ProviderAdapterKind::from_provider_id(provider_id)
            == Some(ProviderAdapterKind::CommandCodeGoat)
        && response_has_insufficient_credits(response_body)
    {
        ProviderErrorClass::InsufficientCredits
    } else {
        base
    }
}

// Only the observed GOAT account-level envelope is evidence. A context-length,
// model, or reasoning validation error must never trigger another billable send.
fn response_has_insufficient_credits(body: &str) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(body) else {
        return false;
    };
    value.pointer("/error/code").and_then(Value::as_str) == Some("BAD_REQUEST")
        && value.pointer("/error/type").and_then(Value::as_str) == Some("invalid_request_error")
        && value.pointer("/error/message").and_then(Value::as_str).is_some_and(|message| {
            message == "You have insufficient credits to make this request."
                || message == "You have insufficient credits to make this request. Please purchase more credits to continue using the service."
        })
}

fn response_has_error_type''')
replace('crates/ocg-core/src/gateway/forwarder.rs', '''        | ProviderErrorClass::ForbiddenRotate => ForwardAction::TryNextAccount,''', '''        | ProviderErrorClass::ForbiddenRotate
        | ProviderErrorClass::InsufficientCredits => ForwardAction::TryNextAccount,''')
replace('crates/ocg-core/src/gateway/forwarder.rs', '''                // Other 4xx: request-level error. Convert its envelope for the caller,
                // but don't retry another account for the same invalid request.''', '''                // A proven GOAT credit rejection is account-scoped and may fall
                // through for this request only. It supplies no reset deadline:
                // do not invent a cooldown or mislabel it as an invalid Key.
                // Other 4xx remain request errors and never replay on another Key.''')
replace('crates/ocg-core/src/gateway/forwarder.rs', '''                return Ok(ForwardResult {
                    response,
                    action,
                    error_message: None,
                });''', '''                return Ok(ForwardResult {
                    response,
                    action,
                    error_message: (class == ProviderErrorClass::InsufficientCredits)
                        .then_some(message),
                });''')
p = Path('crates/ocg-gateway/src/classify/tests.rs')
p.write_text(p.read_text() + '''
#[test]
fn goat_insufficient_credits_is_not_a_generic_client_error() {
    let body = r#"{"error":{"code":"BAD_REQUEST","type":"invalid_request_error","message":"You have insufficient credits to make this request. Please purchase more credits to continue using the service."}}"#;
    assert_eq!(classify_http_response(400, COMMAND_CODE_PROVIDER_ID, false, false, body), ProviderErrorClass::InsufficientCredits);
    for provider in [OPENCODE_PROVIDER_ID, CUSTOM_PROVIDER_ID, "unknown-provider"] {
        assert_eq!(classify_http_response(400, provider, false, false, body), ProviderErrorClass::ClientError);
    }
    for invalid in ["not json", "{}", r#"{"error":{"message":"insufficient credits"}}"#,
        r#"{"error":{"code":"BAD_REQUEST","type":"invalid_request_error","message":"maximum context length exceeded"}}"#,
        r#"{"error":{"code":"BAD_REQUEST","type":"invalid_request_error","message":"The reasoning_content must be passed back"}}"#,
        r#"{"error":{"code":"BAD_REQUEST","type":"invalid_request_error","message":"unknown model"}}"#] {
        assert_eq!(classify_http_response(400, COMMAND_CODE_PROVIDER_ID, false, false, invalid), ProviderErrorClass::ClientError, "{invalid}");
    }
    assert_eq!(classify_http_response(413, COMMAND_CODE_PROVIDER_ID, false, false, body), ProviderErrorClass::ClientError);
    assert!(!ProviderErrorClass::InsufficientCredits.same_account_retry_eligible());
    assert!(!schedule_go_usage_sync(ProviderErrorClass::InsufficientCredits));
}
''')
Path('crates/ocg-core/tests/gateway_goat_credits.rs').write_text('''//! Account-credit 400s must fall through without inventing account state.
use axum::http::StatusCode;
use ocg_core::gateway::provider_adapter::install_goat_loopback_route_for_test;
use ocg_core::models::RoutingMode;
use ocg_core::provider::{COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_ALIAS as MODEL, ZEN_FREE_ACCOUNT_ID};
#[path = "fixtures/gateway_fallback.rs"]
mod fixture;
use fixture::*;

const CREDIT_ERROR: &str = r#"{"error":{"code":"BAD_REQUEST","message":"You have insufficient credits to make this request. Please purchase more credits to continue using the service.","type":"invalid_request_error"}}"#;

#[tokio::test]
async fn goat_credit_400_retries_another_key_and_keeps_unknown_recovery_explicit() {
    let p = PreparedFallback::routing(&[("a", &[reply(400, CREDIT_ERROR), reply(400, CREDIT_ERROR)]), ("b", &[ok(), ok()])], &["unused"], RoutingMode::StickyGlobal, false).await;
    let a = format!("goat-a-{}", uuid::Uuid::new_v4());
    let b = format!("goat-b-{}", uuid::Uuid::new_v4());
    create_goat_account(&p.state, "acct-1", &a, "a");
    create_goat_account(&p.state, "acct-1", &b, "b");
    let _a = install_goat_loopback_route_for_test(a.clone(), p.base_url.clone()).unwrap();
    let _b = install_goat_loopback_route_for_test(b.clone(), p.base_url.clone()).unwrap();
    p.state.db.lock().reorder_accounts(&[a.clone(), b.clone(), "acct-1".into(), ZEN_FREE_ACCOUNT_ID.into()]).unwrap();
    let h = p.bind().await;
    let before = h.account(&a);
    for _ in 0..2 {
        let (status, body) = h.protocol("/v1/chat/completions", MODEL).await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }
    assert_eq!(h.call_keys(), ["a", "b", "a", "b"]);
    let after = h.account(&a);
    assert_eq!(after.cooldown_until, before.cooldown_until);
    assert_eq!(after.auth_error, before.auth_error);
    assert_eq!(after.updated_at, before.updated_at);
    let logs = h.logs();
    let failed: Vec<_> = logs.iter().filter(|row| row.http_status == Some(400)).collect();
    assert_eq!(failed.len(), 2);
    for row in failed {
        assert_eq!(row.attempt, 1);
        let diagnostic: serde_json::Value = serde_json::from_str(row.diagnostic_json.as_deref().unwrap()).unwrap();
        assert_eq!(diagnostic["retry_action"], "try_next_account");
        assert!(row.cost.is_none());
    }
    assert_eq!(logs.iter().filter(|row| row.http_status == Some(200) && row.attempt == 2).count(), 2);
}

#[tokio::test]
async fn ordinary_goat_400_and_413_never_try_a_second_key() {
    for (status, body) in [(400, r#"{"error":{"message":"maximum context length exceeded"}}"#), (400, r#"{"error":{"message":"unknown model"}}"#), (413, CREDIT_ERROR)] {
        let (h, id) = start_goat(&[("goat-key", &[reply(status, body)]), ("open-key", &[ok()])], &[], true, true).await;
        let (actual, _) = h.protocol("/v1/chat/completions", MODEL).await;
        assert_eq!(actual.as_u16(), status);
        assert_eq!(h.call_keys(), ["goat-key"]);
        assert!(h.account(&id).cooldown_until.is_none());
        assert!(h.account(&id).auth_error.is_none());
    }
}
''')
for path, text in [
 ('docs/user/routing.md', '\n### GOAT credit rejection without a reset time\n\nAn exact GOAT `400` / `BAD_REQUEST` / `invalid_request_error` declaring insufficient credits is an account-level rejection, not a malformed prompt. The current request tries the next eligible account once per account. Without an upstream reset time, OCG does not invent a cooldown, disable the Key, or mark it as an authentication failure. New requests may try that account again; logs retain the upstream 400 and fallback action. Other 400s (context, model, reasoning validation), 413s, and similar messages from other Providers do not gain retry permission.\n'),
 ('docs/user/routing.zh-CN.md', '\n### GOAT 未给重置时间的余额不足\n\nGOAT 返回准确的 `400` / `BAD_REQUEST` / `invalid_request_error` 余额不足声明时，当前请求会尝试下一个合格账号，每个账号至多一次。这不是提示词格式错误。上游没有给恢复时间，所以不编造冷却、不禁用 Key，也不标记为鉴权失败；新请求仍可能重试该账号。日志保留真实的 400 和回退动作。上下文过长、模型或 reasoning 参数错误等普通 400、413 及其他供应商的相似文案仍不会获得重试权限。\n')]:
    p = Path(path)
    s = p.read_text()
    p.write_text(s.replace('\n---\n', text + '\n---\n', 1) if '\n---\n' in s else s + text)

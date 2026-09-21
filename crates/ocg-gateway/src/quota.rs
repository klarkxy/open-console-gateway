//! Strict per-Key quota exhaustion recognition.
//!
//! Confirmed exhaustion is evidence, not HTTP status. Bare 403/429/Retry-After
//! and unknown meters never establish it. Zen Free and CPA stay on their
//! existing special paths. Side effects stay in the host.

use ocg_domain::provider::ProviderAdapterKind;
use serde_json::Value;

/// Why a credential is confirmed exhausted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[doc(hidden)]
pub enum QuotaReason {
    QuotaExhausted,
    InsufficientBalance,
}

/// Usage window named by confirmed exhaustion evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[doc(hidden)]
pub enum QuotaWindowKind {
    FiveHours,
    Week,
    Month,
    Unknown,
}

/// Confirmed per-Key exhaustion extracted from a bounded upstream body.
#[derive(Debug, Clone, PartialEq, Eq)]
#[doc(hidden)]
pub struct QuotaEvidence {
    pub reason: QuotaReason,
    pub window: QuotaWindowKind,
    /// RFC3339 reset timestamp copied from a provider that supplies one.
    pub resets_at_rfc3339: Option<String>,
    /// OpenCode Go "Resets in …" text for the host duration parser.
    pub resets_in_text: Option<String>,
}

/// MiniMax `base_resp.status_code` values that are not success and not quota.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[doc(hidden)]
pub enum MiniMaxEnvelope {
    Success,
    Temporary,
    OtherError(i64),
}

const KIMI_FIVE_HOUR: &str = "You've reached your 5-hour usage limit";
const KIMI_WEEK: &str = "You've reached your weekly (7-day) usage limit";
const KIMI_MONTH: &str = "You've reached your monthly usage limit for this billing cycle";

const GOAT_FIVE_HOUR: &str = "5-hour usage limit for your plan";
const GOAT_FIVE_HOUR_ALT: &str = "5 hour usage limit for your plan";
const GOAT_WEEK: &str = "weekly usage limit for your plan";
const GOAT_MONTH: &str = "monthly usage limit for your plan";
const GOAT_RESET_MARKER: &str = "your limit resets at ";

const GOAT_INSUFFICIENT_SHORT: &str = "You have insufficient credits to make this request.";
const GOAT_INSUFFICIENT_LONG: &str = "You have insufficient credits to make this request. Please purchase more credits to continue using the service.";

const MINIMAX_BALANCE: i64 = 1008;
const MINIMAX_TOKEN_PLAN: i64 = 2056;
const MINIMAX_TEMPORARY: i64 = 1002;

/// Recognize confirmed quota exhaustion from one HTTP status and body.
///
/// `body` is already bounded by the caller. SSE frames should be passed
/// through [`recognize_quota_in_sse`].
#[doc(hidden)]
pub fn recognize_quota(status: u16, provider_id: &str, body: &str) -> Option<QuotaEvidence> {
    match adapter_kind(provider_id) {
        Some(ProviderAdapterKind::ZenFree) | Some(ProviderAdapterKind::Cpa) => None,
        Some(ProviderAdapterKind::KimiCn) => kimi_quota(status, body),
        Some(ProviderAdapterKind::OpenCodeGo) => go_quota(status, body),
        Some(ProviderAdapterKind::CommandCodeGoat) => goat_quota(status, body),
        Some(ProviderAdapterKind::MiniMaxCn) => minimax_quota(body),
        Some(ProviderAdapterKind::ConfigurableHttp) | None => http_quota(body),
        Some(ProviderAdapterKind::OllamaCloud) => None,
    }
}

/// Walk SSE `data:` JSON payloads and return the first confirmed exhaustion.
///
/// Structured SSE error events may normalize the HTTP transport status so
/// provider body rules still apply when the stream itself was HTTP 200.
/// Ordinary assistant content is not an error envelope.
#[doc(hidden)]
pub fn recognize_quota_in_sse(
    status: u16,
    provider_id: &str,
    chunk: &str,
) -> Option<QuotaEvidence> {
    if let Some(evidence) = recognize_quota(status, provider_id, chunk) {
        return Some(evidence);
    }
    for payload in sse_json_payloads(chunk) {
        if let Some(evidence) = recognize_quota(status, provider_id, payload) {
            return Some(evidence);
        }
    }
    for payload in sse_error_payloads(chunk) {
        if let Some(evidence) = recognize_quota_sse_error(provider_id, payload) {
            return Some(evidence);
        }
    }
    None
}

fn recognize_quota_sse_error(provider_id: &str, payload: &str) -> Option<QuotaEvidence> {
    match adapter_kind(provider_id) {
        Some(ProviderAdapterKind::ZenFree) | Some(ProviderAdapterKind::Cpa) => None,
        Some(ProviderAdapterKind::KimiCn) => kimi_quota_body(payload),
        Some(ProviderAdapterKind::OpenCodeGo) => go_quota_body(payload),
        Some(ProviderAdapterKind::CommandCodeGoat) => goat_plan_limit(payload),
        Some(ProviderAdapterKind::MiniMaxCn) => minimax_quota(payload),
        Some(ProviderAdapterKind::ConfigurableHttp) | None => http_quota_json(payload),
        Some(ProviderAdapterKind::OllamaCloud) => None,
    }
}

/// MiniMax structured envelope, including HTTP 200 bodies.
#[doc(hidden)]
pub fn minimax_envelope(body: &str) -> Option<MiniMaxEnvelope> {
    let code = minimax_status_code(body)?;
    Some(match code {
        0 => MiniMaxEnvelope::Success,
        MINIMAX_TEMPORARY => MiniMaxEnvelope::Temporary,
        _ => MiniMaxEnvelope::OtherError(code),
    })
}

fn adapter_kind(provider_id: &str) -> Option<ProviderAdapterKind> {
    ProviderAdapterKind::from_provider_id(provider_id)
}

fn kimi_quota(status: u16, body: &str) -> Option<QuotaEvidence> {
    if status != 403 {
        return None;
    }
    kimi_quota_body(body)
}

fn kimi_quota_body(body: &str) -> Option<QuotaEvidence> {
    let owned = json_error_message_owned(body);
    let message = owned.as_deref().unwrap_or(body);
    let window = if message.contains(KIMI_FIVE_HOUR) {
        QuotaWindowKind::FiveHours
    } else if message.contains(KIMI_WEEK) {
        QuotaWindowKind::Week
    } else if message.contains(KIMI_MONTH) {
        QuotaWindowKind::Month
    } else {
        return None;
    };
    Some(QuotaEvidence {
        reason: QuotaReason::QuotaExhausted,
        window,
        resets_at_rfc3339: None,
        resets_in_text: None,
    })
}

fn go_quota(status: u16, body: &str) -> Option<QuotaEvidence> {
    if status != 429 {
        return None;
    }
    go_quota_body(body)
}

fn go_quota_body(body: &str) -> Option<QuotaEvidence> {
    if json_error_type(body).is_some_and(|kind| kind == "CreditsError") {
        return None;
    }
    let text = body.to_ascii_lowercase();
    if text.contains("freeusagelimiterror")
        || text.contains("free usage exceeded")
        || text.contains("free usage limit")
        || text.contains("free couta")
        || text.contains("free quota")
    {
        return None;
    }
    let window = if text.contains("5-hour usage limit") || text.contains("5 hour usage limit") {
        QuotaWindowKind::FiveHours
    } else if text.contains("weekly usage limit") {
        QuotaWindowKind::Week
    } else if text.contains("monthly usage limit") {
        QuotaWindowKind::Month
    } else {
        return None;
    };
    Some(QuotaEvidence {
        reason: QuotaReason::QuotaExhausted,
        window,
        resets_at_rfc3339: None,
        resets_in_text: body.contains("Resets in").then(|| body.to_string()),
    })
}

fn goat_quota(status: u16, body: &str) -> Option<QuotaEvidence> {
    if status == 400 {
        return goat_insufficient_credits(body).then_some(QuotaEvidence {
            reason: QuotaReason::InsufficientBalance,
            window: QuotaWindowKind::Unknown,
            resets_at_rfc3339: None,
            resets_in_text: None,
        });
    }
    if status != 429 {
        return None;
    }
    goat_plan_limit(body)
}

fn goat_plan_limit(body: &str) -> Option<QuotaEvidence> {
    let value = serde_json::from_str::<Value>(body).ok()?;
    let error = value.get("error")?.as_object()?;
    let code_matches = error
        .get("code")
        .and_then(Value::as_str)
        .is_some_and(|code| code.eq_ignore_ascii_case("RATE_LIMITED"));
    let type_matches = error
        .get("type")
        .and_then(Value::as_str)
        .is_some_and(|kind| kind.eq_ignore_ascii_case("rate_limit_error"));
    if !code_matches && !type_matches {
        return None;
    }
    let message = error.get("message")?.as_str()?;
    let message_lower = message.to_ascii_lowercase();
    let window =
        if message_lower.contains(GOAT_FIVE_HOUR) || message_lower.contains(GOAT_FIVE_HOUR_ALT) {
            QuotaWindowKind::FiveHours
        } else if message_lower.contains(GOAT_WEEK) {
            QuotaWindowKind::Week
        } else if message_lower.contains(GOAT_MONTH) {
            QuotaWindowKind::Month
        } else {
            return None;
        };
    Some(QuotaEvidence {
        reason: QuotaReason::QuotaExhausted,
        window,
        resets_at_rfc3339: goat_reset_timestamp(message, &message_lower),
        resets_in_text: None,
    })
}

fn goat_reset_timestamp(message: &str, message_lower: &str) -> Option<String> {
    let start = message_lower.find(GOAT_RESET_MARKER)? + GOAT_RESET_MARKER.len();
    let candidate = message.get(start..)?.split_whitespace().next()?;
    let candidate = candidate.trim_end_matches(['.', ',', ';']);
    if chrono_rfc3339(candidate) {
        Some(candidate.to_string())
    } else {
        None
    }
}

fn chrono_rfc3339(value: &str) -> bool {
    // Keep this crate chrono-free: a well-formed RFC3339 instant has a date,
    // time, and timezone. The host validates the actual timestamp bounds.
    let bytes = value.as_bytes();
    bytes.len() >= 20
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && (bytes[10] == b'T' || bytes[10] == b't')
        && (value.ends_with('Z')
            || value.ends_with('z')
            || value.contains('+')
            || value.rfind('-').is_some_and(|i| i > 10))
}

fn goat_insufficient_credits(body: &str) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(body) else {
        return false;
    };
    value.pointer("/error/code").and_then(Value::as_str) == Some("BAD_REQUEST")
        && value.pointer("/error/type").and_then(Value::as_str) == Some("invalid_request_error")
        && value
            .pointer("/error/message")
            .and_then(Value::as_str)
            .is_some_and(|message| {
                message == GOAT_INSUFFICIENT_SHORT || message == GOAT_INSUFFICIENT_LONG
            })
}

fn minimax_quota(body: &str) -> Option<QuotaEvidence> {
    let code = minimax_status_code(body)?;
    match code {
        MINIMAX_BALANCE => Some(QuotaEvidence {
            reason: QuotaReason::InsufficientBalance,
            window: QuotaWindowKind::Unknown,
            resets_at_rfc3339: None,
            resets_in_text: None,
        }),
        MINIMAX_TOKEN_PLAN => Some(QuotaEvidence {
            reason: QuotaReason::QuotaExhausted,
            window: QuotaWindowKind::Unknown,
            resets_at_rfc3339: None,
            resets_in_text: None,
        }),
        _ => None,
    }
}

fn minimax_status_code(body: &str) -> Option<i64> {
    if let Ok(value) = serde_json::from_str::<Value>(body)
        && let Some(code) = json_status_code(&value)
    {
        return Some(code);
    }
    for payload in sse_json_payloads(body) {
        if let Ok(value) = serde_json::from_str::<Value>(payload)
            && let Some(code) = json_status_code(&value)
        {
            return Some(code);
        }
    }
    None
}

fn json_status_code(value: &Value) -> Option<i64> {
    value
        .pointer("/base_resp/status_code")
        .and_then(json_i64)
        .or_else(|| value.pointer("/error/status_code").and_then(json_i64))
        .or_else(|| value.get("status_code").and_then(json_i64))
}

fn json_i64(value: &Value) -> Option<i64> {
    value.as_i64().or_else(|| value.as_u64().map(|n| n as i64))
}

fn http_quota(body: &str) -> Option<QuotaEvidence> {
    if let Some(evidence) = http_quota_json(body) {
        return Some(evidence);
    }
    for payload in sse_json_payloads(body) {
        if let Some(evidence) = http_quota_json(payload) {
            return Some(evidence);
        }
    }
    None
}

fn http_quota_json(body: &str) -> Option<QuotaEvidence> {
    let value = serde_json::from_str::<Value>(body).ok()?;
    let reason = http_quota_reason(&value)?;
    Some(QuotaEvidence {
        reason,
        window: QuotaWindowKind::Unknown,
        resets_at_rfc3339: None,
        resets_in_text: None,
    })
}

fn http_quota_reason(value: &Value) -> Option<QuotaReason> {
    let code = classify_http_quota_token(value.pointer("/error/code").and_then(Value::as_str));
    let kind = classify_http_quota_token(value.pointer("/error/type").and_then(Value::as_str));
    match (code, kind) {
        (None, None) => None,
        (Some(QuotaReason::QuotaExhausted), _) | (_, Some(QuotaReason::QuotaExhausted)) => {
            Some(QuotaReason::QuotaExhausted)
        }
        (Some(reason), None) | (None, Some(reason)) | (Some(reason), Some(_)) => Some(reason),
    }
}

fn classify_http_quota_token(token: Option<&str>) -> Option<QuotaReason> {
    match token {
        Some("insufficient_quota") => Some(QuotaReason::QuotaExhausted),
        Some("insufficient_balance") => Some(QuotaReason::InsufficientBalance),
        _ => None,
    }
}

fn json_error_message_owned(body: &str) -> Option<String> {
    let value = serde_json::from_str::<Value>(body).ok()?;
    value
        .pointer("/error/message")
        .and_then(Value::as_str)
        .map(ToString::to_string)
        .or_else(|| {
            value
                .pointer("/message")
                .and_then(Value::as_str)
                .map(ToString::to_string)
        })
        .or_else(|| {
            value
                .pointer("/base_resp/status_msg")
                .and_then(Value::as_str)
                .map(ToString::to_string)
        })
}

fn json_error_type(body: &str) -> Option<String> {
    let value = serde_json::from_str::<Value>(body).ok()?;
    value
        .pointer("/error/type")
        .and_then(Value::as_str)
        .or_else(|| value.get("type").and_then(Value::as_str))
        .map(ToString::to_string)
}

fn sse_json_payloads(chunk: &str) -> Vec<&str> {
    let mut payloads = Vec::new();
    for line in chunk.lines() {
        let line = line.trim();
        let Some(data) = line.strip_prefix("data:") else {
            continue;
        };
        let data = data.trim();
        if data.is_empty() || data == "[DONE]" {
            continue;
        }
        if data.starts_with('{') || data.starts_with('[') {
            payloads.push(data);
        }
    }
    payloads
}

fn sse_error_payloads(chunk: &str) -> Vec<&str> {
    let mut payloads = Vec::new();
    let mut event_name = "";
    for line in chunk.lines() {
        let line = line.trim();
        if line.is_empty() {
            event_name = "";
            continue;
        }
        if let Some(event) = line.strip_prefix("event:") {
            event_name = event.trim();
            continue;
        }
        let Some(data) = line.strip_prefix("data:") else {
            continue;
        };
        let data = data.trim();
        if data.is_empty() || data == "[DONE]" {
            continue;
        }
        if event_name.eq_ignore_ascii_case("error") || json_is_structured_error(data) {
            payloads.push(data);
        }
    }
    payloads
}

fn json_is_structured_error(data: &str) -> bool {
    if !(data.starts_with('{') || data.starts_with('[')) {
        return false;
    }
    let Ok(value) = serde_json::from_str::<Value>(data) else {
        return false;
    };
    value
        .get("type")
        .and_then(Value::as_str)
        .is_some_and(|kind| kind.eq_ignore_ascii_case("error"))
        || value.get("error").is_some_and(Value::is_object)
}

#[cfg(test)]
mod tests;

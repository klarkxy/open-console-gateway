//! Quota evidence types and MiniMax application-error envelope validation.
//! Inference error bodies never establish quota exhaustion.
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

/// Confirmed per-Key exhaustion obtained from authoritative usage.
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

const MINIMAX_TEMPORARY: i64 = 1002;

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

#[cfg(test)]
mod tests;

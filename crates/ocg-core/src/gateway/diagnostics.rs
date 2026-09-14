use crate::db::Database;
use crate::gateway::protocol::{decode_anthropic_thinking_block, decode_chat_reasoning};
use crate::kernel::protocol::ApiFormat;
use crate::models::ForwardLog;
use crate::redaction::{redact_exact_occurrences, redact_text, sha256_hex, truncate_text};
use axum::http::HeaderMap;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use std::time::Instant;
use uuid::Uuid;

#[cfg(test)]
use std::cell::RefCell;

pub const REQUEST_ID_HEADER: &str = "x-ocg-request-id";
pub const DIAGNOSTIC_VERSION: u8 = 1;
pub const MAX_REQUEST_SUMMARY_BYTES: usize = 2 * 1024;
pub const MAX_UPSTREAM_ERROR_BYTES: usize = crate::redaction::MAX_UPSTREAM_ERROR_BYTES;
pub const MAX_DIAGNOSTIC_BYTES: usize = 4 * 1024;

pub(crate) use crate::redaction::{
    redact_known_secret, sanitize_upstream_error_value,
    sanitize_upstream_error_value_with_known_secret,
};

#[derive(Debug, Clone)]
pub struct RequestTrace {
    pub request_id: String,
    started_at: Instant,
    client_key_id: Option<String>,
    client_key_name: Option<String>,
}

impl RequestTrace {
    pub fn new() -> Self {
        Self {
            request_id: format!("ocg-{}", Uuid::new_v4()),
            started_at: Instant::now(),
            client_key_id: None,
            client_key_name: None,
        }
    }

    pub fn with_client_key(mut self, id: String, name: String) -> Self {
        self.client_key_id = Some(id);
        self.client_key_name = Some(name);
        self
    }

    pub fn elapsed_ms(&self) -> u64 {
        self.started_at.elapsed().as_millis().min(u64::MAX as u128) as u64
    }
}

impl Default for RequestTrace {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ErrorDiagnostic {
    pub version: u8,
    pub request_id: String,
    pub attempt: u32,
    pub error_source: String,
    pub error_stage: String,
    pub client_format: String,
    pub upstream_format: Option<String>,
    pub model: Option<String>,
    pub stream: Option<bool>,
    pub client_body_bytes: Option<usize>,
    pub upstream_body_bytes: Option<usize>,
    pub duration_ms: u64,
    pub upstream_wait_ms: Option<u64>,
    pub downstream_status: Option<u16>,
    pub upstream_status: Option<u16>,
    pub retry_action: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub upstream_headers: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_summary: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_fingerprint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upstream_error: Option<Value>,
    pub truncated: bool,
}

impl ErrorDiagnostic {
    pub fn new(
        trace: &RequestTrace,
        attempt: u32,
        error_source: &str,
        error_stage: &str,
        client_format: ApiFormat,
    ) -> Self {
        Self {
            version: DIAGNOSTIC_VERSION,
            request_id: trace.request_id.clone(),
            attempt,
            error_source: error_source.to_string(),
            error_stage: error_stage.to_string(),
            client_format: api_format_name(client_format).to_string(),
            upstream_format: None,
            model: None,
            stream: None,
            client_body_bytes: None,
            upstream_body_bytes: None,
            duration_ms: trace.elapsed_ms(),
            upstream_wait_ms: None,
            downstream_status: None,
            upstream_status: None,
            retry_action: None,
            upstream_headers: BTreeMap::new(),
            request_summary: None,
            request_fingerprint: None,
            upstream_error: None,
            truncated: false,
        }
    }

    pub fn with_request_summary(mut self, body: &[u8]) -> Self {
        let (summary, fingerprint) = summarize_request(body);
        self.request_summary = Some(summary);
        self.request_fingerprint = Some(fingerprint);
        self
    }

    pub fn with_upstream_error(mut self, text: &str) -> Self {
        self.upstream_error = Some(sanitize_upstream_error_value(text));
        self
    }
}

pub fn api_format_name(format: ApiFormat) -> &'static str {
    match format {
        ApiFormat::ChatCompletions => "chat_completions",
        ApiFormat::Responses => "responses",
        ApiFormat::Messages => "messages",
        ApiFormat::Gemini => "gemini",
    }
}

pub fn safe_upstream_headers(
    headers: &HeaderMap,
    known_secret: Option<&str>,
) -> BTreeMap<String, String> {
    const ALLOWED: &[&str] = &[
        "x-request-id",
        "request-id",
        "x-trace-id",
        "x-amzn-trace-id",
        "traceparent",
        "cf-ray",
        "retry-after",
        "content-type",
    ];
    let mut safe = BTreeMap::new();
    for name in ALLOWED {
        if let Some(value) = headers.get(*name).and_then(|value| value.to_str().ok()) {
            let value = known_secret.map_or_else(
                || value.to_string(),
                |secret| redact_known_secret(value, secret),
            );
            safe.insert(
                (*name).to_string(),
                truncate_text(&redact_text(&value), 256),
            );
        }
    }
    safe
}

pub fn serialize_diagnostic(mut diagnostic: ErrorDiagnostic) -> String {
    let mut encoded = serde_json::to_string(&diagnostic).unwrap_or_else(|_| {
        format!(
            "{{\"version\":1,\"request_id\":{},\"error_source\":\"gateway\",\"error_stage\":\"internal\",\"truncated\":true}}",
            serde_json::to_string(&diagnostic.request_id).unwrap_or_else(|_| "\"unknown\"".into())
        )
    });
    if encoded.len() <= MAX_DIAGNOSTIC_BYTES {
        return encoded;
    }

    diagnostic.truncated = true;
    diagnostic.request_summary = diagnostic.request_summary.as_ref().map(|value| {
        json!({
            "summary": truncate_text(&value.to_string(), 768),
            "truncated": true
        })
    });
    diagnostic.upstream_error = diagnostic.upstream_error.as_ref().map(|value| {
        json!({
            "summary": truncate_text(&value.to_string(), 768),
            "truncated": true
        })
    });
    encoded = serde_json::to_string(&diagnostic).unwrap_or_default();
    if encoded.len() <= MAX_DIAGNOSTIC_BYTES {
        return encoded;
    }

    diagnostic.upstream_headers.clear();
    diagnostic.request_summary = None;
    diagnostic.upstream_error = None;
    diagnostic.model = None;
    diagnostic.retry_action = None;
    serde_json::to_string(&diagnostic).unwrap_or_else(|_| {
        format!(
            "{{\"version\":1,\"request_id\":{},\"truncated\":true}}",
            serde_json::to_string(&diagnostic.request_id).unwrap_or_else(|_| "\"unknown\"".into())
        )
    })
}

pub fn emit_failure(diagnostic_json: &str) {
    eprintln!("OCG_REQUEST_ERROR {diagnostic_json}");
}

/// Runtime record of a versioned legacy-compat hosted-tool drop.
///
/// Emitted at the actual forward attempt. The payload is request id, compat
/// profile, version, and dropped hosted-tool types only — never the request
/// body or secrets.
pub fn emit_legacy_tool_compat(
    request_id: &str,
    profile: &str,
    version: u32,
    dropped_hosted_tools: &[String],
) {
    let payload = json!({
        "request_id": request_id,
        "profile": profile,
        "version": version,
        "dropped_hosted_tools": dropped_hosted_tools,
    });
    eprintln!("OCG_LEGACY_TOOL_COMPAT {payload}");
    #[cfg(test)]
    record_legacy_tool_compat_emission(&payload);
}

#[cfg(test)]
thread_local! {
    static LEGACY_TOOL_COMPAT_EMISSIONS: RefCell<Vec<Value>> = const { RefCell::new(Vec::new()) };
}

#[cfg(test)]
fn record_legacy_tool_compat_emission(payload: &Value) {
    LEGACY_TOOL_COMPAT_EMISSIONS.with(|slot| slot.borrow_mut().push(payload.clone()));
}

#[cfg(test)]
pub(crate) fn take_legacy_tool_compat_emissions() -> Vec<Value> {
    LEGACY_TOOL_COMPAT_EMISSIONS.with(|slot| std::mem::take(&mut *slot.borrow_mut()))
}

/// Persist a local request failure in the request log without leaking request
/// content into the operational runtime log. Failures that happen before model
/// or account selection use honest unresolved/Gateway placeholders.
pub(crate) fn log_request_failure(
    db: &Database,
    trace: &RequestTrace,
    diagnostic: &ErrorDiagnostic,
    diagnostic_json: &str,
    message: &str,
) {
    let diagnostic_value = serde_json::from_str(diagnostic_json).ok();
    let log = ForwardLog {
        id: 0,
        timestamp: Utc::now(),
        model: diagnostic
            .model
            .clone()
            .unwrap_or_else(|| "(unresolved)".to_string()),
        account_id: String::new(),
        account_name: "Gateway".to_string(),
        route_account_id: None,
        provider_id: None,

        credential_account_id: None,
        client_key_id: trace.client_key_id.clone(),
        client_key_name: trace.client_key_name.clone(),
        status: if diagnostic.error_source == "client" {
            "client_error"
        } else {
            "error"
        }
        .to_string(),
        http_status: diagnostic.downstream_status.map(i32::from),
        route: String::new(),
        prompt_tokens: 0,
        completion_tokens: 0,
        cached_tokens: 0,
        cache_creation_tokens: 0,
        cost: None,
        raw_cost_usd: None,
        quota_debit: None,
        effective_paid_cost_usd: None,
        pricing_revision_id: None,
        quota_multiplier: None,
        local_adjustment_multiplier: None,
        service_tier: None,
        cost_state: "not_applicable".to_string(),
        error_message: Some(redact_text(message)),
        request_id: Some(diagnostic.request_id.clone()),
        attempt: Some(i64::from(diagnostic.attempt)),
        error_source: Some(diagnostic.error_source.clone()),
        error_stage: Some(diagnostic.error_stage.clone()),
        duration_ms: Some(diagnostic.duration_ms.min(i64::MAX as u64) as i64),
        diagnostic: diagnostic_value,
    };
    if let Err(error) = db.log_forward(&log) {
        eprintln!("failed to persist local request failure: {error}");
    }
}

fn summarize_request(body: &[u8]) -> (Value, String) {
    let fingerprint = sha256_hex(body);
    let Ok(value) = serde_json::from_slice::<Value>(body) else {
        return (
            json!({
                "kind": "invalid_json",
                "bytes": body.len(),
                "sha256": fingerprint }),
            fingerprint,
        );
    };

    let mut roles = BTreeMap::<String, usize>::new();
    let mut content_types = BTreeMap::<String, usize>::new();
    let mut parts = Vec::<Value>::new();
    let mut total_strings = 0usize;
    let mut total_string_bytes = 0usize;
    collect_shape(
        &value,
        None,
        0,
        &mut roles,
        &mut content_types,
        &mut parts,
        &mut total_strings,
        &mut total_string_bytes,
    );

    let object = value.as_object();
    let mut parameters = Map::new();
    if let Some(object) = object {
        for key in [
            "stream",
            "max_tokens",
            "max_output_tokens",
            "temperature",
            "top_p",
            "top_k",
            "parallel_tool_calls",
        ] {
            if let Some(value) = object
                .get(key)
                .filter(|value| value.is_boolean() || value.is_number())
            {
                parameters.insert(key.to_string(), value.clone());
            }
        }
        for key in ["reasoning_effort", "service_tier"] {
            if let Some(value) = object
                .get(key)
                .and_then(Value::as_str)
                .filter(|value| value.len() <= 32 && value.chars().all(is_safe_label_char))
            {
                parameters.insert(key.to_string(), Value::String(value.to_string()));
            }
        }
    }

    let summary = json!({
        "bytes": body.len(),
        "object": value.is_object(),
        "messages": array_len(object, "messages"),
        "input_items": array_len(object, "input"),
        "tools": array_len(object, "tools"),
        "roles": roles,
        "content_types": content_types,
        "strings": total_strings,
        "string_bytes": total_string_bytes,
        "content_parts": parts,
        "parameters": parameters,
        "sha256": fingerprint });
    if summary.to_string().len() <= MAX_REQUEST_SUMMARY_BYTES {
        (summary, fingerprint)
    } else {
        (
            json!({
                "bytes": body.len(),
                "messages": array_len(object, "messages"),
                "input_items": array_len(object, "input"),
                "tools": array_len(object, "tools"),
                "strings": total_strings,
                "string_bytes": total_string_bytes,
                "sha256": fingerprint,
                "truncated": true }),
            fingerprint,
        )
    }
}

#[allow(clippy::too_many_arguments)]
fn collect_shape(
    value: &Value,
    key_hint: Option<&str>,
    depth: usize,
    roles: &mut BTreeMap<String, usize>,
    content_types: &mut BTreeMap<String, usize>,
    parts: &mut Vec<Value>,
    total_strings: &mut usize,
    total_string_bytes: &mut usize,
) {
    if depth > 12 {
        return;
    }
    match value {
        Value::String(text) => {
            *total_strings += 1;
            *total_string_bytes = total_string_bytes.saturating_add(text.len());
            let key = key_hint.unwrap_or_default().to_ascii_lowercase();
            if key == "role" {
                let role = match text.as_str() {
                    "system" | "developer" | "user" | "assistant" | "tool" => text.as_str(),
                    _ => "other",
                };
                *roles.entry(role.to_string()).or_default() += 1;
                return;
            }
            if key == "type" {
                let kind = safe_content_type(text);
                *content_types.entry(kind.to_string()).or_default() += 1;
                return;
            }
            if matches!(key.as_str(), "model" | "service_tier" | "reasoning_effort") {
                return;
            }
            if parts.len() < 24 {
                parts.push(json!({
                    "kind": content_kind(&key),
                    "bytes": text.len(),
                    "sha256": &sha256_hex(text.as_bytes())[..12] }));
            }
        }
        Value::Array(values) => {
            for value in values {
                collect_shape(
                    value,
                    key_hint,
                    depth + 1,
                    roles,
                    content_types,
                    parts,
                    total_strings,
                    total_string_bytes,
                );
            }
        }
        Value::Object(values) => {
            for (key, value) in values {
                collect_shape(
                    value,
                    Some(key),
                    depth + 1,
                    roles,
                    content_types,
                    parts,
                    total_strings,
                    total_string_bytes,
                );
            }
        }
        _ => {}
    }
}

fn array_len(object: Option<&Map<String, Value>>, key: &str) -> usize {
    object
        .and_then(|object| object.get(key))
        .and_then(Value::as_array)
        .map_or(0, Vec::len)
}

fn content_kind(key: &str) -> &'static str {
    if key.contains("argument") || key.contains("tool") {
        "tool_data"
    } else if key.contains("url")
        || key.contains("image")
        || key.contains("audio")
        || key.contains("file")
    {
        "resource"
    } else if key.contains("data") {
        "data"
    } else {
        "text"
    }
}

fn safe_content_type(value: &str) -> &'static str {
    match value {
        "text" | "input_text" | "output_text" => "text",
        "image" | "input_image" | "image_url" => "image",
        "audio" | "input_audio" => "audio",
        "video" | "input_video" => "video",
        "tool_use" | "tool_call" | "function_call" => "tool_call",
        "tool_result" | "function_call_output" => "tool_result",
        _ => "other",
    }
}

fn is_safe_label_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.')
}

/// Redact an exact known secret from JSON values without changing object keys.
///
/// Successful upstream responses must retain their schema even when an account
/// Key happens to be a short/common token such as `data`. Error diagnostics use
/// a more aggressive text sanitizer, but client-facing success payloads only
/// need the credential removed from values that can carry provider output.
pub(crate) fn redact_known_secret_values(value: &mut Value, known_secret: &str) {
    if known_secret.is_empty() {
        return;
    }
    redact_known_secret_values_at(value, known_secret, None, None, false, None, true);
}

/// Streaming argument fragments are not standalone JSON yet. Their semantic
/// redactor owns those fields, so the frame-level defense must leave them intact
/// instead of accidentally rewriting nested property names.
pub(crate) fn redact_known_secret_stream_values(value: &mut Value, known_secret: &str) {
    if known_secret.is_empty() {
        return;
    }
    let event_type = value
        .get("type")
        .and_then(Value::as_str)
        .map(str::to_string);
    redact_known_secret_values_at(
        value,
        known_secret,
        None,
        None,
        true,
        event_type.as_deref(),
        true,
    );
}

fn redact_known_secret_values_at(
    value: &mut Value,
    known_secret: &str,
    key_hint: Option<&str>,
    parent_key: Option<&str>,
    streaming: bool,
    stream_event_type: Option<&str>,
    preserve_protocol_controls: bool,
) {
    match value {
        Value::String(text)
            if !(preserve_protocol_controls
                && key_hint.is_some_and(|key| is_protocol_control_string_value(key, text))) =>
        {
            if key_hint == Some("encrypted_content")
                && opaque_replay_contains_secret(text, known_secret)
            {
                text.clear();
                return;
            }
            if streaming
                && stream_fragment_is_semantically_owned(key_hint, parent_key, stream_event_type)
            {
                return;
            }
            if key_hint == Some("arguments")
                && let Ok(mut arguments) = serde_json::from_str::<Value>(text)
            {
                redact_known_secret_values_at(
                    &mut arguments,
                    known_secret,
                    None,
                    None,
                    false,
                    None,
                    false,
                );
                if let Ok(encoded) = serde_json::to_string(&arguments) {
                    *text = encoded;
                    return;
                }
            }
            *text = redact_exact_occurrences(text, known_secret);
        }
        Value::Array(values) => {
            for value in values {
                redact_known_secret_values_at(
                    value,
                    known_secret,
                    key_hint,
                    parent_key,
                    streaming,
                    stream_event_type,
                    preserve_protocol_controls,
                );
            }
        }
        Value::Object(values) => {
            for (key, value) in values {
                let child_preserves_protocol_controls = preserve_protocol_controls
                    && !matches!(key.as_str(), "input" | "args" | "metadata");
                redact_known_secret_values_at(
                    value,
                    known_secret,
                    Some(key),
                    key_hint,
                    streaming,
                    stream_event_type,
                    child_preserves_protocol_controls,
                );
            }
        }
        _ => {}
    }
}

fn opaque_replay_contains_secret(encrypted_content: &str, known_secret: &str) -> bool {
    decode_anthropic_thinking_block(encrypted_content)
        .is_some_and(|block| json_values_contain_secret(&block, known_secret))
        || decode_chat_reasoning(encrypted_content)
            .is_some_and(|reasoning| reasoning.contains(known_secret))
}

fn json_values_contain_secret(value: &Value, known_secret: &str) -> bool {
    match value {
        Value::String(text) => text.contains(known_secret),
        Value::Array(values) => values
            .iter()
            .any(|value| json_values_contain_secret(value, known_secret)),
        Value::Object(values) => values
            .values()
            .any(|value| json_values_contain_secret(value, known_secret)),
        _ => false,
    }
}

fn stream_fragment_is_semantically_owned(
    key: Option<&str>,
    parent_key: Option<&str>,
    event_type: Option<&str>,
) -> bool {
    match key {
        Some("arguments") => {
            matches!(parent_key, Some("function" | "function_call"))
                || matches!(event_type, Some("response.function_call_arguments.done"))
        }
        Some("partial_json") => {
            parent_key == Some("delta") && event_type == Some("content_block_delta")
        }
        Some("delta") if parent_key.is_none() => matches!(
            event_type,
            Some(
                "response.output_text.delta"
                    | "response.refusal.delta"
                    | "response.reasoning_text.delta"
                    | "response.reasoning_summary_text.delta"
                    | "response.function_call_arguments.delta"
                    | "response.custom_tool_call_input.delta"
            )
        ),
        _ => false,
    }
}

/// Protocol discriminators and correlation metadata are not provider content.
/// Rewriting them for a short/common Key (for example `text`, `message`, or
/// `stop`) makes otherwise safe responses unparsable. Unknown string fields are
/// still redacted by default so provider-specific content remains covered.
fn is_protocol_control_string_value(key: &str, value: &str) -> bool {
    match key {
        "type" => matches!(
            value,
            "message"
                | "message_start"
                | "message_delta"
                | "message_stop"
                | "content_block_start"
                | "content_block_delta"
                | "content_block_stop"
                | "text"
                | "text_delta"
                | "input_text"
                | "output_text"
                | "refusal"
                | "image"
                | "input_image"
                | "image_url"
                | "thinking"
                | "redacted_thinking"
                | "thinking_delta"
                | "signature_delta"
                | "reasoning"
                | "reasoning_text"
                | "summary_text"
                | "tool"
                | "tool_use"
                | "server_tool_use"
                | "tool_result"
                | "function"
                | "function_call"
                | "function_call_output"
                | "custom"
                | "custom_tool_call"
                | "input_json_delta"
                | "base64"
                | "url"
                | "auto"
                | "any"
                | "enabled"
                | "disabled"
                | "json_schema"
                | "json_object"
                | "grammar"
                | "object"
                | "string"
                | "error"
                | "api_error"
                | "server_error"
                | "invalid_request_error"
                | "authentication_error"
                | "rate_limit_error"
                | "upstream_outcome_unknown"
                | "response.created"
                | "response.in_progress"
                | "response.output_item.added"
                | "response.content_part.added"
                | "response.reasoning_summary_part.added"
                | "response.content_part.done"
                | "response.output_text.delta"
                | "response.output_text.done"
                | "response.refusal.delta"
                | "response.reasoning_text.delta"
                | "response.reasoning_summary_text.delta"
                | "response.reasoning_summary_text.done"
                | "response.reasoning_summary_part.done"
                | "response.function_call_arguments.delta"
                | "response.function_call_arguments.done"
                | "response.custom_tool_call_input.delta"
                | "response.custom_tool_call_input.done"
                | "response.output_item.done"
                | "response.completed"
                | "response.incomplete"
                | "response.failed"
        ),
        "object" => matches!(
            value,
            "chat.completion" | "chat.completion.chunk" | "list" | "model" | "response"
        ),
        "role" => matches!(
            value,
            "assistant" | "user" | "system" | "developer" | "tool" | "model"
        ),
        "status" => matches!(
            value,
            "queued" | "in_progress" | "completed" | "incomplete" | "failed" | "cancelled"
        ),
        "reason" => matches!(value, "max_output_tokens" | "content_filter"),
        "stop_reason" => matches!(
            value,
            "end_turn" | "max_tokens" | "stop_sequence" | "tool_use" | "pause_turn" | "refusal"
        ),
        "finish_reason" => matches!(
            value,
            "stop" | "length" | "tool_calls" | "function_call" | "content_filter"
        ),
        "finishReason" => matches!(
            value,
            "STOP"
                | "MAX_TOKENS"
                | "SAFETY"
                | "RECITATION"
                | "LANGUAGE"
                | "OTHER"
                | "BLOCKLIST"
                | "PROHIBITED_CONTENT"
                | "SPII"
                | "MALFORMED_FUNCTION_CALL"
        ),
        "service_tier" => matches!(value, "auto" | "default" | "flex" | "priority"),
        "tool_choice" => matches!(value, "auto" | "none" | "required"),
        "truncation" => matches!(value, "auto" | "disabled"),
        "effort" => matches!(
            value,
            "none" | "minimal" | "low" | "medium" | "high" | "xhigh"
        ),
        "format" => matches!(value, "text" | "json_schema" | "json_object" | "grammar"),
        "media_type" => matches!(
            value,
            "application/json"
                | "application/pdf"
                | "text/plain"
                | "image/png"
                | "image/jpeg"
                | "image/gif"
                | "image/webp"
        ),
        "encoding" => matches!(value, "base64" | "utf-8" | "gzip"),
        _ => false,
    }
}

#[cfg(test)]
mod tests;

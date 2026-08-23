use crate::kernel::protocol::ApiFormat;
use axum::http::HeaderMap;
use bytes::Bytes;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

pub use crate::routing_runtime::{
    CONVERSATION_TTL, MAX_CONVERSATIONS, RoutingCandidate, RoutingRuntime,
};

pub const CONVERSATION_HEADER: &str = "x-ocg-conversation-id";
const MAX_EXPLICIT_ID_LEN: usize = 256;

pub fn resolve_conversation_key(
    client_format: ApiFormat,
    model: &str,
    headers: &HeaderMap,
    client_body: &Bytes,
) -> Option<String> {
    if let Some(explicit) = explicit_conversation_id(headers) {
        return Some(namespaced_key(
            client_format,
            model,
            "explicit",
            explicit.as_bytes(),
        ));
    }
    let body: Value = serde_json::from_slice(client_body).ok()?;
    let seed = prompt_seed(client_format, &body)?;
    let canonical = serde_json::to_vec(&canonicalize_json(&seed)).ok()?;
    Some(namespaced_key(client_format, model, "prompt", &canonical))
}

fn explicit_conversation_id(headers: &HeaderMap) -> Option<String> {
    let raw = headers
        .get(CONVERSATION_HEADER)
        .and_then(|value| value.to_str().ok())?;
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.len() > MAX_EXPLICIT_ID_LEN {
        return None;
    }
    if trimmed.chars().any(|ch| ch.is_control()) {
        return None;
    }
    Some(trimmed.to_string())
}

fn namespaced_key(format: ApiFormat, model: &str, kind: &str, payload: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(routing_label(format).as_bytes());
    hasher.update([0]);
    hasher.update(model.as_bytes());
    hasher.update([0]);
    hasher.update(kind.as_bytes());
    hasher.update([0]);
    hasher.update(payload);
    hex::encode(hasher.finalize())
}

// Avoid adding a hex crate; format digest bytes manually.
mod hex {
    pub fn encode(bytes: impl AsRef<[u8]>) -> String {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let bytes = bytes.as_ref();
        let mut out = String::with_capacity(bytes.len() * 2);
        for byte in bytes {
            out.push(HEX[(byte >> 4) as usize] as char);
            out.push(HEX[(byte & 0xf) as usize] as char);
        }
        out
    }
}

fn routing_label(format: ApiFormat) -> &'static str {
    match format {
        ApiFormat::ChatCompletions => "chat_completions",
        ApiFormat::Responses => "responses",
        ApiFormat::Messages => "messages",
        ApiFormat::Gemini => "gemini",
    }
}

fn prompt_seed(format: ApiFormat, body: &Value) -> Option<Value> {
    let (system, tools, first_user) = match format {
        ApiFormat::ChatCompletions => (
            chat_system_seed(body),
            body.get("tools").cloned().filter(|value| !value.is_null()),
            chat_first_user(body),
        ),
        ApiFormat::Responses => (
            body.get("instructions")
                .cloned()
                .filter(|value| !is_empty_json(value)),
            body.get("tools").cloned().filter(|value| !value.is_null()),
            responses_first_user(body),
        ),
        ApiFormat::Messages => (
            body.get("system")
                .cloned()
                .filter(|value| !is_empty_json(value)),
            body.get("tools").cloned().filter(|value| !value.is_null()),
            messages_first_user(body),
        ),
        ApiFormat::Gemini => (
            body.get("systemInstruction")
                .cloned()
                .filter(|value| !is_empty_json(value)),
            body.get("tools").cloned().filter(|value| !value.is_null()),
            gemini_first_user(body),
        ),
    };

    let first_user = first_user.filter(|value| !is_empty_json(value))?;
    let mut seed = Map::new();
    if let Some(system) = system.filter(|value| !is_empty_json(value)) {
        seed.insert("system".into(), system);
    }
    if let Some(tools) = tools.filter(|value| !is_empty_json(value)) {
        seed.insert("tools".into(), tools);
    }
    seed.insert("first_user".into(), first_user);
    Some(Value::Object(seed))
}

fn chat_system_seed(body: &Value) -> Option<Value> {
    let mut parts = Vec::new();
    for message in json_array(body, "messages") {
        if let Some("system" | "developer") = message.get("role").and_then(Value::as_str)
            && let Some(content) = message.get("content")
        {
            parts.push(content.clone());
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(Value::Array(parts))
    }
}

fn chat_first_user(body: &Value) -> Option<Value> {
    for message in json_array(body, "messages") {
        if message.get("role").and_then(Value::as_str) == Some("user") {
            return message.get("content").cloned();
        }
    }
    None
}

fn messages_first_user(body: &Value) -> Option<Value> {
    chat_first_user(body)
}

fn responses_first_user(body: &Value) -> Option<Value> {
    match body.get("input") {
        Some(Value::String(text)) if !text.is_empty() => Some(Value::String(text.clone())),
        Some(Value::Array(items)) => {
            for item in items {
                let role = item.get("role").and_then(Value::as_str);
                let item_type = item.get("type").and_then(Value::as_str);
                if role == Some("user")
                    || (item_type == Some("message") && role.unwrap_or("user") == "user")
                {
                    return item.get("content").cloned().or_else(|| Some(item.clone()));
                }
            }
            None
        }
        _ => None,
    }
}

fn gemini_first_user(body: &Value) -> Option<Value> {
    for content in json_array(body, "contents") {
        match content.get("role").and_then(Value::as_str) {
            Some("user") | None => {
                return content
                    .get("parts")
                    .cloned()
                    .or_else(|| Some(content.clone()));
            }
            _ => {}
        }
    }
    None
}

fn json_array<'a>(value: &'a Value, key: &str) -> &'a [Value] {
    value
        .get(key)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

fn is_empty_json(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::String(text) => text.is_empty(),
        Value::Array(items) => items.is_empty(),
        Value::Object(map) => map.is_empty(),
        _ => false,
    }
}

fn canonicalize_json(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut keys = map.keys().cloned().collect::<Vec<_>>();
            keys.sort();
            let mut out = Map::new();
            for key in keys {
                if let Some(child) = map.get(&key) {
                    out.insert(key, canonicalize_json(child));
                }
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(items.iter().map(canonicalize_json).collect()),
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_header_is_namespaced_by_format_and_model() {
        let mut headers = HeaderMap::new();
        headers.insert(CONVERSATION_HEADER, "  session-1  ".parse().unwrap());
        let body = Bytes::from_static(br#"{"messages":[{"role":"user","content":"hi"}]}"#);
        let chat =
            resolve_conversation_key(ApiFormat::ChatCompletions, "grok-4.5", &headers, &body)
                .unwrap();
        let messages =
            resolve_conversation_key(ApiFormat::Messages, "grok-4.5", &headers, &body).unwrap();
        let other_model =
            resolve_conversation_key(ApiFormat::ChatCompletions, "glm-5.1", &headers, &body)
                .unwrap();
        assert_ne!(chat, messages);
        assert_ne!(chat, other_model);
    }

    #[test]
    fn invalid_explicit_header_falls_back_to_prompt_seed() {
        let mut headers = HeaderMap::new();
        headers.insert(CONVERSATION_HEADER, "   ".parse().unwrap());
        let body = Bytes::from_static(
            br#"{"messages":[{"role":"system","content":"s"},{"role":"user","content":"hello"}]}"#,
        );
        let prompt_key =
            resolve_conversation_key(ApiFormat::ChatCompletions, "grok-4.5", &headers, &body)
                .unwrap();
        let mut good = HeaderMap::new();
        good.insert(CONVERSATION_HEADER, "ok".parse().unwrap());
        let explicit =
            resolve_conversation_key(ApiFormat::ChatCompletions, "grok-4.5", &good, &body).unwrap();
        assert_ne!(prompt_key, explicit);

        let overlong = "x".repeat(MAX_EXPLICIT_ID_LEN + 1);
        let mut long_headers = HeaderMap::new();
        long_headers.insert(CONVERSATION_HEADER, overlong.parse().unwrap());
        assert_eq!(
            resolve_conversation_key(ApiFormat::ChatCompletions, "grok-4.5", &long_headers, &body,)
                .unwrap(),
            prompt_key
        );

        let mut control_headers = HeaderMap::new();
        control_headers.insert(
            CONVERSATION_HEADER,
            axum::http::HeaderValue::from_bytes(b"bad\tid").unwrap(),
        );
        assert_eq!(
            resolve_conversation_key(
                ApiFormat::ChatCompletions,
                "grok-4.5",
                &control_headers,
                &body,
            )
            .unwrap(),
            prompt_key
        );
    }

    #[test]
    fn prompt_seed_ignores_later_history_and_sampling_params() {
        let headers = HeaderMap::new();
        let first = Bytes::from_static(
            br#"{"model":"grok-4.5","temperature":0.2,"messages":[{"role":"system","content":"sys"},{"role":"user","content":"hello"}]}"#,
        );
        let second = Bytes::from_static(
            br#"{"model":"grok-4.5","temperature":0.9,"stream":true,"messages":[{"role":"system","content":"sys"},{"role":"user","content":"hello"},{"role":"assistant","content":"hi"},{"role":"user","content":"again"}]}"#,
        );
        let a = resolve_conversation_key(ApiFormat::ChatCompletions, "grok-4.5", &headers, &first)
            .unwrap();
        let b = resolve_conversation_key(ApiFormat::ChatCompletions, "grok-4.5", &headers, &second)
            .unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn prompt_seed_is_stable_across_object_key_order() {
        let headers = HeaderMap::new();
        let a = Bytes::from_static(
            br#"{"tools":[{"type":"function","function":{"name":"x","parameters":{"b":1,"a":2}}}],"messages":[{"role":"user","content":"hi"}]}"#,
        );
        let b = Bytes::from_static(
            br#"{"messages":[{"content":"hi","role":"user"}],"tools":[{"function":{"parameters":{"a":2,"b":1},"name":"x"},"type":"function"}]}"#,
        );
        assert_eq!(
            resolve_conversation_key(ApiFormat::ChatCompletions, "m", &headers, &a).unwrap(),
            resolve_conversation_key(ApiFormat::ChatCompletions, "m", &headers, &b).unwrap()
        );
    }

    #[test]
    fn prompt_seed_requires_first_user() {
        let headers = HeaderMap::new();
        let body = Bytes::from_static(br#"{"messages":[{"role":"system","content":"only"}]}"#);
        assert!(
            resolve_conversation_key(ApiFormat::ChatCompletions, "m", &headers, &body).is_none()
        );
    }

    #[test]
    fn responses_and_gemini_prompt_seeds_extract_first_user() {
        let headers = HeaderMap::new();
        let responses = Bytes::from_static(
            br#"{"instructions":"sys","input":[{"role":"user","content":"hello"}]}"#,
        );
        assert!(
            resolve_conversation_key(ApiFormat::Responses, "grok-4.5", &headers, &responses)
                .is_some()
        );
        let gemini = Bytes::from_static(
            br#"{"systemInstruction":{"parts":[{"text":"sys"}]},"contents":[{"role":"user","parts":[{"text":"hi"}]}]}"#,
        );
        assert!(
            resolve_conversation_key(ApiFormat::Gemini, "grok-4.5", &headers, &gemini).is_some()
        );
    }
}

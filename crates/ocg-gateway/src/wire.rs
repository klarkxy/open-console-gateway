//! Provider wire normalization: pure byte/JSON rewrites for upstream quirks.
//!
//! Ollama Cloud is the first family whose OpenAI-compatible surface differs
//! from what DeepSeek/OpenAI-style clients expect: chain-of-thought arrives
//! in `reasoning` / `thinking` while clients read `reasoning_content`, and
//! `max_tokens` above the upstream ceiling is rejected with a hard 400.
//! Both quirks are fixed adapter behavior — no user switches — and every
//! function here is a pure `Bytes -> Bytes` / `Value -> Value` transform so
//! the host forwarder can apply it per attempt without touching fallback
//! state or other families' bytes.
//!
//! Functions are rust-public only as the cross-crate bridge; the host crate's
//! `gateway::wire` facade keeps them crate-private.

use bytes::Bytes;
use serde_json::Value;

/// Which fixed wire normalization an attempt must apply. Data-only marker
/// carried on [`crate::attempt::AttemptSpec`]; every adapter except Ollama
/// Cloud leaves it [`WireNormalization::None`] and sends untouched bytes.
///
/// Public only as the cross-crate bridge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[doc(hidden)]
pub enum WireNormalization {
    #[default]
    None,
    OllamaCloud,
}

/// Provider-level output ceiling. Ollama Cloud rejects
/// `max_tokens` / `max_completion_tokens` above this value outright
/// (verified 2026-08-31); the clamp is one-way and model-independent.
pub const OLLAMA_CLOUD_MAX_TOKENS_LIMIT: u64 = 65_535;

impl WireNormalization {
    /// Normalize one upstream request body. Returns the original `Bytes`
    /// handle unchanged when nothing applies (including non-JSON bodies), so
    /// untouched requests keep byte-for-byte identity. The parsed document is
    /// mutated in place and only re-serialized when a rewrite actually
    /// happened — large Ollama contexts pay no clone on the no-op path.
    pub fn normalize_request_body(self, body: Bytes) -> Bytes {
        if self == Self::None {
            return body;
        }
        let Ok(mut value) = serde_json::from_slice::<Value>(&body) else {
            return body;
        };
        if !normalize_ollama_cloud_request_value(&mut value) {
            return body;
        }
        serde_json::to_vec(&value).map(Bytes::from).unwrap_or(body)
    }

    /// Normalize a parsed upstream response/stream JSON value in place.
    /// `[DONE]` and non-JSON frames never reach this function; the caller
    /// passes only successfully parsed payloads.
    pub fn normalize_response_value(self, value: &mut Value) {
        if self == Self::None {
            return;
        }
        backfill_ollama_cloud_reasoning(value);
    }
}

/// Request-direction rewrite for a Chat Completions body:
/// - assistant messages with a non-empty string `reasoning_content` and no
///   `reasoning` gain a copy under `reasoning` (both empty/absent → skip);
/// - `max_tokens` / `max_completion_tokens` above the provider ceiling are
///   clamped down to it (values at or below the ceiling are untouched).
///
/// Returns whether anything changed.
pub fn normalize_ollama_cloud_request_value(body: &mut Value) -> bool {
    let mut changed = false;
    if body
        .get_mut("messages")
        .and_then(Value::as_array_mut)
        .is_some()
    {
        let messages = body
            .get_mut("messages")
            .and_then(Value::as_array_mut)
            .expect("checked above");
        for message in messages.iter_mut() {
            if message.get("role").and_then(Value::as_str) != Some("assistant") {
                continue;
            }
            let reasoning_content = message
                .get("reasoning_content")
                .and_then(Value::as_str)
                .filter(|text| !text.is_empty())
                .map(str::to_string);
            let Some(reasoning_content) = reasoning_content else {
                continue;
            };
            let reasoning_missing_or_empty = message
                .get("reasoning")
                .map(|value| value.is_null() || value.as_str() == Some(""))
                .unwrap_or(true);
            if reasoning_missing_or_empty && let Some(object) = message.as_object_mut() {
                object.insert("reasoning".to_string(), Value::String(reasoning_content));
                changed = true;
            }
        }
    }
    // Malformed bodies are not this layer's problem: the clamp still runs and
    // the upstream returns its own validation error for the rest.
    changed |= clamp_ollama_cloud_max_tokens(body);
    changed
}

/// One-way clamp of the output-limit fields. Missing fields, non-numeric
/// values, and values within the ceiling are left alone. Returns whether
/// anything changed.
fn clamp_ollama_cloud_max_tokens(body: &mut Value) -> bool {
    let mut changed = false;
    let Some(object) = body.as_object_mut() else {
        return false;
    };
    for field in ["max_tokens", "max_completion_tokens"] {
        if object
            .get(field)
            .and_then(Value::as_u64)
            .is_some_and(|limit| limit > OLLAMA_CLOUD_MAX_TOKENS_LIMIT)
            && let Some(value) = object.get_mut(field)
        {
            *value = Value::from(OLLAMA_CLOUD_MAX_TOKENS_LIMIT);
            changed = true;
        }
    }
    changed
}

/// Response/stream-direction backfill: inside `choices[].message` and
/// `choices[].delta`, a non-empty string `reasoning` or `thinking` with no
/// `reasoning_content` is copied into `reasoning_content`. Existing fields
/// are never removed or modified; objects without either field are skipped.
pub fn backfill_ollama_cloud_reasoning(value: &mut Value) {
    let Some(choices) = value.get_mut("choices").and_then(Value::as_array_mut) else {
        return;
    };
    for choice in choices.iter_mut() {
        for container in ["message", "delta"] {
            let Some(target) = choice.get_mut(container) else {
                continue;
            };
            let has_reasoning_content = target
                .get("reasoning_content")
                .map(|existing| !existing.is_null())
                .unwrap_or(false);
            if has_reasoning_content {
                continue;
            }
            let source = ["reasoning", "thinking"]
                .iter()
                .find_map(|field| {
                    target
                        .get(*field)
                        .and_then(Value::as_str)
                        .filter(|text| !text.is_empty())
                })
                .map(str::to_string);
            let Some(source) = source else {
                continue;
            };
            if let Some(object) = target.as_object_mut() {
                object.insert("reasoning_content".to_string(), Value::String(source));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn request(body: Value) -> Bytes {
        Bytes::from(serde_json::to_vec(&body).unwrap())
    }

    fn parsed(body: &Bytes) -> Value {
        serde_json::from_slice(body).unwrap()
    }

    #[test]
    fn request_normalization_copies_assistant_reasoning_content_only_when_missing() {
        let body = request(json!({
            "model": "deepseek-v4-flash:0915",
            "messages": [
                {"role": "user", "content": "hi"},
                {"role": "assistant", "content": "answer", "reasoning_content": "thought"},
                {"role": "assistant", "content": "kept", "reasoning": "already"},
                {"role": "assistant", "content": "empty"},
                {"role": "assistant", "content": "blank", "reasoning_content": ""}
            ]
        }));
        let normalized = WireNormalization::OllamaCloud.normalize_request_body(body.clone());
        let value = parsed(&normalized);
        assert_eq!(
            value["messages"][1]["reasoning"], "thought",
            "assistant reasoning_content is duplicated into reasoning"
        );
        assert_eq!(value["messages"][1]["reasoning_content"], "thought");
        assert_eq!(
            value["messages"][2]["reasoning"], "already",
            "an existing reasoning wins"
        );
        assert!(
            value["messages"][3].get("reasoning").is_none(),
            "no reasoning_content means no rewrite"
        );
        assert!(
            value["messages"][4].get("reasoning").is_none(),
            "empty reasoning_content is skipped"
        );
        assert!(
            value["messages"][0].get("reasoning").is_none(),
            "user messages are untouched"
        );
    }

    #[test]
    fn request_normalization_clamps_output_limits_one_way() {
        let over = request(json!({
            "model": "m",
            "messages": [],
            "max_tokens": 200_000,
            "max_completion_tokens": 100_000
        }));
        let value = parsed(&WireNormalization::OllamaCloud.normalize_request_body(over));
        assert_eq!(value["max_tokens"], OLLAMA_CLOUD_MAX_TOKENS_LIMIT);
        assert_eq!(
            value["max_completion_tokens"],
            OLLAMA_CLOUD_MAX_TOKENS_LIMIT
        );

        let at_limit = request(json!({"model": "m", "messages": [], "max_tokens": 65_535}));
        assert_eq!(
            WireNormalization::OllamaCloud.normalize_request_body(at_limit.clone()),
            at_limit,
            "the exact ceiling is untouched, byte-for-byte"
        );

        let under = request(json!({"model": "m", "messages": [], "max_tokens": 100}));
        assert_eq!(
            WireNormalization::OllamaCloud.normalize_request_body(under.clone()),
            under
        );

        let missing = request(json!({"model": "m", "messages": []}));
        assert_eq!(
            WireNormalization::OllamaCloud.normalize_request_body(missing.clone()),
            missing,
            "missing fields are untouched"
        );
    }

    #[test]
    fn none_marker_and_unaffected_bodies_keep_original_bytes() {
        let body = request(json!({
            "model": "m",
            "messages": [{"role": "user", "content": "hi"}],
            "max_tokens": 8
        }));
        assert_eq!(
            WireNormalization::None.normalize_request_body(body.clone()),
            body.clone(),
            "None never rewrites"
        );
        assert_eq!(
            WireNormalization::OllamaCloud.normalize_request_body(body.clone()),
            body,
            "an Ollama attempt without quirks keeps byte identity"
        );
        let not_json = Bytes::from_static(b"{not json");
        assert_eq!(
            WireNormalization::OllamaCloud.normalize_request_body(not_json.clone()),
            not_json,
            "non-JSON bodies pass through for upstream validation"
        );
        let mut value = json!({"error": true});
        WireNormalization::None.normalize_response_value(&mut value);
        assert_eq!(value, json!({"error": true}));
    }

    #[test]
    fn response_backfill_fills_reasoning_content_from_reasoning_or_thinking() {
        let mut delta = json!({
            "choices": [{"delta": {"content": "hi", "thinking": "chain"}}]
        });
        WireNormalization::OllamaCloud.normalize_response_value(&mut delta);
        assert_eq!(delta["choices"][0]["delta"]["reasoning_content"], "chain");
        assert_eq!(
            delta["choices"][0]["delta"]["thinking"], "chain",
            "source kept"
        );

        let mut message = json!({
            "choices": [{"message": {"role": "assistant", "reasoning": "why"}}]
        });
        WireNormalization::OllamaCloud.normalize_response_value(&mut message);
        assert_eq!(message["choices"][0]["message"]["reasoning_content"], "why");
        assert_eq!(message["choices"][0]["message"]["reasoning"], "why");

        // Existing reasoning_content wins; empty sources are skipped; other
        // shapes (tool calls, empty deltas) are untouched.
        let mut existing = json!({
            "choices": [{"delta": {"reasoning": "new", "reasoning_content": "old"}}]
        });
        WireNormalization::OllamaCloud.normalize_response_value(&mut existing);
        assert_eq!(existing["choices"][0]["delta"]["reasoning_content"], "old");

        let mut empty = json!({"choices": [{"delta": {"reasoning": ""}}]});
        let before = empty.clone();
        WireNormalization::OllamaCloud.normalize_response_value(&mut empty);
        assert_eq!(empty, before);

        let mut other = json!({"choices": [{"delta": {"tool_calls": []}}]});
        let before = other.clone();
        WireNormalization::OllamaCloud.normalize_response_value(&mut other);
        assert_eq!(other, before);
    }
}

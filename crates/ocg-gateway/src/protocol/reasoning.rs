//! Thinking/reasoning block encoding and request-option mapping.
//!
//! The public encode/decode entry points are re-exported from the parent
//! module so their paths stay `ocg_gateway::protocol::*`.

use super::NamespaceToolMapping;
use super::shared::uint;
use super::tools::responses_history_tool_name;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::{Map, Value, json};

pub(super) const ANTHROPIC_THINKING_ENCRYPTED_PREFIX: &str = "ocg-anthropic-thinking-v1:";
pub(super) const CHAT_REASONING_ENCRYPTED_PREFIX: &str = "ocg-chat-reasoning-v1:";
pub(super) const CHAT_TOOL_REASONING_PLACEHOLDER: &str = "Tool call reasoning unavailable.";

pub(super) fn backfill_chat_tool_reasoning(body: &mut Value) {
    let Some(messages) = body.get_mut("messages").and_then(Value::as_array_mut) else {
        return;
    };
    for message in messages {
        if message.get("role").and_then(Value::as_str) != Some("assistant")
            || message
                .get("tool_calls")
                .and_then(Value::as_array)
                .is_none_or(Vec::is_empty)
        {
            continue;
        }
        let existing = message
            .get("reasoning_content")
            .and_then(Value::as_str)
            .filter(|reasoning| !reasoning.trim().is_empty())
            .map(str::to_string)
            .or_else(|| {
                message
                    .get("reasoning")
                    .and_then(Value::as_str)
                    .filter(|reasoning| !reasoning.trim().is_empty())
                    .map(str::to_string)
            })
            .unwrap_or_else(|| CHAT_TOOL_REASONING_PLACEHOLDER.to_string());
        message["reasoning_content"] = json!(existing);
    }
}

pub(super) fn anthropic_tool_choice_to_chat(choice: &Value) -> Value {
    match choice
        .as_str()
        .or_else(|| choice.get("type").and_then(Value::as_str))
    {
        Some("any") => json!("required"),
        Some("tool") => json!({
            "type": "function",
            "function": { "name": choice.get("name").cloned().unwrap_or_else(|| json!("")) }
        }),
        Some(value) => json!(value),
        None => choice.clone(),
    }
}

pub(super) fn chat_tool_choice_to_anthropic(choice: &Value) -> Value {
    if let Some(value) = choice.as_str() {
        return json!({ "type": if value == "required" { "any" } else { value } });
    }
    if let Some(name) = choice.pointer("/function/name") {
        return json!({ "type": "tool", "name": name });
    }
    json!({ "type": "auto" })
}

pub(super) fn anthropic_tool_choice_to_responses(choice: &Value) -> Value {
    match choice
        .as_str()
        .or_else(|| choice.get("type").and_then(Value::as_str))
    {
        Some("any") => json!("required"),
        Some("tool") => json!({
            "type": "function",
            "name": choice.get("name").cloned().unwrap_or_else(|| json!(""))
        }),
        Some(value) => json!(value),
        None => choice.clone(),
    }
}

pub(super) fn responses_tool_choice_to_anthropic(
    choice: &Value,
    namespace_tools: &[NamespaceToolMapping],
) -> Value {
    if let Some(value) = choice.as_str() {
        return json!({ "type": if value == "required" { "any" } else { value } });
    }
    if matches!(
        choice.get("type").and_then(Value::as_str),
        Some("function" | "custom")
    ) {
        let name = responses_history_tool_name(choice, namespace_tools);
        return json!({
            "type": "tool",
            "name": name
        });
    }
    json!({ "type": "auto" })
}

pub(super) fn finish_anthropic_request_options(
    out: &mut Map<String, Value>,
    reasoning_effort: Option<&str>,
    parallel_tool_calls: Option<bool>,
) {
    let has_tools = out
        .get("tools")
        .and_then(Value::as_array)
        .is_some_and(|tools| !tools.is_empty());
    if !has_tools {
        out.remove("tool_choice");
    } else if parallel_tool_calls == Some(false) {
        let choice = out
            .entry("tool_choice")
            .or_insert_with(|| json!({ "type": "auto" }));
        if let Some(choice) = choice.as_object_mut() {
            choice.insert("disable_parallel_tool_use".into(), json!(true));
        }
    }

    let Some(effort) = reasoning_effort else {
        return;
    };
    let forced = matches!(
        out.get("tool_choice")
            .and_then(|choice| choice.get("type"))
            .and_then(Value::as_str),
        Some("any" | "tool")
    );
    let thinking = if forced {
        json!({ "type": "disabled" })
    } else {
        thinking_from_effort(effort, out)
    };
    if thinking.get("type").and_then(Value::as_str) != Some("disabled") {
        out.remove("temperature");
        out.remove("top_p");
    }
    out.insert("thinking".into(), thinking);
}

pub(super) fn anthropic_effort(body: &Value) -> Option<&'static str> {
    if let Some(effort) = body
        .pointer("/output_config/effort")
        .and_then(Value::as_str)
    {
        return match effort {
            "low" => Some("low"),
            "medium" => Some("medium"),
            "high" => Some("high"),
            "max" | "xhigh" => Some("high"),
            _ => None,
        };
    }
    let thinking = body.get("thinking")?;
    match thinking.get("type").and_then(Value::as_str) {
        Some("adaptive") => Some("high"),
        Some("enabled") => match uint(thinking, "budget_tokens") {
            0..=2048 => Some("low"),
            2049..=8192 => Some("medium"),
            _ => Some("high"),
        },
        _ => None,
    }
}

pub(super) fn thinking_from_effort(effort: &str, request: &Map<String, Value>) -> Value {
    let max = request
        .get("max_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(8192);
    if max <= 1024 || effort == "none" {
        return json!({ "type": "disabled" });
    }
    let target = match effort {
        "low" => 1024,
        "medium" => 4096,
        "high" | "xhigh" => 8192,
        _ => 4096,
    };
    let budget = target.min(max / 2);
    if budget < 1024 {
        json!({ "type": "disabled" })
    } else {
        json!({ "type": "enabled", "budget_tokens": budget })
    }
}

pub(super) fn inject_chat_usage(out: &mut Map<String, Value>) {
    if out.get("stream").and_then(Value::as_bool) != Some(true) {
        return;
    }
    let options = out.entry("stream_options").or_insert_with(|| json!({}));
    if let Some(options) = options.as_object_mut() {
        options.insert("include_usage".into(), json!(true));
    }
}

pub fn encode_anthropic_thinking_block(block: &Value) -> Option<String> {
    match block.get("type").and_then(Value::as_str) {
        Some("thinking")
            if block
                .get("signature")
                .and_then(Value::as_str)
                .is_some_and(|signature| !signature.is_empty()) => {}
        Some("redacted_thinking") if block.get("data").and_then(Value::as_str).is_some() => {}
        _ => return None,
    }
    let bytes = serde_json::to_vec(block).ok()?;
    Some(format!(
        "{ANTHROPIC_THINKING_ENCRYPTED_PREFIX}{}",
        URL_SAFE_NO_PAD.encode(bytes)
    ))
}

pub fn decode_anthropic_thinking_block(encrypted_content: &str) -> Option<Value> {
    let encoded = encrypted_content.strip_prefix(ANTHROPIC_THINKING_ENCRYPTED_PREFIX)?;
    let bytes = URL_SAFE_NO_PAD.decode(encoded).ok()?;
    let block: Value = serde_json::from_slice(&bytes).ok()?;
    match block.get("type").and_then(Value::as_str) {
        Some("thinking")
            if block
                .get("signature")
                .and_then(Value::as_str)
                .is_some_and(|signature| !signature.is_empty()) =>
        {
            Some(block)
        }
        Some("redacted_thinking") if block.get("data").and_then(Value::as_str).is_some() => {
            Some(block)
        }
        _ => None,
    }
}

pub fn encode_chat_reasoning(reasoning: &str) -> Option<String> {
    (!reasoning.is_empty()).then(|| {
        format!(
            "{CHAT_REASONING_ENCRYPTED_PREFIX}{}",
            URL_SAFE_NO_PAD.encode(reasoning.as_bytes())
        )
    })
}

pub fn decode_chat_reasoning(encrypted_content: &str) -> Option<String> {
    let encoded = encrypted_content.strip_prefix(CHAT_REASONING_ENCRYPTED_PREFIX)?;
    let bytes = URL_SAFE_NO_PAD.decode(encoded).ok()?;
    String::from_utf8(bytes)
        .ok()
        .filter(|text| !text.is_empty())
}

pub(super) fn reasoning_text(item: &Value) -> String {
    item.get("summary")
        .and_then(Value::as_array)
        .map(|parts| {
            parts
                .iter()
                .filter_map(|part| part.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .filter(|text| !text.is_empty())
        .or_else(|| {
            item.get("content")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_default()
}

pub(super) fn chat_stop_to_anthropic(reason: Option<&Value>) -> Value {
    json!(match reason.and_then(Value::as_str) {
        Some("tool_calls") | Some("function_call") => "tool_use",
        Some("length") => "max_tokens",
        Some("content_filter") => "refusal",
        Some("stop") | None => "end_turn",
        Some(other) => other,
    })
}

pub(super) fn anthropic_stop_to_chat(reason: Option<&Value>) -> Value {
    json!(match reason.and_then(Value::as_str) {
        Some("tool_use") => "tool_calls",
        Some("max_tokens") => "length",
        Some("refusal") => "content_filter",
        _ => "stop",
    })
}

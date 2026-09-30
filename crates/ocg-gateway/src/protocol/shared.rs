//! Private JSON helpers shared by protocol conversion children.
//!
//! Format-specific rules stay in the format modules; this file only holds
//! document-shape utilities used by more than one conversion direction.

use serde_json::{Map, Value, json};

pub(super) fn copy(source: &Value, target: &mut Map<String, Value>, from: &str, to: &str) {
    if let Some(value) = source.get(from) {
        target.insert(to.to_string(), value.clone());
    }
}

pub(super) fn array<'a>(value: &'a Value, key: &str) -> &'a [Value] {
    value
        .get(key)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

pub(super) fn system_text(system: Option<&Value>) -> Option<String> {
    match system {
        Some(Value::String(text)) if !text.is_empty() => Some(text.clone()),
        Some(Value::Array(parts)) => {
            let text = parts
                .iter()
                .filter_map(|part| part.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n\n");
            (!text.is_empty()).then_some(text)
        }
        _ => None,
    }
}

pub(super) fn anthropic_blocks(content: Option<&Value>) -> Vec<Value> {
    match content {
        Some(Value::String(text)) => vec![json!({ "type": "text", "text": text })],
        Some(Value::Array(blocks)) => blocks.clone(),
        _ => Vec::new(),
    }
}

pub(super) fn anthropic_image_url(block: &Value) -> Option<String> {
    let source = block.get("source")?;
    match source.get("type").and_then(Value::as_str) {
        Some("base64") | None => Some(format!(
            "data:{};base64,{}",
            source
                .get("media_type")
                .and_then(Value::as_str)
                .unwrap_or("image/png"),
            source
                .get("data")
                .and_then(Value::as_str)
                .unwrap_or_default()
        )),
        Some("url") => source
            .get("url")
            .and_then(Value::as_str)
            .map(str::to_string),
        _ => None,
    }
}

pub(super) fn anthropic_image(url: &str) -> Value {
    if let Some(rest) = url.strip_prefix("data:")
        && let Some((media_and_encoding, data)) = rest.split_once(',')
    {
        let media_type = media_and_encoding
            .strip_suffix(";base64")
            .unwrap_or(media_and_encoding);
        return json!({
            "type": "image",
            "source": { "type": "base64", "media_type": media_type, "data": data }
        });
    }
    json!({ "type": "image", "source": { "type": "url", "url": url } })
}

pub(super) fn chat_content_to_anthropic(content: Option<&Value>) -> Vec<Value> {
    match content {
        Some(Value::String(text)) if !text.is_empty() => {
            vec![json!({ "type": "text", "text": text })]
        }
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|part| match part.get("type").and_then(Value::as_str) {
                Some("text") | Some("output_text") => Some(json!({
                    "type": "text",
                    "text": part.get("text").cloned().unwrap_or_else(|| json!(""))
                })),
                Some("image_url") => part
                    .pointer("/image_url/url")
                    .or_else(|| part.get("image_url"))
                    .and_then(Value::as_str)
                    .map(anthropic_image),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

pub(super) fn chat_content_text(content: Option<&Value>) -> Option<String> {
    match content {
        Some(Value::String(text)) => Some(text.clone()),
        Some(Value::Array(parts)) => {
            let text = parts
                .iter()
                .filter_map(|part| part.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n");
            (!text.is_empty()).then_some(text)
        }
        _ => None,
    }
}

pub(super) fn parse_json(value: Option<&Value>) -> Option<Value> {
    match value {
        Some(Value::String(text)) => serde_json::from_str(text).ok(),
        Some(value) => Some(value.clone()),
        None => None,
    }
}

pub(super) fn json_string(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(text)) => text.clone(),
        Some(value) => serde_json::to_string(value).unwrap_or_else(|_| "{}".to_string()),
        None => "{}".to_string(),
    }
}

pub(super) fn tool_result_text(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(parts)) => {
            let texts = parts
                .iter()
                .filter_map(|part| part.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>();
            if texts.is_empty() {
                json_string(value)
            } else {
                texts.join("\n")
            }
        }
        Some(value) => json_string(Some(value)),
        None => String::new(),
    }
}

pub(super) fn push_message(messages: &mut Vec<Value>, role: &str, blocks: Vec<Value>) {
    if blocks.is_empty() {
        return;
    }
    if let Some(last) = messages.last_mut()
        && last.get("role").and_then(Value::as_str) == Some(role)
        && let Some(content) = last.get_mut("content").and_then(Value::as_array_mut)
    {
        content.extend(blocks);
        return;
    }
    messages.push(json!({ "role": role, "content": blocks }));
}

pub(super) fn drop_empty_messages(messages: &mut Vec<Value>) {
    messages.retain(|message| match message.get("content") {
        Some(Value::String(text)) => !text.trim().is_empty(),
        Some(Value::Array(parts)) => parts.iter().any(|part| {
            part.get("type").and_then(Value::as_str) != Some("text")
                || part
                    .get("text")
                    .and_then(Value::as_str)
                    .is_some_and(|text| !text.trim().is_empty())
        }),
        Some(value) => !value.is_null(),
        None => false,
    });
}

pub(super) fn ensure_leading_user_message(messages: &mut Vec<Value>) {
    if messages
        .first()
        .and_then(|message| message.get("role"))
        .and_then(Value::as_str)
        != Some("user")
    {
        messages.insert(
            0,
            json!({
                "role": "user",
                "content": [{ "type": "text", "text": "(continuing the conversation)" }]
            }),
        );
    }
}

pub(super) fn uint(value: &Value, key: &str) -> u64 {
    value.get(key).and_then(Value::as_u64).unwrap_or(0)
}

pub(super) fn empty_object() -> Value {
    json!({})
}

pub(super) fn empty_schema() -> Value {
    json!({ "type": "object", "properties": {} })
}

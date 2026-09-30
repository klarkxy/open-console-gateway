//! Response-document conversion. Cross-format pairs route through the Messages
//! document exactly as the original dispatcher did.

use super::reasoning::{
    anthropic_stop_to_chat, chat_stop_to_anthropic, encode_anthropic_thinking_block,
    encode_chat_reasoning, reasoning_text,
};
use super::shared::{chat_content_to_anthropic, empty_object, json_string, parse_json, uint};
use super::usage::{
    anthropic_usage_to_chat, anthropic_usage_to_responses, chat_usage_to_anthropic,
    responses_usage_to_anthropic,
};
use super::{ApiFormat, ConversionError, NamespaceToolMapping, ResponseSynthesis};
use serde_json::{Value, json};

pub(super) fn convert_between(
    upstream: ApiFormat,
    client: ApiFormat,
    body: &Value,
    custom_tools: &[String],
    namespace_tools: &[NamespaceToolMapping],
    model_hint: Option<&str>,
    synthesis: &ResponseSynthesis,
) -> Result<Value, ConversionError> {
    match (upstream, client) {
        (a, b) if a == b => Ok(body.clone()),
        (ApiFormat::Messages, ApiFormat::ChatCompletions) => {
            messages_response_to_chat(body, model_hint)
        }
        (ApiFormat::ChatCompletions, ApiFormat::Messages) => chat_response_to_messages(body),
        (ApiFormat::Messages, ApiFormat::Responses) => {
            messages_response_to_responses(body, custom_tools, namespace_tools, synthesis)
        }
        (ApiFormat::Responses, ApiFormat::Messages) => responses_response_to_messages(body),
        (ApiFormat::ChatCompletions, ApiFormat::Responses) => messages_response_to_responses(
            &chat_response_to_messages(body)?,
            custom_tools,
            namespace_tools,
            synthesis,
        ),
        (ApiFormat::Responses, ApiFormat::ChatCompletions) => {
            messages_response_to_chat(&responses_response_to_messages(body)?, model_hint)
        }
        (ApiFormat::Messages, ApiFormat::Gemini) => messages_response_to_gemini(body),
        (ApiFormat::ChatCompletions, ApiFormat::Gemini) => {
            messages_response_to_gemini(&chat_response_to_messages(body)?)
        }
        (ApiFormat::Responses, ApiFormat::Gemini) => {
            messages_response_to_gemini(&responses_response_to_messages(body)?)
        }
        _ => Err(ConversionError::new(
            "Gemini is a client-only format and cannot be used as an upstream protocol",
        )),
    }
}

pub(super) fn synthesized_response_id(id: &str, empty_response_id: &str) -> String {
    if id.starts_with("resp_") {
        id.to_string()
    } else if id.is_empty() {
        empty_response_id.to_string()
    } else {
        format!("resp_{id}")
    }
}

pub(super) fn chat_response_to_messages(body: &Value) -> Result<Value, ConversionError> {
    let choice = body
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .ok_or_else(|| ConversionError::new("Chat Completions response has no choice"))?;
    let message = choice
        .get("message")
        .ok_or_else(|| ConversionError::new("Chat Completions response has no message"))?;
    let mut content = chat_content_to_anthropic(message.get("content"));
    if let Some(reasoning) = message
        .get("reasoning_content")
        .or_else(|| message.get("reasoning"))
        .and_then(Value::as_str)
        && !reasoning.is_empty()
    {
        content.insert(
            0,
            json!({ "type": "thinking", "thinking": reasoning, "signature": "" }),
        );
    }
    if let Some(calls) = message.get("tool_calls").and_then(Value::as_array) {
        for call in calls {
            let function = call.get("function").unwrap_or(&Value::Null);
            content.push(json!({
                "type": "tool_use",
                "id": call.get("id").and_then(Value::as_str).unwrap_or_default(),
                "name": function.get("name").and_then(Value::as_str).unwrap_or_default(),
                "input": parse_json(function.get("arguments")).unwrap_or_else(empty_object)
            }));
        }
    }
    Ok(json!({
        "id": body.get("id").cloned().unwrap_or_else(|| json!("")),
        "type": "message",
        "role": "assistant",
        "model": body.get("model").cloned().unwrap_or_else(|| json!("")),
        "content": content,
        "stop_reason": chat_stop_to_anthropic(choice.get("finish_reason")),
        "stop_sequence": null,
        "usage": chat_usage_to_anthropic(body.get("usage"))
    }))
}

pub(super) fn messages_response_to_gemini(body: &Value) -> Result<Value, ConversionError> {
    let blocks = body
        .get("content")
        .and_then(Value::as_array)
        .ok_or_else(|| ConversionError::new("Messages response has no content"))?;
    let mut parts = Vec::new();
    for block in blocks {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => {
                if let Some(text) = block.get("text").and_then(Value::as_str)
                    && !text.is_empty()
                {
                    parts.push(json!({ "text": text }));
                }
            }
            Some("tool_use") => {
                let name = block.get("name").and_then(Value::as_str).unwrap_or("tool");
                let id = block
                    .get("id")
                    .and_then(Value::as_str)
                    .filter(|id| !id.is_empty())
                    .unwrap_or(name);
                parts.push(json!({
                    "functionCall": {
                        "id": id,
                        "name": name,
                        "args": block.get("input").cloned().unwrap_or_else(empty_object)
                    }
                }));
            }
            // Provider-specific reasoning signatures cannot be represented
            // safely as Gemini thought signatures, so do not replay them.
            Some("thinking" | "redacted_thinking") => {}
            _ => {}
        }
    }

    let stop_reason = body
        .get("stop_reason")
        .and_then(Value::as_str)
        .unwrap_or("end_turn");
    let finish_reason = match stop_reason {
        "max_tokens" | "model_context_window_exceeded" => "MAX_TOKENS",
        "refusal" => "SAFETY",
        _ => "STOP",
    };
    let mut candidate = json!({
        "content": { "role": "model", "parts": parts },
        "finishReason": finish_reason,
        "index": 0
    });
    if stop_reason == "refusal" {
        candidate["finishMessage"] = json!("upstream model refused the request");
    }

    let usage = body.get("usage").unwrap_or(&Value::Null);
    let cached = uint(usage, "cache_read_input_tokens");
    let created = uint(usage, "cache_creation_input_tokens");
    let input = uint(usage, "input_tokens")
        .saturating_add(cached)
        .saturating_add(created);
    let output = uint(usage, "output_tokens");
    let mut response = json!({
        "candidates": [candidate],
        "usageMetadata": {
            "promptTokenCount": input,
            "candidatesTokenCount": output,
            "totalTokenCount": input.saturating_add(output),
            "cachedContentTokenCount": cached
        }
    });
    if let Some(model) = body
        .get("model")
        .and_then(Value::as_str)
        .filter(|model| !model.is_empty())
    {
        response["modelVersion"] = json!(model);
    }
    if let Some(id) = body
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
    {
        response["responseId"] = json!(id);
    }
    Ok(response)
}

pub(super) fn messages_response_to_chat(
    body: &Value,
    _model_hint: Option<&str>,
) -> Result<Value, ConversionError> {
    let blocks = body
        .get("content")
        .and_then(Value::as_array)
        .ok_or_else(|| ConversionError::new("Messages response has no content"))?;
    let mut text = Vec::new();
    let mut reasoning = Vec::new();
    let mut calls = Vec::new();
    for block in blocks {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => {
                if let Some(value) = block.get("text").and_then(Value::as_str) {
                    text.push(value);
                }
            }
            Some("thinking") => {
                if let Some(value) = block.get("thinking").and_then(Value::as_str) {
                    reasoning.push(value);
                }
            }
            Some("tool_use") => calls.push(json!({
                "id": block.get("id").cloned().unwrap_or_else(|| json!("")),
                "type": "function",
                "function": {
                    "name": block.get("name").cloned().unwrap_or_else(|| json!("")),
                    "arguments": json_string(block.get("input"))
                }
            })),
            _ => {}
        }
    }
    let mut message = json!({ "role": "assistant", "content": text.join("") });
    if !reasoning.is_empty() {
        message["reasoning_content"] = json!(reasoning.join("\n"));
    }
    if !calls.is_empty() {
        message["tool_calls"] = Value::Array(calls);
        if text.is_empty() {
            message["content"] = Value::Null;
        }
    }
    let usage = body.get("usage").cloned().unwrap_or(Value::Null);
    Ok(json!({
        "id": body.get("id").cloned().unwrap_or_else(|| json!("")),
        "object": "chat.completion",
        "created": 0,
        "model": body.get("model").cloned().unwrap_or_else(|| json!("")),
        "choices": [{
            "index": 0,
            "message": message,
            "finish_reason": anthropic_stop_to_chat(body.get("stop_reason"))
        }],
        "usage": anthropic_usage_to_chat(Some(&usage))
    }))
}

pub(super) fn responses_response_to_messages(body: &Value) -> Result<Value, ConversionError> {
    let output = body
        .get("output")
        .and_then(Value::as_array)
        .ok_or_else(|| ConversionError::new("Responses response has no output"))?;
    let mut content = Vec::new();
    let mut has_tool = false;
    for item in output {
        match item.get("type").and_then(Value::as_str) {
            Some("message") => {
                for part in item
                    .get("content")
                    .and_then(Value::as_array)
                    .map(Vec::as_slice)
                    .unwrap_or(&[])
                {
                    match part.get("type").and_then(Value::as_str) {
                        Some("output_text") | Some("text") => content.push(json!({
                            "type": "text",
                            "text": part.get("text").cloned().unwrap_or_else(|| json!(""))
                        })),
                        Some("refusal") => content.push(json!({
                            "type": "text",
                            "text": part.get("refusal").cloned().unwrap_or_else(|| json!(""))
                        })),
                        _ => {}
                    }
                }
            }
            Some("function_call") => {
                has_tool = true;
                content.push(json!({
                    "type": "tool_use",
                    "id": item.get("call_id").or_else(|| item.get("id")).cloned().unwrap_or_else(|| json!("")),
                    "name": item.get("name").cloned().unwrap_or_else(|| json!("")),
                    "input": parse_json(item.get("arguments")).unwrap_or_else(empty_object)
                }));
            }
            Some("reasoning") => {
                let text = reasoning_text(item);
                if !text.is_empty() {
                    content.push(json!({ "type": "thinking", "thinking": text, "signature": "" }));
                }
            }
            _ => {}
        }
    }
    let stop = if has_tool {
        "tool_use"
    } else if body.get("status").and_then(Value::as_str) == Some("incomplete") {
        match body
            .pointer("/incomplete_details/reason")
            .and_then(Value::as_str)
        {
            Some("max_output_tokens") => "max_tokens",
            Some("content_filter") => "refusal",
            _ => "end_turn",
        }
    } else {
        "end_turn"
    };
    Ok(json!({
        "id": body.get("id").cloned().unwrap_or_else(|| json!("")),
        "type": "message",
        "role": "assistant",
        "model": body.get("model").cloned().unwrap_or_else(|| json!("")),
        "content": content,
        "stop_reason": stop,
        "stop_sequence": null,
        "usage": responses_usage_to_anthropic(body.get("usage"))
    }))
}

pub(super) fn messages_response_to_responses(
    body: &Value,
    custom_tools: &[String],
    namespace_tools: &[NamespaceToolMapping],
    synthesis: &ResponseSynthesis,
) -> Result<Value, ConversionError> {
    let blocks = body
        .get("content")
        .and_then(Value::as_array)
        .ok_or_else(|| ConversionError::new("Messages response has no content"))?;
    let response_id = body.get("id").and_then(Value::as_str).unwrap_or_default();
    let mut output = Vec::new();
    let mut message_parts = Vec::new();
    for block in blocks {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => message_parts.push(json!({
                "type": "output_text",
                "text": block.get("text").cloned().unwrap_or_else(|| json!("")),
                "annotations": []
            })),
            Some("thinking" | "redacted_thinking") => {
                flush_responses_text(&mut output, &mut message_parts, response_id);
                let summary = block
                    .get("thinking")
                    .and_then(Value::as_str)
                    .filter(|text| !text.is_empty())
                    .map(|text| vec![json!({ "type": "summary_text", "text": text })])
                    .unwrap_or_default();
                let encrypted_content = encode_anthropic_thinking_block(block).or_else(|| {
                    block
                        .get("thinking")
                        .and_then(Value::as_str)
                        .and_then(encode_chat_reasoning)
                });
                if let Some(encrypted_content) = encrypted_content {
                    output.push(json!({
                        "type": "reasoning",
                        "id": format!("rs_{response_id}_{}", output.len()),
                        "summary": summary,
                        "encrypted_content": encrypted_content
                    }));
                }
            }
            Some("tool_use") => {
                flush_responses_text(&mut output, &mut message_parts, response_id);
                let name = block
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let namespace_tool = namespace_tools
                    .iter()
                    .find(|mapping| mapping.flattened == name);
                if namespace_tool.is_some_and(|mapping| mapping.custom) {
                    let mapping = namespace_tool.expect("checked above");
                    let input = block
                        .pointer("/input/input")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                        .unwrap_or_else(|| json_string(block.get("input")));
                    output.push(json!({
                        "type": "custom_tool_call",
                        "id": block.get("id").cloned().unwrap_or_else(|| json!("")),
                        "call_id": block.get("id").cloned().unwrap_or_else(|| json!("")),
                        "namespace": mapping.namespace,
                        "name": mapping.name,
                        "input": input,
                        "status": "completed"
                    }));
                } else if let Some(mapping) = namespace_tool {
                    output.push(json!({
                        "type": "function_call",
                        "id": block.get("id").cloned().unwrap_or_else(|| json!("")),
                        "call_id": block.get("id").cloned().unwrap_or_else(|| json!("")),
                        "namespace": mapping.namespace,
                        "name": mapping.name,
                        "arguments": json_string(block.get("input")),
                        "status": "completed"
                    }));
                } else if custom_tools.iter().any(|custom| custom == name) {
                    let input = block
                        .pointer("/input/input")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                        .unwrap_or_else(|| json_string(block.get("input")));
                    output.push(json!({
                        "type": "custom_tool_call",
                        "id": block.get("id").cloned().unwrap_or_else(|| json!("")),
                        "call_id": block.get("id").cloned().unwrap_or_else(|| json!("")),
                        "name": name,
                        "input": input,
                        "status": "completed"
                    }));
                } else {
                    output.push(json!({
                        "type": "function_call",
                        "id": block.get("id").cloned().unwrap_or_else(|| json!("")),
                        "call_id": block.get("id").cloned().unwrap_or_else(|| json!("")),
                        "name": name,
                        "arguments": json_string(block.get("input")),
                        "status": "completed"
                    }));
                }
            }
            _ => {}
        }
    }
    flush_responses_text(&mut output, &mut message_parts, response_id);
    let (status, incomplete_details) = match body.get("stop_reason").and_then(Value::as_str) {
        Some("max_tokens" | "model_context_window_exceeded") => {
            ("incomplete", json!({"reason":"max_output_tokens"}))
        }
        Some("refusal") => ("incomplete", json!({"reason":"content_filter"})),
        _ => ("completed", Value::Null),
    };
    let created_at = synthesis.created_at;
    Ok(json!({
        "id": synthesized_response_id(response_id, &synthesis.empty_response_id),
        "object": "response",
        "created_at": created_at,
        "status": status,
        "background": false,
        "completed_at": if status == "completed" { json!(created_at) } else { Value::Null },
        "error": null,
        "incomplete_details": incomplete_details,
        "instructions": null,
        "max_output_tokens": null,
        "max_tool_calls": null,
        "model": body.get("model").cloned().unwrap_or_else(|| json!("")),
        "output": output,
        "parallel_tool_calls": true,
        "previous_response_id": null,
        "reasoning": { "effort": null, "summary": null },
        "store": false,
        "temperature": null,
        "text": { "format": { "type": "text" } },
        "tool_choice": "auto",
        "tools": [],
        "top_p": null,
        "truncation": "disabled",
        "usage": anthropic_usage_to_responses(body.get("usage")),
        "user": null,
        "metadata": {}
    }))
}

pub(super) fn flush_responses_text(
    output: &mut Vec<Value>,
    parts: &mut Vec<Value>,
    response_id: &str,
) {
    if parts.is_empty() {
        return;
    }
    output.push(json!({
        "type": "message",
        "id": format!("msg_{response_id}_{}", output.len()),
        "role": "assistant",
        "status": "completed",
        "content": std::mem::take(parts)
    }));
}

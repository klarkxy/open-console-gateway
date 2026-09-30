//! Request-document conversion. Each format pair keeps its own rules; there is
//! no shared intermediate representation.

use super::reasoning::{
    anthropic_effort, anthropic_tool_choice_to_chat, anthropic_tool_choice_to_responses,
    backfill_chat_tool_reasoning, chat_tool_choice_to_anthropic, decode_anthropic_thinking_block,
    decode_chat_reasoning, finish_anthropic_request_options, inject_chat_usage,
    responses_tool_choice_to_anthropic,
};
use super::shared::{
    anthropic_blocks, anthropic_image, anthropic_image_url, array, chat_content_text,
    chat_content_to_anthropic, copy, drop_empty_messages, empty_object, empty_schema,
    ensure_leading_user_message, json_string, parse_json, push_message, system_text,
    tool_result_text,
};
use super::tools::{responses_history_tool_name, responses_tool_to_anthropic};
use super::validation::gemini_output_schema;
use super::{ApiFormat, ConversionError, NamespaceToolMapping};
use serde_json::{Map, Value, json};

pub(super) fn convert_request(
    client: ApiFormat,
    upstream: ApiFormat,
    body: Value,
    namespace_tools: &[NamespaceToolMapping],
) -> Result<Value, ConversionError> {
    let gemini_schema = if client == ApiFormat::Gemini {
        gemini_output_schema(&body)?
    } else {
        None
    };
    let converted = match (client, upstream) {
        (a, b) if a == b => body,
        (ApiFormat::Messages, ApiFormat::ChatCompletions) => messages_request_to_chat(body)?,
        (ApiFormat::ChatCompletions, ApiFormat::Messages) => chat_request_to_messages(body)?,
        (ApiFormat::Messages, ApiFormat::Responses) => messages_request_to_responses(body)?,
        (ApiFormat::Responses, ApiFormat::Messages) => {
            responses_request_to_messages(body, false, namespace_tools)?
        }
        (ApiFormat::ChatCompletions, ApiFormat::Responses) => {
            messages_request_to_responses(chat_request_to_messages(body)?)?
        }
        (ApiFormat::Responses, ApiFormat::ChatCompletions) => {
            messages_request_to_chat(responses_request_to_messages(body, true, namespace_tools)?)?
        }
        (ApiFormat::Gemini, ApiFormat::Messages) => gemini_request_to_messages(body)?,
        (ApiFormat::Gemini, ApiFormat::ChatCompletions) => {
            messages_request_to_chat(gemini_request_to_messages(body)?)?
        }
        (ApiFormat::Gemini, ApiFormat::Responses) => {
            messages_request_to_responses(gemini_request_to_messages(body)?)?
        }
        _ => {
            return Err(ConversionError::new(
                "Gemini is a client-only format and requires a known native upstream protocol",
            ));
        }
    };

    let mut converted = converted;
    if upstream == ApiFormat::ChatCompletions
        && let Some(out) = converted.as_object_mut()
    {
        // Chat passthrough used to leave stream_options untouched. OpenAI-compatible
        // Zen/Go backends (especially flash-free) omit the trailing usage chunk
        // unless include_usage is set, which made /v1 logs success_no_usage.
        inject_chat_usage(out);
    }
    if let Some(schema) = gemini_schema {
        match upstream {
            ApiFormat::Messages => {
                converted["output_config"] = json!({
                    "format": { "type": "json_schema", "schema": schema }
                });
            }
            ApiFormat::ChatCompletions => {
                converted["response_format"] = json!({
                    "type": "json_schema",
                    "json_schema": {
                        "name": "gemini_response",
                        "strict": true,
                        "schema": schema
                    }
                });
            }
            ApiFormat::Responses => {
                converted["text"] = json!({
                    "format": { "type": "json_schema", "name": "gemini_response", "schema": schema }
                });
            }
            ApiFormat::Gemini => {
                return Err(ConversionError::new(
                    "Gemini cannot be used as an upstream protocol",
                ));
            }
        }
    }
    if upstream == ApiFormat::ChatCompletions && client != ApiFormat::ChatCompletions {
        backfill_chat_tool_reasoning(&mut converted);
        return Ok(converted);
    }
    if upstream != ApiFormat::Messages {
        return Ok(converted);
    }
    let mut converted = normalize_messages_system_roles(converted)?;
    if matches!(client, ApiFormat::Responses | ApiFormat::Gemini)
        && let Some(messages) = converted.get_mut("messages").and_then(Value::as_array_mut)
    {
        ensure_leading_user_message(messages);
    }
    Ok(converted)
}

pub(super) fn normalize_messages_system_roles(mut body: Value) -> Result<Value, ConversionError> {
    let object = body
        .as_object_mut()
        .ok_or_else(|| ConversionError::new("Messages request must be a JSON object"))?;
    let Some(messages) = object.get("messages").and_then(Value::as_array) else {
        return Ok(body);
    };

    let mut system_blocks = Vec::new();
    let mut remaining = Vec::with_capacity(messages.len());
    for message in messages {
        match message.get("role").and_then(Value::as_str) {
            Some("system" | "developer") => {
                append_system_blocks(&mut system_blocks, message.get("content"));
            }
            _ => {
                if let Some(message) = sanitize_messages_history(message) {
                    remaining.push(message);
                }
            }
        }
    }
    object.insert("messages".into(), Value::Array(remaining));
    if !system_blocks.is_empty() {
        let mut combined = Vec::new();
        append_system_blocks(&mut combined, object.get("system"));
        combined.extend(system_blocks);
        object.insert("system".into(), Value::Array(combined));
    }
    Ok(body)
}

pub(super) fn sanitize_messages_history(message: &Value) -> Option<Value> {
    if message.get("role").and_then(Value::as_str) != Some("assistant") {
        return Some(message.clone());
    }
    let Some(blocks) = message.get("content").and_then(Value::as_array) else {
        return Some(message.clone());
    };
    let filtered = blocks
        .iter()
        .filter(|block| match block.get("type").and_then(Value::as_str) {
            Some("thinking") => block
                .get("signature")
                .and_then(Value::as_str)
                .is_some_and(|signature| !signature.is_empty()),
            Some("redacted_thinking") => block
                .get("data")
                .and_then(Value::as_str)
                .is_some_and(|data| !data.is_empty()),
            _ => true,
        })
        .cloned()
        .collect::<Vec<_>>();
    if !blocks.is_empty() && filtered.is_empty() {
        return None;
    }
    let mut message = message.clone();
    message["content"] = Value::Array(filtered);
    Some(message)
}

pub(super) fn append_system_blocks(target: &mut Vec<Value>, content: Option<&Value>) {
    match content {
        Some(Value::String(text)) => {
            target.push(json!({ "type": "text", "text": text }));
        }
        Some(Value::Array(blocks)) => {
            target.extend(blocks.iter().filter_map(|block| match block {
                Value::String(text) => Some(json!({ "type": "text", "text": text })),
                Value::Object(_) if block.get("text").and_then(Value::as_str).is_some() => {
                    Some(block.clone())
                }
                _ => None,
            }));
        }
        Some(Value::Object(_)) if content.and_then(|value| value.get("text")).is_some() => {
            target.push(content.cloned().unwrap_or(Value::Null));
        }
        _ => {}
    }
}

pub(super) fn gemini_request_to_messages(body: Value) -> Result<Value, ConversionError> {
    let mut out = Map::new();
    copy(&body, &mut out, "model", "model");
    copy(&body, &mut out, "stream", "stream");

    let generation = body.get("generationConfig").unwrap_or(&Value::Null);
    // Gemini CLI currently sends topK and thinkingConfig in its chat defaults.
    // Neither field has one portable meaning across every supported Chat and
    // Messages upstream, so they are accepted as compatibility hints but are
    // intentionally not forwarded as provider-specific request fields.
    copy(generation, &mut out, "temperature", "temperature");
    copy(generation, &mut out, "topP", "top_p");
    copy(generation, &mut out, "stopSequences", "stop_sequences");
    out.insert(
        "max_tokens".into(),
        generation
            .get("maxOutputTokens")
            .cloned()
            .unwrap_or_else(|| json!(8192)),
    );

    if let Some(system) = body.get("systemInstruction") {
        let text = array(system, "parts")
            .iter()
            .filter_map(|part| part.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n\n");
        if !text.is_empty() {
            out.insert("system".into(), json!(text));
        }
    }

    let mut messages = Vec::new();
    for content in array(&body, "contents") {
        let role = if content.get("role").and_then(Value::as_str) == Some("model") {
            "assistant"
        } else {
            "user"
        };
        let mut blocks = Vec::new();
        for part in array(content, "parts") {
            if part.get("thought").and_then(Value::as_bool) == Some(true) {
                // Thought signatures are provider-specific and cannot be replayed
                // safely across protocols. Gemini CLI keeps the visible answer and
                // tool calls independently, so dropping thought history is safe.
                continue;
            }
            if let Some(text) = part.get("text").and_then(Value::as_str) {
                if !text.is_empty() {
                    blocks.push(json!({ "type": "text", "text": text }));
                }
                continue;
            }
            if let Some(data) = part.get("inlineData").or_else(|| part.get("inline_data")) {
                let media_type = data
                    .get("mimeType")
                    .or_else(|| data.get("mime_type"))
                    .and_then(Value::as_str)
                    .unwrap_or("image/png");
                let encoded = data.get("data").and_then(Value::as_str).unwrap_or_default();
                blocks.push(json!({
                    "type": "image",
                    "source": { "type": "base64", "media_type": media_type, "data": encoded }
                }));
                continue;
            }
            if let Some(call) = part
                .get("functionCall")
                .or_else(|| part.get("function_call"))
            {
                let name = call.get("name").and_then(Value::as_str).unwrap_or("tool");
                let id = call
                    .get("id")
                    .and_then(Value::as_str)
                    .filter(|id| !id.is_empty())
                    .unwrap_or(name);
                blocks.push(json!({
                    "type": "tool_use",
                    "id": id,
                    "name": name,
                    "input": call.get("args").cloned().unwrap_or_else(empty_object)
                }));
                continue;
            }
            if let Some(response) = part
                .get("functionResponse")
                .or_else(|| part.get("function_response"))
            {
                let name = response
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("tool");
                let id = response
                    .get("id")
                    .and_then(Value::as_str)
                    .filter(|id| !id.is_empty())
                    .unwrap_or(name);
                let payload = response.get("response").filter(|value| !value.is_null());
                let mut block = json!({
                    "type": "tool_result",
                    "tool_use_id": id,
                    "content": json_string(payload)
                });
                if response.get("isError").and_then(Value::as_bool) == Some(true) {
                    block["is_error"] = json!(true);
                }
                blocks.push(block);
            }
        }
        push_message(&mut messages, role, blocks);
    }
    drop_empty_messages(&mut messages);
    if messages.is_empty() {
        return Err(ConversionError::new(
            "Gemini contents cannot be converted to an empty message history",
        ));
    }
    out.insert("messages".into(), Value::Array(messages));

    let function_config = body.pointer("/toolConfig/functionCallingConfig");
    let allowed_names = function_config
        .and_then(|config| config.get("allowedFunctionNames"))
        .and_then(Value::as_array)
        .map(|names| {
            names
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let mut tools = Vec::new();
    for tool in array(&body, "tools") {
        for declaration in array(tool, "functionDeclarations") {
            let name = declaration
                .get("name")
                .and_then(Value::as_str)
                .filter(|name| !name.is_empty())
                .ok_or_else(|| {
                    ConversionError::new("Gemini function declaration name is required")
                })?;
            if !allowed_names.is_empty() && !allowed_names.iter().any(|allowed| allowed == name) {
                continue;
            }
            tools.push(json!({
                "name": name,
                "description": declaration.get("description").cloned().unwrap_or(Value::Null),
                "input_schema": declaration
                    .get("parametersJsonSchema")
                    .or_else(|| declaration.get("parameters"))
                    .cloned()
                    .unwrap_or_else(empty_schema)
            }));
        }
    }
    if !allowed_names.is_empty() {
        for name in &allowed_names {
            if !tools
                .iter()
                .any(|tool| tool.get("name").and_then(Value::as_str) == Some(name))
            {
                return Err(ConversionError::new(format!(
                    "Gemini allowed function `{name}` is not declared"
                )));
            }
        }
    }
    if !tools.is_empty() {
        out.insert("tools".into(), Value::Array(tools));
    }
    if let Some(config) = function_config {
        let mode = config
            .get("mode")
            .and_then(Value::as_str)
            .unwrap_or("AUTO")
            .to_ascii_uppercase();
        let choice = match mode.as_str() {
            "AUTO" => json!({ "type": "auto" }),
            "ANY" if allowed_names.len() == 1 => {
                json!({ "type": "tool", "name": allowed_names[0] })
            }
            "ANY" => json!({ "type": "any" }),
            "NONE" => {
                out.remove("tools");
                Value::Null
            }
            _ => {
                return Err(ConversionError::new(format!(
                    "Gemini function calling mode `{mode}` is not supported"
                )));
            }
        };
        if !choice.is_null() {
            out.insert("tool_choice".into(), choice);
        }
    }
    finish_anthropic_request_options(&mut out, None, None);
    Ok(Value::Object(out))
}

pub(super) fn messages_request_to_chat(body: Value) -> Result<Value, ConversionError> {
    let mut out = Map::new();
    copy(&body, &mut out, "model", "model");
    copy(&body, &mut out, "stream", "stream");
    copy(&body, &mut out, "temperature", "temperature");
    copy(&body, &mut out, "top_p", "top_p");
    copy(&body, &mut out, "max_tokens", "max_tokens");
    copy(&body, &mut out, "stop_sequences", "stop");

    let mut messages = Vec::new();
    if let Some(system) = system_text(body.get("system")) {
        messages.push(json!({ "role": "system", "content": system }));
    }
    for message in array(&body, "messages") {
        messages.extend(message_to_chat(message)?);
    }
    out.insert("messages".into(), Value::Array(messages));

    if let Some(tools) = body.get("tools").and_then(Value::as_array) {
        let tools = tools
            .iter()
            .filter_map(|tool| {
                let name = tool.get("name")?.as_str()?;
                Some(json!({
                    "type": "function",
                    "function": {
                        "name": name,
                        "description": tool.get("description").cloned().unwrap_or(Value::Null),
                        "parameters": tool.get("input_schema").cloned().unwrap_or_else(empty_schema)
                    }
                }))
            })
            .collect::<Vec<_>>();
        if !tools.is_empty() {
            out.insert("tools".into(), Value::Array(tools));
        }
    }
    if let Some(choice) = body.get("tool_choice") {
        out.insert("tool_choice".into(), anthropic_tool_choice_to_chat(choice));
        if choice
            .get("disable_parallel_tool_use")
            .and_then(Value::as_bool)
            == Some(true)
        {
            out.insert("parallel_tool_calls".into(), json!(false));
        }
    }
    if body.pointer("/thinking/type").and_then(Value::as_str) == Some("disabled") {
        out.insert("thinking".into(), json!({ "type": "disabled" }));
    } else if let Some(effort) = anthropic_effort(&body) {
        out.insert("reasoning_effort".into(), json!(effort));
    }
    inject_chat_usage(&mut out);
    Ok(Value::Object(out))
}

pub(super) fn chat_request_to_messages(body: Value) -> Result<Value, ConversionError> {
    let mut out = Map::new();
    copy(&body, &mut out, "model", "model");
    copy(&body, &mut out, "stream", "stream");
    copy(&body, &mut out, "temperature", "temperature");
    copy(&body, &mut out, "top_p", "top_p");
    if let Some(service_tier) = body.get("service_tier").and_then(Value::as_str) {
        out.insert("service_tier".into(), json!(service_tier));
    }
    match body.get("stop") {
        Some(Value::String(stop)) => {
            out.insert("stop_sequences".into(), json!([stop]));
        }
        Some(Value::Array(stops)) => {
            out.insert("stop_sequences".into(), Value::Array(stops.clone()));
        }
        Some(Value::Null) | None => {}
        Some(_) => {
            return Err(ConversionError::new(
                "Chat Completions stop must be a string or array",
            ));
        }
    }
    out.insert(
        "max_tokens".into(),
        body.get("max_completion_tokens")
            .or_else(|| body.get("max_tokens"))
            .cloned()
            .unwrap_or_else(|| json!(8192)),
    );

    let mut systems = Vec::new();
    let mut messages = Vec::new();
    for message in array(&body, "messages") {
        let role = message
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or("user");
        if role == "system" || role == "developer" {
            if let Some(text) = chat_content_text(message.get("content")) {
                systems.push(text);
            }
            continue;
        }
        if role == "tool" {
            let id = message
                .get("tool_call_id")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let content = message.get("content").cloned().unwrap_or(Value::Null);
            push_message(
                &mut messages,
                "user",
                vec![json!({ "type": "tool_result", "tool_use_id": id, "content": content })],
            );
            continue;
        }
        let mut blocks = chat_content_to_anthropic(message.get("content"));
        if role == "assistant"
            && let Some(calls) = message.get("tool_calls").and_then(Value::as_array)
        {
            for call in calls {
                let function = call.get("function").unwrap_or(&Value::Null);
                blocks.push(json!({
                    "type": "tool_use",
                    "id": call.get("id").and_then(Value::as_str).unwrap_or_default(),
                    "name": function.get("name").and_then(Value::as_str).unwrap_or_default(),
                    "input": parse_json(function.get("arguments")).unwrap_or_else(empty_object)
                }));
            }
        }
        if !blocks.is_empty() {
            push_message(
                &mut messages,
                if role == "assistant" {
                    "assistant"
                } else {
                    "user"
                },
                blocks,
            );
        }
    }
    if !systems.is_empty() {
        out.insert("system".into(), json!(systems.join("\n\n")));
    }
    out.insert("messages".into(), Value::Array(messages));

    if let Some(tools) = body.get("tools").and_then(Value::as_array) {
        let converted = tools
            .iter()
            .filter_map(|tool| {
                let function = tool.get("function")?;
                Some(json!({
                    "name": function.get("name")?,
                    "description": function.get("description").cloned().unwrap_or(Value::Null),
                    "input_schema": function.get("parameters").cloned().unwrap_or_else(empty_schema)
                }))
            })
            .collect::<Vec<_>>();
        if !converted.is_empty() {
            out.insert("tools".into(), Value::Array(converted));
        }
    }
    if let Some(choice) = body.get("tool_choice") {
        out.insert("tool_choice".into(), chat_tool_choice_to_anthropic(choice));
    }
    finish_anthropic_request_options(
        &mut out,
        body.get("reasoning_effort").and_then(Value::as_str),
        body.get("parallel_tool_calls").and_then(Value::as_bool),
    );
    Ok(Value::Object(out))
}

pub(super) fn flush_response_parts(input: &mut Vec<Value>, parts: &mut Vec<Value>, role: &str) {
    if parts.is_empty() {
        return;
    }
    let content = std::mem::take(parts);
    input.push(json!({ "type": "message", "role": role, "content": content }));
}

pub(super) fn messages_request_to_responses(body: Value) -> Result<Value, ConversionError> {
    let mut out = Map::new();
    copy(&body, &mut out, "model", "model");
    copy(&body, &mut out, "stream", "stream");
    copy(&body, &mut out, "temperature", "temperature");
    copy(&body, &mut out, "top_p", "top_p");
    copy(&body, &mut out, "max_tokens", "max_output_tokens");
    if let Some(service_tier) = body.get("service_tier").and_then(Value::as_str) {
        out.insert("service_tier".into(), json!(service_tier));
    }
    if let Some(system) = system_text(body.get("system")) {
        out.insert("instructions".into(), json!(system));
    }

    let mut input = Vec::new();
    for message in array(&body, "messages") {
        let role = message
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or("user");
        let blocks = anthropic_blocks(message.get("content"));
        let mut parts = Vec::new();
        for block in blocks {
            match block.get("type").and_then(Value::as_str) {
                Some("text") => parts.push(json!({
                    "type": if role == "assistant" { "output_text" } else { "input_text" },
                    "text": block.get("text").cloned().unwrap_or_else(|| json!(""))
                })),
                Some("image") => {
                    if let Some(url) = anthropic_image_url(&block) {
                        parts.push(json!({ "type": "input_image", "image_url": url }));
                    }
                }
                Some("tool_use") => {
                    flush_response_parts(&mut input, &mut parts, role);
                    input.push(json!({
                        "type": "function_call",
                        "call_id": block.get("id").cloned().unwrap_or_else(|| json!("")),
                        "name": block.get("name").cloned().unwrap_or_else(|| json!("")),
                        "arguments": json_string(block.get("input"))
                    }));
                }
                Some("tool_result") => {
                    flush_response_parts(&mut input, &mut parts, role);
                    input.push(json!({
                        "type": "function_call_output",
                        "call_id": block.get("tool_use_id").cloned().unwrap_or_else(|| json!("")),
                        "output": tool_result_text(block.get("content"))
                    }));
                }
                Some("thinking") => {
                    flush_response_parts(&mut input, &mut parts, role);
                    input.push(json!({
                        "type": "reasoning",
                        "summary": [{ "type": "summary_text", "text": block.get("thinking").cloned().unwrap_or_else(|| json!("")) }]
                    }));
                }
                _ => {}
            }
        }
        if !parts.is_empty() {
            input.push(json!({ "type": "message", "role": role, "content": parts }));
        }
    }
    out.insert("input".into(), Value::Array(input));

    if let Some(tools) = body.get("tools").and_then(Value::as_array) {
        let tools = tools
            .iter()
            .filter_map(|tool| {
                Some(json!({
                    "type": "function",
                    "name": tool.get("name")?,
                    "description": tool.get("description").cloned().unwrap_or(Value::Null),
                    "parameters": tool.get("input_schema").cloned().unwrap_or_else(empty_schema)
                }))
            })
            .collect::<Vec<_>>();
        if !tools.is_empty() {
            out.insert("tools".into(), Value::Array(tools));
        }
    }
    if let Some(choice) = body.get("tool_choice") {
        out.insert(
            "tool_choice".into(),
            anthropic_tool_choice_to_responses(choice),
        );
        if choice
            .get("disable_parallel_tool_use")
            .and_then(Value::as_bool)
            == Some(true)
        {
            out.insert("parallel_tool_calls".into(), json!(false));
        }
    }
    if let Some(effort) = anthropic_effort(&body) {
        out.insert(
            "reasoning".into(),
            json!({ "effort": effort, "summary": "auto" }),
        );
    }
    // OpenCode-Go Responses is used statelessly from this gateway.
    out.insert("store".into(), json!(false));
    Ok(Value::Object(out))
}

pub(super) fn responses_request_to_messages(
    body: Value,
    restore_chat_reasoning: bool,
    namespace_tools: &[NamespaceToolMapping],
) -> Result<Value, ConversionError> {
    let mut out = Map::new();
    copy(&body, &mut out, "model", "model");
    copy(&body, &mut out, "stream", "stream");
    copy(&body, &mut out, "temperature", "temperature");
    copy(&body, &mut out, "top_p", "top_p");
    if let Some(service_tier) = body.get("service_tier").and_then(Value::as_str) {
        out.insert("service_tier".into(), json!(service_tier));
    }
    out.insert(
        "max_tokens".into(),
        body.get("max_output_tokens")
            .cloned()
            .unwrap_or_else(|| json!(8192)),
    );
    if let Some(instructions) = body.get("instructions").and_then(Value::as_str) {
        out.insert("system".into(), json!(instructions));
    }
    let mut messages = Vec::new();
    match body.get("input") {
        Some(Value::String(text)) => push_message(
            &mut messages,
            "user",
            vec![json!({ "type": "text", "text": text })],
        ),
        Some(Value::Array(items)) => {
            for item in items {
                responses_item_to_messages(
                    item,
                    &mut messages,
                    restore_chat_reasoning,
                    namespace_tools,
                )?;
            }
        }
        _ => {}
    }
    drop_empty_messages(&mut messages);
    if messages.is_empty() {
        return Err(ConversionError::new(
            "Responses input cannot be converted to an empty Messages history",
        ));
    }
    out.insert("messages".into(), Value::Array(messages));

    if let Some(tools) = body.get("tools").and_then(Value::as_array) {
        let mut converted = Vec::new();
        for tool in tools {
            match tool.get("type").and_then(Value::as_str) {
                Some("function" | "custom") => {
                    if let Some(tool) = responses_tool_to_anthropic(tool, None, namespace_tools) {
                        converted.push(tool);
                    }
                }
                Some("namespace") => {
                    let namespace = tool.get("name").and_then(Value::as_str).unwrap_or_default();
                    for nested in array(tool, "tools") {
                        if let Some(tool) =
                            responses_tool_to_anthropic(nested, Some(namespace), namespace_tools)
                        {
                            converted.push(tool);
                        }
                    }
                }
                _ => {}
            }
        }
        if !converted.is_empty() {
            out.insert("tools".into(), Value::Array(converted));
        }
    }
    if let Some(choice) = body.get("tool_choice") {
        out.insert(
            "tool_choice".into(),
            responses_tool_choice_to_anthropic(choice, namespace_tools),
        );
    }
    finish_anthropic_request_options(
        &mut out,
        body.pointer("/reasoning/effort").and_then(Value::as_str),
        body.get("parallel_tool_calls").and_then(Value::as_bool),
    );
    Ok(Value::Object(out))
}

pub(super) fn message_to_chat(message: &Value) -> Result<Vec<Value>, ConversionError> {
    let role = match message
        .get("role")
        .and_then(Value::as_str)
        .unwrap_or("user")
    {
        // Chat 上游只接受 system/user/assistant/tool；Responses/Messages 中间态可能携带
        // developer 指令角色，归一化为 system 再透传，避免上游拒收。
        "developer" => "system",
        role => role,
    };
    let content = message.get("content");
    if let Some(text) = content.and_then(Value::as_str) {
        return Ok(vec![json!({ "role": role, "content": text })]);
    }
    let blocks = anthropic_blocks(content);
    let mut parts = Vec::new();
    let mut calls = Vec::new();
    let mut tool_messages = Vec::new();
    let mut reasoning = Vec::new();
    for block in blocks {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => parts.push(json!({
                "type": "text",
                "text": block.get("text").cloned().unwrap_or_else(|| json!(""))
            })),
            Some("image") => {
                if let Some(url) = anthropic_image_url(&block) {
                    parts.push(json!({ "type": "image_url", "image_url": { "url": url } }));
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
            Some("tool_result") => tool_messages.push(json!({
                "role": "tool",
                "tool_call_id": block.get("tool_use_id").cloned().unwrap_or_else(|| json!("")),
                "content": tool_result_text(block.get("content"))
            })),
            Some("thinking") => {
                if let Some(text) = block.get("thinking").and_then(Value::as_str) {
                    reasoning.push(text.to_string());
                }
            }
            _ => {}
        }
    }
    let mut result = tool_messages;
    if !parts.is_empty() || !calls.is_empty() || !reasoning.is_empty() {
        let content = if parts.is_empty() {
            Value::Null
        } else if parts.len() == 1 && parts[0].get("type").and_then(Value::as_str) == Some("text") {
            parts[0].get("text").cloned().unwrap_or(Value::Null)
        } else {
            Value::Array(parts)
        };
        let mut converted = json!({ "role": role, "content": content });
        if !calls.is_empty() {
            converted["tool_calls"] = Value::Array(calls);
        }
        if !reasoning.is_empty() {
            converted["reasoning_content"] = json!(reasoning.join("\n"));
        }
        result.push(converted);
    }
    Ok(result)
}

pub(super) fn responses_item_to_messages(
    item: &Value,
    messages: &mut Vec<Value>,
    restore_chat_reasoning: bool,
    namespace_tools: &[NamespaceToolMapping],
) -> Result<(), ConversionError> {
    match item.get("type").and_then(Value::as_str) {
        Some("function_call") => {
            let name = responses_history_tool_name(item, namespace_tools);
            push_message(
                messages,
                "assistant",
                vec![json!({
                    "type": "tool_use",
                    "id": item.get("call_id").or_else(|| item.get("id")).cloned().unwrap_or_else(|| json!("")),
                    "name": name,
                    "input": parse_json(item.get("arguments")).unwrap_or_else(empty_object)
                })],
            );
        }
        Some("function_call_output") => push_message(
            messages,
            "user",
            vec![json!({
                "type": "tool_result",
                "tool_use_id": item.get("call_id").cloned().unwrap_or_else(|| json!("")),
                "content": responses_tool_output_to_anthropic(item.get("output"))
            })],
        ),
        Some("custom_tool_call") => {
            let name = responses_history_tool_name(item, namespace_tools);
            push_message(
                messages,
                "assistant",
                vec![json!({
                    "type": "tool_use",
                    "id": item.get("call_id").or_else(|| item.get("id")).cloned().unwrap_or_else(|| json!("")),
                    "name": name,
                    "input": { "input": item.get("input").cloned().unwrap_or_else(|| json!("")) }
                })],
            );
        }
        Some("custom_tool_call_output") => push_message(
            messages,
            "user",
            vec![json!({
                "type": "tool_result",
                "tool_use_id": item.get("call_id").cloned().unwrap_or_else(|| json!("")),
                "content": responses_tool_output_to_anthropic(item.get("output"))
            })],
        ),
        Some("tool_search_call" | "tool_search_output" | "web_search_call") => {}
        Some("reasoning") => {
            let encrypted = item.get("encrypted_content").and_then(Value::as_str);
            let block = encrypted
                .and_then(decode_anthropic_thinking_block)
                .or_else(|| {
                    restore_chat_reasoning
                        .then(|| encrypted.and_then(decode_chat_reasoning))
                        .flatten()
                        .map(|reasoning| json!({ "type": "thinking", "thinking": reasoning }))
                });
            let Some(block) = block else {
                return Ok(());
            };
            if let Some(last) = messages.last_mut()
                && last.get("role").and_then(Value::as_str) == Some("assistant")
                && let Some(content) = last.get_mut("content").and_then(Value::as_array_mut)
            {
                let index = content
                    .iter()
                    .position(|part| {
                        !matches!(
                            part.get("type").and_then(Value::as_str),
                            Some("thinking" | "redacted_thinking")
                        )
                    })
                    .unwrap_or(content.len());
                content.insert(index, block);
            } else {
                push_message(messages, "assistant", vec![block]);
            }
        }
        Some("message") | None => {
            let role = item.get("role").and_then(Value::as_str).unwrap_or("user");
            let mut blocks = Vec::new();
            match item.get("content") {
                Some(Value::String(text)) => blocks.push(json!({ "type": "text", "text": text })),
                Some(Value::Array(parts)) => {
                    for part in parts {
                        match part.get("type").and_then(Value::as_str) {
                            Some("input_text") | Some("output_text") | Some("text") => {
                                blocks.push(json!({
                                    "type": "text",
                                    "text": part.get("text").cloned().unwrap_or_else(|| json!(""))
                                }))
                            }
                            Some("input_image") => {
                                if let Some(url) = responses_image_url(part) {
                                    blocks.push(anthropic_image(&url));
                                }
                            }
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
            if !blocks.is_empty() {
                push_message(messages, role, blocks);
            }
        }
        _ => {}
    }
    Ok(())
}

pub(super) fn responses_tool_output_to_anthropic(output: Option<&Value>) -> Value {
    let Some(output) = output else {
        return Value::Null;
    };
    let Some(parts) = output.as_array() else {
        return output.clone();
    };
    let converted = parts
        .iter()
        .filter_map(|part| match part.get("type").and_then(Value::as_str) {
            Some("input_text" | "output_text" | "text") => Some(json!({
                "type": "text",
                "text": part.get("text").cloned().unwrap_or_else(|| json!(""))
            })),
            Some("input_image") => responses_image_url(part).map(|url| anthropic_image(&url)),
            _ => None,
        })
        .collect::<Vec<_>>();
    if converted.is_empty() {
        json!(json_string(Some(output)))
    } else {
        Value::Array(converted)
    }
}

pub(super) fn responses_image_url(part: &Value) -> Option<String> {
    let image = part.get("image_url")?;
    image
        .as_str()
        .map(str::to_string)
        .or_else(|| image.get("url").and_then(Value::as_str).map(str::to_string))
}

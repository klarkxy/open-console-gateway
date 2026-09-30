//! Client-feature and cross-format preservation checks.
//!
//! These reject requests before any document is rewritten. Error strings are
//! part of the host's 400 protocol-error surface and must stay exact.

use super::shared::array;
use super::{ApiFormat, ConversionError};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::Value;

pub(super) fn validate_client_request_features(
    client: ApiFormat,
    body: &Value,
) -> Result<(), ConversionError> {
    if client == ApiFormat::Responses {
        for field in ["previous_response_id", "conversation"] {
            if body.get(field).is_some_and(|value| !value.is_null()) {
                return Err(ConversionError::new(format!(
                    "Responses {field} is not supported by this stateless gateway"
                )));
            }
        }
        if body.get("store") != Some(&Value::Bool(false)) {
            return Err(ConversionError::new(
                "this stateless gateway requires Responses store=false",
            ));
        }
        match body.get("background") {
            None | Some(Value::Null | Value::Bool(false)) => {}
            Some(Value::Bool(true)) => {
                return Err(ConversionError::new(
                    "Responses background=true is not supported by this stateless gateway",
                ));
            }
            Some(_) => {
                return Err(ConversionError::new(
                    "Responses background must be a boolean",
                ));
            }
        }
        if contains_input_image_file_id(body.get("input").unwrap_or(&Value::Null)) {
            return Err(ConversionError::new(
                "Responses input_image.file_id is not supported; use image_url",
            ));
        }
    }

    if client == ApiFormat::Gemini {
        validate_gemini_request(body)?;
    }

    Ok(())
}

pub(super) fn validate_request_features(
    client: ApiFormat,
    upstream: ApiFormat,
    body: &Value,
) -> Result<(), ConversionError> {
    validate_client_request_features(client, body)?;

    if client == upstream {
        return Ok(());
    }
    let unsupported_format = match client {
        ApiFormat::Responses => unsupported_output_format(body.pointer("/text/format")),
        ApiFormat::ChatCompletions => unsupported_output_format(body.get("response_format")),
        ApiFormat::Messages => unsupported_output_format(body.pointer("/output_config/format")),
        ApiFormat::Gemini => false,
    };
    if unsupported_format {
        let field = match client {
            ApiFormat::Responses => "Responses text.format",
            ApiFormat::ChatCompletions => "Chat Completions response_format",
            ApiFormat::Messages => "Messages output_config.format",
            ApiFormat::Gemini => "Gemini generationConfig.responseJsonSchema",
        };
        return Err(ConversionError::new(format!(
            "{field} cannot be preserved by protocol conversion"
        )));
    }
    if client == ApiFormat::Responses && array(body, "tools").iter().any(has_custom_tool_format) {
        return Err(ConversionError::new(
            "Responses custom tool grammar format cannot be preserved by protocol conversion",
        ));
    }
    if let Some(field) = unpreserved_request_field(client, upstream, body) {
        return Err(ConversionError::new(format!(
            "{field} cannot be preserved by protocol conversion"
        )));
    }
    Ok(())
}

pub(super) fn unpreserved_request_field(
    client: ApiFormat,
    upstream: ApiFormat,
    body: &Value,
) -> Option<&'static str> {
    if client == ApiFormat::ChatCompletions {
        match body.get("n") {
            None | Some(Value::Null) => {}
            Some(Value::Number(n)) if n.as_u64() == Some(1) => {}
            Some(_) => return Some("Chat Completions n"),
        }
        if body.get("logprobs") == Some(&Value::Bool(true))
            || body
                .get("top_logprobs")
                .is_some_and(|value| !value.is_null())
        {
            return Some("Chat Completions logprobs");
        }
    }
    if upstream == ApiFormat::Responses && nonempty_stop(body) {
        return Some(match client {
            ApiFormat::ChatCompletions => "Chat Completions stop",
            ApiFormat::Messages => "Messages stop_sequences",
            _ => "stop",
        });
    }
    None
}

pub(super) fn nonempty_stop(body: &Value) -> bool {
    match body.get("stop").or_else(|| body.get("stop_sequences")) {
        Some(Value::String(stop)) => !stop.is_empty(),
        Some(Value::Array(stops)) => !stops.is_empty(),
        _ => false,
    }
}

pub(super) fn validate_gemini_request(body: &Value) -> Result<(), ConversionError> {
    match body.get("safetySettings") {
        None | Some(Value::Null) => {}
        Some(Value::Array(settings)) if settings.is_empty() => {}
        Some(Value::Array(_)) => {
            return Err(ConversionError::new(
                "Gemini safetySettings cannot be preserved by protocol conversion",
            ));
        }
        Some(_) => {
            return Err(ConversionError::new(
                "Gemini safetySettings must be an array",
            ));
        }
    }
    if body
        .get("cachedContent")
        .is_some_and(|value| !value.is_null())
    {
        return Err(ConversionError::new(
            "Gemini cachedContent is not supported by this stateless gateway",
        ));
    }
    if body.get("tools").is_some_and(|tools| !tools.is_array()) {
        return Err(ConversionError::new("Gemini tools must be an array"));
    }
    if body
        .get("generationConfig")
        .is_some_and(|config| !config.is_object())
    {
        return Err(ConversionError::new(
            "Gemini generationConfig must be an object",
        ));
    }
    let contents = body
        .get("contents")
        .and_then(Value::as_array)
        .ok_or_else(|| ConversionError::new("Gemini contents must be an array"))?;
    if contents.is_empty() {
        return Err(ConversionError::new("Gemini contents cannot be empty"));
    }
    for content in contents {
        let role = content
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or("user");
        if !matches!(role, "user" | "model") {
            return Err(ConversionError::new(format!(
                "Gemini content role `{role}` is not supported"
            )));
        }
        let parts = content
            .get("parts")
            .and_then(Value::as_array)
            .ok_or_else(|| ConversionError::new("Gemini content parts must be an array"))?;
        for part in parts {
            if part.get("fileData").is_some() || part.get("file_data").is_some() {
                return Err(ConversionError::new(
                    "Gemini fileData is not supported; use inlineData for images",
                ));
            }
            if let Some(data) = part.get("inlineData").or_else(|| part.get("inline_data")) {
                let media_type = data
                    .get("mimeType")
                    .or_else(|| data.get("mime_type"))
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if !matches!(
                    media_type,
                    "image/png" | "image/jpeg" | "image/gif" | "image/webp"
                ) {
                    return Err(ConversionError::new(
                        "Gemini inlineData supports PNG, JPEG, GIF, and WebP images only",
                    ));
                }
                let encoded = data.get("data").and_then(Value::as_str).unwrap_or_default();
                if encoded.is_empty() {
                    return Err(ConversionError::new("Gemini inlineData.data is required"));
                }
                if STANDARD.decode(encoded).is_err() {
                    return Err(ConversionError::new(
                        "Gemini inlineData.data must be valid base64",
                    ));
                }
            }
            if let Some(call) = part
                .get("functionCall")
                .or_else(|| part.get("function_call"))
            {
                if call
                    .get("name")
                    .and_then(Value::as_str)
                    .is_none_or(str::is_empty)
                {
                    return Err(ConversionError::new("Gemini functionCall.name is required"));
                }
                if call.get("args").is_some_and(|args| !args.is_object()) {
                    return Err(ConversionError::new(
                        "Gemini functionCall.args must be an object",
                    ));
                }
            }
            if let Some(response) = part
                .get("functionResponse")
                .or_else(|| part.get("function_response"))
            {
                if response
                    .get("name")
                    .and_then(Value::as_str)
                    .is_none_or(str::is_empty)
                {
                    return Err(ConversionError::new(
                        "Gemini functionResponse.name is required",
                    ));
                }
                if response.get("parts").is_some() {
                    return Err(ConversionError::new(
                        "Gemini multimodal functionResponse.parts is not supported",
                    ));
                }
            }
            if !part.is_object()
                || !part.as_object().is_some_and(|object| {
                    object.contains_key("text")
                        || object.contains_key("inlineData")
                        || object.contains_key("inline_data")
                        || object.contains_key("functionCall")
                        || object.contains_key("function_call")
                        || object.contains_key("functionResponse")
                        || object.contains_key("function_response")
                })
            {
                return Err(ConversionError::new(
                    "Gemini content part type is not supported",
                ));
            }
        }
    }

    if let Some(system) = body.get("systemInstruction") {
        let valid = system
            .get("parts")
            .and_then(Value::as_array)
            .is_some_and(|parts| {
                !parts.is_empty()
                    && parts
                        .iter()
                        .all(|part| part.get("text").and_then(Value::as_str).is_some())
            });
        if !valid {
            return Err(ConversionError::new(
                "Gemini systemInstruction currently supports text parts only",
            ));
        }
    }

    for tool in array(body, "tools") {
        let Some(object) = tool.as_object() else {
            return Err(ConversionError::new("Gemini tools must be objects"));
        };
        if object.contains_key("googleSearch")
            || object.contains_key("google_search")
            || object.contains_key("googleSearchRetrieval")
        {
            return Err(ConversionError::new(
                "Gemini Google Search tools are not supported by this gateway",
            ));
        }
        if object.contains_key("urlContext") || object.contains_key("url_context") {
            return Err(ConversionError::new(
                "Gemini urlContext is not supported by this gateway",
            ));
        }
        if object.len() != 1 || !object.contains_key("functionDeclarations") {
            return Err(ConversionError::new(
                "only Gemini functionDeclarations tools are supported",
            ));
        }
        let declarations = tool
            .get("functionDeclarations")
            .and_then(Value::as_array)
            .filter(|declarations| !declarations.is_empty())
            .ok_or_else(|| {
                ConversionError::new("Gemini functionDeclarations must be a non-empty array")
            })?;
        for declaration in declarations {
            if declaration.get("parameters").is_some()
                && declaration.get("parametersJsonSchema").is_some()
            {
                return Err(ConversionError::new(
                    "Gemini function declarations cannot contain both parameters and parametersJsonSchema",
                ));
            }
            if declaration.get("response").is_some()
                || declaration.get("responseJsonSchema").is_some()
                || declaration.get("behavior").is_some()
            {
                return Err(ConversionError::new(
                    "Gemini function response schemas and behavior are not supported",
                ));
            }
        }
    }

    if let Some(config) = body.pointer("/toolConfig/functionCallingConfig") {
        let mode = config
            .get("mode")
            .and_then(Value::as_str)
            .unwrap_or("AUTO")
            .to_ascii_uppercase();
        if mode == "VALIDATED" {
            return Err(ConversionError::new(
                "Gemini VALIDATED function calling has no safe cross-protocol equivalent",
            ));
        }
        if mode == "ANY"
            && array(body, "tools")
                .iter()
                .flat_map(|tool| array(tool, "functionDeclarations"))
                .next()
                .is_none()
        {
            return Err(ConversionError::new(
                "Gemini function calling mode ANY requires a declared function",
            ));
        }
        if config
            .get("allowedFunctionNames")
            .and_then(Value::as_array)
            .is_some_and(|names| !names.is_empty())
            && mode != "ANY"
        {
            return Err(ConversionError::new(
                "Gemini allowedFunctionNames is supported only with mode ANY",
            ));
        }
    }

    let generation = body.get("generationConfig").unwrap_or(&Value::Null);
    if let Some(config) = generation.as_object() {
        const SUPPORTED_FIELDS: &[&str] = &[
            "candidateCount",
            "maxOutputTokens",
            "responseJsonSchema",
            "responseMimeType",
            "responseModalities",
            "responseSchema",
            "stopSequences",
            "temperature",
            "thinkingConfig",
            "topK",
            "topP",
        ];
        if let Some((field, _)) = config
            .iter()
            .find(|(field, value)| !value.is_null() && !SUPPORTED_FIELDS.contains(&field.as_str()))
        {
            return Err(ConversionError::new(format!(
                "Gemini generationConfig.{field} cannot be preserved by protocol conversion"
            )));
        }
        if config
            .get("topK")
            .is_some_and(|value| !value.is_null() && !value.is_number())
        {
            return Err(ConversionError::new(
                "Gemini generationConfig.topK must be a number",
            ));
        }
        if config
            .get("thinkingConfig")
            .is_some_and(|value| !value.is_null() && !value.is_object())
        {
            return Err(ConversionError::new(
                "Gemini generationConfig.thinkingConfig must be an object",
            ));
        }
        if config
            .get("responseMimeType")
            .is_some_and(|value| !value.is_null() && !value.is_string())
        {
            return Err(ConversionError::new(
                "Gemini generationConfig.responseMimeType must be a string",
            ));
        }
    }
    match generation.get("candidateCount") {
        None | Some(Value::Null) => {}
        Some(value) if value.as_u64() == Some(1) => {}
        Some(value) if value.as_u64().is_some() => {
            return Err(ConversionError::new(
                "Gemini candidateCount other than 1 is not supported",
            ));
        }
        Some(_) => {
            return Err(ConversionError::new(
                "Gemini generationConfig.candidateCount must be an unsigned integer",
            ));
        }
    }
    match generation.get("responseModalities") {
        None | Some(Value::Null) => {}
        Some(Value::Array(modalities))
            if modalities
                .iter()
                .any(|value| value.as_str() != Some("TEXT")) =>
        {
            return Err(ConversionError::new(
                "Gemini response modalities other than TEXT are not supported",
            ));
        }
        Some(Value::Array(_)) => {}
        Some(_) => {
            return Err(ConversionError::new(
                "Gemini generationConfig.responseModalities must be an array",
            ));
        }
    }
    let _ = gemini_output_schema(body)?;
    Ok(())
}

pub(super) fn gemini_output_schema(body: &Value) -> Result<Option<Value>, ConversionError> {
    let generation = body.get("generationConfig").unwrap_or(&Value::Null);
    let mime = generation.get("responseMimeType").and_then(Value::as_str);
    let schema = generation
        .get("responseJsonSchema")
        .or_else(|| generation.get("responseSchema"))
        .filter(|value| !value.is_null())
        .cloned();
    match (mime, schema) {
        (None | Some("text/plain"), None) => Ok(None),
        (None | Some("application/json"), Some(schema)) => Ok(Some(schema)),
        (Some("application/json"), None) => Err(ConversionError::new(
            "Gemini application/json output requires responseJsonSchema",
        )),
        (Some(other), _) => Err(ConversionError::new(format!(
            "Gemini responseMimeType `{other}` is not supported"
        ))),
    }
}

pub(super) fn unsupported_output_format(format: Option<&Value>) -> bool {
    match format {
        None | Some(Value::Null) => false,
        Some(format) => format.get("type").and_then(Value::as_str) != Some("text"),
    }
}

pub(super) fn has_custom_tool_format(tool: &Value) -> bool {
    match tool.get("type").and_then(Value::as_str) {
        Some("custom") => unsupported_output_format(tool.get("format")),
        Some("namespace") => array(tool, "tools").iter().any(has_custom_tool_format),
        _ => false,
    }
}

pub(super) fn contains_input_image_file_id(value: &Value) -> bool {
    match value {
        Value::Array(values) => values.iter().any(contains_input_image_file_id),
        Value::Object(object) => {
            (object.get("type").and_then(Value::as_str) == Some("input_image")
                && object.get("file_id").is_some_and(|value| !value.is_null()))
                || object.values().any(contains_input_image_file_id)
        }
        _ => false,
    }
}

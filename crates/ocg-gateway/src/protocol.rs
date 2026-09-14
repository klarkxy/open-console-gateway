//! Whole-document JSON protocol conversion.
//!
//! This module converts already-parsed JSON request and response documents
//! between client and upstream formats. It is not `stream == false`: SSE
//! framing, stream state, retry, fallback, redaction, and logging stay in the
//! host crate. UUID/clock generation, byte serialization, visible-model rewrite,
//! usage extraction, and route identities also stay in the host.
//!
//! Callers supply [`ApiFormat`] values; this module never looks up model
//! catalogs or trials a billable inference path.
//!
//! Items are rust-public only as the cross-crate bridge; the host crate keeps
//! parse, usage, stream, and route-identity facades.

use base64::{
    Engine as _,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use ocg_domain::ids::normalize_model_name;
use ocg_domain::protocol::ApiFormat;
use serde_json::{Map, Value, json};
use std::fmt;

/// Conversion failure. The host maps [`Self::message`] to a 400 protocol error.
///
/// Public only as the cross-crate bridge.
#[derive(Debug, Clone, PartialEq, Eq)]
#[doc(hidden)]
pub struct ConversionError {
    pub message: String,
}

impl ConversionError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for ConversionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ConversionError {}

/// Flattened Responses namespace tool identity used on both conversion directions.
///
/// Public only as the cross-crate bridge.
#[derive(Debug, Clone, PartialEq, Eq)]
#[doc(hidden)]
pub struct NamespaceToolMapping {
    pub flattened: String,
    pub namespace: String,
    pub name: String,
    pub custom: bool,
}

/// Versioned legacy tool-compat profile. Optional hosted-tool declarations may
/// still be dropped during conversion; the drop is recorded and never rewrites
/// stored protocol configuration.
pub const LEGACY_TOOL_COMPAT_PROFILE: &str = "legacy_compat";
pub const LEGACY_TOOL_COMPAT_VERSION: u32 = 1;

/// Recorded downgrade when conversion drops optional hosted tools.
///
/// Public only as the cross-crate bridge.
#[derive(Debug, Clone, PartialEq, Eq)]
#[doc(hidden)]
pub struct LegacyToolCompat {
    pub profile: &'static str,
    pub version: u32,
    pub dropped_hosted_tools: Vec<String>,
}

/// Whole-document request conversion result.
///
/// Public only as the cross-crate bridge.
#[derive(Debug, Clone, PartialEq)]
#[doc(hidden)]
pub struct ConvertedRequestJson {
    pub body: Value,
    pub custom_tools: Vec<String>,
    pub namespace_tools: Vec<NamespaceToolMapping>,
    pub legacy_tool_compat: Option<LegacyToolCompat>,
}

/// Host-injected synthesis metadata. The converter never reads the clock or
/// generates UUIDs.
///
/// Public only as the cross-crate bridge.
#[derive(Debug, Clone, PartialEq, Eq)]
#[doc(hidden)]
pub struct ResponseSynthesis {
    pub created_at: u64,
    pub empty_response_id: String,
}

/// Whole-document response conversion result.
///
/// Public only as the cross-crate bridge.
#[derive(Debug, Clone, PartialEq)]
#[doc(hidden)]
pub struct ResponseConversion {
    pub body: Value,
}

/// Convert a parsed JSON request document from `client` to `upstream`.
///
/// Public only as the cross-crate bridge.
#[doc(hidden)]
pub fn convert_request_json(
    client: ApiFormat,
    upstream: ApiFormat,
    body: Value,
) -> Result<ConvertedRequestJson, ConversionError> {
    validate_request_features(client, upstream, &body)?;
    let converting = client != upstream;
    let (tool_context, legacy_tool_compat) = if client == ApiFormat::Responses {
        responses_tool_context(&body, converting)?
    } else {
        (ResponsesToolContext::default(), None)
    };
    let body = convert_request(client, upstream, body, &tool_context.namespace_tools)?;
    Ok(ConvertedRequestJson {
        body,
        custom_tools: tool_context.custom_tools,
        namespace_tools: tool_context.namespace_tools,
        legacy_tool_compat,
    })
}

/// Convert a whole JSON response document from `upstream` to `client`.
///
/// Public only as the cross-crate bridge.
#[doc(hidden)]
pub fn convert_response_json(
    upstream: ApiFormat,
    client: ApiFormat,
    body: &Value,
    custom_tools: &[String],
    namespace_tools: &[NamespaceToolMapping],
    synthesis: ResponseSynthesis,
    model_hint: Option<&str>,
) -> Result<ResponseConversion, ConversionError> {
    let mut body = body.clone();
    if upstream == ApiFormat::Messages {
        let response_model = body.get("model").and_then(Value::as_str).map(str::to_owned);
        let object = body
            .as_object_mut()
            .ok_or_else(|| ConversionError::new("Messages response must be a JSON object"))?;
        if let Some(usage) = object.get_mut("usage") {
            sanitize_minimax_anthropic_usage(response_model.as_deref(), model_hint, usage);
        }
        if let Some(model) = model_hint {
            object.insert("model".to_string(), json!(model));
        }
    }
    let mut transformed = convert_between(
        upstream,
        client,
        &body,
        custom_tools,
        namespace_tools,
        model_hint,
        &synthesis,
    )?;
    if client == ApiFormat::ChatCompletions && upstream == ApiFormat::ChatCompletions {
        let model = transformed
            .get("model")
            .and_then(Value::as_str)
            .map(str::to_owned);
        if let Some(usage) = transformed.get_mut("usage") {
            sanitize_minimax_chat_usage(model.as_deref(), model_hint, usage);
        }
    }
    Ok(ResponseConversion { body: transformed })
}

fn convert_between(
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

fn synthesized_response_id(id: &str, empty_response_id: &str) -> String {
    if id.starts_with("resp_") {
        id.to_string()
    } else if id.is_empty() {
        empty_response_id.to_string()
    } else {
        format!("resp_{id}")
    }
}

const ANTHROPIC_THINKING_ENCRYPTED_PREFIX: &str = "ocg-anthropic-thinking-v1:";
const CHAT_REASONING_ENCRYPTED_PREFIX: &str = "ocg-chat-reasoning-v1:";
const CHAT_TOOL_REASONING_PLACEHOLDER: &str = "Tool call reasoning unavailable.";

fn validate_request_features(
    client: ApiFormat,
    upstream: ApiFormat,
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
    Ok(())
}

fn validate_gemini_request(body: &Value) -> Result<(), ConversionError> {
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

fn gemini_output_schema(body: &Value) -> Result<Option<Value>, ConversionError> {
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

fn unsupported_output_format(format: Option<&Value>) -> bool {
    match format {
        None | Some(Value::Null) => false,
        Some(format) => format.get("type").and_then(Value::as_str) != Some("text"),
    }
}

fn has_custom_tool_format(tool: &Value) -> bool {
    match tool.get("type").and_then(Value::as_str) {
        Some("custom") => unsupported_output_format(tool.get("format")),
        Some("namespace") => array(tool, "tools").iter().any(has_custom_tool_format),
        _ => false,
    }
}

fn contains_input_image_file_id(value: &Value) -> bool {
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

/// Work around MiniMax's Anthropic-compatible endpoint returning the entire prompt as
/// `cache_read_input_tokens` with `input_tokens: 0` on the first turn. When that happens,
/// move the tokens back to `input_tokens` so the gateway doesn't report a 100% cache hit.
///
/// `model` is the model name reported by the upstream response; `model_hint` is the model
/// from the original request plan. OpenCode Go sometimes returns a generic or internal model
/// identifier in the response, so we trust the hint when either name identifies MiniMax.
fn is_minimax_family(model: Option<&str>) -> bool {
    model
        .map(|m| normalize_model_name(m).starts_with("minimax"))
        .unwrap_or(false)
}

pub fn sanitize_minimax_anthropic_usage(
    model: Option<&str>,
    model_hint: Option<&str>,
    usage: &mut Value,
) {
    let is_minimax = is_minimax_family(model) || is_minimax_family(model_hint);
    if !is_minimax {
        return;
    }
    let Some(obj) = usage.as_object_mut() else {
        return;
    };
    let input = obj.get("input_tokens").and_then(Value::as_u64).unwrap_or(0);
    let cached = obj
        .get("cache_read_input_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let creation = obj
        .get("cache_creation_input_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let total = input.saturating_add(cached).saturating_add(creation);
    // Bogus all-cache signature: every input token is reported as a cache read and
    // no new/cache-creation tokens exist. This is impossible on a first turn.
    if total > 0 && input == 0 && creation == 0 && cached == total {
        obj.insert("input_tokens".into(), json!(total));
        obj.insert("cache_read_input_tokens".into(), json!(0));
    }
}

/// Same normalization as `sanitize_minimax_anthropic_usage`, but for the OpenAI Chat
/// Completions wire format where cache hits live in `prompt_tokens_details.cached_tokens`.
/// If the entire prompt is reported as a cache hit (and no cache-creation tokens are
/// present), zero out the cached count so it is billed as regular input.
pub fn sanitize_minimax_chat_usage(
    model: Option<&str>,
    model_hint: Option<&str>,
    usage: &mut Value,
) {
    let is_minimax = is_minimax_family(model) || is_minimax_family(model_hint);
    if !is_minimax {
        return;
    }
    let Some(obj) = usage.as_object_mut() else {
        return;
    };
    let prompt = obj
        .get("prompt_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let cached = obj
        .get("prompt_tokens_details")
        .and_then(|v| v.get("cached_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    if prompt > 0
        && cached == prompt
        && let Some(details) = obj
            .get_mut("prompt_tokens_details")
            .and_then(Value::as_object_mut)
    {
        details.insert("cached_tokens".into(), json!(0));
    }
}

fn convert_request(
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

#[derive(Debug, Default)]
struct ResponsesToolContext {
    custom_tools: Vec<String>,
    namespace_tools: Vec<NamespaceToolMapping>,
}

fn responses_tool_context(
    body: &Value,
    converting: bool,
) -> Result<(ResponsesToolContext, Option<LegacyToolCompat>), ConversionError> {
    let tools = body
        .get("tools")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let mut context = ResponsesToolContext::default();
    let mut used_names = tools
        .iter()
        .filter(|tool| {
            matches!(
                tool.get("type").and_then(Value::as_str),
                Some("function" | "custom")
            )
        })
        .filter_map(|tool| tool.get("name").and_then(Value::as_str).map(str::to_string))
        .collect::<Vec<_>>();
    let mut hosted_tools = Vec::new();

    for tool in tools {
        match tool.get("type").and_then(Value::as_str) {
            Some("function") => {}
            Some("custom") => {
                let name = required_tool_name(tool, "Responses custom tool")?;
                context.custom_tools.push(name.to_string());
            }
            Some("namespace") => {
                let namespace = required_tool_name(tool, "Responses namespace")?;
                let nested = tool.get("tools").and_then(Value::as_array).ok_or_else(|| {
                    ConversionError::new("Responses namespace tools are required")
                })?;
                for nested_tool in nested {
                    let kind = nested_tool.get("type").and_then(Value::as_str);
                    if !matches!(kind, Some("function" | "custom")) {
                        return Err(ConversionError::new(
                            "Responses namespace entries must be function or custom tools",
                        ));
                    }
                    let name = required_tool_name(nested_tool, "Responses namespace tool")?;
                    let flattened = unique_namespace_tool_name(namespace, name, &used_names);
                    used_names.push(flattened.clone());
                    if kind == Some("custom") {
                        context.custom_tools.push(flattened.clone());
                    }
                    context.namespace_tools.push(NamespaceToolMapping {
                        flattened,
                        namespace: namespace.to_string(),
                        name: name.to_string(),
                        custom: kind == Some("custom"),
                    });
                }
            }
            Some(kind) if is_hosted_tool(kind) => {
                if !hosted_tools.iter().any(|existing| existing == kind) {
                    hosted_tools.push(kind.to_string());
                }
            }
            Some(kind) => {
                return Err(ConversionError::new(format!(
                    "Responses tool type `{kind}` is not supported by protocol conversion"
                )));
            }
            None => return Err(ConversionError::new("Responses tool type is required")),
        }
    }

    if !converting {
        if requires_tool(body.get("tool_choice"))
            && used_names.is_empty()
            && hosted_tools.is_empty()
        {
            return Err(ConversionError::new(
                "Responses tool_choice `required` has no convertible function, custom, or namespace tool",
            ));
        }
        return Ok((context, None));
    }

    if let Some(kind) = forced_hosted_tool(body.get("tool_choice")) {
        return Err(ConversionError::new(format!(
            "Responses hosted tool `{kind}` cannot be forced through protocol conversion"
        )));
    }
    if !hosted_tools.is_empty() && used_names.is_empty() {
        return Err(ConversionError::new(format!(
            "Responses hosted tool `{}` cannot be preserved by protocol conversion; refusing to strip it and continue",
            hosted_tools[0]
        )));
    }
    if requires_tool(body.get("tool_choice")) && used_names.is_empty() {
        return Err(ConversionError::new(
            "Responses tool_choice `required` has no convertible function, custom, or namespace tool",
        ));
    }
    let legacy_tool_compat = (!hosted_tools.is_empty()).then_some(LegacyToolCompat {
        profile: LEGACY_TOOL_COMPAT_PROFILE,
        version: LEGACY_TOOL_COMPAT_VERSION,
        dropped_hosted_tools: hosted_tools,
    });
    Ok((context, legacy_tool_compat))
}

fn required_tool_name<'a>(tool: &'a Value, label: &str) -> Result<&'a str, ConversionError> {
    tool.get("name")
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())
        .ok_or_else(|| ConversionError::new(format!("{label} name is required")))
}

fn is_hosted_tool(kind: &str) -> bool {
    matches!(kind, "web_search" | "web_search_preview" | "tool_search")
}

fn forced_hosted_tool(choice: Option<&Value>) -> Option<&str> {
    let choice = choice?;
    choice
        .as_str()
        .filter(|kind| is_hosted_tool(kind))
        .or_else(|| {
            choice
                .get("type")
                .and_then(Value::as_str)
                .filter(|kind| is_hosted_tool(kind))
        })
}

fn requires_tool(choice: Option<&Value>) -> bool {
    choice.is_some_and(|choice| {
        choice.as_str() == Some("required")
            || choice.get("type").and_then(Value::as_str) == Some("required")
    })
}

fn unique_namespace_tool_name(namespace: &str, name: &str, used: &[String]) -> String {
    let raw = format!("{namespace}__{name}");
    let mut flattened = raw
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '_' | '-') {
                character
            } else {
                '_'
            }
        })
        .collect::<String>();
    if flattened.len() <= 64 && !used.iter().any(|used| used == &flattened) {
        return flattened;
    }

    let mut hash = 0xcbf29ce484222325_u64;
    for byte in raw.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    let suffix = format!("__{hash:016x}");
    flattened.truncate(64 - suffix.len());
    flattened.push_str(&suffix);
    flattened
}

fn normalize_messages_system_roles(mut body: Value) -> Result<Value, ConversionError> {
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

fn sanitize_messages_history(message: &Value) -> Option<Value> {
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

fn append_system_blocks(target: &mut Vec<Value>, content: Option<&Value>) {
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

fn gemini_request_to_messages(body: Value) -> Result<Value, ConversionError> {
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

fn messages_request_to_chat(body: Value) -> Result<Value, ConversionError> {
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

fn backfill_chat_tool_reasoning(body: &mut Value) {
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

fn chat_request_to_messages(body: Value) -> Result<Value, ConversionError> {
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

fn messages_request_to_responses(body: Value) -> Result<Value, ConversionError> {
    let mut out = Map::new();
    copy(&body, &mut out, "model", "model");
    copy(&body, &mut out, "stream", "stream");
    copy(&body, &mut out, "temperature", "temperature");
    copy(&body, &mut out, "top_p", "top_p");
    copy(&body, &mut out, "max_tokens", "max_output_tokens");
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
                Some("tool_use") => input.push(json!({
                    "type": "function_call",
                    "call_id": block.get("id").cloned().unwrap_or_else(|| json!("")),
                    "name": block.get("name").cloned().unwrap_or_else(|| json!("")),
                    "arguments": json_string(block.get("input"))
                })),
                Some("tool_result") => input.push(json!({
                    "type": "function_call_output",
                    "call_id": block.get("tool_use_id").cloned().unwrap_or_else(|| json!("")),
                    "output": tool_result_text(block.get("content"))
                })),
                Some("thinking") => input.push(json!({
                    "type": "reasoning",
                    "summary": [{ "type": "summary_text", "text": block.get("thinking").cloned().unwrap_or_else(|| json!("")) }]
                })),
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

fn responses_request_to_messages(
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

fn responses_tool_to_anthropic(
    tool: &Value,
    namespace: Option<&str>,
    mappings: &[NamespaceToolMapping],
) -> Option<Value> {
    let original = tool.get("name")?.as_str()?;
    let name = namespace
        .and_then(|namespace| {
            mappings
                .iter()
                .find(|mapping| mapping.namespace == namespace && mapping.name == original)
                .map(|mapping| mapping.flattened.as_str())
        })
        .unwrap_or(original);
    match tool.get("type").and_then(Value::as_str) {
        Some("function") => Some(json!({
            "name": name,
            "description": tool.get("description").cloned().unwrap_or(Value::Null),
            "input_schema": tool.get("parameters").cloned().unwrap_or_else(empty_schema)
        })),
        Some("custom") => Some(json!({
            "name": name,
            "description": tool.get("description").cloned().unwrap_or(Value::Null),
            "input_schema": {
                "type": "object",
                "properties": { "input": { "type": "string" } },
                "required": ["input"],
                "additionalProperties": false
            }
        })),
        _ => None,
    }
}

fn chat_response_to_messages(body: &Value) -> Result<Value, ConversionError> {
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

fn messages_response_to_gemini(body: &Value) -> Result<Value, ConversionError> {
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

fn messages_response_to_chat(
    body: &Value,
    model_hint: Option<&str>,
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
    let mut usage = body.get("usage").cloned().unwrap_or(Value::Null);
    sanitize_minimax_anthropic_usage(
        body.get("model").and_then(Value::as_str),
        model_hint,
        &mut usage,
    );
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

fn responses_response_to_messages(body: &Value) -> Result<Value, ConversionError> {
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

fn messages_response_to_responses(
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

fn flush_responses_text(output: &mut Vec<Value>, parts: &mut Vec<Value>, response_id: &str) {
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

fn message_to_chat(message: &Value) -> Result<Vec<Value>, ConversionError> {
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

fn responses_item_to_messages(
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

fn responses_history_tool_name(item: &Value, mappings: &[NamespaceToolMapping]) -> String {
    let name = item.get("name").and_then(Value::as_str).unwrap_or_default();
    let Some(namespace) = item
        .get("namespace")
        .and_then(Value::as_str)
        .filter(|namespace| !namespace.is_empty())
    else {
        return name.to_string();
    };
    mappings
        .iter()
        .find(|mapping| mapping.namespace == namespace && mapping.name == name)
        .map(|mapping| mapping.flattened.clone())
        .unwrap_or_else(|| unique_namespace_tool_name(namespace, name, &[]))
}

fn responses_tool_output_to_anthropic(output: Option<&Value>) -> Value {
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

fn responses_image_url(part: &Value) -> Option<String> {
    let image = part.get("image_url")?;
    image
        .as_str()
        .map(str::to_string)
        .or_else(|| image.get("url").and_then(Value::as_str).map(str::to_string))
}

fn copy(source: &Value, target: &mut Map<String, Value>, from: &str, to: &str) {
    if let Some(value) = source.get(from) {
        target.insert(to.to_string(), value.clone());
    }
}

fn array<'a>(value: &'a Value, key: &str) -> &'a [Value] {
    value
        .get(key)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

fn uint(value: &Value, key: &str) -> u64 {
    value.get(key).and_then(Value::as_u64).unwrap_or(0)
}

fn empty_object() -> Value {
    json!({})
}

fn empty_schema() -> Value {
    json!({ "type": "object", "properties": {} })
}

fn system_text(system: Option<&Value>) -> Option<String> {
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

fn anthropic_blocks(content: Option<&Value>) -> Vec<Value> {
    match content {
        Some(Value::String(text)) => vec![json!({ "type": "text", "text": text })],
        Some(Value::Array(blocks)) => blocks.clone(),
        _ => Vec::new(),
    }
}

fn anthropic_image_url(block: &Value) -> Option<String> {
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

fn anthropic_image(url: &str) -> Value {
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

fn chat_content_to_anthropic(content: Option<&Value>) -> Vec<Value> {
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

fn chat_content_text(content: Option<&Value>) -> Option<String> {
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

fn parse_json(value: Option<&Value>) -> Option<Value> {
    match value {
        Some(Value::String(text)) => serde_json::from_str(text).ok(),
        Some(value) => Some(value.clone()),
        None => None,
    }
}

fn json_string(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(text)) => text.clone(),
        Some(value) => serde_json::to_string(value).unwrap_or_else(|_| "{}".to_string()),
        None => "{}".to_string(),
    }
}

fn tool_result_text(value: Option<&Value>) -> String {
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

fn push_message(messages: &mut Vec<Value>, role: &str, blocks: Vec<Value>) {
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

fn drop_empty_messages(messages: &mut Vec<Value>) {
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

fn ensure_leading_user_message(messages: &mut Vec<Value>) {
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

fn anthropic_tool_choice_to_chat(choice: &Value) -> Value {
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

fn chat_tool_choice_to_anthropic(choice: &Value) -> Value {
    if let Some(value) = choice.as_str() {
        return json!({ "type": if value == "required" { "any" } else { value } });
    }
    if let Some(name) = choice.pointer("/function/name") {
        return json!({ "type": "tool", "name": name });
    }
    json!({ "type": "auto" })
}

fn anthropic_tool_choice_to_responses(choice: &Value) -> Value {
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

fn responses_tool_choice_to_anthropic(
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

fn finish_anthropic_request_options(
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

fn anthropic_effort(body: &Value) -> Option<&'static str> {
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

fn thinking_from_effort(effort: &str, request: &Map<String, Value>) -> Value {
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

fn inject_chat_usage(out: &mut Map<String, Value>) {
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

fn reasoning_text(item: &Value) -> String {
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

fn chat_stop_to_anthropic(reason: Option<&Value>) -> Value {
    json!(match reason.and_then(Value::as_str) {
        Some("tool_calls") | Some("function_call") => "tool_use",
        Some("length") => "max_tokens",
        Some("content_filter") => "refusal",
        Some("stop") | None => "end_turn",
        Some(other) => other,
    })
}

fn anthropic_stop_to_chat(reason: Option<&Value>) -> Value {
    json!(match reason.and_then(Value::as_str) {
        Some("tool_use") => "tool_calls",
        Some("max_tokens") => "length",
        Some("refusal") => "content_filter",
        _ => "stop",
    })
}

fn chat_usage_to_anthropic(usage: Option<&Value>) -> Value {
    let usage = usage.unwrap_or(&Value::Null);
    let cached = usage
        .pointer("/prompt_tokens_details/cached_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    json!({
        "input_tokens": uint(usage, "prompt_tokens").saturating_sub(cached),
        "output_tokens": uint(usage, "completion_tokens"),
        "cache_read_input_tokens": cached,
        "cache_creation_input_tokens": 0
    })
}

fn responses_usage_to_anthropic(usage: Option<&Value>) -> Value {
    let usage = usage.unwrap_or(&Value::Null);
    let cached = usage
        .pointer("/input_tokens_details/cached_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    json!({
        "input_tokens": uint(usage, "input_tokens").saturating_sub(cached),
        "output_tokens": uint(usage, "output_tokens"),
        "cache_read_input_tokens": cached,
        "cache_creation_input_tokens": 0
    })
}

fn anthropic_usage_to_chat(usage: Option<&Value>) -> Value {
    let usage = usage.unwrap_or(&Value::Null);
    let cached = uint(usage, "cache_read_input_tokens");
    let prompt = uint(usage, "input_tokens") + cached + uint(usage, "cache_creation_input_tokens");
    json!({
        "prompt_tokens": prompt,
        "completion_tokens": uint(usage, "output_tokens"),
        "total_tokens": prompt + uint(usage, "output_tokens"),
        "prompt_tokens_details": { "cached_tokens": cached }
    })
}

fn anthropic_usage_to_responses(usage: Option<&Value>) -> Value {
    let usage = usage.unwrap_or(&Value::Null);
    let cached = uint(usage, "cache_read_input_tokens");
    let input = uint(usage, "input_tokens") + cached + uint(usage, "cache_creation_input_tokens");
    let output = uint(usage, "output_tokens");
    json!({
        "input_tokens": input,
        "output_tokens": output,
        "total_tokens": input + output,
        "input_tokens_details": { "cached_tokens": cached },
        "output_tokens_details": { "reasoning_tokens": 0 }
    })
}

#[cfg(test)]
mod tests;

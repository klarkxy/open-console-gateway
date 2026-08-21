use crate::models::UpstreamChannel;
use crate::pricing::normalize_model_name;
use crate::provider::{
    COMMAND_CODE_GOAT_CHAT_COMPLETIONS_PATH, COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_ALIAS,
    COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM, COMMAND_CODE_GOAT_MESSAGES_PATH,
};
use axum::http::StatusCode;
use base64::{
    Engine as _,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use bytes::Bytes;
use serde_json::{Map, Value, json};
use std::fmt;
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ApiFormat {
    ChatCompletions,
    Responses,
    Messages,
    /// Google Gemini generateContent wire format. This is client-only: OCG
    /// always translates it to a model's known native upstream protocol.
    Gemini,
}

impl ApiFormat {
    pub fn upstream_path(self) -> Option<&'static str> {
        match self {
            Self::ChatCompletions => Some("/v1/chat/completions"),
            Self::Responses => Some("/v1/responses"),
            Self::Messages => Some("/v1/messages"),
            Self::Gemini => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct RequestPlan {
    pub client: ApiFormat,
    pub upstream: ApiFormat,
    /// Upstream/routed model sent to the provider. Forward logs persist this as
    /// `upstream_model`; pricing still keys off this ID.
    pub model: String,
    /// Original client-requested name, echoed in downstream responses. Forward
    /// logs persist this as `requested_model`.
    pub client_model: String,
    pub stream: bool,
    pub body: Bytes,
    /// Resolved upstream product channel (Go vs Zen free).
    pub channel: UpstreamChannel,
    /// Optional override for `AppConfig.upstream_base_url` (Zen free base).
    pub upstream_base_override: Option<String>,
    /// Client-requested model before prefer mapping, when different.
    pub original_model: Option<String>,
    /// When free pool is exhausted, retry once on Go with `original_model`.
    pub allow_go_fallback: bool,
    pub(crate) service_tier: Option<String>,
    pub(crate) custom_tools: Vec<String>,
    pub(crate) namespace_tools: Vec<NamespaceToolMapping>,
    pub(crate) response_parallel_tool_calls: bool,
    pub(crate) response_tool_choice: Value,
    pub(crate) response_tools: Vec<Value>,
}

impl RequestPlan {
    /// Name written into downstream responses. Falls back to the upstream
    /// model when a caller constructed a plan without `client_model`.
    pub fn response_model(&self) -> &str {
        if self.client_model.is_empty() {
            &self.model
        } else {
            &self.client_model
        }
    }

    /// Client-facing request text persisted on forward logs as `requested_model`.
    pub fn log_requested_model(&self) -> &str {
        self.response_model()
    }

    /// Actual materialized upstream ID persisted on forward logs as `upstream_model`.
    pub fn log_upstream_model(&self) -> &str {
        &self.model
    }
}

/// Client protocol parsed once, before per-candidate materialization.
///
/// Later provider adapters must not re-parse or billable-probe this body.
/// Convert it with [`materialize_parsed_request`] using that candidate's
/// upstream model / channel.
#[derive(Debug, Clone)]
pub struct ParsedClientRequest {
    pub client: ApiFormat,
    pub requested_model: String,
    pub stream: bool,
    parsed: Value,
}

/// Per-candidate identity used to turn a parsed client request into a
/// [`RequestPlan`]. Endpoint and auth stay in the provider adapter.
#[derive(Debug, Clone)]
pub struct MaterializeSpec {
    pub client_model: String,
    pub upstream_model: String,
    /// Canonical registry alias from runtime resolution. `None` when the
    /// candidate is a unique raw ID with no published alias.
    pub resolved_alias: Option<String>,
    pub channel: UpstreamChannel,
    pub upstream_base_override: Option<String>,
    pub original_model: Option<String>,
    pub allow_go_fallback: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NamespaceToolMapping {
    pub(crate) flattened: String,
    pub(crate) namespace: String,
    pub(crate) name: String,
    pub(crate) custom: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtocolError {
    pub status: StatusCode,
    pub message: String,
    pub code: Option<&'static str>,
}

impl ProtocolError {
    pub(crate) fn new(message: impl Into<String>) -> Self {
        Self::with_status(StatusCode::BAD_REQUEST, message)
    }

    pub(crate) fn with_status(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
            code: None,
        }
    }

    pub(crate) fn with_code(
        status: StatusCode,
        code: &'static str,
        message: impl Into<String>,
    ) -> Self {
        Self {
            status,
            message: message.into(),
            code: Some(code),
        }
    }
}

impl fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ProtocolError {}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct UsageCounts {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_tokens: u64,
    pub cache_creation_tokens: u64,
}

/// Hardcoded OpenCode-Go protocol profiles.
///
/// `preferred` matches the official Go docs endpoint table. `supported` is the
/// set of upstream protocols verified with a test account; update only after a
/// fresh probe. Request paths never trial protocols (double-billing risk).
#[derive(Debug, Clone, Copy)]
struct ModelProtocol {
    id: &'static str,
    preferred: ApiFormat,
    supported: &'static [ApiFormat],
    /// Aliases applied to `reasoning.effort` / `reasoning_effort` before forwarding
    /// or converting, for models whose upstream rejects a standard OCG effort.
    /// Empty slice = pass through unchanged.
    effort_aliases: &'static [(&'static str, &'static str)],
}

const NO_EFFORT_ALIASES: &[(&str, &str)] = &[];
const MUSE_SPARK_EFFORT_ALIASES: &[(&str, &str)] = &[("max", "xhigh")];

// Probe matrix (direct OpenCode-Go, 2026-08-14, test account):
// preferred stays on the official docs endpoint; supported is live stream +
// non-stream 2xx with a usable body. 5-turn preferred conversations completed
// 5/5 HTTP OK on every paid model that accepted its preferred endpoint.
const CHAT_ONLY: &[ApiFormat] = &[ApiFormat::ChatCompletions];
const RESPONSES_ONLY: &[ApiFormat] = &[ApiFormat::Responses];
const CHAT_AND_RESPONSES: &[ApiFormat] = &[ApiFormat::ChatCompletions, ApiFormat::Responses];
const CHAT_AND_MESSAGES: &[ApiFormat] = &[ApiFormat::ChatCompletions, ApiFormat::Messages];
const ALL_THREE: &[ApiFormat] = &[
    ApiFormat::ChatCompletions,
    ApiFormat::Responses,
    ApiFormat::Messages,
];

const MODEL_PROTOCOLS: &[ModelProtocol] = &[
    ModelProtocol {
        id: "grok-4.5",
        preferred: ApiFormat::Responses,
        supported: RESPONSES_ONLY,
        effort_aliases: NO_EFFORT_ALIASES,
    },
    ModelProtocol {
        id: "glm-5.3",
        preferred: ApiFormat::ChatCompletions,
        supported: CHAT_ONLY,
        effort_aliases: NO_EFFORT_ALIASES,
    },
    ModelProtocol {
        id: "glm-5.2",
        preferred: ApiFormat::ChatCompletions,
        supported: ALL_THREE,
        effort_aliases: NO_EFFORT_ALIASES,
    },
    ModelProtocol {
        id: "glm-5.1",
        preferred: ApiFormat::ChatCompletions,
        supported: ALL_THREE,
        effort_aliases: NO_EFFORT_ALIASES,
    },
    ModelProtocol {
        id: "glm-5",
        preferred: ApiFormat::ChatCompletions,
        supported: ALL_THREE,
        effort_aliases: NO_EFFORT_ALIASES,
    },
    ModelProtocol {
        id: "gpt-5.6-luna",
        preferred: ApiFormat::Responses,
        supported: CHAT_AND_RESPONSES,
        effort_aliases: NO_EFFORT_ALIASES,
    },
    ModelProtocol {
        id: "muse-spark-1.2",
        preferred: ApiFormat::Responses,
        supported: RESPONSES_ONLY,
        effort_aliases: MUSE_SPARK_EFFORT_ALIASES,
    },
    ModelProtocol {
        id: "muse-spark-1.2-contributor",
        preferred: ApiFormat::Responses,
        supported: RESPONSES_ONLY,
        effort_aliases: MUSE_SPARK_EFFORT_ALIASES,
    },
    ModelProtocol {
        id: "kimi-k3",
        preferred: ApiFormat::ChatCompletions,
        supported: CHAT_AND_MESSAGES,
        effort_aliases: NO_EFFORT_ALIASES,
    },
    ModelProtocol {
        id: "kimi-k2.7-code",
        preferred: ApiFormat::ChatCompletions,
        supported: CHAT_ONLY,
        effort_aliases: NO_EFFORT_ALIASES,
    },
    ModelProtocol {
        id: "kimi-k2.6",
        preferred: ApiFormat::ChatCompletions,
        supported: CHAT_ONLY,
        effort_aliases: NO_EFFORT_ALIASES,
    },
    ModelProtocol {
        id: "kimi-k2.5",
        preferred: ApiFormat::ChatCompletions,
        supported: CHAT_ONLY,
        effort_aliases: NO_EFFORT_ALIASES,
    },
    ModelProtocol {
        id: "deepseek-v4-pro",
        preferred: ApiFormat::ChatCompletions,
        supported: ALL_THREE,
        effort_aliases: NO_EFFORT_ALIASES,
    },
    ModelProtocol {
        id: "deepseek-v4-flash",
        preferred: ApiFormat::ChatCompletions,
        supported: ALL_THREE,
        effort_aliases: NO_EFFORT_ALIASES,
    },
    ModelProtocol {
        id: "mimo-v2.5",
        preferred: ApiFormat::ChatCompletions,
        supported: CHAT_ONLY,
        effort_aliases: NO_EFFORT_ALIASES,
    },
    ModelProtocol {
        id: "mimo-v2.5-pro",
        preferred: ApiFormat::ChatCompletions,
        supported: CHAT_ONLY,
        effort_aliases: NO_EFFORT_ALIASES,
    },
    ModelProtocol {
        id: "hy3",
        preferred: ApiFormat::ChatCompletions,
        supported: CHAT_ONLY,
        effort_aliases: NO_EFFORT_ALIASES,
    },
    ModelProtocol {
        id: "minimax-m3",
        preferred: ApiFormat::Messages,
        supported: CHAT_AND_MESSAGES,
        effort_aliases: NO_EFFORT_ALIASES,
    },
    ModelProtocol {
        id: "minimax-m2.7",
        preferred: ApiFormat::Messages,
        supported: CHAT_AND_MESSAGES,
        effort_aliases: NO_EFFORT_ALIASES,
    },
    ModelProtocol {
        id: "minimax-m2.7-highspeed",
        preferred: ApiFormat::Messages,
        supported: CHAT_AND_MESSAGES,
        effort_aliases: NO_EFFORT_ALIASES,
    },
    ModelProtocol {
        id: "minimax-m2.5",
        preferred: ApiFormat::Messages,
        supported: CHAT_AND_MESSAGES,
        effort_aliases: NO_EFFORT_ALIASES,
    },
    ModelProtocol {
        id: "minimax-m2.5-highspeed",
        preferred: ApiFormat::Messages,
        supported: CHAT_AND_MESSAGES,
        effort_aliases: NO_EFFORT_ALIASES,
    },
    ModelProtocol {
        id: "qwen3.8-max",
        preferred: ApiFormat::Messages,
        supported: CHAT_AND_MESSAGES,
        effort_aliases: NO_EFFORT_ALIASES,
    },
    ModelProtocol {
        id: "qwen3.7-max",
        preferred: ApiFormat::Messages,
        supported: CHAT_AND_MESSAGES,
        effort_aliases: NO_EFFORT_ALIASES,
    },
    ModelProtocol {
        id: "qwen3.7-plus",
        preferred: ApiFormat::Messages,
        supported: CHAT_AND_MESSAGES,
        effort_aliases: NO_EFFORT_ALIASES,
    },
    ModelProtocol {
        id: "qwen3.6-plus",
        preferred: ApiFormat::Messages,
        supported: CHAT_AND_MESSAGES,
        effort_aliases: NO_EFFORT_ALIASES,
    },
    ModelProtocol {
        id: "qwen3.5-plus",
        preferred: ApiFormat::Messages,
        supported: CHAT_AND_MESSAGES,
        effort_aliases: NO_EFFORT_ALIASES,
    },
    ModelProtocol {
        id: "big-pickle",
        preferred: ApiFormat::ChatCompletions,
        supported: CHAT_ONLY,
        effort_aliases: NO_EFFORT_ALIASES,
    },
    ModelProtocol {
        id: "deepseek-v4-flash-free",
        preferred: ApiFormat::ChatCompletions,
        supported: CHAT_ONLY,
        effort_aliases: NO_EFFORT_ALIASES,
    },
    ModelProtocol {
        id: "mimo-v2.5-free",
        preferred: ApiFormat::ChatCompletions,
        supported: CHAT_ONLY,
        effort_aliases: NO_EFFORT_ALIASES,
    },
    ModelProtocol {
        id: "ling-3.0-flash-free",
        preferred: ApiFormat::ChatCompletions,
        supported: CHAT_ONLY,
        effort_aliases: NO_EFFORT_ALIASES,
    },
    ModelProtocol {
        id: "laguna-s-2.1-free",
        preferred: ApiFormat::ChatCompletions,
        supported: CHAT_ONLY,
        effort_aliases: NO_EFFORT_ALIASES,
    },
    ModelProtocol {
        id: "longcat-2.0-free",
        preferred: ApiFormat::ChatCompletions,
        supported: CHAT_ONLY,
        effort_aliases: NO_EFFORT_ALIASES,
    },
    ModelProtocol {
        id: "north-mini-code-free",
        preferred: ApiFormat::ChatCompletions,
        supported: CHAT_ONLY,
        effort_aliases: NO_EFFORT_ALIASES,
    },
    ModelProtocol {
        id: "nemotron-3-ultra-free",
        preferred: ApiFormat::ChatCompletions,
        supported: CHAT_ONLY,
        effort_aliases: NO_EFFORT_ALIASES,
    },
];

/// Returns every model ID with a known preferred upstream protocol.
pub fn supported_model_ids() -> impl Iterator<Item = &'static str> {
    MODEL_PROTOCOLS.iter().map(|profile| profile.id)
}

/// Provider adapters use the same probed OpenCode matrix as request planning;
/// this prevents a mixed-provider selector from probing an unsupported
/// model/protocol pair merely because that account appears earlier.
pub fn opencode_supports_upstream(model: &str, upstream: ApiFormat) -> bool {
    model_protocol(model).is_some_and(|profile| profile.supported.contains(&upstream))
}

/// Command Code GOAT protocol profiles, independent of OpenCode `MODEL_PROTOCOLS`.
/// Lookup is exact (case-insensitive) on the upstream raw ID. Slash IDs are
/// never folded onto kebab OpenCode aliases, so `deepseek/deepseek-v4-flash`
/// cannot steal Go's `deepseek-v4-flash` protocol row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandCodeModelProtocol {
    pub alias: &'static str,
    pub upstream_id: &'static str,
    pub preferred: ApiFormat,
    pub supported_upstream: &'static [ApiFormat],
}

const COMMAND_CODE_MODEL_PROTOCOLS: &[CommandCodeModelProtocol] = &[CommandCodeModelProtocol {
    alias: COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_ALIAS,
    upstream_id: COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM,
    preferred: ApiFormat::ChatCompletions,
    supported_upstream: CHAT_ONLY,
}];

/// Exact Command Code raw-ID lookup. Does not consult OpenCode `MODEL_PROTOCOLS`
/// and does not slash-fold onto a kebab alias.
pub fn command_code_model_protocol(model: &str) -> Option<&'static CommandCodeModelProtocol> {
    let trimmed = model.trim();
    if trimmed.is_empty() {
        return None;
    }
    COMMAND_CODE_MODEL_PROTOCOLS
        .iter()
        .find(|profile| profile.upstream_id.eq_ignore_ascii_case(trimmed))
}

pub fn command_code_supports_upstream(model: &str, upstream: ApiFormat) -> bool {
    command_code_model_protocol(model)
        .is_some_and(|profile| profile.supported_upstream.contains(&upstream))
}

/// Official Command Code relative paths. Responses and Gemini have no upstream
/// path; client formats convert to Chat for the first supported GOAT model.
pub fn command_code_upstream_path(format: ApiFormat) -> Option<&'static str> {
    match format {
        ApiFormat::ChatCompletions => Some(COMMAND_CODE_GOAT_CHAT_COMPLETIONS_PATH),
        ApiFormat::Messages => Some(COMMAND_CODE_GOAT_MESSAGES_PATH),
        ApiFormat::Responses | ApiFormat::Gemini => None,
    }
}

const ANTHROPIC_THINKING_ENCRYPTED_PREFIX: &str = "ocg-anthropic-thinking-v1:";
const CHAT_REASONING_ENCRYPTED_PREFIX: &str = "ocg-chat-reasoning-v1:";
const CHAT_TOOL_REASONING_PLACEHOLDER: &str = "Tool call reasoning unavailable.";

pub fn parse_client_request(
    client: ApiFormat,
    body: Bytes,
) -> Result<ParsedClientRequest, ProtocolError> {
    let parsed: Value = serde_json::from_slice(&body)
        .map_err(|error| ProtocolError::new(format!("invalid JSON request: {error}")))?;
    let model = parsed
        .get("model")
        .and_then(Value::as_str)
        .filter(|model| !model.is_empty())
        .ok_or_else(|| ProtocolError::new("request model is required"))?
        .to_string();
    let stream = parsed
        .get("stream")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    Ok(ParsedClientRequest {
        client,
        requested_model: model,
        stream,
        parsed,
    })
}

pub fn parse_gemini_request(
    model: String,
    stream: bool,
    body: Bytes,
) -> Result<ParsedClientRequest, ProtocolError> {
    if model.trim().is_empty() {
        return Err(ProtocolError::new("request model is required"));
    }
    let mut parsed: Value = serde_json::from_slice(&body)
        .map_err(|error| ProtocolError::new(format!("invalid JSON request: {error}")))?;
    let object = parsed
        .as_object_mut()
        .ok_or_else(|| ProtocolError::new("Gemini request must be a JSON object"))?;
    object.insert("model".into(), json!(model.clone()));
    object.insert("stream".into(), json!(stream));
    Ok(ParsedClientRequest {
        client: ApiFormat::Gemini,
        requested_model: model,
        stream,
        parsed,
    })
}

/// Test-only identity planner. Production inference must parse once, resolve
/// the name through [`crate::alias::resolve`], then call
/// [`materialize_parsed_request`]. This helper never bypasses alias
/// resolution: unknown Chat/Messages models fail closed here too.
#[cfg(test)]
pub fn prepare_request(client: ApiFormat, body: Bytes) -> Result<RequestPlan, ProtocolError> {
    let parsed = parse_client_request(client, body)?;
    materialize_parsed_request(&parsed, &identity_spec(&parsed))
}

/// Test-only Gemini identity planner. Same fail-closed contract as
/// [`prepare_request`]; not a production forward path.
#[cfg(test)]
pub fn prepare_gemini_request(
    model: String,
    stream: bool,
    body: Bytes,
) -> Result<RequestPlan, ProtocolError> {
    let parsed = parse_gemini_request(model, stream, body)?;
    materialize_parsed_request(&parsed, &identity_spec(&parsed))
}

#[cfg(test)]
fn identity_spec(parsed: &ParsedClientRequest) -> MaterializeSpec {
    MaterializeSpec {
        client_model: parsed.requested_model.clone(),
        upstream_model: parsed.requested_model.clone(),
        resolved_alias: match crate::alias::resolve(&parsed.requested_model) {
            Ok(crate::alias::ResolvedModel::Alias { alias, .. }) => Some(alias.to_string()),
            Ok(crate::alias::ResolvedModel::PinnedRaw { .. }) | Err(_) => None,
        },
        channel: UpstreamChannel::Go,
        upstream_base_override: None,
        original_model: None,
        allow_go_fallback: false,
    }
}

/// Convert a request that was already parsed once for a specific candidate.
///
/// Protocol selection uses the OpenCode `MODEL_PROTOCOLS` table for the
/// upstream model. Callers must never trial a billable inference path.
pub fn materialize_parsed_request(
    parsed: &ParsedClientRequest,
    spec: &MaterializeSpec,
) -> Result<RequestPlan, ProtocolError> {
    let mut body = parsed.parsed.clone();
    if let Some(object) = body.as_object_mut() {
        object.insert("model".into(), json!(&spec.upstream_model));
    }
    let mut plan = prepare_parsed_request(
        parsed.client,
        body,
        spec.upstream_model.clone(),
        parsed.stream,
    )?;
    plan.client_model = spec.client_model.clone();
    plan.channel = spec.channel;
    plan.upstream_base_override = spec.upstream_base_override.clone();
    plan.original_model = spec.original_model.clone();
    plan.allow_go_fallback = spec.allow_go_fallback;
    debug_assert!(
        spec.resolved_alias
            .as_deref()
            .is_none_or(|alias| !alias.is_empty())
    );
    Ok(plan)
}

fn prepare_parsed_request(
    client: ApiFormat,
    parsed: Value,
    model: String,
    stream: bool,
) -> Result<RequestPlan, ProtocolError> {
    let upstream = resolve_upstream_format(client, &model)?;
    let aliased_responses_effort = requested_effort_alias(&parsed, &model);
    let parsed = apply_effort_aliases(parsed, &model);
    validate_request_features(client, upstream, &parsed)?;
    let tool_context = if client == ApiFormat::Responses {
        responses_tool_context(&parsed)?
    } else {
        ResponsesToolContext::default()
    };
    let response_parallel_tool_calls = parsed
        .get("parallel_tool_calls")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let response_tool_choice = parsed
        .get("tool_choice")
        .cloned()
        .unwrap_or_else(|| json!("auto"));
    let response_tools = array(&parsed, "tools").to_vec();
    let mut converted = convert_request(client, upstream, parsed, &tool_context.namespace_tools)?;
    if upstream == ApiFormat::Responses
        && let Some(effort) = aliased_responses_effort
    {
        set_responses_reasoning_effort(&mut converted, effort);
    }
    let service_tier = converted
        .get("service_tier")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let body = serde_json::to_vec(&converted)
        .map(Bytes::from)
        .map_err(|error| ProtocolError::new(format!("failed to encode request: {error}")))?;
    Ok(RequestPlan {
        client,
        upstream,
        client_model: model.clone(),
        model,
        stream,
        body,
        channel: UpstreamChannel::Go,
        upstream_base_override: None,
        original_model: None,
        allow_go_fallback: false,
        service_tier,
        custom_tools: tool_context.custom_tools,
        namespace_tools: tool_context.namespace_tools,
        response_parallel_tool_calls,
        response_tool_choice,
        response_tools,
    })
}

fn validate_request_features(
    client: ApiFormat,
    upstream: ApiFormat,
    body: &Value,
) -> Result<(), ProtocolError> {
    if client == ApiFormat::Responses {
        for field in ["previous_response_id", "conversation"] {
            if body.get(field).is_some_and(|value| !value.is_null()) {
                return Err(ProtocolError::new(format!(
                    "Responses {field} is not supported by this stateless gateway"
                )));
            }
        }
        if body.get("store") != Some(&Value::Bool(false)) {
            return Err(ProtocolError::new(
                "this stateless gateway requires Responses store=false",
            ));
        }
        match body.get("background") {
            None | Some(Value::Null | Value::Bool(false)) => {}
            Some(Value::Bool(true)) => {
                return Err(ProtocolError::new(
                    "Responses background=true is not supported by this stateless gateway",
                ));
            }
            Some(_) => {
                return Err(ProtocolError::new("Responses background must be a boolean"));
            }
        }
        if contains_input_image_file_id(body.get("input").unwrap_or(&Value::Null)) {
            return Err(ProtocolError::new(
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
        return Err(ProtocolError::new(format!(
            "{field} cannot be preserved by protocol conversion"
        )));
    }
    if client == ApiFormat::Responses && array(body, "tools").iter().any(has_custom_tool_format) {
        return Err(ProtocolError::new(
            "Responses custom tool grammar format cannot be preserved by protocol conversion",
        ));
    }
    Ok(())
}

fn validate_gemini_request(body: &Value) -> Result<(), ProtocolError> {
    match body.get("safetySettings") {
        None | Some(Value::Null) => {}
        Some(Value::Array(settings)) if settings.is_empty() => {}
        Some(Value::Array(_)) => {
            return Err(ProtocolError::new(
                "Gemini safetySettings cannot be preserved by protocol conversion",
            ));
        }
        Some(_) => {
            return Err(ProtocolError::new("Gemini safetySettings must be an array"));
        }
    }
    if body
        .get("cachedContent")
        .is_some_and(|value| !value.is_null())
    {
        return Err(ProtocolError::new(
            "Gemini cachedContent is not supported by this stateless gateway",
        ));
    }
    if body.get("tools").is_some_and(|tools| !tools.is_array()) {
        return Err(ProtocolError::new("Gemini tools must be an array"));
    }
    if body
        .get("generationConfig")
        .is_some_and(|config| !config.is_object())
    {
        return Err(ProtocolError::new(
            "Gemini generationConfig must be an object",
        ));
    }
    let contents = body
        .get("contents")
        .and_then(Value::as_array)
        .ok_or_else(|| ProtocolError::new("Gemini contents must be an array"))?;
    if contents.is_empty() {
        return Err(ProtocolError::new("Gemini contents cannot be empty"));
    }
    for content in contents {
        let role = content
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or("user");
        if !matches!(role, "user" | "model") {
            return Err(ProtocolError::new(format!(
                "Gemini content role `{role}` is not supported"
            )));
        }
        let parts = content
            .get("parts")
            .and_then(Value::as_array)
            .ok_or_else(|| ProtocolError::new("Gemini content parts must be an array"))?;
        for part in parts {
            if part.get("fileData").is_some() || part.get("file_data").is_some() {
                return Err(ProtocolError::new(
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
                    return Err(ProtocolError::new(
                        "Gemini inlineData supports PNG, JPEG, GIF, and WebP images only",
                    ));
                }
                let encoded = data.get("data").and_then(Value::as_str).unwrap_or_default();
                if encoded.is_empty() {
                    return Err(ProtocolError::new("Gemini inlineData.data is required"));
                }
                if STANDARD.decode(encoded).is_err() {
                    return Err(ProtocolError::new(
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
                    return Err(ProtocolError::new("Gemini functionCall.name is required"));
                }
                if call.get("args").is_some_and(|args| !args.is_object()) {
                    return Err(ProtocolError::new(
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
                    return Err(ProtocolError::new(
                        "Gemini functionResponse.name is required",
                    ));
                }
                if response.get("parts").is_some() {
                    return Err(ProtocolError::new(
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
                return Err(ProtocolError::new(
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
            return Err(ProtocolError::new(
                "Gemini systemInstruction currently supports text parts only",
            ));
        }
    }

    for tool in array(body, "tools") {
        let Some(object) = tool.as_object() else {
            return Err(ProtocolError::new("Gemini tools must be objects"));
        };
        if object.contains_key("googleSearch")
            || object.contains_key("google_search")
            || object.contains_key("googleSearchRetrieval")
        {
            return Err(ProtocolError::new(
                "Gemini Google Search tools are not supported by this gateway",
            ));
        }
        if object.contains_key("urlContext") || object.contains_key("url_context") {
            return Err(ProtocolError::new(
                "Gemini urlContext is not supported by this gateway",
            ));
        }
        if object.len() != 1 || !object.contains_key("functionDeclarations") {
            return Err(ProtocolError::new(
                "only Gemini functionDeclarations tools are supported",
            ));
        }
        let declarations = tool
            .get("functionDeclarations")
            .and_then(Value::as_array)
            .filter(|declarations| !declarations.is_empty())
            .ok_or_else(|| {
                ProtocolError::new("Gemini functionDeclarations must be a non-empty array")
            })?;
        for declaration in declarations {
            if declaration.get("parameters").is_some()
                && declaration.get("parametersJsonSchema").is_some()
            {
                return Err(ProtocolError::new(
                    "Gemini function declarations cannot contain both parameters and parametersJsonSchema",
                ));
            }
            if declaration.get("response").is_some()
                || declaration.get("responseJsonSchema").is_some()
                || declaration.get("behavior").is_some()
            {
                return Err(ProtocolError::new(
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
            return Err(ProtocolError::new(
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
            return Err(ProtocolError::new(
                "Gemini function calling mode ANY requires a declared function",
            ));
        }
        if config
            .get("allowedFunctionNames")
            .and_then(Value::as_array)
            .is_some_and(|names| !names.is_empty())
            && mode != "ANY"
        {
            return Err(ProtocolError::new(
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
            return Err(ProtocolError::new(format!(
                "Gemini generationConfig.{field} cannot be preserved by protocol conversion"
            )));
        }
        if config
            .get("topK")
            .is_some_and(|value| !value.is_null() && !value.is_number())
        {
            return Err(ProtocolError::new(
                "Gemini generationConfig.topK must be a number",
            ));
        }
        if config
            .get("thinkingConfig")
            .is_some_and(|value| !value.is_null() && !value.is_object())
        {
            return Err(ProtocolError::new(
                "Gemini generationConfig.thinkingConfig must be an object",
            ));
        }
        if config
            .get("responseMimeType")
            .is_some_and(|value| !value.is_null() && !value.is_string())
        {
            return Err(ProtocolError::new(
                "Gemini generationConfig.responseMimeType must be a string",
            ));
        }
    }
    match generation.get("candidateCount") {
        None | Some(Value::Null) => {}
        Some(value) if value.as_u64() == Some(1) => {}
        Some(value) if value.as_u64().is_some() => {
            return Err(ProtocolError::new(
                "Gemini candidateCount other than 1 is not supported",
            ));
        }
        Some(_) => {
            return Err(ProtocolError::new(
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
            return Err(ProtocolError::new(
                "Gemini response modalities other than TEXT are not supported",
            ));
        }
        Some(Value::Array(_)) => {}
        Some(_) => {
            return Err(ProtocolError::new(
                "Gemini generationConfig.responseModalities must be an array",
            ));
        }
    }
    let _ = gemini_output_schema(body)?;
    Ok(())
}

fn gemini_output_schema(body: &Value) -> Result<Option<Value>, ProtocolError> {
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
        (Some("application/json"), None) => Err(ProtocolError::new(
            "Gemini application/json output requires responseJsonSchema",
        )),
        (Some(other), _) => Err(ProtocolError::new(format!(
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

pub fn transform_response(plan: &RequestPlan, body: &Value) -> Result<Value, ProtocolError> {
    // Some Anthropic-compatible upstreams (notably MiniMax via OpenCode Go) omit or rewrite
    // the model field in the response body. Carry the request model over so downstream
    // sanitizers (e.g. the MiniMax bogus all-cache workaround) can still identify the model.
    let mut body = body.clone();
    if plan.upstream == ApiFormat::Messages {
        let response_model = body.get("model").and_then(Value::as_str).map(str::to_owned);
        let object = body
            .as_object_mut()
            .ok_or_else(|| ProtocolError::new("Messages response must be a JSON object"))?;
        if let Some(usage) = object.get_mut("usage") {
            sanitize_minimax_anthropic_usage(response_model.as_deref(), Some(&plan.model), usage);
        }
        object.insert("model".to_string(), json!(&plan.model));
    }
    let mut transformed = transform_between_with_tools(
        plan.upstream,
        plan.client,
        &body,
        &plan.custom_tools,
        &plan.namespace_tools,
        Some(&plan.model),
    )?;
    if plan.client == ApiFormat::Responses && plan.upstream != ApiFormat::Responses {
        transformed["parallel_tool_calls"] = json!(plan.response_parallel_tool_calls);
        transformed["tool_choice"] = plan.response_tool_choice.clone();
        transformed["tools"] = Value::Array(plan.response_tools.clone());
    }
    // If the upstream was already Chat Completions and the client also expects Chat
    // Completions, the body is passed through unchanged. MiniMax's Chat Completions
    // endpoint can still report the bogus all-cache signature, so sanitize here too.
    if plan.client == ApiFormat::ChatCompletions && plan.upstream == ApiFormat::ChatCompletions {
        let model = transformed
            .get("model")
            .and_then(Value::as_str)
            .map(str::to_owned);
        if let Some(usage) = transformed.get_mut("usage") {
            sanitize_minimax_chat_usage(model.as_deref(), Some(&plan.model), usage);
        }
    }
    rewrite_visible_model(plan.client, &mut transformed, plan.response_model());
    Ok(transformed)
}

/// Rewrite client-visible model fields to the original requested name.
pub(crate) fn rewrite_visible_model(format: ApiFormat, value: &mut Value, client_model: &str) {
    rewrite_visible_model_inner(format, value, client_model, true);
}

/// Update existing model fields only. Stream passthrough must not insert a
/// `model` key into events that never had one (for example `message_stop`).
pub(crate) fn rewrite_existing_visible_model(
    format: ApiFormat,
    value: &mut Value,
    client_model: &str,
) {
    rewrite_visible_model_inner(format, value, client_model, false);
}

fn rewrite_visible_model_inner(
    format: ApiFormat,
    value: &mut Value,
    client_model: &str,
    insert: bool,
) {
    if client_model.is_empty() {
        return;
    }
    let name = json!(client_model);
    let assign = |object: &mut serde_json::Map<String, Value>, key: &str, value: Value| match object
        .get(key)
    {
        Some(existing) if existing == &value => {}
        Some(_) => {
            object.insert(key.to_string(), value);
        }
        None if insert => {
            object.insert(key.to_string(), value);
        }
        None => {}
    };
    match format {
        ApiFormat::ChatCompletions => {
            if let Some(object) = value.as_object_mut() {
                assign(object, "model", name);
            }
        }
        ApiFormat::Messages => {
            if let Some(object) = value.as_object_mut() {
                assign(object, "model", name.clone());
                if let Some(message) = object.get_mut("message").and_then(Value::as_object_mut) {
                    assign(message, "model", name);
                }
            }
        }
        ApiFormat::Responses => {
            if let Some(object) = value.as_object_mut() {
                assign(object, "model", name.clone());
                if let Some(response) = object.get_mut("response").and_then(Value::as_object_mut) {
                    assign(response, "model", name);
                }
            }
        }
        ApiFormat::Gemini => {
            if let Some(object) = value.as_object_mut() {
                assign(object, "modelVersion", name);
            }
        }
    }
}

pub fn transform_between(
    upstream: ApiFormat,
    client: ApiFormat,
    body: &Value,
) -> Result<Value, ProtocolError> {
    transform_between_with_tools(upstream, client, body, &[], &[], None)
}

fn transform_between_with_tools(
    upstream: ApiFormat,
    client: ApiFormat,
    body: &Value,
    custom_tools: &[String],
    namespace_tools: &[NamespaceToolMapping],
    model_hint: Option<&str>,
) -> Result<Value, ProtocolError> {
    match (upstream, client) {
        (a, b) if a == b => Ok(body.clone()),
        (ApiFormat::Messages, ApiFormat::ChatCompletions) => {
            messages_response_to_chat(body, model_hint)
        }
        (ApiFormat::ChatCompletions, ApiFormat::Messages) => chat_response_to_messages(body),
        (ApiFormat::Messages, ApiFormat::Responses) => {
            messages_response_to_responses(body, custom_tools, namespace_tools)
        }
        (ApiFormat::Responses, ApiFormat::Messages) => responses_response_to_messages(body),
        (ApiFormat::ChatCompletions, ApiFormat::Responses) => messages_response_to_responses(
            &chat_response_to_messages(body)?,
            custom_tools,
            namespace_tools,
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
        _ => Err(ProtocolError::new(
            "Gemini is a client-only format and cannot be used as an upstream protocol",
        )),
    }
}

pub fn format_error(
    format: ApiFormat,
    status: StatusCode,
    message: &str,
    upstream: Option<&Value>,
) -> Value {
    format_error_with_code(format, status, message, upstream, None)
}

pub fn format_protocol_error(
    format: ApiFormat,
    error: &ProtocolError,
    upstream: Option<&Value>,
) -> Value {
    format_error_with_code(format, error.status, &error.message, upstream, error.code)
}

pub fn format_error_with_code(
    format: ApiFormat,
    status: StatusCode,
    message: &str,
    upstream: Option<&Value>,
    code: Option<&str>,
) -> Value {
    if format == ApiFormat::Gemini {
        let upstream_message = upstream
            .and_then(|value| value.pointer("/error/message"))
            .and_then(Value::as_str)
            .unwrap_or(message);
        return gemini_error_body(status, upstream_message, code);
    }
    let upstream_error = upstream.and_then(|value| value.get("error"));
    let message = upstream_error
        .and_then(|error| error.get("message"))
        .and_then(Value::as_str)
        .unwrap_or(message);
    let kind = upstream_error
        .and_then(|error| error.get("type"))
        .and_then(Value::as_str)
        .or(code)
        .unwrap_or_else(|| match status.as_u16() {
            401 | 403 => "authentication_error",
            429 => "rate_limit_error",
            400..=499 => "invalid_request_error",
            _ => "api_error",
        });
    error_body(format, kind, message)
}

pub fn error_body(format: ApiFormat, kind: &str, message: &str) -> Value {
    match format {
        ApiFormat::Messages => json!({
            "type": "error",
            "error": { "type": kind, "message": message }
        }),
        ApiFormat::ChatCompletions => json!({
            "error": { "message": message, "type": kind, "param": null, "code": null }
        }),
        ApiFormat::Responses => json!({
            "error": { "message": message, "type": kind, "code": kind }
        }),
        ApiFormat::Gemini => json!({
            "error": { "code": 500, "message": message, "status": gemini_status_for_kind(kind) }
        }),
    }
}

fn gemini_error_body(status: StatusCode, message: &str, code: Option<&str>) -> Value {
    let status_name = match status.as_u16() {
        400 => "INVALID_ARGUMENT",
        401 => "UNAUTHENTICATED",
        403 => "PERMISSION_DENIED",
        404 => "NOT_FOUND",
        408 => "DEADLINE_EXCEEDED",
        429 => "RESOURCE_EXHAUSTED",
        501 => "UNIMPLEMENTED",
        502..=504 => "UNAVAILABLE",
        _ => "INTERNAL",
    };
    let mut error = json!({
        "code": status.as_u16(),
        "message": message,
        "status": status_name
    });
    if let Some(code) = code
        && let Some(object) = error.as_object_mut()
    {
        object.insert("reason".into(), json!(code));
        object.insert(
            "details".into(),
            json!([{ "reason": code, "message": message }]),
        );
    }
    json!({ "error": error })
}

fn gemini_status_for_kind(kind: &str) -> &'static str {
    match kind {
        "authentication_error" => "UNAUTHENTICATED",
        "permission_error" => "PERMISSION_DENIED",
        "rate_limit_error" => "RESOURCE_EXHAUSTED",
        "invalid_request_error" => "INVALID_ARGUMENT",
        _ => "INTERNAL",
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

pub(crate) fn sanitize_minimax_anthropic_usage(
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
pub(crate) fn sanitize_minimax_chat_usage(
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
    if prompt > 0 && cached == prompt {
        if let Some(details) = obj
            .get_mut("prompt_tokens_details")
            .and_then(Value::as_object_mut)
        {
            details.insert("cached_tokens".into(), json!(0));
        }
    }
}

fn usage_payload(format: ApiFormat, payload: &Value) -> Option<&Value> {
    match format {
        ApiFormat::ChatCompletions => payload.get("usage"),
        ApiFormat::Messages => payload
            .get("usage")
            .or_else(|| payload.pointer("/message/usage")),
        ApiFormat::Responses => payload
            .get("usage")
            .or_else(|| payload.pointer("/response/usage")),
        ApiFormat::Gemini => payload.get("usageMetadata"),
    }
}

pub fn has_usage(format: ApiFormat, payload: &Value) -> bool {
    usage_payload(format, payload).is_some_and(Value::is_object)
}

pub fn has_complete_usage(format: ApiFormat, payload: &Value) -> bool {
    let Some(usage) = usage_payload(format, payload).filter(|value| value.is_object()) else {
        return false;
    };
    let has_u64 = |key: &str| usage.get(key).is_some_and(Value::is_u64);
    match format {
        ApiFormat::ChatCompletions => has_u64("prompt_tokens") && has_u64("completion_tokens"),
        ApiFormat::Messages => has_u64("input_tokens") && has_u64("output_tokens"),
        ApiFormat::Responses => has_u64("input_tokens") && has_u64("output_tokens"),
        ApiFormat::Gemini => has_u64("promptTokenCount") && has_u64("candidatesTokenCount"),
    }
}

pub fn extract_usage(format: ApiFormat, payload: &Value, model_hint: Option<&str>) -> UsageCounts {
    let usage = usage_payload(format, payload);
    let Some(usage) = usage else {
        return UsageCounts::default();
    };
    match format {
        ApiFormat::ChatCompletions => {
            let mut usage = usage.clone();
            let model = payload.get("model").and_then(Value::as_str);
            sanitize_minimax_chat_usage(model, model_hint, &mut usage);
            UsageCounts {
                input_tokens: uint(&usage, "prompt_tokens"),
                output_tokens: uint(&usage, "completion_tokens"),
                cached_tokens: usage
                    .pointer("/prompt_tokens_details/cached_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
                cache_creation_tokens: 0,
            }
        }
        ApiFormat::Messages => {
            let mut usage = usage.clone();
            let model = payload
                .get("model")
                .or_else(|| payload.pointer("/message/model"))
                .and_then(Value::as_str);
            sanitize_minimax_anthropic_usage(model, model_hint, &mut usage);
            let cached = uint(&usage, "cache_read_input_tokens");
            let created = uint(&usage, "cache_creation_input_tokens");
            UsageCounts {
                input_tokens: uint(&usage, "input_tokens")
                    .saturating_add(cached)
                    .saturating_add(created),
                output_tokens: uint(&usage, "output_tokens"),
                cached_tokens: cached,
                cache_creation_tokens: created,
            }
        }
        ApiFormat::Responses => UsageCounts {
            input_tokens: uint(usage, "input_tokens"),
            output_tokens: uint(usage, "output_tokens"),
            cached_tokens: usage
                .pointer("/input_tokens_details/cached_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            cache_creation_tokens: 0,
        },
        ApiFormat::Gemini => UsageCounts {
            input_tokens: uint(usage, "promptTokenCount"),
            output_tokens: uint(usage, "candidatesTokenCount"),
            cached_tokens: uint(usage, "cachedContentTokenCount"),
            cache_creation_tokens: 0,
        },
    }
}

pub fn merge_stream_usage(
    format: ApiFormat,
    payload: &Value,
    counts: &mut UsageCounts,
    model_hint: Option<&str>,
) {
    let next = extract_usage(format, payload, model_hint);
    counts.input_tokens = counts.input_tokens.max(next.input_tokens);
    counts.output_tokens = counts.output_tokens.max(next.output_tokens);
    counts.cached_tokens = counts.cached_tokens.max(next.cached_tokens);
    counts.cache_creation_tokens = counts.cache_creation_tokens.max(next.cache_creation_tokens);
}

fn model_protocol(model: &str) -> Option<&'static ModelProtocol> {
    let normalized = normalize_model_name(model);
    MODEL_PROTOCOLS
        .iter()
        .find(|profile| profile.id == normalized)
}

fn resolve_from_supported(
    client: ApiFormat,
    preferred: ApiFormat,
    supported: &'static [ApiFormat],
) -> ApiFormat {
    match client {
        ApiFormat::Gemini => preferred,
        client if supported.contains(&client) => client,
        _ => preferred,
    }
}

fn resolve_upstream_format(client: ApiFormat, model: &str) -> Result<ApiFormat, ProtocolError> {
    if let Some(profile) = command_code_model_protocol(model) {
        return Ok(resolve_from_supported(
            client,
            profile.preferred,
            profile.supported_upstream,
        ));
    }
    match (client, model_protocol(model)) {
        (ApiFormat::Gemini, Some(profile)) => Ok(profile.preferred),
        (client, Some(profile)) if profile.supported.contains(&client) => Ok(client),
        (_, Some(profile)) => Ok(profile.preferred),
        (_, None) => Err(ProtocolError::new(format!(
            "unknown model `{model}` cannot be routed from this endpoint"
        ))),
    }
}

/// Rewrite `reasoning.effort` (Responses/Gemini) and `reasoning_effort` (Chat)
/// according to the model's `effort_aliases`. No-op for models without aliases.
fn apply_effort_aliases(mut body: Value, model: &str) -> Value {
    let Some(profile) = model_protocol(model) else {
        return body;
    };
    if profile.effort_aliases.is_empty() {
        return body;
    }
    let rewrite = |effort: &str| -> Option<String> {
        profile
            .effort_aliases
            .iter()
            .find(|(from, _)| *from == effort)
            .map(|(_, to)| to.to_string())
    };
    if let Some(replacement) = body
        .pointer("/reasoning/effort")
        .and_then(Value::as_str)
        .and_then(&rewrite)
    {
        if let Some(reasoning) = body.get_mut("reasoning").and_then(Value::as_object_mut) {
            reasoning.insert("effort".into(), Value::String(replacement));
        }
    }
    if let Some(replacement) = body
        .get("reasoning_effort")
        .and_then(Value::as_str)
        .and_then(&rewrite)
    {
        body["reasoning_effort"] = Value::String(replacement);
    }
    if let Some(replacement) = body
        .pointer("/output_config/effort")
        .and_then(Value::as_str)
        .and_then(&rewrite)
    {
        if let Some(output_config) = body.get_mut("output_config").and_then(Value::as_object_mut) {
            output_config.insert("effort".into(), Value::String(replacement));
        }
    }
    body
}

/// Returns the aliased effort requested through any supported client shape.
/// Conversion through Messages represents reasoning as a token budget, so the
/// original alias must be retained until the final Responses body is built.
fn requested_effort_alias(body: &Value, model: &str) -> Option<&'static str> {
    let profile = model_protocol(model)?;
    let effort = body
        .pointer("/reasoning/effort")
        .or_else(|| body.get("reasoning_effort"))
        .or_else(|| body.pointer("/output_config/effort"))
        .and_then(Value::as_str)?;
    profile
        .effort_aliases
        .iter()
        .find_map(|(from, to)| (*from == effort).then_some(*to))
}

fn set_responses_reasoning_effort(body: &mut Value, effort: &str) {
    let reasoning = body
        .as_object_mut()
        .expect("converted request must remain a JSON object")
        .entry("reasoning")
        .or_insert_with(|| json!({ "summary": "auto" }));
    if let Some(reasoning) = reasoning.as_object_mut() {
        reasoning.insert("effort".into(), Value::String(effort.to_string()));
    }
}

fn convert_request(
    client: ApiFormat,
    upstream: ApiFormat,
    body: Value,
    namespace_tools: &[NamespaceToolMapping],
) -> Result<Value, ProtocolError> {
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
            return Err(ProtocolError::new(
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
                return Err(ProtocolError::new(
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

fn responses_tool_context(body: &Value) -> Result<ResponsesToolContext, ProtocolError> {
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

    for tool in tools {
        match tool.get("type").and_then(Value::as_str) {
            Some("function") => {}
            Some("custom") => {
                let name = required_tool_name(tool, "Responses custom tool")?;
                context.custom_tools.push(name.to_string());
            }
            Some("namespace") => {
                let namespace = required_tool_name(tool, "Responses namespace")?;
                let nested = tool
                    .get("tools")
                    .and_then(Value::as_array)
                    .ok_or_else(|| ProtocolError::new("Responses namespace tools are required"))?;
                for nested_tool in nested {
                    let kind = nested_tool.get("type").and_then(Value::as_str);
                    if !matches!(kind, Some("function" | "custom")) {
                        return Err(ProtocolError::new(
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
            Some(kind) if is_hosted_tool(kind) => {}
            Some(kind) => {
                return Err(ProtocolError::new(format!(
                    "Responses tool type `{kind}` is not supported by protocol conversion"
                )));
            }
            None => return Err(ProtocolError::new("Responses tool type is required")),
        }
    }

    if let Some(kind) = forced_hosted_tool(body.get("tool_choice")) {
        return Err(ProtocolError::new(format!(
            "Responses hosted tool `{kind}` cannot be forced through protocol conversion"
        )));
    }
    if requires_tool(body.get("tool_choice")) && used_names.is_empty() {
        return Err(ProtocolError::new(
            "Responses tool_choice `required` has no convertible function, custom, or namespace tool",
        ));
    }
    Ok(context)
}

fn required_tool_name<'a>(tool: &'a Value, label: &str) -> Result<&'a str, ProtocolError> {
    tool.get("name")
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())
        .ok_or_else(|| ProtocolError::new(format!("{label} name is required")))
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

/// Anthropic Messages accepts system instructions only in the top-level
/// `system` field. Recent Claude Code versions can also emit Chat-style
/// system/developer roles, so normalize those even on native Messages
/// passthrough instead of forwarding a request the upstream will reject.
fn normalize_messages_system_roles(mut body: Value) -> Result<Value, ProtocolError> {
    let object = body
        .as_object_mut()
        .ok_or_else(|| ProtocolError::new("Messages request must be a JSON object"))?;
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

fn gemini_request_to_messages(body: Value) -> Result<Value, ProtocolError> {
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
        return Err(ProtocolError::new(
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
                    ProtocolError::new("Gemini function declaration name is required")
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
                return Err(ProtocolError::new(format!(
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
                return Err(ProtocolError::new(format!(
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

fn messages_request_to_chat(body: Value) -> Result<Value, ProtocolError> {
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

fn chat_request_to_messages(body: Value) -> Result<Value, ProtocolError> {
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
            return Err(ProtocolError::new(
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
        if role == "assistant" {
            if let Some(calls) = message.get("tool_calls").and_then(Value::as_array) {
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

fn messages_request_to_responses(body: Value) -> Result<Value, ProtocolError> {
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
) -> Result<Value, ProtocolError> {
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
        return Err(ProtocolError::new(
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

fn chat_response_to_messages(body: &Value) -> Result<Value, ProtocolError> {
    let choice = body
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .ok_or_else(|| ProtocolError::new("Chat Completions response has no choice"))?;
    let message = choice
        .get("message")
        .ok_or_else(|| ProtocolError::new("Chat Completions response has no message"))?;
    let mut content = chat_content_to_anthropic(message.get("content"));
    if let Some(reasoning) = message
        .get("reasoning_content")
        .or_else(|| message.get("reasoning"))
        .and_then(Value::as_str)
    {
        if !reasoning.is_empty() {
            content.insert(
                0,
                json!({ "type": "thinking", "thinking": reasoning, "signature": "" }),
            );
        }
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

fn messages_response_to_gemini(body: &Value) -> Result<Value, ProtocolError> {
    let blocks = body
        .get("content")
        .and_then(Value::as_array)
        .ok_or_else(|| ProtocolError::new("Messages response has no content"))?;
    let mut parts = Vec::new();
    for block in blocks {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => {
                if let Some(text) = block.get("text").and_then(Value::as_str) {
                    if !text.is_empty() {
                        parts.push(json!({ "text": text }));
                    }
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
) -> Result<Value, ProtocolError> {
    let blocks = body
        .get("content")
        .and_then(Value::as_array)
        .ok_or_else(|| ProtocolError::new("Messages response has no content"))?;
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

fn responses_response_to_messages(body: &Value) -> Result<Value, ProtocolError> {
    let output = body
        .get("output")
        .and_then(Value::as_array)
        .ok_or_else(|| ProtocolError::new("Responses response has no output"))?;
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
) -> Result<Value, ProtocolError> {
    let blocks = body
        .get("content")
        .and_then(Value::as_array)
        .ok_or_else(|| ProtocolError::new("Messages response has no content"))?;
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
    let created_at = unix_seconds();
    Ok(json!({
        "id": responses_id(response_id),
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

fn message_to_chat(message: &Value) -> Result<Vec<Value>, ProtocolError> {
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
) -> Result<(), ProtocolError> {
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

pub(crate) fn responses_id(id: &str) -> String {
    if id.starts_with("resp_") {
        id.to_string()
    } else if id.is_empty() {
        format!("resp_{}", Uuid::new_v4().simple())
    } else {
        format!("resp_{id}")
    }
}

pub(crate) fn unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
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
    if let Some(rest) = url.strip_prefix("data:") {
        if let Some((media_and_encoding, data)) = rest.split_once(',') {
            let media_type = media_and_encoding
                .strip_suffix(";base64")
                .unwrap_or(media_and_encoding);
            return json!({
                "type": "image",
                "source": { "type": "base64", "media_type": media_type, "data": data }
            });
        }
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
    if let Some(last) = messages.last_mut() {
        if last.get("role").and_then(Value::as_str) == Some(role) {
            if let Some(content) = last.get_mut("content").and_then(Value::as_array_mut) {
                content.extend(blocks);
                return;
            }
        }
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

pub(crate) fn encode_anthropic_thinking_block(block: &Value) -> Option<String> {
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

pub(crate) fn decode_anthropic_thinking_block(encrypted_content: &str) -> Option<Value> {
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

pub(crate) fn encode_chat_reasoning(reasoning: &str) -> Option<String> {
    (!reasoning.is_empty()).then(|| {
        format!(
            "{CHAT_REASONING_ENCRYPTED_PREFIX}{}",
            URL_SAFE_NO_PAD.encode(reasoning.as_bytes())
        )
    })
}

pub(crate) fn decode_chat_reasoning(encrypted_content: &str) -> Option<String> {
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
mod tests {
    use super::*;

    fn bytes(value: Value) -> Bytes {
        Bytes::from(serde_json::to_vec(&value).expect("test JSON should encode"))
    }

    fn plan(client: ApiFormat, upstream: ApiFormat) -> RequestPlan {
        plan_with_model(client, upstream, "test")
    }

    fn plan_with_model(client: ApiFormat, upstream: ApiFormat, model: &str) -> RequestPlan {
        RequestPlan {
            client,
            upstream,
            model: model.into(),
            client_model: model.into(),
            stream: false,
            body: Bytes::new(),
            channel: UpstreamChannel::Go,
            upstream_base_override: None,
            original_model: None,
            allow_go_fallback: false,
            service_tier: None,
            custom_tools: Vec::new(),
            namespace_tools: Vec::new(),
            response_parallel_tool_calls: true,
            response_tool_choice: json!("auto"),
            response_tools: Vec::new(),
        }
    }

    #[test]
    fn multi_protocol_models_passthrough_supported_formats() {
        // deepseek-v4-flash: 2026-08-14 probe accepts Chat, Responses, and Messages.
        let flash_responses = prepare_request(
            ApiFormat::Responses,
            bytes(json!({
                "model": "deepseek-v4-flash",
                "input": "hi",
                "store": false
            })),
        )
        .expect("flash passthroughs Responses");
        assert_eq!(flash_responses.upstream, ApiFormat::Responses);

        let glm_responses = prepare_request(
            ApiFormat::Responses,
            bytes(json!({
                "model": "glm-5.2",
                "input": "hi",
                "store": false
            })),
        )
        .unwrap();
        assert_eq!(glm_responses.upstream, ApiFormat::Responses);

        let glm_messages = prepare_request(
            ApiFormat::Messages,
            bytes(json!({
                "model": "glm-5.2",
                "max_tokens": 8,
                "messages": [{"role": "user", "content": "hi"}]
            })),
        )
        .unwrap();
        assert_eq!(glm_messages.upstream, ApiFormat::Messages);

        let minimax_chat = prepare_request(
            ApiFormat::ChatCompletions,
            bytes(json!({
                "model": "minimax-m3",
                "messages": [{"role": "user", "content": "hi"}]
            })),
        )
        .unwrap();
        assert_eq!(minimax_chat.upstream, ApiFormat::ChatCompletions);

        let minimax_messages = prepare_request(
            ApiFormat::Messages,
            bytes(json!({
                "model": "minimax-m3",
                "max_tokens": 8,
                "messages": [{"role": "user", "content": "hi"}]
            })),
        )
        .unwrap();
        assert_eq!(minimax_messages.upstream, ApiFormat::Messages);

        let luna_chat = prepare_request(
            ApiFormat::ChatCompletions,
            bytes(json!({
                "model": "gpt-5.6-luna",
                "messages": [{"role": "user", "content": "hi"}]
            })),
        )
        .unwrap();
        assert_eq!(luna_chat.upstream, ApiFormat::ChatCompletions);

        let luna_responses = prepare_request(
            ApiFormat::Responses,
            bytes(json!({
                "model": "gpt-5.6-luna",
                "input": "hi",
                "store": false
            })),
        )
        .unwrap();
        assert_eq!(luna_responses.upstream, ApiFormat::Responses);

        let grok_responses = prepare_request(
            ApiFormat::Responses,
            bytes(json!({
                "model": "grok-4.5",
                "input": "hi",
                "store": false
            })),
        )
        .unwrap();
        assert_eq!(grok_responses.upstream, ApiFormat::Responses);
    }

    #[test]
    fn grok_converts_chat_and_messages_to_official_responses() {
        let chat = prepare_request(
            ApiFormat::ChatCompletions,
            bytes(json!({
                "model": "grok-4.5",
                "messages": [{"role": "user", "content": "hi"}]
            })),
        )
        .unwrap();
        assert_eq!(chat.upstream, ApiFormat::Responses);

        let messages = prepare_request(
            ApiFormat::Messages,
            bytes(json!({
                "model": "grok-4.5",
                "messages": [{"role": "user", "content": "hi"}],
                "max_tokens": 8
            })),
        )
        .unwrap();
        assert_eq!(messages.upstream, ApiFormat::Responses);
    }

    #[test]
    fn kimi_k3_passthroughs_chat_and_messages() {
        let chat = prepare_request(
            ApiFormat::ChatCompletions,
            bytes(json!({
                "model": "kimi-k3",
                "messages": [{"role": "user", "content": "hi"}]
            })),
        )
        .unwrap();
        assert_eq!(chat.upstream, ApiFormat::ChatCompletions);

        let messages = prepare_request(
            ApiFormat::Messages,
            bytes(json!({
                "model": "kimi-k3",
                "messages": [{"role": "user", "content": "hi"}],
                "max_tokens": 8
            })),
        )
        .unwrap();
        assert_eq!(messages.upstream, ApiFormat::Messages);
    }

    #[test]
    fn minimax_highspeed_models_route_as_messages_and_preserve_priority_tier() {
        let plan = prepare_request(
            ApiFormat::Messages,
            bytes(json!({
                "model": "minimax-m2.7-highspeed",
                "messages": [{"role": "user", "content": "hi"}],
                "max_tokens": 8,
                "service_tier": "priority"
            })),
        )
        .expect("highspeed model should be routable");
        assert_eq!(plan.upstream, ApiFormat::Messages);
        assert_eq!(plan.service_tier.as_deref(), Some("priority"));
    }

    #[test]
    fn service_tier_preserves_string_values_and_ignores_non_string_values() {
        // MiniMax accepts Chat natively; force Messages conversion via Responses client.
        let plan = prepare_request(
            ApiFormat::Responses,
            bytes(json!({
                "model": "minimax-m3",
                "input": "hi",
                "store": false,
                "service_tier": "priority"
            })),
        )
        .expect("Responses request should convert to Messages");
        let body: Value = serde_json::from_slice(&plan.body).expect("body is JSON");
        assert_eq!(plan.upstream, ApiFormat::Messages);
        assert_eq!(body["service_tier"], "priority");
        assert_eq!(plan.service_tier.as_deref(), Some("priority"));

        let ignored = prepare_request(
            ApiFormat::Responses,
            bytes(json!({
                "model": "minimax-m3",
                "store": false,
                "input": "hi",
                "service_tier": {"tier": "priority"}
            })),
        )
        .expect("non-string service tier should retain compatibility");
        let body: Value = serde_json::from_slice(&ignored.body).expect("body is JSON");
        assert!(body.get("service_tier").is_none());
        assert_eq!(ignored.service_tier, None);

        let chat = prepare_request(
            ApiFormat::ChatCompletions,
            bytes(json!({
                "model": "minimax-m3",
                "messages": [{"role": "user", "content": "hi"}],
                "service_tier": "priority"
            })),
        )
        .expect("Chat request should passthrough");
        assert_eq!(chat.upstream, ApiFormat::ChatCompletions);
        let chat_body: Value = serde_json::from_slice(&chat.body).expect("body is JSON");
        assert_eq!(chat_body["service_tier"], "priority");
        assert!(chat_body.get("stream_options").is_none());
    }

    #[test]
    fn chat_passthrough_stream_requests_include_usage() {
        let plan = prepare_request(
            ApiFormat::ChatCompletions,
            bytes(json!({
                "model": "deepseek-v4-flash",
                "stream": true,
                "messages": [{"role": "user", "content": "hi"}]
            })),
        )
        .expect("Chat stream should passthrough");
        assert_eq!(plan.upstream, ApiFormat::ChatCompletions);
        let body: Value = serde_json::from_slice(&plan.body).expect("body is JSON");
        assert_eq!(body["stream_options"]["include_usage"], true);
        assert_eq!(body["model"], "deepseek-v4-flash");
        assert_eq!(body["messages"][0]["content"], "hi");
    }

    #[test]
    fn gemini_request_converts_text_image_tools_and_json_schema_to_messages() {
        let request = json!({
            "systemInstruction":{"parts":[{"text":"Be concise."}]},
            "contents":[
                {"role":"user","parts":[
                    {"text":"Read this image."},
                    {"inlineData":{"mimeType":"image/png","data":"aGVsbG8="}}
                ]},
                {"role":"model","parts":[
                    {"functionCall":{"id":"call_1","name":"read_file","args":{"path":"Cargo.toml"}}}
                ]},
                {"role":"user","parts":[
                    {"functionResponse":{"id":"call_1","name":"read_file","response":{"output":"ok"}}}
                ]}
            ],
            "tools":[{"functionDeclarations":[{
                "name":"read_file","description":"Read a file",
                "parametersJsonSchema":{"type":"object","properties":{"path":{"type":"string"}},"required":["path"]}
            }]}],
            "toolConfig":{"functionCallingConfig":{"mode":"ANY","allowedFunctionNames":["read_file"]}},
            "generationConfig":{
                "maxOutputTokens":512,"temperature":0.2,"topP":0.95,
                "stopSequences":["<END>"],"responseMimeType":"application/json",
                "responseJsonSchema":{"type":"object","properties":{"answer":{"type":"string"}},"required":["answer"]}
            }
        });
        let plan = prepare_gemini_request("minimax-m3".into(), false, bytes(request))
            .expect("Gemini request should convert");
        assert_eq!(plan.client, ApiFormat::Gemini);
        assert_eq!(plan.upstream, ApiFormat::Messages);
        let body: Value = serde_json::from_slice(&plan.body).unwrap();
        assert_eq!(body["model"], "minimax-m3");
        assert_eq!(body["system"], "Be concise.");
        assert_eq!(
            body["messages"][0]["content"][0]["text"],
            "Read this image."
        );
        assert_eq!(body["messages"][0]["content"][1]["type"], "image");
        assert_eq!(body["messages"][1]["content"][0]["id"], "call_1");
        assert_eq!(body["messages"][2]["content"][0]["tool_use_id"], "call_1");
        assert_eq!(
            body["messages"][2]["content"][0]["content"],
            "{\"output\":\"ok\"}"
        );
        assert_eq!(body["tools"][0]["input_schema"]["required"][0], "path");
        assert_eq!(body["tool_choice"]["type"], "tool");
        assert_eq!(body["tool_choice"]["name"], "read_file");
        assert_eq!(body["max_tokens"], 512);
        assert_eq!(body["output_config"]["format"]["type"], "json_schema");
    }

    #[test]
    fn gemini_request_converts_to_chat_and_preserves_structured_output() {
        let plan = prepare_gemini_request(
            "deepseek-v4-flash".into(),
            true,
            bytes(json!({
                "contents":[{"role":"user","parts":[
                    {"text":"describe"},
                    {"inlineData":{"mimeType":"image/jpeg","data":"aGVsbG8="}}
                ]}],
                "generationConfig":{
                    "responseMimeType":"application/json",
                    "responseJsonSchema":{"type":"object","properties":{"answer":{"type":"string"}}}
                }
            })),
        )
        .expect("Gemini chat-native request should convert");
        assert_eq!(plan.upstream, ApiFormat::ChatCompletions);
        assert!(plan.stream);
        let body: Value = serde_json::from_slice(&plan.body).unwrap();
        assert_eq!(body["messages"][0]["content"][0]["type"], "text");
        assert!(
            body["messages"][0]["content"][1]["image_url"]["url"]
                .as_str()
                .unwrap()
                .starts_with("data:image/jpeg;base64,")
        );
        assert_eq!(body["response_format"]["type"], "json_schema");
        assert_eq!(body["stream_options"]["include_usage"], true);
    }

    #[test]
    fn gemini_response_converts_text_tools_finish_and_usage() {
        let response = transform_between(
            ApiFormat::Messages,
            ApiFormat::Gemini,
            &json!({
                "id":"msg_1","model":"minimax-m3","stop_reason":"tool_use",
                "content":[
                    {"type":"text","text":"Checking."},
                    {"type":"tool_use","id":"call_2","name":"read_file","input":{"path":"Cargo.toml"}}
                ],
                "usage":{"input_tokens":10,"cache_read_input_tokens":2,"output_tokens":3}
            }),
        )
        .expect("Messages response should convert to Gemini");
        assert_eq!(response["candidates"][0]["finishReason"], "STOP");
        assert_eq!(
            response["candidates"][0]["content"]["parts"][0]["text"],
            "Checking."
        );
        assert_eq!(
            response["candidates"][0]["content"]["parts"][1]["functionCall"]["id"],
            "call_2"
        );
        assert_eq!(response["usageMetadata"]["promptTokenCount"], 12);
        assert_eq!(response["usageMetadata"]["candidatesTokenCount"], 3);
        assert_eq!(response["usageMetadata"]["totalTokenCount"], 15);
        assert_eq!(response["responseId"], "msg_1");
    }

    #[test]
    fn gemini_rejects_unknown_models_and_unsupported_features() {
        let unknown = prepare_gemini_request(
            "gemini-3-pro-preview".into(),
            false,
            bytes(json!({"contents":[{"role":"user","parts":[{"text":"hi"}]}]})),
        )
        .expect_err("Gemini cannot become an upstream protocol");
        assert!(unknown.message.contains("unknown model"));

        let cases = [
            json!({"contents":[{"role":"user","parts":[{"fileData":{"mimeType":"image/png","fileUri":"x"}}]}]}),
            json!({"contents":[{"role":"user","parts":[{"inlineData":{"mimeType":"image/svg+xml","data":"aGVsbG8="}}]}]}),
            json!({"contents":[{"role":"user","parts":[{"inlineData":{"mimeType":"image/png","data":"not base64"}}]}]}),
            json!({"contents":[{"role":"user","parts":[{"text":"hi"}]}],"tools":[{"googleSearch":{}}]}),
            json!({"contents":[{"role":"user","parts":[{"text":"hi"}]}],"tools":[{"urlContext":{}}]}),
            json!({"contents":[{"role":"user","parts":[{"text":"hi"}]}],"tools":[{"functionDeclarations":[{"name":"x","parameters":{},"parametersJsonSchema":{}}]}]}),
            json!({"contents":[{"role":"user","parts":[{"text":"hi"}]}],"toolConfig":{"functionCallingConfig":{"mode":"VALIDATED"}}}),
            json!({"contents":[{"role":"user","parts":[{"text":"hi"}]}],"cachedContent":"cachedContents/1"}),
            json!({"contents":[{"role":"user","parts":[{"text":"hi"}]}],"safetySettings":{}}),
            json!({"contents":[{"role":"user","parts":[{"text":"hi"}]}],"generationConfig":{"seed":7}}),
            json!({"contents":[{"role":"user","parts":[{"text":"hi"}]}],"generationConfig":{"presencePenalty":0.5}}),
            json!({"contents":[{"role":"user","parts":[{"text":"hi"}]}],"generationConfig":{"frequencyPenalty":0.5}}),
            json!({"contents":[{"role":"user","parts":[{"text":"hi"}]}],"generationConfig":{"responseLogprobs":true}}),
            json!({"contents":[{"role":"user","parts":[{"text":"hi"}]}],"generationConfig":{"logprobs":4}}),
            json!({"contents":[{"role":"user","parts":[{"text":"hi"}]}],"generationConfig":{"mediaResolution":"MEDIA_RESOLUTION_HIGH"}}),
            json!({"contents":[{"role":"user","parts":[{"text":"hi"}]}],"generationConfig":{"topK":"64"}}),
            json!({"contents":[{"role":"user","parts":[{"text":"hi"}]}],"generationConfig":{"thinkingConfig":true}}),
            json!({"contents":[{"role":"user","parts":[{"text":"hi"}]}],"generationConfig":{"candidateCount":"1"}}),
            json!({"contents":[{"role":"user","parts":[{"text":"hi"}]}],"generationConfig":{"responseModalities":"TEXT"}}),
            json!({"contents":[{"role":"user","parts":[{"text":"hi"}]}],"generationConfig":{"responseMimeType":1}}),
        ];
        for request in cases {
            assert!(prepare_gemini_request("minimax-m3".into(), false, bytes(request)).is_err());
        }

        let empty_safety_settings = json!({
            "contents":[{"role":"user","parts":[{"text":"hi"}]}],
            "safetySettings":[]
        });
        prepare_gemini_request("minimax-m3".into(), false, bytes(empty_safety_settings))
            .expect("empty Gemini safety settings do not change semantics");

        let safety_error = prepare_gemini_request(
            "minimax-m3".into(),
            false,
            bytes(json!({
                "contents":[{"role":"user","parts":[{"text":"hi"}]}],
                "safetySettings":[{
                    "category":"HARM_CATEGORY_HATE_SPEECH",
                    "threshold":"BLOCK_LOW_AND_ABOVE"
                }]
            })),
        )
        .expect_err("non-empty Gemini safety settings cannot be silently discarded");
        assert_eq!(safety_error.status, StatusCode::BAD_REQUEST);
        assert!(safety_error.message.contains("cannot be preserved"));
    }

    #[test]
    fn gemini_cli_generation_hints_are_accepted_but_not_leaked_upstream() {
        let plan = prepare_gemini_request(
            "minimax-m3".into(),
            false,
            bytes(json!({
                "contents":[{"role":"user","parts":[{"text":"hi"}]}],
                "generationConfig":{
                    "temperature":1,
                    "topP":0.95,
                    "topK":64,
                    "thinkingConfig":{"includeThoughts":true}
                }
            })),
        )
        .expect("Gemini CLI defaults must remain compatible");
        let body: Value = serde_json::from_slice(&plan.body).unwrap();
        assert_eq!(body["temperature"], 1);
        assert_eq!(body["top_p"], 0.95);
        assert!(body.get("topK").is_none());
        assert!(body.get("top_k").is_none());
        assert!(body.get("thinkingConfig").is_none());
        assert!(body.get("thinking").is_none());
    }

    #[test]
    fn gemini_error_and_usage_use_google_envelopes() {
        let body = format_error(
            ApiFormat::Gemini,
            StatusCode::UNAUTHORIZED,
            "invalid gateway key",
            None,
        );
        assert_eq!(body["error"]["code"], 401);
        assert_eq!(body["error"]["status"], "UNAUTHENTICATED");
        assert_eq!(
            extract_usage(
                ApiFormat::Gemini,
                &json!({"usageMetadata":{"promptTokenCount":9,"candidatesTokenCount":3,"cachedContentTokenCount":2}}),
                None,
            ),
            UsageCounts {
                input_tokens: 9,
                output_tokens: 3,
                cached_tokens: 2,
                cache_creation_tokens: 0
            }
        );
    }

    #[test]
    fn messages_request_routes_chat_only_model_with_tools_and_usage() {
        let request = json!({
            "model": "hy3",
            "stream": true,
            "system": [{"type":"text","text":"be terse"}],
            "messages": [
                {"role":"assistant","content":[
                    {"type":"thinking","thinking":"reason"},
                    {"type":"tool_use","id":"call_1","name":"read","input":{"path":"a"}}
                ]},
                {"role":"user","content":[
                    {"type":"tool_result","tool_use_id":"call_1","content":"ok"},
                    {"type":"text","text":"continue"}
                ]}
            ],
            "tools": [{"name":"read","description":"read file","input_schema":{"type":"object"}}],
            "tool_choice": {"type":"any"},
            "thinking": {"type":"enabled","budget_tokens":4096}
        });
        let plan = prepare_request(ApiFormat::Messages, bytes(request)).expect("request converts");
        assert_eq!(plan.upstream, ApiFormat::ChatCompletions);
        assert!(plan.stream);
        let body: Value = serde_json::from_slice(&plan.body).expect("body is JSON");
        assert_eq!(
            body["messages"][0],
            json!({"role":"system","content":"be terse"})
        );
        assert_eq!(
            body["messages"][1]["tool_calls"][0]["function"]["name"],
            "read"
        );
        assert_eq!(body["messages"][1]["reasoning_content"], "reason");
        assert_eq!(body["messages"][2]["role"], "tool");
        assert_eq!(body["tool_choice"], "required");
        assert_eq!(body["reasoning_effort"], "medium");
        assert_eq!(body["stream_options"]["include_usage"], true);
    }

    #[test]
    fn responses_no_reasoning_maps_to_chat_thinking_disabled() {
        let plan = prepare_request(
            ApiFormat::Responses,
            bytes(json!({
                "model":"hy3",
                "input":"hello",
                "store":false,
                "reasoning":{"effort":"none"}
            })),
        )
        .expect("Responses request converts");
        assert_eq!(plan.upstream, ApiFormat::ChatCompletions);
        let body: Value = serde_json::from_slice(&plan.body).unwrap();
        assert_eq!(body["thinking"]["type"], "disabled");
        assert!(body.get("reasoning_effort").is_none());
    }

    #[test]
    fn responses_requires_explicit_store_false_and_rejects_stateful_async_fields() {
        for store in [None, Some(Value::Null), Some(json!(true))] {
            let mut request = json!({"model":"minimax-m2.7","input":"hi"});
            if let Some(store) = store {
                request["store"] = store;
            }
            let error = prepare_request(ApiFormat::Responses, bytes(request))
                .expect_err("store must be explicitly false");
            assert!(error.message.contains("requires Responses store=false"));
        }

        for (field, value) in [
            ("previous_response_id", json!("resp_previous")),
            ("conversation", json!("conv_1")),
            ("background", json!(true)),
        ] {
            let mut request = json!({"model":"minimax-m2.7","input":"hi","store":false});
            request[field] = value;
            let error = prepare_request(ApiFormat::Responses, bytes(request))
                .expect_err("unsupported Responses state must fail");
            assert_eq!(error.status, StatusCode::BAD_REQUEST);
            assert!(error.message.contains(field), "{}", error.message);
        }

        prepare_request(
            ApiFormat::Responses,
            bytes(json!({
                "model":"minimax-m2.7","input":"hi","store":false,"background":false
            })),
        )
        .expect("explicit stateless flags are supported");
    }

    #[test]
    fn cross_protocol_structured_formats_are_rejected() {
        let cases = [
            (
                ApiFormat::Responses,
                json!({
                    "model":"minimax-m2.7","input":"hi","store":false,
                    "text":{"format":{"type":"json_schema","name":"answer","schema":{"type":"object"}}}
                }),
                "text.format",
            ),
            // Chat is native for minimax; use Responses→Chat conversion instead.
            (
                ApiFormat::Responses,
                json!({
                    "model":"hy3","input":"hi","store":false,
                    "text":{"format":{"type":"json_schema","name":"answer","schema":{"type":"object"}}}
                }),
                "text.format",
            ),
            (
                ApiFormat::Messages,
                json!({
                    "model":"hy3","messages":[{"role":"user","content":"hi"}],
                    "output_config":{"format":{"type":"json_schema","schema":{"type":"object"}}}
                }),
                "output_config.format",
            ),
            (
                ApiFormat::Responses,
                json!({
                    "model":"minimax-m2.7","input":"hi","store":false,
                    "tools":[{"type":"custom","name":"patch","format":{"type":"grammar","syntax":"lark","definition":"start: /.+/"}}]
                }),
                "grammar format",
            ),
        ];
        for (format, request, field) in cases {
            let error = prepare_request(format, bytes(request))
                .expect_err("structured conversion must not silently downgrade");
            assert!(error.message.contains(field), "{}", error.message);
        }

        // Same-protocol structured formats may passthrough (not conversion).
        prepare_request(
            ApiFormat::ChatCompletions,
            bytes(json!({
                "model":"minimax-m2.7",
                "messages":[{"role":"user","content":"hi"}],
                "response_format":{"type":"json_schema","json_schema":{"name":"answer","schema":{"type":"object"}}}
            })),
        )
        .expect("Chat-native structured format may passthrough");

        prepare_request(
            ApiFormat::Responses,
            bytes(json!({
                "model":"minimax-m2.7","input":"hi","store":false,
                "text":{"format":{"type":"text"}}
            })),
        )
        .expect("explicit plain text remains convertible");
    }

    #[test]
    fn responses_input_image_file_id_is_rejected() {
        let error = prepare_request(
            ApiFormat::Responses,
            bytes(json!({
                "model":"minimax-m2.7",
                "store":false,
                "input":[{"role":"user","content":[{"type":"input_image","file_id":"file_1"}]}]
            })),
        )
        .expect_err("file-backed images require Files API support");
        assert_eq!(error.status, StatusCode::BAD_REQUEST);
        assert!(error.message.contains("input_image.file_id"));
    }

    #[test]
    fn chat_native_tool_history_backfills_nonempty_reasoning() {
        let requests = [
            (
                ApiFormat::Messages,
                json!({
                    "model":"hy3",
                    "messages":[
                        {"role":"assistant","content":[{"type":"tool_use","id":"c1","name":"f","input":{}}]},
                        {"role":"user","content":[{"type":"tool_result","tool_use_id":"c1","content":"ok"}]}
                    ]
                }),
            ),
            (
                ApiFormat::Responses,
                json!({
                    "model":"hy3",
                    "store":false,
                    "input":[
                        {"type":"function_call","call_id":"c1","name":"f","arguments":"{}"},
                        {"type":"function_call_output","call_id":"c1","output":"ok"}
                    ]
                }),
            ),
        ];

        for (format, request) in requests {
            let plan = prepare_request(format, bytes(request)).expect("request converts");
            let body: Value = serde_json::from_slice(&plan.body).expect("body is JSON");
            let assistant = body["messages"]
                .as_array()
                .unwrap()
                .iter()
                .find(|message| message.get("tool_calls").is_some())
                .expect("assistant tool call exists");
            assert_eq!(
                assistant["reasoning_content"], CHAT_TOOL_REASONING_PLACEHOLDER,
                "{format:?}"
            );
        }
    }

    #[test]
    fn chat_request_routes_minimax_to_messages_with_image_and_tool_result() {
        // Live probe: MiniMax accepts Chat natively, so Chat client passthroughs.
        let chat = prepare_request(
            ApiFormat::ChatCompletions,
            bytes(json!({
                "model": "minimax-m2.7",
                "max_tokens": 5000,
                "messages": [{"role":"user","content":"hi"}]
            })),
        )
        .expect("Chat request passthroughs");
        assert_eq!(chat.upstream, ApiFormat::ChatCompletions);

        // Conversion to preferred Messages still runs for unsupported client formats.
        let request = json!({
            "model": "minimax-m2.7",
            "store": false,
            "max_output_tokens": 5000,
            "instructions": "system",
            "input": [
                {"type":"message","role":"user","content":[
                    {"type":"input_text","text":"look"},
                    {"type":"input_image","image_url":"data:image/png;base64,abc"}
                ]},
                {"type":"function_call","call_id":"c1","name":"f","arguments":"{\"x\":1}"},
                {"type":"function_call_output","call_id":"c1","output":"done"}
            ],
            "tools": [{"type":"function","name":"f","parameters":{"type":"object"}}],
            "tool_choice": {"type":"function","name":"f"}
        });
        let plan = prepare_request(ApiFormat::Responses, bytes(request)).expect("request converts");
        assert_eq!(plan.upstream, ApiFormat::Messages);
        let body: Value = serde_json::from_slice(&plan.body).expect("body is JSON");
        assert_eq!(body["system"], "system");
        assert_eq!(
            body["messages"][0]["content"][1]["source"]["media_type"],
            "image/png"
        );
        assert_eq!(body["messages"][1]["content"][0]["type"], "tool_use");
        assert_eq!(body["messages"][1]["content"][0]["input"]["x"], 1);
        assert_eq!(body["messages"][2]["content"][0]["type"], "tool_result");
        assert_eq!(body["tool_choice"], json!({"type":"tool","name":"f"}));
    }

    #[test]
    fn responses_request_routes_known_model_and_unknown_is_rejected() {
        let request = json!({
            "model":"hy3",
            "store":false,
            "instructions":"system",
            "input":[
                {"type":"message","role":"user","content":[{"type":"input_text","text":"hi"}]},
                {"type":"function_call","call_id":"c","name":"f","arguments":"{}"},
                {"type":"function_call_output","call_id":"c","output":"ok"}
            ],
            "tools":[{"type":"function","name":"f","parameters":{"type":"object"}}]
        });
        let plan =
            prepare_request(ApiFormat::Responses, bytes(request)).expect("known model routes");
        assert_eq!(plan.upstream, ApiFormat::ChatCompletions);
        let body: Value = serde_json::from_slice(&plan.body).expect("body is JSON");
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(
            body["messages"][2]["tool_calls"][0]["function"]["name"],
            "f"
        );
        assert_eq!(body["messages"][3]["role"], "tool");

        let error = prepare_request(
            ApiFormat::Responses,
            bytes(json!({"model":"unknown","input":"hi","store":false})),
        )
        .expect_err("unknown Responses model must not guess a protocol");
        assert_eq!(error.status, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn unknown_chat_and_messages_models_fail_closed() {
        let chat = prepare_request(
            ApiFormat::ChatCompletions,
            bytes(json!({
                "model":"custom",
                "messages":[{"role":"assistant","content":null,"tool_calls":[{"id":"c1","type":"function","function":{"name":"f","arguments":"{}"}}]}]
            })),
        )
        .expect_err("unknown Chat must not stay native");
        let messages = prepare_request(
            ApiFormat::Messages,
            bytes(json!({"model":"custom","max_tokens":1,"messages":[]})),
        )
        .expect_err("unknown Messages must not stay native");
        assert_eq!(chat.status, StatusCode::BAD_REQUEST);
        assert!(chat.message.contains("unknown model"));
        assert_eq!(messages.status, StatusCode::BAD_REQUEST);
        assert!(messages.message.contains("unknown model"));
    }

    #[test]
    fn messages_upstream_moves_system_roles_to_top_level() {
        let native = prepare_request(
            ApiFormat::Messages,
            bytes(json!({
                "model":"minimax-m2.7",
                "max_tokens":128,
                "system":[{"type":"text","text":"existing","cache_control":{"type":"ephemeral"}}],
                "messages":[
                    {"role":"system","content":[{"type":"text","text":"system role","cache_control":{"type":"ephemeral"}}]},
                    {"role":"developer","content":"developer role"},
                    {"role":"user","content":"hello"}
                ]
            })),
        )
        .expect("native Messages request normalizes");
        let body: Value = serde_json::from_slice(&native.body).expect("body is JSON");
        assert_eq!(body["messages"].as_array().unwrap().len(), 1);
        assert_eq!(body["messages"][0]["role"], "user");
        assert_eq!(body["system"].as_array().unwrap().len(), 3);
        assert_eq!(body["system"][0]["cache_control"]["type"], "ephemeral");
        assert_eq!(body["system"][1]["text"], "system role");
        assert_eq!(body["system"][2]["text"], "developer role");

        let responses = prepare_request(
            ApiFormat::Responses,
            bytes(json!({
                "model":"minimax-m2.7",
                "store":false,
                "instructions":"instructions",
                "input":[
                    {"type":"message","role":"developer","content":[{"type":"input_text","text":"dev"}]},
                    {"type":"message","role":"user","content":[{"type":"input_text","text":"hello"}]}
                ]
            })),
        )
        .expect("Responses request normalizes");
        let body: Value = serde_json::from_slice(&responses.body).expect("body is JSON");
        assert_eq!(body["messages"].as_array().unwrap().len(), 1);
        assert_eq!(body["messages"][0]["role"], "user");
        assert_eq!(body["system"][0]["text"], "instructions");
        assert_eq!(body["system"][1]["text"], "dev");
    }

    #[test]
    fn chat_upstream_converts_developer_role_to_system() {
        // Responses 与 Messages 客户端入口共享 message_to_chat 转换链，
        // developer 指令角色都必须归一化为 system，禁止透传 developer。
        for (client, payload) in [
            (
                ApiFormat::Responses,
                json!({
                    "model":"hy3",
                    "store":false,
                    "instructions":"instructions",
                    "input":[
                        {"type":"message","role":"developer","content":[{"type":"input_text","text":"dev"}]},
                        {"type":"message","role":"user","content":[{"type":"input_text","text":"hello"}]}
                    ]
                }),
            ),
            (
                ApiFormat::Messages,
                json!({
                    "model":"hy3",
                    "messages":[
                        {"role":"developer","content":"dev"},
                        {"role":"user","content":"hello"}
                    ]
                }),
            ),
        ] {
            let plan = prepare_request(client, bytes(payload)).unwrap_or_else(|_| {
                panic!("{client:?} request to Chat upstream normalizes developer role")
            });
            assert_eq!(plan.upstream, ApiFormat::ChatCompletions);
            let body: Value = serde_json::from_slice(&plan.body).expect("body is JSON");
            let messages = body["messages"]
                .as_array()
                .expect("chat messages is an array");
            assert!(
                messages.iter().all(|m| m["role"] != "developer"),
                "{client:?}: chat upstream must not carry developer role"
            );
            assert!(
                messages
                    .iter()
                    .any(|m| m["role"] == "system" && m["content"] == "dev"),
                "{client:?}: developer message should become a system message"
            );
            assert!(
                messages
                    .iter()
                    .any(|m| m["role"] == "user" && m["content"] == "hello"),
                "{client:?}: user message must be preserved"
            );
        }
    }

    #[test]
    fn messages_upstream_drops_unsigned_thinking_history() {
        let plan = prepare_request(
            ApiFormat::Messages,
            bytes(json!({
                "model":"minimax-m2.7",
                "messages":[
                    {"role":"assistant","content":[
                        {"type":"thinking","thinking":"from chat","signature":""},
                        {"type":"thinking","thinking":"native","signature":"sig_123"},
                        {"type":"redacted_thinking","data":"opaque"},
                        {"type":"tool_use","id":"c1","name":"f","input":{}}
                    ]},
                    {"role":"user","content":[{"type":"tool_result","tool_use_id":"c1","content":"ok"}]}
                ]
            })),
        )
        .expect("native Messages history normalizes");
        let body: Value = serde_json::from_slice(&plan.body).unwrap();
        let content = body["messages"][0]["content"].as_array().unwrap();
        assert_eq!(content.len(), 3);
        assert_eq!(content[0]["thinking"], "native");
        assert_eq!(content[0]["signature"], "sig_123");
        assert_eq!(content[1]["type"], "redacted_thinking");
        assert_eq!(content[2]["type"], "tool_use");
    }

    #[test]
    fn chat_response_maps_reasoning_tools_stop_and_cache_usage_to_messages() {
        let response = json!({
            "id":"chat1","model":"deepseek-v4-flash",
            "choices":[{"message":{
                "role":"assistant","content":"answer","reasoning":"reason",
                "tool_calls":[{"id":"c1","type":"function","function":{"name":"f","arguments":"{\"x\":1}"}}]
            },"finish_reason":"tool_calls"}],
            "usage":{"prompt_tokens":10,"completion_tokens":2,"prompt_tokens_details":{"cached_tokens":4}}
        });
        let converted = transform_response(
            &plan(ApiFormat::Messages, ApiFormat::ChatCompletions),
            &response,
        )
        .expect("response converts");
        assert_eq!(converted["content"][0]["type"], "thinking");
        assert_eq!(converted["content"][0]["signature"], "");
        assert_eq!(converted["content"][2]["input"]["x"], 1);
        assert_eq!(converted["stop_reason"], "tool_use");
        assert_eq!(converted["usage"]["input_tokens"], 6);
        assert_eq!(converted["usage"]["cache_read_input_tokens"], 4);
    }

    #[test]
    fn messages_response_maps_reasoning_tools_and_usage_to_both_openai_formats() {
        let response = json!({
            "id":"m1","model":"minimax-m2.7",
            "content":[
                {"type":"thinking","thinking":"reason","signature":"sig_123"},
                {"type":"text","text":"answer"},
                {"type":"tool_use","id":"c1","name":"f","input":{"x":1}}
            ],
            "stop_reason":"tool_use",
            "usage":{"input_tokens":6,"output_tokens":2,"cache_read_input_tokens":4}
        });
        let chat = transform_response(
            &plan(ApiFormat::ChatCompletions, ApiFormat::Messages),
            &response,
        )
        .expect("Messages to Chat");
        assert_eq!(chat["choices"][0]["message"]["reasoning_content"], "reason");
        assert_eq!(chat["choices"][0]["finish_reason"], "tool_calls");
        assert_eq!(chat["usage"]["prompt_tokens"], 10);

        let responses =
            transform_response(&plan(ApiFormat::Responses, ApiFormat::Messages), &response)
                .expect("Messages to Responses");
        assert_eq!(responses["status"], "completed");
        let reasoning = responses["output"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["type"] == "reasoning")
            .unwrap();
        assert_eq!(
            decode_anthropic_thinking_block(reasoning["encrypted_content"].as_str().unwrap())
                .unwrap()["signature"],
            "sig_123"
        );
        assert!(
            responses["output"]
                .as_array()
                .expect("output array")
                .iter()
                .any(|item| item["type"] == "function_call")
        );
        assert_eq!(responses["usage"]["input_tokens"], 10);
    }

    #[test]
    fn minimax_bogus_all_cache_usage_is_sanitized() {
        let mut usage = json!({"input_tokens":0,"output_tokens":5,"cache_read_input_tokens":40500});
        sanitize_minimax_anthropic_usage(Some("minimax-m3"), None, &mut usage);
        assert_eq!(usage["input_tokens"], 40500);
        assert_eq!(usage["cache_read_input_tokens"], 0);

        // Normal MiniMax usage (new input + cache read) is left untouched.
        let mut usage =
            json!({"input_tokens":108,"output_tokens":91,"cache_read_input_tokens":14813});
        sanitize_minimax_anthropic_usage(Some("minimax-m3"), None, &mut usage);
        assert_eq!(usage["input_tokens"], 108);
        assert_eq!(usage["cache_read_input_tokens"], 14813);

        // The heuristic only applies to MiniMax models.
        let mut usage = json!({"input_tokens":0,"output_tokens":5,"cache_read_input_tokens":40500});
        sanitize_minimax_anthropic_usage(Some("qwen3.7-max"), None, &mut usage);
        assert_eq!(usage["cache_read_input_tokens"], 40500);

        // OpenCode Go may return a non-MiniMax model identifier while the request plan still
        // points to MiniMax. The hint must still trigger sanitization.
        let mut usage = json!({"input_tokens":0,"output_tokens":5,"cache_read_input_tokens":40500});
        sanitize_minimax_anthropic_usage(Some("ocg-generic"), Some("minimax-m3"), &mut usage);
        assert_eq!(usage["input_tokens"], 40500);
        assert_eq!(usage["cache_read_input_tokens"], 0);
    }

    #[test]
    fn messages_response_to_chat_sanitizes_minimax_bogus_all_cache() {
        // OpenCode Go's Anthropic-compatible endpoint sometimes returns a rewritten model
        // id or omits the field entirely; the request plan's model must still trigger
        // sanitization either way.
        for model_field in [Some("minimax-m3"), None] {
            let mut response = json!({
                "id":"m1",
                "content":[{"type":"text","text":"hi"}],
                "stop_reason":"end_turn",
                "usage":{"input_tokens":0,"output_tokens":5,"cache_read_input_tokens":40500}
            });
            if let Some(model) = model_field {
                response["model"] = json!(model);
            }
            let chat = transform_response(
                &plan_with_model(
                    ApiFormat::ChatCompletions,
                    ApiFormat::Messages,
                    "minimax-m3",
                ),
                &response,
            )
            .expect("Messages to Chat");
            assert_eq!(chat["usage"]["prompt_tokens"], 40500);
            assert_eq!(chat["usage"]["prompt_tokens_details"]["cached_tokens"], 0);
        }
    }

    #[test]
    fn chat_to_messages_minimax_end_to_end_sanitizes_bogus_all_cache() {
        // MiniMax accepts Chat natively; exercise Messages→Chat conversion via a client
        // that still needs preferred Messages (Responses). OpenCode may rewrite the
        // response model to an internal id while returning a bogus all-cache signature.
        let plan = prepare_request(
            ApiFormat::Responses,
            bytes(json!({
                "model": "MiniMax-M3",
                "input": "hi",
                "store": false,
                "max_output_tokens": 8
            })),
        )
        .expect("MiniMax-M3 Responses should route to Messages");
        assert_eq!(plan.upstream, ApiFormat::Messages);

        let upstream_response = json!({
            "id": "msg_1",
            "type": "message",
            "model": "ocg-generic",
            "content": [{"type": "text", "text": "hello"}],
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 0, "output_tokens": 5, "cache_read_input_tokens": 40669}
        });

        let responses =
            transform_response(&plan, &upstream_response).expect("Messages to Responses");
        assert_eq!(responses["usage"]["input_tokens"], 40669);
        assert_eq!(
            responses["usage"]["input_tokens_details"]["cached_tokens"],
            0
        );

        let counts = extract_usage(plan.upstream, &upstream_response, Some(&plan.model));
        assert_eq!(counts.input_tokens, 40669);
        assert_eq!(counts.cached_tokens, 0);
    }

    #[test]
    fn extract_usage_sanitizes_minimax_with_model_hint() {
        let usage = json!({
            "type":"message_start",
            "message":{"usage":{"input_tokens":0,"output_tokens":5,"cache_read_input_tokens":40500}}
        });
        let counts = extract_usage(ApiFormat::Messages, &usage, Some("minimax-m3"));
        assert_eq!(counts.input_tokens, 40500);
        assert_eq!(counts.cached_tokens, 0);

        // When the upstream response contains a non-MiniMax model identifier but the request
        // plan is MiniMax, the hint must still trigger sanitization.
        let usage = json!({
            "type":"message_start",
            "message":{"model":"qwen3.7-max","usage":{"input_tokens":0,"output_tokens":5,"cache_read_input_tokens":40500}}
        });
        let counts = extract_usage(ApiFormat::Messages, &usage, Some("minimax-m3"));
        assert_eq!(counts.input_tokens, 40500);
        assert_eq!(counts.cached_tokens, 0);
    }

    #[test]
    fn extract_usage_sanitizes_minimax_chat_completion_usage() {
        // When a MiniMax model is routed/answered over the OpenAI Chat Completions wire
        // format, the bogus all-cache signature appears as prompt_tokens == cached_tokens.
        let usage = json!({
            "model": "minimax-m3",
            "usage": {
                "prompt_tokens": 40669,
                "completion_tokens": 5,
                "prompt_tokens_details": {"cached_tokens": 40669}
            }
        });
        let counts = extract_usage(ApiFormat::ChatCompletions, &usage, Some("minimax-m3"));
        assert_eq!(counts.input_tokens, 40669);
        assert_eq!(counts.cached_tokens, 0);
    }

    #[test]
    fn transform_response_sanitizes_minimax_chat_completion_passthrough() {
        // If a MiniMax model name does not route to the Anthropic Messages upstream,
        // the response is passed through in Chat Completions format and still needs
        // the bogus all-cache signature removed.
        let response = json!({
            "id": "chatcmpl-1",
            "model": "minimax-m3",
            "choices": [{"index": 0, "message": {"role": "assistant", "content": "hi"}, "finish_reason": "stop"}],
            "usage": {
                "prompt_tokens": 40669,
                "completion_tokens": 5,
                "prompt_tokens_details": {"cached_tokens": 40669}
            }
        });
        let converted = transform_response(
            &plan_with_model(
                ApiFormat::ChatCompletions,
                ApiFormat::ChatCompletions,
                "minimax-m3",
            ),
            &response,
        )
        .expect("Chat Completions passthrough should sanitize");
        assert_eq!(converted["usage"]["prompt_tokens"], 40669);
        assert_eq!(
            converted["usage"]["prompt_tokens_details"]["cached_tokens"],
            0
        );
    }

    #[test]
    fn transform_response_rejects_non_object_messages_without_panicking() {
        let plan = plan_with_model(ApiFormat::Messages, ApiFormat::Messages, "minimax-m3");
        for body in [json!([]), json!("not an object"), json!(42), json!(true)] {
            let error = transform_response(&plan, &body)
                .expect_err("non-object Messages response should be rejected");
            assert_eq!(error.status, StatusCode::BAD_REQUEST);
            assert!(error.message.contains("JSON object"));
        }
    }

    #[test]
    fn transform_response_sanitizes_minimax_messages_for_every_client_format() {
        let response = json!({
            "id": "msg_1",
            "type": "message",
            "role": "assistant",
            "model": "ocg-generic",
            "content": [{"type": "text", "text": "hello"}],
            "stop_reason": "end_turn",
            "stop_sequence": null,
            "usage": {
                "input_tokens": 0,
                "output_tokens": 5,
                "cache_read_input_tokens": 40500
            }
        });

        let messages = transform_response(
            &plan_with_model(ApiFormat::Messages, ApiFormat::Messages, "minimax-m3"),
            &response,
        )
        .expect("Messages passthrough should sanitize");
        assert_eq!(messages["usage"]["input_tokens"], 40500);
        assert_eq!(messages["usage"]["cache_read_input_tokens"], 0);

        let responses = transform_response(
            &plan_with_model(ApiFormat::Responses, ApiFormat::Messages, "minimax-m3"),
            &response,
        )
        .expect("Messages to Responses should sanitize");
        assert_eq!(responses["usage"]["input_tokens"], 40500);
        assert_eq!(
            responses["usage"]["input_tokens_details"]["cached_tokens"],
            0
        );

        let gemini = transform_response(
            &plan_with_model(ApiFormat::Gemini, ApiFormat::Messages, "minimax-m3"),
            &response,
        )
        .expect("Messages to Gemini should sanitize");
        assert_eq!(gemini["usageMetadata"]["promptTokenCount"], 40500);
        assert_eq!(gemini["usageMetadata"]["cachedContentTokenCount"], 0);
    }

    #[test]
    fn minimax_model_detection_is_case_and_separator_insensitive() {
        // OpenCode Go / Qwen Cloud docs expose MiniMax IDs with capital letters (MiniMax-M3).
        // The sanitizer must still recognize them.
        let mut usage = json!({"input_tokens":0,"output_tokens":5,"cache_read_input_tokens":40500});
        sanitize_minimax_anthropic_usage(Some("MiniMax-M3"), None, &mut usage);
        assert_eq!(usage["input_tokens"], 40500);
        assert_eq!(usage["cache_read_input_tokens"], 0);

        let mut usage = json!({"input_tokens":0,"output_tokens":5,"cache_read_input_tokens":40500});
        sanitize_minimax_anthropic_usage(Some("ocg-generic"), Some("MiniMax_M3"), &mut usage);
        assert_eq!(usage["input_tokens"], 40500);
        assert_eq!(usage["cache_read_input_tokens"], 0);

        // Qwen is unaffected.
        let mut usage = json!({"input_tokens":0,"output_tokens":5,"cache_read_input_tokens":40500});
        sanitize_minimax_anthropic_usage(Some("Qwen3.7-Max"), None, &mut usage);
        assert_eq!(usage["cache_read_input_tokens"], 40500);
    }

    #[test]
    fn mixed_case_minimax_routes_to_messages_native_protocol() {
        let plan = prepare_request(
            ApiFormat::ChatCompletions,
            bytes(json!({
                "model": "MiniMax-M3",
                "messages": [{"role": "user", "content": "hi"}],
                "max_tokens": 8
            })),
        )
        .expect("MiniMax-M3 should be routable");
        assert_eq!(plan.upstream, ApiFormat::ChatCompletions);
        assert_eq!(plan.model, "MiniMax-M3");

        let plan = prepare_request(
            ApiFormat::Messages,
            bytes(json!({
                "model": "MiniMax_M2.7",
                "messages": [{"role": "user", "content": "hi"}],
                "max_tokens": 8
            })),
        )
        .expect("MiniMax_M2.7 should be routable");
        assert_eq!(plan.upstream, ApiFormat::Messages);
    }

    #[test]
    fn signed_anthropic_thinking_round_trips_and_foreign_reasoning_is_dropped() {
        let response = json!({
            "id":"m1","model":"minimax-m2.7","stop_reason":"tool_use",
            "content":[
                {"type":"thinking","thinking":"check","signature":"sig_123"},
                {"type":"redacted_thinking","data":"opaque"},
                {"type":"tool_use","id":"call_1","name":"read","input":{"path":"a"}}
            ],
            "usage":{"input_tokens":1,"output_tokens":2}
        });
        let converted =
            transform_response(&plan(ApiFormat::Responses, ApiFormat::Messages), &response)
                .expect("Messages response converts");
        let output = converted["output"].as_array().unwrap();
        let request = json!({
            "model":"minimax-m2.7",
            "store":false,
            "max_output_tokens":4096,
            "input":[
                {"type":"message","role":"user","content":[{"type":"input_text","text":"start"}]},
                output[0].clone(),
                {"type":"reasoning","summary":[{"type":"summary_text","text":"foreign"}],"encrypted_content":"foreign-ciphertext"},
                {"type":"reasoning","summary":[{"type":"summary_text","text":"unsigned"}]},
                output[1].clone(),
                output[2].clone(),
                {"type":"function_call_output","call_id":"call_1","output":"ok"}
            ]
        });
        let plan = prepare_request(ApiFormat::Responses, bytes(request)).expect("request converts");
        let body: Value = serde_json::from_slice(&plan.body).unwrap();
        let assistant = body["messages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|message| message["role"] == "assistant")
            .unwrap();
        assert_eq!(assistant["content"].as_array().unwrap().len(), 3);
        assert_eq!(assistant["content"][0]["signature"], "sig_123");
        assert_eq!(assistant["content"][1]["type"], "redacted_thinking");
        assert_eq!(assistant["content"][2]["type"], "tool_use");
    }

    #[test]
    fn anthropic_terminal_reasons_map_to_responses_incomplete() {
        for (stop_reason, expected_reason) in [
            ("max_tokens", "max_output_tokens"),
            ("model_context_window_exceeded", "max_output_tokens"),
            ("refusal", "content_filter"),
        ] {
            let converted = messages_response_to_responses(
                &json!({
                    "id":"m1","model":"minimax-m2.7",
                    "content":[{"type":"text","text":"partial"}],
                    "stop_reason":stop_reason,
                    "usage":{"input_tokens":1,"output_tokens":1}
                }),
                &[],
                &[],
            )
            .expect("terminal response converts");
            assert_eq!(converted["status"], "incomplete", "{stop_reason}");
            assert_eq!(
                converted["incomplete_details"]["reason"], expected_reason,
                "{stop_reason}"
            );
        }
    }

    #[test]
    fn responses_response_maps_reasoning_tool_and_incomplete_status() {
        let response = json!({
            "id":"r1","model":"deepseek-v4-flash","status":"incomplete",
            "incomplete_details":{"reason":"max_output_tokens"},
            "output":[
                {"type":"reasoning","summary":[{"type":"summary_text","text":"reason"}]},
                {"type":"message","role":"assistant","content":[{"type":"output_text","text":"answer"}]},
                {"type":"function_call","call_id":"c1","name":"f","arguments":"{}"}
            ],
            "usage":{"input_tokens":10,"output_tokens":2,"input_tokens_details":{"cached_tokens":4}}
        });
        let messages =
            transform_response(&plan(ApiFormat::Messages, ApiFormat::Responses), &response)
                .expect("Responses to Messages");
        assert_eq!(messages["content"][0]["thinking"], "reason");
        assert_eq!(messages["content"][0]["signature"], "");
        assert_eq!(messages["content"][2]["type"], "tool_use");
        assert_eq!(messages["stop_reason"], "tool_use");
        assert_eq!(messages["usage"]["input_tokens"], 6);
    }

    #[test]
    fn pivot_converts_chat_response_to_responses() {
        let response = json!({
            "id":"c","model":"deepseek-v4-flash",
            "choices":[{"message":{"role":"assistant","content":"ok"},"finish_reason":"length"}],
            "usage":{"prompt_tokens":3,"completion_tokens":2}
        });
        let converted = transform_response(
            &plan(ApiFormat::Responses, ApiFormat::ChatCompletions),
            &response,
        )
        .expect("pivot converts");
        assert_eq!(converted["status"], "incomplete");
        assert_eq!(
            converted["incomplete_details"]["reason"],
            "max_output_tokens"
        );
        assert_eq!(converted["output"][0]["content"][0]["text"], "ok");
        assert_eq!(converted["id"], "resp_c");
        assert!(
            converted["created_at"]
                .as_u64()
                .is_some_and(|value| value > 0)
        );
        for field in ["parallel_tool_calls", "tool_choice", "tools"] {
            assert!(converted.get(field).is_some(), "missing {field}");
        }
        assert_eq!(converted["store"], false);
    }

    #[test]
    fn chat_reasoning_uses_private_opaque_history_only_for_chat_upstream() {
        let response = json!({
            "id":"c1","model":"deepseek-v4-flash",
            "choices":[{"message":{
                "role":"assistant","content":null,"reasoning_content":"check first",
                "tool_calls":[{"id":"call_1","type":"function","function":{"name":"read","arguments":"{}"}}]
            },"finish_reason":"tool_calls"}],
            "usage":{"prompt_tokens":1,"completion_tokens":2}
        });
        let converted = transform_response(
            &plan(ApiFormat::Responses, ApiFormat::ChatCompletions),
            &response,
        )
        .expect("Chat response converts");
        let output = converted["output"].as_array().unwrap();
        let reasoning = output
            .iter()
            .find(|item| item["type"] == "reasoning")
            .unwrap();
        assert_eq!(
            decode_chat_reasoning(reasoning["encrypted_content"].as_str().unwrap()).as_deref(),
            Some("check first")
        );

        let input = json!([
            {"type":"message","role":"user","content":[{"type":"input_text","text":"start"}]},
            reasoning,
            output.iter().find(|item| item["type"] == "function_call").unwrap(),
            {"type":"function_call_output","call_id":"call_1","output":"ok"}
        ]);
        let chat = prepare_request(
            ApiFormat::Responses,
            bytes(json!({"model":"hy3","input":input,"store":false})),
        )
        .expect("Chat-native history converts");
        assert_eq!(chat.upstream, ApiFormat::ChatCompletions);
        let chat_body: Value = serde_json::from_slice(&chat.body).unwrap();
        assert_eq!(chat_body["messages"][1]["reasoning_content"], "check first");

        let messages = prepare_request(
            ApiFormat::Responses,
            bytes(json!({"model":"minimax-m2.7","input":input,"store":false})),
        )
        .expect("Messages-native history ignores Chat opaque reasoning");
        let messages_body: Value = serde_json::from_slice(&messages.body).unwrap();
        assert!(
            messages_body["messages"][1]["content"]
                .as_array()
                .unwrap()
                .iter()
                .all(|block| block["type"] != "thinking")
        );
    }

    #[test]
    fn responses_custom_tool_converts_both_ways() {
        let request = json!({
            "model":"minimax-m2.7",
            "store":false,
            "input":[
                {"type":"message","role":"user","content":[{"type":"input_text","text":"edit"}]},
                {"type":"custom_tool_call","call_id":"c1","name":"apply_patch","input":"*** Begin Patch"},
                {"type":"custom_tool_call_output","call_id":"c1","output":[
                    {"type":"input_text","text":"done"},
                    {"type":"input_image","image_url":"data:image/png;base64,abc"}
                ]}
            ],
            "tools":[{"type":"custom","name":"apply_patch","description":"patch"}],
            "tool_choice":"required",
            "parallel_tool_calls":false
        });
        let plan = prepare_request(ApiFormat::Responses, bytes(request)).expect("custom converts");
        assert_eq!(plan.custom_tools, vec!["apply_patch"]);
        let body: Value = serde_json::from_slice(&plan.body).unwrap();
        assert_eq!(body["tools"][0]["input_schema"]["required"][0], "input");
        assert_eq!(body["tool_choice"]["type"], "any");
        assert_eq!(body["tool_choice"]["disable_parallel_tool_use"], true);
        assert_eq!(
            body["messages"][1]["content"][0]["input"]["input"],
            "*** Begin Patch"
        );
        assert_eq!(
            body["messages"][2]["content"][0]["content"][0]["type"],
            "text"
        );
        assert_eq!(
            body["messages"][2]["content"][0]["content"][1]["source"]["media_type"],
            "image/png"
        );

        let converted = transform_response(
            &plan,
            &json!({
                "id":"m1","model":"minimax-m2.7","stop_reason":"tool_use",
                "content":[{"type":"tool_use","id":"c2","name":"apply_patch","input":{"input":"patch text"}}],
                "usage":{"input_tokens":1,"output_tokens":1}
            }),
        )
        .expect("custom response converts");
        assert_eq!(converted["output"][0]["type"], "custom_tool_call");
        assert_eq!(converted["output"][0]["input"], "patch text");
        assert_eq!(converted["parallel_tool_calls"], false);
        assert_eq!(converted["tool_choice"], "required");
        assert_eq!(converted["tools"][0]["name"], "apply_patch");
    }

    #[test]
    fn responses_thinking_is_bounded_and_forced_tools_disable_it() {
        let base = json!({
            "model":"minimax-m2.7","input":"hi","store":false,"max_output_tokens":8192,
            "temperature":0.5,"top_p":0.9,
            "tools":[{"type":"function","name":"f","parameters":{"type":"object"}}],
            "tool_choice":"auto","parallel_tool_calls":false,
            "reasoning":{"effort":"high"}
        });
        let plan = prepare_request(ApiFormat::Responses, bytes(base.clone())).unwrap();
        let body: Value = serde_json::from_slice(&plan.body).unwrap();
        assert_eq!(body["thinking"]["budget_tokens"], 4096);
        assert!(body.get("temperature").is_none());
        assert!(body.get("top_p").is_none());
        assert_eq!(body["tool_choice"]["disable_parallel_tool_use"], true);

        let mut forced = base;
        forced["tool_choice"] = json!("required");
        let plan = prepare_request(ApiFormat::Responses, bytes(forced)).unwrap();
        let body: Value = serde_json::from_slice(&plan.body).unwrap();
        assert_eq!(body["thinking"]["type"], "disabled");
        assert_eq!(body["tool_choice"]["type"], "any");
    }

    #[test]
    fn messages_response_to_responses_preserves_block_order() {
        let converted = messages_response_to_responses(
            &json!({
                "id":"m1","model":"minimax-m2.7","stop_reason":"tool_use",
                "content":[
                    {"type":"thinking","thinking":"reason","signature":"sig"},
                    {"type":"text","text":"before"},
                    {"type":"tool_use","id":"c1","name":"f","input":{}},
                    {"type":"text","text":"after"}
                ],
                "usage":{"input_tokens":1,"output_tokens":1}
            }),
            &[],
            &[],
        )
        .unwrap();
        let kinds = converted["output"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item["type"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(kinds, ["reasoning", "message", "function_call", "message"]);
    }

    #[test]
    fn responses_namespace_tools_flatten_history_and_restore_response_names() {
        let request = json!({
            "model":"minimax-m2.7",
            "store":false,
            "input":[
                {"type":"message","role":"user","content":[{"type":"input_text","text":"delegate"}]},
                {"type":"function_call","call_id":"c1","namespace":"multi_agent_v1","name":"spawn_agent","arguments":"{\"task\":\"x\"}"},
                {"type":"function_call_output","call_id":"c1","output":"ok"}
            ],
            "tools":[{
                "type":"namespace","name":"multi_agent_v1","description":"agents","tools":[
                    {"type":"function","name":"spawn_agent","description":"spawn","strict":false,"parameters":{"type":"object"}},
                    {"type":"custom","name":"send_input","description":"send"}
                ]
            }],
            "tool_choice":{"type":"function","namespace":"multi_agent_v1","name":"spawn_agent"}
        });
        let plan =
            prepare_request(ApiFormat::Responses, bytes(request)).expect("namespace converts");
        assert_eq!(plan.namespace_tools.len(), 2);
        assert_eq!(
            plan.namespace_tools[0].flattened,
            "multi_agent_v1__spawn_agent"
        );
        assert_eq!(plan.custom_tools, ["multi_agent_v1__send_input"]);

        let body: Value = serde_json::from_slice(&plan.body).unwrap();
        assert_eq!(body["tools"][0]["name"], "multi_agent_v1__spawn_agent");
        assert_eq!(body["tools"][1]["name"], "multi_agent_v1__send_input");
        assert_eq!(
            body["messages"][1]["content"][0]["name"],
            "multi_agent_v1__spawn_agent"
        );
        assert_eq!(body["tool_choice"]["name"], "multi_agent_v1__spawn_agent");

        let converted = transform_response(
            &plan,
            &json!({
                "id":"m1","model":"minimax-m2.7","stop_reason":"tool_use",
                "content":[
                    {"type":"tool_use","id":"c2","name":"multi_agent_v1__spawn_agent","input":{"task":"y"}},
                    {"type":"tool_use","id":"c3","name":"multi_agent_v1__send_input","input":{"input":"hello"}}
                ],
                "usage":{"input_tokens":1,"output_tokens":1}
            }),
        )
        .expect("namespace response restores");
        assert_eq!(converted["output"][0]["type"], "function_call");
        assert_eq!(converted["output"][0]["namespace"], "multi_agent_v1");
        assert_eq!(converted["output"][0]["name"], "spawn_agent");
        assert_eq!(converted["output"][1]["type"], "custom_tool_call");
        assert_eq!(converted["output"][1]["namespace"], "multi_agent_v1");
        assert_eq!(converted["output"][1]["name"], "send_input");
    }

    #[test]
    fn responses_hosted_tools_and_history_are_ignored_unless_forced() {
        let request = json!({
            "model":"minimax-m2.7",
            "store":false,
            "input":[
                {"type":"message","role":"user","content":[{"type":"input_text","text":"hi"}]},
                {"type":"tool_search_call","call_id":"ts1","execution":"client","arguments":{}},
                {"type":"tool_search_output","call_id":"ts1","status":"completed","execution":"client","tools":[]},
                {"type":"web_search_call","id":"ws1","status":"completed","action":{"type":"search","query":"x"}}
            ],
            "tools":[
                {"type":"function","name":"local","parameters":{"type":"object"}},
                {"type":"tool_search","execution":"client"},
                {"type":"web_search","external_web_access":false}
            ],
            "tool_choice":"auto"
        });
        let plan =
            prepare_request(ApiFormat::Responses, bytes(request)).expect("hosted tools ignored");
        let body: Value = serde_json::from_slice(&plan.body).unwrap();
        assert_eq!(body["tools"].as_array().unwrap().len(), 1);
        assert_eq!(body["tools"][0]["name"], "local");
        assert_eq!(body["messages"].as_array().unwrap().len(), 1);

        for choice in [
            json!({"type":"web_search"}),
            json!({"type":"tool_search"}),
            json!("web_search"),
            json!("required"),
        ] {
            let request = json!({
                "model":"minimax-m2.7","input":"hi","store":false,
                "tools":[{"type":"web_search"}],
                "tool_choice":choice
            });
            assert!(prepare_request(ApiFormat::Responses, bytes(request)).is_err());
        }
    }

    #[test]
    fn responses_messages_add_leading_user_and_reject_empty_input() {
        let plan = prepare_request(
            ApiFormat::Responses,
            bytes(json!({
                "model":"minimax-m2.7",
                "store":false,
                "input":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"continued"}]}]
            })),
        )
        .unwrap();
        let body: Value = serde_json::from_slice(&plan.body).unwrap();
        assert_eq!(body["messages"][0]["role"], "user");
        assert_eq!(body["messages"][1]["role"], "assistant");

        assert!(
            prepare_request(
                ApiFormat::Responses,
                bytes(json!({"model":"minimax-m2.7","input":[],"store":false})),
            )
            .is_err()
        );
    }

    #[test]
    fn usage_extracts_and_stream_merge_keeps_latest_totals() {
        let mut counts = UsageCounts::default();
        merge_stream_usage(
            ApiFormat::Messages,
            &json!({"type":"message_start","message":{"usage":{"input_tokens":6,"cache_read_input_tokens":4,"cache_creation_input_tokens":2}}}),
            &mut counts,
            None,
        );
        merge_stream_usage(
            ApiFormat::Messages,
            &json!({"type":"message_delta","usage":{"output_tokens":7}}),
            &mut counts,
            None,
        );
        assert_eq!(
            counts,
            UsageCounts {
                input_tokens: 12,
                output_tokens: 7,
                cached_tokens: 4,
                cache_creation_tokens: 2
            }
        );

        assert_eq!(
            extract_usage(
                ApiFormat::Responses,
                &json!({"response":{"usage":{"input_tokens":9,"output_tokens":3,"input_tokens_details":{"cached_tokens":2}}}}),
                None,
            ),
            UsageCounts {
                input_tokens: 9,
                output_tokens: 3,
                cached_tokens: 2,
                cache_creation_tokens: 0
            }
        );
    }

    #[test]
    fn format_error_uses_client_envelope_and_upstream_message() {
        let body = format_error(
            ApiFormat::Messages,
            StatusCode::TOO_MANY_REQUESTS,
            "fallback",
            Some(&json!({"error":{"message":"limited","type":"rate_limit_error"}})),
        );
        assert_eq!(body["type"], "error");
        assert_eq!(body["error"]["message"], "limited");
        assert_eq!(body["error"]["type"], "rate_limit_error");
    }

    #[test]
    fn muse_spark_aliases_max_reasoning_effort_to_xhigh_on_responses() {
        let request = json!({
            "model":"muse-spark-1.2","input":"hi","store":false,
            "reasoning":{"effort":"max"}
        });
        let plan = prepare_request(ApiFormat::Responses, bytes(request)).unwrap();
        let body: Value = serde_json::from_slice(&plan.body).unwrap();
        assert_eq!(body["reasoning"]["effort"], "xhigh");
    }

    #[test]
    fn muse_spark_max_alias_reaches_responses_upstream_from_every_client_protocol() {
        let requests = [
            (
                ApiFormat::Responses,
                json!({
                    "model":"muse-spark-1.2","input":"hi","store":false,
                    "reasoning":{"effort":"max"}
                }),
            ),
            (
                ApiFormat::ChatCompletions,
                json!({
                    "model":"muse-spark-1.2-contributor","messages":[{"role":"user","content":"hi"}],
                    "reasoning_effort":"max"
                }),
            ),
            (
                ApiFormat::Messages,
                json!({
                    "model":"muse-spark-1.2","max_tokens":8192,
                    "messages":[{"role":"user","content":"hi"}],
                    "output_config":{"effort":"max"}
                }),
            ),
        ];
        for (client, request) in requests {
            let plan = prepare_request(client, bytes(request)).unwrap();
            assert_eq!(plan.upstream, ApiFormat::Responses);
            let body: Value = serde_json::from_slice(&plan.body).unwrap();
            assert_eq!(body["reasoning"]["effort"], "xhigh");
        }
    }

    #[test]
    fn muse_spark_leaves_non_aliased_effort_untouched() {
        let request = json!({
            "model":"muse-spark-1.2","input":"hi","store":false,
            "reasoning":{"effort":"high"}
        });
        let plan = prepare_request(ApiFormat::Responses, bytes(request)).unwrap();
        let body: Value = serde_json::from_slice(&plan.body).unwrap();
        assert_eq!(body["reasoning"]["effort"], "high");
    }

    #[test]
    fn non_aliased_model_passes_max_through_unchanged() {
        let request = json!({
            "model":"gpt-5.6-luna","input":"hi","store":false,
            "reasoning":{"effort":"max"}
        });
        let plan = prepare_request(ApiFormat::Responses, bytes(request)).unwrap();
        let body: Value = serde_json::from_slice(&plan.body).unwrap();
        assert_eq!(body["reasoning"]["effort"], "max");
    }

    #[test]
    fn prepare_request_records_client_model() {
        let plan = prepare_request(
            ApiFormat::ChatCompletions,
            bytes(json!({
                "model": "MiniMax-M3",
                "messages": [{"role": "user", "content": "hi"}]
            })),
        )
        .unwrap();
        assert_eq!(plan.model, "MiniMax-M3");
        assert_eq!(plan.client_model, "MiniMax-M3");
        assert_eq!(plan.response_model(), "MiniMax-M3");
        assert_eq!(plan.log_requested_model(), "MiniMax-M3");
        assert_eq!(plan.log_upstream_model(), "MiniMax-M3");
        assert_eq!(
            crate::gateway::materialize::native_log_identity(&plan)
                .resolved_alias
                .as_deref(),
            Some("minimax-m3")
        );
    }

    #[test]
    fn parse_once_materializes_a_different_upstream_model() {
        let parsed = parse_client_request(
            ApiFormat::ChatCompletions,
            bytes(json!({
                "model": "deepseek-v4-flash",
                "messages": [{"role": "user", "content": "hi"}]
            })),
        )
        .unwrap();
        let go = materialize_parsed_request(
            &parsed,
            &MaterializeSpec {
                client_model: parsed.requested_model.clone(),
                upstream_model: "deepseek-v4-flash".into(),
                resolved_alias: Some("deepseek-v4-flash".into()),
                channel: UpstreamChannel::Go,
                upstream_base_override: None,
                original_model: None,
                allow_go_fallback: false,
            },
        )
        .unwrap();
        let free = materialize_parsed_request(
            &parsed,
            &MaterializeSpec {
                client_model: parsed.requested_model.clone(),
                upstream_model: "deepseek-v4-flash-free".into(),
                resolved_alias: Some("deepseek-v4-flash".into()),
                channel: UpstreamChannel::Free,
                upstream_base_override: Some("https://opencode.ai/zen".into()),
                original_model: Some("deepseek-v4-flash".into()),
                allow_go_fallback: true,
            },
        )
        .unwrap();
        assert_eq!(go.model, "deepseek-v4-flash");
        assert_eq!(go.client_model, "deepseek-v4-flash");
        assert_eq!(go.log_requested_model(), "deepseek-v4-flash");
        assert_eq!(go.log_upstream_model(), "deepseek-v4-flash");
        assert_eq!(
            crate::gateway::materialize::native_log_identity(&go)
                .resolved_alias
                .as_deref(),
            Some("deepseek-v4-flash")
        );
        assert_eq!(go.channel, UpstreamChannel::Go);
        assert_eq!(free.model, "deepseek-v4-flash-free");
        assert_eq!(free.client_model, "deepseek-v4-flash");
        assert_eq!(free.log_requested_model(), "deepseek-v4-flash");
        assert_eq!(free.log_upstream_model(), "deepseek-v4-flash-free");
        assert_eq!(
            crate::gateway::materialize::native_log_identity(&free)
                .resolved_alias
                .as_deref(),
            Some("deepseek-v4-flash")
        );
        assert_eq!(free.channel, UpstreamChannel::Free);
        assert_eq!(free.original_model.as_deref(), Some("deepseek-v4-flash"));
        let go_body: Value = serde_json::from_slice(&go.body).unwrap();
        let free_body: Value = serde_json::from_slice(&free.body).unwrap();
        assert_eq!(go_body["model"], "deepseek-v4-flash");
        assert_eq!(free_body["model"], "deepseek-v4-flash-free");
    }

    #[test]
    fn transform_response_rewrites_model_to_client_name() {
        let mut plan = plan_with_model(
            ApiFormat::ChatCompletions,
            ApiFormat::ChatCompletions,
            "deepseek-v4-flash-free",
        );
        plan.client_model = "deepseek-v4-flash".into();
        let converted = transform_response(
            &plan,
            &json!({
                "id": "chatcmpl-1",
                "model": "upstream-should-not-leak",
                "choices": [{"index": 0, "message": {"role": "assistant", "content": "ok"}, "finish_reason": "stop"}],
                "usage": {"prompt_tokens": 1, "completion_tokens": 1}
            }),
        )
        .unwrap();
        assert_eq!(converted["model"], "deepseek-v4-flash");

        let mut messages_plan =
            plan_with_model(ApiFormat::Messages, ApiFormat::Messages, "glm-5.2");
        messages_plan.client_model = "claude-sonnet-4-6".into();
        let converted = transform_response(
            &messages_plan,
            &json!({
                "id": "msg_1",
                "type": "message",
                "role": "assistant",
                "model": "glm-5.2",
                "content": [{"type": "text", "text": "hi"}],
                "stop_reason": "end_turn",
                "usage": {"input_tokens": 1, "output_tokens": 1}
            }),
        )
        .unwrap();
        assert_eq!(converted["model"], "claude-sonnet-4-6");
    }

    #[test]
    fn format_error_exposes_ambiguous_model_id() {
        let message = "ambiguous_model_id: send a preferred alias instead of this raw id";
        let code = Some(crate::alias::AMBIGUOUS_MODEL_ID);
        let chat = format_error_with_code(
            ApiFormat::ChatCompletions,
            StatusCode::BAD_REQUEST,
            message,
            None,
            code,
        );
        assert_eq!(chat["error"]["type"], crate::alias::AMBIGUOUS_MODEL_ID);
        assert!(chat["error"]["message"].as_str().unwrap().contains("alias"));

        let messages = format_error_with_code(
            ApiFormat::Messages,
            StatusCode::BAD_REQUEST,
            message,
            None,
            code,
        );
        assert_eq!(messages["error"]["type"], crate::alias::AMBIGUOUS_MODEL_ID);

        let responses = format_error_with_code(
            ApiFormat::Responses,
            StatusCode::BAD_REQUEST,
            message,
            None,
            code,
        );
        assert_eq!(responses["error"]["type"], crate::alias::AMBIGUOUS_MODEL_ID);
        assert_eq!(responses["error"]["code"], crate::alias::AMBIGUOUS_MODEL_ID);

        let gemini = format_error_with_code(
            ApiFormat::Gemini,
            StatusCode::BAD_REQUEST,
            message,
            None,
            code,
        );
        assert_eq!(gemini["error"]["status"], "INVALID_ARGUMENT");
        assert_eq!(gemini["error"]["reason"], crate::alias::AMBIGUOUS_MODEL_ID);
        assert_eq!(
            gemini["error"]["details"][0]["reason"],
            crate::alias::AMBIGUOUS_MODEL_ID
        );
    }

    #[test]
    fn command_code_descriptor_is_separate_from_opencode_table() {
        let goat = command_code_model_protocol(COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM)
            .expect("official GOAT raw id");
        assert_eq!(goat.alias, COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_ALIAS);
        assert_eq!(goat.preferred, ApiFormat::ChatCompletions);
        assert_eq!(goat.supported_upstream, CHAT_ONLY);
        assert!(command_code_supports_upstream(
            COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM,
            ApiFormat::ChatCompletions
        ));
        assert!(!command_code_supports_upstream(
            COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM,
            ApiFormat::Responses
        ));
        assert!(!command_code_supports_upstream(
            COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM,
            ApiFormat::Messages
        ));
        assert!(
            command_code_model_protocol(COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_ALIAS).is_none(),
            "kebab alias must stay OpenCode-owned; GOAT lookup is exact slash raw id only"
        );
        assert!(
            !opencode_supports_upstream(
                COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM,
                ApiFormat::ChatCompletions
            ),
            "GOAT raw id must not resolve through OpenCode MODEL_PROTOCOLS"
        );
        assert!(opencode_supports_upstream(
            COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_ALIAS,
            ApiFormat::Responses
        ));
        assert_eq!(
            command_code_upstream_path(ApiFormat::ChatCompletions),
            Some("/chat/completions")
        );
        assert_eq!(
            command_code_upstream_path(ApiFormat::Messages),
            Some("/messages")
        );
        assert_eq!(command_code_upstream_path(ApiFormat::Responses), None);
        assert_eq!(command_code_upstream_path(ApiFormat::Gemini), None);
    }

    #[test]
    fn command_code_client_formats_convert_to_chat_and_never_emit_responses() {
        for (client, body) in [
            (
                ApiFormat::ChatCompletions,
                bytes(json!({
                    "model": COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM,
                    "messages": [{"role": "user", "content": "hi"}]
                })),
            ),
            (
                ApiFormat::Responses,
                bytes(json!({
                    "model": COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM,
                    "input": "hi",
                    "store": false
                })),
            ),
            (
                ApiFormat::Messages,
                bytes(json!({
                    "model": COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM,
                    "max_tokens": 1,
                    "messages": [{"role": "user", "content": "hi"}]
                })),
            ),
        ] {
            let parsed = parse_client_request(client, body).unwrap();
            let plan = materialize_parsed_request(
                &parsed,
                &MaterializeSpec {
                    client_model: parsed.requested_model.clone(),
                    upstream_model: COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM.into(),
                    resolved_alias: Some(COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_ALIAS.into()),
                    channel: UpstreamChannel::Go,
                    upstream_base_override: None,
                    original_model: None,
                    allow_go_fallback: false,
                },
            )
            .unwrap();
            assert_eq!(plan.upstream, ApiFormat::ChatCompletions, "{client:?}");
            assert_eq!(
                plan.model, COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM,
                "{client:?}"
            );
            assert_ne!(plan.upstream, ApiFormat::Responses);
            assert_eq!(
                command_code_upstream_path(plan.upstream),
                Some("/chat/completions")
            );
        }

        let gemini = parse_gemini_request(
            COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM.into(),
            false,
            bytes(json!({"contents":[{"role":"user","parts":[{"text":"hi"}]}]})),
        )
        .unwrap();
        let plan = materialize_parsed_request(
            &gemini,
            &MaterializeSpec {
                client_model: gemini.requested_model.clone(),
                upstream_model: COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM.into(),
                resolved_alias: Some(COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_ALIAS.into()),
                channel: UpstreamChannel::Go,
                upstream_base_override: None,
                original_model: None,
                allow_go_fallback: false,
            },
        )
        .unwrap();
        assert_eq!(plan.client, ApiFormat::Gemini);
        assert_eq!(plan.upstream, ApiFormat::ChatCompletions);
        assert_eq!(
            command_code_upstream_path(plan.upstream),
            Some("/chat/completions")
        );
    }
}

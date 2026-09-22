use crate::kernel::ids::is_free_model;
use crate::kernel::protocol::model_protocol;
use crate::models::UpstreamChannel;
use axum::http::StatusCode;
use bytes::Bytes;
use serde_json::{Value, json};
use std::fmt;
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

pub use crate::kernel::protocol::{
    ApiFormat, CommandCodeModelProtocol, command_code_model_protocol,
    command_code_supports_upstream, is_known_model, opencode_supports_upstream,
    supported_model_ids, supported_model_protocol_profiles, supported_model_protocols,
};
pub use ocg_domain::protocol::{
    command_code_constructable_formats, command_code_is_anthropic_model,
    command_code_preferred_format, command_code_supported_formats,
};

pub(crate) use ocg_gateway::protocol::{
    LegacyToolCompat, NamespaceToolMapping, decode_anthropic_thinking_block, decode_chat_reasoning,
    encode_anthropic_thinking_block, encode_chat_reasoning,
};

#[cfg(test)]
pub(crate) use ocg_gateway::protocol::{LEGACY_TOOL_COMPAT_PROFILE, LEGACY_TOOL_COMPAT_VERSION};

use ocg_gateway::protocol::{ResponseSynthesis, convert_request_json, convert_response_json};

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
    /// Optional override for the sealed Zen free base (loopback tests).
    pub upstream_base_override: Option<String>,
    /// Client-requested model before prefer mapping, when different.
    pub original_model: Option<String>,
    /// Canonical resolved identity persisted on forward logs.
    pub resolved_alias: Option<String>,
    /// Isolated Custom origin + auth. Presence selects the Custom HTTP path.
    pub custom_route: Option<CustomRouteSpec>,
    pub(crate) service_tier: Option<String>,
    pub(crate) custom_tools: Vec<String>,
    pub(crate) namespace_tools: Vec<NamespaceToolMapping>,
    /// Versioned legacy-compat drop recorded by conversion. Carried to the
    /// forward attempt so runtime diagnostics can name the profile without
    /// rewriting stored protocol configuration.
    pub(crate) legacy_tool_compat: Option<LegacyToolCompat>,
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

/// Client request facts captured at entry for budgets and pre-candidate logs.
///
/// This context does not select, convert, or validate an upstream protocol.
/// Candidate materialization remains the sole upstream conversion authority.
#[derive(Debug, Clone)]
pub(crate) struct RequestFacts {
    pub client: ApiFormat,
    pub client_model: String,
    pub stream: bool,
}

impl RequestFacts {
    pub(crate) fn from_parsed(parsed: &ParsedClientRequest) -> Self {
        Self {
            client: parsed.client,
            client_model: parsed.requested_model.clone(),
            stream: parsed.stream,
        }
    }
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
    /// Skip OpenCode `MODEL_PROTOCOLS` and convert to this account protocol.
    pub forced_upstream: Option<ApiFormat>,
    pub custom_route: Option<CustomRouteSpec>,
}

/// Isolated Custom origin and auth scheme materialized per account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustomRouteSpec {
    pub endpoint_url: String,
    pub auth_kind: ocg_domain::dynamic::DynamicAuthKind,
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

/// Official Command Code relative paths. Gemini has no upstream path.
/// Responses is constructable; per-model enablement stays on persisted
/// catalog evidence rather than this path table.
pub fn command_code_upstream_path(format: ApiFormat) -> Option<&'static str> {
    ocg_domain::protocol::command_code_upstream_path(format)
}

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

/// Request features that apply regardless of which candidate is later
/// selected. Conversion-only checks stay in [`materialize_parsed_request`].
pub(crate) fn validate_client_request_features(
    parsed: &ParsedClientRequest,
) -> Result<(), ProtocolError> {
    ocg_gateway::protocol::validate_client_request_features(parsed.client, &parsed.parsed)
        .map_err(protocol_conversion_error)
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
        forced_upstream: None,
        custom_route: None,
    }
}

/// Convert a request that was already parsed once for a specific candidate.
///
/// Protocol selection uses the OpenCode `MODEL_PROTOCOLS` table for the
/// upstream model unless the caller supplies `forced_upstream` from that
/// candidate's contract. Request-level diagnostics must not call this to
/// guess an upstream. Callers must never trial a billable inference path.
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
        spec.forced_upstream,
    )?;
    plan.client_model = spec.client_model.clone();
    plan.channel = spec.channel;
    plan.upstream_base_override = spec.upstream_base_override.clone();
    plan.original_model = spec.original_model.clone();
    plan.resolved_alias = spec.resolved_alias.clone();
    plan.custom_route = spec.custom_route.clone();
    debug_assert!(
        spec.resolved_alias
            .as_deref()
            .is_none_or(|alias| !alias.is_empty() || spec.custom_route.is_some())
    );
    Ok(plan)
}

fn prepare_parsed_request(
    client: ApiFormat,
    parsed: Value,
    model: String,
    stream: bool,
    forced_upstream: Option<ApiFormat>,
) -> Result<RequestPlan, ProtocolError> {
    let upstream = match forced_upstream {
        Some(forced) => forced,
        None => resolve_upstream_format(client, &model)?,
    };
    let aliased_responses_effort = requested_effort_alias(&parsed, &model);
    let parsed = apply_effort_aliases(parsed, &model);
    let response_parallel_tool_calls = parsed
        .get("parallel_tool_calls")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let response_tool_choice = parsed
        .get("tool_choice")
        .cloned()
        .unwrap_or_else(|| json!("auto"));
    let response_tools = array(&parsed, "tools").to_vec();
    let mut converted =
        convert_request_json(client, upstream, parsed).map_err(protocol_conversion_error)?;
    if upstream == ApiFormat::Responses
        && let Some(effort) = aliased_responses_effort
    {
        set_responses_reasoning_effort(&mut converted.body, effort);
    }
    let service_tier = converted
        .body
        .get("service_tier")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let body = serde_json::to_vec(&converted.body)
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
        resolved_alias: None,
        custom_route: None,
        service_tier,
        custom_tools: converted.custom_tools,
        namespace_tools: converted.namespace_tools,
        legacy_tool_compat: converted.legacy_tool_compat,
        response_parallel_tool_calls,
        response_tool_choice,
        response_tools,
    })
}

fn protocol_conversion_error(error: ocg_gateway::protocol::ConversionError) -> ProtocolError {
    ProtocolError::new(error.message)
}

fn fallback_empty_response_id() -> String {
    format!("resp_{}", Uuid::new_v4().simple())
}

pub fn transform_response(plan: &RequestPlan, body: &Value) -> Result<Value, ProtocolError> {
    let mut transformed = convert_response_json(
        plan.upstream,
        plan.client,
        body,
        &plan.custom_tools,
        &plan.namespace_tools,
        ResponseSynthesis {
            created_at: unix_seconds(),
            empty_response_id: fallback_empty_response_id(),
        },
        Some(&plan.model),
    )
    .map_err(protocol_conversion_error)?
    .body;
    if plan.client == ApiFormat::Responses && plan.upstream != ApiFormat::Responses {
        transformed["parallel_tool_calls"] = json!(plan.response_parallel_tool_calls);
        transformed["tool_choice"] = plan.response_tool_choice.clone();
        transformed["tools"] = Value::Array(plan.response_tools.clone());
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
    convert_response_json(
        upstream,
        client,
        body,
        &[],
        &[],
        ResponseSynthesis {
            created_at: unix_seconds(),
            empty_response_id: fallback_empty_response_id(),
        },
        None,
    )
    .map(|converted| converted.body)
    .map_err(protocol_conversion_error)
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

/// Report upstream usage without model-name-based corrections.
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

pub fn extract_usage(format: ApiFormat, payload: &Value, _model_hint: Option<&str>) -> UsageCounts {
    let usage = usage_payload(format, payload);
    let Some(usage) = usage else {
        return UsageCounts::default();
    };
    match format {
        ApiFormat::ChatCompletions => UsageCounts {
            input_tokens: uint(&usage, "prompt_tokens"),
            output_tokens: uint(&usage, "completion_tokens"),
            cached_tokens: usage
                .pointer("/prompt_tokens_details/cached_tokens")
                .or_else(|| usage.get("prompt_cache_hit_tokens"))
                .and_then(Value::as_u64)
                .unwrap_or(0),
            cache_creation_tokens: 0,
        },
        ApiFormat::Messages => {
            let cached = uint(usage, "cache_read_input_tokens");
            let created = uint(usage, "cache_creation_input_tokens");
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
        (_, None) if is_free_model(model) => Ok(ApiFormat::ChatCompletions),
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
        .and_then(rewrite)
        && let Some(reasoning) = body.get_mut("reasoning").and_then(Value::as_object_mut)
    {
        reasoning.insert("effort".into(), Value::String(replacement));
    }
    if let Some(replacement) = body
        .get("reasoning_effort")
        .and_then(Value::as_str)
        .and_then(rewrite)
    {
        body["reasoning_effort"] = Value::String(replacement);
    }
    if let Some(replacement) = body
        .pointer("/output_config/effort")
        .and_then(Value::as_str)
        .and_then(rewrite)
        && let Some(output_config) = body.get_mut("output_config").and_then(Value::as_object_mut)
    {
        output_config.insert("effort".into(), Value::String(replacement));
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

#[cfg(test)]
mod tests;

//! Custom API runtime helpers: connection-owned route configuration projected
//! per credential for capability matching and verification.
//!
//! Account model capabilities are the client-facing IDs and the exact upstream
//! IDs, each bound to its effective destination route. Verification sends
//! one protocol-correct non-stream request against the first declared model.
//! Discovery never mutates the declared list.
//! The adapter identity is Configurable HTTP, not a base class other providers
//! inherit from. Custom keeps destination-owned HTTP configuration and per-Key enablement;
//! connection verification is an optional tool, not an enablement gate.

use crate::custom_http::{
    self, CustomHttpClient, HttpInferenceTransport, InferenceHttpError, custom_auth_scheme,
    json_content_headers, resolve_custom_endpoints,
};
use crate::kernel::protocol::ApiFormat;
use crate::models::{
    AccountCustomConfig, AccountCustomConfigInput, AccountModelCapability,
    AccountModelCapabilityInput, AppConfig, CustomModelDiscoveryResult,
};
use crate::provider::{ConnectionVerificationStatus, UpstreamProtocolKind};
use reqwest::StatusCode;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::time::Duration;

/// Canonical definition: [`crate::kernel::ids::custom_model_id_matches`].
pub use crate::kernel::ids::custom_model_id_matches;

/// Custom destination URL trust policy lives with the outbound boundary in
/// [`crate::custom_http`]; re-exported here for the Custom runtime surface.
pub use crate::custom_http::{
    CustomUrlHost, CustomUrlTarget, inspect_custom_url, validate_custom_endpoint_url,
};

/// Upper bound for a protocol-correct verification response. Larger bodies
/// are rejected without certifying the account.
pub const MAX_CUSTOM_VERIFICATION_BODY_BYTES: usize = 64 * 1024;

/// Discovery is an interactive dashboard aid, not an unbounded upstream
/// directory mirror. These caps keep a malicious or accidental endpoint from
/// consuming arbitrary memory or issuing an unbounded cursor chain.
pub const MAX_CUSTOM_MODEL_DISCOVERY_BODY_BYTES: usize = 256 * 1024;
pub const MAX_CUSTOM_MODEL_DISCOVERY_MODELS: usize = 1_000;
pub const MAX_CUSTOM_MODEL_DISCOVERY_PAGES: usize = 10;
pub const CUSTOM_MODEL_DISCOVERY_TIMEOUT_SECS: u64 = 30;

/// Dashboard conflict when a stale Custom probe no longer matches the account.
pub const CUSTOM_VERIFICATION_CONFLICT_MESSAGE: &str =
    "the Custom account changed while it was being verified; retry verification";

/// One Custom account's persisted config + declared capabilities, in account order.
#[derive(Debug, Clone)]
pub struct CustomAccountRuntime {
    pub account_id: String,
    pub enabled: bool,
    pub verification_status: ConnectionVerificationStatus,
    pub setup_ready: bool,
    pub has_key: bool,
    pub auth_kind: ocg_domain::dynamic::DynamicAuthKind,
    pub config: AccountCustomConfig,
    pub capabilities: Vec<AccountModelCapability>,
    pub route_overrides: Vec<(String, ocg_domain::dynamic::DynamicModelUpstreamOverride)>,
    /// Linked New API / Sub2API Key: the site converts Chat, Messages, and
    /// Responses, so the contract enables those protocols and the gateway
    /// passes the matching client format through.
    pub protocol_passthrough: bool,
}

impl CustomAccountRuntime {
    pub fn eligible(&self) -> bool {
        self.enabled && self.setup_ready && self.has_key
    }

    pub fn capability_matching_public(&self, requested: &str) -> Option<&AccountModelCapability> {
        self.capabilities
            .iter()
            .find(|capability| custom_model_id_matches(&capability.public_model, requested))
    }

    pub fn route_override_matching_public(
        &self,
        requested: &str,
    ) -> Option<&ocg_domain::dynamic::DynamicModelUpstreamOverride> {
        self.route_overrides
            .iter()
            .find(|(public_model, _)| custom_model_id_matches(public_model, requested))
            .map(|(_, route)| route)
    }

    /// Public-name / protocol rows used to build the Custom contract.
    ///
    /// A passthrough Key advertises Chat, Responses, and Messages for each
    /// declared model so client formats pass through. Ordinary Custom Keys
    /// keep the single stored protocol.
    pub fn declared_protocols(&self) -> Vec<(String, UpstreamProtocolKind)> {
        if !self.protocol_passthrough {
            return self
                .capabilities
                .iter()
                .map(|capability| (capability.public_model.clone(), capability.protocol))
                .collect();
        }
        let mut rows = Vec::new();
        let mut seen = HashSet::new();
        for capability in &self.capabilities {
            if !seen.insert(capability.public_model.to_ascii_lowercase()) {
                continue;
            }
            for protocol in UpstreamProtocolKind::ALL {
                rows.push((capability.public_model.clone(), protocol));
            }
        }
        rows
    }
}

pub fn custom_runtimes_by_account(
    runtimes: &[CustomAccountRuntime],
) -> HashMap<String, CustomAccountRuntime> {
    runtimes
        .iter()
        .cloned()
        .map(|runtime| (runtime.account_id.clone(), runtime))
        .collect()
}

/// Case-preserving declared IDs from eligible enabled+ready Custom accounts,
/// de-duplicated in account then capability order.
pub fn eligible_custom_public_models(runtimes: &[CustomAccountRuntime]) -> Vec<String> {
    let mut ids = Vec::new();
    for runtime in runtimes.iter().filter(|runtime| runtime.eligible()) {
        for capability in &runtime.capabilities {
            if ids.iter().any(|existing: &String| {
                custom_model_id_matches(existing, &capability.public_model)
            }) {
                continue;
            }
            ids.push(capability.public_model.clone());
        }
    }
    ids
}

pub fn any_eligible_custom_model(runtimes: &[CustomAccountRuntime], requested: &str) -> bool {
    runtimes.iter().any(|runtime| {
        runtime.eligible() && runtime.capability_matching_public(requested).is_some()
    })
}

pub fn api_format_for_custom_protocol(protocol: UpstreamProtocolKind) -> ApiFormat {
    match protocol {
        UpstreamProtocolKind::ChatCompletions => ApiFormat::ChatCompletions,
        UpstreamProtocolKind::Responses => ApiFormat::Responses,
        UpstreamProtocolKind::Messages => ApiFormat::Messages,
    }
}

/// Immutable identity of the Custom account a verification probe was issued
/// against. Commit is allowed only when this exact contract still exists and
/// the account is still unverified (`pending` or `failed`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustomVerificationContract {
    pub account_id: String,
    /// Raw `accounts.updated_at` text; the per-account revision token.
    pub account_updated_at: String,
    /// Encrypted key ciphertext, not the plaintext secret.
    pub key_cipher: String,
    pub endpoint_url: String,
    pub upstream_protocol: UpstreamProtocolKind,
    /// Declared capability IDs in persistence order.
    pub capabilities: Vec<(String, String, UpstreamProtocolKind)>,
}

impl CustomVerificationContract {
    pub fn from_parts(
        account_id: impl Into<String>,
        account_updated_at: impl Into<String>,
        key_cipher: impl Into<String>,
        config: &AccountCustomConfig,
        capabilities: &[AccountModelCapability],
    ) -> Self {
        Self {
            account_id: account_id.into(),
            account_updated_at: account_updated_at.into(),
            key_cipher: key_cipher.into(),
            endpoint_url: config.endpoint_url.clone(),
            upstream_protocol: config.upstream_protocol,
            capabilities: capabilities
                .iter()
                .map(|capability| {
                    (
                        capability.public_model.clone(),
                        capability.upstream_model.clone(),
                        capability.protocol,
                    )
                })
                .collect(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustomVerifyFailure {
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustomModelDiscoveryFailure {
    pub message: String,
}

impl fmt::Display for CustomModelDiscoveryFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for CustomModelDiscoveryFailure {}

/// Fetch declared model IDs from the standard `/models` endpoint resolved from
/// the configured API URL. This never probes completion endpoints or writes
/// account state. OpenAI- and Anthropic-compatible list envelopes both use `data`.
pub async fn discover_custom_models(
    config: &AppConfig,
    input: &AccountCustomConfigInput,
    api_key: &str,
) -> Result<CustomModelDiscoveryResult, CustomModelDiscoveryFailure> {
    if api_key.trim().is_empty() {
        return Err(CustomModelDiscoveryFailure {
            message: "Custom model discovery requires an API key".to_string(),
        });
    }
    discover_models_with_auth(
        config,
        input,
        Some(custom_auth_scheme(input.upstream_protocol)),
        api_key,
    )
    .await
}

pub(crate) async fn discover_models_with_auth(
    config: &AppConfig,
    input: &AccountCustomConfigInput,
    auth: Option<crate::provider::UpstreamAuthScheme>,
    api_key: &str,
) -> Result<CustomModelDiscoveryResult, CustomModelDiscoveryFailure> {
    discover_models_with_metadata(config, input, auth, api_key)
        .await
        .map(|(result, _)| result)
}

pub(crate) async fn discover_models_with_metadata(
    config: &AppConfig,
    input: &AccountCustomConfigInput,
    auth: Option<crate::provider::UpstreamAuthScheme>,
    api_key: &str,
) -> Result<
    (
        CustomModelDiscoveryResult,
        std::collections::BTreeMap<String, crate::model_metadata::ModelMetadata>,
    ),
    CustomModelDiscoveryFailure,
> {
    let mut metadata = std::collections::BTreeMap::new();
    let result = tokio::time::timeout(
        Duration::from_secs(CUSTOM_MODEL_DISCOVERY_TIMEOUT_SECS),
        discover_custom_models_inner(config, input, auth, api_key, &mut metadata),
    )
    .await
    .map_err(|_| CustomModelDiscoveryFailure {
        message: format!(
            "Custom model discovery timed out after {CUSTOM_MODEL_DISCOVERY_TIMEOUT_SECS} seconds"
        ),
    })??;
    metadata.retain(|id, value| {
        value.redact_secret(api_key);
        result.models.contains(id) && (api_key.is_empty() || !id.contains(api_key))
    });
    Ok((result, metadata))
}

async fn discover_custom_models_inner(
    config: &AppConfig,
    input: &AccountCustomConfigInput,
    auth: Option<crate::provider::UpstreamAuthScheme>,
    api_key: &str,
    metadata: &mut std::collections::BTreeMap<String, crate::model_metadata::ModelMetadata>,
) -> Result<CustomModelDiscoveryResult, CustomModelDiscoveryFailure> {
    if auth.is_some() && api_key.trim().is_empty() {
        return Err(CustomModelDiscoveryFailure {
            message: "Custom model discovery requires an API key".to_string(),
        });
    }
    let mut url = derive_custom_models_endpoint(&input.endpoint_url, input.upstream_protocol)?;
    let client = custom_http::build_custom_http_client(config).map_err(|error| {
        CustomModelDiscoveryFailure {
            message: format!("failed to build Custom HTTP client: {error}"),
        }
    })?;
    let headers = model_discovery_headers(input.upstream_protocol);
    let timeout = Some(model_discovery_request_timeout(config));
    let mut models = Vec::new();
    let mut seen_models = HashSet::new();
    let mut seen_cursors = HashSet::new();

    for page in 0..MAX_CUSTOM_MODEL_DISCOVERY_PAGES {
        let response = client
            .send_isolated_optional(
                reqwest::Method::GET,
                url.clone(),
                auth.map(|scheme| (scheme, api_key)),
                headers.clone(),
                None,
                timeout,
            )
            .await
            .map_err(|error| CustomModelDiscoveryFailure {
                message: format!("Custom model discovery network or timeout error: {error}"),
            })?;
        let status = response.status();
        let body = read_custom_model_discovery_body(response).await?;
        if !status.is_success() {
            return Err(CustomModelDiscoveryFailure {
                message: discovery_status_message(status),
            });
        }
        let page_result = parse_model_discovery_page(&body)?;
        let page_metadata = crate::model_metadata::parse_catalog(&body);
        for model in page_result.models {
            if seen_models.insert(model.to_ascii_lowercase()) {
                if let Some(facts) = page_metadata.get(&model) {
                    metadata.insert(model.clone(), facts.clone());
                }
                models.push(model);
                if models.len() >= MAX_CUSTOM_MODEL_DISCOVERY_MODELS {
                    return Ok(CustomModelDiscoveryResult {
                        models,
                        truncated: true,
                    });
                }
            }
        }
        if !page_result.has_more {
            return Ok(CustomModelDiscoveryResult {
                models,
                truncated: false,
            });
        }
        if page + 1 >= MAX_CUSTOM_MODEL_DISCOVERY_PAGES {
            return Ok(CustomModelDiscoveryResult {
                models,
                truncated: true,
            });
        }
        let cursor = page_result.cursor.or(page_result.last_valid_id).ok_or_else(|| CustomModelDiscoveryFailure {
            message: "Custom model discovery response has_more=true but contains no valid model ID for after_id".to_string() })?;
        advance_model_discovery_cursor(&mut url, &mut seen_cursors, &cursor)?;
    }
    unreachable!("bounded discovery loop always returns")
}

fn model_discovery_request_timeout(config: &AppConfig) -> Duration {
    Duration::from_secs(
        config
            .non_stream_timeout_secs
            .min(CUSTOM_MODEL_DISCOVERY_TIMEOUT_SECS),
    )
}

pub fn derive_custom_models_endpoint(
    endpoint_url: &str,
    protocol: UpstreamProtocolKind,
) -> Result<reqwest::Url, CustomModelDiscoveryFailure> {
    let resolved = resolve_custom_endpoints(endpoint_url, protocol).map_err(|error| {
        CustomModelDiscoveryFailure {
            message: format!("invalid Custom API URL: {error}"),
        }
    })?;
    let suffix = match protocol {
        UpstreamProtocolKind::ChatCompletions => "/chat/completions",
        UpstreamProtocolKind::Responses => "/responses",
        UpstreamProtocolKind::Messages => "/messages",
    };
    resolved.models.ok_or_else(|| CustomModelDiscoveryFailure {
        message: format!(
            "cannot derive Custom /models endpoint: use an API root, a base ending in `/v1`, or a {protocol:?} endpoint ending with `{suffix}`; add model IDs manually for non-standard paths"
        ) })
}

fn model_discovery_headers(protocol: UpstreamProtocolKind) -> reqwest::header::HeaderMap {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::ACCEPT,
        reqwest::header::HeaderValue::from_static("application/json"),
    );
    if protocol == UpstreamProtocolKind::Messages {
        headers.insert(
            reqwest::header::HeaderName::from_static("anthropic-version"),
            reqwest::header::HeaderValue::from_static("2023-06-01"),
        );
    }
    headers
}

struct CustomModelDiscoveryPage {
    models: Vec<String>,
    has_more: bool,
    last_valid_id: Option<String>,
    cursor: Option<String>,
}

fn parse_model_discovery_page(
    body: &[u8],
) -> Result<CustomModelDiscoveryPage, CustomModelDiscoveryFailure> {
    let value: Value = serde_json::from_slice(body).map_err(|_| CustomModelDiscoveryFailure {
        message: "Custom model discovery did not return JSON with a data array".to_string(),
    })?;
    let object = value
        .as_object()
        .ok_or_else(|| CustomModelDiscoveryFailure {
            message: "Custom model discovery did not return a JSON object with a data array"
                .to_string(),
        })?;
    let data = object
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| CustomModelDiscoveryFailure {
            message: "Custom model discovery response is missing a data array".to_string(),
        })?;
    let has_more = match object.get("has_more") {
        None => false,
        Some(Value::Bool(value)) => *value,
        Some(_) => {
            return Err(CustomModelDiscoveryFailure {
                message: "Custom model discovery response has an invalid has_more value"
                    .to_string(),
            });
        }
    };
    let cursor = match object.get("last_id") {
        None | Some(Value::Null) => None,
        Some(Value::String(value)) if !value.chars().any(char::is_control) => Some(
            crate::provider::validate_custom_model_id(value).map_err(|_| {
                CustomModelDiscoveryFailure {
                    message: "Custom model discovery response has an invalid last_id cursor"
                        .to_string(),
                }
            })?,
        ),
        Some(_) => {
            return Err(CustomModelDiscoveryFailure {
                message: "Custom model discovery response has an invalid last_id cursor"
                    .to_string(),
            });
        }
    };
    let mut models = Vec::new();
    let mut last_valid_id = None;
    for item in data {
        let Some(id) = item
            .as_object()
            .and_then(|item| item.get("id"))
            .and_then(Value::as_str)
        else {
            continue;
        };
        if id.chars().any(char::is_control) {
            continue;
        }
        let Ok(id) = crate::provider::validate_custom_model_id(id) else {
            continue;
        };
        last_valid_id = Some(id.clone());
        models.push(id);
    }
    Ok(CustomModelDiscoveryPage {
        models,
        has_more,
        last_valid_id,
        cursor,
    })
}

fn advance_model_discovery_cursor(
    url: &mut reqwest::Url,
    seen_cursors: &mut HashSet<String>,
    cursor: &str,
) -> Result<(), CustomModelDiscoveryFailure> {
    if !seen_cursors.insert(cursor.to_ascii_lowercase()) {
        return Err(CustomModelDiscoveryFailure {
            message: "Custom model discovery cursor loop detected".to_string(),
        });
    }
    // The base endpoint was validated above and has no query. Only this
    // encoded cursor is added; no upstream-provided URL is ever followed.
    // Replacing rather than appending avoids a multi-page after_id chain.
    url.set_query(None);
    url.query_pairs_mut().append_pair("after_id", cursor);
    Ok(())
}

async fn read_custom_model_discovery_body(
    response: reqwest::Response,
) -> Result<Vec<u8>, CustomModelDiscoveryFailure> {
    HttpInferenceTransport::read_body_limited(response, MAX_CUSTOM_MODEL_DISCOVERY_BODY_BYTES)
        .await
        .map_err(|error| match error {
            InferenceHttpError::Oversize { .. } => oversized_model_discovery_body(),
            other => CustomModelDiscoveryFailure {
                message: format!("Custom model discovery response body failed: {other}"),
            },
        })
}

#[cfg(test)]
fn model_discovery_body_size_allowed(size: usize) -> Result<(), CustomModelDiscoveryFailure> {
    if size > MAX_CUSTOM_MODEL_DISCOVERY_BODY_BYTES {
        Err(oversized_model_discovery_body())
    } else {
        Ok(())
    }
}

fn oversized_model_discovery_body() -> CustomModelDiscoveryFailure {
    CustomModelDiscoveryFailure {
        message: format!(
            "Custom model discovery response exceeded the {MAX_CUSTOM_MODEL_DISCOVERY_BODY_BYTES}-byte limit"
        ),
    }
}

fn discovery_status_message(status: StatusCode) -> String {
    match status {
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => format!(
            "Custom model discovery authentication failed (upstream returned {})",
            status.as_u16()
        ),
        StatusCode::NOT_FOUND | StatusCode::METHOD_NOT_ALLOWED => format!(
            "Custom model discovery is unsupported at this endpoint (upstream returned {})",
            status.as_u16()
        ),
        StatusCode::TOO_MANY_REQUESTS => {
            "Custom model discovery is rate limited by the upstream (429)".to_string()
        }
        status if status.is_server_error() => format!(
            "Custom model discovery upstream server error ({})",
            status.as_u16()
        ),
        _ => format!(
            "Custom model discovery upstream returned {}",
            status.as_u16()
        ),
    }
}

impl fmt::Display for CustomVerifyFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for CustomVerifyFailure {}

pub fn first_declared_capability(
    capabilities: &[AccountModelCapability],
) -> Option<&AccountModelCapability> {
    capabilities.first()
}

/// Every declared model has exactly one capability row using the account's
/// single upstream protocol.
pub fn validate_custom_capability_expansion(
    protocol: UpstreamProtocolKind,
    capabilities: &[AccountModelCapabilityInput],
) -> Result<(), String> {
    let mut seen = HashSet::new();
    for capability in capabilities {
        if capability.protocol != protocol {
            return Err(
                "model capability protocol must equal account custom_config.upstream_protocol"
                    .to_string(),
            );
        }
        if !seen.insert(capability.public_model.to_ascii_lowercase()) {
            return Err(format!(
                "duplicate model capability `{}` for the single Custom upstream protocol",
                capability.public_model
            ));
        }
    }
    Ok(())
}

pub fn minimal_verification_body(
    protocol: UpstreamProtocolKind,
    model_id: &str,
) -> Result<Vec<u8>, CustomVerifyFailure> {
    let body = match protocol {
        UpstreamProtocolKind::ChatCompletions => json!({
            "model": model_id,
            "messages": [{"role": "user", "content": "ping"}],
            "max_tokens": 1,
            "stream": false
        }),
        UpstreamProtocolKind::Responses => json!({
            "model": model_id,
            "input": "ping",
            // OpenAI-compatible Responses rejects budgets below 16.
            "max_output_tokens": 16,
            "store": false,
            "stream": false
        }),
        UpstreamProtocolKind::Messages => json!({
            "model": model_id,
            "max_tokens": 1,
            "stream": false,
            "messages": [{"role": "user", "content": "ping"}]
        }),
    };
    serde_json::to_vec(&body).map_err(|error| CustomVerifyFailure {
        message: format!("failed to encode Custom verification request: {error}"),
    })
}

/// POST one protocol-correct non-stream request to the resolved inference endpoint.
/// Requires a successful response in the requested protocol, not arbitrary JSON.
/// Never uses GET /models or mutates capabilities.
pub async fn probe_custom_connection(
    config: &AppConfig,
    custom_config: &AccountCustomConfig,
    first_capability: &AccountModelCapability,
    api_key: &str,
) -> Result<(), CustomVerifyFailure> {
    probe_connection_with_auth(
        config,
        custom_config,
        first_capability,
        Some(custom_auth_scheme(custom_config.upstream_protocol)),
        api_key,
    )
    .await
}

pub(crate) async fn probe_connection_with_auth(
    config: &AppConfig,
    custom_config: &AccountCustomConfig,
    first_capability: &AccountModelCapability,
    auth: Option<crate::provider::UpstreamAuthScheme>,
    api_key: &str,
) -> Result<(), CustomVerifyFailure> {
    let client = CustomHttpClient::from_config(config)?;
    probe_custom_protocol(
        config,
        custom_config,
        custom_config.upstream_protocol,
        &first_capability.upstream_model,
        auth,
        api_key,
        &client,
    )
    .await
}

async fn probe_custom_protocol(
    config: &AppConfig,
    custom_config: &AccountCustomConfig,
    protocol: UpstreamProtocolKind,
    model_id: &str,
    auth: Option<crate::provider::UpstreamAuthScheme>,
    api_key: &str,
    client: &CustomHttpClient,
) -> Result<(), CustomVerifyFailure> {
    let url = resolve_custom_endpoints(&custom_config.endpoint_url, protocol)
        .map_err(|error| CustomVerifyFailure {
            message: format!("invalid Custom verification endpoint: {error}"),
        })?
        .inference;
    let body = minimal_verification_body(protocol, model_id)?;
    let extra =
        json_content_headers(protocol == UpstreamProtocolKind::Messages).map_err(|error| {
            CustomVerifyFailure {
                message: error.to_string(),
            }
        })?;
    let response = client
        .send_isolated_optional(
            reqwest::Method::POST,
            url,
            auth.map(|scheme| (scheme, api_key)),
            extra,
            Some(body),
            Some(Duration::from_secs(config.non_stream_timeout_secs.min(30))),
        )
        .await
        .map_err(|error| CustomVerifyFailure {
            message: format!("Custom verification request failed: {error}"),
        })?;
    let status = response.status();
    let bytes = read_custom_verification_body(response).await?;
    prove_verified_protocol_response(status, &bytes, protocol)
}

async fn read_custom_verification_body(
    response: reqwest::Response,
) -> Result<Vec<u8>, CustomVerifyFailure> {
    HttpInferenceTransport::read_body_limited(response, MAX_CUSTOM_VERIFICATION_BODY_BYTES)
        .await
        .map_err(|error| match error {
            InferenceHttpError::Oversize { .. } => oversized_verification_body(),
            other => CustomVerifyFailure {
                message: format!("Custom verification response body failed: {other}"),
            },
        })
}

fn oversized_verification_body() -> CustomVerifyFailure {
    CustomVerifyFailure {
        message: format!(
            "Custom verification response exceeded the {MAX_CUSTOM_VERIFICATION_BODY_BYTES}-byte limit"
        ),
    }
}

pub(crate) fn prove_verified_protocol_response(
    status: StatusCode,
    body: &[u8],
    protocol: UpstreamProtocolKind,
) -> Result<(), CustomVerifyFailure> {
    if !status.is_success() {
        return Err(CustomVerifyFailure {
            message: format!("Custom verification upstream returned {}", status.as_u16()),
        });
    }
    let parsed: Value = serde_json::from_slice(body).map_err(|_| CustomVerifyFailure {
        message: "Custom verification did not return a JSON object".to_string(),
    })?;
    if !parsed.is_object() {
        return Err(CustomVerifyFailure {
            message: "Custom verification did not return a JSON object".to_string(),
        });
    }
    if parsed.get("error").is_some_and(|error| !error.is_null())
        || parsed.get("type").and_then(Value::as_str) == Some("error")
    {
        return Err(CustomVerifyFailure {
            message: "upstream returned a protocol error".to_string(),
        });
    }
    let valid = match protocol {
        UpstreamProtocolKind::ChatCompletions => parsed
            .get("choices")
            .and_then(Value::as_array)
            .is_some_and(|choices| {
                !choices.is_empty()
                    && choices
                        .iter()
                        .all(|choice| choice.get("message").is_some_and(Value::is_object))
            }),
        UpstreamProtocolKind::Responses => {
            parsed.get("object").and_then(Value::as_str) == Some("response")
                && parsed.get("output").is_some_and(Value::is_array)
                && !matches!(
                    parsed.get("status").and_then(Value::as_str),
                    Some("failed" | "cancelled" | "queued" | "in_progress")
                )
        }
        UpstreamProtocolKind::Messages => {
            parsed.get("type").and_then(Value::as_str) == Some("message")
                && parsed.get("role").and_then(Value::as_str) == Some("assistant")
                && parsed.get("content").is_some_and(Value::is_array)
        }
    };
    if !valid {
        return Err(CustomVerifyFailure {
            message: format!("upstream response does not match {protocol:?}"),
        });
    }
    Ok(())
}

impl CustomHttpClient {
    fn from_config(config: &AppConfig) -> Result<Self, CustomVerifyFailure> {
        custom_http::build_custom_http_client(config).map_err(|error| CustomVerifyFailure {
            message: format!("failed to build Custom HTTP client: {error}"),
        })
    }
}

#[cfg(test)]
mod tests;

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
//!
//! Format-specific rules stay in the private child modules. There is no shared
//! intermediate representation: each format pair converts with its own rules,
//! and cross-format response pairs still route through the Messages document
//! exactly as before.

mod reasoning;
mod request;
mod response;
mod shared;
mod tools;
mod usage;
mod validation;

use ocg_domain::protocol::ApiFormat;
use serde_json::{Value, json};
use std::fmt;

pub use reasoning::{
    decode_anthropic_thinking_block, decode_chat_reasoning, encode_anthropic_thinking_block,
    encode_chat_reasoning,
};

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
    validation::validate_request_features(client, upstream, &body)?;
    let converting = client != upstream;
    let (tool_context, legacy_tool_compat) = if client == ApiFormat::Responses {
        tools::responses_tool_context(&body, converting)?
    } else {
        (tools::ResponsesToolContext::default(), None)
    };
    let body = request::convert_request(client, upstream, body, &tool_context.namespace_tools)?;
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
        let object = body
            .as_object_mut()
            .ok_or_else(|| ConversionError::new("Messages response must be a JSON object"))?;
        if let Some(model) = model_hint {
            object.insert("model".to_string(), json!(model));
        }
    }
    let transformed = response::convert_between(
        upstream,
        client,
        &body,
        custom_tools,
        namespace_tools,
        model_hint,
        &synthesis,
    )?;
    Ok(ResponseConversion { body: transformed })
}

/// Checks only client features; no candidate protocol or conversion is required.
#[doc(hidden)]
pub fn validate_client_request_features(
    client: ApiFormat,
    body: &Value,
) -> Result<(), ConversionError> {
    validation::validate_client_request_features(client, body)
}

#[cfg(test)]
mod tests;

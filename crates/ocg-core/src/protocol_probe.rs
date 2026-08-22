//! HTTP-neutral admin protocol-probe transport and observation orchestration.
//!
//! Dashboard V2 and V3 share this module. Callers own HTTP envelopes, CAS,
//! persistence, and revision bumps. This module never imports dashboard
//! surfaces, never calls `forward_once` / the executor, and never selects a
//! model-exception proxy leg.

use crate::custom_http::{
    HttpInferenceTransport, HttpInferenceTransportSpec, InferenceHttpRequest, json_content_headers,
};
use crate::gateway::attempt::UpstreamAuth;
use crate::gateway::protocol::{CustomRouteSpec, RequestPlan};
use crate::gateway::provider_adapter::resolve_probe_route;
use crate::models::{Account, AppConfig, UpstreamChannel};
use crate::provider::{ProviderAdapterKind, UpstreamAuthScheme, UpstreamProtocolKind};
use crate::provider_contracts::{self, ContractScope, PersistedModelProtocol, protocol_to_api};
use crate::state::CoreState;
use chrono::{DateTime, Utc};
use std::collections::HashSet;

pub const CEILING_SKIP_MESSAGE: &str = "probe combination is outside the adapter safety ceiling";

#[derive(Debug, Clone)]
pub struct ProtocolProbeOutcome {
    pub protocol: UpstreamProtocolKind,
    pub success: bool,
    pub skipped: bool,
    pub error: Option<String>,
    pub observation: Option<PersistedModelProtocol>,
}

#[derive(Debug)]
pub enum ProtocolProbeRunError {
    Evidence(String),
    Apply(String),
    Persist(String),
}

pub struct ProtocolProbeContext<'a> {
    pub state: &'a CoreState,
    pub config: &'a AppConfig,
    pub account: &'a Account,
    pub adapter: ProviderAdapterKind,
    pub model_id: &'a str,
    pub custom_route: Option<CustomRouteSpec>,
    pub now: DateTime<Utc>,
}

pub fn require_unique_probe_protocols(protocols: &[UpstreamProtocolKind]) -> Result<(), String> {
    let mut seen = HashSet::new();
    for protocol in protocols {
        if !seen.insert(*protocol) {
            return Err("duplicate upstream protocol".to_string());
        }
    }
    Ok(())
}

pub async fn run_protocol_probes<L, P>(
    ctx: &ProtocolProbeContext<'_>,
    scope: &ContractScope,
    protocols: &[UpstreamProtocolKind],
    declared: &[(String, UpstreamProtocolKind)],
    mut load_existing: L,
    mut persist: P,
) -> Result<Vec<ProtocolProbeOutcome>, ProtocolProbeRunError>
where
    L: FnMut(UpstreamProtocolKind) -> Result<Option<PersistedModelProtocol>, String>,
    P: FnMut(&PersistedModelProtocol) -> Result<(), String>,
{
    let mut results = Vec::with_capacity(protocols.len());
    for protocol in protocols {
        if !provider_contracts::probe_may_add(ctx.adapter, ctx.model_id, *protocol, declared) {
            results.push(ProtocolProbeOutcome {
                protocol: *protocol,
                success: false,
                skipped: true,
                error: Some(CEILING_SKIP_MESSAGE.to_string()),
                observation: None,
            });
            continue;
        }
        let existing = load_existing(*protocol).map_err(ProtocolProbeRunError::Evidence)?;
        let (success, error) = match execute_protocol_probe(ctx, *protocol).await {
            Ok(()) => (true, None),
            Err(message) => (false, Some(message)),
        };
        let persisted = provider_contracts::apply_probe_observation(
            existing.as_ref(),
            scope.clone(),
            ctx.model_id,
            *protocol,
            success,
            error.clone(),
            ctx.now,
            true,
        )
        .map_err(ProtocolProbeRunError::Apply)?;
        persist(&persisted).map_err(ProtocolProbeRunError::Persist)?;
        results.push(ProtocolProbeOutcome {
            protocol: *protocol,
            success,
            skipped: false,
            error,
            observation: Some(persisted),
        });
    }
    Ok(results)
}

pub async fn execute_protocol_probe(
    ctx: &ProtocolProbeContext<'_>,
    protocol: UpstreamProtocolKind,
) -> Result<(), String> {
    let format = protocol_to_api(protocol);
    let body = crate::custom::minimal_verification_body(protocol, ctx.model_id)
        .map_err(|error| error.message)?;
    let plan = RequestPlan {
        client: format,
        upstream: format,
        model: ctx.model_id.to_string(),
        client_model: ctx.model_id.to_string(),
        stream: false,
        body: bytes::Bytes::from(body.clone()),
        channel: if ctx.adapter == ProviderAdapterKind::ZenFree {
            UpstreamChannel::Free
        } else {
            UpstreamChannel::Go
        },
        upstream_base_override: None,
        original_model: None,
        allow_go_fallback: false,
        resolved_alias: None,
        custom_route: ctx.custom_route.clone(),
        service_tier: None,
        custom_tools: Vec::new(),
        namespace_tools: Vec::new(),
        response_parallel_tool_calls: true,
        response_tool_choice: serde_json::json!("auto"),
        response_tools: Vec::new(),
    };
    if ctx.adapter == ProviderAdapterKind::ConfigurableHttp && plan.custom_route.is_none() {
        return Err(
            "Custom API accounts require a persisted base URL, protocol, and auth scheme"
                .to_string(),
        );
    }
    let route = resolve_probe_route(ctx.account, ctx.config, &plan)?;
    let secret = if matches!(route.auth, UpstreamAuth::None) {
        None
    } else {
        Some(
            ctx.state
                .decrypt_key(&ctx.account.key_cipher)
                .map_err(|error| error.to_string())?,
        )
    };
    let spec = if route.follow_redirects {
        HttpInferenceTransportSpec::follow_redirects()
    } else {
        HttpInferenceTransportSpec::no_redirects()
    };
    let transport =
        HttpInferenceTransport::build(ctx.config, spec).map_err(|error| error.to_string())?;
    let url = HttpInferenceTransport::join_endpoint(&route.base_url, &route.path)
        .map_err(|error| error.to_string())?;
    let extra = json_content_headers(protocol == UpstreamProtocolKind::Messages)
        .map_err(|error| error.to_string())?;
    let timeout = std::time::Duration::from_secs(ctx.config.non_stream_timeout_secs.clamp(5, 30));
    let auth = match (route.auth, secret.as_deref()) {
        (UpstreamAuth::None, _) => None,
        (UpstreamAuth::XApiKey, Some(key)) => Some((UpstreamAuthScheme::XApiKey, key)),
        (UpstreamAuth::Bearer, Some(key)) => Some((UpstreamAuthScheme::Bearer, key)),
        (UpstreamAuth::OpenCodeProtocolDefault, Some(key))
            if format == crate::kernel::protocol::ApiFormat::Messages =>
        {
            Some((UpstreamAuthScheme::XApiKey, key))
        }
        (UpstreamAuth::OpenCodeProtocolDefault, Some(key)) => {
            Some((UpstreamAuthScheme::Bearer, key))
        }
        (_, None) => {
            return Err("account is missing a decrypted credential for this probe".to_string());
        }
    };
    let response = transport
        .send(InferenceHttpRequest {
            method: reqwest::Method::POST,
            url,
            auth,
            extra_headers: extra,
            body: Some(body),
            request_timeout: Some(timeout),
        })
        .await
        .map_err(|error| {
            provider_contracts::sanitize_probe_error(&error.to_string(), secret.as_deref())
        })?;
    let status = response.status();
    let bytes = HttpInferenceTransport::read_body_limited(
        response,
        crate::custom::MAX_CUSTOM_VERIFICATION_BODY_BYTES,
    )
    .await
    .map_err(|error| {
        provider_contracts::sanitize_probe_error(&error.to_string(), secret.as_deref())
    })?;
    if !status.is_success() {
        let raw = String::from_utf8_lossy(&bytes);
        return Err(provider_contracts::sanitize_probe_error(
            &format!("upstream returned {} {raw}", status.as_u16()),
            secret.as_deref(),
        ));
    }
    let parsed: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|_| "protocol probe did not return a JSON object".to_string())?;
    if !parsed.is_object() {
        return Err("protocol probe did not return a JSON object".to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unique_protocols_preserve_caller_order_and_reject_duplicates() {
        let unique = [
            UpstreamProtocolKind::ChatCompletions,
            UpstreamProtocolKind::Responses,
        ];
        require_unique_probe_protocols(&unique).expect("unique caller order is preserved");
        let error = require_unique_probe_protocols(&[
            UpstreamProtocolKind::ChatCompletions,
            UpstreamProtocolKind::Responses,
            UpstreamProtocolKind::ChatCompletions,
        ])
        .expect_err("duplicates must fail locally");
        assert!(error.contains("duplicate"));
    }
}

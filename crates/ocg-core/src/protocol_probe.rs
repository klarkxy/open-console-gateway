//! HTTP-neutral admin protocol-probe transport and observation orchestration.
//!
//! Dashboard V3 owns the live entrypoint. Callers own HTTP envelopes, CAS,
//! catalog admission, persistence, and revision bumps. This module never imports dashboard
//! surfaces, never calls `forward_once` / the executor, and never selects a
//! model-exception proxy leg.

use crate::custom_http::{
    HttpInferenceTransport, HttpInferenceTransportSpec, InferenceHttpRequest, json_content_headers,
};
use crate::gateway::attempt::UpstreamAuth;
use crate::gateway::forwarder::{
    LiveSendAccountGate, LiveSendSelection, authorize_live_send_secret, confirm_live_send_secret,
};
use crate::gateway::protocol::{CustomRouteSpec, RequestPlan};
use crate::gateway::provider_adapter::{
    resolve_account_test_route_with_dynamics, resolve_probe_route,
};
use crate::models::{Account, AppConfig, UpstreamChannel};
use crate::provider::{ProviderAdapterKind, UpstreamAuthScheme, UpstreamProtocolKind};
use crate::provider_contracts::{self, ContractScope, PersistedModelProtocol, protocol_to_api};
use crate::state::CoreState;
use chrono::{DateTime, Utc};
use std::collections::HashSet;
use std::time::Instant;

#[derive(Debug, Clone)]
pub(crate) struct ProtocolProbeOutcome {
    pub protocol: UpstreamProtocolKind,
    pub success: bool,
    pub skipped: bool,
    pub error: Option<String>,
    pub observation: Option<PersistedModelProtocol>,
    pub attempts: Vec<ProtocolProbeAttempt>,
}

#[derive(Debug, Clone)]
pub(crate) struct ProtocolProbeAttempt {
    pub account_id: String,
    pub protocol: UpstreamProtocolKind,
    pub success: bool,
    pub http_status: Option<i32>,
    pub error: Option<String>,
    pub duration_ms: i64,
}

#[derive(Debug)]
pub(crate) enum ProtocolProbeRunError {
    Evidence(String),
    Apply(String),
}

pub(crate) struct ProtocolProbeContext<'a> {
    pub state: &'a CoreState,
    pub config: &'a AppConfig,
    pub accounts: &'a [Account],
    pub adapter: ProviderAdapterKind,
    pub model_id: &'a str,
    pub custom_route: Option<CustomRouteSpec>,
    pub now: DateTime<Utc>,
}

pub(crate) fn require_unique_probe_protocols(
    protocols: &[UpstreamProtocolKind],
) -> Result<(), String> {
    let mut seen = HashSet::new();
    for protocol in protocols {
        if !seen.insert(*protocol) {
            return Err("duplicate upstream protocol".to_string());
        }
    }
    Ok(())
}

pub(crate) async fn run_protocol_probes<L>(
    ctx: &ProtocolProbeContext<'_>,
    scope: &ContractScope,
    protocols: &[UpstreamProtocolKind],
    mut load_existing: L,
) -> Result<Vec<ProtocolProbeOutcome>, ProtocolProbeRunError>
where
    L: FnMut(UpstreamProtocolKind) -> Result<Option<PersistedModelProtocol>, String>,
{
    let mut results = Vec::with_capacity(protocols.len());
    for protocol in protocols {
        let existing = load_existing(*protocol).map_err(ProtocolProbeRunError::Evidence)?;
        let mut attempts = Vec::with_capacity(ctx.accounts.len());
        let mut success = false;
        let mut error = None;
        for account in ctx.accounts {
            let attempt_started = Instant::now();
            let (attempt_success, attempt_status, attempt_error) =
                match execute_protocol_probe(ctx, account, *protocol).await {
                    Ok(status) => (true, Some(i32::from(status)), None),
                    Err((status, message)) => (false, status.map(i32::from), Some(message)),
                };
            attempts.push(ProtocolProbeAttempt {
                account_id: account.id.clone(),
                protocol: *protocol,
                success: attempt_success,
                http_status: attempt_status,
                error: attempt_error.clone(),
                duration_ms: attempt_started.elapsed().as_millis().min(i64::MAX as u128) as i64,
            });
            error = attempt_error;
            if attempt_success {
                success = true;
                error = None;
                break;
            }
        }
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
        results.push(ProtocolProbeOutcome {
            protocol: *protocol,
            success,
            skipped: false,
            error,
            observation: Some(persisted),
            attempts,
        });
    }
    Ok(results)
}

pub(crate) async fn execute_protocol_probe(
    ctx: &ProtocolProbeContext<'_>,
    account: &Account,
    protocol: UpstreamProtocolKind,
) -> Result<u16, (Option<u16>, String)> {
    execute_protocol_request(ctx, account, protocol, false, &[]).await
}

/// Send the same minimal protocol request used by provider probes, but lock
/// routing to the caller-selected account and retain the production route
/// family for Plans whose provider probes are intentionally unavailable.
pub(crate) struct AccountModelTestInput<'a> {
    pub state: &'a CoreState,
    pub config: &'a AppConfig,
    pub account: &'a Account,
    pub adapter: ProviderAdapterKind,
    pub model_id: &'a str,
    pub protocol: UpstreamProtocolKind,
    pub custom_endpoint_url: Option<&'a str>,
    pub dynamics: &'a [crate::dynamic::DynamicProviderRuntime],
}

pub(crate) async fn execute_account_model_test(
    input: AccountModelTestInput<'_>,
) -> Result<u16, (Option<u16>, String)> {
    let custom_route = input
        .custom_endpoint_url
        .map(|endpoint_url| CustomRouteSpec {
            endpoint_url: endpoint_url.to_string(),
        });
    let ctx = ProtocolProbeContext {
        state: input.state,
        config: input.config,
        accounts: std::slice::from_ref(input.account),
        adapter: input.adapter,
        model_id: input.model_id,
        custom_route,
        now: chrono::Utc::now(),
    };
    execute_protocol_request(&ctx, input.account, input.protocol, true, input.dynamics).await
}

async fn execute_protocol_request(
    ctx: &ProtocolProbeContext<'_>,
    account: &Account,
    protocol: UpstreamProtocolKind,
    account_test: bool,
    dynamics: &[crate::dynamic::DynamicProviderRuntime],
) -> Result<u16, (Option<u16>, String)> {
    let format = protocol_to_api(protocol);
    let body = crate::custom::minimal_verification_body(protocol, ctx.model_id)
        .map_err(|error| (None, error.message))?;
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
        resolved_alias: None,
        custom_route: ctx.custom_route.clone(),
        service_tier: None,
        custom_tools: Vec::new(),
        namespace_tools: Vec::new(),
        legacy_tool_compat: None,
        response_parallel_tool_calls: true,
        response_tool_choice: serde_json::json!("auto"),
        response_tools: Vec::new(),
    };
    let route = if account_test {
        resolve_account_test_route_with_dynamics(account, ctx.adapter, ctx.config, &plan, dynamics)
    } else {
        resolve_probe_route(account, ctx.adapter, ctx.config, &plan)
    }
    .map_err(|error| (None, error))?;
    let selection = {
        let db = ctx.state.db.lock();
        let binding = db
            .list_inference_bindings()
            .ok()
            .and_then(|rows| rows.into_iter().find(|row| row.account_id == account.id));
        LiveSendSelection::from_binding(
            account,
            binding.as_ref(),
            ctx.model_id,
            ctx.model_id,
            ctx.model_id,
        )
    };
    let secret = authorize_live_send_secret(
        ctx.state,
        &selection,
        account,
        &plan,
        &route,
        LiveSendAccountGate::AllowDisabled,
    )
    .map_err(|error| (None, error.to_string()))?;
    let spec = if crate::custom_http::follows_redirects_with_secret(
        route.follow_redirects,
        secret.is_some(),
    ) {
        HttpInferenceTransportSpec::follow_redirects()
    } else {
        HttpInferenceTransportSpec::no_redirects()
    };
    let isolated = matches!(
        route.proxy_routing,
        crate::gateway::attempt::ProxyRoutingModel::IsolatedTrustedAdmin
    );
    let transport = if isolated {
        HttpInferenceTransport::build_isolated_trusted_admin(ctx.config, spec)
    } else {
        HttpInferenceTransport::build(ctx.config, spec)
    }
    .map_err(|error| (None, error.to_string()))?;
    let url = HttpInferenceTransport::join_endpoint(&route.base_url, &route.path)
        .map_err(|error| (None, error.to_string()))?;
    let mut extra = json_content_headers(protocol == UpstreamProtocolKind::Messages)
        .map_err(|error| (None, error.to_string()))?;
    crate::gateway::forwarder::apply_provider_identity_headers(
        &mut extra,
        &reqwest::header::HeaderMap::new(),
        ctx.adapter,
        format,
        ctx.model_id,
        &body,
        &uuid::Uuid::new_v4().to_string(),
    );
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
            return Err((
                None,
                "account is missing a decrypted credential for this probe".to_string(),
            ));
        }
    };
    confirm_live_send_secret(
        ctx.state,
        &selection,
        account,
        &plan,
        &route,
        LiveSendAccountGate::AllowDisabled,
    )
    .map_err(|error| (None, error.to_string()))?;
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
            (
                None,
                provider_contracts::sanitize_probe_error(&error.to_string(), secret.as_deref()),
            )
        })?;
    let status = response.status();
    let status_code = status.as_u16();
    let bytes = HttpInferenceTransport::read_body_limited(
        response,
        crate::custom::MAX_CUSTOM_VERIFICATION_BODY_BYTES,
    )
    .await
    .map_err(|error| {
        (
            Some(status_code),
            provider_contracts::sanitize_probe_error(&error.to_string(), secret.as_deref()),
        )
    })?;
    if !status.is_success() {
        if account_test {
            return Err((
                Some(status_code),
                format!("upstream returned HTTP {status_code}"),
            ));
        }
        let raw = String::from_utf8_lossy(&bytes);
        return Err((
            Some(status_code),
            provider_contracts::sanitize_probe_error(
                &format!("upstream returned {status_code} {raw}"),
                secret.as_deref(),
            ),
        ));
    }
    let parsed: serde_json::Value = serde_json::from_slice(&bytes).map_err(|_| {
        (
            Some(status_code),
            "protocol probe did not return a JSON object".to_string(),
        )
    })?;
    if !parsed.is_object() {
        return Err((
            Some(status_code),
            "protocol probe did not return a JSON object".to_string(),
        ));
    }
    if let Some(error) = non_null_probe_error(&parsed) {
        if account_test {
            return Err((
                Some(status_code),
                "upstream returned a protocol error".to_string(),
            ));
        }
        return Err((
            Some(status_code),
            provider_contracts::sanitize_probe_error(
                &format!("protocol probe returned an error object: {error}"),
                secret.as_deref(),
            ),
        ));
    }
    crate::custom::prove_verified_protocol_response(status, &bytes, protocol)
        .map_err(|failure| (Some(status_code), failure.message))?;
    Ok(status_code)
}

fn non_null_probe_error(value: &serde_json::Value) -> Option<&serde_json::Value> {
    value.get("error").filter(|error| !error.is_null())
}

#[cfg(test)]
mod tests;

//! Authenticated Custom model-list probe.
//!
//! This is not a control mutation: it does not require `expectedRevision` and
//! never bumps the settings revision. The handler snapshots config and CAS
//! tokens, drops every lock before the upstream await, and reuses the V2
//! Custom discovery implementation plus the trusted-admin HTTP boundary
//! (syntax-valid endpoint, isolated auth, no redirects, no dashboard/client
//! credential forwarding, no catalog or account persistence).

use axum::Json;
use axum::body::Bytes;
use axum::extract::State;

use crate::custom;
use crate::models::{Account as ModelAccount, AccountCustomConfigInput};
use crate::redaction::redact_known_secret;
use crate::state::CoreState;

use super::types::{ControlRevision, CustomModelDiscoveryRequest, CustomModelDiscoveryResponse};
use super::{V3ApiError, parse_json};

pub(super) async fn discover_custom_models(
    State(state): State<CoreState>,
    body: Bytes,
) -> Result<Json<CustomModelDiscoveryResponse>, V3ApiError> {
    let mut op = super::settings::open_dashboard(&state, "custom.models.discover", "account", None);
    let result = discover_custom_models_inner(state.clone(), body, &mut op).await;
    let counts = match &result {
        Ok(Json(response)) => {
            let count = super::settings::count_u32(response.models.len() as u64);
            (Some(count), Some(count), None)
        }
        Err(_) => (Some(1), None, Some(1)),
    };
    super::settings::record_after(op, &state, &[], counts, None, result)
}

async fn discover_custom_models_inner(
    state: CoreState,
    body: Bytes,
    op: &mut crate::user_operation::UserOperation,
) -> Result<Json<CustomModelDiscoveryResponse>, V3ApiError> {
    let input = parse_json::<CustomModelDiscoveryRequest>(&body)?;
    if let Some(account_id) = input
        .account_id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
        && let Some(id) = super::settings::known_subject(account_id)
    {
        op.subject(id);
    }
    let captured = ControlRevision::from_state(&state);
    let config = state.config();
    let job = prepare_custom_model_discovery(&state, input)?;
    let discovery = custom::discover_custom_models(&config, &job.custom_config, &job.api_key)
        .await
        .map_err(|failure| map_discovery_failure(&state, &job.api_key, failure))?;
    Ok(Json(CustomModelDiscoveryResponse {
        models: models_without_selected_credential(discovery.models, &job.api_key),
        truncated: discovery.truncated,
        revision: captured.revision,
        process_generation: captured.process_generation,
        pricing_revision: captured.pricing_revision,
    }))
}

struct PreparedCustomModelDiscovery {
    custom_config: AccountCustomConfigInput,
    api_key: String,
}

fn prepare_custom_model_discovery(
    state: &CoreState,
    input: CustomModelDiscoveryRequest,
) -> Result<PreparedCustomModelDiscovery, V3ApiError> {
    let supplied_key = input
        .api_key
        .as_deref()
        .map(str::trim)
        .filter(|key| !key.is_empty());
    let stored_key = if supplied_key.is_none()
        && let Some(account_id) = input
            .account_id
            .as_deref()
            .map(str::trim)
            .filter(|id| !id.is_empty())
    {
        stored_custom_api_key(state, account_id)?
    } else {
        None
    };
    let api_key = supplied_key
        .map(str::to_owned)
        .or(stored_key)
        .filter(|key| !key.trim().is_empty())
        .ok_or_else(|| {
            V3ApiError::invalid_request_at(
                state,
                "Custom model discovery requires an API key or an existing Custom account with a stored key",
            )
        })?;
    Ok(PreparedCustomModelDiscovery {
        custom_config: AccountCustomConfigInput {
            endpoint_url: input.endpoint_url,
            upstream_protocol: input.upstream_protocol.into(),
        },
        api_key,
    })
}

fn stored_custom_api_key(
    state: &CoreState,
    account_id: &str,
) -> Result<Option<String>, V3ApiError> {
    let account = {
        let db = state.db.lock();
        db.get_account(account_id)
            .map_err(V3ApiError::internal)?
            .ok_or_else(|| V3ApiError::not_found(state))?
    };
    require_custom_plan(state, &account)?;
    if account.key_cipher.is_empty() {
        Ok(None)
    } else {
        state
            .decrypt_key(&account.key_cipher)
            .map(Some)
            .map_err(V3ApiError::internal)
    }
}

fn require_custom_plan(state: &CoreState, account: &ModelAccount) -> Result<(), V3ApiError> {
    let plan = crate::provider::builtin_provider(&account.provider_id)
        .ok_or_else(|| V3ApiError::invalid_request_at(state, "unknown provider offering"))?;
    if crate::provider::plan_requires_custom_config(plan) {
        Ok(())
    } else {
        Err(V3ApiError::invalid_request_at(
            state,
            "model discovery is only available for Custom API accounts",
        ))
    }
}

/// Drop upstream-controlled IDs that embed the selected credential.
/// Do not log dropped IDs or name which credential matched.
fn models_without_selected_credential(models: Vec<String>, api_key: &str) -> Vec<String> {
    if api_key.is_empty() {
        return models;
    }
    models
        .into_iter()
        .filter(|model_id| !model_id.contains(api_key))
        .collect()
}

fn map_discovery_failure(
    state: &CoreState,
    api_key: &str,
    failure: custom::CustomModelDiscoveryFailure,
) -> V3ApiError {
    let message = redact_known_secret(&failure.message, api_key);
    if is_outbound_discovery_failure(&message) {
        V3ApiError::outbound_failed(state, message)
    } else {
        V3ApiError::invalid_request_at(state, message)
    }
}

fn is_outbound_discovery_failure(message: &str) -> bool {
    message.contains("timed out")
        || message.contains("network or timeout")
        || message.contains("upstream server error")
        || message.contains("failed to build Custom HTTP client")
        || message.contains("response body failed")
}

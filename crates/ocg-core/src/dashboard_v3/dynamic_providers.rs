//! Dynamic Provider control plane. Distinct from Custom API.

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, State};
use chrono::Utc;
use ocg_domain::dynamic::{
    DynamicAuthKind, DynamicModelMapping, DynamicProviderDefinition, normalize_dynamic_mappings,
    normalize_dynamic_provider_name,
};
use ocg_domain::provider::{ProviderOrigin, ProviderRegistry, builtin_provider};

use crate::custom;
use crate::custom::validate_custom_endpoint_url;
use crate::dynamic::{DynamicProviderRuntime, collides_with_known_id, validate_definition};
use crate::models::{
    Account as ModelAccount, AccountCustomConfig, AccountCustomConfigInput, AccountModelCapability,
    AccountType, normalize_account_notes,
};
use crate::redaction::redact_known_secret;
use crate::state::CoreState;

use super::types::{
    ControlRevision, MutationAck, MutationExpectation, ProviderDefinition,
    ProviderDefinitionCreate, ProviderDefinitionDiscoverRequest,
    ProviderDefinitionDiscoverResponse, ProviderDefinitionModel, ProviderDefinitionMutation,
    ProviderDefinitionTestRequest, ProviderDefinitionTestResponse, ProviderDefinitionUpdate,
};
use super::{V3ApiError, check_expectation, parse_json, parse_mutation_json};

pub(super) async fn create_provider(
    State(state): State<CoreState>,
    body: Bytes,
) -> Result<Json<ProviderDefinitionMutation>, V3ApiError> {
    let input = parse_mutation_json::<ProviderDefinitionCreate>(&body)?;
    create_locked(&state, input).map(Json)
}

pub(super) async fn get_provider(
    State(state): State<CoreState>,
    Path(provider_id): Path<String>,
) -> Result<Json<ProviderDefinition>, V3ApiError> {
    let captured = ControlRevision::from_state(&state);
    let runtime = state
        .db
        .lock()
        .get_provider_definition(&provider_id)
        .map_err(V3ApiError::internal)?
        .ok_or_else(|| V3ApiError::not_found_at(&state, "provider not found"))?;
    Ok(Json(to_wire(
        runtime,
        captured.revision,
        captured.process_generation,
    )))
}

pub(super) async fn update_provider(
    State(state): State<CoreState>,
    Path(provider_id): Path<String>,
    body: Bytes,
) -> Result<Json<ProviderDefinitionMutation>, V3ApiError> {
    let input = parse_mutation_json::<ProviderDefinitionUpdate>(&body)?;
    update_locked(&state, &provider_id, input).map(Json)
}

pub(super) async fn delete_provider(
    State(state): State<CoreState>,
    Path(provider_id): Path<String>,
    body: Bytes,
) -> Result<Json<MutationAck>, V3ApiError> {
    let expectation = parse_mutation_json::<MutationExpectation>(&body)?;
    delete_locked(&state, &provider_id, &expectation).map(Json)
}

pub(super) async fn discover_models(
    State(state): State<CoreState>,
    body: Bytes,
) -> Result<Json<ProviderDefinitionDiscoverResponse>, V3ApiError> {
    let input = parse_json::<ProviderDefinitionDiscoverRequest>(&body)?;
    let captured = ControlRevision::from_state(&state);
    let config = state.config();
    let endpoint = validate_custom_endpoint_url(&input.endpoint_url)
        .map_err(|error| V3ApiError::invalid_request_at(&state, error.to_string()))?;
    let auth_kind = DynamicAuthKind::from(input.auth_kind);
    let key = required_probe_key(&state, auth_kind, input.key.as_deref())?;
    let custom_config = AccountCustomConfigInput {
        endpoint_url: endpoint,
        upstream_protocol: input.upstream_protocol.into(),
    };
    let discovery =
        custom::discover_models_with_auth(&config, &custom_config, auth_kind.upstream_auth(), &key)
            .await
            .map_err(|failure| map_probe_failure(&state, &key, failure.message))?;
    Ok(Json(ProviderDefinitionDiscoverResponse {
        models: models_without_key(discovery.models, &key),
        truncated: discovery.truncated,
        revision: captured.revision,
        process_generation: captured.process_generation,
    }))
}

pub(super) async fn test_provider(
    State(state): State<CoreState>,
    body: Bytes,
) -> Result<Json<ProviderDefinitionTestResponse>, V3ApiError> {
    let input = parse_json::<ProviderDefinitionTestRequest>(&body)?;
    let captured = ControlRevision::from_state(&state);
    let config = state.config();
    let endpoint = validate_custom_endpoint_url(&input.endpoint_url)
        .map_err(|error| V3ApiError::invalid_request_at(&state, error.to_string()))?;
    let auth_kind = DynamicAuthKind::from(input.auth_kind);
    let key = required_probe_key(&state, auth_kind, input.key.as_deref())?;
    let public_model = ocg_domain::provider::validate_custom_model_id(&input.public_model)
        .map_err(|error| V3ApiError::invalid_request_at(&state, error.to_string()))?;
    let upstream_model = ocg_domain::provider::validate_custom_model_id(&input.upstream_model)
        .map_err(|error| V3ApiError::invalid_request_at(&state, error.to_string()))?;
    let custom_config = AccountCustomConfig {
        account_id: String::new(),
        endpoint_url: endpoint,
        upstream_protocol: input.upstream_protocol.into(),
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };
    let capability = AccountModelCapability {
        account_id: String::new(),
        public_model,
        protocol: input.upstream_protocol.into(),
        verified_at: None,
        source: "manual".into(),
        upstream_model,
    };
    let result = custom::probe_connection_with_auth(
        &config,
        &custom_config,
        &capability,
        auth_kind.upstream_auth(),
        &key,
    )
    .await;
    let (ok, error) = match result {
        Ok(()) => (true, None),
        Err(failure) => (false, Some(redact_known_secret(&failure.message, &key))),
    };
    Ok(Json(ProviderDefinitionTestResponse {
        ok,
        error,
        revision: captured.revision,
        process_generation: captured.process_generation,
    }))
}

fn create_locked(
    state: &CoreState,
    input: ProviderDefinitionCreate,
) -> Result<ProviderDefinitionMutation, V3ApiError> {
    let _settings_update = state.settings_update.lock();
    check_expectation(state, &input.expectation)?;
    let now = Utc::now();
    let auth_kind = DynamicAuthKind::from(input.auth_kind);
    let mut definition = validate_wire_definition(
        uuid::Uuid::new_v4().to_string(),
        input.name,
        input.endpoint_url,
        input.upstream_protocol,
        auth_kind,
        input.models,
    )
    .map_err(|error| V3ApiError::invalid_request_at(state, error.to_string()))?;
    definition.preset_id = crate::dynamic::normalize_preset_id(input.preset_id)
        .map_err(|error| V3ApiError::invalid_request_at(state, error.to_string()))?;
    let existing = state.dynamic_providers();
    if collides_with_known_id(&definition.id, &existing) {
        return Err(V3ApiError::conflict_at(
            state,
            "generated provider id collided; retry",
        ));
    }
    let supplied_key = input
        .key
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    // Keyed auth without a Key saves the definition only. Accounts add the Key.
    // No-auth still creates the singleton account because that row is the connection.
    let create_first_account = !auth_kind.requires_key() || supplied_key.is_some();
    let runtime = runtime_from_definition(definition, now, now);
    let first_account = if create_first_account {
        let key_cipher = first_account_key(state, auth_kind, input.key.as_deref())?;
        let account_name = input
            .account_name
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(runtime.name.as_str())
            .to_string();
        let notes = match input.notes.as_deref() {
            Some(value) => normalize_account_notes(value)
                .map_err(|error| V3ApiError::invalid_request_at(state, error.to_string()))?,
            None => None,
        };
        Some(ModelAccount {
            id: uuid::Uuid::new_v4().to_string(),
            provider_id: runtime.id.clone(),
            credential_kind: auth_kind.credential_kind(),
            quota_scope: auth_kind.quota_scope(),
            name: account_name,
            username: None,
            password_cipher: None,
            key_cipher,
            enabled: true,
            account_type: AccountType::Key,
            setup_step: crate::models::AccountSetupStep::Ready,
            referral_code: None,
            purchase_date: String::new(),
            expires_on: String::new(),
            cooldown_until: None,
            cooldown_generic_until: None,
            cooldown_5h_until: None,
            cooldown_week_until: None,
            cooldown_month_until: None,
            cooldown_free_until: None,
            last_error: None,
            auth_error: None,
            notes,
            created_at: now,
            updated_at: now,
        })
    } else {
        None
    };
    let snapshot = {
        let db = state.db.lock();
        match first_account.as_ref() {
            Some(account) => db.create_dynamic_provider(&runtime, account),
            None => db.create_dynamic_provider_definition(&runtime),
        }
        .map_err(|error| V3ApiError::invalid_request_at(state, error.to_string()))?
    };
    state.install_dynamic_providers_snapshot(snapshot);
    Ok(provider_mutation(state, runtime, state.settings_revision()))
}

fn update_locked(
    state: &CoreState,
    provider_id: &str,
    input: ProviderDefinitionUpdate,
) -> Result<ProviderDefinitionMutation, V3ApiError> {
    let _settings_update = state.settings_update.lock();
    check_expectation(state, &input.expectation)?;
    reject_builtin_id(state, provider_id)?;
    let existing = state
        .db
        .lock()
        .get_dynamic_provider(provider_id)
        .map_err(V3ApiError::internal)?
        .ok_or_else(|| V3ApiError::not_found_at(state, "provider not found"))?;
    let auth_kind = DynamicAuthKind::from(input.auth_kind);
    let mut definition = validate_wire_definition(
        existing.id.clone(),
        input.name,
        input.endpoint_url,
        input.upstream_protocol,
        auth_kind,
        input.models,
    )
    .map_err(|error| V3ApiError::invalid_request_at(state, error.to_string()))?;
    definition.preset_id =
        crate::dynamic::normalize_preset_id(input.preset_id.or(existing.preset_id.clone()))
            .map_err(|error| V3ApiError::invalid_request_at(state, error.to_string()))?;
    let account_count = state
        .db
        .lock()
        .count_accounts_for_provider(&existing.id)
        .map_err(V3ApiError::internal)?;
    if auth_kind.is_singleton() && account_count > 1 {
        return Err(V3ApiError::invalid_request_at(
            state,
            "no-auth provider requires a singleton account",
        ));
    }
    let changing_to_none = !existing.auth_kind.is_singleton() && auth_kind.is_singleton();
    let changing_from_none = existing.auth_kind.is_singleton() && !auth_kind.is_singleton();
    let supplied_key = input
        .key
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if !changing_from_none && supplied_key.is_some() {
        return Err(V3ApiError::invalid_request_at(
            state,
            "account Keys are owned by Accounts; rotate them there",
        ));
    }
    let replacement_key = if changing_from_none {
        Some(first_account_key(state, auth_kind, input.key.as_deref())?)
    } else {
        None
    };
    let now = Utc::now();
    let runtime = runtime_from_definition(definition, existing.created_at, now);
    let substantive = existing.endpoint_url != runtime.endpoint_url
        || existing.upstream_protocol != runtime.upstream_protocol
        || existing.auth_kind != runtime.auth_kind
        || existing.mappings != runtime.mappings;
    let snapshot = {
        let db = state.db.lock();
        db.replace_dynamic_provider(
            &runtime,
            substantive,
            changing_to_none,
            replacement_key.as_deref(),
        )
        .map_err(|error| V3ApiError::invalid_request_at(state, error.to_string()))?
    };
    state.install_dynamic_providers_snapshot(snapshot);
    Ok(provider_mutation(state, runtime, state.settings_revision()))
}

fn delete_locked(
    state: &CoreState,
    provider_id: &str,
    expectation: &MutationExpectation,
) -> Result<MutationAck, V3ApiError> {
    let _settings_update = state.settings_update.lock();
    check_expectation(state, expectation)?;
    reject_builtin_id(state, provider_id)?;
    let snapshot = {
        let db = state.db.lock();
        db.delete_dynamic_provider(provider_id)
            .map_err(|error| map_delete_error(state, error))?
    };
    state.install_dynamic_providers_snapshot(snapshot);
    Ok(MutationAck {
        revision: state.settings_revision(),
        process_generation: state.process_generation(),
    })
}

fn reject_builtin_id(state: &CoreState, provider_id: &str) -> Result<(), V3ApiError> {
    if ProviderRegistry::get(provider_id).is_some() || builtin_provider(provider_id).is_some() {
        return Err(V3ApiError::builtin_provider_immutable(
            state,
            "built-in providers cannot be deleted or replaced through this route",
        ));
    }
    Ok(())
}

pub(crate) fn validate_wire_definition(
    id: String,
    name: String,
    endpoint_url: String,
    protocol: super::types::AccountUpstreamProtocol,
    auth_kind: DynamicAuthKind,
    models: Vec<ProviderDefinitionModel>,
) -> Result<DynamicProviderDefinition, ocg_domain::provider::ProviderBindingError> {
    let endpoint_url = validate_custom_endpoint_url(&endpoint_url)?;
    let mappings = models
        .into_iter()
        .map(|model| DynamicModelMapping {
            public_model: model.public_model,
            upstream_model: model.upstream_model,
            upstream_override: model.upstream_override.map(|value| {
                ocg_domain::dynamic::DynamicModelUpstreamOverride {
                    protocol: value.protocol.into(),
                    endpoint_url: value.endpoint_url,
                }
            }),
        })
        .collect::<Vec<_>>();
    validate_definition(DynamicProviderDefinition {
        preset_id: None,
        id,
        name: normalize_dynamic_provider_name(&name)?,
        endpoint_url,
        upstream_protocol: protocol.into(),
        auth_kind,
        mappings: normalize_dynamic_mappings(&mappings)?,
    })
}

pub(crate) fn runtime_from_definition(
    definition: DynamicProviderDefinition,
    created_at: chrono::DateTime<Utc>,
    updated_at: chrono::DateTime<Utc>,
) -> DynamicProviderRuntime {
    let preset_for_origin = definition.preset_id.as_deref();
    let origin = ocg_domain::provider::provider_origin_from_preset(preset_for_origin);
    let offering =
        ocg_domain::provider::preset_offering(preset_for_origin.unwrap_or("")).to_string();
    DynamicProviderRuntime {
        preset_id: definition.preset_id,
        id: definition.id,
        name: definition.name,
        endpoint_url: definition.endpoint_url,
        upstream_protocol: definition.upstream_protocol,
        auth_kind: definition.auth_kind,
        mappings: definition.mappings,
        created_at,
        updated_at,
        origin,
        offering,
    }
}

pub(crate) fn first_account_key(
    state: &CoreState,
    auth_kind: DynamicAuthKind,
    key: Option<&str>,
) -> Result<String, V3ApiError> {
    if !auth_kind.requires_key() {
        return Ok(String::new());
    }
    let trimmed = key.map(str::trim).unwrap_or("");
    if trimmed.is_empty() {
        return Err(V3ApiError::invalid_request_at(state, "key is required"));
    }
    state.encrypt_key(trimmed).map_err(V3ApiError::internal)
}

fn required_probe_key(
    state: &CoreState,
    auth_kind: DynamicAuthKind,
    key: Option<&str>,
) -> Result<String, V3ApiError> {
    if !auth_kind.requires_key() {
        return Ok(String::new());
    }
    let trimmed = key.map(str::trim).unwrap_or("");
    if trimmed.is_empty() {
        return Err(V3ApiError::invalid_request_at(state, "key is required"));
    }
    Ok(trimmed.to_string())
}

fn models_without_key(models: Vec<String>, key: &str) -> Vec<String> {
    if key.is_empty() {
        return models;
    }
    models
        .into_iter()
        .map(|model| redact_known_secret(&model, key))
        .collect()
}

fn map_probe_failure(state: &CoreState, key: &str, message: String) -> V3ApiError {
    V3ApiError::outbound_failed(state, redact_known_secret(&message, key))
}

fn map_delete_error(state: &CoreState, error: anyhow::Error) -> V3ApiError {
    let message = error.to_string();
    if message.contains("still has") {
        V3ApiError::conflict_at(state, message)
    } else if message.contains("unknown provider") {
        V3ApiError::not_found_at(state, message)
    } else {
        V3ApiError::invalid_request_at(state, message)
    }
}

fn provider_mutation(
    state: &CoreState,
    runtime: DynamicProviderRuntime,
    revision: u64,
) -> ProviderDefinitionMutation {
    ProviderDefinitionMutation {
        provider: to_wire(runtime, revision, state.process_generation()),
        revision,
        process_generation: state.process_generation(),
    }
}

fn to_wire(
    runtime: DynamicProviderRuntime,
    revision: u64,
    process_generation: u64,
) -> ProviderDefinition {
    let is_builtin = matches!(runtime.origin, ProviderOrigin::Builtin);
    ProviderDefinition {
        origin: runtime.origin,
        editable: !is_builtin,
        deletable: !is_builtin,
        offering: runtime.offering,
        preset_id: runtime.preset_id,
        id: runtime.id,
        name: runtime.name,
        endpoint_url: if is_builtin {
            None
        } else {
            Some(runtime.endpoint_url)
        },
        upstream_protocol: if is_builtin {
            None
        } else {
            Some(runtime.upstream_protocol.into())
        },
        auth_kind: if is_builtin {
            None
        } else {
            Some(runtime.auth_kind.into())
        },
        models: runtime
            .mappings
            .into_iter()
            .map(|mapping| ProviderDefinitionModel {
                public_model: mapping.public_model,
                upstream_model: mapping.upstream_model,
                upstream_override: mapping.upstream_override.map(|value| {
                    super::types::ProviderModelUpstreamOverride {
                        protocol: value.protocol.into(),
                        endpoint_url: value.endpoint_url,
                    }
                }),
            })
            .collect(),
        created_at: runtime.created_at.to_rfc3339(),
        updated_at: runtime.updated_at.to_rfc3339(),
        revision,
        process_generation,
    }
}

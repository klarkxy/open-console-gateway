//! Local account control plane: secret-free reads and lifecycle mutations.
//!
//! Connection verify, account model-tests, usage, and Custom model discovery
//! live on V3 account routes. Browser runtime lives on the V3 browser routes
//! and reuses this module's secret-free account DTO mapper. Go/Zen protocol
//! probes live on the Providers V3 route. This slice preserves local
//! semantics (enablement, Custom invalidation, managed setup, delete profile
//! staging) behind the V3 CAS envelope.

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use chrono::Utc;

use crate::account_control::{self, AccountControlError};
use crate::db::ReorderAccountsError;
use crate::models::{
    Account as ModelAccount, AccountCustomConfigInput, AccountModelCapabilityInput,
    AccountSetupStep as ModelSetupStep, AccountType as ModelAccountType,
    AccountUpdate as ModelAccountUpdate, NEW_READY_KEY_ACCOUNT_ENABLED, normalize_account_notes,
    normalize_purchase_date,
};
use crate::provider::{CreationAvailability, default_provider_id};
use crate::redaction::redact_known_secret;
use crate::state::CoreState;

use super::types::{
    Account, AccountCreate, AccountCustomConfig, AccountCustomConfigUpdate,
    AccountCustomConfigWrite, AccountList, AccountManagedCreate, AccountModelCapabilitiesUpdate,
    AccountModelCapability, AccountModelCapabilityWrite, AccountMutation, AccountOrder,
    AccountSetupUpdate, AccountUpdate, MutationExpectation, OllamaBillingTier,
};
use super::{V3ApiError, check_expectation, parse_mutation_json};

pub(super) async fn list_accounts(
    State(state): State<CoreState>,
) -> Result<Json<AccountList>, V3ApiError> {
    let _settings_update = state.settings_update.lock();
    let accounts = crate::destination_projection::list_accounts_for_v3(&state.db.lock())
        .map_err(V3ApiError::internal)?;
    Ok(Json(account_list_from_state(&state, accounts)?))
}

pub(super) async fn get_account(
    State(state): State<CoreState>,
    Path(id): Path<String>,
) -> Result<Json<Account>, V3ApiError> {
    let _settings_update = state.settings_update.lock();
    let account = load_model_account(&state, &id)?;
    Ok(Json(account_from_state(&state, account)?))
}

pub(super) async fn create_account(
    State(state): State<CoreState>,
    body: Bytes,
) -> Result<Json<AccountMutation>, V3ApiError> {
    let input = parse_mutation_json::<AccountCreate>(&body)?;
    create_account_locked(&state, input).map(Json)
}

pub(super) async fn create_managed_account(
    State(state): State<CoreState>,
    body: Bytes,
) -> Result<(StatusCode, Json<AccountMutation>), V3ApiError> {
    let input = parse_mutation_json::<AccountManagedCreate>(&body)?;
    create_managed_locked(&state, input).map(|mutation| (StatusCode::CREATED, Json(mutation)))
}

pub(super) async fn update_account(
    State(state): State<CoreState>,
    Path(id): Path<String>,
    body: Bytes,
) -> Result<Json<AccountMutation>, V3ApiError> {
    let input = parse_mutation_json::<AccountUpdate>(&body)?;
    update_account_locked(&state, &id, input).map(Json)
}

pub(super) async fn delete_account(
    State(state): State<CoreState>,
    Path(id): Path<String>,
    body: Bytes,
) -> Result<Json<AccountMutation>, V3ApiError> {
    let expectation = parse_mutation_json::<MutationExpectation>(&body)?;
    delete_account_locked(&state, &id, &expectation)
        .await
        .map(Json)
}

pub(super) async fn reorder_accounts(
    State(state): State<CoreState>,
    body: Bytes,
) -> Result<Json<AccountList>, V3ApiError> {
    let input = parse_mutation_json::<AccountOrder>(&body)?;
    reorder_accounts_locked(&state, input).map(Json)
}

pub(super) async fn toggle_account(
    State(state): State<CoreState>,
    Path(id): Path<String>,
    body: Bytes,
) -> Result<Json<AccountMutation>, V3ApiError> {
    let expectation = parse_mutation_json::<MutationExpectation>(&body)?;
    toggle_account_locked(&state, &id, &expectation).map(Json)
}

pub(super) async fn advance_account_setup(
    State(state): State<CoreState>,
    Path(id): Path<String>,
    body: Bytes,
) -> Result<Json<AccountMutation>, V3ApiError> {
    let input = parse_mutation_json::<AccountSetupUpdate>(&body)?;
    advance_setup_locked(&state, &id, input).map(Json)
}

pub(super) async fn reset_account_cooldown(
    State(state): State<CoreState>,
    Path(id): Path<String>,
    body: Bytes,
) -> Result<Json<AccountMutation>, V3ApiError> {
    let expectation = parse_mutation_json::<MutationExpectation>(&body)?;
    reset_cooldown_locked(&state, &id, &expectation).map(Json)
}

pub(super) async fn put_account_custom_config(
    State(state): State<CoreState>,
    Path(id): Path<String>,
    body: Bytes,
) -> Result<Json<AccountMutation>, V3ApiError> {
    let input = parse_mutation_json::<AccountCustomConfigUpdate>(&body)?;
    put_custom_config_locked(&state, &id, input).map(Json)
}

pub(super) async fn put_account_model_capabilities(
    State(state): State<CoreState>,
    Path(id): Path<String>,
    body: Bytes,
) -> Result<Json<AccountMutation>, V3ApiError> {
    let input = parse_mutation_json::<AccountModelCapabilitiesUpdate>(&body)?;
    put_capabilities_locked(&state, &id, input).map(Json)
}

fn create_dynamic_account_locked(
    state: &CoreState,
    input: AccountCreate,
    runtime: crate::dynamic::DynamicProviderRuntime,
) -> Result<AccountMutation, V3ApiError> {
    if input.custom_config.is_some() || !input.model_capabilities.is_empty() {
        return Err(V3ApiError::invalid_request_at(
            state,
            "dynamic Provider accounts do not own Endpoint, protocol, or model mappings",
        ));
    }
    if runtime.auth_kind.is_singleton() {
        return Err(V3ApiError::invalid_request_at(
            state,
            "no-auth provider already has a singleton account",
        ));
    }
    if input.key.trim().is_empty() {
        return Err(V3ApiError::invalid_request_at(state, "key is required"));
    }
    let notes = match input.notes.as_deref() {
        Some(value) => normalize_account_notes(value)
            .map_err(|error| V3ApiError::invalid_request_at(state, error.to_string()))?,
        None => None,
    };
    let now = Utc::now();
    let id = uuid::Uuid::new_v4().to_string();
    let account = ModelAccount {
        id: id.clone(),
        provider_id: runtime.id.clone(),
        credential_kind: runtime.auth_kind.credential_kind(),
        quota_scope: runtime.auth_kind.quota_scope(),
        name: input.name.trim().to_string(),
        username: clean_optional(input.username),
        password_cipher: None,
        key_cipher: state
            .encrypt_key(input.key.trim())
            .map_err(V3ApiError::internal)?,
        enabled: NEW_READY_KEY_ACCOUNT_ENABLED,
        account_type: ModelAccountType::Key,
        setup_step: ModelSetupStep::Ready,
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
    };
    {
        let db = state.db.lock();
        db.create_account(&account)
            .map_err(|error| map_account_write_error(state, error))?;
    }
    mutation_after_commit(state, &id, false)
}

fn create_account_locked(
    state: &CoreState,
    input: AccountCreate,
) -> Result<AccountMutation, V3ApiError> {
    let _settings_update = state.settings_update.lock();
    check_expectation(state, &input.expectation)?;
    let name = input.name.trim().to_string();
    if name.is_empty() {
        return Err(V3ApiError::invalid_request_at(state, "name is required"));
    }
    let default_provider = default_provider_id();
    let provider_id = input
        .provider_id
        .as_deref()
        .unwrap_or(&default_provider)
        .trim();
    if let Some(runtime) = state
        .dynamic_providers()
        .iter()
        .find(|runtime| crate::dynamic::provider_ids_equal(&runtime.id, provider_id))
        .cloned()
    {
        return create_dynamic_account_locked(state, input, runtime);
    }
    let plan = crate::provider::builtin_provider(provider_id).ok_or_else(|| {
        V3ApiError::invalid_request_at(state, format!("unknown provider `{provider_id}`"))
    })?;
    if plan.creation_availability == CreationAvailability::Unavailable {
        return Err(V3ApiError::invalid_request_at(
            state,
            plan.creation_unavailable_reason
                .unwrap_or("this Plan cannot be created through the generic account API")
                .to_string(),
        ));
    }
    if plan.singleton_account_id.is_some() {
        return Err(V3ApiError::invalid_request_at(
            state,
            "Zen Free is a built-in singleton and cannot be created through the generic account API",
        ));
    }
    crate::provider::validate_plan_key(plan, &input.key)
        .map_err(|error| V3ApiError::invalid_request_at(state, error.to_string()))?;
    let custom_config = input
        .custom_config
        .as_ref()
        .map(custom_config_write_to_input);
    let model_capabilities = input
        .model_capabilities
        .iter()
        .map(capability_write_to_input)
        .collect::<Vec<_>>();
    let requires_custom = crate::provider::plan_requires_custom_config(plan);
    if requires_custom {
        match custom_config.as_ref() {
            Some(config) => {
                crate::custom::validate_custom_endpoint_url(&config.endpoint_url)
                    .map_err(|error| V3ApiError::invalid_request_at(state, error.to_string()))?;
            }
            None => {
                return Err(V3ApiError::invalid_request_at(
                    state,
                    "Custom API accounts require a complete endpoint URL and one upstream protocol",
                ));
            }
        }
        if model_capabilities.is_empty() {
            return Err(V3ApiError::invalid_request_at(
                state,
                "Custom API accounts require at least one model capability",
            ));
        }
        for capability in &model_capabilities {
            crate::provider::validate_custom_model_id(&capability.public_model)
                .map_err(|error| V3ApiError::invalid_request_at(state, error.to_string()))?;
            crate::provider::validate_custom_model_id(&capability.upstream_model)
                .map_err(|error| V3ApiError::invalid_request_at(state, error.to_string()))?;
        }
        if let Some(config) = custom_config.as_ref() {
            crate::custom::validate_custom_capability_expansion(
                config.upstream_protocol,
                &model_capabilities,
            )
            .map_err(|message| V3ApiError::invalid_request_at(state, message))?;
        }
    } else {
        if custom_config.is_some() {
            return Err(V3ApiError::invalid_request_at(
                state,
                "custom config is only available for Custom API accounts",
            ));
        }
        if !model_capabilities.is_empty() {
            return Err(V3ApiError::invalid_request_at(
                state,
                "model capabilities are only available for Custom API accounts",
            ));
        }
    }
    let enabled = NEW_READY_KEY_ACCOUNT_ENABLED;
    let purchase_date = match input.purchase_date {
        Some(value) if !value.trim().is_empty() => normalize_purchase_date(&value)
            .map_err(|error| V3ApiError::invalid_request_at(state, error.to_string()))?,
        _ => String::new(),
    };
    let notes = match input.notes.as_deref() {
        Some(value) => normalize_account_notes(value)
            .map_err(|error| V3ApiError::invalid_request_at(state, error.to_string()))?,
        None => None,
    };
    let now = Utc::now();
    let id = uuid::Uuid::new_v4().to_string();
    let account = ModelAccount {
        id: id.clone(),
        provider_id: plan.provider_id.to_string(),
        credential_kind: plan.credential_kind,
        quota_scope: plan.quota_scope,
        name,
        username: clean_optional(input.username),
        password_cipher: encrypted_optional(state, &input.password)?,
        key_cipher: state
            .encrypt_key(input.key.trim())
            .map_err(V3ApiError::internal)?,
        enabled,
        account_type: ModelAccountType::Key,
        setup_step: ModelSetupStep::Ready,
        referral_code: clean_optional(input.referral_code),
        purchase_date: purchase_date.clone(),
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
    };
    let ollama_billing = parse_ollama_billing_write(
        state,
        plan.provider_id,
        input.ollama_billing_tier,
        &purchase_date,
        true,
    )?;
    {
        let db = state.db.lock();
        db.create_account_with_contract_and_billing(
            &account,
            custom_config.as_ref(),
            &model_capabilities,
            ollama_billing,
        )
        .map_err(|error| map_account_write_error(state, error))?;
        let _ = db.log_gateway(
            "info",
            "account",
            &format!("created account {}", account.name),
        );
    }
    mutation_after_commit(state, &id, true)
}

fn create_managed_locked(
    state: &CoreState,
    input: AccountManagedCreate,
) -> Result<AccountMutation, V3ApiError> {
    let _settings_update = state.settings_update.lock();
    check_expectation(state, &input.expectation)?;
    if state.config().opencode_invite_url.is_empty() {
        return Err(V3ApiError::precondition_failed_at(
            state,
            "configure an OpenCode invite URL before registering a managed account",
        ));
    }
    let name = input.name.trim().to_string();
    if name.is_empty() {
        return Err(V3ApiError::invalid_request_at(state, "name is required"));
    }
    if name.chars().count() > 200 {
        return Err(V3ApiError::invalid_request_at(
            state,
            "name must be at most 200 characters",
        ));
    }
    let notes = match input.notes.as_deref() {
        Some(value) => normalize_account_notes(value)
            .map_err(|error| V3ApiError::invalid_request_at(state, error.to_string()))?,
        None => None,
    };
    let now = Utc::now();
    let id = uuid::Uuid::new_v4().to_string();
    let account = ModelAccount {
        id: id.clone(),
        provider_id: crate::provider::default_provider_id(),

        credential_kind: crate::provider::default_credential_kind(),
        quota_scope: crate::provider::default_quota_scope(),
        name,
        username: clean_optional(input.username),
        password_cipher: None,
        key_cipher: String::new(),
        enabled: false,
        account_type: ModelAccountType::Managed,
        setup_step: ModelSetupStep::GoogleAccount,
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
    };
    {
        let db = state.db.lock();
        db.create_account(&account).map_err(V3ApiError::internal)?;
        let _ = db.log_gateway(
            "info",
            "account",
            &format!("created managed account draft {}", account.name),
        );
    }
    mutation_after_commit(state, &id, false)
}

fn update_account_locked(
    state: &CoreState,
    id: &str,
    input: AccountUpdate,
) -> Result<AccountMutation, V3ApiError> {
    let _settings_update = state.settings_update.lock();
    check_expectation(state, &input.expectation)?;
    let existing = load_model_account(state, id)?;
    if existing.id == crate::provider::CPA_ACCOUNT_ID {
        return Err(V3ApiError::invalid_request_at(
            state,
            "CPA Subscription Pool settings must use the external-integration endpoint",
        ));
    }
    if existing.is_zen_free() {
        return Err(V3ApiError::invalid_request_at(
            state,
            "Zen Free settings must use the dedicated provider-settings endpoint",
        ));
    }
    if !existing.setup_step.is_ready()
        && (input.enabled == Some(true)
            || input
                .key
                .as_deref()
                .is_some_and(|key| !key.trim().is_empty()))
    {
        return Err(V3ApiError::conflict_at(
            state,
            "finish managed-account key verification before enabling or replacing its key",
        ));
    }
    if input.enabled == Some(true) {
        ensure_account_can_enable(state, &existing)?;
    }
    let mut update = ModelAccountUpdate {
        name: input.name,
        username: input.username,
        password: input.password,
        key: input.key,
        enabled: input.enabled,
        referral_code: input.referral_code,
        purchase_date: input.purchase_date,
        notes: input.notes,
    };
    if let Some(value) = update.purchase_date.take() {
        update.purchase_date = Some(
            normalize_purchase_date(&value)
                .map_err(|error| V3ApiError::invalid_request_at(state, error.to_string()))?,
        );
    }
    if let Some(value) = update.notes.take() {
        update.notes = Some(
            normalize_account_notes(&value)
                .map_err(|error| V3ApiError::invalid_request_at(state, error.to_string()))?
                .unwrap_or_default(),
        );
    }
    if let Some(plan) = crate::provider::builtin_provider(&existing.provider_id)
        && let Some(key) = update.key.as_deref()
    {
        crate::provider::validate_plan_key(plan, key)
            .map_err(|error| V3ApiError::invalid_request_at(state, error.to_string()))?;
    }
    let key_cipher = match update.key.as_deref().map(str::trim) {
        Some("") | None => None,
        Some(key) => Some(state.encrypt_key(key).map_err(V3ApiError::internal)?),
    };
    let password_cipher = match update.password.as_deref().map(str::trim) {
        Some("") => Some(String::new()),
        None => None,
        Some(password) => Some(state.encrypt_key(password).map_err(V3ApiError::internal)?),
    };
    let effective_purchase_date = update
        .purchase_date
        .clone()
        .unwrap_or_else(|| existing.purchase_date.clone());
    let ollama_billing = match input.ollama_billing_tier {
        Some(tier) => Some(parse_ollama_billing_write(
            state,
            &existing.provider_id,
            Some(tier),
            &effective_purchase_date,
            false,
        )?),
        None => None,
    };
    {
        let db = state.db.lock();
        db.update_account_with_billing(
            id,
            &update,
            key_cipher.as_deref(),
            password_cipher.as_deref(),
            ollama_billing,
        )
        .map_err(|error| map_account_write_error(state, error))?;
        let _ = db.log_gateway("info", "account", &format!("updated account {id}"));
    }
    mutation_after_commit(state, id, false)
}

async fn delete_account_locked(
    state: &CoreState,
    id: &str,
    expectation: &MutationExpectation,
) -> Result<AccountMutation, V3ApiError> {
    let revision = account_control::delete_account(
        state,
        id,
        Some((
            expectation.expected_revision,
            expectation.process_generation,
        )),
    )
    .await
    .map_err(|error| map_account_control_error(state, error))?;
    Ok(AccountMutation {
        account: None,
        revision,
        process_generation: state.process_generation(),
    })
}

fn reorder_accounts_locked(
    state: &CoreState,
    input: AccountOrder,
) -> Result<AccountList, V3ApiError> {
    let _settings_update = state.settings_update.lock();
    check_expectation(state, &input.expectation)?;
    {
        let db = state.db.lock();
        db.reorder_accounts(&input.account_ids)
            .map_err(|error| match error {
                ReorderAccountsError::DuplicateAccountId => {
                    V3ApiError::invalid_request_at(state, "account_ids contains duplicates")
                }
                ReorderAccountsError::AccountSetMismatch => V3ApiError::conflict_at(
                    state,
                    "account list changed; reload accounts and try again",
                ),
                ReorderAccountsError::Database(error) => V3ApiError::internal(error),
                ReorderAccountsError::Layout(error) => V3ApiError::internal(error),
            })?;
    }
    let (revision, accounts) = with_committed_revision(state, || {
        crate::destination_projection::list_accounts_for_v3(&state.db.lock())
            .map_err(V3ApiError::internal)
    })?;
    account_list_at(state, accounts, revision)
}

fn toggle_account_locked(
    state: &CoreState,
    id: &str,
    expectation: &MutationExpectation,
) -> Result<AccountMutation, V3ApiError> {
    let _settings_update = state.settings_update.lock();
    check_expectation(state, expectation)?;
    let account = load_model_account(state, id)?;
    let next_enabled = !account.enabled;
    let account = account_control::set_account_enabled_locked(state, id, next_enabled)
        .map_err(|error| map_account_control_error(state, error))?;
    mutation_at(state, account, state.settings_revision())
}

fn advance_setup_locked(
    state: &CoreState,
    id: &str,
    input: AccountSetupUpdate,
) -> Result<AccountMutation, V3ApiError> {
    let _settings_update = state.settings_update.lock();
    check_expectation(state, &input.expectation)?;
    let current = load_model_account(state, id)?;
    if current.account_type != ModelAccountType::Managed {
        return Err(V3ApiError::invalid_request_at(
            state,
            "setup steps are only available for managed accounts",
        ));
    }
    let requested: ModelSetupStep = input.setup_step.into();
    if current.setup_step == requested {
        return mutation_from_state(state, current);
    }
    if !current.setup_step.can_transition_to(requested) {
        return Err(V3ApiError::conflict_at(
            state,
            format!(
                "setup cannot move from {} to {}",
                current.setup_step.as_str(),
                requested.as_str()
            ),
        ));
    }
    if !state
        .db
        .lock()
        .advance_managed_setup(id, current.setup_step, requested)
        .map_err(V3ApiError::internal)?
    {
        return Err(V3ApiError::conflict_at(
            state,
            "setup changed; reload the account and try again",
        ));
    }
    mutation_after_commit(state, id, false)
}

fn reset_cooldown_locked(
    state: &CoreState,
    id: &str,
    expectation: &MutationExpectation,
) -> Result<AccountMutation, V3ApiError> {
    let _settings_update = state.settings_update.lock();
    check_expectation(state, expectation)?;
    let account = load_model_account(state, id)?;
    if account.is_zen_free() {
        return Err(V3ApiError::invalid_request_at(
            state,
            "Zen Free uses an egress-wide cooldown that cannot be cleared from an account",
        ));
    }
    {
        let db = state.db.lock();
        db.clear_account_cooldown(id)
            .map_err(V3ApiError::internal)?;
        state.recovery.reset_account(id);
    }
    mutation_after_commit(state, id, false)
}

fn put_custom_config_locked(
    state: &CoreState,
    id: &str,
    input: AccountCustomConfigUpdate,
) -> Result<AccountMutation, V3ApiError> {
    let _settings_update = state.settings_update.lock();
    check_expectation(state, &input.expectation)?;
    let account = load_model_account(state, id)?;
    require_custom_plan(
        state,
        &account,
        "custom config is only available for Custom API accounts",
    )?;
    reject_shared_custom_account_edit(state, id)?;
    let mut config = AccountCustomConfigInput {
        endpoint_url: input.endpoint_url,
        upstream_protocol: input.upstream_protocol.into(),
    };
    {
        let db = state.db.lock();
        if let Some(endpoint) = db
            .platform_hosted_endpoint(id)
            .map_err(V3ApiError::internal)?
        {
            let old = db.account_custom_config(id).map_err(V3ApiError::internal)?;
            if config.endpoint_url != endpoint
                && old.is_none_or(|c| c.endpoint_url != config.endpoint_url)
            {
                return Err(V3ApiError::invalid_request_at(
                    state,
                    "endpoint belongs to the platform account; unlink the Key to change it",
                ));
            }
            config.endpoint_url = endpoint;
        }
    }
    let capabilities = input
        .model_capabilities
        .iter()
        .map(capability_write_to_input)
        .collect::<Vec<_>>();
    state
        .db
        .lock()
        .commit_account_custom_config_and_capabilities(id, &config, &capabilities)
        .map_err(|error| V3ApiError::invalid_request_at(state, error.to_string()))?;
    mutation_after_commit(state, id, true)
}

fn put_capabilities_locked(
    state: &CoreState,
    id: &str,
    input: AccountModelCapabilitiesUpdate,
) -> Result<AccountMutation, V3ApiError> {
    let _settings_update = state.settings_update.lock();
    check_expectation(state, &input.expectation)?;
    let account = load_model_account(state, id)?;
    require_custom_plan(
        state,
        &account,
        "model capabilities are only available for Custom API accounts",
    )?;
    reject_shared_custom_account_edit(state, id)?;
    let capabilities = input
        .capabilities
        .iter()
        .map(capability_write_to_input)
        .collect::<Vec<_>>();
    state
        .db
        .lock()
        .commit_account_model_capabilities(id, &capabilities)
        .map_err(|error| V3ApiError::invalid_request_at(state, error.to_string()))?;
    mutation_after_commit(state, id, true)
}

fn reject_shared_custom_account_edit(
    state: &CoreState,
    account_id: &str,
) -> Result<(), V3ApiError> {
    let count = state
        .db
        .lock()
        .custom_connection_credential_count(account_id)
        .map_err(V3ApiError::internal)?;
    if count > 1 {
        return Err(V3ApiError::invalid_request_at(
            state,
            "this Custom HTTP connection has multiple Keys; edit the service connection on Providers",
        ));
    }
    Ok(())
}

fn ensure_account_can_enable(state: &CoreState, account: &ModelAccount) -> Result<(), V3ApiError> {
    account_control::ensure_account_can_enable(state, account)
        .map_err(|error| map_account_control_error(state, error))
}

fn map_account_control_error(state: &CoreState, error: AccountControlError) -> V3ApiError {
    match error {
        AccountControlError::NotFound => V3ApiError::not_found(state),
        AccountControlError::RevisionConflict => V3ApiError::revision_conflict(state),
        AccountControlError::Invalid(message) => V3ApiError::invalid_request_at(state, message),
        AccountControlError::Conflict(message) => V3ApiError::conflict_at(state, message),
        AccountControlError::Unavailable(message) => {
            V3ApiError::service_unavailable(state, message)
        }
        AccountControlError::Internal(error) => V3ApiError::internal(error),
    }
}

fn require_custom_plan(
    state: &CoreState,
    account: &ModelAccount,
    message: &'static str,
) -> Result<(), V3ApiError> {
    let plan = crate::provider::builtin_provider(&account.provider_id)
        .ok_or_else(|| V3ApiError::invalid_request_at(state, "unknown provider offering"))?;
    if crate::provider::plan_requires_custom_config(plan) {
        Ok(())
    } else {
        Err(V3ApiError::invalid_request_at(state, message))
    }
}

pub(super) fn load_model_account(state: &CoreState, id: &str) -> Result<ModelAccount, V3ApiError> {
    state
        .db
        .lock()
        .get_account(id)
        .map_err(V3ApiError::internal)?
        .ok_or_else(|| V3ApiError::not_found(state))
}

/// Advance the shared CAS token immediately after a persistence commit.
fn committed_revision(state: &CoreState) -> u64 {
    state.bump_settings_revision()
}

/// Run fallible post-commit reload/read/purge after the CAS token has already
/// advanced, so a later error cannot hide the committed change.
fn with_committed_revision<T>(
    state: &CoreState,
    then: impl FnOnce() -> Result<T, V3ApiError>,
) -> Result<(u64, T), V3ApiError> {
    let revision = committed_revision(state);
    let value = then()?;
    Ok((revision, value))
}

fn mutation_after_commit(
    state: &CoreState,
    id: &str,
    reload_contracts: bool,
) -> Result<AccountMutation, V3ApiError> {
    let (revision, account) = with_committed_revision(state, || {
        if reload_contracts {
            state
                .reload_provider_contracts()
                .map_err(V3ApiError::internal)?;
        }
        load_model_account(state, id)
    })?;
    mutation_at(state, account, revision)
}

fn account_list_from_state(
    state: &CoreState,
    accounts: Vec<ModelAccount>,
) -> Result<AccountList, V3ApiError> {
    account_list_at(state, accounts, state.settings_revision())
}

fn account_list_at(
    state: &CoreState,
    accounts: Vec<ModelAccount>,
    revision: u64,
) -> Result<AccountList, V3ApiError> {
    Ok(AccountList {
        accounts: accounts
            .into_iter()
            .map(|account| {
                let mut dto = account_from_state(state, account)?;
                dto.revision = revision;
                Ok(dto)
            })
            .collect::<Result<Vec<_>, _>>()?,
        revision,
        process_generation: state.process_generation(),
    })
}

pub(super) fn mutation_from_state(
    state: &CoreState,
    account: ModelAccount,
) -> Result<AccountMutation, V3ApiError> {
    mutation_at(state, account, state.settings_revision())
}

pub(super) fn mutation_at(
    state: &CoreState,
    account: ModelAccount,
    revision: u64,
) -> Result<AccountMutation, V3ApiError> {
    let mut account = account_from_state(state, account)?;
    account.revision = revision;
    Ok(AccountMutation {
        account: Some(account),
        revision,
        process_generation: state.process_generation(),
    })
}

fn account_from_state(state: &CoreState, account: ModelAccount) -> Result<Account, V3ApiError> {
    let ((usage_sync_last_success_at, usage_sync_next_allowed_at), contract, ollama_billing) = {
        let db = state.db.lock();
        let sync = db
            .account_usage_sync_state(&account.id)
            .map_err(V3ApiError::internal)?;
        let contract = db
            .load_account_contract(&account.id)
            .map_err(V3ApiError::internal)?;
        let ollama_billing = if account.provider_id == crate::provider::OLLAMA_PROVIDER_ID {
            db.ollama_cloud_billing_tier(&account.id)
                .map_err(V3ApiError::internal)?
        } else {
            None
        };
        (
            crate::usage_sync::dashboard_sync_fields(sync.as_ref(), state.usage_sync.now()),
            contract,
            ollama_billing,
        )
    };
    let known_secret = if account.last_error.is_some()
        || account.auth_error.is_some()
        || contract.verification.verification_error.is_some()
    {
        if account.key_cipher.is_empty() {
            Some(String::new())
        } else {
            state.decrypt_key(&account.key_cipher).ok()
        }
    } else {
        None
    };
    let sanitize_persisted_error = |error: Option<String>| {
        error.and_then(|error| {
            known_secret
                .as_deref()
                .map(|secret| redact_known_secret(&error, secret))
        })
    };
    let plan = crate::provider::builtin_provider(&account.provider_id);
    // A stored legacy purchase anchor is not evidence of a dynamic Provider's
    // billing cadence or credential expiry. Keep storage intact, but do not
    // publish invented subscription dates for these account-owned Keys.
    let has_builtin_lifecycle = plan.is_some();
    Ok(Account {
        id: account.id.clone(),
        provider_id: account.provider_id.clone(),

        credential_kind: account.credential_kind.into(),
        quota_scope: account.quota_scope.into(),
        name: account.name,
        username: account.username,
        enabled: account.enabled,
        account_type: account.account_type.into(),
        setup_step: account.setup_step.into(),
        purchase_date: if has_builtin_lifecycle {
            account.purchase_date
        } else {
            String::new()
        },
        expires_on: if has_builtin_lifecycle {
            account.expires_on
        } else {
            String::new()
        },
        cooldown_until: account.cooldown_until.map(|t| t.to_rfc3339()),
        cooldown_generic_until: account.cooldown_generic_until.map(|t| t.to_rfc3339()),
        cooldown_5h_until: account.cooldown_5h_until.map(|t| t.to_rfc3339()),
        cooldown_week_until: account.cooldown_week_until.map(|t| t.to_rfc3339()),
        cooldown_month_until: account.cooldown_month_until.map(|t| t.to_rfc3339()),
        cooldown_free_until: account.cooldown_free_until.map(|t| t.to_rfc3339()),
        last_error: sanitize_persisted_error(account.last_error),
        auth_error: sanitize_persisted_error(account.auth_error),
        notes: account.notes,
        usage_sync_last_success_at,
        usage_sync_next_allowed_at,
        created_at: account.created_at.to_rfc3339(),
        updated_at: account.updated_at.to_rfc3339(),
        revision: state.settings_revision(),
        process_generation: state.process_generation(),
        verification_status: contract.verification.status.into(),
        connection_verified_at: contract
            .verification
            .connection_verified_at
            .map(|value| value.to_rfc3339()),
        verification_error: sanitize_persisted_error(contract.verification.verification_error),
        plan_routable: plan.is_some_and(|plan| plan.routable)
            || crate::dynamic::find_runtime(&state.dynamic_providers(), &account.provider_id)
                .is_some(),
        custom_config: contract.custom_config.map(custom_config_from_model),
        model_capabilities: contract
            .model_capabilities
            .into_iter()
            .map(capability_from_model)
            .collect(),
        ollama_billing_tier: present_ollama_billing(&account.provider_id, ollama_billing),
    })
}

fn present_ollama_billing(
    provider_id: &str,
    tier: Option<crate::provider::OllamaBillingTier>,
) -> Option<OllamaBillingTier> {
    if provider_id != crate::provider::OLLAMA_PROVIDER_ID {
        return None;
    }
    tier.map(OllamaBillingTier::from)
}

fn parse_ollama_billing_write(
    state: &CoreState,
    provider_id: &str,
    requested: Option<OllamaBillingTier>,
    purchase_date: &str,
    create: bool,
) -> Result<Option<crate::provider::OllamaBillingTier>, V3ApiError> {
    if provider_id != crate::provider::OLLAMA_PROVIDER_ID {
        if requested.is_some() {
            return Err(V3ApiError::invalid_request_at(
                state,
                "Ollama billing tier is only valid for Ollama Cloud accounts",
            ));
        }
        return Ok(None);
    }
    let Some(requested) = requested else {
        if create {
            return Err(V3ApiError::invalid_request_at(
                state,
                "Ollama Cloud accounts require a billing tier",
            ));
        }
        return Ok(None);
    };
    if purchase_date.trim().is_empty() {
        return Err(V3ApiError::invalid_request_at(
            state,
            "a configured Ollama paid tier requires purchase_date",
        ));
    }
    Ok(Some(requested.into()))
}

fn custom_config_from_model(config: crate::models::AccountCustomConfig) -> AccountCustomConfig {
    AccountCustomConfig {
        account_id: config.account_id,
        endpoint_url: config.endpoint_url,
        upstream_protocol: config.upstream_protocol.into(),
        created_at: config.created_at.to_rfc3339(),
        updated_at: config.updated_at.to_rfc3339(),
    }
}

fn capability_from_model(
    capability: crate::models::AccountModelCapability,
) -> AccountModelCapability {
    AccountModelCapability {
        public_model: capability.public_model,
        upstream_model: capability.upstream_model,
        protocol: capability.protocol.into(),
        verified_at: capability.verified_at.map(|value| value.to_rfc3339()),
        source: capability.source,
    }
}

fn custom_config_write_to_input(write: &AccountCustomConfigWrite) -> AccountCustomConfigInput {
    AccountCustomConfigInput {
        endpoint_url: write.endpoint_url.clone(),
        upstream_protocol: write.upstream_protocol.into(),
    }
}

fn capability_write_to_input(write: &AccountModelCapabilityWrite) -> AccountModelCapabilityInput {
    match write {
        AccountModelCapabilityWrite::Canonical(write) => AccountModelCapabilityInput {
            public_model: write.public_model.clone(),
            upstream_model: write.upstream_model.clone(),
            protocol: write.protocol.into(),
            source: write.source.clone(),
        },
        AccountModelCapabilityWrite::Legacy(write) => AccountModelCapabilityInput {
            public_model: write.model_id.clone(),
            upstream_model: write.model_id.clone(),
            protocol: write.protocol.into(),
            source: write.source.clone(),
        },
    }
}

fn clean_optional(value: Option<String>) -> Option<String> {
    value.and_then(|s| {
        let trimmed = s.trim().to_string();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed)
        }
    })
}

fn encrypted_optional(
    state: &CoreState,
    value: &Option<String>,
) -> Result<Option<String>, V3ApiError> {
    match value.as_deref().map(str::trim) {
        Some("") | None => Ok(None),
        Some(v) => state.encrypt_key(v).map(Some).map_err(V3ApiError::internal),
    }
}

fn map_account_write_error(state: &CoreState, error: anyhow::Error) -> V3ApiError {
    if let Some(binding) = error.downcast_ref::<crate::provider::ProviderBindingError>() {
        return map_provider_binding_error(state, binding.clone());
    }
    let message = error.to_string();
    if message.contains("not routable") {
        V3ApiError::conflict_at(state, message)
    } else if message.contains("Custom API accounts require")
        || message.contains("only available for Custom")
        || message.contains("risk acknowledgement")
        || message.contains("base URL")
        || message.contains("model id")
        || message.contains("model capability")
        || message.contains("protocol and auth")
        || message.contains("upstream protocol")
        || message.contains("duplicate model")
    {
        V3ApiError::invalid_request_at(state, message)
    } else {
        V3ApiError::internal(error)
    }
}

fn map_provider_binding_error(
    state: &CoreState,
    error: crate::provider::ProviderBindingError,
) -> V3ApiError {
    match error {
        crate::provider::ProviderBindingError::EnablementNotRoutable { .. } => {
            V3ApiError::conflict_at(state, error.to_string())
        }
        other => V3ApiError::invalid_request_at(state, other.to_string()),
    }
}

#[cfg(test)]
mod committed_revision_tests {
    use super::*;
    use crate::crypto::{KeyCipher, StaticKeyCipher};
    use crate::db::Database;
    use crate::state::CoreStateInner;
    use std::sync::Arc;

    #[test]
    fn committed_revision_advances_before_fallible_post_commit_work() {
        let dir =
            std::env::temp_dir().join(format!("ocg-v3-accounts-rev-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = Database::open(dir.clone()).unwrap();
        let cipher: Arc<dyn KeyCipher + Send + Sync> =
            Arc::new(StaticKeyCipher::new("v3-accounts-rev"));
        let state = Arc::new(CoreStateInner::new(db, dir.clone(), cipher).unwrap());
        let before = state.settings_revision();
        let err = with_committed_revision::<()>(&state, || {
            Err(V3ApiError::internal("injected post-commit failure"))
        })
        .expect_err("post-commit failure must surface");
        assert_eq!(err.status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(err.body.code, super::super::ERROR_INTERNAL);
        assert_eq!(state.settings_revision(), before + 1);
        drop(state);
        let _ = std::fs::remove_dir_all(dir);
    }
}

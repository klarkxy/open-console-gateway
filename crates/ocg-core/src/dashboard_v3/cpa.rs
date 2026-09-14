//! Typed Dashboard V3 control plane for one user-operated local CPA runtime.
//! Network operations are serialized and never hold SQLite or synchronous
//! state locks while awaiting CPA.

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path as AxumPath, Query, State};
use axum::http::{HeaderMap, HeaderValue, header};
use axum::response::{IntoResponse, Response};
use chrono::Utc;
use serde::Deserialize;

use crate::cpa::{self, CpaClient};
use crate::cpa_cli_import::CliRoots;
use crate::cpa_runtime::{self, CpaRuntimeError};
use crate::models::{Account as ModelAccount, AccountSetupStep, AccountType};
use crate::provider::{
    CPA_ACCOUNT_ID, CPA_ACCOUNT_NAME, CPA_PROVIDER_ID, CredentialKind, QuotaScope,
};
use crate::state::CoreState;

use super::types::{
    CpaAccount, CpaAccountDelete, CpaAccountStatusUpdate, CpaAccounts, CpaCliImportOutcome,
    CpaCliImportRequest, CpaCliImportResult, CpaCliImportSource, CpaCliImports,
    CpaConnectionReport, CpaIntegration, CpaIntegrationUpdate, CpaModel, CpaModels, CpaOAuthMethod,
    CpaOAuthProvider, CpaOAuthSessionDelete, CpaOAuthStart, CpaOAuthStartRequest, CpaOAuthStatus,
    CpaQuotaReset, CpaRuntime, CpaRuntimeCheck, CpaRuntimeInstall, CpaRuntimeKey,
    CpaRuntimeKeyCreated, CpaRuntimeKeys, CpaRuntimeLogs, CpaRuntimePhase, CpaTestRequest,
    MutationAck, MutationExpectation,
};
use super::{V3ApiError, check_expectation, parse_json, parse_mutation_json};

struct SavedCpa {
    base_url: String,
    management_key: String,
    inference_key: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct OAuthStatusQuery {
    state: String,
}

pub(super) async fn get_integration(
    State(state): State<CoreState>,
) -> Result<Json<CpaIntegration>, V3ApiError> {
    integration_view(&state).map(Json)
}

pub(super) async fn put_integration(
    State(state): State<CoreState>,
    body: Bytes,
) -> Result<Json<CpaIntegration>, V3ApiError> {
    let input = parse_mutation_json::<CpaIntegrationUpdate>(&body)?;
    let _operation = state.cpa_operations.lock().await;
    let _settings = state.settings_update.lock();
    check_expectation(&state, &input.expectation)?;

    let env_base = cpa::env_base_url().map_err(|error| map_cpa_error(&state, error))?;
    if env_base.is_some() && input.base_url.is_some() {
        return Err(V3ApiError::invalid_request_at(
            &state,
            "CPA base URL is controlled by OCG_CPA_BASE_URL in this runtime",
        ));
    }
    let (existing_record, existing_account) = {
        let db = state.db.lock();
        (
            db.cpa_integration().map_err(V3ApiError::internal)?,
            db.get_account(CPA_ACCOUNT_ID)
                .map_err(V3ApiError::internal)?,
        )
    };
    let managed = cpa_runtime::load_managed(&state.data_dir())
        .map_err(|error| map_runtime_error(&state, error))?;
    if managed.is_some() && env_base.is_some() {
        return Err(V3ApiError::invalid_request_at(
            &state,
            "OCG_CPA_BASE_URL selects an external CPA; unset it before changing the managed connection",
        ));
    }
    if managed.is_some()
        && (input.base_url.is_some()
            || input.management_key.is_some()
            || input.inference_key.is_some())
    {
        return Err(V3ApiError::invalid_request_at(
            &state,
            "managed CPA connection fields are owned by the Windows runtime; only enabled may change",
        ));
    }
    let base_url = managed
        .as_ref()
        .map(|item| format!("http://127.0.0.1:{}", item.port))
        .or(env_base)
        .or_else(|| input.base_url.clone())
        .or_else(|| {
            existing_record
                .as_ref()
                .map(|record| record.base_url.clone())
        })
        .unwrap_or_else(|| cpa::DEFAULT_CPA_BASE_URL.to_string());
    let base_url = cpa::normalize_base_url(&base_url, false)
        .or_else(|error| {
            if std::env::var_os(cpa::CPA_BASE_URL_ENV).is_some() {
                cpa::normalize_base_url(&base_url, true)
            } else {
                Err(error)
            }
        })
        .map_err(|error| map_cpa_error(&state, error))?;

    let management_key_cipher = match clean_secret(input.management_key) {
        Some(value) => state.encrypt_key(&value).map_err(V3ApiError::internal)?,
        None => existing_record
            .as_ref()
            .map(|record| record.management_key_cipher.clone())
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                V3ApiError::invalid_request_at(&state, "CPA Management Key is required")
            })?,
    };
    let inference_key_cipher = match clean_secret(input.inference_key) {
        Some(value) => state.encrypt_key(&value).map_err(V3ApiError::internal)?,
        None => existing_account
            .as_ref()
            .map(|account| account.key_cipher.clone())
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                V3ApiError::invalid_request_at(&state, "CPA Inference Key is required")
            })?,
    };
    let now = Utc::now();
    let account = ModelAccount {
        id: CPA_ACCOUNT_ID.to_string(),
        provider_id: CPA_PROVIDER_ID.to_string(),

        credential_kind: CredentialKind::ApiKey,
        quota_scope: QuotaScope::Key,
        name: CPA_ACCOUNT_NAME.to_string(),
        username: None,
        password_cipher: None,
        key_cipher: inference_key_cipher,
        enabled: input
            .enabled
            .unwrap_or_else(|| existing_account.as_ref().is_some_and(|item| item.enabled)),
        account_type: AccountType::Key,
        setup_step: AccountSetupStep::Ready,
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
        notes: None,
        created_at: existing_account
            .as_ref()
            .map_or(now, |item| item.created_at),
        updated_at: now,
    };
    state
        .db
        .lock()
        .upsert_cpa_integration(&account, &base_url, &management_key_cipher)
        .map_err(V3ApiError::internal)?;
    state.routing.reset();
    state.bump_settings_revision();
    integration_view(&state).map(Json)
}

pub(super) async fn delete_integration(
    State(state): State<CoreState>,
    body: Bytes,
) -> Result<Json<MutationAck>, V3ApiError> {
    let expectation = parse_mutation_json::<MutationExpectation>(&body)?;
    let _operation = state.cpa_operations.lock().await;
    let _settings = state.settings_update.lock();
    check_expectation(&state, &expectation)?;
    if cpa_runtime::load_managed(&state.data_dir())
        .map_err(|error| map_runtime_error(&state, error))?
        .is_some()
    {
        return Err(V3ApiError::invalid_request_at(
            &state,
            "remove the managed CPA runtime instead of deleting its connection",
        ));
    }
    state
        .disconnect_cpa_integration()
        .map_err(V3ApiError::internal)?;
    Ok(Json(committed_ack(&state)))
}

pub(super) async fn test_connection(
    State(state): State<CoreState>,
    body: Bytes,
) -> Result<Json<CpaConnectionReport>, V3ApiError> {
    let input = parse_json::<CpaTestRequest>(&body)?;
    let _operation = state.cpa_operations.lock().await;
    let env_base = cpa::env_base_url().map_err(|error| map_cpa_error(&state, error))?;
    let saved = load_saved(&state).ok();
    let managed = cpa_runtime::load_managed(&state.data_dir())
        .map_err(|error| map_runtime_error(&state, error))?;
    if managed.is_some() && env_base.is_some() {
        return Err(V3ApiError::invalid_request_at(
            &state,
            "OCG_CPA_BASE_URL selects an external CPA; unset it before testing the managed connection",
        ));
    }
    if managed.is_some()
        && (input.base_url.is_some()
            || input.management_key.is_some()
            || input.inference_key.is_some())
    {
        return Err(V3ApiError::invalid_request_at(
            &state,
            "managed CPA connection tests use only the owned runtime connection",
        ));
    }
    let base_url = if managed.is_some() {
        saved.as_ref().map(|item| item.base_url.clone())
    } else {
        env_base.or(input.base_url)
    }
    .or_else(|| saved.as_ref().map(|item| item.base_url.clone()))
    .unwrap_or_else(|| cpa::DEFAULT_CPA_BASE_URL.to_string());
    let management_key = clean_secret(input.management_key)
        .or_else(|| saved.as_ref().map(|item| item.management_key.clone()))
        .ok_or_else(|| V3ApiError::invalid_request_at(&state, "CPA Management Key is required"))?;
    let inference_key = clean_secret(input.inference_key)
        .or_else(|| saved.as_ref().map(|item| item.inference_key.clone()))
        .ok_or_else(|| V3ApiError::invalid_request_at(&state, "CPA Inference Key is required"))?;
    let client = CpaClient::new(
        &state.config(),
        &base_url,
        management_key.clone(),
        inference_key.clone(),
        std::env::var_os(cpa::CPA_BASE_URL_ENV).is_some(),
    )
    .map_err(|error| map_cpa_error(&state, error))?;
    let report = client
        .test()
        .await
        .map_err(|error| map_cpa_error(&state, error))?;
    let management_error = report
        .management_error
        .map(|message| redact_cpa_message(&message, &[&management_key, &inference_key]));
    let inference_error = report
        .inference_error
        .map(|message| redact_cpa_message(&message, &[&management_key, &inference_key]));
    Ok(Json(CpaConnectionReport {
        reachable: report.reachable,
        management_ready: report.management_ready,
        inference_ready: report.inference_ready,
        version: report.version.as_ref().map(|item| item.version.clone()),
        commit: report.version.as_ref().and_then(|item| item.commit.clone()),
        build_date: report
            .version
            .as_ref()
            .and_then(|item| item.build_date.clone()),
        model_count: report.model_count,
        management_error,
        inference_error,
        revision: state.settings_revision(),
        process_generation: state.process_generation(),
    }))
}

pub(super) async fn get_models(
    State(state): State<CoreState>,
) -> Result<Json<CpaModels>, V3ApiError> {
    let catalog = {
        let db = state.db.lock();
        db.cpa_model_catalog().map_err(V3ApiError::internal)?
    };
    Ok(Json(models_payload(&state, catalog.as_ref())))
}

pub(super) async fn refresh_models(
    State(state): State<CoreState>,
    body: Bytes,
) -> Result<Json<CpaModels>, V3ApiError> {
    let expectation = parse_mutation_json::<MutationExpectation>(&body)?;
    let _operation = state.cpa_operations.lock().await;
    {
        let _settings = state.settings_update.lock();
        check_expectation(&state, &expectation)?;
    }
    let (client, base_url) = saved_client(&state)?;
    let incoming = client
        .models()
        .await
        .map_err(|error| map_cpa_error(&state, error))?;
    let previous = {
        let db = state.db.lock();
        db.cpa_model_catalog().map_err(V3ApiError::internal)?
    };
    let models = crate::db::CpaCatalogModel::merge_refresh(
        incoming,
        previous
            .as_ref()
            .map(|item| item.models.as_slice())
            .unwrap_or(&[]),
    );
    let _settings = state.settings_update.lock();
    check_expectation(&state, &expectation)?;
    let refreshed_at = Utc::now();
    state
        .activate_cpa_model_catalog(models.clone(), &base_url, refreshed_at)
        .map_err(V3ApiError::internal)?;
    let revision = state.bump_settings_revision();
    Ok(Json(CpaModels {
        models: models.iter().map(cpa_model_view).collect(),
        source_url: Some(base_url),
        refreshed_at: Some(refreshed_at.to_rfc3339()),
        revision,
        process_generation: state.process_generation(),
    }))
}

pub(super) async fn list_accounts(
    State(state): State<CoreState>,
) -> Result<Json<CpaAccounts>, V3ApiError> {
    let _operation = state.cpa_operations.lock().await;
    let (client, _) = saved_client(&state)?;
    let (version, accounts) = client
        .accounts()
        .await
        .map_err(|error| map_cpa_error(&state, error))?;
    Ok(Json(CpaAccounts {
        accounts: accounts.into_iter().map(account_view).collect(),
        version: version.version,
        revision: state.settings_revision(),
        process_generation: state.process_generation(),
    }))
}

pub(super) async fn set_account_status(
    State(state): State<CoreState>,
    body: Bytes,
) -> Result<Json<MutationAck>, V3ApiError> {
    let input = parse_mutation_json::<CpaAccountStatusUpdate>(&body)?;
    let _operation = state.cpa_operations.lock().await;
    check_before_external_write(&state, &input.expectation)?;
    let (client, _) = saved_client(&state)?;
    client
        .set_account_disabled(&input.name, &input.auth_index, input.disabled)
        .await
        .map_err(|error| map_cpa_error(&state, error))?;
    Ok(Json(committed_ack(&state)))
}

pub(super) async fn delete_account(
    State(state): State<CoreState>,
    body: Bytes,
) -> Result<Json<MutationAck>, V3ApiError> {
    let input = parse_mutation_json::<CpaAccountDelete>(&body)?;
    let _operation = state.cpa_operations.lock().await;
    check_before_external_write(&state, &input.expectation)?;
    let (client, _) = saved_client(&state)?;
    client
        .delete_account(&input.name, &input.auth_index)
        .await
        .map_err(|error| map_cpa_error(&state, error))?;
    Ok(Json(committed_ack(&state)))
}

pub(super) async fn reset_quota(
    State(state): State<CoreState>,
    body: Bytes,
) -> Result<Json<MutationAck>, V3ApiError> {
    let input = parse_mutation_json::<CpaQuotaReset>(&body)?;
    let _operation = state.cpa_operations.lock().await;
    check_before_external_write(&state, &input.expectation)?;
    let (client, _) = saved_client(&state)?;
    client
        .reset_quota(&input.name, &input.auth_index)
        .await
        .map_err(|error| map_cpa_error(&state, error))?;
    Ok(Json(committed_ack(&state)))
}

fn require_local_cli_import(state: &CoreState, headers: &HeaderMap) -> Result<(), V3ApiError> {
    if !crate::dashboard_session::is_local_dashboard_request(state.dashboard_local_mode(), headers)
    {
        return Err(V3ApiError::forbidden_at(
            state,
            "CLI account import is only available in the local OCG dashboard.",
        ));
    }
    Ok(())
}

fn cli_import_response(value: impl serde::Serialize) -> Response {
    let mut response = Json(value).into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

pub(super) async fn cli_import_sources(
    State(state): State<CoreState>,
    headers: HeaderMap,
) -> Result<Response, V3ApiError> {
    require_local_cli_import(&state, &headers)?;
    let roots =
        CliRoots::from_env().map_err(|message| V3ApiError::invalid_request_at(&state, message))?;
    let sources = roots
        .discover()
        .into_iter()
        .map(|source| CpaCliImportSource {
            provider: match source.provider {
                cpa::CpaOAuthProvider::Codex => CpaOAuthProvider::Codex,
                cpa::CpaOAuthProvider::Anthropic => CpaOAuthProvider::Anthropic,
                cpa::CpaOAuthProvider::Antigravity => CpaOAuthProvider::Antigravity,
                cpa::CpaOAuthProvider::Kimi => CpaOAuthProvider::Kimi,
                cpa::CpaOAuthProvider::Xai => CpaOAuthProvider::Xai,
            },
            source: source.source.into(),
            supported: source.supported,
            available: source.available,
            reason: source.reason.map(str::to_owned),
        })
        .collect();
    Ok(cli_import_response(CpaCliImports { sources }))
}

pub(super) async fn import_cli_account(
    State(state): State<CoreState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, V3ApiError> {
    require_local_cli_import(&state, &headers)?;
    let input = parse_mutation_json::<CpaCliImportRequest>(&body)?;
    let _operation = state.cpa_operations.lock().await;
    check_before_external_write(&state, &input.expectation)?;
    let (client, _) = saved_client(&state)?;
    let roots =
        CliRoots::from_env().map_err(|message| V3ApiError::invalid_request_at(&state, message))?;
    let credential = roots
        .read(cpa_provider(input.provider))
        .map_err(|message| V3ApiError::invalid_request_at(&state, message))?;
    import_cli_credential(&state, &client, input, credential).await
}

async fn import_cli_credential(
    state: &CoreState,
    client: &CpaClient,
    input: CpaCliImportRequest,
    credential: crate::cpa_cli_import::ImportedCredential,
) -> Result<Response, V3ApiError> {
    let (_, accounts) = client
        .accounts()
        .await
        .map_err(|error| map_cpa_error(state, error))?;
    let existing = accounts
        .iter()
        .find(|account| account.name.eq_ignore_ascii_case(&credential.name));
    check_before_external_write(state, &input.expectation)?;
    let outcome = if let Some(existing) = existing {
        if existing.provider != credential.cpa_provider || existing.runtime_only {
            return Err(V3ApiError::invalid_request_at(
                state,
                "CPA import filename conflicts with an existing account.",
            ));
        }
        CpaCliImportOutcome::AlreadyImported
    } else {
        // A failed response can follow a committed upstream file write. Keep the
        // same deterministic name and reconcile instead of blindly creating again.
        let uploaded = client.upload_cli_credential(&credential).await;
        let confirmed = client.accounts().await.is_ok_and(|(_, accounts)| {
            accounts.iter().any(|account| {
                account.name.eq_ignore_ascii_case(&credential.name)
                    && account.provider == credential.cpa_provider
                    && !account.runtime_only
            })
        });
        if !confirmed
            && let Err(
                error @ cpa::CpaError::Http {
                    status: 400..=499, ..
                },
            ) = uploaded
        {
            return Err(map_cpa_error(state, error));
        }
        // Even uncertain writes invalidate the consumed CAS token; status is
        // explicit and a retry of the same identity will reconcile by filename.
        state.bump_settings_revision();
        if confirmed {
            CpaCliImportOutcome::Imported
        } else {
            CpaCliImportOutcome::Unconfirmed
        }
    };
    Ok(cli_import_response(CpaCliImportResult {
        provider: input.provider,
        name: credential.name.clone(),
        outcome,
        revision: state.settings_revision(),
        process_generation: state.process_generation(),
    }))
}

pub(super) async fn start_oauth(
    State(state): State<CoreState>,
    body: Bytes,
) -> Result<Json<CpaOAuthStart>, V3ApiError> {
    let input = parse_mutation_json::<CpaOAuthStartRequest>(&body)?;
    let _operation = state.cpa_operations.lock().await;
    check_before_external_write(&state, &input.expectation)?;
    let started = match input.method {
        CpaOAuthMethod::Browser => {
            let (client, _) = saved_client(&state)?;
            client
                .start_oauth(cpa_provider(input.provider))
                .await
                .map_err(|error| map_cpa_error(&state, error))?
        }
        CpaOAuthMethod::Device => {
            if input.provider != CpaOAuthProvider::Codex {
                return Err(V3ApiError::invalid_request_at(
                    &state,
                    "Device method is only supported for managed Codex login.",
                ));
            }
            let started = state
                .start_cpa_device_oauth()
                .await
                .map_err(|error| map_runtime_error(&state, error))?;
            if let Err(error) = check_before_external_write(&state, &input.expectation) {
                let _ = state.cancel_cpa_device_oauth(&started.state);
                return Err(error);
            }
            started
        }
    };
    let revision = state.bump_settings_revision();
    Ok(Json(CpaOAuthStart {
        provider: input.provider,
        state: started.state,
        url: started.url,
        flow: started.flow,
        user_code: started.user_code,
        expires_in: started.expires_in,
        revision,
        process_generation: state.process_generation(),
    }))
}

pub(super) async fn oauth_status(
    State(state): State<CoreState>,
    Query(query): Query<OAuthStatusQuery>,
) -> Result<Json<CpaOAuthStatus>, V3ApiError> {
    let _operation = state.cpa_operations.lock().await;
    let status = if let Some(status) = state.cpa_device_oauth_status(&query.state) {
        status.map_err(|error| map_runtime_error(&state, error))?
    } else {
        let (client, _) = saved_client(&state)?;
        client
            .oauth_status(&query.state)
            .await
            .map_err(|error| map_cpa_error(&state, error))?
    };
    Ok(Json(CpaOAuthStatus {
        state: query.state,
        status: status.status,
        error: status.error,
        revision: state.settings_revision(),
        process_generation: state.process_generation(),
    }))
}

pub(super) async fn get_runtime(
    State(state): State<CoreState>,
) -> Result<Json<CpaRuntime>, V3ApiError> {
    Ok(Json(runtime_view(&state, state.cpa_runtime_snapshot())))
}

pub(super) async fn check_runtime_update(
    State(state): State<CoreState>,
    body: Bytes,
) -> Result<Json<CpaRuntimeCheck>, V3ApiError> {
    let expectation = parse_mutation_json::<MutationExpectation>(&body)?;
    let _operation = state.cpa_operations.lock().await;
    check_before_external_write(&state, &expectation)?;
    let check = state
        .check_cpa_runtime_update(
            expectation.expected_revision,
            expectation.process_generation,
        )
        .await
        .map_err(|error| map_runtime_error(&state, error))?;
    Ok(Json(CpaRuntimeCheck {
        current_version: check.current_version,
        latest_version: check.latest_version,
        update_available: check.update_available,
        release_url: check.release_url,
        revision: state.settings_revision(),
        process_generation: state.process_generation(),
    }))
}

pub(super) async fn install_runtime(
    State(state): State<CoreState>,
    body: Bytes,
) -> Result<Json<CpaRuntime>, V3ApiError> {
    let input = parse_mutation_json::<CpaRuntimeInstall>(&body)?;
    let _operation = state.cpa_operations.lock().await;
    {
        let _settings = state.settings_update.lock();
        check_expectation(&state, &input.expectation)?;
    }
    let snapshot = state
        .install_cpa_runtime(
            input.expectation.expected_revision,
            input.expectation.process_generation,
            input.expected_version.as_deref(),
        )
        .await
        .map_err(|error| map_runtime_error(&state, error))?;
    Ok(Json(runtime_view(&state, snapshot)))
}

pub(super) async fn update_runtime(
    State(state): State<CoreState>,
    body: Bytes,
) -> Result<Json<CpaRuntime>, V3ApiError> {
    let input = parse_mutation_json::<CpaRuntimeInstall>(&body)?;
    let _operation = state.cpa_operations.lock().await;
    {
        let _settings = state.settings_update.lock();
        check_expectation(&state, &input.expectation)?;
    }
    let snapshot = state
        .update_cpa_runtime(
            input.expectation.expected_revision,
            input.expectation.process_generation,
            input.expected_version.as_deref(),
        )
        .await
        .map_err(|error| map_runtime_error(&state, error))?;
    Ok(Json(runtime_view(&state, snapshot)))
}

pub(super) async fn remove_runtime(
    State(state): State<CoreState>,
    body: Bytes,
) -> Result<Json<CpaRuntime>, V3ApiError> {
    let expectation = parse_mutation_json::<MutationExpectation>(&body)?;
    let _operation = state.cpa_operations.lock().await;
    {
        let _settings = state.settings_update.lock();
        check_expectation(&state, &expectation)?;
    }
    let snapshot = state
        .remove_cpa_runtime(
            expectation.expected_revision,
            expectation.process_generation,
        )
        .await
        .map_err(|error| map_runtime_error(&state, error))?;
    Ok(Json(runtime_view(&state, snapshot)))
}

pub(super) async fn start_runtime(
    State(state): State<CoreState>,
    body: Bytes,
) -> Result<Json<CpaRuntime>, V3ApiError> {
    let expectation = parse_mutation_json::<MutationExpectation>(&body)?;
    let _operation = state.cpa_operations.lock().await;
    {
        let _settings = state.settings_update.lock();
        check_expectation(&state, &expectation)?;
    }
    let snapshot = state
        .start_cpa_runtime(
            expectation.expected_revision,
            expectation.process_generation,
        )
        .await
        .map_err(|error| map_runtime_error(&state, error))?;
    Ok(Json(runtime_view(&state, snapshot)))
}

pub(super) async fn stop_runtime(
    State(state): State<CoreState>,
    body: Bytes,
) -> Result<Json<CpaRuntime>, V3ApiError> {
    let expectation = parse_mutation_json::<MutationExpectation>(&body)?;
    let _operation = state.cpa_operations.lock().await;
    {
        let _settings = state.settings_update.lock();
        check_expectation(&state, &expectation)?;
    }
    let snapshot = state
        .stop_cpa_runtime(
            expectation.expected_revision,
            expectation.process_generation,
        )
        .map_err(|error| map_runtime_error(&state, error))?;
    Ok(Json(runtime_view(&state, snapshot)))
}

pub(super) async fn rollback_runtime(
    State(state): State<CoreState>,
    body: Bytes,
) -> Result<Json<CpaRuntime>, V3ApiError> {
    let expectation = parse_mutation_json::<MutationExpectation>(&body)?;
    let _operation = state.cpa_operations.lock().await;
    {
        let _settings = state.settings_update.lock();
        check_expectation(&state, &expectation)?;
    }
    let snapshot = state
        .rollback_cpa_runtime(
            expectation.expected_revision,
            expectation.process_generation,
        )
        .await
        .map_err(|error| map_runtime_error(&state, error))?;
    Ok(Json(runtime_view(&state, snapshot)))
}

pub(super) async fn get_runtime_logs(
    State(state): State<CoreState>,
) -> Result<Json<CpaRuntimeLogs>, V3ApiError> {
    let logs = state
        .cpa_runtime_logs()
        .map_err(|error| map_runtime_error(&state, error))?;
    Ok(Json(CpaRuntimeLogs {
        stdout: logs.stdout,
        stderr: logs.stderr,
        revision: state.settings_revision(),
        process_generation: state.process_generation(),
    }))
}

pub(super) async fn list_runtime_keys(
    State(state): State<CoreState>,
) -> Result<Json<CpaRuntimeKeys>, V3ApiError> {
    let _operation = state.cpa_operations.lock().await;
    let keys = state
        .list_cpa_runtime_keys()
        .await
        .map_err(|error| map_runtime_error(&state, error))?;
    Ok(Json(CpaRuntimeKeys {
        keys: keys
            .into_iter()
            .map(|key| CpaRuntimeKey {
                fingerprint: key.fingerprint,
                hint: key.hint,
                protected: key.protected,
            })
            .collect(),
        revision: state.settings_revision(),
        process_generation: state.process_generation(),
    }))
}

pub(super) async fn create_runtime_key(
    State(state): State<CoreState>,
    body: Bytes,
) -> Result<Response, V3ApiError> {
    let expectation = parse_mutation_json::<MutationExpectation>(&body)?;
    let _operation = state.cpa_operations.lock().await;
    {
        let _settings = state.settings_update.lock();
        check_expectation(&state, &expectation)?;
    }
    let created = state
        .create_cpa_runtime_key(
            expectation.expected_revision,
            expectation.process_generation,
        )
        .await
        .map_err(|error| map_runtime_error(&state, error))?;
    Ok(one_time_key_response(CpaRuntimeKeyCreated {
        fingerprint: created.fingerprint,
        hint: created.hint,
        secret: created.secret,
        revision: state.settings_revision(),
        process_generation: state.process_generation(),
    }))
}

pub(super) async fn delete_runtime_key(
    State(state): State<CoreState>,
    AxumPath(fingerprint): AxumPath<String>,
    body: Bytes,
) -> Result<Json<MutationAck>, V3ApiError> {
    let expectation = parse_mutation_json::<MutationExpectation>(&body)?;
    let _operation = state.cpa_operations.lock().await;
    {
        let _settings = state.settings_update.lock();
        check_expectation(&state, &expectation)?;
    }
    state
        .delete_cpa_runtime_key(
            expectation.expected_revision,
            expectation.process_generation,
            &fingerprint,
        )
        .await
        .map_err(|error| map_runtime_error(&state, error))?;
    Ok(Json(current_ack(&state)))
}

pub(super) async fn rotate_runtime_key(
    State(state): State<CoreState>,
    AxumPath(fingerprint): AxumPath<String>,
    body: Bytes,
) -> Result<Response, V3ApiError> {
    let expectation = parse_mutation_json::<MutationExpectation>(&body)?;
    let _operation = state.cpa_operations.lock().await;
    {
        let _settings = state.settings_update.lock();
        check_expectation(&state, &expectation)?;
    }
    let created = state
        .rotate_cpa_runtime_key(
            expectation.expected_revision,
            expectation.process_generation,
            &fingerprint,
        )
        .await
        .map_err(|error| map_runtime_error(&state, error))?;
    Ok(one_time_key_response(CpaRuntimeKeyCreated {
        fingerprint: created.fingerprint,
        hint: created.hint,
        secret: created.secret,
        revision: state.settings_revision(),
        process_generation: state.process_generation(),
    }))
}

pub(super) async fn cancel_oauth(
    State(state): State<CoreState>,
    body: Bytes,
) -> Result<Json<MutationAck>, V3ApiError> {
    let input = parse_mutation_json::<CpaOAuthSessionDelete>(&body)?;
    let _operation = state.cpa_operations.lock().await;
    check_before_external_write(&state, &input.expectation)?;
    if let Some(cancelled) = state.cancel_cpa_device_oauth(&input.state) {
        let cancelled = cancelled.map_err(|error| map_runtime_error(&state, error))?;
        return Ok(Json(if cancelled {
            committed_ack(&state)
        } else {
            current_ack(&state)
        }));
    }
    let (client, _) = saved_client(&state)?;
    client
        .cancel_oauth(&input.state)
        .await
        .map_err(|error| map_cpa_error(&state, error))?;
    Ok(Json(committed_ack(&state)))
}

fn cpa_model_view(model: &crate::db::CpaCatalogModel) -> CpaModel {
    CpaModel {
        id: model.id.clone(),
        owned_by: model.owned_by.clone(),
    }
}

fn models_payload(state: &CoreState, catalog: Option<&crate::db::CpaCatalogRecord>) -> CpaModels {
    CpaModels {
        models: catalog
            .map(|item| item.models.iter().map(cpa_model_view).collect())
            .unwrap_or_default(),
        source_url: catalog.map(|item| item.source_url.clone()),
        refreshed_at: catalog
            .and_then(|item| item.refreshed_at)
            .map(|value| value.to_rfc3339()),
        revision: state.settings_revision(),
        process_generation: state.process_generation(),
    }
}

fn integration_view(state: &CoreState) -> Result<CpaIntegration, V3ApiError> {
    let env_base = cpa::env_base_url().map_err(|error| map_cpa_error(state, error))?;
    let managed = cpa_runtime::load_managed(&state.data_dir())
        .map_err(|error| map_runtime_error(state, error))?;
    let runtime = state.cpa_runtime_snapshot();
    let (record, account, catalog) = {
        let db = state.db.lock();
        (
            db.cpa_integration().map_err(V3ApiError::internal)?,
            db.get_account(CPA_ACCOUNT_ID)
                .map_err(V3ApiError::internal)?,
            db.cpa_model_catalog().map_err(V3ApiError::internal)?,
        )
    };
    Ok(CpaIntegration {
        configured: record.is_some() && account.is_some(),
        base_url: env_base
            .clone()
            .or_else(|| {
                managed
                    .as_ref()
                    .map(|item| format!("http://127.0.0.1:{}", item.port))
            })
            .or_else(|| record.as_ref().map(|item| item.base_url.clone()))
            .unwrap_or_else(|| cpa::DEFAULT_CPA_BASE_URL.to_string()),
        base_url_read_only: env_base.is_some() || managed.is_some(),
        management_key_configured: record
            .as_ref()
            .is_some_and(|item| !item.management_key_cipher.is_empty()),
        inference_key_configured: account
            .as_ref()
            .is_some_and(|item| !item.key_cipher.is_empty()),
        enabled: account.as_ref().is_some_and(|item| item.enabled),
        account_id: account.map(|item| item.id),
        model_count: catalog.as_ref().map_or(0, |item| item.models.len()),
        models_refreshed_at: catalog
            .and_then(|item| item.refreshed_at)
            .map(|value| value.to_rfc3339()),
        runtime_supported: state.cpa_runtime_supported(),
        runtime_owned: managed.is_some(),
        runtime_running: runtime.running,
        installed_version: managed
            .as_ref()
            .map(|managed| managed.current_version.clone()),
        latest_version: runtime.latest_version,
        update_available: runtime.update_available,
        current_operation: runtime.current_operation,
        runtime_unavailable_reason: if !state.cpa_runtime_supported() {
            Some(cpa_runtime::UNAVAILABLE_REASON.to_string())
        } else if managed.is_some() && env_base.is_some() {
            Some("OCG_CPA_BASE_URL selects an external CPA; unset it to manage the installed runtime".into())
        } else {
            None
        },
        revision: state.settings_revision(),
        process_generation: state.process_generation(),
    })
}

fn runtime_view(state: &CoreState, snapshot: cpa_runtime::CpaRuntimeSnapshot) -> CpaRuntime {
    CpaRuntime {
        supported: snapshot.supported,
        unavailable_reason: snapshot.unavailable_reason,
        installed: snapshot.installed,
        running: snapshot.running,
        owned: snapshot.owned,
        current_version: snapshot.current_version,
        previous_version: snapshot.previous_version,
        asset_sha256: snapshot.asset_sha256,
        port: snapshot.port,
        base_url: snapshot.base_url,
        phase: runtime_phase(snapshot.phase),
        error: snapshot.error,
        latest_version: snapshot.latest_version,
        update_available: snapshot.update_available,
        current_operation: snapshot.current_operation,
        revision: state.settings_revision(),
        process_generation: state.process_generation(),
    }
}

fn runtime_phase(phase: cpa_runtime::CpaRuntimePhase) -> CpaRuntimePhase {
    match phase {
        cpa_runtime::CpaRuntimePhase::Idle => CpaRuntimePhase::Idle,
        cpa_runtime::CpaRuntimePhase::Checking => CpaRuntimePhase::Checking,
        cpa_runtime::CpaRuntimePhase::Downloading => CpaRuntimePhase::Downloading,
        cpa_runtime::CpaRuntimePhase::Installing => CpaRuntimePhase::Installing,
        cpa_runtime::CpaRuntimePhase::Starting => CpaRuntimePhase::Starting,
        cpa_runtime::CpaRuntimePhase::Failed => CpaRuntimePhase::Failed,
    }
}

fn load_saved(state: &CoreState) -> Result<SavedCpa, V3ApiError> {
    let (record, account) = {
        let db = state.db.lock();
        (
            db.cpa_integration().map_err(V3ApiError::internal)?,
            db.get_account(CPA_ACCOUNT_ID)
                .map_err(V3ApiError::internal)?,
        )
    };
    let record =
        record.ok_or_else(|| V3ApiError::precondition_failed_at(state, "CPA is not configured"))?;
    let account = account.ok_or_else(|| {
        V3ApiError::precondition_failed_at(state, "CPA singleton account is missing")
    })?;
    let managed = cpa_runtime::load_managed(&state.data_dir())
        .map_err(|error| map_runtime_error(state, error))?
        .is_some();
    let env_base = cpa::env_base_url().map_err(|error| map_cpa_error(state, error))?;
    if managed && env_base.is_some() {
        return Err(V3ApiError::invalid_request_at(
            state,
            "OCG_CPA_BASE_URL selects an external CPA; unset it before managing the installed runtime",
        ));
    }
    let base_url = env_base.unwrap_or(record.base_url);
    Ok(SavedCpa {
        base_url,
        management_key: state
            .decrypt_key(&record.management_key_cipher)
            .map_err(V3ApiError::internal)?,
        inference_key: state
            .decrypt_key(&account.key_cipher)
            .map_err(V3ApiError::internal)?,
    })
}

fn saved_client(state: &CoreState) -> Result<(CpaClient, String), V3ApiError> {
    let saved = load_saved(state)?;
    let base_url = saved.base_url.clone();
    let client = CpaClient::new(
        &state.config(),
        &saved.base_url,
        saved.management_key,
        saved.inference_key,
        std::env::var_os(cpa::CPA_BASE_URL_ENV).is_some(),
    )
    .map_err(|error| map_cpa_error(state, error))?;
    Ok((client, base_url))
}

fn account_view(item: cpa::CpaAccountView) -> CpaAccount {
    CpaAccount {
        name: item.name,
        auth_index: item.auth_index,
        provider: item.provider,
        label: item.label,
        status: item.status,
        status_message: item.status_message,
        disabled: item.disabled,
        unavailable: item.unavailable,
        runtime_only: item.runtime_only,
        mutable: item.mutable,
        email: item.email,
        quota: item.quota,
    }
}

fn cpa_provider(provider: CpaOAuthProvider) -> cpa::CpaOAuthProvider {
    match provider {
        CpaOAuthProvider::Codex => cpa::CpaOAuthProvider::Codex,
        CpaOAuthProvider::Anthropic => cpa::CpaOAuthProvider::Anthropic,
        CpaOAuthProvider::Antigravity => cpa::CpaOAuthProvider::Antigravity,
        CpaOAuthProvider::Kimi => cpa::CpaOAuthProvider::Kimi,
        CpaOAuthProvider::Xai => cpa::CpaOAuthProvider::Xai,
    }
}

fn clean_secret(value: Option<String>) -> Option<String> {
    value
        .map(|item| item.trim().to_string())
        .filter(|item| !item.is_empty())
}

fn check_before_external_write(
    state: &CoreState,
    expectation: &MutationExpectation,
) -> Result<(), V3ApiError> {
    let _settings = state.settings_update.lock();
    check_expectation(state, expectation)
}

fn current_ack(state: &CoreState) -> MutationAck {
    MutationAck {
        revision: state.settings_revision(),
        process_generation: state.process_generation(),
    }
}

fn one_time_key_response(created: CpaRuntimeKeyCreated) -> Response {
    let mut response = Json(created).into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

fn committed_ack(state: &CoreState) -> MutationAck {
    let revision = state.bump_settings_revision();
    MutationAck {
        revision,
        process_generation: state.process_generation(),
    }
}

fn map_runtime_error(state: &CoreState, error: CpaRuntimeError) -> V3ApiError {
    match error {
        CpaRuntimeError::Unavailable(message) => V3ApiError::invalid_request_at(state, message),
        CpaRuntimeError::Invalid(message) => V3ApiError::invalid_request_at(state, message),
        CpaRuntimeError::Conflict(message) if message == "revisionConflict" => {
            V3ApiError::revision_conflict(state)
        }
        CpaRuntimeError::Conflict(message) => V3ApiError::conflict_at(state, message),
        CpaRuntimeError::Unreachable(message) => {
            V3ApiError::service_unavailable(state, format!("CPA is unreachable: {message}"))
        }
        CpaRuntimeError::Failed(message) => V3ApiError::outbound_failed(state, message),
    }
}

fn map_cpa_error(state: &CoreState, error: cpa::CpaError) -> V3ApiError {
    let known = known_cpa_secrets(state);
    let secrets = known.iter().map(String::as_str).collect::<Vec<_>>();
    match error {
        cpa::CpaError::Invalid(message) => {
            V3ApiError::invalid_request_at(state, redact_cpa_message(&message, &secrets))
        }
        cpa::CpaError::Unreachable(message) => V3ApiError::service_unavailable(
            state,
            format!(
                "CPA is unreachable: {}",
                redact_cpa_message(&message, &secrets)
            ),
        ),
        cpa::CpaError::Http { status, message } => V3ApiError::outbound_failed(
            state,
            format!(
                "CPA returned HTTP {status}: {}",
                redact_cpa_message(&message, &secrets)
            ),
        ),
        cpa::CpaError::Response(message) | cpa::CpaError::Incompatible(message) => {
            V3ApiError::outbound_failed(state, redact_cpa_message(&message, &secrets))
        }
    }
}

fn known_cpa_secrets(state: &CoreState) -> Vec<String> {
    let (record, account) = {
        let db = state.db.lock();
        (
            db.cpa_integration().ok().flatten(),
            db.get_account(CPA_ACCOUNT_ID).ok().flatten(),
        )
    };
    [
        record.and_then(|record| state.decrypt_key(&record.management_key_cipher).ok()),
        account.and_then(|account| state.decrypt_key(&account.key_cipher).ok()),
    ]
    .into_iter()
    .flatten()
    .collect()
}

fn redact_cpa_message(message: &str, secrets: &[&str]) -> String {
    secrets
        .iter()
        .filter(|secret| !secret.is_empty())
        .fold(message.to_string(), |message, secret| {
            message.replace(secret, "[REDACTED]")
        })
}

#[cfg(test)]
#[path = "cpa/device_tests.rs"]
mod device_tests;

#[cfg(test)]
#[path = "cpa/cli_import_tests.rs"]
mod cli_import_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cpa_runtime::{
        CpaRuntimeError, CpaRuntimeLogTail, CpaRuntimeProcessHost, CpaRuntimeProcessSpec,
        CpaRuntimeSecret,
    };
    use crate::crypto::{KeyCipher, StaticKeyCipher};
    use crate::dashboard_v3::ERROR_REVISION_CONFLICT;
    use crate::db::Database;
    use crate::state::CoreStateInner;
    use axum::Router;
    use axum::extract::{Query, State as AxumState};
    use axum::http::StatusCode;
    use axum::response::IntoResponse;
    use axum::routing::{delete, get, patch, post};
    use serde_json::{Value, json};
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    fn test_state(label: &str) -> (std::path::PathBuf, CoreState) {
        let dir = std::env::temp_dir().join(format!("ocg-v3-cpa-{label}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let cipher: Arc<dyn KeyCipher + Send + Sync> =
            Arc::new(StaticKeyCipher::new("v3-cpa-test"));
        let state = Arc::new(
            CoreStateInner::new(Database::open(dir.clone()).unwrap(), dir.clone(), cipher).unwrap(),
        );
        (dir, state)
    }

    #[tokio::test]
    async fn config_is_singleton_cas_encrypted_secret_free_and_disconnectable() {
        let (dir, state) = test_state("lifecycle");
        let body = Bytes::from(
            serde_json::to_vec(&json!({
                "expectedRevision": state.settings_revision(),
                "processGeneration": state.process_generation(),
                "managementKey": "management-secret",
                "inferenceKey": "inference-secret",
                "enabled": true
            }))
            .unwrap(),
        );
        let Ok(Json(view)) = put_integration(State(state.clone()), body).await else {
            panic!("CPA configuration should save");
        };
        assert!(view.configured);
        assert!(view.management_key_configured);
        assert!(view.inference_key_configured);
        assert!(view.enabled);
        let encoded = serde_json::to_string(&view).unwrap();
        assert!(!encoded.contains("management-secret"));
        assert!(!encoded.contains("inference-secret"));
        assert!(!encoded.contains("cipher"));

        let (record, account) = {
            let db = state.db.lock();
            (
                db.cpa_integration().unwrap().unwrap(),
                db.get_account(CPA_ACCOUNT_ID).unwrap().unwrap(),
            )
        };
        assert_eq!(
            state.decrypt_key(&record.management_key_cipher).unwrap(),
            "management-secret"
        );
        assert_eq!(
            state.decrypt_key(&account.key_cipher).unwrap(),
            "inference-secret"
        );

        let delete = Bytes::from(
            serde_json::to_vec(&json!({
                "expectedRevision": state.settings_revision(),
                "processGeneration": state.process_generation()
            }))
            .unwrap(),
        );
        assert!(
            delete_integration(State(state.clone()), delete)
                .await
                .is_ok()
        );
        assert!(state.db.lock().cpa_integration().unwrap().is_none());
        assert!(
            state
                .db
                .lock()
                .get_account(CPA_ACCOUNT_ID)
                .unwrap()
                .is_none()
        );
        drop(state);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn stale_config_write_is_rejected_before_persistence() {
        let (dir, state) = test_state("cas");
        let body = Bytes::from(
            serde_json::to_vec(&json!({
                "expectedRevision": state.settings_revision().wrapping_sub(1),
                "processGeneration": state.process_generation(),
                "managementKey": "management-secret",
                "inferenceKey": "inference-secret"
            }))
            .unwrap(),
        );
        assert!(put_integration(State(state.clone()), body).await.is_err());
        assert!(state.db.lock().cpa_integration().unwrap().is_none());
        drop(state);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn queued_external_write_rechecks_cas_after_serialization() {
        let (dir, state) = test_state("queued-cas");
        let configure = Bytes::from(
            serde_json::to_vec(&json!({
                "expectedRevision": state.settings_revision(),
                "processGeneration": state.process_generation(),
                "managementKey": "management-secret",
                "inferenceKey": "inference-secret"
            }))
            .unwrap(),
        );
        assert!(
            put_integration(State(state.clone()), configure)
                .await
                .is_ok(),
            "CPA configuration should save"
        );

        let queued_revision = state.settings_revision();
        let operation = state.cpa_operations.lock().await;
        let request = Bytes::from(
            serde_json::to_vec(&json!({
                "expectedRevision": queued_revision,
                "processGeneration": state.process_generation(),
                "provider": "codex"
            }))
            .unwrap(),
        );
        let queued_state = state.clone();
        let queued = tokio::spawn(async move { start_oauth(State(queued_state), request).await });
        tokio::task::yield_now().await;
        state.bump_settings_revision();
        drop(operation);

        let error = queued
            .await
            .expect("queued handler should finish")
            .expect_err("stale queued write must be rejected before CPA network I/O");
        assert_eq!(error.status, StatusCode::CONFLICT);

        drop(state);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn managed_runtime_blocks_connection_secrets_and_disconnect_but_allows_enabled() {
        let (dir, state) = test_state("managed-fields");
        let configure = Bytes::from(
            serde_json::to_vec(&json!({
                "expectedRevision": state.settings_revision(),
                "processGeneration": state.process_generation(),
                "managementKey": "management-secret",
                "inferenceKey": "inference-secret",
                "enabled": false
            }))
            .unwrap(),
        );
        assert!(
            put_integration(State(state.clone()), configure)
                .await
                .is_ok()
        );
        cpa_runtime::save_managed(
            &dir,
            &cpa_runtime::ManagedCpa {
                current_version: "7.2.147".into(),
                previous_version: None,
                asset_sha256: "a".repeat(64),
                port: 8317,
            },
        )
        .unwrap();

        let secret_change = Bytes::from(
            serde_json::to_vec(&json!({
                "expectedRevision": state.settings_revision(),
                "processGeneration": state.process_generation(),
                "inferenceKey": "replacement"
            }))
            .unwrap(),
        );
        assert!(
            put_integration(State(state.clone()), secret_change)
                .await
                .is_err()
        );

        let enabled_change = Bytes::from(
            serde_json::to_vec(&json!({
                "expectedRevision": state.settings_revision(),
                "processGeneration": state.process_generation(),
                "enabled": true
            }))
            .unwrap(),
        );
        let Ok(Json(updated)) = put_integration(State(state.clone()), enabled_change).await else {
            panic!("managed enabled-only update should succeed");
        };
        assert!(updated.enabled);
        assert!(updated.runtime_owned);

        let delete = Bytes::from(
            serde_json::to_vec(&json!({
                "expectedRevision": state.settings_revision(),
                "processGeneration": state.process_generation()
            }))
            .unwrap(),
        );
        assert!(
            delete_integration(State(state.clone()), delete)
                .await
                .is_err()
        );
        assert!(state.db.lock().cpa_integration().unwrap().is_some());

        drop(state);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn cpa_error_redaction_removes_every_known_secret() {
        let redacted = redact_cpa_message(
            "management-secret then inference-secret",
            &["management-secret", "inference-secret"],
        );
        assert_eq!(redacted, "[REDACTED] then [REDACTED]");
    }

    #[derive(Clone, Default)]
    struct FakeCpa {
        status: Arc<AtomicUsize>,
        delete: Arc<AtomicUsize>,
        reset: Arc<AtomicUsize>,
        oauth_start: Arc<AtomicUsize>,
        oauth_cancel: Arc<AtomicUsize>,
        fail_status: Arc<AtomicBool>,
    }

    async fn fake_accounts() -> impl IntoResponse {
        (
            [("x-cpa-version", "7.2.145")],
            Json(json!({
                "files": [{
                    "name": "claude account.json",
                    "auth_index": "claude-1",
                    "provider": "claude",
                    "disabled": false,
                    "runtime_only": false
                }]
            })),
        )
    }

    async fn fake_status(
        AxumState(fake): AxumState<FakeCpa>,
        Json(_body): Json<Value>,
    ) -> impl IntoResponse {
        fake.status.fetch_add(1, Ordering::SeqCst);
        if fake.fail_status.load(Ordering::SeqCst) {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "status failed" })),
            )
                .into_response();
        }
        Json(json!({ "status": "ok" })).into_response()
    }

    async fn fake_delete(AxumState(fake): AxumState<FakeCpa>) -> Json<Value> {
        fake.delete.fetch_add(1, Ordering::SeqCst);
        Json(json!({ "status": "ok" }))
    }

    async fn fake_reset(
        AxumState(fake): AxumState<FakeCpa>,
        Json(_body): Json<Value>,
    ) -> Json<Value> {
        fake.reset.fetch_add(1, Ordering::SeqCst);
        Json(json!({ "status": "ok" }))
    }

    async fn fake_oauth_start(AxumState(fake): AxumState<FakeCpa>) -> Json<Value> {
        fake.oauth_start.fetch_add(1, Ordering::SeqCst);
        Json(json!({
            "state": "oauth-state-1",
            "url": "https://example.com/oauth",
            "flow": "browser"
        }))
    }

    async fn fake_oauth_cancel(
        AxumState(fake): AxumState<FakeCpa>,
        Query(_query): Query<HashMap<String, String>>,
    ) -> Json<Value> {
        fake.oauth_cancel.fetch_add(1, Ordering::SeqCst);
        Json(json!({ "cancelled": true }))
    }

    async fn spawn_fake_cpa() -> (String, FakeCpa) {
        let fake = FakeCpa::default();
        let app = Router::new()
            .route(
                "/v0/management/auth-files",
                get(fake_accounts).delete(fake_delete),
            )
            .route("/v0/management/auth-files/status", patch(fake_status))
            .route("/v0/management/reset-quota", post(fake_reset))
            .route("/v0/management/codex-auth-url", get(fake_oauth_start))
            .route("/v0/management/oauth-session", delete(fake_oauth_cancel))
            .with_state(fake.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("http://{address}"), fake)
    }

    async fn configure_cpa(state: &CoreState, base_url: &str) {
        let body = Bytes::from(
            serde_json::to_vec(&json!({
                "expectedRevision": state.settings_revision(),
                "processGeneration": state.process_generation(),
                "baseUrl": base_url,
                "managementKey": "management-secret",
                "inferenceKey": "inference-secret",
                "enabled": true
            }))
            .unwrap(),
        );
        let _ = unwrap_ok(
            put_integration(State(state.clone()), body).await,
            "CPA configuration should save",
        );
    }

    fn unwrap_ok<T>(result: Result<T, V3ApiError>, what: &str) -> T {
        result.unwrap_or_else(|error| panic!("{what}: {} ({})", error.body.message, error.status))
    }

    fn mutation_bytes(state: &CoreState, extra: Value) -> Bytes {
        mutation_bytes_at(state.settings_revision(), state.process_generation(), extra)
    }

    fn mutation_bytes_at(revision: u64, generation: u64, extra: Value) -> Bytes {
        let mut body = extra;
        let object = body.as_object_mut().expect("mutation body object");
        object.insert("expectedRevision".into(), json!(revision));
        object.insert("processGeneration".into(), json!(generation));
        Bytes::from(serde_json::to_vec(&body).unwrap())
    }

    fn assert_stale_conflict(error: V3ApiError, revision: u64, generation: u64) {
        assert_eq!(error.status, StatusCode::CONFLICT);
        assert_eq!(error.body.code, ERROR_REVISION_CONFLICT);
        assert_eq!(error.body.current_revision, Some(revision));
        assert_eq!(error.body.process_generation, Some(generation));
    }

    fn assert_stale_side_effect(
        error: V3ApiError,
        revision: u64,
        generation: u64,
        counter: &AtomicUsize,
    ) {
        assert_stale_conflict(error, revision, generation);
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn successful_cpa_side_effects_bump_revision_and_reject_stale_tokens() {
        let (dir, state) = test_state("side-effect-cas");
        let (base_url, fake) = spawn_fake_cpa().await;
        configure_cpa(&state, &base_url).await;
        let generation = state.process_generation();
        let account = json!({
            "name": "claude account.json",
            "authIndex": "claude-1"
        });

        let before = state.settings_revision();
        let Json(ack) = unwrap_ok(
            set_account_status(
                State(state.clone()),
                mutation_bytes(
                    &state,
                    json!({
                        "name": "claude account.json",
                        "authIndex": "claude-1",
                        "disabled": true
                    }),
                ),
            )
            .await,
            "account status should succeed",
        );
        assert_eq!(ack.revision, before + 1);
        assert_eq!(fake.status.load(Ordering::SeqCst), 1);
        let error = set_account_status(
            State(state.clone()),
            mutation_bytes_at(
                before,
                generation,
                json!({
                    "name": "claude account.json",
                    "authIndex": "claude-1",
                    "disabled": false
                }),
            ),
        )
        .await
        .expect_err("stale account status token must 409");
        assert_stale_side_effect(error, ack.revision, generation, &fake.status);

        let before = state.settings_revision();
        let Json(ack) = unwrap_ok(
            delete_account(
                State(state.clone()),
                mutation_bytes(&state, account.clone()),
            )
            .await,
            "account delete should succeed",
        );
        assert_eq!(ack.revision, before + 1);
        assert_eq!(fake.delete.load(Ordering::SeqCst), 1);
        let error = delete_account(
            State(state.clone()),
            mutation_bytes_at(before, generation, account.clone()),
        )
        .await
        .expect_err("stale account delete token must 409");
        assert_stale_side_effect(error, ack.revision, generation, &fake.delete);

        let before = state.settings_revision();
        let Json(ack) = unwrap_ok(
            reset_quota(
                State(state.clone()),
                mutation_bytes(&state, account.clone()),
            )
            .await,
            "quota reset should succeed",
        );
        assert_eq!(ack.revision, before + 1);
        assert_eq!(fake.reset.load(Ordering::SeqCst), 1);
        let error = reset_quota(
            State(state.clone()),
            mutation_bytes_at(before, generation, account),
        )
        .await
        .expect_err("stale quota reset token must 409");
        assert_stale_side_effect(error, ack.revision, generation, &fake.reset);

        let before = state.settings_revision();
        let Json(started) = unwrap_ok(
            start_oauth(
                State(state.clone()),
                mutation_bytes(&state, json!({ "provider": "codex" })),
            )
            .await,
            "oauth start should succeed",
        );
        assert_eq!(started.revision, before + 1);
        assert_eq!(fake.oauth_start.load(Ordering::SeqCst), 1);
        let error = start_oauth(
            State(state.clone()),
            mutation_bytes_at(before, generation, json!({ "provider": "codex" })),
        )
        .await
        .expect_err("stale oauth start token must 409");
        assert_stale_side_effect(error, started.revision, generation, &fake.oauth_start);

        let before = state.settings_revision();
        let Json(ack) = unwrap_ok(
            cancel_oauth(
                State(state.clone()),
                mutation_bytes(&state, json!({ "state": "oauth-state-1" })),
            )
            .await,
            "oauth cancel should succeed",
        );
        assert_eq!(ack.revision, before + 1);
        assert_eq!(fake.oauth_cancel.load(Ordering::SeqCst), 1);
        let error = cancel_oauth(
            State(state.clone()),
            mutation_bytes_at(before, generation, json!({ "state": "oauth-state-1" })),
        )
        .await
        .expect_err("stale oauth cancel token must 409");
        assert_stale_side_effect(error, ack.revision, generation, &fake.oauth_cancel);

        drop(state);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn failed_cpa_side_effect_does_not_bump_revision() {
        let (dir, state) = test_state("failed-status-cas");
        let (base_url, fake) = spawn_fake_cpa().await;
        configure_cpa(&state, &base_url).await;
        fake.fail_status.store(true, Ordering::SeqCst);
        let before = state.settings_revision();
        let generation = state.process_generation();
        let body = mutation_bytes(
            &state,
            json!({
                "name": "claude account.json",
                "authIndex": "claude-1",
                "disabled": true
            }),
        );
        assert!(
            set_account_status(State(state.clone()), body.clone())
                .await
                .is_err()
        );
        assert_eq!(fake.status.load(Ordering::SeqCst), 1);
        assert_eq!(state.settings_revision(), before);

        fake.fail_status.store(false, Ordering::SeqCst);
        let Json(ack) = unwrap_ok(
            set_account_status(State(state.clone()), body).await,
            "retry with the same token should succeed after CPA recovers",
        );
        assert_eq!(ack.revision, before + 1);
        assert_eq!(fake.status.load(Ordering::SeqCst), 2);
        assert_eq!(ack.process_generation, generation);

        drop(state);
        std::fs::remove_dir_all(dir).unwrap();
    }

    struct CountingRuntimeHost {
        running: AtomicBool,
        starts: AtomicUsize,
        stops: AtomicUsize,
    }

    impl CpaRuntimeProcessHost for CountingRuntimeHost {
        fn start_owned(&self, _spec: &CpaRuntimeProcessSpec) -> Result<(), CpaRuntimeError> {
            self.starts.fetch_add(1, Ordering::SeqCst);
            self.running.store(true, Ordering::SeqCst);
            Ok(())
        }

        fn stop_owned(&self) -> Result<(), CpaRuntimeError> {
            self.stops.fetch_add(1, Ordering::SeqCst);
            self.running.store(false, Ordering::SeqCst);
            Ok(())
        }

        fn owned_running(&self) -> bool {
            self.running.load(Ordering::SeqCst)
        }

        fn logs(&self) -> CpaRuntimeLogTail {
            CpaRuntimeLogTail {
                stdout: String::new(),
                stderr: String::new(),
            }
        }

        fn add_log_secret(&self, _secret: &CpaRuntimeSecret) {}
    }

    #[tokio::test]
    async fn runtime_stop_bumps_revision_and_rejects_stale_start_or_stop() {
        let (dir, state) = test_state("runtime-stop-cas");
        let host = Arc::new(CountingRuntimeHost {
            running: AtomicBool::new(true),
            starts: AtomicUsize::new(0),
            stops: AtomicUsize::new(0),
        });
        state.set_cpa_runtime_host(host.clone());
        cpa_runtime::save_managed(
            &dir,
            &cpa_runtime::ManagedCpa {
                current_version: "7.2.147".into(),
                previous_version: None,
                asset_sha256: "a".repeat(64),
                port: 8317,
            },
        )
        .unwrap();

        let before = state.settings_revision();
        let generation = state.process_generation();
        let Json(started) = unwrap_ok(
            start_runtime(State(state.clone()), mutation_bytes(&state, json!({}))).await,
            "already-running start is a no-op",
        );
        assert_eq!(started.revision, before);
        assert!(started.running);
        assert_eq!(host.starts.load(Ordering::SeqCst), 0);

        let Json(stopped) = unwrap_ok(
            stop_runtime(State(state.clone()), mutation_bytes(&state, json!({}))).await,
            "runtime stop should succeed",
        );
        assert_eq!(stopped.revision, before + 1);
        assert!(!stopped.running);
        assert_eq!(host.stops.load(Ordering::SeqCst), 1);

        let error = stop_runtime(
            State(state.clone()),
            mutation_bytes_at(before, generation, json!({})),
        )
        .await
        .expect_err("stale runtime stop token must 409");
        assert_stale_conflict(error, stopped.revision, generation);
        assert_eq!(host.stops.load(Ordering::SeqCst), 1);
        assert!(!host.owned_running());

        let error = start_runtime(
            State(state.clone()),
            mutation_bytes_at(before, generation, json!({})),
        )
        .await
        .expect_err("stale runtime start token must 409");
        assert_stale_conflict(error, stopped.revision, generation);
        assert_eq!(host.starts.load(Ordering::SeqCst), 0);

        drop(state);
        std::fs::remove_dir_all(dir).unwrap();
    }
}

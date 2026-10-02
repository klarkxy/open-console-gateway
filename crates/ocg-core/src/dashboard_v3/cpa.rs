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
use crate::log_types::OperationOutcome;
use crate::models::{Account as ModelAccount, AccountSetupStep, AccountType};
use crate::provider::{
    CPA_ACCOUNT_ID, CPA_ACCOUNT_NAME, CPA_PROVIDER_ID, CredentialKind, QuotaScope,
};
use crate::state::CoreState;
use crate::user_operation::UserOperation;

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

#[cfg(all(test, windows))]
mod receipt_tests;

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
    let op = super::settings::open_dashboard(
        &state,
        "cpa.integration.update",
        "cpa",
        Some(CPA_ACCOUNT_ID.to_string()),
    );
    let result = put_integration_inner(state.clone(), body).await;
    super::settings::record_after(op, &state, &[], (Some(1), Some(1), None), None, result)
}

async fn put_integration_inner(
    state: CoreState,
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
    // The integration row is a routing destination and credential, so publish
    // the rebuilt preparation view instead of leaving the first request after
    // this write to discover the drift and re-enter the settings gate.
    state
        .publish_gateway_preparation(&state.db.lock())
        .map_err(|error| V3ApiError::internal_at(&state, error.to_string()))?;
    integration_view(&state).map(Json)
}

pub(super) async fn delete_integration(
    State(state): State<CoreState>,
    body: Bytes,
) -> Result<Json<MutationAck>, V3ApiError> {
    let op = super::settings::open_dashboard(
        &state,
        "cpa.integration.delete",
        "cpa",
        Some(CPA_ACCOUNT_ID.to_string()),
    );
    let result = delete_integration_inner(state.clone(), body).await;
    super::settings::record_after(op, &state, &[], (Some(1), Some(1), None), None, result)
}

async fn delete_integration_inner(
    state: CoreState,
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
    let op = super::settings::open_dashboard(
        &state,
        "cpa.connection.test",
        "cpa",
        Some(CPA_ACCOUNT_ID.to_string()),
    );
    let result = test_connection_inner(state.clone(), body).await;
    let ok_outcome = match &result {
        Ok(Json(report)) if report.management_ready && report.inference_ready => None,
        Ok(_) => Some((OperationOutcome::Failed, "outboundFailed")),
        Err(_) => None,
    };
    let counts = match &ok_outcome {
        Some(_) => (Some(1), Some(0), Some(1)),
        None if result.is_ok() => (Some(1), Some(1), None),
        None => (Some(1), None, Some(1)),
    };
    super::settings::record_after(op, &state, &[], counts, ok_outcome, result)
}

async fn test_connection_inner(
    state: CoreState,
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
    let op = super::settings::open_dashboard(
        &state,
        "cpa.models.refresh",
        "cpa",
        Some(CPA_ACCOUNT_ID.to_string()),
    );
    let result = refresh_models_inner(state.clone(), body).await;
    let counts = match &result {
        Ok(Json(models)) => {
            let count = super::settings::count_u32(models.models.len() as u64);
            (Some(count), Some(count), None)
        }
        Err(_) => (Some(1), None, Some(1)),
    };
    super::settings::record_after(op, &state, &["catalog"], counts, None, result)
}

async fn refresh_models_inner(
    state: CoreState,
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
    let op = super::settings::open_dashboard(
        &state,
        "cpa.account.status",
        "cpa",
        Some(CPA_ACCOUNT_ID.to_string()),
    );
    let result = set_account_status_inner(state.clone(), body).await;
    super::settings::record_after(
        op,
        &state,
        &["disabled"],
        (Some(1), Some(1), None),
        None,
        result,
    )
}

async fn set_account_status_inner(
    state: CoreState,
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
    let op = super::settings::open_dashboard(
        &state,
        "cpa.account.delete",
        "cpa",
        Some(CPA_ACCOUNT_ID.to_string()),
    );
    let result = delete_account_inner(state.clone(), body).await;
    super::settings::record_after(op, &state, &[], (Some(1), Some(1), None), None, result)
}

async fn delete_account_inner(
    state: CoreState,
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
    let op = super::settings::open_dashboard(
        &state,
        "cpa.account.reset",
        "cpa",
        Some(CPA_ACCOUNT_ID.to_string()),
    );
    let result = reset_quota_inner(state.clone(), body).await;
    super::settings::record_after(op, &state, &[], (Some(1), Some(1), None), None, result)
}

async fn reset_quota_inner(state: CoreState, body: Bytes) -> Result<Json<MutationAck>, V3ApiError> {
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
    let mut op = super::settings::open_dashboard(&state, "cpa.cli.import", "cpa", None);
    let result = import_cli_account_inner(state.clone(), headers, body, &mut op).await;
    let (counts, ok_outcome) = match &result {
        Ok(imported) if imported.outcome == CpaCliImportOutcome::Unconfirmed => (
            (Some(1), Some(0), Some(1)),
            Some((OperationOutcome::Partial, "outboundFailed")),
        ),
        Ok(_) => ((Some(1), Some(1), None), None),
        Err(_) => ((Some(1), None, Some(1)), None),
    };
    super::settings::record_after(op, &state, &[], counts, ok_outcome, result)
        .map(cli_import_response)
}

async fn import_cli_account_inner(
    state: CoreState,
    headers: HeaderMap,
    body: Bytes,
    op: &mut UserOperation,
) -> Result<CpaCliImportResult, V3ApiError> {
    require_local_cli_import(&state, &headers)?;
    let input = parse_mutation_json::<CpaCliImportRequest>(&body)?;
    op.subject(oauth_subject(input.provider));
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
) -> Result<CpaCliImportResult, V3ApiError> {
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
    Ok(CpaCliImportResult {
        provider: input.provider,
        name: credential.name.clone(),
        outcome,
        revision: state.settings_revision(),
        process_generation: state.process_generation(),
    })
}

enum OauthStartFailure {
    Api(V3ApiError),
    RolledBack { error: V3ApiError, restored: bool },
}

impl From<V3ApiError> for OauthStartFailure {
    fn from(error: V3ApiError) -> Self {
        Self::Api(error)
    }
}

pub(super) async fn start_oauth(
    State(state): State<CoreState>,
    body: Bytes,
) -> Result<Json<CpaOAuthStart>, V3ApiError> {
    let mut op = super::settings::open_dashboard(&state, "cpa.oauth.start", "cpa", None);
    match start_oauth_inner(state.clone(), body, &mut op).await {
        Ok((body, device_session)) => {
            let mut metadata =
                super::settings::metadata_for(&state, &[], Some(1), Some(0), None, None);
            if let Some(session) = device_session {
                // Accept first so a terminal session finishes this pending row.
                // The handoff is the Arc from start, not the current slot.
                op.accepted(metadata);
                session.attach(op);
            } else {
                // One-way association of this flow. The raw state, URL, and
                // token stay on the HTTP body and out of the receipt.
                metadata.related_ids = vec![oauth_flow_related_id(&body.state)];
                op.accepted(metadata);
            }
            Ok(body)
        }
        Err(OauthStartFailure::Api(error)) => super::settings::record_after(
            op,
            &state,
            &[],
            (Some(1), None, Some(1)),
            None,
            Err(error),
        ),
        Err(OauthStartFailure::RolledBack { error, restored }) => {
            let outcome = if restored {
                OperationOutcome::Compensated
            } else {
                OperationOutcome::Partial
            };
            op.complete(
                outcome,
                Some(error.operation_reason()),
                super::settings::metadata_for(&state, &[], Some(1), None, Some(1), Some(restored)),
            );
            Err(error)
        }
    }
}

async fn start_oauth_inner(
    state: CoreState,
    body: Bytes,
    op: &mut UserOperation,
) -> Result<
    (
        Json<CpaOAuthStart>,
        Option<cpa_runtime::CpaDeviceLoginSession>,
    ),
    OauthStartFailure,
> {
    let input = parse_mutation_json::<CpaOAuthStartRequest>(&body)?;
    op.subject(oauth_subject(input.provider));
    let _operation = state.cpa_operations.lock().await;
    check_before_external_write(&state, &input.expectation)?;
    let (started, device_session) = match input.method {
        CpaOAuthMethod::Browser => {
            let (client, _) = saved_client(&state)?;
            let started = client
                .start_oauth(cpa_provider(input.provider))
                .await
                .map_err(|error| map_cpa_error(&state, error))?;
            (started, None)
        }
        CpaOAuthMethod::Device => {
            if input.provider != CpaOAuthProvider::Codex {
                return Err(V3ApiError::invalid_request_at(
                    &state,
                    "Device method is only supported for managed Codex login.",
                )
                .into());
            }
            let (started, session) = state
                .start_cpa_device_oauth()
                .await
                .map_err(|error| map_runtime_error(&state, error))?;
            if let Err(error) = check_before_external_write(&state, &input.expectation) {
                let restored = state
                    .cancel_cpa_device_oauth(&started.state)
                    .is_some_and(|result| matches!(result, Ok(true)));
                drop(session);
                return Err(OauthStartFailure::RolledBack { error, restored });
            }
            (started, Some(session))
        }
    };
    let revision = state.bump_settings_revision();
    Ok((
        Json(CpaOAuthStart {
            provider: input.provider,
            state: started.state,
            url: started.url,
            flow: started.flow,
            user_code: started.user_code,
            expires_in: started.expires_in,
            revision,
            process_generation: state.process_generation(),
        }),
        device_session,
    ))
}

pub(super) async fn oauth_status(
    State(state): State<CoreState>,
    Query(query): Query<OAuthStatusQuery>,
) -> Result<Json<CpaOAuthStatus>, V3ApiError> {
    let (oauth_state, status, device) = {
        let _operation = state.cpa_operations.lock().await;
        if let Some(status) = state.cpa_device_oauth_status(&query.state) {
            let status = status.map_err(|error| map_runtime_error(&state, error))?;
            (query.state, status, true)
        } else {
            let (client, _) = saved_client(&state)?;
            let status = client
                .oauth_status(&query.state)
                .await
                .map_err(|error| map_cpa_error(&state, error))?;
            (query.state, status, false)
        }
    };
    // Finish only after `cpa_operations` and the device mutex are released.
    // A logging failure must not change this read.
    if device {
        state.finish_cpa_device_oauth_operation(&oauth_state);
    } else {
        finish_browser_oauth_operation(&state, &oauth_state, &status.status);
    }
    Ok(Json(CpaOAuthStatus {
        state: oauth_state,
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
    record_cpa(
        &state,
        "cpa.runtime.check",
        check_runtime_update_inner(state.clone(), body),
    )
    .await
}

async fn check_runtime_update_inner(
    state: CoreState,
    body: Bytes,
) -> Result<Json<CpaRuntimeCheck>, CpaMappedFailure> {
    let expectation = parse_mutation_json::<MutationExpectation>(&body)?;
    let _operation = state.cpa_operations.lock().await;
    check_before_external_write(&state, &expectation)?;
    let check = state
        .check_cpa_runtime_update(
            expectation.expected_revision,
            expectation.process_generation,
        )
        .await
        .map_err(|error| map_plain_runtime(&state, error))?;
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
    record_cpa(
        &state,
        "cpa.runtime.install",
        install_runtime_inner(state.clone(), body),
    )
    .await
}

async fn install_runtime_inner(
    state: CoreState,
    body: Bytes,
) -> Result<Json<CpaRuntime>, CpaMappedFailure> {
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
        .map_err(|failure| map_runtime_failure(&state, failure))?;
    Ok(Json(runtime_view(&state, snapshot)))
}

pub(super) async fn update_runtime(
    State(state): State<CoreState>,
    body: Bytes,
) -> Result<Json<CpaRuntime>, V3ApiError> {
    record_cpa(
        &state,
        "cpa.runtime.update",
        update_runtime_inner(state.clone(), body),
    )
    .await
}

async fn update_runtime_inner(
    state: CoreState,
    body: Bytes,
) -> Result<Json<CpaRuntime>, CpaMappedFailure> {
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
        .map_err(|failure| map_runtime_failure(&state, failure))?;
    Ok(Json(runtime_view(&state, snapshot)))
}

pub(super) async fn remove_runtime(
    State(state): State<CoreState>,
    body: Bytes,
) -> Result<Json<CpaRuntime>, V3ApiError> {
    record_cpa(
        &state,
        "cpa.runtime.remove",
        remove_runtime_inner(state.clone(), body),
    )
    .await
}

async fn remove_runtime_inner(
    state: CoreState,
    body: Bytes,
) -> Result<Json<CpaRuntime>, CpaMappedFailure> {
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
        .map_err(|failure| map_runtime_failure(&state, failure))?;
    Ok(Json(runtime_view(&state, snapshot)))
}

pub(super) async fn start_runtime(
    State(state): State<CoreState>,
    body: Bytes,
) -> Result<Json<CpaRuntime>, V3ApiError> {
    record_cpa(
        &state,
        "cpa.runtime.start",
        start_runtime_inner(state.clone(), body),
    )
    .await
}

async fn start_runtime_inner(
    state: CoreState,
    body: Bytes,
) -> Result<Json<CpaRuntime>, CpaMappedFailure> {
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
        .map_err(|failure| map_runtime_failure(&state, failure))?;
    Ok(Json(runtime_view(&state, snapshot)))
}

pub(super) async fn stop_runtime(
    State(state): State<CoreState>,
    body: Bytes,
) -> Result<Json<CpaRuntime>, V3ApiError> {
    record_cpa(
        &state,
        "cpa.runtime.stop",
        stop_runtime_inner(state.clone(), body),
    )
    .await
}

async fn stop_runtime_inner(
    state: CoreState,
    body: Bytes,
) -> Result<Json<CpaRuntime>, CpaMappedFailure> {
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
        .map_err(|failure| map_runtime_failure(&state, failure))?;
    Ok(Json(runtime_view(&state, snapshot)))
}

pub(super) async fn rollback_runtime(
    State(state): State<CoreState>,
    body: Bytes,
) -> Result<Json<CpaRuntime>, V3ApiError> {
    record_cpa(
        &state,
        "cpa.runtime.rollback",
        rollback_runtime_inner(state.clone(), body),
    )
    .await
}

async fn rollback_runtime_inner(
    state: CoreState,
    body: Bytes,
) -> Result<Json<CpaRuntime>, CpaMappedFailure> {
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
        .map_err(|failure| map_runtime_failure(&state, failure))?;
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
    let mut op = super::settings::open_dashboard(&state, "cpa.key.create", "cpa", None);
    let result = create_runtime_key_inner(state.clone(), body).await;
    if let Ok(created) = &result
        && let Some(id) = super::settings::known_subject(&created.fingerprint)
    {
        op.subject(id);
    }
    finish_cpa(op, &state, result).map(one_time_key_response)
}

async fn create_runtime_key_inner(
    state: CoreState,
    body: Bytes,
) -> Result<CpaRuntimeKeyCreated, CpaMappedFailure> {
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
        .map_err(|failure| map_runtime_failure(&state, failure))?;
    Ok(CpaRuntimeKeyCreated {
        fingerprint: created.fingerprint,
        hint: created.hint,
        secret: created.secret,
        revision: state.settings_revision(),
        process_generation: state.process_generation(),
    })
}

pub(super) async fn delete_runtime_key(
    State(state): State<CoreState>,
    AxumPath(fingerprint): AxumPath<String>,
    body: Bytes,
) -> Result<Json<MutationAck>, V3ApiError> {
    let op = super::settings::open_dashboard(
        &state,
        "cpa.key.delete",
        "cpa",
        super::settings::known_subject(&fingerprint),
    );
    let result = delete_runtime_key_inner(state.clone(), fingerprint, body).await;
    finish_cpa(op, &state, result)
}

async fn delete_runtime_key_inner(
    state: CoreState,
    fingerprint: String,
    body: Bytes,
) -> Result<Json<MutationAck>, CpaMappedFailure> {
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
        .map_err(|failure| map_runtime_failure(&state, failure))?;
    Ok(Json(current_ack(&state)))
}

pub(super) async fn rotate_runtime_key(
    State(state): State<CoreState>,
    AxumPath(fingerprint): AxumPath<String>,
    body: Bytes,
) -> Result<Response, V3ApiError> {
    let mut op = super::settings::open_dashboard(
        &state,
        "cpa.key.rotate",
        "cpa",
        super::settings::known_subject(&fingerprint),
    );
    let result = rotate_runtime_key_inner(state.clone(), fingerprint, body).await;
    if let Ok(created) = &result
        && let Some(id) = super::settings::known_subject(&created.fingerprint)
    {
        op.subject(id);
    }
    finish_cpa(op, &state, result).map(one_time_key_response)
}

async fn rotate_runtime_key_inner(
    state: CoreState,
    fingerprint: String,
    body: Bytes,
) -> Result<CpaRuntimeKeyCreated, CpaMappedFailure> {
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
        .map_err(|failure| map_runtime_failure(&state, failure))?;
    Ok(CpaRuntimeKeyCreated {
        fingerprint: created.fingerprint,
        hint: created.hint,
        secret: created.secret,
        revision: state.settings_revision(),
        process_generation: state.process_generation(),
    })
}

pub(super) async fn cancel_oauth(
    State(state): State<CoreState>,
    body: Bytes,
) -> Result<Json<MutationAck>, V3ApiError> {
    let op = super::settings::open_dashboard(
        &state,
        "cpa.oauth.cancel",
        "cpa",
        Some(CPA_ACCOUNT_ID.to_string()),
    );
    let result = cancel_oauth_inner(state.clone(), body).await;
    if let Ok((_, oauth_state, kind)) = &result {
        match kind {
            OauthCancelKind::Device => state.finish_cpa_device_oauth_operation(oauth_state),
            OauthCancelKind::Browser => {
                finish_browser_oauth_operation(&state, oauth_state, "cancelled")
            }
        }
    }
    let result = result.map(|(ack, _, _)| ack);
    super::settings::record_after(op, &state, &[], (Some(1), Some(1), None), None, result)
}

enum OauthCancelKind {
    Device,
    Browser,
}

async fn cancel_oauth_inner(
    state: CoreState,
    body: Bytes,
) -> Result<(Json<MutationAck>, String, OauthCancelKind), V3ApiError> {
    let input = parse_mutation_json::<CpaOAuthSessionDelete>(&body)?;
    let oauth_state = input.state.clone();
    let _operation = state.cpa_operations.lock().await;
    check_before_external_write(&state, &input.expectation)?;
    if let Some(cancelled) = state.cancel_cpa_device_oauth(&input.state) {
        let cancelled = cancelled.map_err(|error| map_runtime_error(&state, error))?;
        let ack = Json(if cancelled {
            committed_ack(&state)
        } else {
            current_ack(&state)
        });
        return Ok((ack, oauth_state, OauthCancelKind::Device));
    }
    let (client, _) = saved_client(&state)?;
    client
        .cancel_oauth(&input.state)
        .await
        .map_err(|error| map_cpa_error(&state, error))?;
    Ok((
        Json(committed_ack(&state)),
        oauth_state,
        OauthCancelKind::Browser,
    ))
}

pub(super) fn oauth_flow_related_id(oauth_state: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(oauth_state.as_bytes());
    let mut related = String::from("oauth:");
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for byte in digest {
        related.push(HEX[(byte >> 4) as usize] as char);
        related.push(HEX[(byte & 0xf) as usize] as char);
    }
    related
}

pub(super) fn finish_browser_oauth_operation(state: &CoreState, oauth_state: &str, status: &str) {
    let Some((outcome, reason)) = browser_oauth_terminal(status) else {
        return;
    };
    let related = oauth_flow_related_id(oauth_state);
    let pending = match state
        .db
        .lock()
        .pending_operations_by_related_id("cpa.oauth.start", &related)
    {
        Ok(rows) => rows,
        Err(error) => {
            tracing::warn!(
                action = "cpa.oauth.start",
                error = %error,
                "user operation receipt could not be recorded"
            );
            return;
        }
    };
    for row in pending {
        let operation_id = row.operation_id;
        let metadata = crate::log_types::OperationMetadata {
            changed_fields: row.metadata.changed_fields,
            requested_count: row.metadata.requested_count.or(Some(1)),
            completed_count: if outcome == OperationOutcome::Success {
                Some(1)
            } else {
                row.metadata.completed_count
            },
            failed_count: if outcome == OperationOutcome::Success {
                None
            } else {
                Some(1)
            },
            revision: Some(state.settings_revision()),
            compensated: None,
            related_ids: row.metadata.related_ids,
        };
        let finish = crate::log_types::OperationFinish {
            completed_at: Utc::now(),
            outcome,
            reason_code: reason.map(str::to_owned),
            metadata,
        };
        if let Err(error) = state.db.lock().finish_operation(&operation_id, &finish) {
            tracing::warn!(
                operation_id = %operation_id,
                action = "cpa.oauth.start",
                error = %error,
                "user operation receipt could not be recorded"
            );
        }
    }
}

fn browser_oauth_terminal(status: &str) -> Option<(OperationOutcome, Option<&'static str>)> {
    match status {
        "ok" => Some((OperationOutcome::Success, None)),
        "error" => Some((OperationOutcome::Failed, Some("outboundFailed"))),
        "expired" => Some((OperationOutcome::Failed, Some("expired"))),
        "cancelled" => Some((OperationOutcome::Rejected, Some("cancelled"))),
        _ => None,
    }
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
        desired_running: snapshot.desired_running,
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

struct CpaMappedFailure {
    api: V3ApiError,
    effect: cpa_runtime::CpaExternalEffect,
}

impl From<V3ApiError> for CpaMappedFailure {
    fn from(api: V3ApiError) -> Self {
        Self {
            api,
            effect: cpa_runtime::CpaExternalEffect::None,
        }
    }
}

fn finish_cpa<T>(
    op: UserOperation,
    state: &CoreState,
    result: Result<T, CpaMappedFailure>,
) -> Result<T, V3ApiError> {
    match result {
        Ok(value) => {
            super::settings::record_after(op, state, &[], (Some(1), Some(1), None), None, Ok(value))
        }
        Err(failure) if failure.effect == cpa_runtime::CpaExternalEffect::None => {
            super::settings::record_after(
                op,
                state,
                &[],
                (Some(1), Some(1), None),
                None,
                Err(failure.api),
            )
        }
        Err(failure) => {
            let compensated = failure.effect == cpa_runtime::CpaExternalEffect::Compensated;
            let outcome = if compensated {
                OperationOutcome::Compensated
            } else {
                OperationOutcome::Partial
            };
            op.complete(
                outcome,
                Some(failure.api.operation_reason()),
                super::settings::metadata_for(
                    state,
                    &[],
                    Some(1),
                    Some(1),
                    Some(1),
                    Some(compensated),
                ),
            );
            Err(failure.api)
        }
    }
}

async fn record_cpa<T>(
    state: &CoreState,
    action: &'static str,
    work: impl std::future::Future<Output = Result<T, CpaMappedFailure>>,
) -> Result<T, V3ApiError> {
    let op =
        super::settings::open_dashboard(state, action, "cpa", Some(CPA_ACCOUNT_ID.to_string()));
    finish_cpa(op, state, work.await)
}

fn oauth_subject(provider: CpaOAuthProvider) -> String {
    let code = match provider {
        CpaOAuthProvider::Codex => "codex",
        CpaOAuthProvider::Anthropic => "anthropic",
        CpaOAuthProvider::Antigravity => "antigravity",
        CpaOAuthProvider::Kimi => "kimi",
        CpaOAuthProvider::Xai => "xai",
    };
    code.to_string()
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

fn map_runtime_failure(
    state: &CoreState,
    failure: cpa_runtime::CpaRuntimeFailure,
) -> CpaMappedFailure {
    CpaMappedFailure {
        api: map_runtime_error(state, failure.error),
        effect: failure.effect,
    }
}

fn map_plain_runtime(state: &CoreState, error: CpaRuntimeError) -> CpaMappedFailure {
    CpaMappedFailure {
        api: map_runtime_error(state, error),
        effect: cpa_runtime::CpaExternalEffect::None,
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
    use crate::dashboard_v3::{ERROR_OUTBOUND_FAILED, ERROR_REVISION_CONFLICT};
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

    pub(super) fn test_state(label: &str) -> (std::path::PathBuf, CoreState) {
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
                desired_running: false,
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

    pub(super) fn mutation_bytes(state: &CoreState, extra: Value) -> Bytes {
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
                desired_running: false,
            },
        )
        .unwrap();

        let before = state.settings_revision();
        let generation = state.process_generation();
        let Json(started) = unwrap_ok(
            start_runtime(State(state.clone()), mutation_bytes(&state, json!({}))).await,
            "already-running start persists run intent",
        );
        assert_eq!(started.revision, before + 1);
        assert!(started.running);
        assert!(started.desired_running);
        assert_eq!(host.starts.load(Ordering::SeqCst), 0);

        let Json(stopped) = unwrap_ok(
            stop_runtime(State(state.clone()), mutation_bytes(&state, json!({}))).await,
            "runtime stop should succeed",
        );
        assert_eq!(stopped.revision, before + 2);
        assert!(!stopped.running);
        assert!(!stopped.desired_running);
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

    #[derive(Clone)]
    struct KeyApi {
        state: CoreState,
        keys: Arc<std::sync::Mutex<Vec<String>>>,
        puts: Arc<std::sync::Mutex<Vec<Vec<String>>>>,
        authorizations: Arc<std::sync::Mutex<Vec<String>>>,
        restore_fails: bool,
        bump_on_first_put: bool,
    }

    async fn spawn_key_api(api: KeyApi) -> u16 {
        async fn list(AxumState(api): AxumState<KeyApi>) -> Json<Value> {
            Json(json!(api.keys.lock().unwrap().clone()))
        }
        async fn put_keys(
            AxumState(api): AxumState<KeyApi>,
            headers: HeaderMap,
            Json(body): Json<Value>,
        ) -> Response {
            if let Some(value) = headers
                .get(header::AUTHORIZATION)
                .and_then(|v| v.to_str().ok())
            {
                api.authorizations.lock().unwrap().push(value.to_string());
            }
            let incoming = body
                .as_array()
                .expect("CPA key replace body is an array")
                .iter()
                .map(|value| value.as_str().expect("key").to_string())
                .collect::<Vec<_>>();
            let attempt = {
                let mut puts = api.puts.lock().unwrap();
                puts.push(incoming.clone());
                puts.len()
            };
            if attempt == 1 {
                *api.keys.lock().unwrap() = incoming;
                if api.bump_on_first_put {
                    api.state.bump_settings_revision();
                }
                return Json(json!([])).into_response();
            }
            if api.restore_fails {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"error": "restore failed"})),
                )
                    .into_response();
            }
            *api.keys.lock().unwrap() = incoming;
            Json(json!([])).into_response()
        }

        let app = Router::new()
            .route("/v0/management/api-keys", get(list).put(put_keys))
            .with_state(api);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        port
    }

    fn key_receipts(state: &CoreState) -> Vec<(String, Option<String>, String)> {
        let db = state.db.lock();
        let mut statement = db
            .conn
            .prepare(
                "SELECT outcome, reason_code, metadata_json
                 FROM operation_logs WHERE action = 'cpa.key.create' ORDER BY rowid",
            )
            .unwrap();
        statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    }

    async fn external_key_replace_then_cas_advance(restore_fails: bool) {
        let (dir, state) = test_state(if restore_fails {
            "key-partial"
        } else {
            "key-compensated"
        });
        let configure = Bytes::from(
            serde_json::to_vec(&json!({
                "expectedRevision": state.settings_revision(),
                "processGeneration": state.process_generation(),
                "managementKey": "management-secret",
                "inferenceKey": "inference-secret",
                "enabled": true
            }))
            .unwrap(),
        );
        let _ = unwrap_ok(
            put_integration(State(state.clone()), configure).await,
            "CPA configuration should save",
        );
        let api = KeyApi {
            state: state.clone(),
            keys: Arc::new(std::sync::Mutex::new(vec!["inference-secret".into()])),
            puts: Arc::new(std::sync::Mutex::new(Vec::new())),
            authorizations: Arc::new(std::sync::Mutex::new(Vec::new())),
            restore_fails,
            bump_on_first_put: true,
        };
        let port = spawn_key_api(api.clone()).await;
        let root = cpa_runtime::runtime_dir(&dir);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("config.yaml"),
            "api-keys:\n  - \"inference-secret\"\n",
        )
        .unwrap();
        cpa_runtime::save_managed(
            &dir,
            &cpa_runtime::ManagedCpa {
                current_version: "7.2.147".into(),
                previous_version: None,
                asset_sha256: "a".repeat(64),
                port,
                desired_running: true,
            },
        )
        .unwrap();
        state.set_cpa_runtime_host(Arc::new(CountingRuntimeHost {
            running: AtomicBool::new(true),
            starts: AtomicUsize::new(0),
            stops: AtomicUsize::new(0),
        }));

        let revision = state.settings_revision();
        let error = create_runtime_key(State(state.clone()), mutation_bytes(&state, json!({})))
            .await
            .expect_err("CAS after the external key replace must fail");
        assert_eq!(state.settings_revision(), revision + 1);
        let puts = api.puts.lock().unwrap().clone();
        assert_eq!(puts.len(), 2, "replace then restore");
        assert!(puts[0].iter().any(|key| key == "inference-secret"));
        assert!(puts[0].iter().any(|key| key.starts_with("cpa-")));
        assert_eq!(puts[1], vec!["inference-secret".to_string()]);
        assert!(
            api.authorizations
                .lock()
                .unwrap()
                .iter()
                .any(|value| value == "Bearer management-secret")
        );
        let rows = key_receipts(&state);
        assert_eq!(rows.len(), 1);
        let metadata = &rows[0].2;
        assert!(!metadata.contains("inference-secret"));
        assert!(!metadata.contains("management-secret"));
        assert!(!metadata.contains("cpa-"));
        assert!(metadata.contains("\"requestedCount\":1"));
        assert!(metadata.contains("\"completedCount\":1"));
        assert!(metadata.contains("\"failedCount\":1"));
        if restore_fails {
            assert_eq!(error.status, StatusCode::BAD_GATEWAY);
            assert_eq!(error.body.code, ERROR_OUTBOUND_FAILED);
            assert!(error.body.message.contains("revisionConflict"));
            assert!(
                error
                    .body
                    .message
                    .contains("restoring CPA client keys also failed")
            );
            assert!(!error.body.message.contains("inference-secret"));
            assert_eq!(rows[0].0, "partial");
            assert_eq!(rows[0].1.as_deref(), Some("outboundFailed"));
            assert!(metadata.contains("\"compensated\":false"));
            assert_eq!(
                api.keys.lock().unwrap().clone(),
                puts[0],
                "failed restore leaves the replaced keys"
            );
        } else {
            assert_eq!(error.status, StatusCode::CONFLICT);
            assert_eq!(error.body.code, ERROR_REVISION_CONFLICT);
            assert_eq!(
                error.body.message,
                "settings changed since they were loaded; reload and try again"
            );
            assert_eq!(error.body.current_revision, Some(revision + 1));
            assert_eq!(rows[0].0, "compensated");
            assert_eq!(rows[0].1.as_deref(), Some("revisionConflict"));
            assert!(metadata.contains("\"compensated\":true"));
            assert_eq!(
                api.keys.lock().unwrap().clone(),
                vec!["inference-secret".to_string()]
            );
        }
        drop(api);
        drop(state);
        std::fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn external_key_replace_restored_after_cas_advance_is_compensated() {
        external_key_replace_then_cas_advance(false).await;
    }

    #[tokio::test]
    async fn external_key_replace_restore_failure_after_cas_advance_is_partial() {
        external_key_replace_then_cas_advance(true).await;
    }

    pub(super) fn action_receipts(
        state: &CoreState,
        action: &str,
    ) -> Vec<(String, Option<String>, String)> {
        let db = state.db.lock();
        let mut statement = db
            .conn
            .prepare(
                "SELECT outcome, reason_code, metadata_json
                 FROM operation_logs WHERE action = ?1 ORDER BY rowid",
            )
            .unwrap();
        statement
            .query_map([action], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    }

    /// Lets the removal read `managed.json` and makes the later delete fail.
    #[cfg(windows)]
    pub(super) fn hold_readable_no_delete(path: &std::path::Path) -> std::fs::File {
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            const FILE_SHARE_READ: u32 = 1;
            const FILE_SHARE_WRITE: u32 = 2;
            options.share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE);
        }
        options.open(path).unwrap()
    }

    #[tokio::test]
    #[cfg(windows)]
    async fn late_removal_keeps_the_original_http_error_and_partial_receipt() {
        let (dir, state) = test_state("remove-partial-http");
        let root = cpa_runtime::runtime_dir(&dir);
        std::fs::create_dir_all(root.join("auth")).unwrap();
        std::fs::create_dir_all(root.join("versions")).unwrap();
        std::fs::write(
            root.join("config.yaml"),
            b"api-keys:\n  - \"inference-key\"\n",
        )
        .unwrap();
        cpa_runtime::save_managed(
            &dir,
            &cpa_runtime::ManagedCpa {
                current_version: "7.2.147".into(),
                previous_version: None,
                asset_sha256: "a".repeat(64),
                port: 8317,
                desired_running: false,
            },
        )
        .unwrap();
        state
            .persist_managed_connection(
                8317,
                "management-key",
                "inference-key",
                vec!["model".into()],
            )
            .unwrap();
        state.set_cpa_runtime_host(Arc::new(CountingRuntimeHost {
            running: AtomicBool::new(false),
            starts: AtomicUsize::new(0),
            stops: AtomicUsize::new(0),
        }));
        let managed = cpa_runtime::managed_path(&dir);
        let _held = hold_readable_no_delete(&managed);

        let error = remove_runtime(State(state.clone()), mutation_bytes(&state, json!({})))
            .await
            .expect_err("manifest delete must fail after the owned files are gone");
        assert_eq!(error.status, StatusCode::BAD_GATEWAY);
        assert_eq!(error.body.code, ERROR_OUTBOUND_FAILED);
        assert!(error.body.message.contains("CPA runtime file error"));
        assert!(
            !error
                .body
                .message
                .contains("restoring the previous CPA runtime also failed")
        );
        let rows = action_receipts(&state, "cpa.runtime.remove");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0, "partial");
        assert_eq!(rows[0].1.as_deref(), Some("outboundFailed"));
        assert!(rows[0].2.contains("\"compensated\":false"));
        assert!(!root.join("config.yaml").exists());
        assert!(managed.is_file());
        assert!(state.db.lock().cpa_integration().unwrap().is_some());
        drop(_held);
        drop(state);
        std::fs::remove_dir_all(dir).ok();
    }

    struct IntentStopHost {
        running: AtomicBool,
    }

    impl CpaRuntimeProcessHost for IntentStopHost {
        fn start_owned(&self, _spec: &CpaRuntimeProcessSpec) -> Result<(), CpaRuntimeError> {
            self.running.store(true, Ordering::SeqCst);
            Ok(())
        }

        fn stop_owned(&self) -> Result<(), CpaRuntimeError> {
            Err(CpaRuntimeError::Failed(
                "owned CPA child refused to stop".into(),
            ))
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
    async fn saved_stop_intent_and_host_failure_is_partial() {
        let (dir, state) = test_state("stop-intent-partial");
        cpa_runtime::save_managed(
            &dir,
            &cpa_runtime::ManagedCpa {
                current_version: "7.2.147".into(),
                previous_version: None,
                asset_sha256: "a".repeat(64),
                port: 8317,
                desired_running: true,
            },
        )
        .unwrap();
        state.set_cpa_runtime_host(Arc::new(IntentStopHost {
            running: AtomicBool::new(true),
        }));

        let error = stop_runtime(State(state.clone()), mutation_bytes(&state, json!({})))
            .await
            .expect_err("host stop must surface after the intent is saved");
        assert_eq!(error.status, StatusCode::BAD_GATEWAY);
        assert_eq!(error.body.code, ERROR_OUTBOUND_FAILED);
        assert_eq!(error.body.message, "owned CPA child refused to stop");
        let rows = action_receipts(&state, "cpa.runtime.stop");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0, "partial");
        assert_eq!(rows[0].1.as_deref(), Some("outboundFailed"));
        assert!(rows[0].2.contains("\"compensated\":false"));
        assert!(
            !cpa_runtime::load_managed(&dir)
                .unwrap()
                .unwrap()
                .desired_running
        );
        drop(state);
        std::fs::remove_dir_all(dir).unwrap();
    }

    struct DashboardProbeHost {
        running: AtomicBool,
        fail_stop: AtomicBool,
        port: u16,
    }

    impl CpaRuntimeProcessHost for DashboardProbeHost {
        fn start_owned(&self, _spec: &CpaRuntimeProcessSpec) -> Result<(), CpaRuntimeError> {
            use axum::routing::get;
            use serde_json::json as sjson;
            let port = self.port;
            let (ready_tx, ready_rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();
                runtime.block_on(async move {
                    let app = Router::new()
                        .route("/healthz", get(|| async { Json(sjson!({"status": "ok"})) }))
                        .route(
                            "/v0/management/auth-files",
                            get(|| async {
                                ([("x-cpa-version", "7.2.147")], Json(sjson!({"files": []})))
                            }),
                        )
                        .route(
                            "/v1/models",
                            get(|| async { Json(sjson!({"data": [{"id": "model"}]})) }),
                        );
                    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
                        .await
                        .unwrap();
                    let _ = ready_tx.send(());
                    let _ = axum::serve(listener, app).await;
                });
            });
            ready_rx.recv().unwrap();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
            while std::net::TcpStream::connect(("127.0.0.1", port)).is_err() {
                if std::time::Instant::now() >= deadline {
                    panic!("probe host did not listen");
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            self.running.store(true, Ordering::SeqCst);
            Ok(())
        }

        fn stop_owned(&self) -> Result<(), CpaRuntimeError> {
            if self.fail_stop.load(Ordering::SeqCst) {
                return Err(CpaRuntimeError::Failed(
                    "owned CPA child refused to stop".into(),
                ));
            }
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

    async fn start_cas_receipt(stop_fails: bool) {
        let port = {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            listener.local_addr().unwrap().port()
        };
        let (dir, state) = test_state(if stop_fails {
            "start-cas-partial"
        } else {
            "start-cas-compensated"
        });
        let root = cpa_runtime::runtime_dir(&dir);
        let version_dir = root.join("versions").join("7.2.147");
        std::fs::create_dir_all(&version_dir).unwrap();
        std::fs::write(version_dir.join("cli-proxy-api.exe"), b"mz").unwrap();
        std::fs::write(
            root.join("config.yaml"),
            format!(
                "host: \"127.0.0.1\"\nport: {port}\nauth-dir: \"auth\"\ndebug: false\napi-keys:\n  - \"inference-key\"\n"
            ),
        )
        .unwrap();
        cpa_runtime::save_managed(
            &dir,
            &cpa_runtime::ManagedCpa {
                current_version: "7.2.147".into(),
                previous_version: None,
                asset_sha256: "a".repeat(64),
                port,
                desired_running: false,
            },
        )
        .unwrap();
        state
            .persist_managed_connection(
                port,
                "management-key",
                "inference-key",
                vec!["model".into()],
            )
            .unwrap();
        let host = Arc::new(DashboardProbeHost {
            running: AtomicBool::new(false),
            fail_stop: AtomicBool::new(stop_fails),
            port,
        });
        state.set_cpa_runtime_host(host.clone());
        let arrived = Arc::new(std::sync::Barrier::new(2));
        let release = Arc::new(std::sync::Barrier::new(2));
        let pause_arrived = arrived.clone();
        let pause_release = release.clone();
        state
            .cpa_runtime
            .set_before_manual_start_commit_pause(move || {
                pause_arrived.wait();
                pause_release.wait();
            });
        let bumper = state.clone();
        let bump_arrived = arrived.clone();
        let bump_release = release.clone();
        let bumper_thread = std::thread::spawn(move || {
            bump_arrived.wait();
            bumper.bump_settings_revision();
            bump_release.wait();
        });
        let revision = state.settings_revision();
        let body = mutation_bytes(&state, json!({}));
        let error = start_runtime(State(state.clone()), body)
            .await
            .expect_err("CAS after launch must fail");
        bumper_thread.join().unwrap();
        assert_eq!(error.status, StatusCode::CONFLICT);
        assert_eq!(error.body.code, ERROR_REVISION_CONFLICT);
        assert_eq!(
            error.body.message,
            "settings changed since they were loaded; reload and try again"
        );
        assert_eq!(error.body.current_revision, Some(revision + 1));
        let rows = action_receipts(&state, "cpa.runtime.start");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].1.as_deref(), Some("revisionConflict"));
        assert!(
            !cpa_runtime::load_managed(&dir)
                .unwrap()
                .unwrap()
                .desired_running
        );
        if stop_fails {
            assert_eq!(rows[0].0, "partial");
            assert!(rows[0].2.contains("\"compensated\":false"));
            assert!(host.owned_running());
        } else {
            assert_eq!(rows[0].0, "compensated");
            assert!(rows[0].2.contains("\"compensated\":true"));
            assert!(!host.owned_running());
        }
        drop(host);
        drop(state);
        std::fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn start_cas_failure_stop_success_receipt_is_compensated() {
        start_cas_receipt(false).await;
    }

    #[tokio::test]
    async fn start_cas_failure_stop_failure_receipt_is_partial() {
        start_cas_receipt(true).await;
    }

    #[tokio::test]
    async fn yaml_restore_failure_after_upstream_restore_is_partial() {
        let (dir, state) = test_state("key-yaml-partial");
        let configure = Bytes::from(
            serde_json::to_vec(&json!({
                "expectedRevision": state.settings_revision(),
                "processGeneration": state.process_generation(),
                "managementKey": "management-secret",
                "inferenceKey": "inference-secret",
                "enabled": true
            }))
            .unwrap(),
        );
        let _ = unwrap_ok(
            put_integration(State(state.clone()), configure).await,
            "CPA configuration should save",
        );
        let api = KeyApi {
            state: state.clone(),
            keys: Arc::new(std::sync::Mutex::new(vec!["inference-secret".into()])),
            puts: Arc::new(std::sync::Mutex::new(Vec::new())),
            authorizations: Arc::new(std::sync::Mutex::new(Vec::new())),
            restore_fails: false,
            bump_on_first_put: false,
        };
        let port = spawn_key_api(api.clone()).await;
        let root = cpa_runtime::runtime_dir(&dir);
        std::fs::create_dir_all(&root).unwrap();
        let config = root.join("config.yaml");
        std::fs::write(&config, "api-keys:\n  - \"inference-secret\"\n").unwrap();
        let original = std::fs::read(&config).unwrap();
        std::fs::create_dir(root.join("config.yaml.previous")).unwrap();
        cpa_runtime::save_managed(
            &dir,
            &cpa_runtime::ManagedCpa {
                current_version: "7.2.147".into(),
                previous_version: None,
                asset_sha256: "a".repeat(64),
                port,
                desired_running: true,
            },
        )
        .unwrap();
        state.set_cpa_runtime_host(Arc::new(CountingRuntimeHost {
            running: AtomicBool::new(true),
            starts: AtomicUsize::new(0),
            stops: AtomicUsize::new(0),
        }));
        let _fault = cpa_runtime::FailAtomicWrites::arm(&config, 1, 2);

        let revision = state.settings_revision();
        let error = create_runtime_key(State(state.clone()), mutation_bytes(&state, json!({})))
            .await
            .expect_err("previous config restore must fail closed");
        assert_eq!(error.status, StatusCode::BAD_GATEWAY);
        assert_eq!(error.body.code, ERROR_OUTBOUND_FAILED);
        assert!(error.body.message.contains("CPA runtime file error"));
        assert!(
            !error
                .body
                .message
                .contains("restoring CPA client keys also failed")
        );
        assert!(!error.body.message.contains("inference-secret"));
        assert_eq!(state.settings_revision(), revision);
        assert_eq!(api.puts.lock().unwrap().len(), 2);
        assert_eq!(
            api.keys.lock().unwrap().clone(),
            vec!["inference-secret".to_string()]
        );
        assert_ne!(std::fs::read(&config).unwrap(), original);
        let rows = action_receipts(&state, "cpa.key.create");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0, "partial");
        assert_eq!(rows[0].1.as_deref(), Some("outboundFailed"));
        assert!(rows[0].2.contains("\"compensated\":false"));
        assert!(rows[0].2.contains("\"failedCount\":1"));
        drop(api);
        drop(state);
        std::fs::remove_dir_all(dir).ok();
    }
}

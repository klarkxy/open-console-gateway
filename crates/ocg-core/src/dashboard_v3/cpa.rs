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
use crate::provider::CPA_ACCOUNT_ID;
use crate::state::CoreState;

use super::types::{
    CpaAccount, CpaAccountDelete, CpaAccountStatusUpdate, CpaAccounts, CpaCliImportOutcome,
    CpaCliImportRequest, CpaCliImportResult, CpaCliImportSource, CpaCliImports,
    CpaConnectionReport, CpaControlTarget, CpaIntegration, CpaIntegrationUpdate, CpaModel,
    CpaModels, CpaOAuthMethod, CpaOAuthProvider, CpaOAuthSessionDelete, CpaOAuthStart,
    CpaOAuthStartRequest, CpaOAuthStatus, CpaQuotaReset, CpaRuntime, CpaRuntimeCheck,
    CpaRuntimeInstall, CpaRuntimeKeys, CpaRuntimeLogs, CpaRuntimePhase, CpaTestRequest,
    MutationAck, MutationExpectation,
};
use super::{V3ApiError, check_expectation, parse_json, parse_mutation_json};

const REMOTE_MIGRATION: &str =
    "Stored remote CPA configuration requires explicit migration and was left unchanged";
const REMOTE_NOT_TARGET: &str = "Remote CPA is not a target in this product";
const DEVICE_POLL_BUSY: &str = "CPA device login completion is already in progress.";

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct OAuthStatusQuery {
    state: String,
    #[serde(default)]
    target: Option<CpaControlTarget>,
}

#[derive(Debug, Default, Deserialize)]
pub(super) struct CpaTargetQuery {
    #[serde(default)]
    target: Option<CpaControlTarget>,
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
    let remote_material =
        input.base_url.is_some() || input.management_key.is_some() || input.inference_key.is_some();
    resolve_owned_control(&state, input.target, remote_material)?;
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
    Err(V3ApiError::invalid_request_at(
        &state,
        remote_target_message(&state)?,
    ))
}

pub(super) async fn test_connection(
    State(state): State<CoreState>,
    body: Bytes,
) -> Result<Json<CpaConnectionReport>, V3ApiError> {
    let input = parse_json::<CpaTestRequest>(&body)?;
    let _operation = state.cpa_operations.lock().await;
    let remote_material =
        input.base_url.is_some() || input.management_key.is_some() || input.inference_key.is_some();
    resolve_owned_control(&state, input.target, remote_material)?;
    Ok(Json(owned_connection_report(&state)))
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
    Query(target): Query<CpaTargetQuery>,
    body: Bytes,
) -> Result<Json<CpaModels>, V3ApiError> {
    let expectation = parse_mutation_json::<MutationExpectation>(&body)?;
    resolve_owned_control(&state, target.target, false)?;
    refresh_owned_native(&state, &expectation).await.map(Json)
}

pub(super) async fn list_accounts(
    State(state): State<CoreState>,
    Query(target): Query<CpaTargetQuery>,
) -> Result<Json<CpaAccounts>, V3ApiError> {
    let _operation = state.cpa_operations.lock().await;
    let client = control_client(&state, target.target)?;
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
    Query(target): Query<CpaTargetQuery>,
    body: Bytes,
) -> Result<Json<MutationAck>, V3ApiError> {
    let input = parse_mutation_json::<CpaAccountStatusUpdate>(&body)?;
    resolve_owned_control(&state, target.target, false)?;
    let _operation = state.cpa_operations.lock().await;
    check_before_external_write(&state, &input.expectation)?;
    let bound = {
        let db = state.db.lock();
        crate::cpa_execution::sync_owned_enabled(
            &db.conn,
            &input.name,
            &input.auth_index,
            !input.disabled,
        )
        .map_err(|error| map_execution_error(&state, error))?
    };
    if !bound {
        return Err(V3ApiError::invalid_request_at(
            &state,
            "owned native account is not bound",
        ));
    }
    let client = owned_control_client(&state)?;
    client
        .set_account_disabled(&input.name, &input.auth_index, input.disabled)
        .await
        .map_err(|error| map_cpa_error(&state, error))?;
    Ok(Json(committed_ack(&state)))
}

pub(super) async fn delete_account(
    State(state): State<CoreState>,
    Query(target): Query<CpaTargetQuery>,
    body: Bytes,
) -> Result<Json<MutationAck>, V3ApiError> {
    let input = parse_mutation_json::<CpaAccountDelete>(&body)?;
    resolve_owned_control(&state, target.target, false)?;
    let _operation = state.cpa_operations.lock().await;
    check_before_external_write(&state, &input.expectation)?;
    let name = input.name.clone();
    let auth_index = input.auth_index.clone();
    let rpc_name = name.clone();
    let rpc_index = auth_index.clone();
    owned_fenced_rpc(&state, &name, &auth_index, move |client| async move {
        client
            .delete_account(&rpc_name, &rpc_index)
            .await
            .map(|_| ())
    })
    .await?;
    Ok(Json(committed_ack(&state)))
}

pub(super) async fn reset_quota(
    State(state): State<CoreState>,
    Query(target): Query<CpaTargetQuery>,
    body: Bytes,
) -> Result<Json<MutationAck>, V3ApiError> {
    let input = parse_mutation_json::<CpaQuotaReset>(&body)?;
    resolve_owned_control(&state, target.target, false)?;
    let _operation = state.cpa_operations.lock().await;
    check_before_external_write(&state, &input.expectation)?;
    let name = input.name.clone();
    let auth_index = input.auth_index.clone();
    let rpc_name = name.clone();
    let rpc_index = auth_index.clone();
    owned_fenced_rpc(&state, &name, &auth_index, move |client| async move {
        client.reset_quota(&rpc_name, &rpc_index).await
    })
    .await?;
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
    Query(target): Query<CpaTargetQuery>,
    body: Bytes,
) -> Result<Response, V3ApiError> {
    require_local_cli_import(&state, &headers)?;
    let input = parse_mutation_json::<CpaCliImportRequest>(&body)?;
    resolve_owned_control(&state, target.target, false)?;
    let roots =
        CliRoots::from_env().map_err(|message| V3ApiError::invalid_request_at(&state, message))?;
    import_owned_from_roots(&state, input, roots).await
}

async fn import_owned_from_roots(
    state: &CoreState,
    input: CpaCliImportRequest,
    roots: CliRoots,
) -> Result<Response, V3ApiError> {
    let credential = roots
        .read(cpa_provider(input.provider))
        .map_err(|message| V3ApiError::invalid_request_at(state, message))?;
    {
        let db = state.db.lock();
        match crate::cpa_execution::fence_mapped_target(&db.conn, &credential.name, "") {
            Ok(_) => {}
            Err(crate::cpa_execution::ExecutionError::Invalid(message))
                if message == "owned native account is not bound" => {}
            Err(error) => return Err(map_execution_error(state, error)),
        }
    }
    let response = {
        let _operation = state.cpa_operations.lock().await;
        check_before_external_write(state, &input.expectation)?;
        let client = owned_control_client(state)?;
        import_cli_credential(state, &client, input, credential).await?
    };
    reconcile_after_import(state).await;
    Ok(response)
}

async fn reconcile_after_import(state: &CoreState) {
    let Ok(ready) = crate::cpa_execution::fetch_owned_ready_once(state).await else {
        return;
    };
    let lease = {
        let db = state.db.lock();
        let Ok(generation) = crate::cpa_execution::persisted_child_generation(&db.conn) else {
            return;
        };
        match crate::cpa_execution::capture_native_lease(
            &db.conn,
            generation,
            state.settings_revision(),
        ) {
            Ok(lease) => lease,
            Err(_) => return,
        }
    };
    let snapshot = crate::cpa_execution::discovery_from_ready_body(&ready);
    let report = {
        let db = state.db.lock();
        if state.settings_revision() != lease.settings_revision {
            return;
        }
        match crate::cpa_execution::reconcile_owned_discovery(&db.conn, &snapshot, &lease) {
            Ok(report) => report,
            Err(_) => return,
        }
    };
    if report.changed {
        state.bump_settings_revision();
    }
    if report.needs_apply {
        let _ = crate::cpa_execution::schedule_owned_apply(state).await;
    }
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
    Query(target): Query<CpaTargetQuery>,
    body: Bytes,
) -> Result<Json<CpaOAuthStart>, V3ApiError> {
    let input = parse_mutation_json::<CpaOAuthStartRequest>(&body)?;
    let _operation = state.cpa_operations.lock().await;
    check_before_external_write(&state, &input.expectation)?;
    let (started, device) = match input.method {
        CpaOAuthMethod::Browser => {
            let client = control_client(&state, target.target)?;
            let started = client
                .start_oauth(cpa_provider(input.provider))
                .await
                .map_err(|error| map_cpa_error(&state, error))?;
            (started, false)
        }
        CpaOAuthMethod::Device => {
            if input.provider != CpaOAuthProvider::Codex {
                return Err(V3ApiError::invalid_request_at(
                    &state,
                    "Device method is only supported for managed Codex login.",
                ));
            }
            resolve_owned_control(&state, target.target, false)?;
            let started = state
                .start_cpa_device_oauth()
                .await
                .map_err(|error| map_runtime_error(&state, error))?;
            if let Err(error) = check_before_external_write(&state, &input.expectation) {
                let _ = state.cancel_cpa_device_oauth(&started.state);
                return Err(error);
            }
            (started, true)
        }
    };
    let revision = if device {
        let observed = state.settings_revision();
        if state.owned_device_captured_revision(&started.state) != Some(observed)
            || observed != input.expectation.expected_revision
        {
            let _ = state.cancel_cpa_device_oauth(&started.state);
            return Err(V3ApiError::revision_conflict(&state));
        }
        let revision = state.bump_settings_revision();
        if revision != observed.wrapping_add(1)
            || !state.note_owned_device_settings(&started.state, observed, revision)
        {
            let _ = state.cancel_cpa_device_oauth(&started.state);
            return Err(V3ApiError::revision_conflict(&state));
        }
        revision
    } else {
        state.bump_settings_revision()
    };
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
    // Session fence, acquired before status validation or fetch. This is not
    // cpa_operations: schedule_owned_device_apply holds that lock through apply.
    let gate = state
        .try_begin_device_poll(&query.state)
        .map_err(|error| map_runtime_error(&state, error))?;
    match gate {
        crate::cpa_execution::device::DevicePollGate::NotDevice => {}
        crate::cpa_execution::device::DevicePollGate::Busy(status) if status.status != "ok" => {
            return Ok(Json(device_status_body(&state, &query.state, status)));
        }
        crate::cpa_execution::device::DevicePollGate::Busy(_) => {
            return Err(map_runtime_error(
                &state,
                CpaRuntimeError::Conflict(DEVICE_POLL_BUSY.to_string()),
            ));
        }
        crate::cpa_execution::device::DevicePollGate::Ready(guard) => {
            let status = guard.owner_status(&state);
            if status.status != "ok" {
                return Ok(Json(device_status_body(&state, &query.state, status)));
            }
            let (status, error) = reconcile_device_completion(&state, &query.state, &guard).await?;
            return Ok(Json(CpaOAuthStatus {
                state: query.state,
                status,
                error,
                revision: state.settings_revision(),
                process_generation: state.process_generation(),
            }));
        }
    }
    let _operation = state.cpa_operations.lock().await;
    let client = control_client(&state, query.target)?;
    let status = client
        .oauth_status(&query.state)
        .await
        .map_err(|error| map_cpa_error(&state, error))?;
    Ok(Json(CpaOAuthStatus {
        state: query.state,
        status: status.status,
        error: status.error,
        revision: state.settings_revision(),
        process_generation: state.process_generation(),
    }))
}

fn device_status_body(
    state: &CoreState,
    oauth_state: &str,
    status: crate::cpa::CpaOAuthStatus,
) -> CpaOAuthStatus {
    CpaOAuthStatus {
        state: oauth_state.to_string(),
        status: status.status,
        error: status.error,
        revision: state.settings_revision(),
        process_generation: state.process_generation(),
    }
}

fn stale_captured_completion(
    state: &CoreState,
    oauth_state: &str,
    captured: &crate::cpa_execution::device::DeviceCompletion,
) -> Result<(String, Option<String>), V3ApiError> {
    let winner_current = state
        .owned_device_completion(oauth_state)
        .is_some_and(|live| {
            live != *captured
                && crate::cpa_execution::device::completion_current(state, &live).is_ok()
        });
    if winner_current {
        return Ok((
            "error".into(),
            Some("CPA device login completion is no longer current.".into()),
        ));
    }
    let status = state
        .device_owner_oauth_status(oauth_state)
        .ok_or_else(|| {
            V3ApiError::invalid_request_at(state, "Unknown or replaced CPA device login session.")
        })?
        .map_err(|error| map_runtime_error(state, error))?;
    Ok((status.status, status.error))
}

async fn reconcile_device_completion(
    state: &CoreState,
    oauth_state: &str,
    guard: &crate::cpa_execution::device::DeviceOperationGuard,
) -> Result<(String, Option<String>), V3ApiError> {
    // This value authorizes the ready body. A later session lease must not replace it.
    let Some(captured) = state.owned_device_completion(oauth_state) else {
        return Ok((
            "error".into(),
            Some("owned CPA ready identity is incomplete".into()),
        ));
    };
    if crate::cpa_execution::device::completion_current(state, &captured).is_err() {
        return stale_captured_completion(state, oauth_state, &captured);
    }
    // Ready IO is the only section a newer poll may preempt. Cancel during
    // that wait wins over a transport error from the same fetch.
    guard.begin_ready_fetch();
    let fetched = crate::cpa_execution::fetch_owned_ready_once(state).await;
    guard.end_ready_fetch();
    if guard.cancelled() {
        return Ok((
            "error".into(),
            Some("CPA device login completion is no longer current.".into()),
        ));
    }
    let ready = match fetched {
        Ok(body) => body,
        Err(error) => return Ok(("error".into(), Some(error.to_string()))),
    };
    // Captured-lease and cancel checks run inside cpa_operations below.
    // A cancel ack bumps settings; calling the stale fallback first would
    // force_stale the still-ok terminal before the fence is consulted.
    let snapshot = crate::cpa_execution::discovery_from_ready_body(&ready);
    if !matches!(
        snapshot,
        crate::cpa_execution::DiscoverySnapshot::Complete(_)
    ) {
        return Ok((
            "error".into(),
            Some("owned CPA ready identity is incomplete".into()),
        ));
    }
    // Ready IO is finished. cancel_oauth holds this mutex across CAS and cancel.
    // Reconcile and this poll's own revision/lease note stay inside it. The
    // database lock is dropped before the test pause. Drop this mutex before
    // schedule_owned_device_apply, which locks it again and rechecks this
    // guard and the noted completion before apply.
    let report = {
        let _cancel_window = state.cpa_operations.lock().await;
        let cancelled = guard.cancelled();
        let captured_current =
            crate::cpa_execution::device::completion_current(state, &captured).is_ok();
        if cancelled {
            return Ok((
                "error".into(),
                Some("CPA device login completion is no longer current.".into()),
            ));
        }
        if !captured_current {
            return stale_captured_completion(state, oauth_state, &captured);
        }
        let report = {
            let db = state.db.lock();
            match crate::cpa_execution::reconcile_owned_discovery(
                &db.conn,
                &snapshot,
                &captured.lease,
            ) {
                Ok(report) => report,
                Err(crate::cpa_execution::ExecutionError::ApplyConflict(_)) => {
                    drop(db);
                    return stale_captured_completion(state, oauth_state, &captured);
                }
                Err(error) => return Ok(("error".into(), Some(error.to_string()))),
            }
        };
        #[cfg(test)]
        state.await_device_reconcile_pause().await;
        if guard.cancelled() {
            return Ok((
                "error".into(),
                Some("CPA device login completion is no longer current.".into()),
            ));
        }
        if report.changed {
            let observed = state.settings_revision();
            let committed = state.bump_settings_revision();
            if committed != observed.wrapping_add(1)
                || !state.note_owned_device_settings(oauth_state, observed, committed)
            {
                return stale_captured_completion(state, oauth_state, &captured);
            }
        }
        // The pre-fetch lease authorized the transaction above. Note only this
        // response's committed record, and only while the captured child is still
        // the session child. A concurrent poll's lease is not adopted.
        let child = captured.child_generation;
        let settings_now = state.settings_revision();
        let refreshed = {
            let db = state.db.lock();
            match crate::cpa_execution::capture_native_lease(&db.conn, child, settings_now) {
                Ok(lease) => lease,
                Err(_) => {
                    drop(db);
                    return stale_captured_completion(state, oauth_state, &captured);
                }
            }
        };
        if !state.note_owned_device_lease(oauth_state, settings_now, child, refreshed) {
            return stale_captured_completion(state, oauth_state, &captured);
        }
        report
    };
    if report.needs_apply {
        let Some(noted) = state.owned_device_completion(oauth_state) else {
            return stale_captured_completion(state, oauth_state, &captured);
        };
        if noted.child_generation != captured.child_generation
            || noted.applied_revision != captured.applied_revision
            || noted.applied_digest != captured.applied_digest
            || crate::cpa_execution::device::completion_current(state, &noted).is_err()
        {
            return stale_captured_completion(state, oauth_state, &captured);
        }
        let before_revision = captured.applied_revision;
        if guard.cancelled() {
            return Ok((
                "error".into(),
                Some("CPA device login completion is no longer current.".into()),
            ));
        }
        let applied = match crate::cpa_execution::schedule_owned_device_apply(state, &noted, guard)
            .await
        {
            Ok(report) => report,
            Err(error) => {
                // Cancel already won the helper's lock. The ack bumped settings,
                // but the parsed ok terminal stays. stale_captured_completion
                // would force_stale it only because that revision moved.
                if guard.cancelled() {
                    return Ok((
                        "error".into(),
                        Some("CPA device login completion is no longer current.".into()),
                    ));
                }
                let moved = state
                    .owned_device_completion(oauth_state)
                    .is_none_or(|completion| {
                        crate::cpa_execution::device::completion_current(state, &completion)
                            .is_err()
                    });
                if moved {
                    return stale_captured_completion(state, oauth_state, &captured);
                }
                return Ok(("error".into(), Some(error.to_string())));
            }
        };
        if applied.apply_status != "applied" || applied.applied_revision <= before_revision {
            return Ok((
                "error".into(),
                Some("owned CPA child is not the applied projection".into()),
            ));
        }
        let settings = state.settings_revision();
        let lease = {
            let db = state.db.lock();
            match crate::cpa_execution::capture_native_lease(
                &db.conn,
                applied.child_generation,
                settings,
            ) {
                Ok(lease) => lease,
                Err(_) => {
                    drop(db);
                    return stale_captured_completion(state, oauth_state, &captured);
                }
            }
        };
        if !state.accept_owned_device_write(
            oauth_state,
            applied.child_generation,
            applied.applied_revision,
            applied.applied_digest,
            settings,
            lease,
        ) {
            return stale_captured_completion(state, oauth_state, &captured);
        }
        let Some(noted) = state.owned_device_completion(oauth_state) else {
            return stale_captured_completion(state, oauth_state, &captured);
        };
        if crate::cpa_execution::device::completion_current(state, &noted).is_err() {
            return stale_captured_completion(state, oauth_state, &captured);
        }
        return Ok(("ok".into(), None));
    }
    let Some(noted) = state.owned_device_completion(oauth_state) else {
        return stale_captured_completion(state, oauth_state, &captured);
    };
    if noted.admitted && crate::cpa_execution::device::completion_current(state, &noted).is_ok() {
        return Ok(("ok".into(), None));
    }
    if crate::cpa_execution::device::completion_current(state, &noted).is_err() {
        return stale_captured_completion(state, oauth_state, &captured);
    }
    Ok((
        "error".into(),
        Some("owned CPA ready identity is incomplete".into()),
    ))
}

pub(super) async fn get_runtime(
    State(state): State<CoreState>,
) -> Result<Json<CpaRuntime>, V3ApiError> {
    Ok(Json(execution_runtime_view(&state)))
}

pub(super) async fn check_runtime_update(
    State(state): State<CoreState>,
    body: Bytes,
) -> Result<Json<CpaRuntimeCheck>, V3ApiError> {
    let expectation = parse_mutation_json::<MutationExpectation>(&body)?;
    let _operation = state.cpa_operations.lock().await;
    check_before_external_write(&state, &expectation)?;
    let report = crate::cpa_execution::execution_report(&state);
    Ok(Json(CpaRuntimeCheck {
        current_version: report.current_version,
        latest_version: "v8.0.10".to_string(),
        update_available: false,
        release_url: "pinned://ocg-cpa-host/v8.0.10".to_string(),
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
    resolve_owned_control(&state, input.target, false)?;
    apply_artifact_dir(&state, input.artifact_dir.as_deref());
    let report = crate::cpa_execution::install(
        &state,
        input.expectation.expected_revision,
        input.expectation.process_generation,
        input.expected_version.as_deref(),
    )
    .map_err(|error| map_execution_error(&state, error))?;
    Ok(Json(execution_runtime_view_from(&state, report)))
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
    resolve_owned_control(&state, input.target, false)?;
    apply_artifact_dir(&state, input.artifact_dir.as_deref());
    let report = crate::cpa_execution::update(
        &state,
        input.expectation.expected_revision,
        input.expectation.process_generation,
        input.expected_version.as_deref(),
    )
    .await
    .map_err(|error| map_execution_error(&state, error))?;
    Ok(Json(execution_runtime_view_from(&state, report)))
}

pub(super) async fn remove_runtime(
    State(state): State<CoreState>,
    Query(query): Query<CpaTargetQuery>,
    body: Bytes,
) -> Result<Json<CpaRuntime>, V3ApiError> {
    let expectation = parse_mutation_json::<MutationExpectation>(&body)?;
    let _operation = state.cpa_operations.lock().await;
    {
        let _settings = state.settings_update.lock();
        check_expectation(&state, &expectation)?;
    }
    resolve_owned_control(&state, query.target, false)?;
    let report = crate::cpa_execution::remove(
        &state,
        expectation.expected_revision,
        expectation.process_generation,
    )
    .map_err(|error| map_execution_error(&state, error))?;
    Ok(Json(execution_runtime_view_from(&state, report)))
}

pub(super) async fn start_runtime(
    State(state): State<CoreState>,
    Query(query): Query<CpaTargetQuery>,
    body: Bytes,
) -> Result<Json<CpaRuntime>, V3ApiError> {
    let expectation = parse_mutation_json::<MutationExpectation>(&body)?;
    let _operation = state.cpa_operations.lock().await;
    {
        let _settings = state.settings_update.lock();
        check_expectation(&state, &expectation)?;
    }
    resolve_owned_control(&state, query.target, false)?;
    let report = crate::cpa_execution::start(
        &state,
        expectation.expected_revision,
        expectation.process_generation,
    )
    .await
    .map_err(|error| map_execution_error(&state, error))?;
    Ok(Json(execution_runtime_view_from(&state, report)))
}

pub(super) async fn stop_runtime(
    State(state): State<CoreState>,
    Query(query): Query<CpaTargetQuery>,
    body: Bytes,
) -> Result<Json<CpaRuntime>, V3ApiError> {
    let expectation = parse_mutation_json::<MutationExpectation>(&body)?;
    let _operation = state.cpa_operations.lock().await;
    {
        let _settings = state.settings_update.lock();
        check_expectation(&state, &expectation)?;
    }
    resolve_owned_control(&state, query.target, false)?;
    let report = crate::cpa_execution::stop(
        &state,
        expectation.expected_revision,
        expectation.process_generation,
    )
    .map_err(|error| map_execution_error(&state, error))?;
    Ok(Json(execution_runtime_view_from(&state, report)))
}

pub(super) async fn rollback_runtime(
    State(state): State<CoreState>,
    Query(query): Query<CpaTargetQuery>,
    body: Bytes,
) -> Result<Json<CpaRuntime>, V3ApiError> {
    let expectation = parse_mutation_json::<MutationExpectation>(&body)?;
    let _operation = state.cpa_operations.lock().await;
    {
        let _settings = state.settings_update.lock();
        check_expectation(&state, &expectation)?;
    }
    resolve_owned_control(&state, query.target, false)?;
    let report = crate::cpa_execution::rollback(
        &state,
        expectation.expected_revision,
        expectation.process_generation,
    )
    .await
    .map_err(|error| map_execution_error(&state, error))?;
    Ok(Json(execution_runtime_view_from(&state, report)))
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
    Ok(Json(CpaRuntimeKeys {
        keys: Vec::new(),
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
    let _ = expectation;
    Err(V3ApiError::invalid_request_at(
        &state,
        "cpa_hop_secret_only",
    ))
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
    let _ = (expectation, fingerprint);
    Err(V3ApiError::invalid_request_at(
        &state,
        "cpa_hop_secret_only",
    ))
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
    let _ = (expectation, fingerprint);
    Err(V3ApiError::invalid_request_at(
        &state,
        "cpa_hop_secret_only",
    ))
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
    let client = owned_control_client(&state)?;
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
    integration_view_ignoring_obsolete_selector(state, None)
}

/// `obsolete_selector` is a retired `OCG_CPA_BASE_URL` shape, including a
/// malformed value. It is not parsed, contacted, or preferred over the owned
/// address. Production [`integration_view`] passes `None` and does not read
/// the process environment.
fn integration_view_ignoring_obsolete_selector(
    state: &CoreState,
    _obsolete_selector: Option<&str>,
) -> Result<CpaIntegration, V3ApiError> {
    let managed = cpa_runtime::load_managed(&state.data_dir())
        .map_err(|error| map_runtime_error(state, error))?;
    let execution = crate::cpa_execution::execution_report(state);
    let legacy_migration_required = {
        let db = state.db.lock();
        db.cpa_integration()
            .map_err(V3ApiError::internal)?
            .is_some()
    };
    let owned_present = managed.is_some() || execution.owned;
    let base_url = execution.base_url.clone().or_else(|| {
        managed
            .as_ref()
            .map(|item| format!("http://127.0.0.1:{}", item.port))
    });
    Ok(CpaIntegration {
        configured: owned_present,
        base_url: base_url.unwrap_or_default(),
        base_url_read_only: true,
        management_key_configured: false,
        inference_key_configured: false,
        enabled: false,
        account_id: None,
        model_count: 0,
        models_refreshed_at: None,
        runtime_supported: state.cpa_runtime_supported(),
        runtime_owned: owned_present,
        runtime_running: execution.running,
        installed_version: execution
            .current_version
            .clone()
            .or_else(|| managed.map(|item| item.current_version)),
        latest_version: execution.latest_version.clone(),
        update_available: execution.update_available,
        current_operation: execution.current_operation.clone(),
        runtime_unavailable_reason: owned_runtime_unavailable_reason(state, &execution),
        legacy_migration_required,
        revision: state.settings_revision(),
        process_generation: state.process_generation(),
    })
}

fn owned_runtime_unavailable_reason(
    state: &CoreState,
    execution: &crate::cpa_execution::ExecutionReport,
) -> Option<String> {
    if !state.cpa_runtime_supported() {
        Some(cpa_runtime::UNAVAILABLE_REASON.to_string())
    } else if execution.unavailable {
        Some(
            execution
                .error
                .clone()
                .unwrap_or_else(|| "cpa execution is unavailable".to_string()),
        )
    } else {
        None
    }
}

fn execution_runtime_view(state: &CoreState) -> CpaRuntime {
    execution_runtime_view_from(state, crate::cpa_execution::execution_report(state))
}

fn execution_runtime_view_from(
    state: &CoreState,
    report: crate::cpa_execution::ExecutionReport,
) -> CpaRuntime {
    execution_runtime_view_ignoring_obsolete_selector(state, report, None)
}

/// `obsolete_selector` is ignored. See [`integration_view_ignoring_obsolete_selector`].
/// A stored historical row is not read and does not change this snapshot.
fn execution_runtime_view_ignoring_obsolete_selector(
    state: &CoreState,
    report: crate::cpa_execution::ExecutionReport,
    _obsolete_selector: Option<&str>,
) -> CpaRuntime {
    let unavailable_reason = owned_runtime_unavailable_reason(state, &report);
    CpaRuntime {
        supported: state.cpa_runtime_supported(),
        unavailable_reason,
        installed: report.installed,
        running: report.running,
        desired_running: report.desired_running,
        owned: report.owned,
        current_version: report.current_version,
        previous_version: report.previous_version,
        asset_sha256: report.asset_sha256,
        port: report.port,
        base_url: report.base_url,
        phase: runtime_phase(report.phase),
        error: report.error,
        latest_version: report.latest_version,
        update_available: false,
        current_operation: report.current_operation.clone(),
        child_process_generation: Some(report.child_generation),
        desired_revision: Some(report.desired_revision),
        applied_revision: Some(report.applied_revision),
        desired_digest: digest_field(&report.desired_digest),
        applied_digest: digest_field(&report.applied_digest),
        apply_status: Some(report.apply_status),
        policy_ready: report.policy_ready,
        execution_unavailable: report.unavailable,
        revision: state.settings_revision(),
        process_generation: state.process_generation(),
    }
}

fn digest_field(value: &str) -> Option<String> {
    let digest = value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase());
    digest.then(|| value.to_string())
}

fn apply_artifact_dir(state: &CoreState, artifact_dir: Option<&str>) {
    let Some(dir) = artifact_dir.map(str::trim).filter(|dir| !dir.is_empty()) else {
        return;
    };
    crate::cpa_execution::set_artifact_dir(state, std::path::PathBuf::from(dir));
}

fn resolve_owned_control(
    state: &CoreState,
    explicit: Option<CpaControlTarget>,
    remote_material: bool,
) -> Result<(), V3ApiError> {
    // Omission and `owned` both address the owned child. A saved historical
    // row is not an input and does not select `integration`. Explicit
    // `integration`, or any retired base URL or remote key, is refused here
    // before a client exists and before any write.
    if explicit == Some(CpaControlTarget::Owned) && remote_material {
        return Err(V3ApiError::invalid_request_at(
            state,
            "owned CPA control does not accept a remote base URL or key",
        ));
    }
    if explicit == Some(CpaControlTarget::Integration) || remote_material {
        return Err(V3ApiError::invalid_request_at(
            state,
            remote_target_message(state)?,
        ));
    }
    Ok(())
}

fn remote_target_message(state: &CoreState) -> Result<&'static str, V3ApiError> {
    // The stored row chooses the refusal sentence only. It does not select a
    // target and this read does not open a connection.
    let record = state
        .db
        .lock()
        .cpa_integration()
        .map_err(V3ApiError::internal)?;
    Ok(if record.is_some() {
        REMOTE_MIGRATION
    } else {
        REMOTE_NOT_TARGET
    })
}

fn owned_connection_report(state: &CoreState) -> CpaConnectionReport {
    let ready = crate::cpa_execution::owned_inference_connection(state).is_ok();
    CpaConnectionReport {
        reachable: ready,
        management_ready: false,
        inference_ready: ready,
        version: None,
        commit: None,
        build_date: None,
        model_count: 0,
        management_error: None,
        inference_error: (!ready).then(|| "owned CPA child is not ready".to_string()),
        revision: state.settings_revision(),
        process_generation: state.process_generation(),
    }
}

fn map_execution_error(
    state: &CoreState,
    error: crate::cpa_execution::ExecutionError,
) -> V3ApiError {
    use crate::cpa_execution::ExecutionError;
    match error {
        ExecutionError::ApplyConflict(message) if message == "revisionConflict" => {
            V3ApiError::revision_conflict(state)
        }
        ExecutionError::ApplyConflict(message) => V3ApiError::conflict_at(state, message),
        ExecutionError::ApplyFailed(message) | ExecutionError::Unavailable(message) => {
            V3ApiError::precondition_failed_at(state, message)
        }
        ExecutionError::Invalid(message) => V3ApiError::invalid_request_at(state, message),
        ExecutionError::RollbackUnavailable => {
            V3ApiError::precondition_failed_at(state, "rollback_unavailable")
        }
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

fn control_client(
    state: &CoreState,
    explicit: Option<CpaControlTarget>,
) -> Result<CpaClient, V3ApiError> {
    resolve_owned_control(state, explicit, false)?;
    owned_control_client(state)
}

fn owned_control_client(state: &CoreState) -> Result<CpaClient, V3ApiError> {
    let access = crate::cpa_execution::owned_control_access(state)
        .map_err(|error| map_execution_error(state, error))?;
    let origin = access.origin().to_string();
    CpaClient::new(
        &state.config(),
        &origin,
        access.management().to_string(),
        access.hop().to_string(),
        false,
    )
    .map_err(|error| map_cpa_error(state, error))
}

async fn owned_fenced_rpc<F, Fut>(
    state: &CoreState,
    name: &str,
    auth_index: &str,
    call: F,
) -> Result<(), V3ApiError>
where
    F: FnOnce(CpaClient) -> Fut,
    Fut: std::future::Future<Output = Result<(), cpa::CpaError>>,
{
    let fenced = {
        let db = state.db.lock();
        crate::cpa_execution::fence_mapped_target(&db.conn, name, auth_index)
            .map_err(|error| map_execution_error(state, error))?
    };
    let client = owned_control_client(state)?;
    if let Err(error) = call(client).await {
        let db = state.db.lock();
        let _ = crate::cpa_execution::note_external_failure(
            &db.conn,
            &fenced.credential_id,
            fenced.credential_version,
            fenced.auth_state_version,
        );
        return Err(map_cpa_error(state, error));
    }
    Ok(())
}

async fn refresh_owned_native(
    state: &CoreState,
    expectation: &MutationExpectation,
) -> Result<CpaModels, V3ApiError> {
    let (incoming, origin, needs_apply) = {
        let _operation = state.cpa_operations.lock().await;
        {
            let _settings = state.settings_update.lock();
            check_expectation(state, expectation)?;
        }
        let lease = {
            let db = state.db.lock();
            let generation = crate::cpa_execution::persisted_child_generation(&db.conn)
                .map_err(|error| map_execution_error(state, error))?;
            crate::cpa_execution::capture_native_lease(
                &db.conn,
                generation,
                state.settings_revision(),
            )
            .map_err(|error| map_execution_error(state, error))?
        };
        let access = crate::cpa_execution::owned_control_access(state)
            .map_err(|error| map_execution_error(state, error))?;
        let origin = access.origin().to_string();
        let client = CpaClient::new(
            &state.config(),
            &origin,
            access.management().to_string(),
            access.hop().to_string(),
            false,
        )
        .map_err(|error| map_cpa_error(state, error))?;
        let incoming = client
            .models()
            .await
            .map_err(|error| map_cpa_error(state, error))?;
        let ready = crate::cpa_execution::fetch_owned_ready_once(state)
            .await
            .map_err(|error| map_execution_error(state, error))?;
        let snapshot = crate::cpa_execution::discovery_from_ready_body(&ready);
        let report = {
            let db = state.db.lock();
            if state.settings_revision() != lease.settings_revision {
                return Err(V3ApiError::revision_conflict(state));
            }
            crate::cpa_execution::reconcile_owned_discovery(&db.conn, &snapshot, &lease)
                .map_err(|error| map_execution_error(state, error))?
        };
        if report.changed {
            state.bump_settings_revision();
        }
        (incoming, origin, report.needs_apply)
    };
    if needs_apply {
        let _ = crate::cpa_execution::schedule_owned_apply(state).await;
    }
    Ok(CpaModels {
        models: incoming.iter().map(cpa_model_view).collect(),
        source_url: Some(origin),
        refreshed_at: Some(Utc::now().to_rfc3339()),
        revision: state.settings_revision(),
        process_generation: state.process_generation(),
    })
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

    fn insert_historical_remote(state: &CoreState, base_url: &str, enabled: bool) {
        use crate::models::{Account, AccountSetupStep, AccountType};
        use crate::provider::{
            CPA_ACCOUNT_ID, CPA_ACCOUNT_NAME, CPA_PROVIDER_ID, CredentialKind, QuotaScope,
        };
        let now = chrono::Utc::now();
        let account = Account {
            id: CPA_ACCOUNT_ID.to_string(),
            provider_id: CPA_PROVIDER_ID.to_string(),
            credential_kind: CredentialKind::ApiKey,
            quota_scope: QuotaScope::Key,
            name: CPA_ACCOUNT_NAME.to_string(),
            username: None,
            password_cipher: None,
            key_cipher: state.encrypt_key("historical-inference").unwrap(),
            enabled,
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
            created_at: now,
            updated_at: now,
        };
        state
            .db
            .lock()
            .upsert_cpa_integration(
                &account,
                base_url,
                &state.encrypt_key("historical-management").unwrap(),
            )
            .unwrap();
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
        let error = put_integration(State(state.clone()), body)
            .await
            .expect_err("remote CPA material is not stored");
        let message = error_message(error).await;
        assert!(message.contains("Remote CPA is not a target"), "{message}");
        assert!(!message.contains("management-secret"));
        assert!(!message.contains("inference-secret"));
        assert!(state.db.lock().cpa_integration().unwrap().is_none());

        insert_historical_remote(&state, "http://127.0.0.1:9", true);
        let delete = Bytes::from(
            serde_json::to_vec(&json!({
                "expectedRevision": state.settings_revision(),
                "processGeneration": state.process_generation()
            }))
            .unwrap(),
        );
        let error = delete_integration(State(state.clone()), delete)
            .await
            .expect_err("historical remote row is not deleted");
        let message = error_message(error).await;
        assert!(message.contains("explicit migration"), "{message}");
        assert!(!message.contains("127.0.0.1"));
        assert!(!message.contains("historical-inference"));
        let kept = state.db.lock().cpa_integration().unwrap().unwrap();
        assert_eq!(kept.base_url, "http://127.0.0.1:9");
        assert!(
            state
                .db
                .lock()
                .get_account(CPA_ACCOUNT_ID)
                .unwrap()
                .is_some()
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
                .is_err(),
            "remote CPA material is refused before the queued oauth write"
        );
        assert!(state.db.lock().cpa_integration().unwrap().is_none());

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
        let queued = tokio::spawn(async move {
            start_oauth(
                State(queued_state),
                Query(CpaTargetQuery::default()),
                request,
            )
            .await
        });
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
        insert_historical_remote(&state, "http://127.0.0.1:8317", false);
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
        let Json(updated) = put_integration(State(state.clone()), enabled_change)
            .await
            .expect("enabled-only request does not rewrite the stored row");
        assert!(!updated.enabled);
        assert!(updated.runtime_owned);
        assert!(updated.configured);
        assert!(updated.legacy_migration_required);
        assert_eq!(updated.base_url, "http://127.0.0.1:8317");
        assert!(updated.base_url_read_only);
        assert!(!updated.management_key_configured);
        assert!(!updated.inference_key_configured);
        assert!(updated.account_id.is_none());
        assert_eq!(updated.model_count, 0);
        assert!(updated.models_refreshed_at.is_none());
        let reason = updated.runtime_unavailable_reason.unwrap_or_default();
        assert!(
            !reason.contains("explicit migration"),
            "migration stays off the runtime reason: {reason}"
        );
        assert!(
            !reason.contains("OCG_CPA_BASE_URL"),
            "obsolete env stays off the runtime reason: {reason}"
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
                .is_err()
        );
        assert_eq!(
            state.db.lock().cpa_integration().unwrap().unwrap().base_url,
            "http://127.0.0.1:8317"
        );
        assert!(
            !state
                .db
                .lock()
                .get_account(CPA_ACCOUNT_ID)
                .unwrap()
                .unwrap()
                .enabled
        );

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

    fn assert_no_remote_contact(fake: &FakeCpa) {
        assert_eq!(fake.status.load(Ordering::SeqCst), 0);
        assert_eq!(fake.delete.load(Ordering::SeqCst), 0);
        assert_eq!(fake.reset.load(Ordering::SeqCst), 0);
        assert_eq!(fake.oauth_start.load(Ordering::SeqCst), 0);
        assert_eq!(fake.oauth_cancel.load(Ordering::SeqCst), 0);
    }

    fn integration_target() -> Query<CpaTargetQuery> {
        Query(CpaTargetQuery {
            target: Some(CpaControlTarget::Integration),
        })
    }

    fn stored_remote(state: &CoreState) -> (String, bool) {
        let db = state.db.lock();
        let row = db
            .cpa_integration()
            .unwrap()
            .expect("historical remote row");
        let enabled = db
            .get_account(CPA_ACCOUNT_ID)
            .unwrap()
            .expect("historical account")
            .enabled;
        (row.base_url, enabled)
    }

    #[tokio::test]
    async fn successful_cpa_side_effects_bump_revision_and_reject_stale_tokens() {
        let (dir, state) = test_state("side-effect-cas");
        let (base_url, fake) = spawn_fake_cpa().await;
        fake.fail_status.store(true, Ordering::SeqCst);
        let generation = state.process_generation();
        let revision = state.settings_revision();
        let refused = Bytes::from(
            serde_json::to_vec(&json!({
                "expectedRevision": revision,
                "processGeneration": generation,
                "baseUrl": base_url,
                "managementKey": "management-secret",
                "inferenceKey": "inference-secret",
                "enabled": true
            }))
            .unwrap(),
        );
        let error = put_integration(State(state.clone()), refused)
            .await
            .expect_err("remote CPA material is not stored");
        let message = error_message(error).await;
        assert!(message.contains("Remote CPA is not a target"), "{message}");
        assert!(!message.contains("management-secret"), "{message}");
        assert!(!message.contains("inference-secret"), "{message}");
        assert!(!message.contains(&base_url), "{message}");
        assert!(state.db.lock().cpa_integration().unwrap().is_none());
        assert_eq!(state.settings_revision(), revision);
        assert_no_remote_contact(&fake);

        insert_historical_remote(&state, &base_url, true);
        let (stored_url, stored_enabled) = stored_remote(&state);
        let status = json!({
            "name": "claude account.json",
            "authIndex": "claude-1",
            "disabled": true
        });
        let account = json!({
            "name": "claude account.json",
            "authIndex": "claude-1"
        });
        let error = set_account_status(
            State(state.clone()),
            integration_target(),
            mutation_bytes(&state, status.clone()),
        )
        .await
        .expect_err("explicit integration status does not call CPA");
        let message = error_message(error).await;
        assert!(message.contains("explicit migration"), "{message}");
        assert!(!message.contains(&base_url), "{message}");

        let error = set_account_status(
            State(state.clone()),
            Query(CpaTargetQuery::default()),
            mutation_bytes(&state, status.clone()),
        )
        .await
        .expect_err("unbound owned status stays local");
        let message = error_message(error).await;
        assert!(
            message.contains("owned native account is not bound"),
            "{message}"
        );

        let error = delete_account(
            State(state.clone()),
            integration_target(),
            mutation_bytes(&state, account.clone()),
        )
        .await
        .expect_err("explicit integration delete does not call CPA");
        let message = error_message(error).await;
        assert!(message.contains("explicit migration"), "{message}");
        let error = delete_account(
            State(state.clone()),
            Query(CpaTargetQuery::default()),
            mutation_bytes(&state, account.clone()),
        )
        .await
        .expect_err("unbound owned delete stays local");
        let message = error_message(error).await;
        assert!(
            message.contains("owned native account is not bound"),
            "{message}"
        );

        let error = reset_quota(
            State(state.clone()),
            integration_target(),
            mutation_bytes(&state, account.clone()),
        )
        .await
        .expect_err("explicit integration quota reset does not call CPA");
        let message = error_message(error).await;
        assert!(message.contains("explicit migration"), "{message}");
        let error = reset_quota(
            State(state.clone()),
            Query(CpaTargetQuery::default()),
            mutation_bytes(&state, account),
        )
        .await
        .expect_err("unbound owned quota reset stays local");
        let message = error_message(error).await;
        assert!(
            message.contains("owned native account is not bound"),
            "{message}"
        );

        let error = start_oauth(
            State(state.clone()),
            integration_target(),
            mutation_bytes(&state, json!({ "provider": "codex" })),
        )
        .await
        .expect_err("explicit integration oauth does not call CPA");
        let message = error_message(error).await;
        assert!(message.contains("explicit migration"), "{message}");
        let error = start_oauth(
            State(state.clone()),
            Query(CpaTargetQuery::default()),
            mutation_bytes(&state, json!({ "provider": "codex" })),
        )
        .await
        .expect_err("owned oauth does not use the historical base URL");
        let message = error_message(error).await;
        assert!(message.contains("owned CPA child is missing"), "{message}");
        let error = cancel_oauth(
            State(state.clone()),
            mutation_bytes(&state, json!({ "state": "oauth-state-1" })),
        )
        .await
        .expect_err("owned oauth cancel does not use the historical base URL");
        let message = error_message(error).await;
        assert!(message.contains("owned CPA child is missing"), "{message}");

        let stale_status = set_account_status(
            State(state.clone()),
            Query(CpaTargetQuery::default()),
            mutation_bytes_at(revision.wrapping_add(9), generation, status),
        )
        .await
        .expect_err("stale owned status is a conflict before network");
        assert_stale_conflict(stale_status, revision, generation);
        let stale_oauth = start_oauth(
            State(state.clone()),
            integration_target(),
            mutation_bytes_at(
                revision.wrapping_add(9),
                generation,
                json!({ "provider": "codex" }),
            ),
        )
        .await
        .expect_err("stale oauth is a conflict before network");
        assert_stale_conflict(stale_oauth, revision, generation);
        let stale_cancel = cancel_oauth(
            State(state.clone()),
            mutation_bytes_at(
                revision.wrapping_add(9),
                generation,
                json!({ "state": "oauth-state-1" }),
            ),
        )
        .await
        .expect_err("stale oauth cancel is a conflict before network");
        assert_stale_conflict(stale_cancel, revision, generation);

        assert_no_remote_contact(&fake);
        assert_eq!(state.settings_revision(), revision);
        let (url, enabled) = stored_remote(&state);
        assert_eq!(url, stored_url);
        assert_eq!(enabled, stored_enabled);
        drop(state);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn failed_cpa_side_effect_does_not_bump_revision() {
        let (dir, state) = test_state("failed-status-cas");
        let (base_url, fake) = spawn_fake_cpa().await;
        fake.fail_status.store(true, Ordering::SeqCst);
        let revision = state.settings_revision();
        let generation = state.process_generation();
        let stale = Bytes::from(
            serde_json::to_vec(&json!({
                "expectedRevision": revision.wrapping_sub(1),
                "processGeneration": generation,
                "baseUrl": base_url,
                "managementKey": "management-secret",
                "inferenceKey": "inference-secret"
            }))
            .unwrap(),
        );
        let error = put_integration(State(state.clone()), stale)
            .await
            .expect_err("stale remote configure conflicts before persistence");
        assert_stale_conflict(error, revision, generation);
        assert!(state.db.lock().cpa_integration().unwrap().is_none());

        let current = Bytes::from(
            serde_json::to_vec(&json!({
                "expectedRevision": revision,
                "processGeneration": generation,
                "baseUrl": base_url,
                "managementKey": "management-secret",
                "inferenceKey": "inference-secret"
            }))
            .unwrap(),
        );
        let error = put_integration(State(state.clone()), current)
            .await
            .expect_err("current remote configure is refused");
        let message = error_message(error).await;
        assert!(message.contains("Remote CPA is not a target"), "{message}");
        assert!(!message.contains("management-secret"), "{message}");
        assert!(!message.contains("inference-secret"), "{message}");
        assert!(state.db.lock().cpa_integration().unwrap().is_none());

        insert_historical_remote(&state, &base_url, false);
        let (stored_url, stored_enabled) = stored_remote(&state);
        let body = mutation_bytes(
            &state,
            json!({
                "name": "claude account.json",
                "authIndex": "claude-1",
                "disabled": true
            }),
        );
        let error = set_account_status(
            State(state.clone()),
            Query(CpaTargetQuery::default()),
            body.clone(),
        )
        .await
        .expect_err("unbound status does not become a remote failure");
        let message = error_message(error).await;
        assert!(
            message.contains("owned native account is not bound"),
            "{message}"
        );
        assert_eq!(state.settings_revision(), revision);

        fake.fail_status.store(false, Ordering::SeqCst);
        let error =
            set_account_status(State(state.clone()), Query(CpaTargetQuery::default()), body)
                .await
                .expect_err("the same token still does not reach CPA");
        let message = error_message(error).await;
        assert!(
            message.contains("owned native account is not bound"),
            "{message}"
        );
        assert_eq!(state.settings_revision(), revision);
        assert_eq!(state.process_generation(), generation);
        assert_no_remote_contact(&fake);
        let (url, enabled) = stored_remote(&state);
        assert_eq!(url, stored_url);
        assert_eq!(enabled, stored_enabled);

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
            start_runtime(
                State(state.clone()),
                Query(CpaTargetQuery::default()),
                mutation_bytes(&state, json!({})),
            )
            .await,
            "already-running start persists run intent",
        );
        assert_eq!(started.revision, before + 1);
        assert!(started.running);
        assert!(started.desired_running);
        assert_eq!(host.starts.load(Ordering::SeqCst), 0);

        let Json(stopped) = unwrap_ok(
            stop_runtime(
                State(state.clone()),
                Query(CpaTargetQuery::default()),
                mutation_bytes(&state, json!({})),
            )
            .await,
            "runtime stop should succeed",
        );
        assert_eq!(stopped.revision, before + 2);
        assert!(!stopped.running);
        assert!(!stopped.desired_running);
        assert_eq!(host.stops.load(Ordering::SeqCst), 1);

        let error = stop_runtime(
            State(state.clone()),
            Query(CpaTargetQuery::default()),
            mutation_bytes_at(before, generation, json!({})),
        )
        .await
        .expect_err("stale runtime stop token must 409");
        assert_stale_conflict(error, stopped.revision, generation);
        assert_eq!(host.stops.load(Ordering::SeqCst), 1);
        assert!(!host.owned_running());

        let error = start_runtime(
            State(state.clone()),
            Query(CpaTargetQuery::default()),
            mutation_bytes_at(before, generation, json!({})),
        )
        .await
        .expect_err("stale runtime start token must 409");
        assert_stale_conflict(error, stopped.revision, generation);
        assert_eq!(host.starts.load(Ordering::SeqCst), 0);

        drop(state);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn owned_target_does_not_write_the_remote_row() {
        let (dir, state) = test_state("owned-target");
        let host = Arc::new(CountingRuntimeHost {
            running: AtomicBool::new(false),
            starts: AtomicUsize::new(0),
            stops: AtomicUsize::new(0),
        });
        state.set_cpa_runtime_host(host.clone());
        let owned = Bytes::from(
            serde_json::to_vec(&json!({
                "expectedRevision": state.settings_revision(),
                "processGeneration": state.process_generation(),
                "target": "owned"
            }))
            .unwrap(),
        );
        let Json(view) = put_integration(State(state.clone()), owned)
            .await
            .expect("explicit owned omits the remote row");
        assert!(!view.configured);
        assert!(state.db.lock().cpa_integration().unwrap().is_none());

        let mixed = Bytes::from(
            serde_json::to_vec(&json!({
                "expectedRevision": state.settings_revision(),
                "processGeneration": state.process_generation(),
                "target": "owned",
                "baseUrl": "http://127.0.0.1:8317",
                "managementKey": "remote-management",
                "inferenceKey": "remote-inference"
            }))
            .unwrap(),
        );
        let error = put_integration(State(state.clone()), mixed)
            .await
            .expect_err("owned plus remote material");
        let message = error_message(error).await;
        assert!(message.contains("remote base URL or key"), "{message}");
        assert!(state.db.lock().cpa_integration().unwrap().is_none());

        state
            .db
            .lock()
            .conn
            .execute(
                "INSERT INTO access_keys (id, name, key, is_primary, enabled, deleted_at, created_at)
                 VALUES ('owned-downstream', 'downstream', 'downstream-client-key', 0, 1, NULL, ?1)",
                [chrono::Utc::now().to_rfc3339()],
            )
            .unwrap();
        let downstream = Bytes::from(
            serde_json::to_vec(&json!({
                "expectedRevision": state.settings_revision(),
                "processGeneration": state.process_generation(),
                "baseUrl": "http://127.0.0.1:8317",
                "managementKey": "remote-management",
                "inferenceKey": "downstream-client-key"
            }))
            .unwrap(),
        );
        let error = put_integration(State(state.clone()), downstream)
            .await
            .expect_err("downstream key");
        let message = error_message(error).await;
        assert!(
            message.contains("Remote CPA is not a target in this product"),
            "{message}"
        );
        assert!(!message.contains("downstream-client-key"), "{message}");
        assert!(!message.contains("remote-management"), "{message}");
        assert!(state.db.lock().cpa_integration().unwrap().is_none());

        let created = Bytes::from(
            serde_json::to_vec(&json!({
                "expectedRevision": state.settings_revision(),
                "processGeneration": state.process_generation(),
                "baseUrl": "http://127.0.0.1:8317",
                "managementKey": "remote-management",
                "inferenceKey": "opaque-provider-key"
            }))
            .unwrap(),
        );
        let error = put_integration(State(state.clone()), created)
            .await
            .expect_err("remote create is not a target");
        let message = error_message(error).await;
        assert!(message.contains("Remote CPA is not a target"), "{message}");
        assert!(!message.contains("opaque-provider-key"), "{message}");
        assert!(!message.contains("remote-management"), "{message}");
        assert!(state.db.lock().cpa_integration().unwrap().is_none());

        let error = start_runtime(
            State(state.clone()),
            integration_target(),
            mutation_bytes(&state, json!({})),
        )
        .await
        .expect_err("integration start does not launch a child");
        let message = error_message(error).await;
        assert!(message.contains("Remote CPA is not a target"), "{message}");
        assert!(state.db.lock().cpa_integration().unwrap().is_none());
        assert_eq!(host.starts.load(Ordering::SeqCst), 0);
        assert_eq!(host.stops.load(Ordering::SeqCst), 0);
        assert!(!host.owned_running());
        drop(state);
        std::fs::remove_dir_all(dir).unwrap();
    }

    fn insert_historical_catalog(state: &CoreState) {
        let refreshed = chrono::DateTime::parse_from_rfc3339("2020-01-02T03:04:05Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        state
            .db
            .lock()
            .replace_cpa_model_catalog(
                &[crate::db::CpaCatalogModel {
                    id: "historical-model".into(),
                    owned_by: Some("remote".into()),
                    enabled: true,
                }],
                "http://10.1.2.3:9/v1",
                refreshed,
            )
            .unwrap();
    }

    fn persisted_cpa_bytes(state: &CoreState) -> Vec<String> {
        let db = state.db.lock();
        let conn = &db.conn;
        let mut lines = Vec::new();
        let mut destinations = conn
            .prepare(
                "SELECT id, IFNULL(base_url, ''), IFNULL(observer_credential_id, ''), enabled
                 FROM destinations ORDER BY id",
            )
            .unwrap();
        for row in destinations
            .query_map([], |row| {
                Ok(format!(
                    "dest {}|{}|{}|{}",
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?
                ))
            })
            .unwrap()
        {
            lines.push(row.unwrap());
        }
        let mut credentials = conn
            .prepare(
                "SELECT id, legacy_account_id, key_cipher, enabled, IFNULL(updated_at, '')
                 FROM credentials ORDER BY id",
            )
            .unwrap();
        for row in credentials
            .query_map([], |row| {
                Ok(format!(
                    "cred {}|{}|{}|{}|{}",
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, String>(4)?
                ))
            })
            .unwrap()
        {
            lines.push(row.unwrap());
        }
        let mut catalogs = conn
            .prepare(
                "SELECT provider_id, models_json, IFNULL(refreshed_at, ''), IFNULL(source_url, '')
                 FROM provider_model_catalogs ORDER BY provider_id",
            )
            .unwrap();
        for row in catalogs
            .query_map([], |row| {
                Ok(format!(
                    "catalog {}|{}|{}|{}",
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?
                ))
            })
            .unwrap()
        {
            lines.push(row.unwrap());
        }
        let models_sql = "SELECT destination_id, public_model, upstream_model, protocols_json,
                                 IFNULL(preferred, ''), enabled, IFNULL(upstream_override, '')
                          FROM destination_models ORDER BY destination_id, public_model_key";
        let mut models = conn.prepare(models_sql).unwrap();
        for row in models
            .query_map([], |row| {
                Ok(format!(
                    "model {}|{}|{}|{}|{}|{}|{}",
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, String>(6)?
                ))
            })
            .unwrap()
        {
            lines.push(row.unwrap());
        }
        lines
    }

    fn assert_health_independent_of_migration(view: &CpaIntegration) {
        let reason = view.runtime_unavailable_reason.as_deref().unwrap_or("");
        assert!(
            !reason.contains("explicit migration"),
            "migration indicator is not runtime health: {reason}"
        );
        assert!(
            !reason.contains("OCG_CPA_BASE_URL"),
            "obsolete selector is not runtime health: {reason}"
        );
    }

    #[tokio::test]
    async fn owned_view_keeps_historical_bytes_and_ignores_obsolete_selector() {
        let (absent_dir, absent) = test_state("owned-absent");
        insert_historical_remote(&absent, "http://10.1.2.3:9", true);
        insert_historical_catalog(&absent);
        let absent_bytes = persisted_cpa_bytes(&absent);
        let absent_revision = absent.settings_revision();
        let absent_view = integration_view_ignoring_obsolete_selector(&absent, Some("http://[::1"))
            .expect("a malformed obsolete selector does not fail the owned view");
        assert!(!absent_view.configured);
        assert!(!absent_view.runtime_owned);
        assert!(!absent_view.runtime_running);
        assert!(absent_view.base_url.is_empty());
        assert!(absent_view.legacy_migration_required);
        assert!(!absent_view.enabled);
        assert!(absent_view.account_id.is_none());
        assert_eq!(absent_view.model_count, 0);
        assert!(absent_view.models_refreshed_at.is_none());
        assert_health_independent_of_migration(&absent_view);
        let absent_runtime = execution_runtime_view_ignoring_obsolete_selector(
            &absent,
            crate::cpa_execution::execution_report(&absent),
            Some("http://192.0.2.8:1"),
        );
        assert!(!absent_runtime.installed);
        assert!(!absent_runtime.running);
        assert!(!absent_runtime.owned);
        assert!(absent_runtime.base_url.is_none());
        assert_eq!(
            absent_runtime.unavailable_reason,
            absent_view.runtime_unavailable_reason
        );
        let Json(first) = get_integration(State(absent.clone()))
            .await
            .expect("historical GET");
        let Json(second) = get_integration(State(absent.clone()))
            .await
            .expect("repeated historical GET");
        assert_eq!(first, second);
        assert_eq!(absent.settings_revision(), absent_revision);
        assert_eq!(persisted_cpa_bytes(&absent), absent_bytes);
        drop(absent);
        std::fs::remove_dir_all(absent_dir).unwrap();

        let (clean_dir, clean) = test_state("owned-clean");
        let clean_view = integration_view(&clean).expect("absent owned view");
        assert!(!clean_view.legacy_migration_required);
        assert_eq!(
            clean_view.runtime_unavailable_reason,
            absent_view.runtime_unavailable_reason
        );
        assert!(!clean_view.configured);
        assert!(clean_view.base_url.is_empty());
        drop(clean);
        std::fs::remove_dir_all(clean_dir).unwrap();

        let (dir, state) = test_state("owned-present");
        let host = Arc::new(CountingRuntimeHost {
            running: AtomicBool::new(true),
            starts: AtomicUsize::new(0),
            stops: AtomicUsize::new(0),
        });
        state.set_cpa_runtime_host(host.clone());
        insert_historical_remote(&state, "http://10.1.2.3:9", true);
        insert_historical_catalog(&state);
        cpa_runtime::save_managed(
            &dir,
            &cpa_runtime::ManagedCpa {
                current_version: "7.2.147".into(),
                previous_version: None,
                asset_sha256: "b".repeat(64),
                port: 8317,
                desired_running: true,
            },
        )
        .unwrap();
        let before = persisted_cpa_bytes(&state);
        let revision = state.settings_revision();
        let selectors = ["http://192.0.2.8:1", "http://[::1", "not a url", ""];
        for selector in selectors {
            let view = integration_view_ignoring_obsolete_selector(&state, Some(selector))
                .expect("obsolete selector is ignored");
            assert!(view.configured);
            assert!(view.runtime_owned);
            assert!(view.runtime_running);
            assert_eq!(view.base_url, "http://127.0.0.1:8317");
            assert!(view.base_url_read_only);
            assert_eq!(view.installed_version.as_deref(), Some("7.2.147"));
            assert!(view.runtime_supported);
            assert!(view.runtime_unavailable_reason.is_none());
            assert!(view.legacy_migration_required);
            assert!(!view.enabled);
            assert!(!view.management_key_configured);
            assert!(!view.inference_key_configured);
            assert!(view.account_id.is_none());
            assert_eq!(view.model_count, 0);
            assert!(view.models_refreshed_at.is_none());
            assert!(!view.base_url.contains("10.1.2.3"));
            assert!(!view.base_url.contains("192.0.2.8"));
            assert_health_independent_of_migration(&view);
            let report = crate::cpa_execution::execution_report(&state);
            let runtime = execution_runtime_view_ignoring_obsolete_selector(
                &state,
                report.clone(),
                Some(selector),
            );
            assert_eq!(runtime.installed, report.installed);
            assert_eq!(runtime.running, report.running);
            assert!(runtime.running);
            assert_eq!(runtime.owned, report.owned);
            assert_eq!(runtime.unavailable_reason, view.runtime_unavailable_reason);
            let reason = runtime.unavailable_reason.as_deref().unwrap_or("");
            assert!(!reason.contains(selector) || selector.is_empty());
            assert!(!reason.contains("explicit migration"));
            assert!(!reason.contains("OCG_CPA_BASE_URL"));
        }
        let Json(repeated) = get_integration(State(state.clone()))
            .await
            .expect("owned GET");
        assert_eq!(state.settings_revision(), revision);
        assert_eq!(persisted_cpa_bytes(&state), before);
        assert!(repeated.legacy_migration_required);
        assert!(repeated.runtime_running);
        assert_eq!(host.starts.load(Ordering::SeqCst), 0);
        assert_eq!(host.stops.load(Ordering::SeqCst), 0);
        drop(state);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn explicit_remote_material_refuses_without_network_or_writes() {
        let (dir, state) = test_state("remote-refuse");
        insert_historical_remote(&state, "http://10.1.2.3:9", true);
        insert_historical_catalog(&state);
        let before = persisted_cpa_bytes(&state);
        let revision = state.settings_revision();
        let generation = state.process_generation();
        let remote = json!({
            "target": "integration",
            "baseUrl": "http://10.1.2.3:9",
            "managementKey": "historical-management",
            "inferenceKey": "historical-inference"
        });
        let error = put_integration(State(state.clone()), mutation_bytes(&state, remote.clone()))
            .await
            .expect_err("explicit integration material is refused");
        let message = error_message(error).await;
        assert!(message.contains("explicit migration"), "{message}");
        assert!(!message.contains("10.1.2.3"), "{message}");
        assert!(!message.contains("historical-management"), "{message}");
        assert!(!message.contains("historical-inference"), "{message}");

        let omitted = json!({
            "baseUrl": "http://10.9.8.7:4",
            "managementKey": "other-management"
        });
        let error = put_integration(State(state.clone()), mutation_bytes(&state, omitted))
            .await
            .expect_err("omitted target plus remote material stays a refusal");
        let message = error_message(error).await;
        assert!(message.contains("explicit migration"), "{message}");

        let owned_mixed = json!({
            "target": "owned",
            "inferenceKey": "historical-inference"
        });
        let error = put_integration(State(state.clone()), mutation_bytes(&state, owned_mixed))
            .await
            .expect_err("owned plus a remote key is refused");
        let message = error_message(error).await;
        assert!(message.contains("remote base URL or key"), "{message}");

        let probe = Bytes::from(serde_json::to_vec(&remote).unwrap());
        let error = test_connection(State(state.clone()), probe)
            .await
            .expect_err("remote connection test does not open a client");
        let message = error_message(error).await;
        assert!(message.contains("explicit migration"), "{message}");
        assert!(!message.contains("historical-inference"), "{message}");

        let error = delete_integration(State(state.clone()), mutation_bytes(&state, json!({})))
            .await
            .expect_err("delete does not remove the historical row");
        let message = error_message(error).await;
        assert!(message.contains("explicit migration"), "{message}");

        assert_eq!(state.settings_revision(), revision);
        assert_eq!(state.process_generation(), generation);
        assert_eq!(persisted_cpa_bytes(&state), before);
        let (url, enabled) = stored_remote(&state);
        assert_eq!(url, "http://10.1.2.3:9");
        assert!(enabled);
        drop(state);
        std::fs::remove_dir_all(dir).unwrap();
    }

    async fn error_message(error: V3ApiError) -> String {
        let response = error.into_response();
        let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .unwrap();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        body["message"].as_str().unwrap_or_default().to_string()
    }
}

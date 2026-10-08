//! Authenticated V4 CAS boundary for the concrete Copilot extension.
use super::applications::{DashboardReceipt, application_gateway_key_effect};
use super::types::{CopilotApplication, CopilotInstallRequest, CopilotMutationRequest};
use crate::{
    byok_application::{ByokError, ByokErrorKind},
    copilot_application::*,
    dashboard_v3::{
        ControlRevision, MutationExpectation, V3ApiError, check_expectation, parse_mutation_json,
    },
    dsh_application::DshGatewaySecret,
    log_types::{OperationMetadata, OperationOutcome},
    state::CoreState,
};
use axum::{
    Json,
    body::Bytes,
    extract::{Query, State},
    http::{StatusCode, header},
    response::IntoResponse,
};
fn gateway_url(state: &CoreState) -> String {
    let settings = state.settings_config();
    let root = settings.client_root_url.trim_end_matches('/');
    if root.is_empty() {
        format!("http://127.0.0.1:{}/v1", state.active_gateway_port())
    } else {
        format!("{root}/v1")
    }
}
fn payload(state: &CoreState, inspection: CopilotInspection) -> CopilotApplication {
    let _guard = state.settings_update.lock();
    CopilotApplication {
        inspection,
        gateway_v1_url: gateway_url(state),
        revision: ControlRevision::from_state(state),
    }
}
fn host_error(state: &CoreState, e: ByokError) -> V3ApiError {
    match e.kind {
        ByokErrorKind::Invalid => V3ApiError::invalid_request_at(state, e.message),
        ByokErrorKind::Precondition => V3ApiError::precondition_failed_at(state, e.message),
        ByokErrorKind::Conflict => V3ApiError::conflict_at(state, e.message),
        ByokErrorKind::Internal => V3ApiError::internal_at(state, e.message),
    }
}
pub(super) async fn inspect(
    State(state): State<CoreState>,
    Query(target): Query<CopilotTarget>,
) -> Result<Json<CopilotApplication>, V3ApiError> {
    let error_state = state.clone();
    let result = tokio::task::spawn_blocking(move || {
        let inspection = match state.copilot_application_host() {
            Some(host) => host(CopilotApplicationHostRequest::Inspect { target })
                .map_err(|e| host_error(&state, e))?,
            None => CopilotInspection::unsupported(target),
        };
        Ok::<_, V3ApiError>(payload(&state, inspection))
    })
    .await
    .map_err(|_| V3ApiError::internal_at(&error_state, "Copilot inspection task failed"))??;
    Ok(Json(result))
}
pub(super) async fn download_package() -> impl IntoResponse {
    match crate::copilot_extension_package::vsix_bytes() {
        Ok(bytes) => (
            [
                (header::CONTENT_TYPE, "application/octet-stream"),
                (
                    header::CONTENT_DISPOSITION,
                    "attachment; filename=\"open-console-gateway-copilot.vsix\"",
                ),
            ],
            bytes,
        )
            .into_response(),
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "Cannot prepare Copilot extension package",
        )
            .into_response(),
    }
}
pub(super) async fn install(
    State(state): State<CoreState>,
    body: Bytes,
) -> Result<Json<CopilotApplication>, V3ApiError> {
    let mut receipt = DashboardReceipt::open(
        &state,
        "application.install",
        "application",
        Some("copilot-extension".into()),
    );
    let created = std::sync::Arc::new(std::sync::Mutex::new(None));
    let slot = created.clone();
    let task_state = state.clone();
    let result = tokio::task::spawn_blocking(move || {
        let input = parse_mutation_json::<CopilotInstallRequest>(&body)?;
        let host = task_state.copilot_application_host().ok_or_else(|| {
            V3ApiError::precondition_failed_at(
                &task_state,
                "Install Copilot on the local VS Code host, or download the VSIX for manual setup",
            )
        })?;
        let expectation = MutationExpectation {
            expected_revision: input.expected_revision,
            process_generation: input.process_generation,
        };
        {
            let _guard = task_state.settings_update.lock();
            check_expectation(&task_state, &expectation)?;
        }
        // Nonmutating target/provenance preflight precedes Key creation.
        let before = host(CopilotApplicationHostRequest::Inspect {
            target: input.target.clone(),
        })
        .map_err(|e| host_error(&task_state, e))?;
        if before.fingerprint.as_deref() != Some(&input.expected_fingerprint) {
            return Err(V3ApiError::conflict_at(
                &task_state,
                "Copilot target changed; inspect again before installing",
            ));
        }
        if !before.install_supported {
            return Err(V3ApiError::precondition_failed_at(
                &task_state,
                "Copilot cannot be installed into this target",
            ));
        }
        let (url, key) = {
            let _guard = task_state.settings_update.lock();
            check_expectation(&task_state, &expectation)?;
            let url = validated_gateway(&gateway_url(&task_state))
                .map_err(|e| host_error(&task_state, e))?;
            let key = if let Some(id) = input.key_id.as_deref() {
                super::applications::selected_gateway_key(&task_state, id)?
            } else {
                let effect = application_gateway_key_effect(&task_state, "copilot")?;
                if let (Some(id), Some(revision)) = (effect.created_id, effect.revision) {
                    *slot.lock().unwrap() = Some((id, revision));
                }
                effect.key
            };
            (url, key)
        };
        let inspection = host(CopilotApplicationHostRequest::Install {
            target: input.target,
            expected_fingerprint: input.expected_fingerprint,
            gateway_v1_url: url,
            secret: DshGatewaySecret::new(key),
        })
        .map_err(|e| host_error(&task_state, e))?;
        Ok::<_, V3ApiError>(payload(&task_state, inspection))
    })
    .await
    .map_err(|_| V3ApiError::internal_at(&state, "Copilot installation task failed"))
    .and_then(|v| v);
    let key_effect = created.lock().unwrap().take();
    if let Some((id, revision)) = key_effect {
        if result.is_err() {
            receipt.decide(
                OperationOutcome::Partial,
                None,
                OperationMetadata {
                    revision: Some(revision),
                    related_ids: vec![id],
                    requested_count: Some(2),
                    completed_count: Some(1),
                    failed_count: Some(1),
                    ..Default::default()
                },
            );
        } else {
            receipt.succeed(OperationMetadata {
                revision: Some(revision),
                related_ids: vec![id],
                ..Default::default()
            });
        }
    }
    receipt.finish(result).map(Json)
}
pub(super) async fn disconnect(
    State(state): State<CoreState>,
    body: Bytes,
) -> Result<Json<CopilotApplication>, V3ApiError> {
    mutate(state, body, false).await.map(Json)
}
pub(super) async fn uninstall(
    State(state): State<CoreState>,
    body: Bytes,
) -> Result<Json<CopilotApplication>, V3ApiError> {
    mutate(state, body, true).await.map(Json)
}
async fn mutate(
    state: CoreState,
    body: Bytes,
    uninstall: bool,
) -> Result<CopilotApplication, V3ApiError> {
    let receipt = DashboardReceipt::open(
        &state,
        if uninstall {
            "application.uninstall"
        } else {
            "application.disconnect"
        },
        "application",
        Some("copilot-extension".into()),
    );
    let task_state = state.clone();
    let result = tokio::task::spawn_blocking(move || {
        let input = parse_mutation_json::<CopilotMutationRequest>(&body)?;
        let host = task_state.copilot_application_host().ok_or_else(|| {
            V3ApiError::precondition_failed_at(&task_state, "Copilot local host is unavailable")
        })?;
        {
            let _guard = task_state.settings_update.lock();
            check_expectation(
                &task_state,
                &MutationExpectation {
                    expected_revision: input.expected_revision,
                    process_generation: input.process_generation,
                },
            )?;
        }
        let request = if uninstall {
            CopilotApplicationHostRequest::Uninstall {
                target: input.target,
                expected_fingerprint: input.expected_fingerprint,
            }
        } else {
            CopilotApplicationHostRequest::Disconnect {
                target: input.target,
                expected_fingerprint: input.expected_fingerprint,
            }
        };
        let inspection = host(request).map_err(|e| host_error(&task_state, e))?;
        Ok::<_, V3ApiError>(payload(&task_state, inspection))
    })
    .await
    .map_err(|_| V3ApiError::internal_at(&state, "Copilot lifecycle task failed"))
    .and_then(|v| v);
    receipt.finish(result)
}
#[cfg(test)]
mod tests;

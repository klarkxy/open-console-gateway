//! BYOK control plane: same model publication, enabled Keys, CAS, and no secret responses.
use super::applications::{DashboardReceipt, opaque_subject};
use super::types::{ByokApplication, ByokConfigureRequest, ByokMutationRequest};
use crate::byok_application::ByokStatus;
use crate::log_types::{OperationMetadata, OperationOutcome};
use crate::{
    byok_application::{
        ByokClient, ByokError, ByokErrorKind, ByokHostRequest, ByokInspection, ByokModel,
        ByokSecret,
    },
    dashboard_v3::{
        ControlRevision, MutationExpectation, V3ApiError, check_expectation, parse_mutation_json,
    },
    model_metadata::ModelMetadata,
    state::CoreState,
};
use axum::{
    Json,
    body::Bytes,
    extract::{Path, Query, State},
};
use serde::Deserialize;

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct TargetQuery {
    target_path: Option<String>,
}

pub(super) async fn inspect(
    State(state): State<CoreState>,
    Path(client): Path<String>,
    Query(query): Query<TargetQuery>,
) -> Result<Json<ByokApplication>, V3ApiError> {
    let client = parse_client(&state, &client)?;
    let result = tokio::task::spawn_blocking(move || {
        let inspection = match state.byok_application_host() {
            Some(host) => host(ByokHostRequest::Inspect {
                client,
                target_path: clean_target(query.target_path),
            })
            .map_err(|e| host_error(&state, e))?,
            None => ByokInspection::unsupported(client),
        };
        let _guard = state.settings_update.lock();
        Ok::<_, V3ApiError>(payload_locked(&state, inspection))
    })
    .await
    .map_err(|_| V3ApiError::internal("BYOK inspection failed"))??;
    Ok(Json(result))
}

pub(super) async fn configure(
    State(state): State<CoreState>,
    Path(client): Path<String>,
    body: Bytes,
) -> Result<Json<ByokApplication>, V3ApiError> {
    let mut receipt = DashboardReceipt::open(
        &state,
        "application.configure",
        "application",
        opaque_subject(&client),
    );
    let (result, created) = configure_work(state, client, body).await;
    note_byok(&mut receipt, &result, created, ByokKind::Configure);
    receipt.finish(result).map(Json)
}

async fn configure_work(
    state: CoreState,
    client: String,
    body: Bytes,
) -> (Result<ByokApplication, V3ApiError>, Option<(String, u64)>) {
    let created = std::sync::Arc::new(std::sync::Mutex::new(None));
    let slot = created.clone();
    let joined = tokio::task::spawn_blocking(move || {
        let client = parse_client(&state, &client)?;
        let input = parse_mutation_json::<ByokConfigureRequest>(&body)?;
        let host = state.byok_application_host().ok_or_else(|| {
            V3ApiError::precondition_failed_at(
                &state,
                "Native application configuration is unavailable on this host",
            )
        })?;
        // Key creation and model publication use the ordinary control-plane boundary.
        let _guard = state.settings_update.lock();
        check_expectation(
            &state,
            &MutationExpectation {
                expected_revision: input.expected_revision,
                process_generation: input.process_generation,
            },
        )?;
        let models = available_models_locked(&state)?;
        if models.is_empty() {
            return Err(V3ApiError::precondition_failed_at(
                &state,
                "No models are published by this Key yet; configure a model source first",
            ));
        }
        let effect =
            super::applications::application_gateway_key_effect(&state, client.key_name())?;
        if let (Some(id), Some(revision)) = (effect.created_id, effect.revision) {
            *slot.lock().unwrap_or_else(|poison| poison.into_inner()) = Some((id, revision));
        }
        let inspection = host(ByokHostRequest::Configure {
            client,
            target_path: clean_target(input.target_path),
            expected_fingerprint: input.expected_fingerprint,
            gateway_v1_url: gateway_url(&state),
            secret: ByokSecret::new(effect.key),
            models,
            // The host selects the existing public default or first model while
            // holding its file lock and checking the inspected fingerprint.
            default_model_id: None,
            client_closed: input.client_closed,
        })
        .map_err(|e| host_error(&state, e))?;
        state.bump_settings_revision();
        Ok::<_, V3ApiError>(payload_locked(&state, inspection))
    })
    .await;
    let created = created
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .clone();
    let result = match joined {
        Ok(result) => result,
        Err(_) => Err(V3ApiError::internal("BYOK configuration failed")),
    };
    (result, created)
}

pub(super) async fn remove(
    State(state): State<CoreState>,
    Path(client): Path<String>,
    body: Bytes,
) -> Result<Json<ByokApplication>, V3ApiError> {
    record_without_key(state, client, body, false).await
}

pub(super) async fn recover(
    State(state): State<CoreState>,
    Path(client): Path<String>,
    body: Bytes,
) -> Result<Json<ByokApplication>, V3ApiError> {
    record_without_key(state, client, body, true).await
}

async fn record_without_key(
    state: CoreState,
    client: String,
    body: Bytes,
    recovery: bool,
) -> Result<Json<ByokApplication>, V3ApiError> {
    let mut receipt = DashboardReceipt::open(
        &state,
        if recovery {
            "application.recover"
        } else {
            "application.remove"
        },
        "application",
        opaque_subject(&client),
    );
    let result = mutate_without_key(state, client, body, recovery)
        .await
        .map(|Json(value)| value);
    note_byok(
        &mut receipt,
        &result,
        None,
        if recovery {
            ByokKind::Recover
        } else {
            ByokKind::Remove
        },
    );
    receipt.finish(result).map(Json)
}

enum ByokKind {
    Configure,
    Remove,
    Recover,
}

fn note_byok(
    receipt: &mut DashboardReceipt,
    result: &Result<ByokApplication, V3ApiError>,
    created: Option<(String, u64)>,
    kind: ByokKind,
) {
    match result {
        Ok(payload) => {
            let mut metadata = OperationMetadata {
                revision: Some(payload.revision.revision),
                ..OperationMetadata::default()
            };
            if let Some((id, _)) = &created {
                metadata.related_ids.push(id.clone());
            }
            if byok_failed(payload.inspection.status, &kind) && created.is_some() {
                metadata.requested_count = Some(2);
                metadata.completed_count = Some(1);
                metadata.failed_count = Some(1);
                receipt.decide(OperationOutcome::Partial, Some("business.failed"), metadata);
            } else if byok_failed(payload.inspection.status, &kind) {
                receipt.decide(OperationOutcome::Failed, Some("business.failed"), metadata);
            } else {
                if created.is_some() {
                    metadata.completed_count = Some(1);
                }
                receipt.succeed(metadata);
            }
        }
        Err(_) => {
            if let Some((id, revision)) = created {
                receipt.decide(
                    OperationOutcome::Partial,
                    None,
                    OperationMetadata {
                        revision: Some(revision),
                        requested_count: Some(2),
                        completed_count: Some(1),
                        failed_count: Some(1),
                        related_ids: vec![id],
                        ..OperationMetadata::default()
                    },
                );
            }
        }
    }
}

fn byok_failed(status: ByokStatus, kind: &ByokKind) -> bool {
    match kind {
        ByokKind::Configure | ByokKind::Recover => {
            !matches!(status, ByokStatus::Configured | ByokStatus::Ready)
        }
        ByokKind::Remove => !matches!(status, ByokStatus::NotDetected | ByokStatus::Ready),
    }
}

async fn mutate_without_key(
    state: CoreState,
    client: String,
    body: Bytes,
    recovery: bool,
) -> Result<Json<ByokApplication>, V3ApiError> {
    let client = parse_client(&state, &client)?;
    let input = parse_mutation_json::<ByokMutationRequest>(&body)?;
    let result = tokio::task::spawn_blocking(move || {
        let host = state.byok_application_host().ok_or_else(|| {
            V3ApiError::precondition_failed_at(
                &state,
                "Native application configuration is unavailable on this host",
            )
        })?;
        let _guard = state.settings_update.lock();
        check_expectation(
            &state,
            &MutationExpectation {
                expected_revision: input.expected_revision,
                process_generation: input.process_generation,
            },
        )?;
        let request = if recovery {
            ByokHostRequest::Recover {
                client,
                target_path: clean_target(input.target_path),
                expected_fingerprint: input.expected_fingerprint,
                client_closed: input.client_closed,
            }
        } else {
            ByokHostRequest::Remove {
                client,
                target_path: clean_target(input.target_path),
                expected_fingerprint: input.expected_fingerprint,
                client_closed: input.client_closed,
            }
        };
        let inspection = host(request).map_err(|e| host_error(&state, e))?;
        state.bump_settings_revision();
        Ok::<_, V3ApiError>(payload_locked(&state, inspection))
    })
    .await
    .map_err(|_| V3ApiError::internal("BYOK configuration operation failed"))??;
    Ok(Json(result))
}

fn parse_client(state: &CoreState, id: &str) -> Result<ByokClient, V3ApiError> {
    ByokClient::ALL
        .into_iter()
        .find(|c| c.id() == id)
        .ok_or_else(|| V3ApiError::invalid_request_at(state, "Unknown BYOK application"))
}
fn clean_target(target: Option<String>) -> Option<String> {
    target
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
}
fn gateway_url(state: &CoreState) -> String {
    let config = state.settings_config();
    let root = config.client_root_url.trim_end_matches('/');
    if root.is_empty() {
        format!("http://127.0.0.1:{}/v1", state.active_gateway_port())
    } else {
        format!("{root}/v1")
    }
}
fn available_models_locked(state: &CoreState) -> Result<Vec<ByokModel>, V3ApiError> {
    let rows = crate::gateway::handler::published_models_data_locked(state)
        .map_err(V3ApiError::internal)?;
    let fields = [
        "name",
        "contextWindow",
        "maxOutputTokens",
        "inputModalities",
        "outputModalities",
        "reasoning",
        "reasoningEfforts",
        "toolCalling",
        "parallelToolCalls",
    ];
    let mut models = Vec::new();
    for row in rows {
        let Some(id) = row.get("id").and_then(|v| v.as_str()) else {
            continue;
        };
        let metadata = row
            .get("ocg")
            .and_then(|v| v.as_object())
            .map(|object| {
                serde_json::Value::Object(
                    object
                        .iter()
                        .filter(|(key, _)| fields.contains(&key.as_str()))
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect(),
                )
            })
            .unwrap_or_else(|| serde_json::json!({}));
        let metadata: ModelMetadata = serde_json::from_value(metadata)
            .map_err(|_| V3ApiError::internal("Invalid published model metadata"))?;
        models.push(ByokModel {
            id: id.to_owned(),
            metadata,
        });
    }
    models.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(models)
}

fn payload_locked(state: &CoreState, inspection: ByokInspection) -> ByokApplication {
    ByokApplication {
        inspection,
        gateway_v1_url: gateway_url(state),
        revision: ControlRevision::from_state(state),
    }
}

fn host_error(state: &CoreState, error: ByokError) -> V3ApiError {
    match error.kind {
        ByokErrorKind::Invalid => V3ApiError::invalid_request_at(state, error.message),
        ByokErrorKind::Precondition => V3ApiError::precondition_failed_at(state, error.message),
        ByokErrorKind::Conflict => V3ApiError::conflict_at(state, error.message),
        ByokErrorKind::Internal => V3ApiError::internal_at(state, error.message),
    }
}

#[cfg(test)]
mod tests;

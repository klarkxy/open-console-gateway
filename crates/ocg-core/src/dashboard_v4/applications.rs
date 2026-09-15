//! DSH-specific application installation control plane.
//!
//! This is intentionally not a generic application/plugin registry. The
//! Desktop host owns all local filesystem and process effects; the HTTP layer
//! owns the dashboard session, CAS, and selection of an enabled Gateway Key.

use axum::Json;
use axum::body::Bytes;
use axum::extract::State;

use crate::dashboard_v3::{ControlRevision, V3ApiError, check_expectation, parse_mutation_json};
use crate::dsh_application::{
    DshApplicationError, DshApplicationErrorKind, DshApplicationHostRequest,
    DshApplicationInspection, DshApplicationPhase, DshGatewaySecret,
};
use crate::gateway_keys::PRIMARY_KEY_ID;
use crate::state::CoreState;

use super::types::{DshApplication, DshApplicationInstallRequest, DshApplicationStatus};

pub(super) async fn get_dsh(
    State(state): State<CoreState>,
) -> Result<Json<DshApplication>, V3ApiError> {
    let Some(host) = state.dsh_application_host() else {
        return Ok(Json(payload(
            &state,
            DshApplicationInspection::unsupported(),
        )));
    };
    let gateway_v1_url = {
        let _settings_update = state.settings_update.lock();
        gateway_v1_url(&state)
    };
    let inspection = tokio::task::spawn_blocking(move || {
        host(DshApplicationHostRequest::Inspect { gateway_v1_url })
    })
    .await
    .map_err(|error| V3ApiError::internal(format!("DSH inspection task failed: {error}")))?
    .map_err(|error| map_host_error(&state, error))?;
    Ok(Json(payload(&state, inspection)))
}

pub(super) async fn install_dsh(
    State(state): State<CoreState>,
    body: Bytes,
) -> Result<Json<DshApplication>, V3ApiError> {
    let input = parse_mutation_json::<DshApplicationInstallRequest>(&body)?;
    let host = state.dsh_application_host().ok_or_else(|| {
        V3ApiError::precondition_failed_at(
            &state,
            "DSH installation is unavailable in this build; use the Desktop app or a native CLI on the DSH host",
        )
    })?;

    let (gateway_v1_url, secret) = {
        let _settings_update = state.settings_update.lock();
        check_expectation(&state, &input.expectation)?;
        let secret = selected_gateway_key(&state, &input.key_id)?;
        (gateway_v1_url(&state), secret)
    };

    let request = DshApplicationHostRequest::Install {
        expected_fingerprint: input.expected_fingerprint,
        gateway_v1_url,
        secret: DshGatewaySecret::new(secret),
    };
    let inspection = tokio::task::spawn_blocking(move || host(request))
        .await
        .map_err(|error| V3ApiError::internal(format!("DSH installation task failed: {error}")))?
        .map_err(|error| map_host_error(&state, error))?;
    Ok(Json(payload(&state, inspection)))
}

fn gateway_v1_url(state: &CoreState) -> String {
    let settings = state.settings_config();
    let root = settings.client_root_url.trim_end_matches('/');
    if root.is_empty() {
        format!("http://127.0.0.1:{}/v1", state.active_gateway_port())
    } else {
        format!("{root}/v1")
    }
}

fn selected_gateway_key(state: &CoreState, key_id: &str) -> Result<String, V3ApiError> {
    if key_id == PRIMARY_KEY_ID {
        let key = state.config().gateway_key;
        if key.is_empty() {
            return Err(V3ApiError::precondition_failed_at(
                state,
                "the primary Key is unavailable",
            ));
        }
        return Ok(key);
    }
    let key = state
        .db
        .lock()
        .get_sub_gateway_key(key_id)
        .map_err(V3ApiError::internal)?
        .filter(|key| key.authenticates())
        .ok_or_else(|| {
            V3ApiError::precondition_failed_at(state, "the selected Key is missing or disabled")
        })?;
    Ok(key.key)
}

fn payload(state: &CoreState, inspection: DshApplicationInspection) -> DshApplication {
    DshApplication {
        status: match inspection.phase {
            DshApplicationPhase::UnsupportedRuntime => DshApplicationStatus::UnsupportedRuntime,
            DshApplicationPhase::NotDetected => DshApplicationStatus::NotDetected,
            DshApplicationPhase::Ready => DshApplicationStatus::Ready,
            DshApplicationPhase::Installed => DshApplicationStatus::Installed,
            DshApplicationPhase::Incompatible => DshApplicationStatus::Incompatible,
            DshApplicationPhase::Conflict => DshApplicationStatus::Conflict,
        },
        detected: inspection.detected,
        installed: inspection.installed,
        install_supported: inspection.install_supported,
        activation_required: inspection.activation_required,
        version: inspection.version,
        detail: inspection.detail,
        target_paths: inspection.target_paths,
        fingerprint: inspection.fingerprint,
        revision: ControlRevision::from_state(state),
    }
}

fn map_host_error(state: &CoreState, error: DshApplicationError) -> V3ApiError {
    match error.kind {
        DshApplicationErrorKind::Invalid => V3ApiError::invalid_request_at(state, error.message),
        DshApplicationErrorKind::Precondition => {
            V3ApiError::precondition_failed_at(state, error.message)
        }
        DshApplicationErrorKind::Conflict => V3ApiError::conflict_at(state, error.message),
        DshApplicationErrorKind::Internal => V3ApiError::internal(error.message),
    }
}

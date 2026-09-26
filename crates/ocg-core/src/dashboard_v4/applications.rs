//! DSH-specific application installation control plane.
//!
//! This is intentionally not a generic application/plugin registry. The
//! Desktop host owns all local filesystem and process effects; the HTTP layer
//! owns the dashboard session, CAS, and selection of an enabled Gateway Key.

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Query, State};
use serde::Deserialize;
use serde::de::DeserializeOwned;

use crate::dashboard_v3::{ControlRevision, V3ApiError, check_expectation, parse_mutation_json};
use crate::dsh_application::{
    DshApplicationError, DshApplicationErrorKind, DshApplicationHostRequest,
    DshApplicationInspection, DshApplicationOutcome as HostOutcome, DshApplicationPhase,
    DshGatewaySecret,
};
use crate::gateway_keys::PRIMARY_KEY_ID;
use crate::state::CoreState;

use super::types::{
    DshApplication, DshApplicationInstallRequest, DshApplicationOutcome, DshApplicationStatus,
    DshApplicationUninstallRequest, DshDiscoveredProfile,
};

/// Extra-field check without `flatten`, so serde `deny_unknown_fields` works.
/// `parse_mutation_json` then maps those failures to the shared invalid-JSON
/// envelope without widening V3 constructor visibility.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[allow(dead_code)]
struct DshInstallMutationCheck {
    expected_revision: u64,
    process_generation: u64,
    key_id: String,
    profile_path: Option<String>,
    runtime_url: Option<String>,
    expected_fingerprint: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[allow(dead_code)]
struct DshUninstallMutationCheck {
    expected_revision: u64,
    process_generation: u64,
    profile_path: Option<String>,
    runtime_url: Option<String>,
    expected_fingerprint: String,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct DshProfileQuery {
    profile_path: Option<String>,
    runtime_url: Option<String>,
}

pub(super) async fn get_dsh(
    State(state): State<CoreState>,
    Query(query): Query<DshProfileQuery>,
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
        host(DshApplicationHostRequest::Inspect {
            gateway_v1_url,
            profile_path: empty_to_none(query.profile_path),
            runtime_url: empty_to_none(query.runtime_url),
        })
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
    let input = parse_dsh_mutation::<DshInstallMutationCheck, DshApplicationInstallRequest>(&body)?;
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
        profile_path: empty_to_none(input.profile_path),
        runtime_url: empty_to_none(input.runtime_url),
        secret: DshGatewaySecret::new(secret),
    };
    let inspection = tokio::task::spawn_blocking(move || host(request))
        .await
        .map_err(|error| V3ApiError::internal(format!("DSH installation task failed: {error}")))?
        .map_err(|error| map_host_error(&state, error))?;
    Ok(Json(payload(&state, inspection)))
}

pub(super) async fn uninstall_dsh(
    State(state): State<CoreState>,
    body: Bytes,
) -> Result<Json<DshApplication>, V3ApiError> {
    let input =
        parse_dsh_mutation::<DshUninstallMutationCheck, DshApplicationUninstallRequest>(&body)?;
    let host = state.dsh_application_host().ok_or_else(|| {
        V3ApiError::precondition_failed_at(
            &state,
            "DSH uninstallation is unavailable in this build; use the Desktop app or a native CLI on the DSH host",
        )
    })?;

    let gateway_v1_url = {
        let _settings_update = state.settings_update.lock();
        check_expectation(&state, &input.expectation)?;
        gateway_v1_url(&state)
    };

    let request = DshApplicationHostRequest::Uninstall {
        expected_fingerprint: input.expected_fingerprint,
        gateway_v1_url,
        profile_path: empty_to_none(input.profile_path),
        runtime_url: empty_to_none(input.runtime_url),
    };
    let inspection = tokio::task::spawn_blocking(move || host(request))
        .await
        .map_err(|error| V3ApiError::internal(format!("DSH uninstallation task failed: {error}")))?
        .map_err(|error| map_host_error(&state, error))?;
    Ok(Json(payload(&state, inspection)))
}

fn parse_dsh_mutation<Check: DeserializeOwned, T: DeserializeOwned>(
    bytes: &[u8],
) -> Result<T, V3ApiError> {
    let _checked = parse_mutation_json::<Check>(bytes)?;
    parse_mutation_json::<T>(bytes)
}

fn empty_to_none(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_owned())
        }
    })
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
        selected_profile_path: inspection.selected_profile_path,
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
        discovered_profiles: inspection
            .discovered_profiles
            .into_iter()
            .map(|profile| DshDiscoveredProfile {
                home: profile.home,
                name: profile.name,
                path: profile.path,
            })
            .collect(),
        fingerprint: inspection.fingerprint,
        revision: ControlRevision::from_state(state),
        runtime_url: inspection.runtime_url,
        uninstall_supported: inspection.uninstall_supported,
        enabled: inspection.enabled,
        application: inspection.application.map(|outcome| match outcome {
            HostOutcome::Applied => DshApplicationOutcome::Applied,
            HostOutcome::RestartRequired => DshApplicationOutcome::RestartRequired,
            HostOutcome::Overridden => DshApplicationOutcome::Overridden,
            HostOutcome::Failed => DshApplicationOutcome::Failed,
            HostOutcome::Cancelled => DshApplicationOutcome::Cancelled,
        }),
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn install_and_uninstall_reject_unknown_fields() {
        let extra = json!({
            "expectedRevision": 1,
            "processGeneration": 1,
            "keyId": "primary",
            "expectedFingerprint": "abc",
            "extra": true
        });
        assert!(
            parse_dsh_mutation::<DshInstallMutationCheck, DshApplicationInstallRequest>(
                &serde_json::to_vec(&extra).unwrap()
            )
            .is_err()
        );
        let with_key = json!({
            "expectedRevision": 1,
            "processGeneration": 1,
            "expectedFingerprint": "abc",
            "keyId": "must-not-be-accepted"
        });
        assert!(
            parse_dsh_mutation::<DshUninstallMutationCheck, DshApplicationUninstallRequest>(
                &serde_json::to_vec(&with_key).unwrap()
            )
            .is_err()
        );
        let valid = json!({
            "expectedRevision": 1,
            "processGeneration": 1,
            "expectedFingerprint": "abc",
            "runtimeUrl": "http://127.0.0.1:3080"
        });
        assert!(
            parse_dsh_mutation::<DshUninstallMutationCheck, DshApplicationUninstallRequest>(
                &serde_json::to_vec(&valid).unwrap()
            )
            .is_ok()
        );
    }
}

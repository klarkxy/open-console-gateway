//! GET `/connection` — lightweight secret-bearing connection-center view.

use axum::Json;
use axum::extract::State;

use crate::state::CoreState;

use super::V3ApiError;
use super::types::{ConnectionInfo, ConnectionSubKey};

pub(super) async fn get_connection(
    State(state): State<CoreState>,
) -> Result<Json<ConnectionInfo>, V3ApiError> {
    let _settings_update = state.settings_update.lock();
    Ok(Json(connection_from_state(&state)?))
}

fn connection_from_state(state: &CoreState) -> Result<ConnectionInfo, V3ApiError> {
    let settings = state.settings_config();
    let sub_keys = state
        .db
        .lock()
        .list_active_sub_gateway_keys()
        .map_err(V3ApiError::internal)?
        .into_iter()
        .map(|key| ConnectionSubKey {
            id: key.id,
            name: key.name,
            enabled: key.enabled,
            value: key.key,
        })
        .collect();
    Ok(ConnectionInfo {
        gateway_port: settings.gateway_port,
        client_root_url: settings.client_root_url,
        upstream_base_url: settings.upstream_base_url,
        primary_key: settings.gateway_key,
        sub_keys,
        revision: state.settings_revision(),
        process_generation: state.process_generation(),
    })
}

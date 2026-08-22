//! Dashboard V3 HTTP contract kernel.
//!
//! Mounted at `/dashboard/api/v3` beside the unchanged V2 `/dashboard/api`
//! router. This module owns the shared DTO / error / CAS envelope, process
//! generation, connection/settings reads, and the settings write path.

mod connection;
mod settings;
mod types;

use axum::extract::{Request, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};

use crate::state::CoreState;

pub use types::{
    CATALOG_TYPE_NAMES, ConnectionInfo, ConnectionSubKey, ControlRevision, ERROR_INTERNAL,
    ERROR_INVALID_JSON, ERROR_INVALID_REQUEST, ERROR_MISSING_EXPECTED_REVISION,
    ERROR_REVISION_CONFLICT, ERROR_UNAUTHORIZED, MutationAck, MutationExpectation, PricingRevision,
    ProxyListDirection, ProxyMode, ProxySupportedModel, RoutingMode, Settings, SettingsUpdate,
    V3Error, contract_schema, contract_schema_pretty,
};

/// Must match `dashboard.rs` `SESSION_COOKIE`. V2 owns login; V3 only checks it.
const SESSION_COOKIE: &str = "ocg_dashboard_session";

pub fn api_router(state: CoreState) -> Router<CoreState> {
    Router::new()
        .route("/contract", get(get_contract))
        .route("/connection", get(connection::get_connection))
        .route(
            "/settings",
            get(settings::get_settings).put(settings::put_settings),
        )
        .route_layer(middleware::from_fn_with_state(state, require_v3_session))
}

struct V3ApiError {
    status: StatusCode,
    body: V3Error,
}

impl V3ApiError {
    fn unauthorized() -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            body: V3Error::unauthorized(),
        }
    }

    fn invalid_json() -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            body: V3Error::invalid_json(),
        }
    }

    fn missing_expected_revision() -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            body: V3Error::missing_expected_revision(),
        }
    }

    fn revision_conflict(state: &CoreState) -> Self {
        Self {
            status: StatusCode::CONFLICT,
            body: V3Error::revision_conflict(state.settings_revision(), state.process_generation()),
        }
    }

    fn invalid_request_at(state: &CoreState, message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            body: V3Error::invalid_request_at(
                message,
                state.settings_revision(),
                state.process_generation(),
            ),
        }
    }

    fn internal(message: impl std::fmt::Display) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            body: V3Error::internal(message.to_string()),
        }
    }
}

impl IntoResponse for V3ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(self.body)).into_response()
    }
}

async fn require_v3_session(State(state): State<CoreState>, req: Request, next: Next) -> Response {
    if is_local_dashboard_request(&state, req.headers())
        || has_dashboard_session(&state, req.headers())
    {
        next.run(req).await
    } else {
        V3ApiError::unauthorized().into_response()
    }
}

fn is_local_dashboard_request(state: &CoreState, headers: &HeaderMap) -> bool {
    state.dashboard_local_mode()
        && [
            "forwarded",
            "x-forwarded-for",
            "x-forwarded-proto",
            "x-real-ip",
        ]
        .iter()
        .all(|name| !headers.contains_key(*name))
}

fn has_dashboard_session(state: &CoreState, headers: &HeaderMap) -> bool {
    let current = state.dashboard_session_token.lock();
    dashboard_session_value(headers)
        .map(|value| value == current.as_str())
        .unwrap_or(false)
}

fn dashboard_session_value(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::COOKIE)
        .and_then(|value| value.to_str().ok())
        .and_then(|cookies| {
            cookies.split(';').find_map(|cookie| {
                let (name, value) = cookie.trim().split_once('=')?;
                (name == SESSION_COOKIE).then_some(value)
            })
        })
}

async fn get_contract(State(state): State<CoreState>) -> Json<ControlRevision> {
    Json(ControlRevision::from_state(&state))
}

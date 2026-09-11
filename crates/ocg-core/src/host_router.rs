//! Host HTTP router composition.
//!
//! Assembles the inference router with Dashboard V3/V4, the retired V2 REST
//! tombstone, public V2 auth, the V2 browser WebSocket, and dashboard assets.
//! This module is the HTTP composition root: it depends on `gateway`,
//! `dashboard`, `dashboard_v3`, and `dashboard_v4`. Those modules, and `state`,
//! must not import this module.

use crate::dashboard_session;
use crate::gateway::listener::GatewayRouterHost;
use crate::state::CoreState;
use axum::extract::{OriginalUri, Request, State};
use axum::http::StatusCode;
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde_json::json;

/// Structured code for the authenticated Dashboard V2 REST tombstone.
pub const DASHBOARD_V2_REMOVED_CODE: &str = "dashboardV2Removed";
/// Client-visible message for the authenticated Dashboard V2 REST tombstone.
pub const DASHBOARD_V2_REMOVED_MESSAGE: &str =
    "Dashboard API V2 has been removed; refresh the page and retry.";

/// Authenticated HTTP 410 body for retired `/dashboard/api` REST paths.
pub fn v2_removed_response() -> Response {
    (
        StatusCode::GONE,
        Json(json!({
            "code": DASHBOARD_V2_REMOVED_CODE,
            "message": DASHBOARD_V2_REMOVED_MESSAGE })),
    )
        .into_response()
}

pub fn build_router(state: CoreState) -> Router {
    Router::new()
        .merge(crate::gateway::inference_router(state.clone()))
        .nest(
            "/dashboard/api/v3",
            crate::dashboard_v3::api_router(state.clone()),
        )
        .nest(
            "/dashboard/api/v4",
            crate::dashboard_v4::api_router(state.clone()),
        )
        .nest(
            "/dashboard/api",
            crate::dashboard::api_router(state.clone())
                // Capture unknown `/dashboard/api/...` paths so the tombstone
                // middleware runs; Axum nest otherwise 404s before the layer.
                .fallback(unmatched_legacy_v2_rest)
                .layer(middleware::from_fn_with_state(
                    state.clone(),
                    retire_legacy_v2_rest,
                )),
        )
        .route("/dashboard", get(crate::dashboard::serve_index))
        .route("/dashboard/", get(crate::dashboard::serve_index))
        .route(
            "/dashboard/assets/{*path}",
            get(crate::dashboard::serve_asset),
        )
        .layer(middleware::from_fn_with_state(
            state.clone(),
            require_local_dashboard_authority,
        ))
        .with_state(state)
}

// Cover public auth as well as protected APIs: otherwise DNS rebinding can
// register the first administrator and obtain a session before reading Keys.
async fn require_local_dashboard_authority(
    State(state): State<CoreState>,
    req: Request,
    next: Next,
) -> Response {
    let uri = req
        .extensions()
        .get::<OriginalUri>()
        .map(|original| &original.0)
        .unwrap_or_else(|| req.uri());
    let path = uri.path();
    if state.dashboard_local_mode()
        && (path == "/dashboard/api" || path.starts_with("/dashboard/api/"))
        && !dashboard_session::has_local_dashboard_authority(req.headers())
    {
        if path == "/dashboard/api/v3"
            || path.starts_with("/dashboard/api/v3/")
            || path == "/dashboard/api/v4"
            || path.starts_with("/dashboard/api/v4/")
        {
            return (
                StatusCode::FORBIDDEN,
                Json(crate::dashboard_v3::V3Error::forbidden(
                    "Dashboard Host is not loopback, or Origin does not match Host",
                    state.settings_revision(),
                    state.process_generation(),
                )),
            )
                .into_response();
        }
        return StatusCode::FORBIDDEN.into_response();
    }
    next.run(req).await
}

impl GatewayRouterHost for CoreState {
    /// Axum assembly used by the listener. Defined here so `gateway` does not
    /// import dashboard mounts.
    fn compose_router(state: CoreState) -> Router {
        build_router(state)
    }
}

async fn unmatched_legacy_v2_rest() -> StatusCode {
    StatusCode::NOT_FOUND
}

/// Auth runs before the tombstone: anonymous retired REST stays 401; a valid
/// dashboard session (including loopback local mode) receives 410. Preserved
/// V2 families — auth, browser WebSocket — fall through to their handlers.
async fn retire_legacy_v2_rest(
    State(state): State<CoreState>,
    req: Request,
    next: Next,
) -> Response {
    let path = request_path(&req);
    if !is_retired_legacy_v2_rest_path(&path) {
        return next.run(req).await;
    }
    let authorized = {
        let current = state.dashboard_session_token.lock();
        dashboard_session::is_authorized(
            state.dashboard_local_mode(),
            current.as_str(),
            req.headers(),
        )
    };
    if authorized {
        v2_removed_response()
    } else {
        StatusCode::UNAUTHORIZED.into_response()
    }
}

fn request_path(req: &Request) -> String {
    req.extensions()
        .get::<OriginalUri>()
        .map(|original| original.0.path().to_string())
        .unwrap_or_else(|| {
            let path = req.uri().path();
            if path == "/dashboard/api" || path.starts_with("/dashboard/api/") {
                path.to_string()
            } else {
                format!("/dashboard/api{path}")
            }
        })
}

/// Retired protected V2 REST, including unknown `/dashboard/api/...` paths.
/// Auth, V3, browser WS, dashboard assets, and inference are not retired.
pub(crate) fn is_retired_legacy_v2_rest_path(path: &str) -> bool {
    match v2_api_remainder(path) {
        Some(rest) => !is_preserved_legacy_v2_path(rest),
        None => false,
    }
}

fn v2_api_remainder(path: &str) -> Option<&str> {
    const PREFIX: &str = "/dashboard/api";
    let rest = if path == PREFIX {
        ""
    } else {
        path.strip_prefix("/dashboard/api/")?
    };
    if rest == "v3" || rest.starts_with("v3/") || rest == "v4" || rest.starts_with("v4/") {
        return None;
    }
    Some(rest)
}

fn is_preserved_legacy_v2_path(rest: &str) -> bool {
    matches!(
        rest,
        "auth/status" | "auth/register" | "auth/login" | "auth/logout"
    ) || is_browser_session_ws(rest)
}

fn is_browser_session_ws(rest: &str) -> bool {
    let Some(after) = rest.strip_prefix("browser/sessions/") else {
        return false;
    };
    matches!(
        after.split_once('/'),
        Some((token, "ws")) if !token.is_empty() && !token.contains('/')
    )
}

#[cfg(test)]
mod tests {
    use super::is_retired_legacy_v2_rest_path;

    #[test]
    fn classifies_retired_and_preserved_paths() {
        for path in [
            "/dashboard/api",
            "/dashboard/api/",
            "/dashboard/api/settings",
            "/dashboard/api/accounts",
            "/dashboard/api/accounts/abc/verify",
            "/dashboard/api/providers/catalog",
            "/dashboard/api/browser/capabilities",
            "/dashboard/api/browser/sessions/tok",
            "/dashboard/api/does-not-exist",
        ] {
            assert!(
                is_retired_legacy_v2_rest_path(path),
                "{path} should be retired V2 REST"
            );
        }
        for path in [
            "/dashboard/api/auth/status",
            "/dashboard/api/auth/register",
            "/dashboard/api/auth/login",
            "/dashboard/api/auth/logout",
            "/dashboard/api/browser/sessions/opaque-token/ws",
            "/dashboard/api/v3",
            "/dashboard/api/v3/contract",
            "/dashboard/api/v3/accounts",
            "/dashboard/api/v4",
            "/dashboard/api/v4/contract",
            "/dashboard/api/v4/connections",
            "/dashboard/api/v4/onboarding/commit",
            "/dashboard",
            "/dashboard/",
            "/dashboard/assets/index.js",
            "/v1/models",
            "/v1/chat/completions",
            "/v1beta/models/m:generateContent",
            "/claude-desktop/v1/models",
        ] {
            assert!(
                !is_retired_legacy_v2_rest_path(path),
                "{path} must stay out of the V2 REST tombstone"
            );
        }
    }

    #[test]
    fn similar_looking_paths_cannot_bypass_the_tombstone() {
        for path in [
            "/dashboard/api/auth",
            "/dashboard/api/auth/",
            "/dashboard/api/auth/status/extra",
            "/dashboard/api/auth/register/extra",
            "/dashboard/api/auth/login/now",
            "/dashboard/api/auth/logout/now",
            "/dashboard/api/auth/status/",
            "/dashboard/api/auth/status//",
            "/dashboard/api/auth//status",
            "/dashboard/api/auth/statusx",
            "/dashboard/api/authentication/status",
            "/dashboard/api/v2/auth/status",
            "/dashboard/api/browser/sessions//ws",
            "/dashboard/api/browser/sessions/tok",
            "/dashboard/api/browser/sessions/tok/websocket",
            "/dashboard/api/browser/sessions/tok/ws/extra",
            "/dashboard/api/browser/sessions/tok/ws/",
            "/dashboard/api/browser/sessions/tok//ws",
            "/dashboard/api/browser/sessions/tok/ws/../ws",
            "/dashboard/api/browser/session/tok/ws",
            "/dashboard/api/browser/sessions/tok/ws/extra/",
            "/dashboard/api/v3accounts",
            "/dashboard/api/V3/accounts",
            "/dashboard/api/v3-contract",
            "/dashboard/api/v4connections",
            "/dashboard/api/V4/connections",
            "/dashboard/api/v4-contract",
        ] {
            assert!(
                is_retired_legacy_v2_rest_path(path),
                "{path} must not bypass the V2 REST tombstone"
            );
        }
    }

    #[test]
    fn only_exact_auth_and_nonempty_browser_ws_are_preserved() {
        for path in [
            "/dashboard/api/auth/status",
            "/dashboard/api/auth/register",
            "/dashboard/api/auth/login",
            "/dashboard/api/auth/logout",
            "/dashboard/api/browser/sessions/opaque-token/ws",
            "/dashboard/api/browser/sessions/a/ws",
        ] {
            assert!(
                !is_retired_legacy_v2_rest_path(path),
                "{path} is an exact preserved V2 family"
            );
        }
        for path in [
            "/dashboard/api/auth/status/",
            "/dashboard/api/auth/logout/",
            "/dashboard/api/auth/status/extra",
            "/dashboard/api/browser/sessions//ws",
            "/dashboard/api/browser/sessions/opaque-token/ws/",
            "/dashboard/api/browser/sessions/tok/ws/extra",
        ] {
            assert!(
                is_retired_legacy_v2_rest_path(path),
                "{path} is not an exact preserved V2 family"
            );
        }
    }

    #[test]
    fn v3_inference_and_static_paths_stay_outside_the_nested_tombstone() {
        for path in [
            "/dashboard/api/v3",
            "/dashboard/api/v3/",
            "/dashboard/api/v3/contract",
            "/dashboard/api/v3/accounts",
            "/dashboard/api/v4",
            "/dashboard/api/v4/",
            "/dashboard/api/v4/contract",
            "/dashboard/api/v4/connections",
            "/dashboard/api/v4/onboarding/commit",
            "/dashboard/api/v3/auth/status",
            "/dashboard/api/v3/browser/sessions/tok/ws",
            "/dashboard/api/v3/settings",
            "/v3/contract",
            "/v1/models",
            "/v1/chat/completions",
            "/v1/responses",
            "/v1beta/models/m:generateContent",
            "/v1/models/m:generateContent",
            "/claude-desktop/v1/models",
            "/claude-desktop/v1/messages",
            "/dashboard",
            "/dashboard/",
            "/dashboard/assets/index.js",
            "/dashboard/assets/app.css",
        ] {
            assert!(
                !is_retired_legacy_v2_rest_path(path),
                "{path} must stay outside the nested V2 REST tombstone"
            );
        }
    }
}

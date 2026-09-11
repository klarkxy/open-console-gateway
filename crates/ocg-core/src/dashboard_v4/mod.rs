//! Dashboard V4 HTTP contract kernel.
//!
//! Mounted at `/dashboard/api/v4` beside V3. This slice is a parallel
//! additive control plane: read-only connection/template projections plus
//! CAS-protected onboarding and credential-rotate writes. It reuses V3
//! session middleware and the V3 error envelope. Handlers must not issue
//! outbound network requests.

mod connections;
mod credentials;
mod identities;
mod onboarding;
mod templates;
mod types;

use axum::extract::State;
use axum::middleware;
use axum::routing::{get, post};
use axum::{Json, Router};

use crate::dashboard_v3::{ControlRevision, require_v3_session};
use crate::state::CoreState;

pub use types::{
    CATALOG_TYPE_NAMES, ConnectionList, ConnectionSummary, CredentialRotateRequest,
    CredentialRotateResult, IdentityList, IdentitySummary, OnboardingAuthorization,
    OnboardingCommitRequest, OnboardingCommitResult, OnboardingConnection, OnboardingTarget,
    ProviderTemplate, TemplateList, contract_schema, contract_schema_pretty,
};

pub fn api_router(state: CoreState) -> Router<CoreState> {
    Router::new()
        .route("/contract", get(get_contract))
        .route("/templates", get(templates::list_templates))
        .route("/connections", get(connections::list_connections))
        .route("/accounts", get(identities::list_accounts))
        .route("/onboarding/commit", post(onboarding::commit))
        .route("/credentials/{id}/rotate", post(credentials::rotate))
        .route_layer(middleware::from_fn_with_state(state, require_v3_session))
}

async fn get_contract(State(state): State<CoreState>) -> Json<ControlRevision> {
    Json(ControlRevision::from_state(&state))
}

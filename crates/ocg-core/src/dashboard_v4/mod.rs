//! Dashboard V4 HTTP contract kernel.
//!
//! Mounted at `/dashboard/api/v4` beside V3. This slice is a parallel
//! additive control plane: read-only connection/template projections plus
//! CAS-protected onboarding, binding, credential, local CPA catalog, and
//! built-in Provider catalog writes. It reuses V3 session middleware and
//! the V3 error envelope. Handlers must not issue outbound network requests.

mod bindings;
mod catalog;
mod connections;
mod cpa;
mod credentials;
mod identities;
mod onboarding;
mod templates;
mod types;

use axum::extract::State;
use axum::middleware;
use axum::routing::{get, patch, post};
use axum::{Json, Router};

use crate::dashboard_v3::{ControlRevision, require_v3_session};
use crate::state::CoreState;

pub use types::{
    CATALOG_TYPE_NAMES, ConnectionList, ConnectionSummary, CpaCatalog, CpaCatalogUpdate,
    CredentialRotateRequest, CredentialRotateResult, IdentityList, IdentitySummary,
    OnboardingAuthorization, OnboardingCommitRequest, OnboardingCommitResult, OnboardingConnection,
    OnboardingTarget, ProviderTemplate, TemplateList, contract_schema, contract_schema_pretty,
};

pub fn api_router(state: CoreState) -> Router<CoreState> {
    Router::new()
        .route("/contract", get(get_contract))
        .route("/templates", get(templates::list_templates))
        .route("/connections", get(connections::list_connections))
        .route("/accounts", get(identities::list_accounts))
        .route("/onboarding/commit", post(onboarding::commit))
        .route("/credentials/{id}/rotate", post(credentials::rotate))
        .route("/bindings/{id}", patch(bindings::patch))
        .route(
            "/identities/{id}/credentials",
            post(identities::create_credential),
        )
        .route("/cpa/models", get(cpa::get_models).put(cpa::put_models))
        .route(
            "/provider-contracts/{scope_kind}/{scope_id}/catalog/remove",
            post(catalog::remove_models),
        )
        .route_layer(middleware::from_fn_with_state(state, require_v3_session))
}

async fn get_contract(State(state): State<CoreState>) -> Json<ControlRevision> {
    Json(ControlRevision::from_state(&state))
}

//! Dashboard V4 HTTP contract kernel.
//!
//! Mounted at `/dashboard/api/v4`. This slice owns the additive control
//! plane (read-only connection/template projections plus CAS-protected
//! onboarding, binding, credential, local CPA catalog, built-in Provider
//! catalog writes, alias publication, and New API Key import) and remounts
//! the operational V3 handlers on the same prefix. `GET /accounts` stays the
//! identity listing; the remounted V3 account-list shim is `GET
//! /account-records`. `GET /contract` is the V4-native ControlRevision.
//! It reuses V3 session middleware and the V3 error envelope.
//! Handlers do not issue outbound network requests except Key import and
//! the explicit official-API balance/price refreshes.

mod applications;
mod bindings;
mod catalog;
mod connections;
mod cpa;
mod credentials;
mod destinations;
mod identities;
mod official_api;
mod onboarding;
mod platform_keys;
mod publication;
mod routing;
mod templates;
pub(crate) mod types;

use axum::extract::State;
use axum::middleware;
use axum::routing::{get, patch, post};
use axum::{Json, Router};

use crate::dashboard_v3::{ControlRevision, require_v3_session};
use crate::state::CoreState;

pub use types::{
    CATALOG_TYPE_NAMES, ConnectionList, ConnectionSummary, CpaCatalog, CpaCatalogUpdate,
    CredentialList, CredentialRotateRequest, CredentialRotateResult, DestinationCredentialDto,
    DestinationDto, DestinationList, DshApplication, DshApplicationInstallRequest,
    DshApplicationStatus, IdentityList, IdentitySummary, OnboardingAuthorization,
    OnboardingCommitRequest, OnboardingCommitResult, OnboardingConnection, OnboardingTarget,
    PlatformKeyImportFailure, PlatformKeyImportRequest, PlatformKeyImportResult, ProviderTemplate,
    RoutingExplanation, TemplateList, contract_schema, contract_schema_pretty,
};

pub fn api_router(state: CoreState) -> Router<CoreState> {
    let v4_native = Router::new()
        .route("/contract", get(get_contract))
        .route("/templates", get(templates::list_templates))
        .route("/connections", get(connections::list_connections))
        .route("/accounts", get(identities::list_accounts))
        .route("/destinations", get(destinations::list_destinations))
        .route(
            "/destinations/{id}",
            patch(destinations::patch_destination).delete(destinations::delete_destination),
        )
        .route("/credentials", get(destinations::list_credentials))
        .route("/accounts/{id}/official-api", get(official_api::get_status))
        .route(
            "/accounts/{id}/official-api/balance",
            post(official_api::refresh_balance),
        )
        .route(
            "/providers/{id}/official-api/pricing",
            get(official_api::get_prices).post(official_api::refresh_prices),
        )
        .route(
            "/applications/dsh",
            get(applications::get_dsh).post(applications::install_dsh),
        )
        .route("/onboarding/commit", post(onboarding::commit))
        .route(
            "/platform-accounts/{id}/import-keys",
            post(platform_keys::import_keys),
        )
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
        .route(
            "/alias-publication",
            get(publication::get_publication).patch(publication::patch_publication),
        )
        .route("/routing/explain", get(routing::explain))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            require_v3_session,
        ));

    // V3 no longer registers GET /accounts or GET /contract, so these V4-native
    // routes stay authoritative after merge.
    v4_native.merge(crate::dashboard_v3::api_router(state))
}

async fn get_contract(State(state): State<CoreState>) -> Json<ControlRevision> {
    Json(ControlRevision::from_state(&state))
}

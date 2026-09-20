//! Read-only V4 destination and credential listings.
//!
//! Served from the stage-4a [`crate::destination_projection`] mapper. Handlers
//! take the settings lock, read SQLite only, and never issue outbound HTTP or
//! write. Mapping totality is still live [`project`]; a populated v50 shadow
//! is the served snapshot, and an empty shadow falls back to live. Projection
//! refusals are a structured 409.

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use ocg_domain::destination::{
    AdapterKind, AuthScheme, Capabilities, CatalogModel, Cooldowns, Credential, Destination,
    ExpiryCadence, Grants, LegacyDestinationRef, MappingError, ModelResolution, OnboardingTaskRef,
    Plan, PlanWindow, PlanWindowKind, PricingSource, RedirectPolicy, UsageSource,
};
use ocg_domain::dynamic::{
    DynamicAuthKind, DynamicModelMapping, DynamicModelUpstreamOverride, DynamicProviderDefinition,
};

use crate::dashboard_v3::{
    ControlRevision, MutationExpectation, V3ApiError, check_expectation, parse_mutation_json,
};
use crate::destination_projection::{ProjectionRefusal, RefusedRow, read_v4_projection};
use crate::dynamic::{DynamicProviderRuntime, validate_definition};
use crate::state::CoreState;

use super::types::{
    AdapterKindDto, AuthSchemeDto, CapabilitiesDto, CatalogModelDto, CredentialCooldownsDto,
    CredentialGrantsDto, CredentialList, DestinationCredentialDto as CredentialDto,
    DestinationDeleteResult, DestinationDto, DestinationList, DestinationModelPatch,
    DestinationOnboardingTaskDto, DestinationPatchRequest, DestinationPatchResult,
    DestinationProjectionRefusalDto, DestinationProjectionRefusedError, ExpiryCadenceDto,
    LegacyDestinationKindDto, LegacyDestinationRefDto, MappingErrorCodeDto, ModelResolutionDto,
    PlanDto, PlanWindowDto, PlanWindowKindDto, PricingSourceDto, ProtocolDto, RedirectPolicyDto,
    RefusedRowDto, RefusedRowKindDto, UsageSourceDto,
};

/// Stable 409 code when the stage-4a projection cannot map every live row.
pub const ERROR_DESTINATION_PROJECTION_REFUSED: &str = "destinationProjectionRefused";

pub(super) enum DestinationsError {
    Api(V3ApiError),
    Refused(DestinationProjectionRefusedError),
}

impl From<V3ApiError> for DestinationsError {
    fn from(error: V3ApiError) -> Self {
        Self::Api(error)
    }
}

impl IntoResponse for DestinationsError {
    fn into_response(self) -> Response {
        match self {
            Self::Api(error) => error.into_response(),
            Self::Refused(body) => (StatusCode::CONFLICT, Json(body)).into_response(),
        }
    }
}

pub(super) async fn list_destinations(
    State(state): State<CoreState>,
) -> Result<Json<DestinationList>, DestinationsError> {
    let (projection, revision) = load_projection(&state)?;
    Ok(Json(DestinationList {
        revision,
        destinations: projection
            .destinations
            .iter()
            .map(DestinationDto::from)
            .collect(),
    }))
}

pub(super) async fn list_credentials(
    State(state): State<CoreState>,
) -> Result<Json<CredentialList>, DestinationsError> {
    let (projection, revision) = load_projection(&state)?;
    Ok(Json(CredentialList {
        revision,
        credentials: projection
            .credentials
            .iter()
            .map(CredentialDto::from)
            .collect(),
    }))
}

pub(super) async fn patch_destination(
    State(state): State<CoreState>,
    Path(id): Path<String>,
    body: Bytes,
) -> Result<Json<DestinationPatchResult>, DestinationsError> {
    let input = parse_mutation_json::<DestinationPatchRequest>(&body)?;
    patch_destination_locked(&state, &id, input).map(Json)
}

pub(super) async fn delete_destination(
    State(state): State<CoreState>,
    Path(id): Path<String>,
    body: Bytes,
) -> Result<Json<DestinationDeleteResult>, DestinationsError> {
    let expectation = parse_mutation_json::<MutationExpectation>(&body)?;
    delete_destination_locked(&state, &id, expectation).map(Json)
}

fn patch_destination_locked(
    state: &CoreState,
    destination_id: &str,
    input: DestinationPatchRequest,
) -> Result<DestinationPatchResult, DestinationsError> {
    let _settings_update = state.settings_update.lock();
    check_expectation(state, &input.expectation)?;
    let destination = load_destination(state, destination_id)?;
    let legacy = destination.legacy.clone();
    let definition = destination_definition(&destination, &input)
        .map_err(|message| V3ApiError::invalid_request_at(state, message))?;

    match &legacy {
        LegacyDestinationRef::Dynamic(provider_id) => {
            let existing = state
                .db
                .lock()
                .get_dynamic_provider(provider_id)
                .map_err(V3ApiError::internal)?
                .ok_or_else(|| V3ApiError::not_found_at(state, "destination not found"))?;
            let account_count = state
                .db
                .lock()
                .count_accounts_for_provider(provider_id)
                .map_err(V3ApiError::internal)?;
            if definition.auth_kind.is_singleton() && account_count > 1 {
                return Err(V3ApiError::invalid_request_at(
                    state,
                    "no-auth destinations require at most one credential",
                )
                .into());
            }
            let now = chrono::Utc::now();
            let runtime = DynamicProviderRuntime {
                preset_id: existing.preset_id,
                id: existing.id,
                name: definition.name,
                endpoint_url: definition.endpoint_url,
                upstream_protocol: definition.upstream_protocol,
                auth_kind: definition.auth_kind,
                mappings: definition.mappings,
                created_at: existing.created_at,
                updated_at: now,
                origin: existing.origin,
                offering: existing.offering,
            };
            let substantive = existing.endpoint_url != runtime.endpoint_url
                || existing.upstream_protocol != runtime.upstream_protocol
                || existing.auth_kind != runtime.auth_kind
                || existing.mappings != runtime.mappings;
            let changing_to_none =
                !existing.auth_kind.is_singleton() && runtime.auth_kind.is_singleton();
            let snapshot = state
                .db
                .lock()
                .replace_dynamic_provider_authorized(
                    &runtime,
                    substantive,
                    changing_to_none,
                    None,
                    &input.authorize_credential_ids,
                )
                .map_err(|error| V3ApiError::invalid_request_at(state, error.to_string()))?;
            state
                .install_dynamic_providers_snapshot(snapshot)
                .map_err(V3ApiError::internal)?;
        }
        LegacyDestinationRef::CustomAccount(_) => {
            state
                .db
                .lock()
                .replace_custom_destination(
                    destination_id,
                    &definition,
                    &input.authorize_credential_ids,
                )
                .map_err(|error| V3ApiError::invalid_request_at(state, error.to_string()))?;
            state.bump_settings_revision();
            state
                .reload_provider_contracts()
                .map_err(V3ApiError::internal)?;
        }
        LegacyDestinationRef::Builtin(_) | LegacyDestinationRef::PlatformParent(_) => {
            return Err(V3ApiError::invalid_request_at(
                state,
                "sealed and platform-managed destinations are immutable",
            )
            .into());
        }
    }

    // `patch_destination_locked` already owns `settings_update`; do not call
    // `load_projection`, which would try to acquire the non-reentrant lock.
    let projection = {
        let db = state.db.lock();
        read_v4_projection(&db).map_err(V3ApiError::internal)?
    }
    .map_err(|refusals| DestinationsError::Refused(projection_refused(state, &refusals)))?;
    let updated = projection
        .destinations
        .iter()
        .find(|destination| destination.id == destination_id)
        .ok_or_else(|| V3ApiError::not_found_at(state, "destination not found"))?;
    Ok(DestinationPatchResult {
        revision: ControlRevision::from_state(state),
        destination: DestinationDto::from(updated),
        credentials: projection
            .credentials
            .iter()
            .map(CredentialDto::from)
            .collect(),
    })
}

fn delete_destination_locked(
    state: &CoreState,
    destination_id: &str,
    expectation: MutationExpectation,
) -> Result<DestinationDeleteResult, DestinationsError> {
    let _settings_update = state.settings_update.lock();
    check_expectation(state, &expectation)?;
    let destination = load_destination(state, destination_id)?;
    match destination.legacy {
        LegacyDestinationRef::Dynamic(provider_id) => {
            let snapshot = state
                .db
                .lock()
                .delete_dynamic_provider(&provider_id)
                .map_err(|error| V3ApiError::invalid_request_at(state, error.to_string()))?;
            state
                .install_dynamic_providers_snapshot(snapshot)
                .map_err(V3ApiError::internal)?;
        }
        LegacyDestinationRef::CustomAccount(_) => {
            state
                .db
                .lock()
                .delete_empty_custom_destination(destination_id)
                .map_err(|error| V3ApiError::invalid_request_at(state, error.to_string()))?;
            state.bump_settings_revision();
            state
                .reload_provider_contracts()
                .map_err(V3ApiError::internal)?;
        }
        LegacyDestinationRef::Builtin(_) | LegacyDestinationRef::PlatformParent(_) => {
            return Err(V3ApiError::invalid_request_at(
                state,
                "sealed and platform-managed destinations cannot be deleted",
            )
            .into());
        }
    }
    Ok(DestinationDeleteResult {
        revision: ControlRevision::from_state(state),
    })
}

fn load_destination(state: &CoreState, id: &str) -> Result<Destination, DestinationsError> {
    let projection = {
        let db = state.db.lock();
        read_v4_projection(&db).map_err(V3ApiError::internal)?
    };
    let projection = projection
        .map_err(|refusals| DestinationsError::Refused(projection_refused(state, &refusals)))?;
    projection
        .destinations
        .into_iter()
        .find(|destination| destination.id == id)
        .ok_or_else(|| V3ApiError::not_found_at(state, "destination not found").into())
}

fn destination_definition(
    destination: &Destination,
    input: &DestinationPatchRequest,
) -> Result<DynamicProviderDefinition, String> {
    if destination.adapter != AdapterKind::Http {
        return Err("sealed destination adapters are immutable".to_string());
    }
    let endpoint_url = crate::custom::validate_custom_endpoint_url(&input.endpoint_url)
        .map_err(|error| error.to_string())?;
    let auth_kind = match input.auth_scheme {
        AuthSchemeDto::Bearer => DynamicAuthKind::Bearer,
        AuthSchemeDto::XApiKey => DynamicAuthKind::XApiKey,
        AuthSchemeDto::None => DynamicAuthKind::None,
    };
    let id = match &destination.legacy {
        LegacyDestinationRef::Dynamic(id) | LegacyDestinationRef::CustomAccount(id) => id.clone(),
        LegacyDestinationRef::Builtin(_) | LegacyDestinationRef::PlatformParent(_) => {
            return Err("sealed and platform-managed destinations are immutable".to_string());
        }
    };
    let definition = DynamicProviderDefinition {
        preset_id: None,
        id,
        name: input.name.clone(),
        endpoint_url,
        upstream_protocol: input.upstream_protocol.into(),
        auth_kind,
        mappings: input.models.iter().cloned().map(model_patch).collect(),
    };
    validate_definition(definition).map_err(|error| error.to_string())
}

fn model_patch(model: DestinationModelPatch) -> DynamicModelMapping {
    DynamicModelMapping {
        public_model: model.public_model,
        upstream_model: model.upstream_model,
        upstream_override: model
            .upstream_override
            .map(|value| DynamicModelUpstreamOverride {
                protocol: value.protocol.into(),
                endpoint_url: value.endpoint_url,
            }),
    }
}

fn load_projection(
    state: &CoreState,
) -> Result<
    (
        crate::destination_projection::DestinationProjection,
        ControlRevision,
    ),
    DestinationsError,
> {
    let _settings_update = state.settings_update.lock();
    let result = {
        let db = state.db.lock();
        read_v4_projection(&db).map_err(V3ApiError::internal)?
    };
    match result {
        Ok(projection) => Ok((projection, ControlRevision::from_state(state))),
        Err(refusals) => Err(DestinationsError::Refused(projection_refused(
            state, &refusals,
        ))),
    }
}

/// Structured 409 reused by both destination and credential reads.
fn projection_refused(
    state: &CoreState,
    refusals: &[ProjectionRefusal],
) -> DestinationProjectionRefusedError {
    DestinationProjectionRefusedError {
        code: ERROR_DESTINATION_PROJECTION_REFUSED.to_string(),
        message: "destination projection refused".to_string(),
        current_revision: Some(state.settings_revision()),
        process_generation: Some(state.process_generation()),
        details: refusals
            .iter()
            .map(DestinationProjectionRefusalDto::from)
            .collect(),
    }
}

impl From<&Destination> for DestinationDto {
    fn from(destination: &Destination) -> Self {
        Self {
            id: destination.id.clone(),
            legacy: LegacyDestinationRefDto::from(&destination.legacy),
            adapter: destination.adapter.into(),
            name: destination.name.clone(),
            brand_family: destination.brand_family.clone(),
            base_url: destination.base_url.clone(),
            protocols: destination
                .protocols
                .iter()
                .copied()
                .map(ProtocolDto::from)
                .collect(),
            auth_scheme: destination.auth_scheme.into(),
            model_resolution: destination.model_resolution.into(),
            catalog: destination
                .catalog
                .iter()
                .map(CatalogModelDto::from)
                .collect(),
            capabilities: CapabilitiesDto::from(&destination.capabilities),
            plan: destination.plan.as_ref().map(PlanDto::from),
            max_credentials: destination.max_credentials,
            observer_credential_id: destination.observer_credential_id.clone(),
            enabled: destination.enabled,
        }
    }
}

impl From<&LegacyDestinationRef> for LegacyDestinationRefDto {
    fn from(value: &LegacyDestinationRef) -> Self {
        let (kind, id) = match value {
            LegacyDestinationRef::Builtin(id) => (LegacyDestinationKindDto::Builtin, id),
            LegacyDestinationRef::Dynamic(id) => (LegacyDestinationKindDto::Dynamic, id),
            LegacyDestinationRef::CustomAccount(id) => {
                (LegacyDestinationKindDto::CustomAccount, id)
            }
            LegacyDestinationRef::PlatformParent(id) => {
                (LegacyDestinationKindDto::PlatformParent, id)
            }
        };
        Self {
            kind,
            id: id.clone(),
        }
    }
}

impl From<&Credential> for CredentialDto {
    fn from(credential: &Credential) -> Self {
        Self {
            id: credential.id.clone(),
            legacy_account_id: credential.legacy_account_id.clone(),
            destination_id: credential.destination_id.clone(),
            name: credential.name.clone(),
            notes: credential.notes.clone(),
            has_secret: credential.has_secret,
            enabled: credential.enabled,
            routing_rank: credential.routing_rank,
            scope: credential.scope.clone(),
            grants: CredentialGrantsDto::from(&credential.grants),
            auth_state: credential.auth_state,
            last_error: credential.last_error.clone(),
            cooldowns: CredentialCooldownsDto::from(&credential.cooldowns),
            quota_pool_id: credential.quota_pool_id.clone(),
            onboarding_task: credential
                .onboarding_task
                .as_ref()
                .map(DestinationOnboardingTaskDto::from),
            purchase_date: credential.purchase_date.clone(),
        }
    }
}

impl From<AdapterKind> for AdapterKindDto {
    fn from(value: AdapterKind) -> Self {
        match value {
            AdapterKind::OpencodeGo => Self::OpencodeGo,
            AdapterKind::Zen => Self::Zen,
            AdapterKind::Goat => Self::Goat,
            AdapterKind::Minimax => Self::Minimax,
            AdapterKind::Kimi => Self::Kimi,
            AdapterKind::Ollama => Self::Ollama,
            AdapterKind::Cpa => Self::Cpa,
            AdapterKind::Http => Self::Http,
        }
    }
}

impl From<AuthScheme> for AuthSchemeDto {
    fn from(value: AuthScheme) -> Self {
        match value {
            AuthScheme::None => Self::None,
            AuthScheme::Bearer => Self::Bearer,
            AuthScheme::XApiKey => Self::XApiKey,
        }
    }
}

impl From<ModelResolution> for ModelResolutionDto {
    fn from(value: ModelResolution) -> Self {
        match value {
            ModelResolution::AdapterDefined => Self::AdapterDefined,
            ModelResolution::PublicOnly => Self::PublicOnly,
            ModelResolution::PublicAndUpstream => Self::PublicAndUpstream,
        }
    }
}

impl From<ProtocolDto> for ocg_domain::catalog::UpstreamProtocolKind {
    fn from(value: ProtocolDto) -> Self {
        match value {
            ProtocolDto::ChatCompletions => Self::ChatCompletions,
            ProtocolDto::Responses => Self::Responses,
            ProtocolDto::Messages => Self::Messages,
        }
    }
}

impl From<ocg_domain::catalog::UpstreamProtocolKind> for ProtocolDto {
    fn from(value: ocg_domain::catalog::UpstreamProtocolKind) -> Self {
        match value {
            ocg_domain::catalog::UpstreamProtocolKind::ChatCompletions => Self::ChatCompletions,
            ocg_domain::catalog::UpstreamProtocolKind::Responses => Self::Responses,
            ocg_domain::catalog::UpstreamProtocolKind::Messages => Self::Messages,
        }
    }
}

impl From<RedirectPolicy> for RedirectPolicyDto {
    fn from(value: RedirectPolicy) -> Self {
        match value {
            RedirectPolicy::NoFollow => Self::NoFollow,
            RedirectPolicy::FollowKeyless => Self::FollowKeyless,
        }
    }
}

impl From<&Capabilities> for CapabilitiesDto {
    fn from(value: &Capabilities) -> Self {
        Self {
            testable: value.testable,
            discoverable_models: value.discoverable_models,
            official_balance_probe: value.official_balance_probe.clone(),
            observer: value.observer,
            managed_signup: value.managed_signup,
            external_integration: value.external_integration,
            billing_tier_required: value.billing_tier_required,
            redirect_policy: value.redirect_policy.into(),
            identity_headers: value.identity_headers,
        }
    }
}

impl From<UsageSource> for UsageSourceDto {
    fn from(value: UsageSource) -> Self {
        match value {
            UsageSource::OfficialApi => Self::OfficialApi,
            UsageSource::LocalProjection => Self::LocalProjection,
            UsageSource::None => Self::None,
        }
    }
}

impl From<PlanWindowKind> for PlanWindowKindDto {
    fn from(value: PlanWindowKind) -> Self {
        match value {
            PlanWindowKind::FiveHours => Self::FiveHours,
            PlanWindowKind::Week => Self::Week,
            PlanWindowKind::Month => Self::Month,
            PlanWindowKind::Free => Self::Free,
        }
    }
}

impl From<PlanWindow> for PlanWindowDto {
    fn from(value: PlanWindow) -> Self {
        Self {
            kind: value.kind.into(),
        }
    }
}

impl From<ExpiryCadence> for ExpiryCadenceDto {
    fn from(value: ExpiryCadence) -> Self {
        match value {
            ExpiryCadence::Monthly => Self::Monthly,
        }
    }
}

impl From<PricingSource> for PricingSourceDto {
    fn from(value: PricingSource) -> Self {
        match value {
            PricingSource::Official => Self::Official,
            PricingSource::VerifiedSnapshot => Self::VerifiedSnapshot,
            PricingSource::Unpriced => Self::Unpriced,
        }
    }
}

impl From<&Plan> for PlanDto {
    fn from(value: &Plan) -> Self {
        Self {
            usage_source: value.usage_source.into(),
            windows: value
                .windows
                .iter()
                .copied()
                .map(PlanWindowDto::from)
                .collect(),
            expiry_cadence: value.expiry_cadence.map(ExpiryCadenceDto::from),
            pricing_source: value.pricing_source.into(),
            manual_calibration: value.manual_calibration,
        }
    }
}

impl From<&CatalogModel> for CatalogModelDto {
    fn from(value: &CatalogModel) -> Self {
        Self {
            public_model: value.public_model.clone(),
            upstream_model: value.upstream_model.clone(),
            protocols: value
                .protocols
                .iter()
                .copied()
                .map(ProtocolDto::from)
                .collect(),
            preferred: value.preferred.map(ProtocolDto::from),
            enabled: value.enabled,
            upstream_override: value.upstream_override.as_ref().map(|route| {
                super::types::DestinationUpstreamOverridePatch {
                    protocol: ProtocolDto::from(route.protocol),
                    endpoint_url: route.endpoint_url.clone(),
                }
            }),
        }
    }
}

impl From<&Grants> for CredentialGrantsDto {
    fn from(value: &Grants) -> Self {
        Self {
            allowed_endpoint_ids: value.allowed_endpoint_ids.clone(),
            allowed_origins: value.allowed_origins.clone(),
        }
    }
}

impl From<&Cooldowns> for CredentialCooldownsDto {
    fn from(value: &Cooldowns) -> Self {
        Self {
            generic_until: value.generic_until.map(|until| until.to_rfc3339()),
            five_hour_until: value.five_hour_until.map(|until| until.to_rfc3339()),
            week_until: value.week_until.map(|until| until.to_rfc3339()),
            month_until: value.month_until.map(|until| until.to_rfc3339()),
            free_until: value.free_until.map(|until| until.to_rfc3339()),
        }
    }
}

impl From<&OnboardingTaskRef> for DestinationOnboardingTaskDto {
    fn from(value: &OnboardingTaskRef) -> Self {
        Self {
            kind: value.kind,
            state: value.state,
            step: value.step.clone(),
        }
    }
}

#[cfg(test)]
mod tests;

impl From<&ProjectionRefusal> for DestinationProjectionRefusalDto {
    fn from(refusal: &ProjectionRefusal) -> Self {
        Self {
            row: RefusedRowDto::from(&refusal.row),
            error: MappingErrorCodeDto::from(&refusal.error),
            detail: refusal.error.to_string(),
        }
    }
}

impl From<&RefusedRow> for RefusedRowDto {
    fn from(row: &RefusedRow) -> Self {
        match row {
            RefusedRow::Account { id, provider_id } => Self {
                kind: RefusedRowKindDto::Account,
                id: id.clone(),
                provider_id: Some(provider_id.clone()),
            },
            RefusedRow::DynamicProvider { id } => Self {
                kind: RefusedRowKindDto::DynamicProvider,
                id: id.clone(),
                provider_id: None,
            },
            RefusedRow::PlatformParent { id } => Self {
                kind: RefusedRowKindDto::PlatformParent,
                id: id.clone(),
                provider_id: None,
            },
        }
    }
}

impl From<&MappingError> for MappingErrorCodeDto {
    fn from(error: &MappingError) -> Self {
        match error {
            MappingError::UnknownProvider { .. } => Self::UnknownProvider,
            MappingError::MissingDestination { .. } => Self::MissingDestination,
            MappingError::CustomRequiresAccount => Self::CustomRequiresAccount,
            MappingError::CustomAccountMissingEndpoint { .. } => Self::CustomAccountMissingEndpoint,
            MappingError::DynamicMissingEndpoint { .. } => Self::DynamicMissingEndpoint,
            MappingError::PlatformMissingBaseUrl { .. } => Self::PlatformMissingBaseUrl,
        }
    }
}

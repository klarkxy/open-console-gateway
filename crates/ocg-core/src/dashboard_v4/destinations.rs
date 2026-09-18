//! Read-only V4 destination and credential listings.
//!
//! Served from the stage-4a [`crate::destination_projection`] mapper. Handlers
//! take the settings lock, read SQLite only, and never issue outbound HTTP or
//! write. Mapping totality is still live [`project`]; a populated v50 shadow
//! is the served snapshot, and an empty shadow falls back to live. Projection
//! refusals are a structured 409.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use ocg_domain::destination::{
    AdapterKind, AuthScheme, Capabilities, CatalogModel, Cooldowns, Credential, Destination,
    ExpiryCadence, Grants, LegacyDestinationRef, MappingError, OnboardingTaskRef, Plan, PlanWindow,
    PlanWindowKind, PricingSource, RedirectPolicy, UsageSource,
};

use crate::dashboard_v3::{ControlRevision, V3ApiError};
use crate::destination_projection::{ProjectionRefusal, RefusedRow, read_v4_projection};
use crate::state::CoreState;

use super::types::{
    AdapterKindDto, AuthSchemeDto, CapabilitiesDto, CatalogModelDto, CredentialCooldownsDto,
    CredentialGrantsDto, CredentialList, DestinationCredentialDto as CredentialDto, DestinationDto,
    DestinationList, DestinationOnboardingTaskDto, DestinationProjectionRefusalDto,
    DestinationProjectionRefusedError, ExpiryCadenceDto, LegacyDestinationKindDto,
    LegacyDestinationRefDto, MappingErrorCodeDto, PlanDto, PlanWindowDto, PlanWindowKindDto,
    PricingSourceDto, ProtocolDto, RedirectPolicyDto, RefusedRowDto, RefusedRowKindDto,
    UsageSourceDto,
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

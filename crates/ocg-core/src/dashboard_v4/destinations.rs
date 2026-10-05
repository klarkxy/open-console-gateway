//! V4 destination/credential reads and transactional HTTP configuration writes.
//! All reads use persisted configuration; writes preflight the runtime before
//! committing and publish under the same settings lock. Refusals are explicit.

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
use ocg_domain::dynamic::{DynamicAuthKind, DynamicModelMapping, DynamicModelUpstreamOverride};

use crate::dashboard_v3::{ControlRevision, MutationExpectation, V3ApiError, parse_mutation_json};
use crate::destination_projection::{ProjectionRefusal, RefusedRow, read_v4_projection};
use crate::state::CoreState;

use super::types::{
    AccountConfigurationOwnerDto, AccountConsoleLinkDto, AccountControlsDto, AccountToggleWriteDto,
    AdapterKindDto, AuthSchemeDto, CapabilitiesDto, CatalogModelDto, CredentialCooldownsDto,
    CredentialGrantsDto, CredentialList, DestinationCredentialDto as CredentialDto,
    DestinationDeleteResult, DestinationDto, DestinationList, DestinationModelPatch,
    DestinationOnboardingTaskDto, DestinationPatchRequest, DestinationPatchResult,
    DestinationProjectionRefusalDto, DestinationProjectionRefusedError, ExpiryCadenceDto,
    HttpProtocolRouteDto, LegacyDestinationKindDto, LegacyDestinationRefDto, MappingErrorCodeDto,
    ModelResolutionDto, PlanDto, PlanWindowDto, PlanWindowKindDto, PricingSourceDto, ProtocolDto,
    QuotaRecoveryDto, QuotaRecoveryReason, QuotaRecoveryStatus, QuotaRecoveryWindow,
    RedirectPolicyDto, RefusedRowDto, RefusedRowKindDto, UsageSourceDto,
};

/// Stable 409 code when the stage-4a projection cannot map every live row.
pub const ERROR_DESTINATION_PROJECTION_REFUSED: &str = "destinationProjectionRefused";

#[derive(Debug)]
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
        credentials: overlay_with_policy(&state, &projection.credentials)?,
    }))
}

pub(super) async fn patch_destination(
    State(state): State<CoreState>,
    Path(id): Path<String>,
    body: Bytes,
) -> Result<Json<DestinationPatchResult>, DestinationsError> {
    let input = parse_mutation_json::<DestinationPatchRequest>(&body)?;
    let receipt = commit_destination_patch(&state, &id, input)?;
    crate::cpa_execution::note_product_apply(&state).await;
    receipt.map(Json)
}

pub(super) async fn delete_destination(
    State(state): State<CoreState>,
    Path(id): Path<String>,
    body: Bytes,
) -> Result<Json<DestinationDeleteResult>, DestinationsError> {
    let expectation = parse_mutation_json::<MutationExpectation>(&body)?;
    let saved = delete_destination_locked(&state, &id, expectation)?;
    crate::cpa_execution::note_product_apply(&state).await;
    Ok(Json(saved))
}

fn patch_destination_locked(
    state: &CoreState,
    destination_id: &str,
    input: DestinationPatchRequest,
) -> Result<DestinationPatchResult, DestinationsError> {
    commit_destination_patch(state, destination_id, input)?
}

/// Outer error: nothing was committed. Inner error: the commit succeeded and
/// the receipt read failed. The async handler applies once for every inner result.
fn commit_destination_patch(
    state: &CoreState,
    destination_id: &str,
    input: DestinationPatchRequest,
) -> Result<Result<DestinationPatchResult, DestinationsError>, DestinationsError> {
    let catalog_updates = input
        .models
        .iter()
        .map(|model| crate::account_control::CatalogModelEdit {
            public_model: model.public_model.clone(),
            enabled: model.enabled,
            protocols: model
                .protocols
                .as_ref()
                .map(|protocols| protocols.iter().copied().map(Into::into).collect()),
            preferred: model.preferred.map(Into::into),
        })
        .collect();
    let update = crate::account_control::HttpDestinationUpdate {
        name: input.name,
        endpoint_url: input.endpoint_url,
        upstream_protocol: input.upstream_protocol.into(),
        auth_kind: match input.auth_scheme {
            AuthSchemeDto::Bearer => DynamicAuthKind::Bearer,
            AuthSchemeDto::XApiKey => DynamicAuthKind::XApiKey,
            AuthSchemeDto::ApiKey => DynamicAuthKind::ApiKey,
            AuthSchemeDto::None => DynamicAuthKind::None,
        },
        mappings: input.models.into_iter().map(model_patch).collect(),
        authorize_credential_ids: input.authorize_credential_ids,
        protocol_routes: input.protocol_routes.map(|routes| {
            routes
                .into_iter()
                .map(|route| ocg_domain::destination::HttpProtocolRoute::from(&route))
                .collect()
        }),
        enabled: input.enabled,
        catalog_updates,
    };
    let _settings_update = state.settings_update.lock();
    if input.expectation.expected_revision != state.settings_revision()
        || input.expectation.process_generation != state.process_generation()
    {
        return Err(V3ApiError::revision_conflict(state).into());
    }
    ensure_quota_presentation(state)?;
    crate::account_control::update_http_destination_locked(state, destination_id, update)
        .map_err(|error| map_destination_control_error(state, error))?;
    Ok(mutation_result_locked(state, destination_id))
}

pub(super) fn ensure_quota_presentation(state: &CoreState) -> Result<(), DestinationsError> {
    let db = state.db.lock();
    crate::cpa_quota::present_all(&db.conn, state.sample_gateway_clock().0)
        .map_err(|_| V3ApiError::internal("official quota policy could not be read"))?;
    Ok(())
}

#[cfg(test)]
thread_local! {
    static FAIL_NEXT_MUTATION_RECEIPT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Next `mutation_result_locked` returns the quota-read error after the commit.
#[cfg(test)]
pub(super) fn fail_next_mutation_receipt() {
    FAIL_NEXT_MUTATION_RECEIPT.with(|flag| flag.set(true));
}

#[cfg(test)]
pub(super) fn clear_mutation_receipt_failure() {
    FAIL_NEXT_MUTATION_RECEIPT.with(|flag| flag.set(false));
}

pub(super) fn mutation_result_locked(
    state: &CoreState,
    destination_id: &str,
) -> Result<DestinationPatchResult, DestinationsError> {
    #[cfg(test)]
    if FAIL_NEXT_MUTATION_RECEIPT.with(|flag| flag.replace(false)) {
        return Err(V3ApiError::internal("official quota policy could not be read").into());
    }
    // The caller already owns `settings_update`; do not call
    // `load_projection`, which would try to acquire the non-reentrant lock.
    let projection = {
        let db = state.db.lock();
        read_v4_projection(&db).map_err(V3ApiError::internal)?
    };
    let projection = projection
        .map_err(|refusals| DestinationsError::Refused(projection_refused(state, &refusals)))?;
    let updated = projection
        .destinations
        .iter()
        .find(|destination| destination.id == destination_id)
        .ok_or_else(|| V3ApiError::not_found_at(state, "destination not found"))?;
    Ok(DestinationPatchResult {
        revision: ControlRevision::from_state(state),
        destination: DestinationDto::from(updated),
        credentials: overlay_with_policy(state, &projection.credentials)?,
    })
}

fn delete_destination_locked(
    state: &CoreState,
    destination_id: &str,
    expectation: MutationExpectation,
) -> Result<DestinationDeleteResult, DestinationsError> {
    let _settings_update = state.settings_update.lock();
    if expectation.expected_revision != state.settings_revision()
        || expectation.process_generation != state.process_generation()
    {
        return Err(V3ApiError::revision_conflict(state).into());
    }
    crate::account_control::delete_http_destination_locked(state, destination_id)
        .map_err(|error| map_destination_control_error(state, error))?;
    Ok(DestinationDeleteResult {
        revision: ControlRevision::from_state(state),
    })
}

fn map_destination_control_error(
    state: &CoreState,
    error: crate::account_control::AccountControlError,
) -> DestinationsError {
    match error {
        crate::account_control::AccountControlError::NotFound => {
            V3ApiError::not_found_at(state, "destination not found").into()
        }
        crate::account_control::AccountControlError::Invalid(message)
        | crate::account_control::AccountControlError::Conflict(message) => {
            V3ApiError::invalid_request_at(state, message).into()
        }
        crate::account_control::AccountControlError::RevisionConflict => {
            V3ApiError::revision_conflict(state).into()
        }
        crate::account_control::AccountControlError::Unavailable(message) => {
            V3ApiError::precondition_failed_at(state, message).into()
        }
        crate::account_control::AccountControlError::Internal(error) => {
            V3ApiError::internal(error).into()
        }
    }
}

#[cfg(test)]
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
    let db = state.db.lock();
    let projection = read_v4_projection(&db).map_err(V3ApiError::internal)?;
    let revision = ControlRevision::from_state(state);
    match projection {
        Ok(projection) => Ok((projection, revision)),
        Err(refusals) => Err(DestinationsError::Refused(projection_refused(
            state, &refusals,
        ))),
    }
}

pub(super) fn overlay_with_policy(
    state: &CoreState,
    credentials: &[Credential],
) -> Result<Vec<CredentialDto>, V3ApiError> {
    let db = state.db.lock();
    let policy = crate::cpa_quota::present_all(&db.conn, state.sample_gateway_clock().0)
        .map_err(|_| V3ApiError::internal("official quota policy could not be read"))?;
    Ok(overlay_credential_dtos(credentials, &policy))
}

pub(super) fn overlay_credential_dtos(
    credentials: &[Credential],
    policy: &std::collections::HashMap<String, crate::cpa_quota::EffectivePlan>,
) -> Vec<CredentialDto> {
    credentials
        .iter()
        .map(|credential| {
            let mut dto = CredentialDto::from(credential);
            dto.quota_recovery = policy.get(&credential.id).map(policy_quota_dto);
            dto
        })
        .collect()
}

pub(super) fn with_policy_quota(
    credential: &Credential,
    plan: &crate::cpa_quota::EffectivePlan,
) -> CredentialDto {
    let mut dto = CredentialDto::from(credential);
    dto.quota_recovery = Some(policy_quota_dto(plan));
    dto
}

fn policy_quota_dto(plan: &crate::cpa_quota::EffectivePlan) -> QuotaRecoveryDto {
    use crate::cpa_quota::{PlanStatus, PlanWindowLabel};
    QuotaRecoveryDto {
        status: match plan.card.status {
            PlanStatus::Waiting => QuotaRecoveryStatus::Waiting,
            PlanStatus::Ready => QuotaRecoveryStatus::Ready,
            PlanStatus::Probing => QuotaRecoveryStatus::Probing,
        },
        reason: QuotaRecoveryReason::QuotaExhausted,
        window: match plan.card.window {
            PlanWindowLabel::FiveHours => QuotaRecoveryWindow::FiveHours,
            PlanWindowLabel::Week => QuotaRecoveryWindow::Week,
            PlanWindowLabel::Month => QuotaRecoveryWindow::Month,
            PlanWindowLabel::Unknown => QuotaRecoveryWindow::Unknown,
        },
        observed_at: plan.card.observed_at.to_rfc3339(),
        resets_at: plan.card.resets_at.map(|at| at.to_rfc3339()),
        next_retry_at: plan.card.next_retry_at.to_rfc3339(),
        failure_count: 0,
    }
}

/// Structured 409 reused by both destination and credential reads.
pub(super) fn projection_refused(
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

impl From<&Destination> for AccountControlsDto {
    fn from(destination: &Destination) -> Self {
        Self {
            toggle_write: if destination.adapter == AdapterKind::Zen {
                AccountToggleWriteDto::ProviderSettings
            } else {
                AccountToggleWriteDto::Account
            },
            configuration_owner: if matches!(
                destination.legacy,
                LegacyDestinationRef::CustomAccount(_)
            ) {
                AccountConfigurationOwnerDto::Account
            } else {
                AccountConfigurationOwnerDto::Destination
            },
            console_link: match destination.adapter {
                AdapterKind::OpencodeGo => Some(AccountConsoleLinkDto::Opencode),
                AdapterKind::Ollama => Some(AccountConsoleLinkDto::Ollama),
                _ => None,
            },
            browser_profile: destination.adapter == AdapterKind::OpencodeGo,
        }
    }
}

impl From<&Destination> for DestinationDto {
    fn from(destination: &Destination) -> Self {
        Self {
            account_controls: AccountControlsDto::from(destination),
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
            protocol_routes: destination
                .protocol_routes
                .iter()
                .map(HttpProtocolRouteDto::from)
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
            quota_recovery: None,
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
            AuthScheme::ApiKey => Self::ApiKey,
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

impl From<&ocg_domain::destination::HttpProtocolRoute> for HttpProtocolRouteDto {
    fn from(route: &ocg_domain::destination::HttpProtocolRoute) -> Self {
        Self {
            protocol: route.protocol.into(),
            endpoint_url: route.endpoint_url.clone(),
            auth_scheme: route.auth_scheme.into(),
        }
    }
}

impl From<&HttpProtocolRouteDto> for ocg_domain::destination::HttpProtocolRoute {
    fn from(route: &HttpProtocolRouteDto) -> Self {
        Self {
            protocol: route.protocol.into(),
            endpoint_url: route.endpoint_url.clone(),
            auth_scheme: match route.auth_scheme {
                AuthSchemeDto::Bearer => AuthScheme::Bearer,
                AuthSchemeDto::XApiKey => AuthScheme::XApiKey,
                AuthSchemeDto::ApiKey => AuthScheme::ApiKey,
                AuthSchemeDto::None => AuthScheme::None,
            },
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

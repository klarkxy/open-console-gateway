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
use ocg_domain::dynamic::{
    DynamicAuthKind, DynamicModelMapping, DynamicModelUpstreamOverride, DynamicProviderDefinition,
};

use crate::dashboard_v3::{
    ControlRevision, MutationExpectation, V3ApiError, check_expectation, parse_mutation_json,
};
use crate::destination_projection::{ProjectionRefusal, RefusedRow, read_v4_projection};
use crate::dynamic::validate_definition;
use crate::quota_recovery::{
    PersistedQuotaReason, PersistedQuotaRecovery, PersistedQuotaWindow, QuotaEpisode,
    QuotaPresentationStatus, QuotaRecoveryView,
};
use crate::state::CoreState;

use super::types::{
    AccountConfigurationOwnerDto, AccountConsoleLinkDto, AccountControlsDto, AccountToggleWriteDto,
    AdapterKindDto, AuthSchemeDto, CapabilitiesDto, CatalogModelDto, CredentialCooldownsDto,
    CredentialGrantsDto, CredentialList, DestinationCatalogModelUpdate,
    DestinationCredentialDto as CredentialDto, DestinationDeleteResult, DestinationDto,
    DestinationList, DestinationModelPatch, DestinationOnboardingTaskDto, DestinationPatchRequest,
    DestinationPatchResult, DestinationProjectionRefusalDto, DestinationProjectionRefusedError,
    ExpiryCadenceDto, HttpProtocolRouteDto, LegacyDestinationKindDto, LegacyDestinationRefDto,
    MappingErrorCodeDto, ModelResolutionDto, PlanDto, PlanWindowDto, PlanWindowKindDto,
    PricingSourceDto, ProtocolDto, QuotaRecoveryDto, QuotaRecoveryReason, QuotaRecoveryStatus,
    QuotaRecoveryWindow, RedirectPolicyDto, RefusedRowDto, RefusedRowKindDto, UsageSourceDto,
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
    let (projection, _, _, revision) = load_projection(&state)?;
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
    let (projection, recoveries, probes, revision) = load_projection(&state)?;
    Ok(Json(CredentialList {
        revision,
        credentials: overlay_credential_dtos(&state, &projection.credentials, &recoveries, &probes),
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
    let definition = destination_definition(&destination, &input)
        .map_err(|message| V3ApiError::invalid_request_at(state, message))?;
    state
        .commit_configuration_update(|db| {
            let routes = input.protocol_routes.as_ref().map(|routes| {
                routes
                    .iter()
                    .map(ocg_domain::destination::HttpProtocolRoute::from)
                    .collect::<Vec<_>>()
            });
            crate::db::destination_commands::replace_http_destination_with_routes_on(
                db,
                destination_id,
                &definition,
                &input.authorize_credential_ids,
                routes.as_deref(),
            )?;
            if let Some(enabled) = input.enabled {
                db.conn.execute(
                    "UPDATE destinations SET enabled = ?2 WHERE id = ?1",
                    rusqlite::params![destination_id, enabled],
                )?;
            }
            let configured = crate::destination_projection::load_runtime(db)?
                .destinations
                .into_iter()
                .find(|row| row.id == destination_id)
                .ok_or_else(|| anyhow::anyhow!("destination not found"))?;
            let updates: Vec<_> = input
                .models
                .iter()
                .map(|model| DestinationCatalogModelUpdate {
                    public_model: model.public_model.clone(),
                    enabled: model.enabled,
                    protocols: model.protocols.clone(),
                    preferred: model.preferred,
                })
                .collect();
            let catalog = super::destination_catalog::apply_updates(&configured, &updates, &[])
                .map_err(anyhow::Error::msg)?;
            crate::db::destination_store::replace_destination_catalog(
                &db.conn,
                destination_id,
                &catalog,
            )?;
            Ok(())
        })
        .map_err(|error| V3ApiError::invalid_request_at(state, error.to_string()))?;

    mutation_result_locked(state, destination_id)
}

pub(super) fn mutation_result_locked(
    state: &CoreState,
    destination_id: &str,
) -> Result<DestinationPatchResult, DestinationsError> {
    // The caller already owns `settings_update`; do not call
    // `load_projection`, which would try to acquire the non-reentrant lock.
    let (projection, recoveries, probes) = {
        let db = state.db.lock();
        let projection = read_v4_projection(&db).map_err(V3ApiError::internal)?;
        let recoveries = crate::db::quota_recovery::load_all_identified_on(&db.conn)
            .map_err(V3ApiError::internal)?;
        let probes = state.quota_probes.lock().clone();
        (projection, recoveries, probes)
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
        credentials: overlay_credential_dtos(state, &projection.credentials, &recoveries, &probes),
    })
}

fn delete_destination_locked(
    state: &CoreState,
    destination_id: &str,
    expectation: MutationExpectation,
) -> Result<DestinationDeleteResult, DestinationsError> {
    let _settings_update = state.settings_update.lock();
    check_expectation(state, &expectation)?;
    state
        .commit_configuration_update(|db| {
            crate::db::destination_commands::delete_http_destination_on(db, destination_id)
        })
        .map_err(|error| V3ApiError::invalid_request_at(state, error.to_string()))?;
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
    if destination.capabilities.observer {
        return Err("platform-managed destinations are immutable".to_string());
    }
    let id = destination.id.clone();
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

#[allow(clippy::type_complexity)]
fn load_projection(
    state: &CoreState,
) -> Result<
    (
        crate::destination_projection::DestinationProjection,
        std::collections::HashMap<String, crate::db::quota_recovery::QuotaRecoveryRow>,
        std::collections::HashMap<String, QuotaEpisode>,
        ControlRevision,
    ),
    DestinationsError,
> {
    let _settings_update = state.settings_update.lock();
    let db = state.db.lock();
    let projection = read_v4_projection(&db).map_err(V3ApiError::internal)?;
    let recoveries = crate::db::quota_recovery::load_all_identified_on(&db.conn)
        .map_err(V3ApiError::internal)?;
    let probes = state.quota_probes.lock().clone();
    let revision = ControlRevision::from_state(state);
    match projection {
        Ok(projection) => Ok((projection, recoveries, probes, revision)),
        Err(refusals) => Err(DestinationsError::Refused(projection_refused(
            state, &refusals,
        ))),
    }
}

pub(super) fn overlay_credential_dtos(
    state: &CoreState,
    credentials: &[Credential],
    recoveries: &std::collections::HashMap<String, crate::db::quota_recovery::QuotaRecoveryRow>,
    probes: &std::collections::HashMap<String, QuotaEpisode>,
) -> Vec<CredentialDto> {
    let now = state.sample_gateway_clock().0;
    credentials
        .iter()
        .map(|credential| {
            let mut dto = CredentialDto::from(credential);
            dto.quota_recovery = recoveries.get(&credential.id).map(|row| {
                let probing = probes.get(&credential.id).is_some_and(|episode| {
                    crate::routing_snapshot::quota_episode_matches(
                        episode,
                        &credential.id,
                        row.credential_version,
                        &row.key_cipher,
                        row.recovery.epoch,
                    )
                });
                quota_recovery_dto(row.recovery.present(now, probing))
            });
            dto
        })
        .collect()
}

pub(super) fn overlay_one_credential_dto(
    state: &CoreState,
    credential: &Credential,
    recovery: Option<&PersistedQuotaRecovery>,
    probing: bool,
) -> CredentialDto {
    let now = state.sample_gateway_clock().0;
    let mut dto = CredentialDto::from(credential);
    dto.quota_recovery = recovery.map(|row| quota_recovery_dto(row.present(now, probing)));
    dto
}

fn quota_recovery_dto(view: QuotaRecoveryView) -> QuotaRecoveryDto {
    QuotaRecoveryDto {
        status: match view.status {
            QuotaPresentationStatus::Waiting => QuotaRecoveryStatus::Waiting,
            QuotaPresentationStatus::Ready => QuotaRecoveryStatus::Ready,
            QuotaPresentationStatus::Probing => QuotaRecoveryStatus::Probing,
        },
        reason: match view.reason {
            PersistedQuotaReason::QuotaExhausted => QuotaRecoveryReason::QuotaExhausted,
            PersistedQuotaReason::InsufficientBalance => QuotaRecoveryReason::InsufficientBalance,
        },
        window: match view.window {
            PersistedQuotaWindow::FiveHours => QuotaRecoveryWindow::FiveHours,
            PersistedQuotaWindow::Week => QuotaRecoveryWindow::Week,
            PersistedQuotaWindow::Month => QuotaRecoveryWindow::Month,
            PersistedQuotaWindow::Unknown => QuotaRecoveryWindow::Unknown,
        },
        observed_at: view.observed_at.to_rfc3339(),
        resets_at: view.resets_at.map(|at| at.to_rfc3339()),
        next_retry_at: view.next_retry_at.to_rfc3339(),
        failure_count: view.failure_count,
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

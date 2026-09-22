//! Materializes frozen destination/catalog targets into request and transport
//! plans. Model resolution, protocol selection, endpoint and credential grants
//! consume explicit persisted facts. The Account materializer is test-only
//! compatibility coverage for operational adapters.

use crate::alias::{ProviderMapping, ResolveError, ResolvedModel};
#[cfg(test)]
use crate::custom::CustomAccountRuntime;
#[cfg(test)]
use crate::destination_projection::DestinationProjection;
#[cfg(test)]
use crate::gateway::free_models::resolve_upstream_base;
use crate::gateway::protocol::{
    CustomRouteSpec, MaterializeSpec, ParsedClientRequest, ProtocolError, RequestPlan,
    materialize_parsed_request,
};
use crate::gateway::provider_adapter;
use crate::gateway::routing::RoutingCandidate;
#[cfg(test)]
use crate::goat::GoatAccountRuntime;
use crate::kernel::ids::{custom_model_id_matches, normalize_model_name};
use crate::kernel::protocol::ApiFormat;
#[cfg(test)]
use crate::models::Account;
use crate::models::{AppConfig, UpstreamChannel};
use crate::provider::ProviderAdapterKind;
#[cfg(test)]
use crate::provider_contracts::{ContractScope, EffectiveContractSet};
use axum::http::StatusCode;
use ocg_domain::credential::{ModelScope, model_scope_allows};
use ocg_domain::destination::Destination;
#[cfg(test)]
use ocg_domain::destination::LegacyDestinationRef;
#[cfg(test)]
use std::collections::HashMap;

pub use crate::gateway::protocol::{
    parse_client_request as parse_client, parse_gemini_request as parse_gemini,
};

#[derive(Debug, Clone)]
#[cfg(test)]
pub(crate) struct MaterializedCandidate {
    pub routing: RoutingCandidate,
    pub plan: RequestPlan,
}

/// Stable internal rejection identity. Wire/explain codes are `as_str()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RouteRejectionCode {
    MappingProtocolIncompatible,
    CredentialDisabled,
    BindingDisabled,
    ModelScopeDenied,
    #[allow(dead_code)] // Historical explanation code retained for compatibility fixtures.
    GoatNotEligible,
    #[allow(dead_code)] // Historical explanation code retained for compatibility fixtures.
    GoatUnverified,
    CandidateMaterializationFailed,
    ProductionRouteUnsupported,
}

impl RouteRejectionCode {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::MappingProtocolIncompatible => "mapping_protocol_incompatible",
            Self::CredentialDisabled => "credential_disabled",
            Self::BindingDisabled => "binding_disabled",
            Self::ModelScopeDenied => "model_scope_denied",
            Self::GoatNotEligible => "goat_not_eligible",
            Self::GoatUnverified => "goat_unverified",
            Self::CandidateMaterializationFailed => "candidate_materialization_failed",
            Self::ProductionRouteUnsupported => "production_route_unsupported",
        }
    }
}

/// Typed materialize rejection with the historical human detail string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RouteRejection {
    pub code: RouteRejectionCode,
    pub detail: String,
    pub account_id: Option<String>,
    pub provider_id: Option<String>,
    pub upstream_model: Option<String>,
}

#[cfg(test)]
fn mapping_rejection(
    code: RouteRejectionCode,
    mapping: &ProviderMapping,
    detail: String,
) -> RouteRejection {
    RouteRejection {
        code,
        detail,
        account_id: None,
        provider_id: Some(mapping.provider_id.clone()),
        upstream_model: Some(mapping.upstream_model.clone()),
    }
}

#[cfg(test)]
fn account_rejection(
    code: RouteRejectionCode,
    account: &Account,
    suffix: impl std::fmt::Display,
) -> RouteRejection {
    RouteRejection {
        code,
        detail: format!(
            "{}/{} account `{}`: {suffix}",
            account.provider_id, account.provider_id, account.name
        ),
        account_id: Some(account.id.clone()),
        provider_id: Some(account.provider_id.clone()),
        upstream_model: None,
    }
}

#[derive(Debug, Clone)]
#[cfg(test)]
pub(crate) struct MaterializedRouteSet {
    pub routes: Vec<MaterializedCandidate>,
    pub free_only: bool,
    pub incompatibility: Option<String>,
    /// Account/mapping rejection notes collected while building candidates.
    /// Empty when every considered account produced a route. Live send ignores
    /// this list; the read-only shadow planner surfaces it for compare.
    pub rejected: Vec<String>,
    /// Typed form of [`Self::rejected`]. Same order and human detail.
    pub rejections: Vec<RouteRejection>,
}

#[derive(Debug, Clone)]
#[cfg(test)]
pub(crate) struct InferenceBindingGate {
    pub enabled: bool,
    pub model_scope: ModelScope,
}

#[cfg(test)]
pub(crate) type InferenceBindingIndex = HashMap<String, InferenceBindingGate>;

#[cfg(test)]
fn default_inference_binding() -> InferenceBindingGate {
    InferenceBindingGate {
        enabled: true,
        model_scope: ModelScope::All,
    }
}

pub(crate) fn binding_allows_requested_model(
    scope: &ModelScope,
    client_model: &str,
    routing_model: &str,
    plan_models: impl IntoIterator<Item = impl AsRef<str>>,
) -> bool {
    model_scope_allows(scope, routing_model)
        || (client_model != routing_model && model_scope_allows(scope, client_model))
        || plan_models
            .into_iter()
            .any(|model| model_scope_allows(scope, model.as_ref()))
}

pub(crate) fn mapping_adapter_kind(mapping: &ProviderMapping) -> Option<ProviderAdapterKind> {
    crate::dynamic::adapter_kind_for(&mapping.provider_id, &[]).or_else(|| {
        uuid::Uuid::parse_str(&mapping.provider_id)
            .ok()
            .map(|_| ProviderAdapterKind::ConfigurableHttp)
    })
}

/// Custom/platform catalog row: Configurable HTTP whose `provider_id` is the
/// sealed custom catalog key, not a dynamic UUID.
pub(crate) fn mapping_is_custom_http_catalog(mapping: &ProviderMapping) -> bool {
    mapping_adapter_kind(mapping) == Some(ProviderAdapterKind::ConfigurableHttp)
        && crate::dynamic::adapter_kind_for(&mapping.provider_id, &[])
            == Some(ProviderAdapterKind::ConfigurableHttp)
}

#[cfg(test)]
fn mapping_is_configurable_http(mapping: &ProviderMapping) -> bool {
    mapping_adapter_kind(mapping) == Some(ProviderAdapterKind::ConfigurableHttp)
}

#[cfg(test)]
pub(crate) fn mapping_is_zen_free(mapping: &ProviderMapping) -> bool {
    mapping_adapter_kind(mapping) == Some(ProviderAdapterKind::ZenFree)
}

pub(crate) fn protocol_error_from_resolve(error: ResolveError) -> ProtocolError {
    match error.code() {
        Some(code) => ProtocolError::with_code(StatusCode::BAD_REQUEST, code, error.message()),
        None => ProtocolError::new(error.message()),
    }
}

/// Canonical registry alias persisted on forward logs for this resolution.
pub(crate) fn resolved_alias_from_model(resolved: &ResolvedModel) -> Option<String> {
    match resolved {
        ResolvedModel::Alias { alias, .. } => Some((*alias).to_string()),
        ResolvedModel::PinnedRaw { mapping, .. } => registry_alias_for_mapping(mapping),
    }
}

/// Registry alias for a unique raw mapping, when one is published.
pub(crate) fn registry_alias_for_mapping(mapping: &ProviderMapping) -> Option<String> {
    for published in crate::alias::published_aliases() {
        match crate::alias::resolve(&published) {
            Ok(ResolvedModel::Alias {
                alias, mappings, ..
            }) => {
                if mappings.iter().any(|candidate| {
                    candidate.provider_id == mapping.provider_id
                        && candidate.upstream_model == mapping.upstream_model
                }) {
                    return Some(alias.to_string());
                }
            }
            Ok(ResolvedModel::PinnedRaw { .. }) | Err(_) => {}
        }
    }
    None
}

/// Request / alias / upstream identity persisted on every forward log row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NativeLogIdentity {
    pub requested_model: String,
    pub resolved_alias: Option<String>,
    pub upstream_model: String,
}

/// Carry materialization identity into logs without inferring at the DB layer.
pub(crate) fn native_log_identity(plan: &RequestPlan) -> NativeLogIdentity {
    let requested_model = plan.log_requested_model().to_string();
    let upstream_model = plan.log_upstream_model().to_string();
    let resolved_alias = plan
        .resolved_alias
        .clone()
        .filter(|alias| !alias.is_empty())
        .or_else(|| resolved_alias_for_name(&requested_model))
        .or_else(|| {
            plan.original_model
                .as_deref()
                .and_then(resolved_alias_for_name)
        })
        .or_else(|| resolved_alias_for_name(&upstream_model));
    NativeLogIdentity {
        requested_model,
        resolved_alias,
        upstream_model,
    }
}

pub(crate) fn resolved_alias_for_name(name: &str) -> Option<String> {
    match crate::alias::resolve(name) {
        Ok(resolved) => resolved_alias_from_model(&resolved),
        Err(_) => None,
    }
}

/// Preserve original casing when the client name already identifies this mapping.
pub(crate) fn upstream_model_for(requested: &str, canonical: &str) -> String {
    if normalize_model_name(requested) == normalize_model_name(canonical) {
        requested.to_string()
    } else {
        canonical.to_string()
    }
}

#[cfg(test)]
struct MappingPlan {
    mapping: ProviderMapping,
    plan: RequestPlan,
}

#[allow(clippy::too_many_arguments)]
#[cfg(test)]
pub(crate) fn materialize_account_routes_with_bindings(
    accounts: &[Account],
    config: &AppConfig,
    parsed: &ParsedClientRequest,
    resolved: &ResolvedModel,
    client_model: &str,
    routing_model: &str,
    free_available: bool,
    custom_runtimes: &std::collections::HashMap<String, CustomAccountRuntime>,
    goat_runtimes: &std::collections::HashMap<String, GoatAccountRuntime>,
    cpa_base_url: Option<&str>,
    contracts: &EffectiveContractSet,
    dynamics: &[crate::dynamic::DynamicProviderRuntime],
    bindings: &InferenceBindingIndex,
    projection: Option<&DestinationProjection>,
) -> Result<MaterializedRouteSet, ProtocolError> {
    match resolved {
        ResolvedModel::PinnedRaw { mapping, .. } => {
            let zen_only = mapping_is_zen_free(mapping);
            let plan = materialize_mapping_plan(
                config,
                parsed,
                client_model,
                routing_model,
                mapping,
                resolved_alias_from_model(resolved),
                None,
                cpa_base_url,
                contracts,
            )?;
            collect_mapping_plans(
                accounts,
                config,
                parsed,
                client_model,
                routing_model,
                resolved_alias_from_model(resolved),
                vec![MappingPlan {
                    mapping: mapping.clone(),
                    plan,
                }],
                zen_only,
                Vec::new(),
                custom_runtimes,
                goat_runtimes,
                contracts,
                dynamics,
                bindings,
                projection,
            )
        }
        ResolvedModel::Alias {
            mappings, alias, ..
        } => {
            let routeable: Vec<ProviderMapping> = mappings
                .iter()
                .filter(|mapping| mapping.routeable)
                .cloned()
                .collect();
            let zen_only = !routeable.is_empty() && routeable.iter().all(mapping_is_zen_free);
            let mut plans = Vec::new();
            let mut rejections = Vec::new();
            let mut first_materialization_error = None;
            let resolved_alias = Some(alias.to_string());
            for mapping in &routeable {
                if mapping_is_zen_free(mapping) && !free_available && !zen_only {
                    continue;
                }
                match materialize_mapping_plan(
                    config,
                    parsed,
                    client_model,
                    routing_model,
                    mapping,
                    resolved_alias.clone(),
                    None,
                    cpa_base_url,
                    contracts,
                ) {
                    Ok(plan) => plans.push(MappingPlan {
                        mapping: mapping.clone(),
                        plan,
                    }),
                    Err(error) => {
                        rejections.push(mapping_rejection(
                            RouteRejectionCode::MappingProtocolIncompatible,
                            mapping,
                            format!(
                                "{}/{} mapping `{}`: {error}",
                                mapping.provider_id, mapping.provider_id, mapping.upstream_model
                            ),
                        ));
                        first_materialization_error.get_or_insert(error);
                    }
                }
            }

            // Preserve the existing pure-builtin 400 when every actual
            // mapping rejects the request. Mixed resolutions continue so a
            // compatible Custom account can still be materialized below.
            if plans.is_empty()
                && let Some(error) = first_materialization_error
            {
                return Err(error);
            }

            collect_mapping_plans(
                accounts,
                config,
                parsed,
                client_model,
                routing_model,
                Some(alias.to_string()),
                plans,
                zen_only,
                rejections,
                custom_runtimes,
                goat_runtimes,
                contracts,
                dynamics,
                bindings,
                projection,
            )
        }
    }
}

#[allow(clippy::too_many_arguments)]
#[cfg(test)]
fn materialize_mapping_plan(
    config: &AppConfig,
    parsed: &ParsedClientRequest,
    client_model: &str,
    routing_model: &str,
    mapping: &ProviderMapping,
    resolved_alias: Option<String>,
    original_model: Option<String>,
    cpa_base_url: Option<&str>,
    contracts: &EffectiveContractSet,
) -> Result<RequestPlan, ProtocolError> {
    let adapter_kind = mapping_adapter_kind(mapping);
    let channel = if adapter_kind == Some(ProviderAdapterKind::ZenFree) {
        UpstreamChannel::Free
    } else {
        // GOAT / Configurable HTTP share the Go channel discriminator.
        // Custom is rematerialized per account with that account's configured
        // protocol. Configurable HTTP is not a base class.
        UpstreamChannel::Go
    };
    let model = if adapter_kind == Some(ProviderAdapterKind::ConfigurableHttp) {
        routing_model.to_string()
    } else if original_model.is_some() {
        mapping.upstream_model.to_string()
    } else {
        upstream_model_for(routing_model, &mapping.upstream_model)
    };
    let forced_upstream = if adapter_kind == Some(ProviderAdapterKind::ConfigurableHttp) {
        Some(parsed.client)
    } else if adapter_kind == Some(ProviderAdapterKind::Cpa) {
        // CPA owns the model's upstream choice. Only Gemini is client-only
        // here and must be converted to a protocol exposed by CPA.
        Some(match parsed.client {
            ApiFormat::Gemini => ApiFormat::ChatCompletions,
            protocol => protocol,
        })
    } else {
        Some(
            contracts
                .select_for_mapping(mapping, parsed.client, &model)
                .map_err(|error| ProtocolError::new(error.message))?,
        )
    };
    let mut plan = materialize_channel_plan(
        config,
        parsed,
        client_model,
        &model,
        resolved_alias,
        channel,
        original_model,
        forced_upstream,
        None,
    )?;
    if adapter_kind == Some(ProviderAdapterKind::Cpa) {
        plan.upstream_base_override = Some(
            cpa_base_url
                .ok_or_else(|| ProtocolError::new("CPA is not configured"))?
                .to_string(),
        );
    }
    Ok(plan)
}

#[allow(clippy::too_many_arguments)]
#[cfg(test)]
fn materialize_channel_plan(
    config: &AppConfig,
    parsed: &ParsedClientRequest,
    client_model: &str,
    model: &str,
    resolved_alias: Option<String>,
    channel: UpstreamChannel,
    original_model: Option<String>,
    forced_upstream: Option<ApiFormat>,
    custom_route: Option<CustomRouteSpec>,
) -> Result<RequestPlan, ProtocolError> {
    let base =
        resolve_upstream_base(channel, &config.upstream_base_url).map_err(ProtocolError::new)?;
    materialize_parsed_request(
        parsed,
        &MaterializeSpec {
            client_model: client_model.to_string(),
            upstream_model: model.to_string(),
            resolved_alias,
            channel,
            upstream_base_override: match channel {
                UpstreamChannel::Free => Some(base),
                UpstreamChannel::Go => None,
            },
            original_model,
            forced_upstream,
            custom_route,
        },
    )
}

#[allow(clippy::too_many_arguments)]
#[cfg(test)]
fn materialize_custom_account_plan(
    account: &Account,
    runtime: Option<&CustomAccountRuntime>,
    config: &AppConfig,
    parsed: &ParsedClientRequest,
    client_model: &str,
    routing_model: &str,
    resolved_alias: Option<String>,
    contracts: &EffectiveContractSet,
) -> Result<RequestPlan, ProtocolError> {
    let runtime = runtime.ok_or_else(|| {
        ProtocolError::new(format!(
            "Custom account `{}` is missing a persisted API URL and upstream protocol",
            account.name
        ))
    })?;
    if !runtime.eligible() {
        return Err(ProtocolError::new(format!(
            "Custom account `{}` is not enabled, ready, and configured with a non-empty Key",
            account.name
        )));
    }
    let capability = runtime
        .capability_matching_public(routing_model)
        .ok_or_else(|| {
            ProtocolError::new(format!(
                "Custom account `{}` did not declare model `{routing_model}`",
                account.name
            ))
        })?;
    let resolved_alias = resolved_alias
        .filter(|alias| !alias.is_empty())
        .or_else(|| Some(capability.public_model.clone()));
    let contract = contracts
        .scope(&ContractScope::custom_endpoint(&account.id))
        .ok_or_else(|| {
            ProtocolError::new(format!(
                "no effective contract for custom_endpoint `{}`",
                account.id
            ))
        })?;
    // Every Custom account has one declared upstream protocol. The contract
    // passes the same client format through or converts every other format to it.
    let upstream = crate::provider_contracts::select_upstream_protocol(
        contract,
        parsed.client,
        &capability.public_model,
    )
    .map_err(|error| ProtocolError::new(error.message))?;
    let endpoint_url = runtime
        .route_override_matching_public(&capability.public_model)
        .map(|route| route.endpoint_url.clone())
        .unwrap_or_else(|| runtime.config.endpoint_url.clone());
    materialize_channel_plan(
        config,
        parsed,
        client_model,
        &capability.upstream_model,
        resolved_alias,
        UpstreamChannel::Go,
        None,
        Some(upstream),
        Some(CustomRouteSpec {
            endpoint_url,
            auth_kind: runtime.auth_kind,
        }),
    )
}

#[cfg(test)]
struct DynamicPlanNames<'a> {
    client_model: &'a str,
    routing_model: &'a str,
    resolved_alias: Option<String>,
    mapping_upstream: &'a str,
}

#[cfg(test)]
fn materialize_dynamic_account_plan(
    account: &Account,
    runtime: Option<&crate::dynamic::DynamicProviderRuntime>,
    config: &AppConfig,
    parsed: &ParsedClientRequest,
    names: DynamicPlanNames<'_>,
) -> Result<RequestPlan, ProtocolError> {
    let runtime = runtime.ok_or_else(|| {
        ProtocolError::new(format!(
            "dynamic provider `{}` is not in the request snapshot",
            account.provider_id
        ))
    })?;
    if runtime.auth_kind.requires_key() && account.key_cipher.trim().is_empty() {
        return Err(ProtocolError::new(format!(
            "account `{}` has no stored Key",
            account.name
        )));
    }
    let selected = runtime
        .mapping_for_public(names.routing_model)
        .or_else(|| runtime.mapping_for_upstream(names.mapping_upstream))
        .or_else(|| runtime.mapping_for_upstream(names.routing_model))
        .ok_or_else(|| {
            ProtocolError::new(format!(
                "dynamic provider `{}` has no mapping for `{}`",
                runtime.name, names.routing_model
            ))
        })?;
    let route = runtime.effective_route(selected);
    let upstream = crate::provider_contracts::select_enabled_upstream(
        parsed.client,
        route.protocol,
        std::slice::from_ref(&route.protocol),
        std::slice::from_ref(&route.protocol),
    )
    .map_err(|error| ProtocolError::new(error.message))?;
    materialize_channel_plan(
        config,
        parsed,
        names.client_model,
        &selected.upstream_model,
        names
            .resolved_alias
            .or_else(|| Some(selected.public_model.clone())),
        UpstreamChannel::Go,
        None,
        Some(upstream),
        Some(CustomRouteSpec {
            endpoint_url: route.endpoint_url,
            auth_kind: runtime.auth_kind,
        }),
    )
}

#[cfg(test)]
fn mapping_is_command_code_goat(mapping: &ProviderMapping) -> bool {
    mapping_adapter_kind(mapping) == Some(ProviderAdapterKind::CommandCodeGoat)
}

#[cfg(test)]
struct RoutingAccount<'a> {
    account: &'a Account,
    destination: Option<&'a Destination>,
    credential_enabled: Option<bool>,
}

#[cfg(test)]
fn routing_accounts<'a>(
    accounts: &'a [Account],
    projection: Option<&'a DestinationProjection>,
) -> Vec<RoutingAccount<'a>> {
    let Some(projection) = projection else {
        return accounts
            .iter()
            .map(|account| RoutingAccount {
                account,
                destination: None,
                credential_enabled: None,
            })
            .collect();
    };
    let by_id: HashMap<&str, &Account> = accounts
        .iter()
        .map(|account| (account.id.as_str(), account))
        .collect();
    let dest_by_id: HashMap<&str, &Destination> = projection
        .destinations
        .iter()
        .map(|destination| (destination.id.as_str(), destination))
        .collect();
    let mut seen = std::collections::HashSet::new();
    let mut rows = Vec::new();
    for credential in &projection.credentials {
        let Some(account) = by_id.get(credential.legacy_account_id.as_str()).copied() else {
            continue;
        };
        seen.insert(account.id.as_str());
        rows.push(RoutingAccount {
            account,
            destination: dest_by_id.get(credential.destination_id.as_str()).copied(),
            credential_enabled: Some(credential.enabled),
        });
    }
    for account in accounts {
        if seen.contains(account.id.as_str()) {
            continue;
        }
        rows.push(RoutingAccount {
            account,
            destination: None,
            credential_enabled: None,
        });
    }
    rows
}

#[cfg(test)]
pub(crate) fn destination_matches_mapping(
    destination: &Destination,
    mapping: &ProviderMapping,
) -> bool {
    match &destination.legacy {
        LegacyDestinationRef::Builtin(id) | LegacyDestinationRef::Dynamic(id) => {
            id == &mapping.provider_id
        }
        LegacyDestinationRef::CustomAccount(_) | LegacyDestinationRef::PlatformParent(_) => {
            destination.adapter == ocg_domain::destination::AdapterKind::Http
                && mapping_is_custom_http_catalog(mapping)
        }
    }
}

#[cfg(test)]
fn account_matches_mapping(
    account: &Account,
    destination: Option<&Destination>,
    mapping: &ProviderMapping,
) -> bool {
    match destination {
        Some(destination) => destination_matches_mapping(destination, mapping),
        None => account.provider_id == mapping.provider_id,
    }
}

#[allow(clippy::too_many_arguments)]
#[cfg(test)]
fn collect_mapping_plans(
    accounts: &[Account],
    config: &AppConfig,
    parsed: &ParsedClientRequest,
    client_model: &str,
    routing_model: &str,
    resolved_alias: Option<String>,
    plans: Vec<MappingPlan>,
    free_only: bool,
    mut rejections: Vec<RouteRejection>,
    custom_runtimes: &std::collections::HashMap<String, CustomAccountRuntime>,
    goat_runtimes: &std::collections::HashMap<String, GoatAccountRuntime>,
    contracts: &EffectiveContractSet,
    dynamics: &[crate::dynamic::DynamicProviderRuntime],
    bindings: &InferenceBindingIndex,
    projection: Option<&DestinationProjection>,
) -> Result<MaterializedRouteSet, ProtocolError> {
    let mut routes = Vec::new();
    for row in routing_accounts(accounts, projection) {
        let account = row.account;
        if row.credential_enabled == Some(false) {
            rejections.push(account_rejection(
                RouteRejectionCode::CredentialDisabled,
                account,
                "credential is disabled",
            ));
            continue;
        }
        let binding = bindings
            .get(&account.id)
            .cloned()
            .unwrap_or_else(default_inference_binding);
        if !binding.enabled {
            rejections.push(account_rejection(
                RouteRejectionCode::BindingDisabled,
                account,
                "inference binding is disabled",
            ));
            continue;
        }
        for candidate in &plans {
            if !account_matches_mapping(account, row.destination, &candidate.mapping) {
                continue;
            }
            // Scope is per candidate. OR-ing every same-provider plan would let
            // an allowlisted sibling model admit a request for X (D01).
            if !binding_allows_requested_model(
                &binding.model_scope,
                client_model,
                routing_model,
                std::iter::once(candidate.plan.model.as_str()),
            ) {
                rejections.push(account_rejection(
                    RouteRejectionCode::ModelScopeDenied,
                    account,
                    format!("model `{routing_model}` is outside binding model scope"),
                ));
                continue;
            }
            if routes.iter().any(|route: &MaterializedCandidate| {
                route.routing.account.id == account.id
                    && route.routing.channel == candidate.plan.channel
            }) {
                continue;
            }
            if mapping_is_command_code_goat(&candidate.mapping) {
                match goat_runtimes.get(&account.id) {
                    Some(runtime) if runtime.eligible() => {}
                    Some(_) => {
                        rejections.push(account_rejection(
                            RouteRejectionCode::GoatNotEligible,
                            account,
                            "Command Code GOAT account is not eligible for routing",
                        ));
                        continue;
                    }
                    None => {
                        rejections.push(account_rejection(
                            RouteRejectionCode::GoatUnverified,
                            account,
                            "Command Code GOAT production inference endpoint, auth, protocol, and model catalog are not verified; route is disabled",
                        ));
                        continue;
                    }
                }
            }
            let custom_owned = match row.destination {
                Some(destination) => matches!(
                    destination.legacy,
                    LegacyDestinationRef::CustomAccount(_)
                        | LegacyDestinationRef::PlatformParent(_)
                ),
                None => crate::dynamic::find_runtime(dynamics, &account.provider_id).is_none(),
            };
            let plan = if mapping_is_configurable_http(&candidate.mapping) && custom_owned {
                match materialize_custom_account_plan(
                    account,
                    custom_runtimes.get(&account.id),
                    config,
                    parsed,
                    client_model,
                    routing_model,
                    resolved_alias.clone(),
                    contracts,
                ) {
                    Ok(plan) => plan,
                    Err(error) => {
                        rejections.push(account_rejection(
                            RouteRejectionCode::CandidateMaterializationFailed,
                            account,
                            error,
                        ));
                        continue;
                    }
                }
            } else if mapping_is_configurable_http(&candidate.mapping) {
                match materialize_dynamic_account_plan(
                    account,
                    crate::dynamic::find_runtime(dynamics, &account.provider_id),
                    config,
                    parsed,
                    DynamicPlanNames {
                        client_model,
                        routing_model,
                        resolved_alias: resolved_alias.clone(),
                        mapping_upstream: &candidate.mapping.upstream_model,
                    },
                ) {
                    Ok(plan) => plan,
                    Err(error) => {
                        rejections.push(account_rejection(
                            RouteRejectionCode::CandidateMaterializationFailed,
                            account,
                            error,
                        ));
                        continue;
                    }
                }
            } else {
                candidate.plan.clone()
            };
            let adapter = row
                .destination
                .map(|destination| ProviderAdapterKind::from(destination.adapter))
                .or_else(|| mapping_adapter_kind(&candidate.mapping))
                .unwrap_or(ProviderAdapterKind::ConfigurableHttp);
            match provider_adapter::supports_production_plan(
                account, adapter, config, &plan, contracts, dynamics,
            ) {
                Ok(()) => {
                    routes.push(MaterializedCandidate {
                        routing: RoutingCandidate {
                            adapter,
                            account: account.clone(),
                            channel: plan.channel,
                            resolved_model: plan.model.clone(),
                        },
                        plan,
                    });
                    break;
                }
                Err(error) => rejections.push(account_rejection(
                    RouteRejectionCode::ProductionRouteUnsupported,
                    account,
                    error,
                )),
            }
        }
    }
    let rejected: Vec<String> = rejections
        .iter()
        .map(|rejection| rejection.detail.clone())
        .collect();
    let incompatibility = (routes.is_empty() && !rejected.is_empty()).then(|| {
        format!(
            "no compatible provider account for model `{client_model}` and {:?}: {}",
            parsed.client,
            rejected.join("; ")
        )
    });
    Ok(MaterializedRouteSet {
        routes,
        free_only,
        incompatibility,
        rejected,
        rejections,
    })
}

/// The persisted model selected at entry. Authorization compares this identity;
/// it never selects a different row after an edit.
#[derive(Clone, Debug)]
pub(crate) struct FrozenTarget {
    pub destination: Destination,
    pub model: ocg_domain::destination::CatalogModel,
    pub endpoint_id: String,
}

#[derive(Clone, Debug)]
pub(crate) struct ExecutionRoute {
    pub routing: RoutingCandidate<crate::routing_snapshot::ExecutionCredential>,
    pub plan: RequestPlan,
    pub spec: crate::gateway::attempt::AttemptSpec,
    pub target: FrozenTarget,
}

pub(crate) struct ExecutionRouteSet {
    pub routes: Vec<ExecutionRoute>,
    pub free_only: bool,
    pub incompatibility: Option<String>,
    pub rejections: Vec<RouteRejection>,
}

pub(crate) fn resolved_contains_model(
    resolved: &ResolvedModel,
    destination: &Destination,
    model: &ocg_domain::destination::CatalogModel,
    requested: &str,
) -> bool {
    use ocg_domain::destination::{AdapterKind, ModelResolution};
    let mapping_matches = |mapping: &ProviderMapping| {
        if !mapping.routeable {
            return false;
        }
        if destination.adapter == AdapterKind::Http {
            match destination.model_resolution {
                ModelResolution::PublicOnly => {
                    mapping.provider_id == crate::provider::CUSTOM_PROVIDER_ID
                        && custom_model_id_matches(&model.public_model, requested)
                }
                ModelResolution::PublicAndUpstream => {
                    let requested_public = destination
                        .catalog
                        .iter()
                        .find(|row| custom_model_id_matches(&row.public_model, requested));
                    mapping.provider_id == destination.id
                        && mapping.upstream_model == model.upstream_model
                        && requested_public.is_none_or(|row| row.public_model == model.public_model)
                }
                ModelResolution::AdapterDefined => false,
            }
        } else {
            crate::provider::ProviderRegistry::get_by_kind(ProviderAdapterKind::from(
                destination.adapter,
            ))
            .is_some_and(|descriptor| descriptor.provider_id == mapping.provider_id)
                && normalize_model_name(&mapping.upstream_model)
                    == normalize_model_name(&model.upstream_model)
        }
    };
    match resolved {
        ResolvedModel::Alias { mappings, .. } => mappings.iter().any(mapping_matches),
        ResolvedModel::PinnedRaw { mapping, .. } => mapping_matches(mapping),
    }
}

pub(crate) fn endpoint_id_for_target(
    credential: &crate::routing_snapshot::ExecutionCredential,
    destination: &Destination,
    model: &ocg_domain::destination::CatalogModel,
    upstream: ApiFormat,
) -> Result<String, String> {
    use ocg_domain::connection::{ConnectionId, EndpointOperation, endpoint_id_for};
    use ocg_domain::credential::assigned_endpoints_for_routes;
    use ocg_domain::destination::AdapterKind;
    let connection: ConnectionId =
        serde_json::from_value(serde_json::json!(credential.authorization_connection_id))
            .map_err(|e| e.to_string())?;
    if credential.authorization_connection_id.is_empty() {
        return Err("missing authorization connection identity".into());
    }
    let protocol = match upstream {
        ApiFormat::ChatCompletions => ocg_domain::destination::Protocol::ChatCompletions,
        ApiFormat::Responses => ocg_domain::destination::Protocol::Responses,
        ApiFormat::Messages => ocg_domain::destination::Protocol::Messages,
        ApiFormat::Gemini => return Err("client-only upstream protocol".into()),
    };
    if destination.adapter != AdapterKind::Http {
        return Ok(endpoint_id_for(&connection, EndpointOperation::from(protocol)).to_string());
    }
    let routes = ocg_domain::destination::http_configured_routes(destination);
    let selected = ocg_domain::destination::http_model_route(destination, model, protocol)
        .ok_or("missing configured HTTP protocol route")?;
    assigned_endpoints_for_routes(&connection, &routes)
        .into_iter()
        .zip(routes)
        .find(|(_, route)| {
            route.operation == EndpointOperation::from(protocol)
                && route.url.as_deref() == Some(selected.endpoint_url.as_str())
        })
        .map(|(assigned, _)| assigned.id)
        .ok_or_else(|| "missing persisted route grant identity".into())
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn materialize_execution_routes(
    snapshot: &crate::routing_snapshot::RoutingSnapshot,
    config: &AppConfig,
    parsed: &ParsedClientRequest,
    resolved: &ResolvedModel,
    client_model: &str,
    routing_model: &str,
    cpa_base_url: Option<&str>,
) -> Result<ExecutionRouteSet, ProtocolError> {
    use ocg_domain::destination::{AdapterKind, AuthScheme};
    let mut routes = Vec::new();
    let mut rejections = Vec::new();
    let mut conversion_error = None;
    let mut conversion_failures = 0;
    for credential in &snapshot.credentials {
        let Some(destination) = snapshot
            .projection
            .destinations
            .iter()
            .find(|d| d.id == credential.destination_id)
        else {
            continue;
        };
        let reject = |code, detail: String| RouteRejection {
            code,
            detail,
            account_id: Some(credential.id.clone()),
            provider_id: Some(credential.provider_id.clone()),
            upstream_model: None,
        };
        if !destination
            .catalog
            .iter()
            .any(|m| resolved_contains_model(resolved, destination, m, routing_model))
        {
            continue;
        }
        if !destination.enabled || !credential.enabled {
            rejections.push(reject(
                RouteRejectionCode::CredentialDisabled,
                "credential or destination disabled".into(),
            ));
            continue;
        }
        if !credential.binding_enabled {
            rejections.push(reject(
                RouteRejectionCode::BindingDisabled,
                "inference binding disabled".into(),
            ));
            continue;
        }
        for model in &destination.catalog {
            if !resolved_contains_model(resolved, destination, model, routing_model) {
                continue;
            }
            if !model.enabled || model.protocols.is_empty() {
                rejections.push(reject(
                    RouteRejectionCode::MappingProtocolIncompatible,
                    "model has no enabled protocol".into(),
                ));
                continue;
            }
            if !binding_allows_requested_model(
                &credential.scope,
                client_model,
                routing_model,
                [&model.upstream_model, &model.public_model],
            ) {
                rejections.push(reject(
                    RouteRejectionCode::ModelScopeDenied,
                    "model is outside credential scope".into(),
                ));
                continue;
            }
            if destination.auth_scheme != AuthScheme::None && credential.key_cipher.is_empty() {
                rejections.push(reject(
                    RouteRejectionCode::CandidateMaterializationFailed,
                    "credential has no Key".into(),
                ));
                continue;
            }
            let preferred = model
                .preferred
                .or_else(|| model.protocols.first().copied())
                .ok_or_else(|| ProtocolError::new("missing preferred protocol"))?;
            let upstream = match crate::provider_contracts::select_enabled_upstream(
                parsed.client,
                preferred,
                &model.protocols,
                &model.protocols,
            ) {
                Ok(upstream) => upstream,
                Err(error) => {
                    rejections.push(reject(
                        RouteRejectionCode::MappingProtocolIncompatible,
                        error.message,
                    ));
                    continue;
                }
            };
            let adapter = ProviderAdapterKind::from(destination.adapter);
            let channel = crate::routing_runtime::channel_for_adapter(adapter);
            let custom_route = if destination.adapter == AdapterKind::Http {
                let selected_protocol = match upstream {
                    ApiFormat::ChatCompletions => {
                        ocg_domain::destination::Protocol::ChatCompletions
                    }
                    ApiFormat::Responses => ocg_domain::destination::Protocol::Responses,
                    ApiFormat::Messages => ocg_domain::destination::Protocol::Messages,
                    ApiFormat::Gemini => {
                        return Err(ProtocolError::new("client-only upstream protocol"));
                    }
                };
                let Some(selected) = ocg_domain::destination::http_model_route(
                    destination,
                    model,
                    selected_protocol,
                ) else {
                    rejections.push(reject(
                        RouteRejectionCode::ProductionRouteUnsupported,
                        "model protocol has no configured route".into(),
                    ));
                    continue;
                };
                Some(CustomRouteSpec {
                    endpoint_url: selected.endpoint_url,
                    auth_kind: match selected.auth_scheme {
                        AuthScheme::Bearer => ocg_domain::dynamic::DynamicAuthKind::Bearer,
                        AuthScheme::XApiKey => ocg_domain::dynamic::DynamicAuthKind::XApiKey,
                        AuthScheme::None => ocg_domain::dynamic::DynamicAuthKind::None,
                    },
                })
            } else {
                None
            };
            let plan = materialize_parsed_request(
                parsed,
                &MaterializeSpec {
                    client_model: client_model.into(),
                    upstream_model: if destination.adapter == AdapterKind::Http {
                        model.upstream_model.clone()
                    } else {
                        upstream_model_for(routing_model, &model.upstream_model)
                    },
                    resolved_alias: resolved_alias_from_model(resolved),
                    channel,
                    upstream_base_override: if destination.adapter == AdapterKind::Cpa {
                        cpa_base_url
                            .map(str::to_string)
                            .or_else(|| destination.base_url.clone())
                    } else {
                        None
                    },
                    original_model: None,
                    forced_upstream: Some(upstream),
                    custom_route,
                },
            );
            let plan = match plan {
                Ok(plan) => plan,
                Err(error) => {
                    conversion_failures += 1;
                    conversion_error.get_or_insert_with(|| error.clone());
                    rejections.push(reject(
                        RouteRejectionCode::CandidateMaterializationFailed,
                        error.message,
                    ));
                    continue;
                }
            };
            let spec = match provider_adapter::resolve_execution_route(
                credential,
                destination,
                config,
                &plan,
            ) {
                Ok(spec) => spec,
                Err(error) => {
                    rejections.push(reject(
                        RouteRejectionCode::ProductionRouteUnsupported,
                        error,
                    ));
                    continue;
                }
            };
            let endpoint_id = endpoint_id_for_target(credential, destination, model, upstream)
                .map_err(ProtocolError::new)?;
            routes.push(ExecutionRoute {
                routing: RoutingCandidate {
                    account: credential.clone(),
                    adapter,
                    channel,
                    resolved_model: model.upstream_model.clone(),
                },
                plan,
                spec,
                target: FrozenTarget {
                    destination: destination.clone(),
                    model: model.clone(),
                    endpoint_id,
                },
            });
            break;
        }
    }
    // Only real candidate conversions can reject client features. Unavailable
    // credentials still produce availability errors, and any viable route wins.
    if routes.is_empty()
        && conversion_failures
            + rejections
                .iter()
                .filter(|rejection| {
                    rejection.code == RouteRejectionCode::MappingProtocolIncompatible
                })
                .count()
            == rejections.len()
    {
        if let Some(error) = conversion_error {
            return Err(error);
        }
    }
    let free_only = !routes.is_empty()
        && routes
            .iter()
            .all(|r| r.routing.channel == UpstreamChannel::Free);
    let incompatibility = (routes.is_empty() && !rejections.is_empty()).then(|| {
        rejections
            .iter()
            .map(|r| r.detail.as_str())
            .collect::<Vec<_>>()
            .join("; ")
    });
    Ok(ExecutionRouteSet {
        routes,
        free_only,
        incompatibility,
        rejections,
    })
}

#[cfg(test)]
mod tests;

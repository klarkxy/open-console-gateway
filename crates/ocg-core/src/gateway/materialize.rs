//! Candidate request materialization for the alias runtime.
//!
//! # Adapter interface
//!
//! Later provider adapters should treat this module as the boundary between
//! a parsed client request and an upstream call:
//!
//! 1. Parse the client protocol **once** with
//!    [`parse_client_request`] / [`parse_gemini_request`].
//! 2. Resolve the requested name through [`crate::alias::resolve`]. Alias
//!    results may follow account order, sticky, and fallback. A unique raw
//!    upstream ID is pinned to its single mapping; overlapping raw IDs return
//!    [`crate::alias::AMBIGUOUS_MODEL_ID`].
//! 3. Build candidate plans from [`ResolvedModel`] mappings. Match accounts in saved
//!    account order through [`super::provider_adapter::supports_production_plan`], using
//!    mapping order only as the per-account tie-break. Protocol selection
//!    uses the OpenCode `MODEL_PROTOCOLS` table for Go/Zen upstream models
//!    and Command Code family rules (Chat vs Messages) for GOAT IDs.
//!    **Never** trial a billable inference path.
//!    Adapter identity is [`crate::provider::ProviderAdapterKind`]; Custom is
//!    Configurable HTTP, not a base class.
//! 4. Ask [`super::provider_adapter::resolve_route_with_dynamics`] for endpoint + auth.
//!    Production GOAT uses the official Provider API after a saved verified
//!    catalog snapshot. The official slash raw ID pins to command-code/goat
//!    without stealing Go kebab aliases.
//!
//! OpenCode Go, Zen Free, and the shared Configurable HTTP materialization
//! boundary are implemented here.

use crate::alias::{ProviderMapping, ResolveError, ResolvedModel};
use crate::custom::CustomAccountRuntime;
use crate::gateway::free_models::resolve_upstream_base;
use crate::gateway::protocol::{
    CustomRouteSpec, MaterializeSpec, ParsedClientRequest, ProtocolError, RequestPlan,
    materialize_parsed_request,
};
use crate::gateway::provider_adapter;
use crate::gateway::routing::RoutingCandidate;
use crate::goat::GoatAccountRuntime;
use crate::kernel::ids::normalize_model_name;
use crate::kernel::protocol::ApiFormat;
use crate::models::{Account, AppConfig, UpstreamChannel};
use crate::provider::ProviderAdapterKind;
use crate::provider_contracts::{ContractScope, EffectiveContractSet};
use axum::http::StatusCode;
use bytes::Bytes;
use ocg_domain::credential::{ModelScope, model_scope_allows};
use std::collections::HashMap;

pub use crate::gateway::protocol::{
    parse_client_request as parse_client, parse_gemini_request as parse_gemini,
};

#[derive(Debug, Clone)]
pub(crate) struct MaterializedCandidate {
    pub routing: RoutingCandidate,
    pub plan: RequestPlan,
}

#[derive(Debug, Clone)]
pub(crate) struct MaterializedRouteSet {
    pub routes: Vec<MaterializedCandidate>,
    pub free_only: bool,
    pub incompatibility: Option<String>,
    /// Account/mapping rejection notes collected while building candidates.
    /// Empty when every considered account produced a route. Live send ignores
    /// this list; the read-only shadow planner surfaces it for compare.
    pub rejected: Vec<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct InferenceBindingGate {
    pub enabled: bool,
    pub model_scope: ModelScope,
}

pub(crate) type InferenceBindingIndex = HashMap<String, InferenceBindingGate>;

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

/// Diagnostics are not a candidate protocol decision. If a resolution can use
/// Custom, Command Code GOAT, or Ollama Cloud, preserve the client wire format
/// until each actual mapping/account is materialized. Unique GOAT and Ollama
/// catalog IDs are not in OpenCode `MODEL_PROTOCOLS`. Zen-only resolutions
/// default to Chat so unknown catalog `-free` rows (and their stripped Alias)
/// are not rejected against the Go protocol table. Pure builtin resolutions
/// keep their normal early validation.
pub(crate) fn diagnostic_forced_upstream(
    resolved: &ResolvedModel,
    client: ApiFormat,
) -> Option<ApiFormat> {
    if let ResolvedModel::PinnedRaw { mapping, .. } = resolved
        && mapping_adapter_kind(mapping) == Some(ProviderAdapterKind::Cpa)
    {
        return Some(
            crate::kernel::protocol::model_protocol(&mapping.upstream_model)
                .map(|profile| profile.preferred)
                .unwrap_or(ApiFormat::ChatCompletions),
        );
    }
    let zen_only = match resolved {
        ResolvedModel::PinnedRaw { mapping, .. } => mapping_is_zen_free(mapping),
        ResolvedModel::Alias { mappings, .. } => {
            let routeable: Vec<_> = mappings
                .iter()
                .filter(|mapping| mapping.routeable)
                .collect();
            !routeable.is_empty() && routeable.iter().all(|mapping| mapping_is_zen_free(mapping))
        }
    };
    if zen_only {
        return Some(ApiFormat::ChatCompletions);
    }
    let preserve_client = match resolved {
        ResolvedModel::PinnedRaw { mapping, .. } => mapping_preserves_client_wire(mapping),
        ResolvedModel::Alias { mappings, .. } => mappings
            .iter()
            .any(|mapping| mapping.routeable && mapping_preserves_client_wire(mapping)),
    };
    preserve_client.then_some(client)
}

fn mapping_preserves_client_wire(mapping: &ProviderMapping) -> bool {
    mapping_is_configurable_http(mapping)
        || mapping_is_command_code_goat(mapping)
        || mapping_is_ollama_cloud(mapping)
}

fn mapping_adapter_kind(mapping: &ProviderMapping) -> Option<ProviderAdapterKind> {
    crate::dynamic::adapter_kind_for(&mapping.provider_id, &[]).or_else(|| {
        uuid::Uuid::parse_str(&mapping.provider_id)
            .ok()
            .map(|_| ProviderAdapterKind::ConfigurableHttp)
    })
}

fn mapping_is_configurable_http(mapping: &ProviderMapping) -> bool {
    mapping_adapter_kind(mapping) == Some(ProviderAdapterKind::ConfigurableHttp)
}

fn mapping_is_zen_free(mapping: &ProviderMapping) -> bool {
    mapping_adapter_kind(mapping) == Some(ProviderAdapterKind::ZenFree)
}

fn mapping_is_ollama_cloud(mapping: &ProviderMapping) -> bool {
    mapping_adapter_kind(mapping) == Some(ProviderAdapterKind::OllamaCloud)
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

struct MappingPlan {
    mapping: ProviderMapping,
    plan: RequestPlan,
}

#[allow(dead_code, clippy::too_many_arguments)]
pub(crate) fn materialize_account_routes(
    accounts: &[Account],
    config: &AppConfig,
    parsed: &ParsedClientRequest,
    resolved: &ResolvedModel,
    client_model: &str,
    routing_model: &str,
    _client_body: &Bytes,
    free_available: bool,
    custom_runtimes: &std::collections::HashMap<String, CustomAccountRuntime>,
    goat_runtimes: &std::collections::HashMap<String, GoatAccountRuntime>,
    cpa_base_url: Option<&str>,
    contracts: &EffectiveContractSet,
    dynamics: &[crate::dynamic::DynamicProviderRuntime],
) -> Result<MaterializedRouteSet, ProtocolError> {
    materialize_account_routes_with_bindings(
        accounts,
        config,
        parsed,
        resolved,
        client_model,
        routing_model,
        _client_body,
        free_available,
        custom_runtimes,
        goat_runtimes,
        cpa_base_url,
        contracts,
        dynamics,
        &HashMap::new(),
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn materialize_account_routes_with_bindings(
    accounts: &[Account],
    config: &AppConfig,
    parsed: &ParsedClientRequest,
    resolved: &ResolvedModel,
    client_model: &str,
    routing_model: &str,
    _client_body: &Bytes,
    free_available: bool,
    custom_runtimes: &std::collections::HashMap<String, CustomAccountRuntime>,
    goat_runtimes: &std::collections::HashMap<String, GoatAccountRuntime>,
    cpa_base_url: Option<&str>,
    contracts: &EffectiveContractSet,
    dynamics: &[crate::dynamic::DynamicProviderRuntime],
    bindings: &InferenceBindingIndex,
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
                false,
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
            let mut rejected = Vec::new();
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
                    false,
                    cpa_base_url,
                    contracts,
                ) {
                    Ok(plan) => plans.push(MappingPlan {
                        mapping: mapping.clone(),
                        plan,
                    }),
                    Err(error) => {
                        rejected.push(format!(
                            "{}/{} mapping `{}`: {error}",
                            mapping.provider_id, mapping.provider_id, mapping.upstream_model
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
                rejected,
                custom_runtimes,
                goat_runtimes,
                contracts,
                dynamics,
                bindings,
            )
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn materialize_mapping_plan(
    config: &AppConfig,
    parsed: &ParsedClientRequest,
    client_model: &str,
    routing_model: &str,
    mapping: &ProviderMapping,
    resolved_alias: Option<String>,
    original_model: Option<String>,
    allow_go_fallback: bool,
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
    } else if adapter_kind == Some(ProviderAdapterKind::CommandCodeGoat) {
        Some(
            contracts
                .select_for_mapping(mapping, parsed.client, &model)
                .map_err(|error| ProtocolError::new(error.message))?,
        )
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
        allow_go_fallback,
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
fn materialize_channel_plan(
    config: &AppConfig,
    parsed: &ParsedClientRequest,
    client_model: &str,
    model: &str,
    resolved_alias: Option<String>,
    channel: UpstreamChannel,
    original_model: Option<String>,
    allow_go_fallback: bool,
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
            allow_go_fallback,
            forced_upstream,
            custom_route,
        },
    )
}

#[allow(clippy::too_many_arguments)]
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
    materialize_channel_plan(
        config,
        parsed,
        client_model,
        &capability.upstream_model,
        resolved_alias,
        UpstreamChannel::Go,
        None,
        false,
        Some(upstream),
        Some(CustomRouteSpec {
            endpoint_url: runtime.config.endpoint_url.clone(),
        }),
    )
}

struct DynamicPlanNames<'a> {
    client_model: &'a str,
    routing_model: &'a str,
    resolved_alias: Option<String>,
    mapping_upstream: &'a str,
}

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
        false,
        Some(upstream),
        Some(CustomRouteSpec {
            endpoint_url: route.endpoint_url,
        }),
    )
}

fn mapping_is_command_code_goat(mapping: &ProviderMapping) -> bool {
    mapping_adapter_kind(mapping) == Some(ProviderAdapterKind::CommandCodeGoat)
}

#[allow(clippy::too_many_arguments)]
fn collect_mapping_plans(
    accounts: &[Account],
    config: &AppConfig,
    parsed: &ParsedClientRequest,
    client_model: &str,
    routing_model: &str,
    resolved_alias: Option<String>,
    plans: Vec<MappingPlan>,
    free_only: bool,
    mut rejected: Vec<String>,
    custom_runtimes: &std::collections::HashMap<String, CustomAccountRuntime>,
    goat_runtimes: &std::collections::HashMap<String, GoatAccountRuntime>,
    contracts: &EffectiveContractSet,
    dynamics: &[crate::dynamic::DynamicProviderRuntime],
    bindings: &InferenceBindingIndex,
) -> Result<MaterializedRouteSet, ProtocolError> {
    let mut routes = Vec::new();
    for account in accounts {
        let binding = bindings
            .get(&account.id)
            .cloned()
            .unwrap_or_else(default_inference_binding);
        if !binding.enabled {
            rejected.push(format!(
                "{}/{} account `{}`: inference binding is disabled",
                account.provider_id, account.provider_id, account.name
            ));
            continue;
        }
        for candidate in &plans {
            if account.provider_id != candidate.mapping.provider_id {
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
                rejected.push(format!(
                    "{}/{} account `{}`: model `{routing_model}` is outside binding model scope",
                    account.provider_id, account.provider_id, account.name
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
                    Some(runtime) if runtime.serves(&candidate.plan.model) => {}
                    Some(_) => {
                        rejected.push(format!(
                            "{}/{} account `{}`: Command Code GOAT catalog does not include model `{}`",
                            account.provider_id,
                            account.provider_id,
                            account.name,
                            candidate.plan.model
                        ));
                        continue;
                    }
                    None => {
                        rejected.push(format!(
                            "{}/{} account `{}`: Command Code GOAT production inference endpoint, auth, protocol, and model catalog are not verified; route is disabled",
                            account.provider_id, account.provider_id, account.name
                        ));
                        continue;
                    }
                }
            }
            let plan = if mapping_is_configurable_http(&candidate.mapping)
                && crate::provider::is_custom_api(&account.provider_id)
            {
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
                        rejected.push(format!(
                            "{}/{} account `{}`: {error}",
                            account.provider_id, account.provider_id, account.name
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
                        rejected.push(format!(
                            "{}/{} account `{}`: {error}",
                            account.provider_id, account.provider_id, account.name
                        ));
                        continue;
                    }
                }
            } else {
                candidate.plan.clone()
            };
            match provider_adapter::supports_production_plan(
                account, config, &plan, contracts, dynamics,
            ) {
                Ok(()) => {
                    routes.push(MaterializedCandidate {
                        routing: RoutingCandidate {
                            account: account.clone(),
                            channel: plan.channel,
                            resolved_model: plan.model.clone(),
                        },
                        plan,
                    });
                    break;
                }
                Err(error) => rejected.push(format!(
                    "{}/{} account `{}`: {error}",
                    account.provider_id, account.provider_id, account.name
                )),
            }
        }
    }
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
    })
}

#[cfg(test)]
mod tests;

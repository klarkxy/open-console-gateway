//! Local/Zen Dashboard V3 provider control plane.
//!
//! Catalog, contracts, model capabilities, and saved Zen models are local
//! reads. Zen enablement and provider-scope protocol switches share the V3
//! CAS envelope. Zen catalog refresh uses the fixed official keyless directory.
//! Go/Zen protocol probes share the crate-root transport; Custom probes stay
//! account-owned on V2.

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, State};
use chrono::{DateTime, Utc};
#[cfg(debug_assertions)]
use std::collections::BTreeMap;
use std::collections::HashMap;

#[cfg(debug_assertions)]
use futures_util::StreamExt;
#[cfg(debug_assertions)]
use parking_lot::Mutex;
#[cfg(debug_assertions)]
use std::net::{Ipv4Addr, Ipv6Addr};
#[cfg(debug_assertions)]
use std::time::Duration;

use crate::alias;
use crate::kernel::protocol::supported_model_protocol_profiles;
#[cfg(debug_assertions)]
use crate::kernel::zen::{ZEN_MODELS_SOURCE_URL, parse_catalog};
use crate::kernel::zen::{ZenFreeModelCatalog, model_views};
use crate::models::{Account as ModelAccount, AppConfig};
use crate::protocol_probe::{self, ProtocolProbeContext, ProtocolProbeRunError};
use crate::provider::{
    BUILTIN_PLANS, BuiltinPlan, CUSTOM_PROVIDER_ID, ConnectionVerificationStatus, GO_OFFERING_ID,
    OPENCODE_PROVIDER_ID, ProviderAdapterKind, ProviderRegistry, ZEN_FREE_ACCOUNT_ID,
    default_verification_status,
};
use crate::provider_contracts::{
    self, ContractScope, EffectiveContractSet, EffectiveModelContract as DomainModelContract,
    EffectiveProtocolEvidence as DomainProtocolEvidence, PersistedModelProtocol,
    ProtocolSwitches as DomainProtocolSwitches,
};
use crate::state::CoreState;

use super::types::{
    AccountAuthScheme, AccountQuotaScope, AccountUpstreamProtocol, AccountVerificationStatus,
    CapabilitySummary, CardCapabilitySummary, ContractEvidenceSource, ContractScopeKind,
    ControlRevision, CustomEndpointContract, EffectiveCatalog, EffectiveModelContract,
    EffectiveModelProtocols, EffectiveProtocolEvidence, MutationExpectation, ProbeResultKind,
    ProtocolProbeRequest, ProtocolProbeResponse, ProtocolProbeResult, ProtocolSwitchUpdate,
    ProtocolSwitches, ProviderAccountChoice, ProviderCatalog, ProviderCatalogEntry,
    ProviderCatalogFormField, ProviderCatalogRiskNotice, ProviderContractGroup, ProviderContracts,
    ProviderModelCapability, ProviderOfferingChoice, ZenFreeModel, ZenFreeModels, ZenFreeSettings,
    ZenFreeSettingsUpdate,
};
use super::{V3ApiError, check_expectation, parse_mutation_json};

#[cfg(debug_assertions)]
const MAX_ZEN_BODY_BYTES: usize = 512 * 1024;
#[cfg(debug_assertions)]
const ZEN_REFRESH_TIMEOUT_SECS: u64 = 30;

#[cfg(debug_assertions)]
static ZEN_MODELS_SOURCE_OVERRIDES: Mutex<BTreeMap<u64, String>> = Mutex::new(BTreeMap::new());

/// Loopback-only Zen directory used by Dashboard V3 refresh tests.
///
/// Keyed by `CoreState::process_generation` so parallel harnesses cannot
/// overwrite each other. Compiled out of release production.
#[cfg(debug_assertions)]
pub fn set_zen_models_source_url_override_for_tests(process_generation: u64, url: Option<String>) {
    let mut overrides = ZEN_MODELS_SOURCE_OVERRIDES.lock();
    match url.and_then(|value| parse_loopback_http_url(&value)) {
        Some(canonical) => {
            overrides.insert(process_generation, canonical);
        }
        None => {
            overrides.remove(&process_generation);
        }
    }
}

#[cfg(debug_assertions)]
fn debug_zen_models_source_url(process_generation: u64) -> Option<String> {
    ZEN_MODELS_SOURCE_OVERRIDES
        .lock()
        .get(&process_generation)
        .cloned()
}

/// Accept only an unambiguous loopback HTTP(S) origin: parsed host must be
/// exactly `127.0.0.1`, `localhost`, or `::1`, with no userinfo, query, or
/// fragment. Prefix matching is not used.
#[cfg(debug_assertions)]
fn parse_loopback_http_url(url: &str) -> Option<String> {
    let parsed = reqwest::Url::parse(url.trim()).ok()?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return None;
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return None;
    }
    if parsed.query().is_some() || parsed.fragment().is_some() {
        return None;
    }
    if !host_is_exact_loopback(&parsed) {
        return None;
    }
    Some(parsed.as_str().to_string())
}

#[cfg(debug_assertions)]
fn host_is_exact_loopback(parsed: &reqwest::Url) -> bool {
    let Some(host) = parsed.host() else {
        return false;
    };
    let rendered = host.to_string();
    if let Some(inside) = rendered
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
    {
        return inside
            .parse::<Ipv6Addr>()
            .is_ok_and(|ip| ip == Ipv6Addr::LOCALHOST);
    }
    if let Ok(ip) = rendered.parse::<Ipv4Addr>() {
        return ip == Ipv4Addr::LOCALHOST;
    }
    rendered.eq_ignore_ascii_case("localhost")
}

pub(super) async fn get_providers(State(state): State<CoreState>) -> Json<ProviderCatalog> {
    let _settings_update = state.settings_update.lock();
    Json(provider_catalog_from_state(&state))
}

pub(super) async fn get_model_capabilities(
    State(state): State<CoreState>,
) -> Json<Vec<ProviderModelCapability>> {
    let _settings_update = state.settings_update.lock();
    Json(model_capabilities())
}

pub(super) async fn get_zen_free_settings(
    State(state): State<CoreState>,
) -> Result<Json<ZenFreeSettings>, V3ApiError> {
    let _settings_update = state.settings_update.lock();
    zen_free_settings_from_state(&state).map(Json)
}

pub(super) async fn patch_zen_free_settings(
    State(state): State<CoreState>,
    body: Bytes,
) -> Result<Json<ZenFreeSettings>, V3ApiError> {
    let input = parse_mutation_json::<ZenFreeSettingsUpdate>(&body)?;
    let _settings_update = state.settings_update.lock();
    check_expectation(&state, &input.expectation)?;
    {
        let db = state.db.lock();
        db.set_zen_free_enabled(input.enabled)
            .map_err(V3ApiError::internal)?;
    }
    let _revision = state.bump_settings_revision();
    zen_free_settings_from_state(&state).map(Json)
}

pub(super) async fn get_zen_free_models(State(state): State<CoreState>) -> Json<ZenFreeModels> {
    let _settings_update = state.settings_update.lock();
    Json(zen_free_models_from_state(&state))
}

pub(super) async fn refresh_zen_free_models(
    State(state): State<CoreState>,
    body: Bytes,
) -> Result<Json<ZenFreeModels>, V3ApiError> {
    let expectation = parse_mutation_json::<MutationExpectation>(&body)?;
    let _refresh = state.zen_free_models_refresh.try_lock().map_err(|_| {
        V3ApiError::conflict_at(&state, "Zen Free model refresh is already running")
    })?;
    let config = {
        let _settings_update = state.settings_update.lock();
        check_expectation(&state, &expectation)?;
        state.config()
    };
    let fetched = fetch_zen_free_catalog(&state, &config).await;
    let _settings_update = state.settings_update.lock();
    let catalog = fetched.map_err(|message| V3ApiError::outbound_failed(&state, message))?;
    check_expectation(&state, &expectation)?;
    if catalog.models.is_empty() {
        return Err(V3ApiError::outbound_failed(
            &state,
            "Zen model catalog contains no model IDs ending in `-free`",
        ));
    }
    state
        .activate_zen_free_model_catalog(catalog)
        .map_err(V3ApiError::internal)?;
    let _revision = state.bump_settings_revision();
    Ok(Json(zen_free_models_from_state(&state)))
}

pub(super) async fn get_provider_contracts(
    State(state): State<CoreState>,
) -> Result<Json<ProviderContracts>, V3ApiError> {
    let _settings_update = state.settings_update.lock();
    let contracts = state.provider_contracts();
    let (accounts, statuses) = load_accounts_with_verification(&state)?;
    Ok(Json(provider_contracts_from_state(
        &state, &contracts, &accounts, &statuses,
    )))
}

pub(super) async fn put_provider_protocol_switch(
    State(state): State<CoreState>,
    Path((scope_id, protocol)): Path<(String, String)>,
    body: Bytes,
) -> Result<Json<ProviderContracts>, V3ApiError> {
    let input = parse_mutation_json::<ProtocolSwitchUpdate>(&body)?;
    let _settings_update = state.settings_update.lock();
    check_expectation(&state, &input.expectation)?;
    let scope = ContractScope::parse(provider_contracts::SCOPE_KIND_PROVIDER, &scope_id)
        .map_err(|message| V3ApiError::invalid_request_at(&state, message))?;
    let protocol = provider_contracts::parse_upstream_protocol(&protocol)
        .map_err(|message| V3ApiError::invalid_request_at(&state, message))?;
    validate_provider_scope(&state, &scope)?;
    let now = Utc::now();
    {
        let db = state.db.lock();
        db.set_protocol_switch(&scope, protocol, input.enabled, now)
            .map_err(V3ApiError::internal)?;
        state
            .reload_provider_contracts_locked(&db)
            .map_err(V3ApiError::internal)?;
    }
    state.routing.reset();
    let _revision = state.bump_settings_revision();
    let contracts = state.provider_contracts();
    let (accounts, statuses) = load_accounts_with_verification(&state)?;
    Ok(Json(provider_contracts_from_state(
        &state, &contracts, &accounts, &statuses,
    )))
}

pub(super) async fn run_provider_protocol_probes(
    State(state): State<CoreState>,
    Path(provider_id): Path<String>,
    body: Bytes,
) -> Result<Json<ProtocolProbeResponse>, V3ApiError> {
    let input = parse_mutation_json::<ProtocolProbeRequest>(&body)?;
    let prepared = {
        let _settings_update = state.settings_update.lock();
        check_expectation(&state, &input.expectation)?;
        prepare_protocol_probe(&state, &provider_id, &input)?
    };
    let outcomes = protocol_probe::run_protocol_probes(
        &ProtocolProbeContext {
            state: &state,
            config: &prepared.config,
            account: &prepared.account,
            adapter: prepared.adapter,
            model_id: &prepared.model_id,
            custom_route: None,
            now: prepared.now,
        },
        &prepared.scope,
        &prepared.protocols,
        &[],
        |protocol| Ok(prepared.existing.get(&protocol).cloned().flatten()),
        |_| Ok(()),
    )
    .await
    .map_err(|error| match error {
        ProtocolProbeRunError::Apply(message) => V3ApiError::invalid_request_at(&state, message),
        ProtocolProbeRunError::Evidence(message) | ProtocolProbeRunError::Persist(message) => {
            V3ApiError::internal(message)
        }
    })?;
    let observations: Vec<_> = outcomes
        .iter()
        .filter_map(|outcome| outcome.observation.clone())
        .collect();
    let _settings_update = state.settings_update.lock();
    persist_probe_observations(&state, &observations)?;
    let revision = ControlRevision::from_state(&state);
    let contract = state
        .provider_contracts()
        .scope(&prepared.scope)
        .and_then(|scope| scope.model(&prepared.model_id).cloned())
        .map(|model| model_contract_from_domain(&model));
    Ok(Json(ProtocolProbeResponse {
        account_id: prepared.account.id.clone(),
        provider_id: prepared.provider_id,
        model_id: prepared.model_id.clone(),
        results: outcomes
            .into_iter()
            .map(|outcome| ProtocolProbeResult {
                protocol: AccountUpstreamProtocol::from(outcome.protocol),
                success: outcome.success,
                skipped: outcome.skipped,
                error: outcome.error,
            })
            .collect(),
        contract,
        revision: revision.revision,
        process_generation: revision.process_generation,
        pricing_revision: revision.pricing_revision,
    }))
}

fn persist_probe_observations(
    state: &CoreState,
    observations: &[PersistedModelProtocol],
) -> Result<(), V3ApiError> {
    if observations.is_empty() {
        return Ok(());
    }
    {
        let db = state.db.lock();
        db.upsert_model_protocols(observations)
            .map_err(V3ApiError::internal)?;
        // Advance CAS immediately after commit so a later reload/read
        // failure cannot hide the persisted mutation behind an unchanged token.
        let _revision = state.bump_settings_revision();
        state
            .reload_provider_contracts_locked(&db)
            .map_err(V3ApiError::internal)?;
    }
    state.routing.reset();
    Ok(())
}

struct PreparedProtocolProbe {
    provider_id: String,
    account: ModelAccount,
    adapter: ProviderAdapterKind,
    config: AppConfig,
    scope: ContractScope,
    model_id: String,
    protocols: Vec<crate::provider::UpstreamProtocolKind>,
    existing: HashMap<crate::provider::UpstreamProtocolKind, Option<PersistedModelProtocol>>,
    now: DateTime<Utc>,
}

fn prepare_protocol_probe(
    state: &CoreState,
    provider_id: &str,
    input: &ProtocolProbeRequest,
) -> Result<PreparedProtocolProbe, V3ApiError> {
    let adapter = match provider_contracts::adapter_kind_for_provider_scope(provider_id) {
        Some(ProviderAdapterKind::ConfigurableHttp) => {
            return Err(V3ApiError::invalid_request_at(
                state,
                "protocol probes for Custom API are account-owned",
            ));
        }
        None => return Err(V3ApiError::not_found_at(state, "provider not found")),
        Some(kind) if !kind.protocol_probe_supported() => {
            return Err(V3ApiError::not_implemented(
                state,
                "protocol probes are not available for this Plan in this slice",
            ));
        }
        Some(kind) => kind,
    };
    let model_id = input.model_id.trim();
    if model_id.is_empty() {
        return Err(V3ApiError::invalid_request_at(state, "modelId is required"));
    }
    if input.protocols.is_empty() {
        return Err(V3ApiError::invalid_request_at(
            state,
            "at least one explicit upstream protocol is required",
        ));
    }
    let protocols: Vec<_> = input
        .protocols
        .iter()
        .copied()
        .map(crate::provider::UpstreamProtocolKind::from)
        .collect();
    protocol_probe::require_unique_probe_protocols(&protocols)
        .map_err(|message| V3ApiError::invalid_request_at(state, message))?;
    let requested_account = input
        .account_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let account_id = match adapter {
        ProviderAdapterKind::OpenCodeGo => requested_account.ok_or_else(|| {
            V3ApiError::invalid_request_at(
                state,
                "accountId is required for OpenCode Go protocol probes",
            )
        })?,
        ProviderAdapterKind::ZenFree => match requested_account {
            None => ZEN_FREE_ACCOUNT_ID,
            Some(id) if id == ZEN_FREE_ACCOUNT_ID => ZEN_FREE_ACCOUNT_ID,
            Some(_) => {
                return Err(V3ApiError::invalid_request_at(
                    state,
                    "accountId must be the Zen Free singleton",
                ));
            }
        },
        ProviderAdapterKind::ConfigurableHttp
        | ProviderAdapterKind::CommandCodeGoat
        | ProviderAdapterKind::Scnet => {
            unreachable!("zero-call adapters return before account resolution")
        }
    };
    let account = state
        .db
        .lock()
        .get_account(account_id)
        .map_err(V3ApiError::internal)?
        .ok_or_else(|| V3ApiError::not_found_at(state, "account not found"))?;
    if account.provider_id != provider_id
        || (adapter == ProviderAdapterKind::OpenCodeGo && account.offering_id != GO_OFFERING_ID)
    {
        return Err(V3ApiError::invalid_request_at(
            state,
            "account does not belong to this provider",
        ));
    }
    let scope = ContractScope::from_account(&account).ok_or_else(|| {
        V3ApiError::invalid_request_at(state, "account does not own a provider contract scope")
    })?;
    let now = Utc::now();
    let mut existing = HashMap::new();
    {
        let db = state.db.lock();
        for protocol in &protocols {
            if provider_contracts::probe_may_add(adapter, model_id, *protocol, &[]) {
                existing.insert(
                    *protocol,
                    db.load_model_protocol(&scope, model_id, *protocol)
                        .map_err(V3ApiError::internal)?,
                );
            }
        }
    }
    Ok(PreparedProtocolProbe {
        provider_id: provider_id.to_string(),
        account,
        adapter,
        config: state.config(),
        scope,
        model_id: model_id.to_string(),
        protocols,
        existing,
        now,
    })
}

fn validate_provider_scope(state: &CoreState, scope: &ContractScope) -> Result<(), V3ApiError> {
    match scope {
        ContractScope::Provider(provider_id)
            if provider_contracts::builtin_provider_scope_ids().contains(&provider_id.as_str()) =>
        {
            Ok(())
        }
        ContractScope::Provider(_) => Err(V3ApiError::not_found_at(
            state,
            "provider contract scope not found",
        )),
        ContractScope::CustomEndpoint(_) => Err(V3ApiError::invalid_request_at(
            state,
            "protocol switches on this path are limited to provider scopes",
        )),
    }
}

fn provider_catalog_from_state(state: &CoreState) -> ProviderCatalog {
    let revision = ControlRevision::from_state(state);
    let zen_catalog = state.zen_free_model_catalog();
    ProviderCatalog {
        entries: BUILTIN_PLANS
            .iter()
            .map(|plan| catalog_entry(plan, &zen_catalog.models))
            .collect(),
        revision: revision.revision,
        process_generation: revision.process_generation,
        pricing_revision: revision.pricing_revision,
    }
}

fn catalog_entry(plan: &BuiltinPlan, zen_models: &[String]) -> ProviderCatalogEntry {
    ProviderCatalogEntry {
        provider_id: plan.offering.provider_id.to_string(),
        offering_id: plan.offering.offering_id.to_string(),
        display_name: plan.display_name.to_string(),
        display_family: plan.display_family.to_string(),
        credential_kind: plan.offering.credential_kind.into(),
        quota_scope: AccountQuotaScope::from(plan.offering.quota_scope),
        singleton: plan.offering.singleton_account_id.is_some(),
        creation_availability: plan.creation_availability.as_str().to_string(),
        creation_unavailable_reason: plan.creation_unavailable_reason.map(str::to_string),
        verification_policy: plan.verification_policy.as_str().to_string(),
        verification_runtime_availability: plan.verification_runtime_availability.to_string(),
        routable: plan.routable,
        managed_registration: plan.managed_registration,
        pricing_availability: plan.pricing_availability.to_string(),
        usage_availability: plan.usage_availability.to_string(),
        manual_usage_calibration: plan.manual_usage_calibration,
        quota_unit: plan.quota_unit.to_string(),
        model_source: plan.model_source.to_string(),
        key_prefix: plan.key_prefix.map(str::to_string),
        auth_schemes: plan
            .auth_schemes
            .iter()
            .copied()
            .map(AccountAuthScheme::from)
            .collect(),
        upstream_protocols: plan
            .upstream_protocols
            .iter()
            .copied()
            .map(AccountUpstreamProtocol::from)
            .collect(),
        form_fields: plan
            .form_fields
            .iter()
            .map(|field| ProviderCatalogFormField {
                id: field.id.to_string(),
                kind: field.kind.to_string(),
                required: field.required,
                immutable_after_create: field.immutable_after_create,
            })
            .collect(),
        risk_notice: plan.risk_notice.map(|notice| ProviderCatalogRiskNotice {
            acknowledgement_id: notice.acknowledgement_id.to_string(),
            version: notice.version.to_string(),
            source_url: notice.source_url.to_string(),
            body: notice.body.to_string(),
            content_hash: notice.content_hash(),
        }),
        model_aliases: alias::routeable_aliases_for_with_zen(
            plan.offering.provider_id,
            plan.offering.offering_id,
            zen_models,
        ),
    }
}

fn model_capabilities() -> Vec<ProviderModelCapability> {
    supported_model_protocol_profiles()
        .filter_map(|(model_id, preferred, supported)| {
            Some(ProviderModelCapability {
                model_id: model_id.to_string(),
                provider_id: OPENCODE_PROVIDER_ID.to_string(),
                offering_id: GO_OFFERING_ID.to_string(),
                preferred_protocol: upstream_protocol(preferred)?,
                supported_protocols: supported
                    .iter()
                    .copied()
                    .filter_map(upstream_protocol)
                    .collect(),
            })
        })
        .collect()
}

fn upstream_protocol(
    format: crate::kernel::protocol::ApiFormat,
) -> Option<AccountUpstreamProtocol> {
    provider_contracts::protocol_from_api(format).map(AccountUpstreamProtocol::from)
}

fn zen_free_settings_from_state(state: &CoreState) -> Result<ZenFreeSettings, V3ApiError> {
    let account = state
        .db
        .lock()
        .get_account(ZEN_FREE_ACCOUNT_ID)
        .map_err(V3ApiError::internal)?
        .ok_or_else(|| V3ApiError::internal("Zen Free singleton is missing"))?;
    let revision = ControlRevision::from_state(state);
    Ok(ZenFreeSettings {
        account_id: account.id,
        enabled: account.enabled,
        revision: revision.revision,
        process_generation: revision.process_generation,
        pricing_revision: revision.pricing_revision,
    })
}

fn zen_free_models_from_state(state: &CoreState) -> ZenFreeModels {
    let catalog = state.zen_free_model_catalog();
    let revision = ControlRevision::from_state(state);
    ZenFreeModels {
        account_id: ZEN_FREE_ACCOUNT_ID.to_string(),
        models: model_views(&catalog)
            .into_iter()
            .map(|model| ZenFreeModel {
                model_id: model.model_id,
                alias: model.alias,
            })
            .collect(),
        refreshed_at: catalog.refreshed_at.map(|value| value.to_rfc3339()),
        source_url: catalog.source_url.clone(),
        revision: revision.revision,
        process_generation: revision.process_generation,
        pricing_revision: revision.pricing_revision,
    }
}

async fn fetch_zen_free_catalog(
    state: &CoreState,
    config: &crate::models::AppConfig,
) -> Result<ZenFreeModelCatalog, String> {
    #[cfg(debug_assertions)]
    if let Some(url) = debug_zen_models_source_url(state.process_generation()) {
        let mut catalog = fetch_zen_catalog_at(config, &url).await?;
        catalog.source_url = ZEN_MODELS_SOURCE_URL.to_string();
        return Ok(catalog);
    }
    #[cfg(not(debug_assertions))]
    let _ = state;
    crate::zen_models::fetch_catalog(config).await
}

#[cfg(debug_assertions)]
async fn fetch_zen_catalog_at(
    config: &crate::models::AppConfig,
    source_url: &str,
) -> Result<ZenFreeModelCatalog, String> {
    let client = crate::http_client::build_no_redirect(config)
        .map_err(|error| format!("failed to build Zen model catalog client: {error}"))?;
    let timeout = Duration::from_secs(
        config
            .non_stream_timeout_secs
            .clamp(5, ZEN_REFRESH_TIMEOUT_SECS),
    );
    let response = tokio::time::timeout(
        Duration::from_secs(ZEN_REFRESH_TIMEOUT_SECS),
        client
            .get(source_url)
            .header(reqwest::header::ACCEPT, "application/json")
            .timeout(timeout)
            .send(),
    )
    .await
    .map_err(|_| "Zen model catalog refresh timed out".to_string())?
    .map_err(|error| format!("Zen model catalog request failed: {error}"))?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!(
            "Zen model catalog upstream returned HTTP {}",
            status.as_u16()
        ));
    }
    if response
        .content_length()
        .is_some_and(|length| length > MAX_ZEN_BODY_BYTES as u64)
    {
        return Err("Zen model catalog response is too large".to_string());
    }
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| format!("Zen model catalog body failed: {error}"))?;
        if body.len().saturating_add(chunk.len()) > MAX_ZEN_BODY_BYTES {
            return Err("Zen model catalog response is too large".to_string());
        }
        body.extend_from_slice(&chunk);
    }
    Ok(ZenFreeModelCatalog {
        models: parse_catalog(&body)?,
        refreshed_at: Some(Utc::now()),
        source_url: source_url.to_string(),
    })
}

fn load_accounts_with_verification(
    state: &CoreState,
) -> Result<
    (
        Vec<ModelAccount>,
        HashMap<String, ConnectionVerificationStatus>,
    ),
    V3ApiError,
> {
    let db = state.db.lock();
    let accounts = db.list_accounts().map_err(V3ApiError::internal)?;
    let mut statuses = HashMap::new();
    for account in &accounts {
        if let Some(verification) = db
            .account_verification_state(&account.id)
            .map_err(V3ApiError::internal)?
        {
            statuses.insert(account.id.clone(), verification.status);
        }
    }
    Ok((accounts, statuses))
}

fn provider_contracts_from_state(
    state: &CoreState,
    contracts: &EffectiveContractSet,
    accounts: &[ModelAccount],
    statuses: &HashMap<String, ConnectionVerificationStatus>,
) -> ProviderContracts {
    let revision = ControlRevision::from_state(state);
    let mut providers = Vec::new();
    for provider_id in provider_contracts::builtin_provider_scope_ids() {
        let Some(contract) = contracts.providers.get(provider_id) else {
            continue;
        };
        let descriptor = ProviderRegistry::iter()
            .find(|item| item.kind == contract.adapter_kind)
            .expect("adapter has a catalog offering");
        let offerings = BUILTIN_PLANS
            .iter()
            .filter(|plan| plan.offering.provider_id == provider_id)
            .map(|plan| ProviderOfferingChoice {
                offering_id: plan.offering.offering_id.to_string(),
                display_name: plan.display_name.to_string(),
                routable: plan.routable,
                accounts: accounts
                    .iter()
                    .filter(|account| {
                        account.provider_id == plan.offering.provider_id
                            && account.offering_id == plan.offering.offering_id
                    })
                    .map(|account| account_choice(account, statuses))
                    .collect(),
            })
            .collect();
        providers.push(ProviderContractGroup {
            scope_kind: ContractScopeKind::Provider,
            scope_id: provider_id.to_string(),
            provider_id: provider_id.to_string(),
            offerings,
            catalog: catalog_from_domain(&contract.catalog),
            models: contract
                .models
                .values()
                .map(model_contract_from_domain)
                .collect(),
            protocols: protocol_switches(contract.switches),
            pricing: CapabilitySummary {
                availability: descriptor.pricing.availability.to_string(),
            },
            usage: CapabilitySummary {
                availability: descriptor.usage.catalog_availability.to_string(),
            },
            card: card_summary(descriptor),
            catalog_routable: contract.catalog_routable,
            production_inference: contract.production_inference,
            disabled_reasons: contract.disabled_reasons.clone(),
            revision: contract.revision,
        });
    }
    let custom_endpoints = contracts
        .custom_endpoints
        .values()
        .map(|contract| {
            let descriptor =
                ProviderRegistry::get(CUSTOM_PROVIDER_ID, crate::provider::CUSTOM_API_OFFERING_ID)
                    .expect("custom offering is registered");
            let account = accounts
                .iter()
                .find(|account| account.id == contract.scope.id())
                .map(|account| account_choice(account, statuses))
                .unwrap_or(ProviderAccountChoice {
                    id: contract.scope.id().to_string(),
                    name: contract.scope.id().to_string(),
                    enabled: false,
                    verification_status: AccountVerificationStatus::Pending,
                });
            CustomEndpointContract {
                scope_kind: ContractScopeKind::CustomEndpoint,
                scope_id: contract.scope.id().to_string(),
                provider_id: CUSTOM_PROVIDER_ID.to_string(),
                account,
                catalog: catalog_from_domain(&contract.catalog),
                models: contract
                    .models
                    .values()
                    .map(model_contract_from_domain)
                    .collect(),
                protocols: protocol_switches(contract.switches),
                pricing: CapabilitySummary {
                    availability: descriptor.pricing.availability.to_string(),
                },
                usage: CapabilitySummary {
                    availability: descriptor.usage.catalog_availability.to_string(),
                },
                card: card_summary(descriptor),
                catalog_routable: contract.catalog_routable,
                production_inference: contract.production_inference,
                disabled_reasons: contract.disabled_reasons.clone(),
                revision: contract.revision,
            }
        })
        .collect();
    ProviderContracts {
        providers,
        custom_endpoints,
        revision: revision.revision,
        process_generation: revision.process_generation,
        pricing_revision: revision.pricing_revision,
    }
}

fn account_choice(
    account: &ModelAccount,
    statuses: &HashMap<String, ConnectionVerificationStatus>,
) -> ProviderAccountChoice {
    let verification_status = statuses
        .get(&account.id)
        .copied()
        .or_else(|| {
            crate::provider::builtin_plan(&account.provider_id, &account.offering_id)
                .map(default_verification_status)
        })
        .unwrap_or(ConnectionVerificationStatus::NotRequired);
    ProviderAccountChoice {
        id: account.id.clone(),
        name: account.name.clone(),
        enabled: account.enabled,
        verification_status: AccountVerificationStatus::from(verification_status),
    }
}

fn card_summary(descriptor: crate::provider::ProviderDescriptor) -> CardCapabilitySummary {
    CardCapabilitySummary {
        fetch_zen_models: descriptor.card_actions.fetch_zen_models,
        discover_models: descriptor.card_actions.discover_models,
        protocol_probe: descriptor.card_actions.protocol_probe,
        catalog_refresh: descriptor.card_actions.catalog_refresh,
    }
}

fn protocol_switches(value: DomainProtocolSwitches) -> ProtocolSwitches {
    ProtocolSwitches {
        chat_completions: value.chat_completions,
        responses: value.responses,
        messages: value.messages,
    }
}

fn catalog_from_domain(catalog: &provider_contracts::EffectiveCatalog) -> EffectiveCatalog {
    EffectiveCatalog {
        source: catalog.source.clone(),
        source_url: catalog.source_url.clone(),
        refreshed_at: catalog.refreshed_at.map(|value| value.to_rfc3339()),
        models: catalog.models.clone(),
        refresh_supported: catalog.refresh_supported,
    }
}

fn model_contract_from_domain(model: &DomainModelContract) -> EffectiveModelContract {
    EffectiveModelContract {
        model_id: model.model_id.clone(),
        preferred_protocol: AccountUpstreamProtocol::from(model.preferred_protocol),
        protocols: model_protocols_from_domain(&model.protocols),
        routable: model.routable,
        disabled_reasons: model.disabled_reasons.clone(),
    }
}

fn model_protocols_from_domain(
    map: &std::collections::BTreeMap<String, DomainProtocolEvidence>,
) -> EffectiveModelProtocols {
    let mut protocols = EffectiveModelProtocols {
        chat_completions: None,
        responses: None,
        messages: None,
    };
    for row in map.values() {
        let evidence = evidence_from_domain(row);
        match row.protocol {
            crate::provider::UpstreamProtocolKind::ChatCompletions => {
                protocols.chat_completions = Some(evidence);
            }
            crate::provider::UpstreamProtocolKind::Responses => {
                protocols.responses = Some(evidence);
            }
            crate::provider::UpstreamProtocolKind::Messages => {
                protocols.messages = Some(evidence);
            }
        }
    }
    protocols
}

fn evidence_from_domain(row: &DomainProtocolEvidence) -> EffectiveProtocolEvidence {
    EffectiveProtocolEvidence {
        protocol: AccountUpstreamProtocol::from(row.protocol),
        available: row.available,
        enabled: row.enabled,
        source: ContractEvidenceSource::from(row.source),
        verified_at: row.verified_at.map(|value| value.to_rfc3339()),
        observed_at: row.observed_at.map(|value| value.to_rfc3339()),
        last_probe_result: row.last_probe_result.map(ProbeResultKind::from),
        last_probe_at: row.last_probe_at.map(|value| value.to_rfc3339()),
        last_probe_error: row.last_probe_error.clone(),
    }
}

#[cfg(all(test, debug_assertions))]
mod zen_source_override_tests {
    use super::{
        debug_zen_models_source_url, parse_loopback_http_url,
        set_zen_models_source_url_override_for_tests,
    };

    fn unique_generation() -> u64 {
        uuid::Uuid::new_v4().as_u128() as u64
    }

    #[test]
    fn parse_loopback_http_url_requires_exact_host_without_userinfo_query_or_fragment() {
        assert_eq!(
            parse_loopback_http_url("http://127.0.0.1:9/zen/v1/models").as_deref(),
            Some("http://127.0.0.1:9/zen/v1/models")
        );
        assert_eq!(
            parse_loopback_http_url("http://localhost:9/zen/v1/models").as_deref(),
            Some("http://localhost:9/zen/v1/models")
        );
        assert_eq!(
            parse_loopback_http_url("http://[::1]:9/zen/v1/models").as_deref(),
            Some("http://[::1]:9/zen/v1/models")
        );
        assert_eq!(
            parse_loopback_http_url("HTTP://127.0.0.1:9/zen/v1/models").as_deref(),
            Some("http://127.0.0.1:9/zen/v1/models")
        );

        assert!(parse_loopback_http_url("http://127.0.0.1:9/zen/v1/models?x=1").is_none());
        assert!(parse_loopback_http_url("http://127.0.0.1:9/zen/v1/models#frag").is_none());
        assert!(parse_loopback_http_url("http://user@127.0.0.1:9/zen/v1/models").is_none());
        assert!(parse_loopback_http_url("http://:pass@127.0.0.1:9/zen/v1/models").is_none());
        assert!(parse_loopback_http_url("http://127.0.0.1:9@example.com/zen/v1/models").is_none());
        assert!(parse_loopback_http_url("http://127.0.0.1.example.com:9/zen/v1/models").is_none());
        assert!(parse_loopback_http_url("http://127.0.0.2:9/zen/v1/models").is_none());
        assert!(parse_loopback_http_url("http://[::ffff:127.0.0.1]:9/zen/v1/models").is_none());
        assert!(parse_loopback_http_url("https://opencode.ai/zen/v1/models").is_none());
        assert!(parse_loopback_http_url("http://example.com/zen/v1/models").is_none());
    }

    #[test]
    fn overrides_are_isolated_by_process_generation_and_reject_ambiguous_urls() {
        let first = unique_generation();
        let second = unique_generation();
        set_zen_models_source_url_override_for_tests(
            first,
            Some("http://127.0.0.1:11/a".to_string()),
        );
        set_zen_models_source_url_override_for_tests(
            second,
            Some("http://127.0.0.1:12/b".to_string()),
        );
        assert_eq!(
            debug_zen_models_source_url(first).as_deref(),
            Some("http://127.0.0.1:11/a")
        );
        assert_eq!(
            debug_zen_models_source_url(second).as_deref(),
            Some("http://127.0.0.1:12/b")
        );

        set_zen_models_source_url_override_for_tests(
            first,
            Some("http://127.0.0.1:11@example.com/a".to_string()),
        );
        assert!(debug_zen_models_source_url(first).is_none());
        assert_eq!(
            debug_zen_models_source_url(second).as_deref(),
            Some("http://127.0.0.1:12/b")
        );

        set_zen_models_source_url_override_for_tests(second, None);
        assert!(debug_zen_models_source_url(second).is_none());
    }
}

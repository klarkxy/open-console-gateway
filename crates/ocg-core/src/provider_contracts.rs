//! Effective provider/custom-endpoint contracts: merge, selection, and views.
//!
//! Persistence lives in [`crate::db`]. This module is the only merge/selection
//! seam: dashboard, materialize, and `/v1/models` read an immutable snapshot
//! captured at request entry. Request paths never discover or probe.

use crate::alias::ProviderMapping;
use crate::custom::CustomAccountRuntime;
use crate::kernel::ids::{
    COMMAND_CODE_PROVIDER_ID, CPA_PROVIDER_ID, CUSTOM_PROVIDER_ID, KIMI_PROVIDER_ID,
    MINIMAX_PROVIDER_ID, OLLAMA_PROVIDER_ID, OPENCODE_PROVIDER_ID, OPENCODE_ZEN_FREE_PROVIDER_ID,
    custom_model_id_matches, normalize_model_name,
};
use crate::kernel::protocol::ApiFormat;
use crate::kernel::zen::ZenFreeModelCatalog;
use crate::models::Account;
use crate::provider::{
    COMMAND_CODE_GOAT_BASE_URL, OPENCODE_CONSTRUCTABLE_PROTOCOLS, ProtocolProbeDescriptor,
    ProviderAdapterKind, ProviderRegistry, StructuralProbeCeiling, UpstreamProtocolKind,
    command_code_goat_includes_model,
};
use crate::redaction::sanitize_upstream_error_value_with_known_secret;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;

pub const SCOPE_KIND_PROVIDER: &str = "provider";
pub const SCOPE_KIND_CUSTOM_ENDPOINT: &str = "custom_endpoint";

pub const CATALOG_SOURCE_STATIC: &str = "static";
pub const CATALOG_SOURCE_OFFICIAL_ZEN: &str = "official_zen";
pub const CATALOG_SOURCE_CUSTOM_DISCOVERY: &str = "custom_discovery";
pub const CATALOG_SOURCE_DECLARED: &str = "account_declared";
pub const CATALOG_SOURCE_COMMAND_CODE_MODELS: &str = "command_code_get_models";
pub const CATALOG_SOURCE_OPENCODE_MODELS: &str = "opencode_get_models";
pub const CATALOG_SOURCE_MINIMAX_CN_MODELS: &str = "minimax_cn_get_models";
pub const CATALOG_SOURCE_KIMI_CN_MODELS: &str = "kimi_cn_get_models";
pub const CATALOG_SOURCE_OLLAMA_CLOUD_MODELS: &str = "ollama_cloud_get_models";

pub const NO_ENABLED_UPSTREAM_PROTOCOL: &str =
    "no enabled upstream protocol is available for this model";

const MAX_PROBE_ERROR_CHARS: usize = 500;

pub fn static_protocol_snapshot_date(scope_id: &str) -> Option<&'static str> {
    let provider_id = provider_scope_descriptor(scope_id)?.provider_id;
    match provider_id {
        OPENCODE_PROVIDER_ID
        | OPENCODE_ZEN_FREE_PROVIDER_ID
        | COMMAND_CODE_PROVIDER_ID
        | MINIMAX_PROVIDER_ID
        | KIMI_PROVIDER_ID => Some(crate::kernel::protocol::OFFICIAL_PROTOCOL_BASELINE_DATE),
        OLLAMA_PROVIDER_ID => {
            Some(crate::kernel::protocol::OLLAMA_CLOUD_STATIC_PROTOCOL_SNAPSHOT_DATE)
        }
        _ => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContractScopeKind {
    Provider,
    CustomEndpoint,
}

impl ContractScopeKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Provider => SCOPE_KIND_PROVIDER,
            Self::CustomEndpoint => SCOPE_KIND_CUSTOM_ENDPOINT,
        }
    }
}

impl TryFrom<&str> for ContractScopeKind {
    type Error = String;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            SCOPE_KIND_PROVIDER => Ok(Self::Provider),
            SCOPE_KIND_CUSTOM_ENDPOINT => Ok(Self::CustomEndpoint),
            other => Err(format!("unknown contract scope kind `{other}`")),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ContractScope {
    Provider(String),
    CustomEndpoint(String),
}

impl ContractScope {
    pub fn provider(provider_id: impl Into<String>) -> Self {
        Self::Provider(provider_id.into())
    }

    pub fn custom_endpoint(account_id: impl Into<String>) -> Self {
        Self::CustomEndpoint(account_id.into())
    }

    pub fn kind(&self) -> ContractScopeKind {
        match self {
            Self::Provider(_) => ContractScopeKind::Provider,
            Self::CustomEndpoint(_) => ContractScopeKind::CustomEndpoint,
        }
    }

    pub fn kind_str(&self) -> &'static str {
        self.kind().as_str()
    }

    pub fn id(&self) -> &str {
        match self {
            Self::Provider(id) | Self::CustomEndpoint(id) => id,
        }
    }

    pub fn parse(kind: &str, id: &str) -> Result<Self, String> {
        let id = id.trim();
        if id.is_empty() {
            return Err("contract scope id is required".to_string());
        }
        match ContractScopeKind::try_from(kind)? {
            ContractScopeKind::Provider => provider_scope_descriptor(id)
                .map(|_| Self::provider(id))
                .ok_or_else(|| format!("unknown provider contract scope `{id}`")),
            ContractScopeKind::CustomEndpoint => Ok(Self::custom_endpoint(id)),
        }
    }

    pub fn from_account(account: &crate::models::Account) -> Option<Self> {
        Self::from_provider_id(&account.provider_id, Some(&account.id))
    }

    pub fn from_mapping(mapping: &ProviderMapping) -> Option<Self> {
        Self::from_provider_id(&mapping.provider_id, None)
    }

    pub fn from_provider_id(provider_id: &str, account_id: Option<&str>) -> Option<Self> {
        let descriptor = ProviderRegistry::get(provider_id)?;
        match descriptor.kind {
            ProviderAdapterKind::ConfigurableHttp => account_id
                .map(str::trim)
                .filter(|id| !id.is_empty())
                .map(Self::custom_endpoint),
            _ => descriptor.contract_scope_id.map(Self::provider),
        }
    }
}

pub fn builtin_provider_scope_ids() -> Vec<&'static str> {
    ProviderRegistry::iter()
        .filter_map(|descriptor| descriptor.contract_scope_id)
        .collect()
}

/// Resolve one exact, statically declared Provider contract scope. The opaque
/// scope id is deliberately distinct from Provider identity so a future second
/// Offering can declare its own scope without changing persistence or V3 wire
/// shapes.
pub fn provider_scope_descriptor(scope_id: &str) -> Option<crate::provider::ProviderDescriptor> {
    ProviderRegistry::iter().find(|descriptor| descriptor.contract_scope_id == Some(scope_id))
}

pub fn parse_upstream_protocol(value: &str) -> Result<UpstreamProtocolKind, String> {
    UpstreamProtocolKind::try_from(value).map_err(|_| {
        format!(
            "unknown upstream protocol `{value}`; expected chat_completions, responses, or messages"
        )
    })
}

pub fn protocol_from_api(format: ApiFormat) -> Option<UpstreamProtocolKind> {
    match format {
        ApiFormat::ChatCompletions => Some(UpstreamProtocolKind::ChatCompletions),
        ApiFormat::Responses => Some(UpstreamProtocolKind::Responses),
        ApiFormat::Messages => Some(UpstreamProtocolKind::Messages),
        ApiFormat::Gemini => None,
    }
}

/// Administrator `force_on` Chat rows for the Ollama Cloud catalog. Used as
/// the pin set when multiple `:`-tagged snapshot ids share one alias stem.
pub fn ollama_cloud_pinned_model_ids(contracts: &EffectiveContractSet) -> Vec<String> {
    contracts
        .providers
        .get(OLLAMA_PROVIDER_ID)
        .map(|scope| {
            scope
                .models
                .iter()
                .filter(|(_, model)| {
                    model
                        .protocols
                        .get(UpstreamProtocolKind::ChatCompletions.as_str())
                        .is_some_and(|row| row.r#override == ProtocolOverrideState::ForceOn)
                })
                .map(|(id, _)| id.clone())
                .collect()
        })
        .unwrap_or_default()
}

pub fn protocol_to_api(protocol: UpstreamProtocolKind) -> ApiFormat {
    match protocol {
        UpstreamProtocolKind::ChatCompletions => ApiFormat::ChatCompletions,
        UpstreamProtocolKind::Responses => ApiFormat::Responses,
        UpstreamProtocolKind::Messages => ApiFormat::Messages,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContractEvidenceSource {
    Static,
    Preset,
    ProbeConfirmed,
    ProbeObserved,
}

impl ContractEvidenceSource {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Static => "static",
            Self::Preset => "preset",
            Self::ProbeConfirmed => "probe_confirmed",
            Self::ProbeObserved => "probe_observed",
        }
    }

    pub const fn confers_support(self) -> bool {
        matches!(self, Self::Static | Self::Preset | Self::ProbeConfirmed)
    }
}

impl TryFrom<&str> for ContractEvidenceSource {
    type Error = String;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "static" => Ok(Self::Static),
            "preset" => Ok(Self::Preset),
            "probe_confirmed" => Ok(Self::ProbeConfirmed),
            "probe_observed" => Ok(Self::ProbeObserved),
            other => Err(format!("unknown contract evidence source `{other}`")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProbeResultKind {
    Success,
    Failure,
}

impl ProbeResultKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Failure => "failure",
        }
    }
}

impl TryFrom<&str> for ProbeResultKind {
    type Error = String;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "success" => Ok(Self::Success),
            "failure" => Ok(Self::Failure),
            other => Err(format!("unknown probe result `{other}`")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProtocolOverrideState {
    /// Follow persisted evidence and adapter safety ceiling.
    #[default]
    Auto,
    /// Request protocol enablement. Adapter-specific safety ceilings may still
    /// reject or suppress protocols the adapter cannot legally route.
    ForceOn,
    /// Disable the protocol regardless of evidence.
    ForceOff,
}

impl ProtocolOverrideState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::ForceOn => "force_on",
            Self::ForceOff => "force_off",
        }
    }
}

impl TryFrom<&str> for ProtocolOverrideState {
    type Error = String;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "auto" => Ok(Self::Auto),
            "force_on" => Ok(Self::ForceOn),
            "force_off" => Ok(Self::ForceOff),
            other => Err(format!("unknown protocol override state `{other}`")),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersistedScopeRow {
    pub scope: ContractScope,
    pub catalog_models: Vec<String>,
    pub catalog_refreshed_at: Option<DateTime<Utc>>,
    pub catalog_source: String,
    pub catalog_source_url: String,
    pub revision: u64,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersistedModelProtocol {
    pub scope: ContractScope,
    pub model_id: String,
    pub protocol: UpstreamProtocolKind,
    pub source: ContractEvidenceSource,
    pub verified_at: Option<DateTime<Utc>>,
    pub observed_at: Option<DateTime<Utc>>,
    pub last_probe_result: Option<ProbeResultKind>,
    pub last_probe_at: Option<DateTime<Utc>>,
    pub last_probe_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersistedModelProtocolOverride {
    pub scope: ContractScope,
    pub model_id: String,
    pub protocol: UpstreamProtocolKind,
    pub state: ProtocolOverrideState,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PersistedContracts {
    pub scopes: HashMap<ContractScope, PersistedScopeRow>,
    pub evidence: HashMap<ContractScope, Vec<PersistedModelProtocol>>,
    pub overrides: HashMap<ContractScope, Vec<PersistedModelProtocolOverride>>,
    /// Selected conversion-default protocol, independent of enablement.
    pub preferences: HashMap<ContractScope, Vec<(String, UpstreamProtocolKind)>>,
}

/// True when a persisted preferred protocol may be stored for this provider.
/// CPA is excluded. Custom endpoint scopes are rejected by the writer
/// (`scope.kind == provider`), not here.
pub fn selectable_model_protocol(provider_id: &str, protocol: UpstreamProtocolKind) -> bool {
    let id = provider_id.trim();
    !id.is_empty()
        && id != CPA_PROVIDER_ID
        && matches!(
            protocol,
            UpstreamProtocolKind::ChatCompletions
                | UpstreamProtocolKind::Responses
                | UpstreamProtocolKind::Messages
        )
}

/// `force_off` rows that a mutually exclusive radio wrote on *available*
/// sibling protocols. Deleting them restores Auto so passthrough can use
/// every available protocol. Unavailable siblings stay `force_off`.
pub fn exclusive_available_force_off_repairs(
    set: &EffectiveContractSet,
    persisted: &PersistedContracts,
) -> Vec<(ContractScope, String, UpstreamProtocolKind)> {
    let mut repairs = Vec::new();
    for (scope, rows) in &persisted.overrides {
        let Some(contract) = set.scope(scope) else {
            continue;
        };
        if contract.adapter_kind == ProviderAdapterKind::Cpa {
            continue;
        }
        let mut by_model: HashMap<String, Vec<&PersistedModelProtocolOverride>> = HashMap::new();
        for row in rows {
            by_model
                .entry(row.model_id.trim().to_ascii_lowercase())
                .or_default()
                .push(row);
        }
        for (model_key, model_rows) in by_model {
            let Some(model) = contract
                .models
                .values()
                .find(|model| custom_or_case_match(&model.model_id, &model_key))
            else {
                continue;
            };
            let available: Vec<UpstreamProtocolKind> = model
                .protocols
                .values()
                .filter(|row| row.available)
                .map(|row| row.protocol)
                .collect();
            if available.len() < 2 {
                continue;
            }
            let mut force_on = Vec::new();
            let mut force_off = Vec::new();
            for protocol in &available {
                match model_rows
                    .iter()
                    .find(|row| row.protocol == *protocol)
                    .map(|row| row.state)
                {
                    Some(ProtocolOverrideState::ForceOn) => force_on.push(*protocol),
                    Some(ProtocolOverrideState::ForceOff) => force_off.push(*protocol),
                    Some(ProtocolOverrideState::Auto) | None => {}
                }
            }
            if force_on.len() == 1 && force_off.len() == available.len() - 1 {
                for protocol in force_off {
                    repairs.push((scope.clone(), model.model_id.clone(), protocol));
                }
            }
        }
    }
    repairs
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EffectiveCatalog {
    pub source: String,
    pub source_url: String,
    pub refreshed_at: Option<DateTime<Utc>>,
    pub models: Vec<String>,
    pub refresh_supported: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EffectiveProtocolEvidence {
    pub protocol: UpstreamProtocolKind,
    pub available: bool,
    pub enabled: bool,
    pub source: ContractEvidenceSource,
    pub verified_at: Option<DateTime<Utc>>,
    pub observed_at: Option<DateTime<Utc>>,
    pub last_probe_result: Option<ProbeResultKind>,
    pub last_probe_at: Option<DateTime<Utc>>,
    pub last_probe_error: Option<String>,
    #[serde(rename = "override")]
    pub r#override: ProtocolOverrideState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EffectiveModelContract {
    pub model_id: String,
    pub preferred_protocol: UpstreamProtocolKind,
    pub protocols: BTreeMap<String, EffectiveProtocolEvidence>,
    pub routable: bool,
    pub disabled_reasons: Vec<String>,
}

impl EffectiveModelContract {
    pub fn enabled_protocols(&self) -> Vec<UpstreamProtocolKind> {
        self.protocols
            .values()
            .filter(|row| row.enabled)
            .map(|row| row.protocol)
            .collect()
    }

    pub fn has_enabled_protocol(&self) -> bool {
        self.protocols.values().any(|row| row.enabled)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectiveScopeContract {
    pub scope: ContractScope,
    pub provider_id: String,

    pub adapter_kind: ProviderAdapterKind,
    pub catalog_routable: bool,
    pub production_inference: bool,
    pub catalog: EffectiveCatalog,
    pub models: BTreeMap<String, EffectiveModelContract>,
    pub revision: u64,
    pub fallback_priority: &'static [UpstreamProtocolKind],
    pub disabled_reasons: Vec<String>,
}

impl EffectiveScopeContract {
    pub fn model(&self, model_id: &str) -> Option<&EffectiveModelContract> {
        let normalized = normalize_model_name(model_id);
        self.models
            .get(model_id)
            .or_else(|| self.models.get(&normalized))
            .or_else(|| {
                self.models
                    .values()
                    .find(|model| custom_or_case_match(&model.model_id, model_id))
            })
    }

    pub fn model_has_enabled_protocol(&self, model_id: &str) -> bool {
        self.model(model_id)
            .is_some_and(EffectiveModelContract::has_enabled_protocol)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EffectiveContractSet {
    pub providers: BTreeMap<String, EffectiveScopeContract>,
    pub custom_endpoints: BTreeMap<String, EffectiveScopeContract>,
}

impl EffectiveContractSet {
    /// Compatibility/dashboard evidence view. Routing decisions belong to the
    /// saved destination catalog; evidence and override labels remain visible.
    pub(crate) fn apply_destination_configuration(
        &mut self,
        projection: &crate::destination_projection::DestinationProjection,
    ) {
        let destinations: HashMap<_, _> = projection
            .destinations
            .iter()
            .map(|destination| (destination.id.as_str(), destination))
            .collect();
        let by_account: HashMap<_, _> = projection
            .credentials
            .iter()
            .filter_map(|credential| {
                destinations
                    .get(credential.destination_id.as_str())
                    .map(|destination| (credential.legacy_account_id.as_str(), *destination))
            })
            .collect();
        for scope in self
            .providers
            .values_mut()
            .chain(self.custom_endpoints.values_mut())
        {
            let destination = match &scope.scope {
                ContractScope::Provider(id) => destinations
                    .get(ocg_domain::destination::destination_id_for_builtin(id).as_str())
                    .copied(),
                ContractScope::CustomEndpoint(id) => by_account.get(id.as_str()).copied(),
            };
            let Some(destination) = destination else {
                continue;
            };
            scope.catalog.models = destination
                .catalog
                .iter()
                .map(|model| model.public_model.clone())
                .collect();
            for model in scope.models.values_mut() {
                let saved = destination
                    .catalog
                    .iter()
                    .find(|saved| saved.public_model.eq_ignore_ascii_case(&model.model_id));
                for evidence in model.protocols.values_mut() {
                    evidence.enabled = destination.enabled
                        && saved.is_some_and(|saved| {
                            saved.enabled && saved.protocols.contains(&evidence.protocol)
                        });
                    if saved.is_some_and(|saved| saved.protocols.contains(&evidence.protocol)) {
                        evidence.available = true;
                    }
                }
                if let Some(preferred) = saved.and_then(|saved| saved.preferred) {
                    model.preferred_protocol = preferred;
                }
                model.routable = model.has_enabled_protocol() && scope.production_inference;
                if !model.routable && model.disabled_reasons.is_empty() {
                    model.disabled_reasons.push("model_disabled".to_string());
                } else if model.routable {
                    model.disabled_reasons.clear();
                }
            }
            scope.catalog_routable = scope.models.values().any(|model| model.routable);
        }
    }

    pub fn scope(&self, scope: &ContractScope) -> Option<&EffectiveScopeContract> {
        match scope {
            ContractScope::Provider(id) => self.providers.get(id),
            ContractScope::CustomEndpoint(id) => self.custom_endpoints.get(id),
        }
    }

    pub fn provider_offering(&self, provider_id: &str) -> Option<&EffectiveScopeContract> {
        let scope = ContractScope::from_provider_id(provider_id, None)?;
        self.scope(&scope)
    }

    pub fn mapping_has_enabled_protocol(&self, mapping: &ProviderMapping) -> bool {
        let Some(scope) = ContractScope::from_mapping(mapping) else {
            return false;
        };
        self.scope(&scope)
            .is_some_and(|contract| contract.model_has_enabled_protocol(&mapping.upstream_model))
    }

    pub fn production_protocol_allowed(
        &self,
        account: &Account,
        model_id: &str,
        protocol: UpstreamProtocolKind,
    ) -> bool {
        let Some(scope) = ContractScope::from_account(account) else {
            return false;
        };
        self.scope(&scope)
            .and_then(|contract| contract.model(model_id))
            .and_then(|model| model.protocols.get(protocol.as_str()))
            .is_some_and(|row| row.available && row.enabled)
    }

    pub fn select_for_mapping(
        &self,
        mapping: &ProviderMapping,
        client: ApiFormat,
        model_id: &str,
    ) -> Result<ApiFormat, ProtocolSelectError> {
        let scope = ContractScope::from_mapping(mapping).ok_or_else(|| {
            ProtocolSelectError::new(format!(
                "no contract scope for `{}/{}`",
                mapping.provider_id, mapping.provider_id
            ))
        })?;
        self.select_upstream(&scope, client, model_id)
    }

    pub fn select_upstream(
        &self,
        scope: &ContractScope,
        client: ApiFormat,
        model_id: &str,
    ) -> Result<ApiFormat, ProtocolSelectError> {
        let contract = self.scope(scope).ok_or_else(|| {
            ProtocolSelectError::new(format!(
                "no effective contract for {} `{}`",
                scope.kind_str(),
                scope.id()
            ))
        })?;
        select_upstream_protocol(contract, client, model_id)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtocolSelectError {
    pub message: String,
}

impl ProtocolSelectError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for ProtocolSelectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ProtocolSelectError {}

/// Pick the upstream protocol for one request.
///
/// 1. Client protocol is enabled → passthrough.
/// 2. Else preferred is enabled → convert to preferred.
/// 3. Else the first enabled protocol in `fallback_priority`.
///
/// Gemini is client-only and never matches step 1.
pub fn select_enabled_upstream(
    client: ApiFormat,
    preferred: UpstreamProtocolKind,
    enabled: &[UpstreamProtocolKind],
    fallback_priority: &[UpstreamProtocolKind],
) -> Result<ApiFormat, ProtocolSelectError> {
    if enabled.is_empty() {
        return Err(ProtocolSelectError::new(NO_ENABLED_UPSTREAM_PROTOCOL));
    }
    if let Some(client_protocol) = protocol_from_api(client)
        && enabled.contains(&client_protocol)
    {
        return Ok(protocol_to_api(client_protocol));
    }
    if enabled.contains(&preferred) {
        return Ok(protocol_to_api(preferred));
    }
    for protocol in fallback_priority {
        if enabled.contains(protocol) {
            return Ok(protocol_to_api(*protocol));
        }
    }
    Err(ProtocolSelectError::new(NO_ENABLED_UPSTREAM_PROTOCOL))
}

pub fn select_upstream_protocol(
    contract: &EffectiveScopeContract,
    client: ApiFormat,
    model_id: &str,
) -> Result<ApiFormat, ProtocolSelectError> {
    let model = contract.model(model_id).ok_or_else(|| {
        ProtocolSelectError::new(format!(
            "model `{model_id}` is not in the effective contract"
        ))
    })?;
    let available = model.enabled_protocols();
    if available.is_empty() {
        return Err(ProtocolSelectError::new(NO_ENABLED_UPSTREAM_PROTOCOL));
    }
    let preferred = model.preferred_protocol;
    // CPA keeps its existing branch: matching Chat/Responses/Messages pass
    // through; Gemini falls through to preferred / adapter fallback.
    if contract.adapter_kind == ProviderAdapterKind::Cpa
        && let Some(client_protocol) = protocol_from_api(client)
        && available.contains(&client_protocol)
    {
        return Ok(protocol_to_api(client_protocol));
    }
    select_enabled_upstream(client, preferred, &available, contract.fallback_priority)
}

pub fn safety_ceiling_protocols(
    probe: ProtocolProbeDescriptor,
    model_id: &str,
) -> Vec<UpstreamProtocolKind> {
    match probe.structural_ceiling {
        StructuralProbeCeiling::Unavailable => Vec::new(),
        StructuralProbeCeiling::CommandCodeConstructable => {
            ocg_domain::protocol::command_code_supported_formats(model_id)
                .iter()
                .copied()
                .filter_map(protocol_from_api)
                .collect()
        }
        StructuralProbeCeiling::Fixed(protocols) => protocols.to_vec(),
        StructuralProbeCeiling::OpenCodeConstructable => {
            if model_id.trim().is_empty() {
                Vec::new()
            } else {
                OPENCODE_CONSTRUCTABLE_PROTOCOLS.to_vec()
            }
        }
        StructuralProbeCeiling::ZenFreeConstructable => {
            if crate::kernel::ids::is_free_model(model_id) {
                vec![UpstreamProtocolKind::ChatCompletions]
            } else {
                Vec::new()
            }
        }
    }
}

/// Official-docs Static rows currently stored for Go / Zen / Command.
/// Probe observations are ignored: they must not expand sealed adapter authority.
pub fn official_static_protocols(
    adapter: ProviderAdapterKind,
    model_id: &str,
    evidence: &[PersistedModelProtocol],
) -> Vec<UpstreamProtocolKind> {
    if !matches!(
        adapter,
        ProviderAdapterKind::OpenCodeGo
            | ProviderAdapterKind::ZenFree
            | ProviderAdapterKind::CommandCodeGoat
    ) {
        return Vec::new();
    }
    evidence
        .iter()
        .filter(|row| {
            custom_or_case_match(&row.model_id, model_id)
                && row.source == ContractEvidenceSource::Static
        })
        .map(|row| row.protocol)
        .collect()
}

fn structural_admission_protocols(
    adapter: ProviderAdapterKind,
    probe: ProtocolProbeDescriptor,
    model_id: &str,
) -> Vec<UpstreamProtocolKind> {
    if adapter == ProviderAdapterKind::CommandCodeGoat
        && !model_id.eq_ignore_ascii_case("stealth/ox-alpha")
    {
        ocg_domain::protocol::command_code_supported_formats(model_id)
            .iter()
            .copied()
            .filter_map(protocol_from_api)
            .collect()
    } else {
        safety_ceiling_protocols(probe, model_id)
    }
}

/// Protocols the effective contract may admit: the adapter structural ceiling
/// plus current official-docs Static evidence. Probe-manufactured rows do not
/// widen this set on sealed adapters.
pub fn admitted_protocols(
    adapter: ProviderAdapterKind,
    probe: ProtocolProbeDescriptor,
    model_id: &str,
    evidence: &[PersistedModelProtocol],
) -> Vec<UpstreamProtocolKind> {
    let mut admitted = structural_admission_protocols(adapter, probe, model_id);
    for protocol in official_static_protocols(adapter, model_id, evidence) {
        if !admitted.contains(&protocol) {
            admitted.push(protocol);
        }
    }
    admitted
}

pub fn static_verified_protocols(
    adapter: ProviderAdapterKind,
    model_id: &str,
    declared: &[(String, UpstreamProtocolKind)],
) -> Vec<UpstreamProtocolKind> {
    if adapter == ProviderAdapterKind::ConfigurableHttp {
        return declared
            .iter()
            .filter(|(id, _)| custom_model_id_matches(id, model_id))
            .map(|(_, protocol)| *protocol)
            .collect();
    }
    match adapter {
        ProviderAdapterKind::OpenCodeGo | ProviderAdapterKind::ZenFree => Vec::new(),
        ProviderAdapterKind::CommandCodeGoat => {
            if model_id.eq_ignore_ascii_case("stealth/ox-alpha") {
                Vec::new()
            } else {
                ocg_domain::protocol::command_code_supported_formats(model_id).to_vec()
            }
        }
        ProviderAdapterKind::MiniMaxCn => {
            return vec![
                UpstreamProtocolKind::ChatCompletions,
                UpstreamProtocolKind::Messages,
                UpstreamProtocolKind::Responses,
            ];
        }
        ProviderAdapterKind::KimiCn => {
            return vec![
                UpstreamProtocolKind::ChatCompletions,
                UpstreamProtocolKind::Messages,
            ];
        }
        ProviderAdapterKind::OllamaCloud => {
            return if model_id.trim().is_empty() {
                Vec::new()
            } else {
                vec![UpstreamProtocolKind::ChatCompletions]
            };
        }
        ProviderAdapterKind::Cpa => {
            return vec![
                UpstreamProtocolKind::ChatCompletions,
                UpstreamProtocolKind::Responses,
                UpstreamProtocolKind::Messages,
            ];
        }
        ProviderAdapterKind::ConfigurableHttp => unreachable!("handled above"),
    }
    .into_iter()
    .filter_map(protocol_from_api)
    .collect()
}

pub fn probe_may_add(
    probe: ProtocolProbeDescriptor,
    model_id: &str,
    protocol: UpstreamProtocolKind,
) -> bool {
    probe.explicit_probe && safety_ceiling_protocols(probe, model_id).contains(&protocol)
}

#[allow(clippy::too_many_arguments)]
pub fn apply_probe_observation(
    existing: Option<&PersistedModelProtocol>,
    scope: ContractScope,
    model_id: &str,
    protocol: UpstreamProtocolKind,
    success: bool,
    error: Option<String>,
    now: DateTime<Utc>,
    inside_ceiling: bool,
) -> Result<PersistedModelProtocol, String> {
    if success && !inside_ceiling {
        return Err(
            "probe success cannot add a model/protocol combination outside the adapter safety ceiling"
                .to_string(),
        );
    }
    let sanitized = error.map(|value| sanitize_probe_error(&value, None));
    if let Some(row) = existing {
        let mut next = row.clone();
        next.observed_at = Some(now);
        next.last_probe_at = Some(now);
        next.last_probe_result = Some(if success {
            ProbeResultKind::Success
        } else {
            ProbeResultKind::Failure
        });
        next.last_probe_error = if success { None } else { sanitized };
        if success && next.verified_at.is_none() {
            next.verified_at = Some(now);
        }
        return Ok(next);
    }
    if success {
        return Ok(PersistedModelProtocol {
            scope,
            model_id: model_id.to_string(),
            protocol,
            source: ContractEvidenceSource::ProbeObserved,
            verified_at: Some(now),
            observed_at: Some(now),
            last_probe_result: Some(ProbeResultKind::Success),
            last_probe_at: Some(now),
            last_probe_error: None,
        });
    }
    Ok(PersistedModelProtocol {
        scope,
        model_id: model_id.to_string(),
        protocol,
        source: ContractEvidenceSource::ProbeObserved,
        verified_at: None,
        observed_at: Some(now),
        last_probe_result: Some(ProbeResultKind::Failure),
        last_probe_at: Some(now),
        last_probe_error: sanitized,
    })
}

pub fn sanitize_probe_error(raw: &str, secret: Option<&str>) -> String {
    let value = secret.map_or_else(
        || sanitize_upstream_error_value_with_known_secret(raw, "").to_string(),
        |secret| sanitize_upstream_error_value_with_known_secret(raw, secret).to_string(),
    );
    truncate_chars(&strip_credential_urls(&value), MAX_PROBE_ERROR_CHARS)
}

fn strip_credential_urls(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(scheme_at) = rest.find("://") {
        let prefix_start = rest[..scheme_at]
            .rfind(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '+' || ch == '.' || ch == '-'))
            .map(|index| index + 1)
            .unwrap_or(0);
        output.push_str(&rest[..prefix_start]);
        let after_scheme = &rest[scheme_at + 3..];
        if let Some(at) = after_scheme.find('@') {
            let host_end = after_scheme[at + 1..]
                .find(|ch: char| ch == '/' || ch == '?' || ch == '#' || ch.is_whitespace())
                .map(|index| at + 1 + index)
                .unwrap_or(after_scheme.len());
            output.push_str(&rest[prefix_start..scheme_at + 3]);
            output.push_str(&after_scheme[at + 1..host_end]);
            rest = &after_scheme[host_end..];
        } else {
            output.push_str(&rest[prefix_start..scheme_at + 3]);
            rest = after_scheme;
        }
    }
    output.push_str(rest);
    output
}

fn truncate_chars(input: &str, max_chars: usize) -> String {
    let count = input.chars().count();
    if count <= max_chars {
        return input.to_string();
    }
    let mut truncated: String = input.chars().take(max_chars).collect();
    truncated.push('…');
    truncated
}

fn refreshed_catalog(
    persisted: Option<&PersistedScopeRow>,
    fallback_source: &str,
    fallback_url: &str,
) -> (EffectiveCatalog, Vec<String>) {
    let fetched = persisted.is_some_and(|row| !row.catalog_models.is_empty());
    let models = if fetched {
        persisted
            .map(|row| row.catalog_models.clone())
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    (
        EffectiveCatalog {
            source: if fetched {
                persisted
                    .map(|row| row.catalog_source.clone())
                    .filter(|source| !source.is_empty())
                    .unwrap_or_else(|| fallback_source.to_string())
            } else {
                String::new()
            },
            source_url: if fetched {
                persisted
                    .map(|row| row.catalog_source_url.clone())
                    .filter(|url| !url.is_empty())
                    .unwrap_or_else(|| fallback_url.to_string())
            } else {
                String::new()
            },
            refreshed_at: if fetched {
                persisted.and_then(|row| row.catalog_refreshed_at)
            } else {
                None
            },
            models: models.clone(),
            refresh_supported: true,
        },
        models,
    )
}

fn persisted_catalog_is_explicit(row: &PersistedScopeRow) -> bool {
    !row.catalog_models.is_empty()
        || row.catalog_refreshed_at.is_some()
        || !row.catalog_source.is_empty()
}

fn zen_refreshable_catalog(
    persisted: Option<&PersistedScopeRow>,
    zen_catalog: &ZenFreeModelCatalog,
) -> (EffectiveCatalog, Vec<String>) {
    if persisted.is_some_and(persisted_catalog_is_explicit) {
        return refreshed_catalog(
            persisted,
            CATALOG_SOURCE_OFFICIAL_ZEN,
            &zen_catalog.source_url,
        );
    }
    if zen_catalog.models.is_empty() {
        return refreshed_catalog(None, CATALOG_SOURCE_OFFICIAL_ZEN, &zen_catalog.source_url);
    }
    let fetched = zen_catalog.refreshed_at.is_some();
    (
        EffectiveCatalog {
            source: if fetched {
                CATALOG_SOURCE_OFFICIAL_ZEN.to_string()
            } else {
                String::new()
            },
            source_url: if fetched {
                zen_catalog.source_url.clone()
            } else {
                String::new()
            },
            refreshed_at: zen_catalog.refreshed_at,
            models: zen_catalog.models.clone(),
            refresh_supported: true,
        },
        zen_catalog.models.clone(),
    )
}

pub fn build_effective_contracts(
    zen_catalog: &ZenFreeModelCatalog,
    custom_runtimes: &[CustomAccountRuntime],
    persisted: PersistedContracts,
) -> EffectiveContractSet {
    let mut set = EffectiveContractSet::default();
    for scope_id in builtin_provider_scope_ids() {
        let scope = ContractScope::provider(scope_id);
        let descriptor = provider_scope_descriptor(scope_id)
            .expect("builtin provider scopes map to exact descriptors");
        let persisted_scope = persisted.scopes.get(&scope);
        let evidence = persisted.evidence.get(&scope).cloned().unwrap_or_default();
        let overrides = persisted.overrides.get(&scope).cloned().unwrap_or_default();
        let mut contract = merge_provider_scope(
            descriptor,
            zen_catalog,
            persisted_scope,
            &evidence,
            &overrides,
        );
        for (model_id, protocol) in persisted.preferences.get(&scope).into_iter().flatten() {
            if selectable_model_protocol(scope_id, *protocol)
                && let Some(model) = contract
                    .models
                    .values_mut()
                    .find(|model| custom_or_case_match(&model.model_id, model_id))
                && model.protocols.contains_key(protocol.as_str())
            {
                model.preferred_protocol = *protocol;
            }
        }
        set.providers.insert(scope_id.to_string(), contract);
    }
    for runtime in custom_runtimes {
        let scope = ContractScope::custom_endpoint(&runtime.account_id);
        let persisted_scope = persisted.scopes.get(&scope);
        let evidence = persisted.evidence.get(&scope).cloned().unwrap_or_default();
        let overrides = persisted.overrides.get(&scope).cloned().unwrap_or_default();
        let contract = merge_custom_scope(runtime, persisted_scope, &evidence, &overrides);
        set.custom_endpoints
            .insert(runtime.account_id.clone(), contract);
    }
    set
}

fn merge_provider_scope(
    descriptor: crate::provider::ProviderDescriptor,
    zen_catalog: &ZenFreeModelCatalog,
    persisted: Option<&PersistedScopeRow>,
    evidence: &[PersistedModelProtocol],
    overrides: &[PersistedModelProtocolOverride],
) -> EffectiveScopeContract {
    let adapter = descriptor.kind;
    let scope_id = descriptor
        .contract_scope_id
        .expect("provider contract descriptor must declare a scope id");
    let revision = persisted.map(|row| row.revision).unwrap_or(1);
    let (catalog, static_models) = match adapter {
        ProviderAdapterKind::OpenCodeGo => refreshed_catalog(
            persisted,
            CATALOG_SOURCE_OPENCODE_MODELS,
            crate::provider::OPENCODE_GO_BASE_URL,
        ),
        ProviderAdapterKind::ZenFree => zen_refreshable_catalog(persisted, zen_catalog),
        ProviderAdapterKind::CommandCodeGoat => refreshed_catalog(
            persisted,
            CATALOG_SOURCE_COMMAND_CODE_MODELS,
            COMMAND_CODE_GOAT_BASE_URL,
        ),
        ProviderAdapterKind::MiniMaxCn => refreshed_catalog(
            persisted,
            CATALOG_SOURCE_MINIMAX_CN_MODELS,
            crate::provider::MINIMAX_CN_BASE_URL,
        ),
        ProviderAdapterKind::KimiCn => refreshed_catalog(
            persisted,
            CATALOG_SOURCE_KIMI_CN_MODELS,
            crate::provider::KIMI_CN_BASE_URL,
        ),
        ProviderAdapterKind::OllamaCloud => refreshed_catalog(
            persisted,
            CATALOG_SOURCE_OLLAMA_CLOUD_MODELS,
            crate::kernel::ids::OLLAMA_CLOUD_BASE_URL,
        ),
        ProviderAdapterKind::Cpa => {
            unreachable!("CPA is an external integration without a Provider contract scope")
        }
        ProviderAdapterKind::ConfigurableHttp => unreachable!("custom uses merge_custom_scope"),
    };

    let mut models = BTreeMap::new();
    for model_id in &static_models {
        let default_source = if (adapter == ProviderAdapterKind::CommandCodeGoat
            && command_code_goat_includes_model(model_id))
            || (adapter == ProviderAdapterKind::OllamaCloud
                && crate::kernel::protocol::ollama_cloud_includes_model(model_id))
        {
            ContractEvidenceSource::Preset
        } else {
            ContractEvidenceSource::Static
        };
        models.insert(
            model_id.clone(),
            merge_model_contract(
                adapter,
                descriptor.protocol_probe,
                model_id,
                &[],
                default_source,
                evidence,
                overrides,
                descriptor.inference.catalog_routable && descriptor.inference.production_inference,
            ),
        );
    }
    let mut disabled_reasons = Vec::new();
    if !descriptor.inference.catalog_routable {
        disabled_reasons.push("catalog offering is not routable".to_string());
    }
    if !descriptor.inference.production_inference {
        disabled_reasons.push("production inference is disabled".to_string());
    }

    EffectiveScopeContract {
        scope: ContractScope::provider(scope_id),
        provider_id: descriptor.provider_id.to_string(),

        adapter_kind: adapter,
        catalog_routable: descriptor.inference.catalog_routable,
        production_inference: descriptor.inference.production_inference,
        catalog,
        models,
        revision,
        fallback_priority: descriptor.protocol_probe.fallback_priority,
        disabled_reasons,
    }
}

fn merge_custom_scope(
    runtime: &CustomAccountRuntime,
    persisted: Option<&PersistedScopeRow>,
    evidence: &[PersistedModelProtocol],
    overrides: &[PersistedModelProtocolOverride],
) -> EffectiveScopeContract {
    let descriptor =
        ProviderRegistry::get(CUSTOM_PROVIDER_ID).expect("custom offering is registered");
    let revision = persisted.map(|row| row.revision).unwrap_or(1);
    let declared: Vec<(String, UpstreamProtocolKind)> = runtime.declared_protocols();
    let mut catalog_models = Vec::new();
    let mut catalog_seen = HashSet::new();
    for (model_id, _) in &declared {
        if catalog_seen.insert(model_id.to_ascii_lowercase()) {
            catalog_models.push(model_id.clone());
        }
    }
    let catalog = EffectiveCatalog {
        source: CATALOG_SOURCE_DECLARED.to_string(),
        source_url: String::new(),
        refreshed_at: None,
        models: catalog_models,
        refresh_supported: false,
    };

    let mut models = BTreeMap::new();
    let mut seen = HashSet::new();
    for (model_id, _) in &declared {
        if !seen.insert(model_id.to_ascii_lowercase()) {
            continue;
        }
        models.insert(
            model_id.clone(),
            merge_model_contract(
                ProviderAdapterKind::ConfigurableHttp,
                descriptor.protocol_probe,
                model_id,
                &declared,
                ContractEvidenceSource::Preset,
                evidence,
                overrides,
                descriptor.inference.catalog_routable && descriptor.inference.production_inference,
            ),
        );
    }
    overlay_probe_confirmed_models(
        &mut models,
        ProviderAdapterKind::ConfigurableHttp,
        descriptor.protocol_probe,
        &declared,
        evidence,
        overrides,
        descriptor.inference.catalog_routable && descriptor.inference.production_inference,
    );

    EffectiveScopeContract {
        scope: ContractScope::custom_endpoint(&runtime.account_id),
        provider_id: CUSTOM_PROVIDER_ID.to_string(),

        adapter_kind: ProviderAdapterKind::ConfigurableHttp,
        catalog_routable: descriptor.inference.catalog_routable,
        production_inference: descriptor.inference.production_inference,
        catalog,
        models,
        revision,
        fallback_priority: descriptor.protocol_probe.fallback_priority,
        disabled_reasons: Vec::new(),
    }
}

fn preferred_protocol(
    adapter: ProviderAdapterKind,
    model_id: &str,
    declared: &[(String, UpstreamProtocolKind)],
) -> UpstreamProtocolKind {
    match adapter {
        ProviderAdapterKind::OpenCodeGo | ProviderAdapterKind::ZenFree => {
            // The V3 preferred field is non-null; this placeholder does not
            // confer support when an unknown model has no admitted evidence.
            provider_default_protocol(adapter, model_id)
                .unwrap_or(UpstreamProtocolKind::ChatCompletions)
        }
        ProviderAdapterKind::CommandCodeGoat => {
            ocg_domain::protocol::command_code_preferred_format(model_id)
                .and_then(protocol_from_api)
                .unwrap_or(UpstreamProtocolKind::ChatCompletions)
        }
        // MiniMax recommends its Anthropic-compatible API. Enabled alternatives
        // are fallback choices only when this configured preference is disabled.
        ProviderAdapterKind::MiniMaxCn => UpstreamProtocolKind::Messages,
        ProviderAdapterKind::KimiCn => UpstreamProtocolKind::ChatCompletions,
        ProviderAdapterKind::OllamaCloud => UpstreamProtocolKind::ChatCompletions,
        ProviderAdapterKind::Cpa => UpstreamProtocolKind::ChatCompletions,
        ProviderAdapterKind::ConfigurableHttp => {
            // A Custom endpoint binds every declared model to exactly one
            // upstream protocol; that protocol is also the conversion target.
            declared
                .iter()
                .filter(|(id, _)| custom_model_id_matches(id, model_id))
                .map(|(_, protocol)| *protocol)
                .next()
                .unwrap_or(UpstreamProtocolKind::ChatCompletions)
        }
    }
}

/// Known per-model defaults only. Directory discovery cannot assert Chat support.
fn provider_default_protocol(
    adapter: ProviderAdapterKind,
    model_id: &str,
) -> Option<UpstreamProtocolKind> {
    match adapter {
        ProviderAdapterKind::OpenCodeGo => {
            crate::official_protocols::known_opencode_default(model_id, false)
        }
        ProviderAdapterKind::ZenFree => {
            crate::official_protocols::known_opencode_default(model_id, true)
        }
        _ => None,
    }
}

#[allow(clippy::too_many_arguments)]
fn merge_model_contract(
    adapter: ProviderAdapterKind,
    probe: ProtocolProbeDescriptor,
    model_id: &str,
    declared: &[(String, UpstreamProtocolKind)],
    default_source: ContractEvidenceSource,
    evidence: &[PersistedModelProtocol],
    overrides: &[PersistedModelProtocolOverride],
    adapter_routable: bool,
) -> EffectiveModelContract {
    let official_docs = official_static_protocols(adapter, model_id, evidence);
    // GOAT extras stay Auto-off until official-docs Static evidence exists.
    // Included preset rows, and every other sealed adapter, default on once a
    // trusted protocol baseline makes the protocol available. Directory
    // discovery still cannot assert Chat for a model with no protocol evidence.
    let default_enabled = adapter != ProviderAdapterKind::CommandCodeGoat
        || command_code_goat_includes_model(model_id)
        || !official_docs.is_empty();
    let preferred = official_docs
        .first()
        .copied()
        .unwrap_or_else(|| preferred_protocol(adapter, model_id, declared));
    let ceiling = admitted_protocols(adapter, probe, model_id, evidence);
    let mut static_verified = if official_docs.is_empty() {
        static_verified_protocols(adapter, model_id, declared)
    } else {
        official_docs
    };
    if static_verified.is_empty()
        && let Some(default) = provider_default_protocol(adapter, model_id)
    {
        static_verified.push(default);
    }
    let mut protocols = BTreeMap::new();
    for protocol in UpstreamProtocolKind::ALL {
        let persisted = evidence
            .iter()
            .find(|row| custom_or_case_match(&row.model_id, model_id) && row.protocol == protocol);
        let in_ceiling = ceiling.contains(&protocol);
        let statically_verified = static_verified.contains(&protocol);
        let override_state = overrides
            .iter()
            .find(|row| custom_or_case_match(&row.model_id, model_id) && row.protocol == protocol)
            .map(|row| row.state)
            .unwrap_or(ProtocolOverrideState::Auto);
        // Persisted rows from an older or broader baseline must not resurrect a
        // protocol outside the adapter's current sealed structural ceiling.
        if adapter != ProviderAdapterKind::ConfigurableHttp && !in_ceiling && !statically_verified {
            continue;
        }
        let source = persisted
            .map(|row| row.source)
            .unwrap_or(if statically_verified {
                default_source
            } else {
                ContractEvidenceSource::ProbeObserved
            });
        let supported = statically_verified || in_ceiling;
        // Static/preset support is declaration truth: a stale probe-failure
        // observation must not demote it. Probe outcomes move enablement only
        // through the explicit overrides the probe handler persists.
        let evidence_available = (statically_verified || source.confers_support()) && supported;
        let (available, enabled) = match override_state {
            // Sealed and configurable HTTP adapters may force on only a
            // protocol already admitted by their adapter safety ceiling.
            ProtocolOverrideState::ForceOn
                if matches!(
                    adapter,
                    ProviderAdapterKind::CommandCodeGoat | ProviderAdapterKind::ConfigurableHttp
                ) =>
            {
                (supported, supported)
            }
            ProtocolOverrideState::ForceOn => (true, true),
            ProtocolOverrideState::ForceOff => (evidence_available, false),
            ProtocolOverrideState::Auto => (
                evidence_available,
                evidence_available && (default_enabled || source == ContractEvidenceSource::Preset),
            ),
        };
        protocols.insert(
            protocol.as_str().to_string(),
            EffectiveProtocolEvidence {
                protocol,
                available,
                enabled,
                source: if statically_verified && persisted.is_none() {
                    default_source
                } else {
                    persisted.map(|row| row.source).unwrap_or(source)
                },
                verified_at: persisted.and_then(|row| row.verified_at),
                observed_at: persisted.and_then(|row| row.observed_at),
                last_probe_result: persisted.and_then(|row| row.last_probe_result),
                last_probe_at: persisted.and_then(|row| row.last_probe_at),
                last_probe_error: persisted.and_then(|row| row.last_probe_error.clone()),
                r#override: override_state,
            },
        );
    }
    if !protocols.contains_key(preferred.as_str()) && static_verified.contains(&preferred) {
        protocols.insert(
            preferred.as_str().to_string(),
            EffectiveProtocolEvidence {
                protocol: preferred,
                available: true,
                enabled: default_enabled,
                source: default_source,
                verified_at: None,
                observed_at: None,
                last_probe_result: None,
                last_probe_at: None,
                last_probe_error: None,
                r#override: ProtocolOverrideState::Auto,
            },
        );
    }
    let mut disabled_reasons = Vec::new();
    if !adapter_routable {
        disabled_reasons.push("adapter safety ceiling forbids production routing".to_string());
    }
    if !protocols.values().any(|row| row.enabled) {
        disabled_reasons.push(NO_ENABLED_UPSTREAM_PROTOCOL.to_string());
    }
    EffectiveModelContract {
        model_id: model_id.to_string(),
        preferred_protocol: preferred,
        routable: adapter_routable && protocols.values().any(|row| row.enabled),
        protocols,
        disabled_reasons,
    }
}

fn overlay_probe_confirmed_models(
    models: &mut BTreeMap<String, EffectiveModelContract>,
    adapter: ProviderAdapterKind,
    probe: ProtocolProbeDescriptor,
    declared: &[(String, UpstreamProtocolKind)],
    evidence: &[PersistedModelProtocol],
    overrides: &[PersistedModelProtocolOverride],
    adapter_routable: bool,
) {
    let mut extra: HashSet<String> = HashSet::new();
    for row in evidence {
        if !row.source.confers_support() {
            continue;
        }
        if models
            .keys()
            .any(|id| custom_or_case_match(id, &row.model_id))
        {
            continue;
        }
        if !probe_may_add(probe, &row.model_id, row.protocol) {
            continue;
        }
        extra.insert(row.model_id.clone());
    }
    for model_id in extra {
        models.insert(
            model_id.clone(),
            merge_model_contract(
                adapter,
                probe,
                &model_id,
                declared,
                ContractEvidenceSource::ProbeConfirmed,
                evidence,
                overrides,
                adapter_routable,
            ),
        );
    }
}

fn custom_or_case_match(left: &str, right: &str) -> bool {
    custom_model_id_matches(left, right) || left.eq_ignore_ascii_case(right)
}

#[cfg(test)]
mod tests;

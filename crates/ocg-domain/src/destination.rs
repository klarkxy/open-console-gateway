//! Target destination and credential model (account-model unification RFC §3).
//!
//! I/O-free types plus a total mapper from legacy row facts. This crate cannot
//! depend on `ocg-core`, so callers feed plain fact structs the same way
//! [`crate::credential::legacy_account_objects`] does.

use crate::account::{AccountSetupStep, AccountType};
use crate::catalog::UpstreamAuthScheme;
use crate::connection::CONNECTION_ID_NAMESPACE;
use crate::connection::EndpointOperation;
use crate::credential::{
    AuthState, ModelScope, OnboardingTaskKind, OnboardingTaskState, RouteSpec,
    credential_id_for_legacy_account, derive_auth_state,
    observer_credential_id_for_platform_account,
};
use crate::dynamic::{DynamicAuthKind, DynamicModelUpstreamOverride, DynamicProviderDefinition};
use crate::ids::{
    CPA_ACCOUNT_ID, CPA_PROVIDER_ID, CUSTOM_PROVIDER_ID, OLLAMA_CLOUD_BASE_URL,
    OPENCODE_ZEN_FREE_PROVIDER_ID, ZEN_FREE_ACCOUNT_ID,
};
use crate::provider::{
    COMMAND_CODE_GOAT_BASE_URL, KIMI_CN_BASE_URL, MINIMAX_CN_BASE_URL, OPENCODE_GO_BASE_URL,
    OPENCODE_ZEN_BASE_URL, ProviderAdapterKind, builtin_provider,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use uuid::Uuid;

/// Wire protocol for destination catalog rows. Reuses the sealed
/// `chat_completions | responses | messages` vocabulary.
pub type Protocol = crate::catalog::UpstreamProtocolKind;

/// Hosts that Configurable HTTP may probe for an official current-balance API.
pub const OFFICIAL_BALANCE_PROBE_HOSTS: &[&str] = &[
    "api.deepseek.com",
    "api.moonshot.cn",
    "api.moonshot.ai",
    "api.stepfun.com",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdapterKind {
    OpencodeGo,
    Zen,
    Goat,
    Minimax,
    Kimi,
    Ollama,
    Cpa,
    Http,
}

impl AdapterKind {
    pub const ALL: [Self; 8] = [
        Self::OpencodeGo,
        Self::Zen,
        Self::Goat,
        Self::Minimax,
        Self::Kimi,
        Self::Ollama,
        Self::Cpa,
        Self::Http,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OpencodeGo => "opencode_go",
            Self::Zen => "zen",
            Self::Goat => "goat",
            Self::Minimax => "minimax",
            Self::Kimi => "kimi",
            Self::Ollama => "ollama",
            Self::Cpa => "cpa",
            Self::Http => "http",
        }
    }
}

impl From<ProviderAdapterKind> for AdapterKind {
    fn from(kind: ProviderAdapterKind) -> Self {
        match kind {
            ProviderAdapterKind::OpenCodeGo => Self::OpencodeGo,
            ProviderAdapterKind::ZenFree => Self::Zen,
            ProviderAdapterKind::CommandCodeGoat => Self::Goat,
            ProviderAdapterKind::MiniMaxCn => Self::Minimax,
            ProviderAdapterKind::KimiCn => Self::Kimi,
            ProviderAdapterKind::OllamaCloud => Self::Ollama,
            ProviderAdapterKind::ConfigurableHttp => Self::Http,
            ProviderAdapterKind::Cpa => Self::Cpa,
        }
    }
}

impl From<AdapterKind> for ProviderAdapterKind {
    fn from(kind: AdapterKind) -> Self {
        match kind {
            AdapterKind::OpencodeGo => Self::OpenCodeGo,
            AdapterKind::Zen => Self::ZenFree,
            AdapterKind::Goat => Self::CommandCodeGoat,
            AdapterKind::Minimax => Self::MiniMaxCn,
            AdapterKind::Kimi => Self::KimiCn,
            AdapterKind::Ollama => Self::OllamaCloud,
            AdapterKind::Http => Self::ConfigurableHttp,
            AdapterKind::Cpa => Self::Cpa,
        }
    }
}

/// Resolve a sealed `provider_id` to [`AdapterKind`]. Unknown ids yield `None`.
pub fn adapter_kind_for_builtin(provider_id: &str) -> Option<AdapterKind> {
    ProviderAdapterKind::from_provider_id(provider_id).map(AdapterKind::from)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthScheme {
    None,
    Bearer,
    XApiKey,
}

impl AuthScheme {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Bearer => "bearer",
            Self::XApiKey => "x_api_key",
        }
    }
}

impl From<DynamicAuthKind> for AuthScheme {
    fn from(kind: DynamicAuthKind) -> Self {
        match kind {
            DynamicAuthKind::None => Self::None,
            DynamicAuthKind::Bearer => Self::Bearer,
            DynamicAuthKind::XApiKey => Self::XApiKey,
        }
    }
}

impl From<UpstreamAuthScheme> for AuthScheme {
    fn from(kind: UpstreamAuthScheme) -> Self {
        match kind {
            UpstreamAuthScheme::Bearer => Self::Bearer,
            UpstreamAuthScheme::XApiKey => Self::XApiKey,
        }
    }
}

/// One configured HTTP protocol endpoint. Empty [`Destination::protocol_routes`]
/// means legacy interpretation of `protocols` + `base_url` + `auth_scheme`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct HttpProtocolRoute {
    pub protocol: Protocol,
    pub endpoint_url: String,
    pub auth_scheme: AuthScheme,
}

/// Bound on a nonempty explicit protocol-route list.
pub const MAX_HTTP_PROTOCOL_ROUTES: usize = 3;

/// Why stored or imported protocol routes cannot be accepted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProtocolRouteError {
    TooMany { count: usize },
    DuplicateProtocol { protocol: Protocol },
    EmptyEndpoint { protocol: Protocol },
    FirstRouteMismatch,
    ProtocolsMismatch,
    SealedMustBeEmpty,
}

impl std::fmt::Display for ProtocolRouteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooMany { count } => {
                write!(
                    f,
                    "at most {MAX_HTTP_PROTOCOL_ROUTES} HTTP protocol routes are supported, found {count}"
                )
            }
            Self::DuplicateProtocol { protocol } => {
                write!(f, "duplicate HTTP protocol `{}`", protocol.as_str())
            }
            Self::EmptyEndpoint { protocol } => {
                write!(
                    f,
                    "HTTP protocol `{}` is missing its endpoint",
                    protocol.as_str()
                )
            }
            Self::FirstRouteMismatch => write!(
                f,
                "first protocol route must match destination base URL and authentication"
            ),
            Self::ProtocolsMismatch => {
                write!(
                    f,
                    "destination protocols must match configured protocol routes"
                )
            }
            Self::SealedMustBeEmpty => {
                write!(f, "sealed destinations cannot store protocol routes")
            }
        }
    }
}

impl std::error::Error for ProtocolRouteError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RedirectPolicy {
    NoFollow,
    FollowKeyless,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct Capabilities {
    /// Test connection is offered on every ready credential except external
    /// integrations, whose credentials live outside OCG.
    pub testable: bool,
    pub discoverable_models: bool,
    pub official_balance_probe: Vec<String>,
    pub observer: bool,
    pub managed_signup: bool,
    pub external_integration: bool,
    pub billing_tier_required: bool,
    pub redirect_policy: RedirectPolicy,
    pub identity_headers: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageSource {
    OfficialApi,
    LocalProjection,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanWindowKind {
    FiveHours,
    Week,
    Month,
    Free,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct PlanWindow {
    pub kind: PlanWindowKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExpiryCadence {
    Monthly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PricingSource {
    Official,
    VerifiedSnapshot,
    Unpriced,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct Plan {
    pub usage_source: UsageSource,
    pub windows: Vec<PlanWindow>,
    pub expiry_cadence: Option<ExpiryCadence>,
    pub pricing_source: PricingSource,
    pub manual_calibration: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct CatalogModel {
    pub public_model: String,
    pub upstream_model: String,
    pub protocols: Vec<Protocol>,
    pub preferred: Option<Protocol>,
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream_override: Option<DynamicModelUpstreamOverride>,
}

/// Which client model names may resolve to a destination mapping.
///
/// Legacy Custom API rows accepted only their declared public names. Normal
/// user-defined HTTP connections also accept a unique exact upstream model
/// id. Built-in adapters retain their sealed, adapter-defined alias rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelResolution {
    AdapterDefined,
    PublicOnly,
    PublicAndUpstream,
}

impl ModelResolution {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AdapterDefined => "adapter_defined",
            Self::PublicOnly => "public_only",
            Self::PublicAndUpstream => "public_and_upstream",
        }
    }
}

/// Which V3-era row a destination was projected from. Migration-era bridge
/// only: the dashboard uses it to reach the V3 mutation routes until stage 4d
/// makes destinations the primary model, after which this field is removed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "id")]
pub enum LegacyDestinationRef {
    /// Sealed builtin; `id` is the `provider_id`.
    Builtin(String),
    /// User-defined Provider; `id` is the dynamic `provider_id`.
    Dynamic(String),
    /// Account-owned Custom API endpoint; `id` is the account id.
    CustomAccount(String),
    /// New API / Sub2API site; `id` is the platform parent id.
    PlatformParent(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct Destination {
    pub id: String,
    pub legacy: LegacyDestinationRef,
    pub adapter: AdapterKind,
    pub name: String,
    pub brand_family: Option<String>,
    pub base_url: Option<String>,
    pub protocols: Vec<Protocol>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub protocol_routes: Vec<HttpProtocolRoute>,
    pub auth_scheme: AuthScheme,
    pub model_resolution: ModelResolution,
    pub catalog: Vec<CatalogModel>,
    pub capabilities: Capabilities,
    pub plan: Option<Plan>,
    pub max_credentials: Option<u32>,
    pub observer_credential_id: Option<String>,
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct Grants {
    pub allowed_endpoint_ids: Vec<String>,
    pub allowed_origins: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct Cooldowns {
    pub generic_until: Option<DateTime<Utc>>,
    pub five_hour_until: Option<DateTime<Utc>>,
    pub week_until: Option<DateTime<Utc>>,
    pub month_until: Option<DateTime<Utc>>,
    pub free_until: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct OnboardingTaskRef {
    pub kind: OnboardingTaskKind,
    pub state: OnboardingTaskState,
    pub step: String,
}

/// Routable credential shape. Never carries ciphertext or plaintext secrets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct Credential {
    pub id: String,
    /// The `accounts` row this credential was projected from (migration-era
    /// bridge to the V3 mutation routes; removed in stage 4d).
    pub legacy_account_id: String,
    pub destination_id: String,
    pub name: String,
    pub notes: Option<String>,
    pub has_secret: bool,
    pub enabled: bool,
    pub routing_rank: u32,
    pub scope: ModelScope,
    pub grants: Grants,
    pub auth_state: AuthState,
    pub last_error: Option<String>,
    pub cooldowns: Cooldowns,
    pub quota_pool_id: Option<String>,
    pub onboarding_task: Option<OnboardingTaskRef>,
    pub purchase_date: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlatformKind {
    NewApi,
    Sub2Api,
}

/// Legacy destination row facts. `ocg-core` can populate these without a
/// dependency inversion into this crate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LegacyDestinationFacts {
    Builtin {
        provider_id: String,
    },
    Dynamic {
        definition: DynamicProviderDefinition,
    },
    CustomAccount {
        account_id: String,
        name: String,
        endpoint_url: String,
        protocol: Protocol,
        model_capabilities: Vec<(String, String)>,
    },
    PlatformParent {
        id: String,
        kind: PlatformKind,
        name: String,
        base_url: String,
        has_user_credential: bool,
    },
    Cpa,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacyPlatformLink {
    pub parent_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacyIdentityFacts {
    pub quota_pool_id: Option<String>,
    pub model_scope: ModelScope,
    pub allowed_endpoint_ids: Vec<String>,
    pub allowed_origins: Vec<String>,
    pub binding_enabled: bool,
}

/// Subset of a legacy `accounts` row needed to project a target credential.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacyCredentialFacts {
    pub id: String,
    pub provider_id: String,
    pub name: String,
    pub notes: Option<String>,
    pub has_key: bool,
    pub enabled: bool,
    pub order_index: u32,
    pub setup_step: AccountSetupStep,
    pub account_type: AccountType,
    pub auth_error: Option<String>,
    pub last_error: Option<String>,
    pub purchase_date: Option<String>,
    pub cooldown_generic_until: Option<DateTime<Utc>>,
    pub cooldown_5h_until: Option<DateTime<Utc>>,
    pub cooldown_week_until: Option<DateTime<Utc>>,
    pub cooldown_month_until: Option<DateTime<Utc>>,
    pub cooldown_free_until: Option<DateTime<Utc>>,
    pub verified: bool,
    pub platform_link: Option<LegacyPlatformLink>,
    pub identity: Option<LegacyIdentityFacts>,
}

/// Why one legacy row cannot map. The migration is total or it refuses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MappingError {
    UnknownProvider { provider_id: String },
    MissingDestination { destination_id: String },
    CustomRequiresAccount,
    CustomAccountMissingEndpoint { account_id: String },
    DynamicMissingEndpoint { provider_id: String },
    PlatformMissingBaseUrl { id: String },
}

impl std::fmt::Display for MappingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownProvider { provider_id } => {
                write!(f, "unknown provider `{provider_id}`")
            }
            Self::MissingDestination { destination_id } => {
                write!(f, "missing destination `{destination_id}`")
            }
            Self::CustomRequiresAccount => {
                write!(f, "custom provider requires an account-owned destination")
            }
            Self::CustomAccountMissingEndpoint { account_id } => {
                write!(f, "custom account `{account_id}` is missing an endpoint")
            }
            Self::DynamicMissingEndpoint { provider_id } => {
                write!(f, "dynamic provider `{provider_id}` is missing an endpoint")
            }
            Self::PlatformMissingBaseUrl { id } => {
                write!(f, "platform parent `{id}` is missing a base url")
            }
        }
    }
}

impl std::error::Error for MappingError {}

fn namespaced_uuid(name: &str) -> String {
    Uuid::new_v5(&CONNECTION_ID_NAMESPACE, name.as_bytes()).to_string()
}

pub fn destination_id_for_builtin(provider_id: &str) -> String {
    namespaced_uuid(&format!("destination:builtin:{provider_id}"))
}

/// Deterministic destination id for an account-owned Custom API endpoint.
pub fn destination_id_for_custom_account(account_id: &str) -> String {
    namespaced_uuid(&format!("destination:custom_account:{account_id}"))
}

pub fn destination_id_for_dynamic(provider_id: &str) -> String {
    namespaced_uuid(&format!("destination:dynamic:{provider_id}"))
}

pub fn destination_id_for_platform_account(platform_account_id: &str) -> String {
    namespaced_uuid(&format!("destination:platform:{platform_account_id}"))
}

fn official_balance_probe_hosts() -> Vec<String> {
    OFFICIAL_BALANCE_PROBE_HOSTS
        .iter()
        .map(|host| (*host).to_string())
        .collect()
}

fn plan_windows(kinds: &[PlanWindowKind]) -> Vec<PlanWindow> {
    kinds
        .iter()
        .copied()
        .map(|kind| PlanWindow { kind })
        .collect()
}

/// Sealed adapter capabilities from today's builtin registry and runtime facts.
///
/// Secret-bearing adapters never follow redirects (the legacy Go descriptor's
/// `follow_redirects` flag is superseded by that invariant); only the keyless
/// Zen adapter may follow. Auth header kind is not a capability: it is derived
/// per destination from `auth_scheme` and, for `Http`, the wire protocol.
pub fn sealed_capabilities(adapter: AdapterKind) -> Capabilities {
    let base = Capabilities {
        testable: adapter != AdapterKind::Cpa,
        discoverable_models: adapter == AdapterKind::Http,
        official_balance_probe: Vec::new(),
        observer: false,
        managed_signup: adapter == AdapterKind::OpencodeGo,
        external_integration: adapter == AdapterKind::Cpa,
        billing_tier_required: adapter == AdapterKind::Ollama,
        redirect_policy: if adapter == AdapterKind::Zen {
            RedirectPolicy::FollowKeyless
        } else {
            RedirectPolicy::NoFollow
        },
        identity_headers: matches!(adapter, AdapterKind::OpencodeGo | AdapterKind::Zen),
    };
    match adapter {
        AdapterKind::Http => Capabilities {
            official_balance_probe: official_balance_probe_hosts(),
            ..base
        },
        _ => base,
    }
}

/// Sealed commercial offering for a builtin `provider_id`. `custom` and unknown
/// ids have no plan.
pub fn sealed_plan(provider_id: &str) -> Option<Plan> {
    let adapter = adapter_kind_for_builtin(provider_id)?;
    match adapter {
        AdapterKind::OpencodeGo => Some(Plan {
            usage_source: UsageSource::OfficialApi,
            windows: plan_windows(&[
                PlanWindowKind::FiveHours,
                PlanWindowKind::Week,
                PlanWindowKind::Month,
            ]),
            expiry_cadence: Some(ExpiryCadence::Monthly),
            pricing_source: PricingSource::Official,
            manual_calibration: false,
        }),
        AdapterKind::Zen => Some(Plan {
            usage_source: UsageSource::None,
            windows: plan_windows(&[PlanWindowKind::Free]),
            expiry_cadence: None,
            pricing_source: PricingSource::Unpriced,
            manual_calibration: false,
        }),
        AdapterKind::Goat => Some(Plan {
            usage_source: UsageSource::LocalProjection,
            windows: plan_windows(&[
                PlanWindowKind::FiveHours,
                PlanWindowKind::Week,
                PlanWindowKind::Month,
            ]),
            expiry_cadence: Some(ExpiryCadence::Monthly),
            pricing_source: PricingSource::VerifiedSnapshot,
            manual_calibration: true,
        }),
        AdapterKind::Minimax => Some(Plan {
            usage_source: UsageSource::OfficialApi,
            // The official remains endpoint reports a rolling interval and a
            // weekly window; the dashboard presents them as 5h and week.
            windows: plan_windows(&[PlanWindowKind::FiveHours, PlanWindowKind::Week]),
            expiry_cadence: Some(ExpiryCadence::Monthly),
            pricing_source: PricingSource::Unpriced,
            manual_calibration: false,
        }),
        AdapterKind::Kimi => Some(Plan {
            usage_source: UsageSource::OfficialApi,
            windows: plan_windows(&[PlanWindowKind::FiveHours, PlanWindowKind::Week]),
            expiry_cadence: Some(ExpiryCadence::Monthly),
            pricing_source: PricingSource::Unpriced,
            manual_calibration: false,
        }),
        AdapterKind::Ollama => Some(Plan {
            usage_source: UsageSource::LocalProjection,
            windows: plan_windows(&[PlanWindowKind::Month]),
            expiry_cadence: Some(ExpiryCadence::Monthly),
            pricing_source: PricingSource::Official,
            manual_calibration: true,
        }),
        AdapterKind::Cpa | AdapterKind::Http => None,
    }
}

fn sealed_base_url(adapter: AdapterKind) -> Option<String> {
    match adapter {
        AdapterKind::OpencodeGo => Some(OPENCODE_GO_BASE_URL.to_string()),
        AdapterKind::Zen => Some(OPENCODE_ZEN_BASE_URL.to_string()),
        AdapterKind::Goat => Some(COMMAND_CODE_GOAT_BASE_URL.to_string()),
        AdapterKind::Minimax => Some(MINIMAX_CN_BASE_URL.to_string()),
        AdapterKind::Kimi => Some(KIMI_CN_BASE_URL.to_string()),
        AdapterKind::Ollama => Some(OLLAMA_CLOUD_BASE_URL.to_string()),
        // CPA's origin is the runtime loopback of the managed child, not a
        // sealed URL; `Http` destinations carry their own base_url.
        AdapterKind::Cpa | AdapterKind::Http => None,
    }
}

fn sealed_max_credentials(adapter: AdapterKind) -> Option<u32> {
    match adapter {
        AdapterKind::Zen | AdapterKind::Cpa => Some(1),
        AdapterKind::OpencodeGo
        | AdapterKind::Goat
        | AdapterKind::Minimax
        | AdapterKind::Kimi
        | AdapterKind::Ollama
        | AdapterKind::Http => None,
    }
}

fn auth_scheme_from_builtin(auth_schemes: &[UpstreamAuthScheme]) -> AuthScheme {
    match auth_schemes.first().copied() {
        Some(scheme) => AuthScheme::from(scheme),
        None => AuthScheme::None,
    }
}

fn auth_scheme_for_http_protocol(protocol: Protocol) -> AuthScheme {
    match protocol {
        Protocol::Messages => AuthScheme::XApiKey,
        Protocol::ChatCompletions | Protocol::Responses => AuthScheme::Bearer,
    }
}

fn required_url(value: &str, missing: impl FnOnce() -> MappingError) -> Result<&str, MappingError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        Err(missing())
    } else {
        Ok(trimmed)
    }
}

fn catalog_from_pairs(pairs: &[(String, String)], protocol: Protocol) -> Vec<CatalogModel> {
    pairs
        .iter()
        .map(|(public_model, upstream_model)| CatalogModel {
            public_model: public_model.clone(),
            upstream_model: upstream_model.clone(),
            protocols: vec![protocol],
            preferred: Some(protocol),
            enabled: true,
            upstream_override: None,
        })
        .collect()
}

fn builtin_destination(provider_id: &str) -> Result<Destination, MappingError> {
    if provider_id == CUSTOM_PROVIDER_ID {
        return Err(MappingError::CustomRequiresAccount);
    }
    let provider = builtin_provider(provider_id).ok_or_else(|| MappingError::UnknownProvider {
        provider_id: provider_id.to_string(),
    })?;
    let adapter =
        adapter_kind_for_builtin(provider_id).ok_or_else(|| MappingError::UnknownProvider {
            provider_id: provider_id.to_string(),
        })?;
    Ok(Destination {
        id: destination_id_for_builtin(provider_id),
        legacy: LegacyDestinationRef::Builtin(provider_id.to_string()),
        adapter,
        name: provider.display_name.to_string(),
        brand_family: Some(provider.display_family.to_string()),
        base_url: sealed_base_url(adapter),
        protocols: provider.upstream_protocols.to_vec(),
        protocol_routes: Vec::new(),
        auth_scheme: auth_scheme_from_builtin(provider.auth_schemes),
        model_resolution: ModelResolution::AdapterDefined,
        // Builtin catalogs are persisted snapshots (`provider_model_catalogs`);
        // the projection layer joins them onto the destination after mapping.
        catalog: Vec::new(),
        capabilities: sealed_capabilities(adapter),
        plan: sealed_plan(provider_id),
        max_credentials: sealed_max_credentials(adapter),
        observer_credential_id: None,
        enabled: true,
    })
}

/// Map one legacy destination shape. Unknown or incomplete rows refuse.
pub fn destination_from_legacy(
    facts: &LegacyDestinationFacts,
) -> Result<Destination, MappingError> {
    match facts {
        LegacyDestinationFacts::Builtin { provider_id } => builtin_destination(provider_id),
        LegacyDestinationFacts::Cpa => builtin_destination(CPA_PROVIDER_ID),
        LegacyDestinationFacts::Dynamic { definition } => {
            let endpoint = required_url(&definition.endpoint_url, || {
                MappingError::DynamicMissingEndpoint {
                    provider_id: definition.id.clone(),
                }
            })?;
            let protocol = definition.upstream_protocol;
            let catalog = definition
                .mappings
                .iter()
                .map(|mapping| CatalogModel {
                    public_model: mapping.public_model.clone(),
                    upstream_model: mapping.upstream_model.clone(),
                    protocols: vec![
                        mapping
                            .upstream_override
                            .as_ref()
                            .map(|route| route.protocol)
                            .unwrap_or(protocol),
                    ],
                    preferred: Some(
                        mapping
                            .upstream_override
                            .as_ref()
                            .map(|route| route.protocol)
                            .unwrap_or(protocol),
                    ),
                    enabled: true,
                    upstream_override: mapping.upstream_override.clone(),
                })
                .collect();
            Ok(Destination {
                id: destination_id_for_dynamic(&definition.id),
                legacy: LegacyDestinationRef::Dynamic(definition.id.clone()),
                adapter: AdapterKind::Http,
                name: definition.name.clone(),
                brand_family: None,
                base_url: Some(endpoint.to_string()),
                protocols: vec![protocol],
                protocol_routes: Vec::new(),
                auth_scheme: AuthScheme::from(definition.auth_kind),
                model_resolution: ModelResolution::PublicAndUpstream,
                catalog,
                capabilities: sealed_capabilities(AdapterKind::Http),
                plan: None,
                max_credentials: None,
                observer_credential_id: None,
                enabled: true,
            })
        }
        LegacyDestinationFacts::CustomAccount {
            account_id,
            name,
            endpoint_url,
            protocol,
            model_capabilities,
        } => {
            let endpoint = required_url(endpoint_url, || {
                MappingError::CustomAccountMissingEndpoint {
                    account_id: account_id.clone(),
                }
            })?;
            Ok(Destination {
                id: destination_id_for_custom_account(account_id),
                legacy: LegacyDestinationRef::CustomAccount(account_id.clone()),
                adapter: AdapterKind::Http,
                name: name.clone(),
                brand_family: None,
                base_url: Some(endpoint.to_string()),
                protocols: vec![*protocol],
                protocol_routes: Vec::new(),
                auth_scheme: auth_scheme_for_http_protocol(*protocol),
                model_resolution: ModelResolution::PublicOnly,
                catalog: catalog_from_pairs(model_capabilities, *protocol),
                capabilities: sealed_capabilities(AdapterKind::Http),
                plan: None,
                max_credentials: None,
                observer_credential_id: None,
                enabled: true,
            })
        }
        LegacyDestinationFacts::PlatformParent {
            id,
            kind,
            name,
            base_url,
            has_user_credential: _,
        } => {
            let endpoint = required_url(base_url, || MappingError::PlatformMissingBaseUrl {
                id: id.clone(),
            })?;
            let mut capabilities = sealed_capabilities(AdapterKind::Http);
            capabilities.observer = true;
            Ok(Destination {
                id: destination_id_for_platform_account(id),
                legacy: LegacyDestinationRef::PlatformParent(id.clone()),
                adapter: AdapterKind::Http,
                name: name.clone(),
                brand_family: Some(match kind {
                    PlatformKind::NewApi => "New API".to_string(),
                    PlatformKind::Sub2Api => "Sub2API".to_string(),
                }),
                base_url: Some(endpoint.to_string()),
                protocols: Protocol::ALL.to_vec(),
                protocol_routes: Vec::new(),
                auth_scheme: AuthScheme::Bearer,
                model_resolution: ModelResolution::PublicOnly,
                // Platform catalogs live in refresh snapshots; the projection
                // layer joins them after mapping.
                catalog: Vec::new(),
                capabilities,
                plan: None,
                max_credentials: None,
                observer_credential_id: Some(
                    observer_credential_id_for_platform_account(id).to_string(),
                ),
                enabled: true,
            })
        }
    }
}

fn resolve_credential_destination_id(
    facts: &LegacyCredentialFacts,
    destinations: &[Destination],
) -> Result<String, MappingError> {
    if let Some(link) = &facts.platform_link {
        let destination_id = destination_id_for_platform_account(&link.parent_id);
        return require_destination(destinations, destination_id);
    }
    if facts.provider_id == CUSTOM_PROVIDER_ID {
        let destination_id = destination_id_for_custom_account(&facts.id);
        return require_destination(destinations, destination_id);
    }
    if adapter_kind_for_builtin(&facts.provider_id).is_some() {
        let destination_id = destination_id_for_builtin(&facts.provider_id);
        return require_destination(destinations, destination_id);
    }
    let destination_id = destination_id_for_dynamic(&facts.provider_id);
    if destinations
        .iter()
        .any(|destination| destination.id == destination_id)
    {
        return Ok(destination_id);
    }
    Err(MappingError::UnknownProvider {
        provider_id: facts.provider_id.clone(),
    })
}

fn require_destination(
    destinations: &[Destination],
    destination_id: String,
) -> Result<String, MappingError> {
    if destinations
        .iter()
        .any(|destination| destination.id == destination_id)
    {
        Ok(destination_id)
    } else {
        Err(MappingError::MissingDestination { destination_id })
    }
}

fn credential_has_secret(facts: &LegacyCredentialFacts) -> bool {
    if facts.id == ZEN_FREE_ACCOUNT_ID || facts.provider_id == OPENCODE_ZEN_FREE_PROVIDER_ID {
        return false;
    }
    if facts.id == CPA_ACCOUNT_ID || facts.provider_id == CPA_PROVIDER_ID {
        return false;
    }
    facts.has_key
}

fn onboarding_from_facts(facts: &LegacyCredentialFacts) -> Option<OnboardingTaskRef> {
    if facts.account_type != AccountType::Managed || facts.setup_step.is_ready() {
        return None;
    }
    Some(OnboardingTaskRef {
        kind: OnboardingTaskKind::ManagedRegistration,
        state: OnboardingTaskState::InProgress,
        step: facts.setup_step.as_str().to_string(),
    })
}

fn purchase_date_from_facts(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

/// Map one legacy account/credential row onto a target credential.
pub fn credential_from_legacy(
    facts: &LegacyCredentialFacts,
    destinations: &[Destination],
) -> Result<Credential, MappingError> {
    let destination_id = resolve_credential_destination_id(facts, destinations)?;
    let (scope, grants, quota_pool_id) = match &facts.identity {
        Some(identity) => (
            identity.model_scope.clone(),
            Grants {
                allowed_endpoint_ids: identity.allowed_endpoint_ids.clone(),
                allowed_origins: identity.allowed_origins.clone(),
            },
            identity.quota_pool_id.clone(),
        ),
        None => (
            ModelScope::All,
            Grants {
                allowed_endpoint_ids: Vec::new(),
                allowed_origins: Vec::new(),
            },
            None,
        ),
    };
    Ok(Credential {
        id: credential_id_for_legacy_account(&facts.id).to_string(),
        legacy_account_id: facts.id.clone(),
        destination_id,
        name: facts.name.clone(),
        notes: facts.notes.clone(),
        has_secret: credential_has_secret(facts),
        enabled: facts.enabled,
        routing_rank: facts.order_index,
        scope,
        grants,
        auth_state: derive_auth_state(facts.auth_error.is_some(), facts.verified),
        last_error: facts.last_error.clone(),
        cooldowns: Cooldowns {
            generic_until: facts.cooldown_generic_until,
            five_hour_until: facts.cooldown_5h_until,
            week_until: facts.cooldown_week_until,
            month_until: facts.cooldown_month_until,
            free_until: facts.cooldown_free_until,
        },
        quota_pool_id,
        onboarding_task: onboarding_from_facts(facts),
        purchase_date: purchase_date_from_facts(facts.purchase_date.as_deref()),
    })
}

/// Explicit stored routes, or one legacy route per `protocols` member.
pub fn http_protocol_routes(destination: &Destination) -> Vec<HttpProtocolRoute> {
    if !destination.protocol_routes.is_empty() {
        return destination.protocol_routes.clone();
    }
    let endpoint_url = destination.base_url.clone().unwrap_or_default();
    destination
        .protocols
        .iter()
        .copied()
        .map(|protocol| HttpProtocolRoute {
            protocol,
            endpoint_url: endpoint_url.clone(),
            auth_scheme: destination.auth_scheme,
        })
        .collect()
}

/// Protocols this model may use: dest configured routes, or the single override.
pub fn http_model_protocols(destination: &Destination, model: &CatalogModel) -> Vec<Protocol> {
    if let Some(route) = &model.upstream_override {
        return vec![route.protocol];
    }
    http_protocol_routes(destination)
        .into_iter()
        .map(|route| route.protocol)
        .collect()
}

/// Exact saved route for `protocol`. Override models stay single-protocol.
pub fn http_model_route(
    destination: &Destination,
    model: &CatalogModel,
    protocol: Protocol,
) -> Option<HttpProtocolRoute> {
    if let Some(route) = &model.upstream_override {
        if route.protocol != protocol {
            return None;
        }
        return Some(HttpProtocolRoute {
            protocol,
            endpoint_url: route.endpoint_url.clone(),
            auth_scheme: auth_for_protocol(destination, protocol),
        });
    }
    http_protocol_routes(destination)
        .into_iter()
        .find(|route| route.protocol == protocol)
}

/// Base protocol routes first, then unique catalog overrides. Assignment order
/// stays stable for [`crate::credential::assigned_endpoints_for_routes`].
pub fn http_configured_routes(destination: &Destination) -> Vec<RouteSpec> {
    let mut routes = Vec::new();
    let mut seen = HashSet::new();
    for route in http_protocol_routes(destination) {
        let url = nonempty_route_url(&route.endpoint_url);
        if seen.insert((route.protocol, url.clone().unwrap_or_default())) {
            routes.push(RouteSpec {
                operation: EndpointOperation::from(route.protocol),
                url,
            });
        }
    }
    for model in &destination.catalog {
        let Some(route) = &model.upstream_override else {
            continue;
        };
        if seen.insert((route.protocol, route.endpoint_url.clone())) {
            routes.push(RouteSpec {
                operation: EndpointOperation::from(route.protocol),
                url: Some(route.endpoint_url.clone()),
            });
        }
    }
    routes
}

/// Reject inconsistent or duplicate explicit routes. Empty is legacy.
pub fn validate_http_protocol_route_list(
    routes: &[HttpProtocolRoute],
) -> Result<(), ProtocolRouteError> {
    if routes.is_empty() {
        return Ok(());
    }
    if routes.len() > MAX_HTTP_PROTOCOL_ROUTES {
        return Err(ProtocolRouteError::TooMany {
            count: routes.len(),
        });
    }
    let mut seen = HashSet::new();
    for route in routes {
        if !seen.insert(route.protocol) {
            return Err(ProtocolRouteError::DuplicateProtocol {
                protocol: route.protocol,
            });
        }
        if route.endpoint_url.trim().is_empty() {
            return Err(ProtocolRouteError::EmptyEndpoint {
                protocol: route.protocol,
            });
        }
    }
    Ok(())
}

/// Full destination alignment for a loaded or imported explicit route list.
pub fn validate_destination_protocol_routes(
    destination: &Destination,
) -> Result<(), ProtocolRouteError> {
    if destination.protocol_routes.is_empty() {
        return Ok(());
    }
    if destination.adapter != AdapterKind::Http {
        return Err(ProtocolRouteError::SealedMustBeEmpty);
    }
    validate_http_protocol_route_list(&destination.protocol_routes)?;
    let first = &destination.protocol_routes[0];
    if destination.base_url.as_deref() != Some(first.endpoint_url.as_str())
        || destination.auth_scheme != first.auth_scheme
    {
        return Err(ProtocolRouteError::FirstRouteMismatch);
    }
    let derived: Vec<Protocol> = destination
        .protocol_routes
        .iter()
        .map(|route| route.protocol)
        .collect();
    if destination.protocols != derived {
        return Err(ProtocolRouteError::ProtocolsMismatch);
    }
    Ok(())
}

fn auth_for_protocol(destination: &Destination, protocol: Protocol) -> AuthScheme {
    destination
        .protocol_routes
        .iter()
        .find(|route| route.protocol == protocol)
        .map(|route| route.auth_scheme)
        .unwrap_or(destination.auth_scheme)
}

fn nonempty_route_url(endpoint_url: &str) -> Option<String> {
    if endpoint_url.is_empty() {
        None
    } else {
        Some(endpoint_url.to_string())
    }
}

#[cfg(test)]
mod tests;

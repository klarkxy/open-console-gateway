//! Shared Dashboard V3 wire types and the JSON Schema catalog.
//!
//! Response objects always serialize nullable fields as `T | null` (never omitted).
//! Request optional fields may be omitted; `expectedRevision` is required on every
//! control-plane mutation, including `/auth/register`, `/auth/login`, and
//! `/auth/logout`, and `POST /accounts/{id}/usage/refresh`. Pricing mutations
//! also require `expectedPricingRevision`.
//! Operational diagnostics such as `POST /settings/test-proxy` are not mutations
//! and neither require nor accept CAS tokens. Custom model discovery is also an
//! operational probe without `expectedRevision`. `GET /settings/check-update` and
//! `GET /settings/update-status` are operational reads without CAS and do not bump
//! revision. `POST /settings/install-update` is an in-memory control-plane mutation
//! that requires `expectedRevision` and `processGeneration` but does not bump them.
//! Plaintext Open Console Gateway Keys must not appear on `Settings` or
//! provider/Zen/contract DTOs —`ConnectionInfo` is the only secret-bearing
//! V3 response DTO for those Keys. `CpaRuntimeKeyCreated.secret` returns a
//! newly generated CPA client inference key once. `CustomModelDiscoveryRequest.apiKey` is write-only. Protocol path/switch tokens
//! stay `chat_completions`, `responses`, and `messages`. Pricing wire DTOs are
//! distinct from `kernel::pricing` and from stored provider pricing blobs. Usage
//! wire DTOs are distinct from `models::UsageWindow` and from stored quota/sync
//! rows. Browser wire DTOs are distinct from `browser` runtime structs and never
//! carry worker URLs or control tokens.

use schemars::JsonSchema;
use schemars::generate::{SchemaGenerator, SchemaSettings};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::models::{AccountSetupStep as ModelAccountSetupStep, AccountType as ModelAccountType};
use crate::provider::{
    ConnectionVerificationStatus as ProviderVerificationStatus, CredentialKind, ProviderOrigin,
    QuotaScope, UpstreamAuthScheme, UpstreamProtocolKind,
};
use crate::provider_contracts::{
    ContractEvidenceSource as DomainContractEvidenceSource,
    ContractScopeKind as DomainContractScopeKind, ProbeResultKind as DomainProbeResultKind,
};
use crate::state::CoreState;

/// JSON Schema `$defs` names for the kernel catalog.
///
/// Later leases append new names here and register the matching DTO. Existing
/// definition objects must stay byte-identical.
pub const CATALOG_TYPE_NAMES: &[&str] = &[
    "ControlRevision",
    "MutationAck",
    "MutationExpectation",
    "PricingRevision",
    "V3Error",
    "ConnectionInfo",
    "ConnectionSubKey",
    "Settings",
    "SettingsUpdate",
    "ProxySupportedModel",
    "KeyCreate",
    "KeyUpdate",
    "Account",
    "AccountList",
    "AccountMutation",
    "AccountCustomConfig",
    "AccountModelCapability",
    "AccountCreate",
    "AccountManagedCreate",
    "AccountModelTestRequest",
    "AccountModelTestResponse",
    "AccountUpdate",
    "AccountOrder",
    "AccountSetupUpdate",
    "AccountCustomConfigUpdate",
    "AccountCustomConfigWrite",
    "AccountModelCapabilitiesUpdate",
    "AccountModelCapabilityWrite",
    "ProviderCatalog",
    "ProviderCatalogEntry",
    "ProviderCatalogFormField",
    "ProviderModelCapability",
    "ZenFreeSettings",
    "ZenFreeSettingsUpdate",
    "ZenFreeModels",
    "ZenFreeModel",
    "ProviderContracts",
    "ProviderContractGroup",
    "CustomEndpointContract",
    "ProviderAccountChoice",
    "EffectiveCatalog",
    "EffectiveModelContract",
    "EffectiveModelProtocols",
    "EffectiveProtocolEvidence",
    "CapabilitySummary",
    "CardCapabilitySummary",
    "ModelProtocolOverridesUpdate",
    "ModelProtocolOverride",
    "ProtocolOverrideState",
    "ProtocolProbeRequest",
    "ProtocolProbeResult",
    "ProtocolProbeResponse",
    "PricingSnapshot",
    "PricingLimits",
    "PricingModel",
    "PricingAdjustment",
    "PricingTimeWindow",
    "PricingRefresh",
    "PricingRefreshStatus",
    "PricingMultiplierChange",
    "PricingRefreshUpdate",
    "PricingRefreshPolicy",
    "PricingMultipliersUpdate",
    "PricingMultiplierWrite",
    "ProviderPricing",
    "PricingAvailability",
    "GatewayStatus",
    "ApplicationModels",
    "DashboardSummary",
    "DailyModelTokens",
    "DailyTokensByModel",
    "GatewayLog",
    "GatewayLogs",
    "ForwardLog",
    "ForwardLogSummary",
    "ForwardLogs",
    "ForwardLogClientKey",
    "ForwardLogKeys",
    "ForwardLogModels",
    "GatewayLogQuery",
    "ForwardLogQuery",
    "DailyTokensQuery",
    "UsageWindow",
    "UsageMutation",
    "AccountUsageUpdate",
    "ProviderUsage",
    "QuotaWindow",
    "CreditBalance",
    "UsageSyncState",
    "UsageAvailability",
    "AuthStatus",
    "AuthRegister",
    "AuthLogin",
    "AuthLogout",
    "ProxyTestRequest",
    "ProxyTestResponse",
    "CustomModelDiscoveryRequest",
    "CustomModelDiscoveryResponse",
    "AccountVerify",
    "BrowserMode",
    "BrowserTarget",
    "BrowserCapabilities",
    "BrowserOpenRequest",
    "BrowserOpen",
    "UpdateCheck",
    "DesktopUpdate",
    "InstallUpdate",
    "AccountManagedKeyVerify",
    "UsageRefresh",
    "UsageRefreshUpdate",
    "UsageRefreshThrottleError",
    "ProviderModelsRefreshUpdate",
    "ProviderModels",
    "ProviderPricingSnapshot",
    "ProviderPricingValue",
    "ProviderPricingRefresh",
    "ProviderPricingRefreshUpdate",
    "AccountExportRequest",
    "AccountExport",
    "AccountImportPreviewRequest",
    "AccountImportPreview",
    "AccountImportPreviewItem",
    "AccountImportDisposition",
    "AccountImportRequest",
    "AccountImportResult",
    "CpaIntegration",
    "CpaIntegrationUpdate",
    "CpaTestRequest",
    "CpaConnectionReport",
    "CpaModel",
    "CpaModels",
    "CpaAccounts",
    "CpaAccount",
    "CpaAccountStatusUpdate",
    "CpaAccountDelete",
    "CpaQuotaReset",
    "CpaOAuthProvider",
    "CpaOAuthStartRequest",
    "CpaOAuthStart",
    "CpaOAuthStatus",
    "CpaOAuthSessionDelete",
    "CpaCliImportSource",
    "CpaCliImports",
    "CpaCliImportRequest",
    "CpaCliImportResult",
    "CpaRuntime",
    "CpaRuntimePhase",
    "CpaRuntimeCheck",
    "CpaRuntimeInstall",
    "CpaRuntimeLogs",
    "CpaRuntimeKey",
    "CpaRuntimeKeys",
    "CpaRuntimeKeyCreated",
    "ProviderDefinitionAuthKind",
    "ProviderDefinitionModel",
    "ProviderDefinition",
    "ProviderDefinitionCreate",
    "ProviderDefinitionUpdate",
    "ProviderDefinitionMutation",
    "ProviderDefinitionDiscoverRequest",
    "ProviderDefinitionDiscoverResponse",
    "ProviderDefinitionTestRequest",
    "ProviderDefinitionTestResponse",
    "OllamaBillingTier",
    "PlatformAccounts",
    "PlatformAccount",
    "PlatformLink",
    "PlatformSnapshot",
    "PlatformCreate",
    "PlatformUpdate",
    "PlatformLinkWrite",
    "PlatformRefresh",
];

pub const ERROR_UNAUTHORIZED: &str = "unauthorized";
pub const ERROR_INVALID_JSON: &str = "invalidJson";
pub const ERROR_MISSING_EXPECTED_REVISION: &str = "missingExpectedRevision";
pub const ERROR_REVISION_CONFLICT: &str = "revisionConflict";
pub const ERROR_INVALID_REQUEST: &str = "invalidRequest";
pub const ERROR_INTERNAL: &str = "internal";
pub const ERROR_NOT_FOUND: &str = "notFound";
pub const ERROR_CONFLICT: &str = "conflict";
pub const ERROR_PRECONDITION_FAILED: &str = "preconditionFailed";
pub const ERROR_SERVICE_UNAVAILABLE: &str = "serviceUnavailable";
pub const ERROR_NOT_IMPLEMENTED: &str = "notImplemented";
pub const ERROR_OUTBOUND_FAILED: &str = "outboundFailed";
pub const ERROR_FORBIDDEN: &str = "forbidden";
pub const ERROR_GONE: &str = "gone";
pub const ERROR_GATEWAY_TIMEOUT: &str = "gatewayTimeout";
pub const ERROR_THROTTLED: &str = "throttled";
pub const ERROR_BUILTIN_PROVIDER_IMMUTABLE: &str = "builtinProviderImmutable";
pub const ERROR_OPERATION_PAYLOAD_MISMATCH: &str = "operationPayloadMismatch";

/// Live CAS token, process generation, and pricing snapshot id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct ControlRevision {
    pub revision: u64,
    pub process_generation: u64,
    pub pricing_revision: String,
}

impl ControlRevision {
    pub fn from_state(state: &CoreState) -> Self {
        Self {
            revision: state.settings_revision(),
            process_generation: state.process_generation(),
            pricing_revision: state.pricing_snapshot().revision.clone(),
        }
    }
}

/// Required process-scoped mutation precondition.
///
/// Both fields travel at the top level of every mutation request. The random
/// process generation prevents a revision captured before restart from being
/// accepted by a fresh process whose in-memory counter reused the same value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct MutationExpectation {
    pub expected_revision: u64,
    pub process_generation: u64,
}

/// Successful control-plane mutation acknowledgement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct MutationAck {
    pub revision: u64,
    pub process_generation: u64,
}

/// Public dashboard authentication snapshot. Never carries a password,
/// session token, Key, cipher, or secret.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct AuthStatus {
    pub local: bool,
    pub initialized: bool,
    pub authenticated: bool,
    pub revision: u64,
    pub process_generation: u64,
}

/// POST `/auth/register` body. CAS tokens, `username`, and `password` are
/// required. `password` is write-only and never echoed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct AuthRegister {
    #[serde(flatten)]
    pub expectation: MutationExpectation,
    pub username: String,
    pub password: String,
}

/// POST `/auth/login` body. Same required fields as [`AuthRegister`]; kept
/// as a distinct catalog type so the two endpoints stay separately versionable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct AuthLogin {
    #[serde(flatten)]
    pub expectation: MutationExpectation,
    pub username: String,
    pub password: String,
}

/// POST `/auth/logout` body. CAS tokens are required; unknown fields,
/// including credentials, are rejected.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct AuthLogout {
    #[serde(flatten)]
    pub expectation: MutationExpectation,
}

/// Pricing snapshot identity. Distinct from the u64 settings CAS token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct PricingRevision {
    pub pricing_revision: String,
}

/// Stable non-2xx JSON envelope for every Dashboard V3 error.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct V3Error {
    pub code: String,
    pub message: String,
    pub current_revision: Option<u64>,
    pub process_generation: Option<u64>,
}

impl V3Error {
    pub fn unauthorized() -> Self {
        Self {
            code: ERROR_UNAUTHORIZED.to_string(),
            message: "dashboard session is required".to_string(),
            current_revision: None,
            process_generation: None,
        }
    }

    pub fn invalid_json() -> Self {
        Self {
            code: ERROR_INVALID_JSON.to_string(),
            message: "request body must be valid JSON".to_string(),
            current_revision: None,
            process_generation: None,
        }
    }

    pub fn missing_expected_revision() -> Self {
        Self {
            code: ERROR_MISSING_EXPECTED_REVISION.to_string(),
            message: "expectedRevision is required".to_string(),
            current_revision: None,
            process_generation: None,
        }
    }

    pub fn revision_conflict(current_revision: u64, process_generation: u64) -> Self {
        Self {
            code: ERROR_REVISION_CONFLICT.to_string(),
            message: "settings changed since they were loaded; reload and try again".to_string(),
            current_revision: Some(current_revision),
            process_generation: Some(process_generation),
        }
    }

    pub fn invalid_request(message: impl Into<String>) -> Self {
        Self {
            code: ERROR_INVALID_REQUEST.to_string(),
            message: message.into(),
            current_revision: None,
            process_generation: None,
        }
    }

    pub fn invalid_request_at(
        message: impl Into<String>,
        current_revision: u64,
        process_generation: u64,
    ) -> Self {
        Self {
            code: ERROR_INVALID_REQUEST.to_string(),
            message: message.into(),
            current_revision: Some(current_revision),
            process_generation: Some(process_generation),
        }
    }

    pub fn builtin_provider_immutable(
        message: impl Into<String>,
        current_revision: u64,
        process_generation: u64,
    ) -> Self {
        Self {
            code: ERROR_BUILTIN_PROVIDER_IMMUTABLE.to_string(),
            message: message.into(),
            current_revision: Some(current_revision),
            process_generation: Some(process_generation),
        }
    }

    pub fn operation_payload_mismatch(
        message: impl Into<String>,
        current_revision: u64,
        process_generation: u64,
    ) -> Self {
        Self {
            code: ERROR_OPERATION_PAYLOAD_MISMATCH.to_string(),
            message: message.into(),
            current_revision: Some(current_revision),
            process_generation: Some(process_generation),
        }
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self {
            code: ERROR_INTERNAL.to_string(),
            message: message.into(),
            current_revision: None,
            process_generation: None,
        }
    }

    pub fn not_found(
        message: impl Into<String>,
        current_revision: u64,
        process_generation: u64,
    ) -> Self {
        Self {
            code: ERROR_NOT_FOUND.to_string(),
            message: message.into(),
            current_revision: Some(current_revision),
            process_generation: Some(process_generation),
        }
    }

    pub fn conflict(
        message: impl Into<String>,
        current_revision: u64,
        process_generation: u64,
    ) -> Self {
        Self {
            code: ERROR_CONFLICT.to_string(),
            message: message.into(),
            current_revision: Some(current_revision),
            process_generation: Some(process_generation),
        }
    }

    pub fn precondition_failed(
        message: impl Into<String>,
        current_revision: u64,
        process_generation: u64,
    ) -> Self {
        Self {
            code: ERROR_PRECONDITION_FAILED.to_string(),
            message: message.into(),
            current_revision: Some(current_revision),
            process_generation: Some(process_generation),
        }
    }

    pub fn service_unavailable(
        message: impl Into<String>,
        current_revision: u64,
        process_generation: u64,
    ) -> Self {
        Self {
            code: ERROR_SERVICE_UNAVAILABLE.to_string(),
            message: message.into(),
            current_revision: Some(current_revision),
            process_generation: Some(process_generation),
        }
    }

    pub fn not_implemented(
        message: impl Into<String>,
        current_revision: u64,
        process_generation: u64,
    ) -> Self {
        Self {
            code: ERROR_NOT_IMPLEMENTED.to_string(),
            message: message.into(),
            current_revision: Some(current_revision),
            process_generation: Some(process_generation),
        }
    }

    pub fn forbidden(
        message: impl Into<String>,
        current_revision: u64,
        process_generation: u64,
    ) -> Self {
        Self {
            code: ERROR_FORBIDDEN.to_string(),
            message: message.into(),
            current_revision: Some(current_revision),
            process_generation: Some(process_generation),
        }
    }

    pub fn gone(
        message: impl Into<String>,
        current_revision: u64,
        process_generation: u64,
    ) -> Self {
        Self {
            code: ERROR_GONE.to_string(),
            message: message.into(),
            current_revision: Some(current_revision),
            process_generation: Some(process_generation),
        }
    }

    pub fn gateway_timeout(
        message: impl Into<String>,
        current_revision: u64,
        process_generation: u64,
    ) -> Self {
        Self {
            code: ERROR_GATEWAY_TIMEOUT.to_string(),
            message: message.into(),
            current_revision: Some(current_revision),
            process_generation: Some(process_generation),
        }
    }
}

/// Lightweight connection-center payload. The only V3 DTO allowed to carry
/// plaintext primary and sub Key values.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConnectionInfo {
    pub gateway_port: u16,
    pub client_root_url: String,
    pub primary_key: String,
    pub sub_keys: Vec<ConnectionSubKey>,
    pub revision: u64,
    pub process_generation: u64,
}

/// One non-deleted sub Key as exposed by [`ConnectionInfo`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConnectionSubKey {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub value: String,
}

/// Application settings contract. Never contains primary/sub Key plaintext
/// or a field named `gatewayKey` / `key`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct Settings {
    pub revision: u64,
    pub process_generation: u64,
    pub gateway_port: u16,
    pub gateway_port_from_env: bool,
    pub proxy_mode: ProxyMode,
    pub proxy_url: String,
    pub proxy_list_direction: ProxyListDirection,
    pub proxy_list_models: Vec<String>,
    pub proxy_supported_models: Vec<ProxySupportedModel>,
    pub opencode_invite_url: String,
    pub client_root_url: String,
    pub client_root_url_from_env: bool,
    pub auto_start: Option<bool>,
    pub auto_start_supported: bool,
    pub show_dock_icon: Option<bool>,
    pub dock_visibility_supported: bool,
    pub connect_timeout_secs: u64,
    pub non_stream_timeout_secs: u64,
    pub stream_idle_timeout_secs: u64,
    pub routing_mode: RoutingMode,
    pub conversation_sticky: bool,
}

/// PATCH-style settings write. `expectedRevision` and `processGeneration`
/// are required; every other field may be omitted. Unknown fields, including
/// any Key material, are rejected.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct SettingsUpdate {
    #[serde(flatten)]
    pub expectation: MutationExpectation,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gateway_port: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy_mode: Option<ProxyMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy_list_direction: Option<ProxyListDirection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy_list_models: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opencode_invite_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_root_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_start: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub show_dock_icon: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connect_timeout_secs: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub non_stream_timeout_secs: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream_idle_timeout_secs: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub routing_mode: Option<RoutingMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation_sticky: Option<bool>,
}

/// POST `/settings/test-proxy` body. This is an operational diagnostic, not a
/// control-plane mutation: CAS tokens are neither required nor accepted.
/// Unknown fields, including an upstream URL, are rejected. `proxyUrl` and
/// `proxyListDirection` may be omitted; omitted direction keeps the persisted
/// list-mode direction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProxyTestRequest {
    pub proxy_mode: ProxyMode,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub proxy_url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy_list_direction: Option<ProxyListDirection>,
}

/// POST `/settings/test-proxy` result. Any diagnostic HTTP status is success.
/// The body never includes proxy credentials, the diagnostic URL, or the
/// upstream payload. `revision` and `processGeneration` are captured before
/// network I/O and are not bumped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProxyTestResponse {
    pub proxy_mode: ProxyMode,
    pub status: u16,
    pub latency_ms: u64,
    pub revision: u64,
    pub process_generation: u64,
}

/// POST `/keys` body. CAS tokens are required; `name` is required. Unknown
/// fields, including any Key material, are rejected.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct KeyCreate {
    #[serde(flatten)]
    pub expectation: MutationExpectation,
    pub name: String,
}

/// PATCH `/keys/{id}` body. CAS tokens are required; `name` and `enabled`
/// may be omitted. Unknown fields, including any Key material, are rejected.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct KeyUpdate {
    #[serde(flatten)]
    pub expectation: MutationExpectation,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
}

/// One known model backing the list-mode checkbox grid.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProxySupportedModel {
    pub id: String,
    pub preferred_protocol: String,
    pub zen_free: bool,
}

/// Global outbound proxy mode. Wire values stay kebab-case, matching V2.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
#[schemars(rename_all = "kebab-case")]
pub enum ProxyMode {
    Auto,
    Manual,
    Direct,
    List,
}

/// Which listed models take the list-mode exception leg.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
#[schemars(rename_all = "kebab-case")]
pub enum ProxyListDirection {
    Whitelist,
    Blacklist,
}

/// Account selection mode. Wire values stay kebab-case, matching V2.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
#[schemars(rename_all = "kebab-case")]
pub enum RoutingMode {
    StrictPriority,
    StickyGlobal,
    RoundRobin,
}

/// Secret-free account resource. Distinct from `models::Account`.
///
/// Responses emit `T | null` for every optional field. Plaintext upstream
/// keys, passwords, ciphers, gateway Keys, and referral codes never appear.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct Account {
    pub id: String,
    pub provider_id: String,

    pub credential_kind: AccountCredentialKind,
    pub quota_scope: AccountQuotaScope,
    pub name: String,
    pub username: Option<String>,
    pub enabled: bool,
    pub account_type: AccountType,
    pub setup_step: AccountSetupStep,
    pub purchase_date: String,
    pub expires_on: String,
    pub cooldown_until: Option<String>,
    pub cooldown_generic_until: Option<String>,
    pub cooldown_5h_until: Option<String>,
    pub cooldown_week_until: Option<String>,
    pub cooldown_month_until: Option<String>,
    pub cooldown_free_until: Option<String>,
    pub last_error: Option<String>,
    pub auth_error: Option<String>,
    pub notes: Option<String>,
    pub usage_sync_last_success_at: Option<String>,
    pub usage_sync_next_allowed_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub revision: u64,
    pub process_generation: u64,
    pub verification_status: AccountVerificationStatus,
    pub connection_verified_at: Option<String>,
    pub verification_error: Option<String>,
    pub plan_routable: bool,
    pub custom_config: Option<AccountCustomConfig>,
    pub model_capabilities: Vec<AccountModelCapability>,
    #[serde(default)]
    pub ollama_billing_tier: Option<OllamaBillingTier>,
}

/// GET `/accounts` and PUT `/accounts/order` envelope.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccountList {
    pub accounts: Vec<Account>,
    pub revision: u64,
    pub process_generation: u64,
}

/// Successful single-account mutation. `account` is `null` after delete.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccountMutation {
    pub account: Option<Account>,
    pub revision: u64,
    pub process_generation: u64,
}

/// POST `/accounts/transfer/export` body. The password is write-only and used
/// only to encrypt the portable node-state file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccountExportRequest {
    pub bundle_password: String,
}

/// Encrypted node migration package returned by the export endpoint.
/// `bundle` contains only a versioned Argon2id + AES-256-GCM envelope; no
/// plaintext upstream credential is returned to the dashboard.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccountExport {
    pub filename: String,
    pub bundle: String,
    pub exported_accounts: u64,
    pub skipped_accounts: u64,
    pub revision: u64,
    pub process_generation: u64,
}

/// POST `/accounts/transfer/preview` body. `password` is write-only and never
/// appears in a response or log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccountImportPreviewRequest {
    pub password: String,
    pub bundle: String,
}

/// Secret-free preview of one node migration package.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccountImportPreview {
    pub exported_at: String,
    pub items: Vec<AccountImportPreviewItem>,
    pub importable_accounts: u64,
    pub duplicate_accounts: u64,
    pub revision: u64,
    pub process_generation: u64,
}

/// One secret-free preview/result row. Account Keys, endpoint credentials,
/// browser identity, and verification evidence never appear.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccountImportPreviewItem {
    pub index: u64,
    pub name: String,
    pub provider_id: String,

    pub account_type: AccountType,
    pub disposition: AccountImportDisposition,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[schemars(rename_all = "snake_case")]
pub enum AccountImportDisposition {
    Import,
    Imported,
    Merge,
    Merged,
    Duplicate,
}

/// POST `/accounts/transfer/import` body. CAS tokens are rechecked only after
/// the expensive authenticated decryption and validation phase finishes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccountImportRequest {
    #[serde(flatten)]
    pub expectation: MutationExpectation,
    pub password: String,
    pub bundle: String,
}

/// Atomic database import result. `items` is secret-free and preserves bundle
/// order; duplicates are the only per-row skip in V1.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccountImportResult {
    pub items: Vec<AccountImportPreviewItem>,
    pub imported_accounts: u64,
    pub duplicate_accounts: u64,
    pub revision: u64,
    pub process_generation: u64,
}

/// Nested Custom HTTP destination as returned on an account.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccountCustomConfig {
    pub account_id: String,
    pub endpoint_url: String,
    pub upstream_protocol: AccountUpstreamProtocol,
    pub created_at: String,
    pub updated_at: String,
}

/// One declared Custom model capability as returned on an account.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccountModelCapability {
    pub public_model: String,
    pub upstream_model: String,
    pub protocol: AccountUpstreamProtocol,
    pub verified_at: Option<String>,
    pub source: String,
}

/// POST `/accounts` body. CAS tokens and `name` are required. `key`,
/// `password`, and `referralCode` are write-only and never echoed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccountCreate {
    #[serde(flatten)]
    pub expectation: MutationExpectation,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_id: Option<String>,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
    pub key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub referral_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub purchase_date: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_config: Option<AccountCustomConfigWrite>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub model_capabilities: Vec<AccountModelCapabilityWrite>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ollama_billing_tier: Option<OllamaBillingTier>,
}

/// POST `/accounts/managed` body. CAS tokens and `name` are required.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccountManagedCreate {
    #[serde(flatten)]
    pub expectation: MutationExpectation,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

/// PATCH `/accounts/{id}` body. CAS tokens are required; other fields may be
/// omitted. Write-only `key` / `password` / `referralCode` are accepted and
/// never echoed. Unknown fields, including provider binding, are rejected.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccountUpdate {
    #[serde(flatten)]
    pub expectation: MutationExpectation,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub referral_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub purchase_date: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ollama_billing_tier: Option<OllamaBillingTier>,
}

/// PUT `/accounts/order` body. CAS tokens and the complete id set are required.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccountOrder {
    #[serde(flatten)]
    pub expectation: MutationExpectation,
    pub account_ids: Vec<String>,
}

/// PATCH `/accounts/{id}/setup` body. CAS tokens and `setupStep` are required.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccountSetupUpdate {
    #[serde(flatten)]
    pub expectation: MutationExpectation,
    pub setup_step: AccountSetupStep,
}

/// POST `/accounts/{id}/setup/verify-key` body. CAS tokens and the write-only
/// `key` are required. Unknown fields are rejected. The key is never echoed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccountManagedKeyVerify {
    #[serde(flatten)]
    pub expectation: MutationExpectation,
    pub key: String,
}

/// PUT `/accounts/{id}/custom-config` body. The endpoint binding and complete
/// model capability list are replaced atomically under one CAS expectation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccountCustomConfigUpdate {
    #[serde(flatten)]
    pub expectation: MutationExpectation,
    pub endpoint_url: String,
    pub upstream_protocol: AccountUpstreamProtocol,
    pub model_capabilities: Vec<AccountModelCapabilityWrite>,
}

/// Create-time Custom destination (no timestamps). Nested under `AccountCreate`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccountCustomConfigWrite {
    pub endpoint_url: String,
    pub upstream_protocol: AccountUpstreamProtocol,
}

/// PUT `/accounts/{id}/model-capabilities` body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccountModelCapabilitiesUpdate {
    #[serde(flatten)]
    pub expectation: MutationExpectation,
    pub capabilities: Vec<AccountModelCapabilityWrite>,
}

/// One declared Custom model capability on create or replace. Canonical writes
/// carry both identities; the legacy `modelId` shape remains accepted so a
/// stale dashboard can complete one migration-window write without data loss.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum AccountModelCapabilityWrite {
    Canonical(AccountModelCapabilityWriteCanonical),
    Legacy(AccountModelCapabilityWriteLegacy),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccountModelCapabilityWriteCanonical {
    pub public_model: String,
    pub upstream_model: String,
    pub protocol: AccountUpstreamProtocol,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccountModelCapabilityWriteLegacy {
    pub model_id: String,
    pub protocol: AccountUpstreamProtocol,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

/// POST `/accounts/{id}/verify` body. CAS tokens are required. Unknown
/// fields, including any Key material, are rejected.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccountVerify {
    #[serde(flatten)]
    pub expectation: MutationExpectation,
}

/// POST `/accounts/{id}/model-tests` body. This is an operational test, so it
/// intentionally carries no CAS tokens and has no account mutation effect.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccountModelTestRequest {
    pub model_id: String,
}

/// Result of one exact-account, one-model operational request. Error text is
/// transport-sanitized and never includes an upstream response body or key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccountModelTestResponse {
    pub account_id: String,
    pub model_id: String,
    pub protocol: AccountUpstreamProtocol,
    pub success: bool,
    pub http_status: Option<u16>,
    pub duration_ms: u64,
    pub error: Option<String>,
}

/// Wire identity matching V2 `api_key` / `none`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[schemars(rename_all = "snake_case")]
pub enum AccountCredentialKind {
    ApiKey,
    None,
}

impl From<CredentialKind> for AccountCredentialKind {
    fn from(value: CredentialKind) -> Self {
        match value {
            CredentialKind::ApiKey => Self::ApiKey,
            CredentialKind::None => Self::None,
        }
    }
}

/// Wire identity matching V2 `key` / `egress-ip`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
#[schemars(rename_all = "kebab-case")]
pub enum AccountQuotaScope {
    Key,
    EgressIp,
}

impl From<QuotaScope> for AccountQuotaScope {
    fn from(value: QuotaScope) -> Self {
        match value {
            QuotaScope::Key => Self::Key,
            QuotaScope::EgressIp => Self::EgressIp,
        }
    }
}

/// Wire identity matching V2 `key` / `managed`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[schemars(rename_all = "snake_case")]
pub enum AccountType {
    Key,
    Managed,
}

impl From<ModelAccountType> for AccountType {
    fn from(value: ModelAccountType) -> Self {
        match value {
            ModelAccountType::Key => Self::Key,
            ModelAccountType::Managed => Self::Managed,
        }
    }
}

impl From<AccountType> for ModelAccountType {
    fn from(value: AccountType) -> Self {
        match value {
            AccountType::Key => Self::Key,
            AccountType::Managed => Self::Managed,
        }
    }
}

/// Managed-setup wizard step. Wire values match V2 snake_case.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[schemars(rename_all = "snake_case")]
pub enum AccountSetupStep {
    GoogleAccount,
    OpencodeRegistration,
    Payment,
    KeyVerification,
    Ready,
}

impl From<ModelAccountSetupStep> for AccountSetupStep {
    fn from(value: ModelAccountSetupStep) -> Self {
        match value {
            ModelAccountSetupStep::GoogleAccount => Self::GoogleAccount,
            ModelAccountSetupStep::OpencodeRegistration => Self::OpencodeRegistration,
            ModelAccountSetupStep::Payment => Self::Payment,
            ModelAccountSetupStep::KeyVerification => Self::KeyVerification,
            ModelAccountSetupStep::Ready => Self::Ready,
        }
    }
}

impl From<AccountSetupStep> for ModelAccountSetupStep {
    fn from(value: AccountSetupStep) -> Self {
        match value {
            AccountSetupStep::GoogleAccount => Self::GoogleAccount,
            AccountSetupStep::OpencodeRegistration => Self::OpencodeRegistration,
            AccountSetupStep::Payment => Self::Payment,
            AccountSetupStep::KeyVerification => Self::KeyVerification,
            AccountSetupStep::Ready => Self::Ready,
        }
    }
}

/// Connection-verification status. Wire values match V2 snake_case.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[schemars(rename_all = "snake_case")]
pub enum AccountVerificationStatus {
    NotRequired,
    Pending,
    Verified,
    Failed,
}

impl From<ProviderVerificationStatus> for AccountVerificationStatus {
    fn from(value: ProviderVerificationStatus) -> Self {
        match value {
            ProviderVerificationStatus::NotRequired => Self::NotRequired,
            ProviderVerificationStatus::Pending => Self::Pending,
            ProviderVerificationStatus::Verified => Self::Verified,
            ProviderVerificationStatus::Failed => Self::Failed,
        }
    }
}

/// Custom/upstream protocol. Wire values match V2 snake_case.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[schemars(rename_all = "snake_case")]
pub enum AccountUpstreamProtocol {
    ChatCompletions,
    Responses,
    Messages,
}

impl From<UpstreamProtocolKind> for AccountUpstreamProtocol {
    fn from(value: UpstreamProtocolKind) -> Self {
        match value {
            UpstreamProtocolKind::ChatCompletions => Self::ChatCompletions,
            UpstreamProtocolKind::Responses => Self::Responses,
            UpstreamProtocolKind::Messages => Self::Messages,
        }
    }
}

impl From<AccountUpstreamProtocol> for UpstreamProtocolKind {
    fn from(value: AccountUpstreamProtocol) -> Self {
        match value {
            AccountUpstreamProtocol::ChatCompletions => Self::ChatCompletions,
            AccountUpstreamProtocol::Responses => Self::Responses,
            AccountUpstreamProtocol::Messages => Self::Messages,
        }
    }
}

/// Custom auth scheme. Wire values match V2 kebab-case.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
#[schemars(rename_all = "kebab-case")]
pub enum AccountAuthScheme {
    Bearer,
    XApiKey,
    ApiKey,
}

impl From<UpstreamAuthScheme> for AccountAuthScheme {
    fn from(value: UpstreamAuthScheme) -> Self {
        match value {
            UpstreamAuthScheme::Bearer => Self::Bearer,
            UpstreamAuthScheme::XApiKey => Self::XApiKey,
            UpstreamAuthScheme::ApiKey => Self::ApiKey,
        }
    }
}

impl From<AccountAuthScheme> for UpstreamAuthScheme {
    fn from(value: AccountAuthScheme) -> Self {
        match value {
            AccountAuthScheme::Bearer => Self::Bearer,
            AccountAuthScheme::XApiKey => Self::XApiKey,
            AccountAuthScheme::ApiKey => Self::ApiKey,
        }
    }
}

/// Built-in Plan catalog. Model capabilities are a separate DTO.
///
/// `pricingRevision` is the live pricing snapshot id, not a CAS token.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProviderCatalog {
    pub entries: Vec<ProviderCatalogEntry>,
    pub revision: u64,
    pub process_generation: u64,
    pub pricing_revision: String,
}

/// One Provider Registry entry as a wire catalog row. Identity strings are
/// data copied from the static registry; this DTO does not define them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProviderCatalogEntry {
    pub provider_id: String,

    /// Row provenance in the unified `providers` table. Wire values:
    /// `builtin` (sealed adapter), `preset` (preset-derived dynamic row),
    /// `custom` (manual dynamic row).
    pub origin: ProviderOrigin,
    /// Whether the dashboard may PATCH this entry. Always `false` for
    /// `builtin` rows; `true` for `preset`/`custom`.
    pub editable: bool,
    /// Whether the dashboard may DELETE this entry. Same rules as `editable`.
    pub deletable: bool,
    /// Plan/api offering label carried by the catalog row. Builtin rows use
    /// the sealed builtin map; dynamic rows mirror the persisted `offering`.
    pub offering: String,

    pub display_name: String,
    pub display_family: String,
    pub credential_kind: AccountCredentialKind,
    pub quota_scope: AccountQuotaScope,
    pub singleton: bool,
    pub creation_availability: String,
    pub creation_unavailable_reason: Option<String>,
    pub verification_policy: String,
    pub verification_runtime_availability: String,
    pub routable: bool,
    pub managed_registration: bool,
    pub pricing_availability: String,
    pub usage_availability: String,
    pub manual_usage_calibration: bool,
    pub quota_unit: String,
    pub model_source: String,
    pub key_prefix: Option<String>,
    pub auth_schemes: Vec<AccountAuthScheme>,
    pub upstream_protocols: Vec<AccountUpstreamProtocol>,
    pub form_fields: Vec<ProviderCatalogFormField>,
    pub model_aliases: Vec<String>,
}

/// One create-form field advertised by a catalog provider.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProviderCatalogFormField {
    pub id: String,
    pub kind: String,
    pub required: bool,
    pub immutable_after_create: bool,
}

/// Plan risk notice shown before create. `body` is the acknowledgement text,
/// not a secret.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProviderCatalogRiskNotice {
    pub acknowledgement_id: String,
    pub version: String,
    pub source_url: String,
    pub body: String,
    pub content_hash: String,
}

/// One Go protocol-table capability. GOAT must not reuse these rows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProviderModelCapability {
    pub model_id: String,
    pub provider_id: String,

    pub preferred_protocol: AccountUpstreamProtocol,
    pub supported_protocols: Vec<AccountUpstreamProtocol>,
}

/// Zen Free enablement after a successful settings write.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct ZenFreeSettings {
    pub account_id: String,
    pub enabled: bool,
    pub revision: u64,
    pub process_generation: u64,
    pub pricing_revision: String,
}

/// PATCH Zen Free enablement. CAS tokens and `enabled` are required.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct ZenFreeSettingsUpdate {
    #[serde(flatten)]
    pub expectation: MutationExpectation,
    pub enabled: bool,
}

/// Last successful Zen Free catalog snapshot. `sourceUrl` is the public
/// official directory, not a credential.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct ZenFreeModels {
    pub account_id: String,
    pub models: Vec<ZenFreeModel>,
    pub refreshed_at: Option<String>,
    pub source_url: String,
    pub revision: u64,
    pub process_generation: u64,
    pub pricing_revision: String,
}

/// One persisted Zen Free model and its de-suffixed alias.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct ZenFreeModel {
    pub model_id: String,
    pub alias: String,
}

/// Effective provider-scope and custom-endpoint contracts.
///
/// Top-level `revision` is the settings CAS token. Nested `revision` values
/// are display-only and must not be sent as `expectedRevision`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProviderContracts {
    pub providers: Vec<ProviderContractGroup>,
    pub custom_endpoints: Vec<CustomEndpointContract>,
    pub revision: u64,
    pub process_generation: u64,
    pub pricing_revision: String,
}

/// One built-in Provider contract scope.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProviderContractGroup {
    pub scope_kind: ContractScopeKind,
    pub scope_id: String,
    pub provider_id: String,
    /// The built-in provider's static protocol-evidence snapshot date. This is
    /// `null` when the scope has no restorable snapshot and is distinct from
    /// catalog `refreshed_at`.
    pub static_protocol_snapshot_date: Option<String>,
    pub accounts: Vec<ProviderAccountChoice>,
    pub catalog: EffectiveCatalog,
    pub models: Vec<EffectiveModelContract>,
    pub pricing: CapabilitySummary,
    pub usage: CapabilitySummary,
    pub card: CardCapabilitySummary,
    pub catalog_routable: bool,
    pub production_inference: bool,
    pub disabled_reasons: Vec<String>,
    /// Display revision for this scope, distinct from the top-level CAS token.
    pub revision: u64,
}

/// One Custom API account scope. Distinct from built-in provider groups.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct CustomEndpointContract {
    pub scope_kind: ContractScopeKind,
    pub scope_id: String,
    pub provider_id: String,
    pub account: ProviderAccountChoice,
    pub catalog: EffectiveCatalog,
    pub models: Vec<EffectiveModelContract>,
    pub pricing: CapabilitySummary,
    pub usage: CapabilitySummary,
    pub card: CardCapabilitySummary,
    pub catalog_routable: bool,
    pub production_inference: bool,
    pub disabled_reasons: Vec<String>,
    /// Display revision for this endpoint, distinct from the top-level CAS token.
    pub revision: u64,
}

/// Secret-free account identity on a contract card.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProviderAccountChoice {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub verification_status: AccountVerificationStatus,
}

/// Per-model/per-protocol override state. `auto` removes any persisted override.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[schemars(rename_all = "snake_case", deny_unknown_fields)]
pub enum ProtocolOverrideState {
    Auto,
    ForceOn,
    ForceOff,
}

/// Merged model-id catalog for one contract scope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct EffectiveCatalog {
    pub source: String,
    pub source_url: String,
    pub refreshed_at: Option<String>,
    pub models: Vec<String>,
    pub refresh_supported: bool,
}

/// One model's preferred protocol and per-protocol evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct EffectiveModelContract {
    pub alias: String,
    pub model_id: String,
    pub preferred_protocol: AccountUpstreamProtocol,
    pub protocols: EffectiveModelProtocols,
    pub routable: bool,
    pub disabled_reasons: Vec<String>,
}

/// Per-protocol evidence keyed by snake_case protocol tokens. Missing
/// protocols serialize as `null` and must not be invented as available.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[schemars(rename_all = "snake_case", deny_unknown_fields)]
pub struct EffectiveModelProtocols {
    pub chat_completions: Option<EffectiveProtocolEvidence>,
    pub responses: Option<EffectiveProtocolEvidence>,
    pub messages: Option<EffectiveProtocolEvidence>,
}

/// Merged evidence for one upstream protocol on one model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct EffectiveProtocolEvidence {
    pub protocol: AccountUpstreamProtocol,
    pub available: bool,
    pub enabled: bool,
    pub source: ContractEvidenceSource,
    pub verified_at: Option<String>,
    pub observed_at: Option<String>,
    pub last_probe_result: Option<ProbeResultKind>,
    pub last_probe_at: Option<String>,
    pub last_probe_error: Option<String>,
    #[serde(rename = "override")]
    #[schemars(rename = "override")]
    pub r#override: ProtocolOverrideState,
}

/// Registry pricing or usage availability copied as display data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct CapabilitySummary {
    pub availability: String,
}

/// Provider-page actions that are actually implemented for this scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct CardCapabilitySummary {
    pub fetch_zen_models: bool,
    pub discover_models: bool,
    pub protocol_probe: bool,
    pub catalog_refresh: bool,
}

/// Provider-scoped model-directory refresh. OpenCode Go requires a selected
/// account credential; Command Code exposes a public Provider catalog.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ProviderModelsRefreshUpdate {
    #[serde(flatten)]
    pub expectation: MutationExpectation,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ProviderModels {
    pub provider_id: String,
    pub account_id: Option<String>,
    pub models: Vec<String>,
    pub refreshed_at: String,
    pub source_url: String,
    pub revision: u64,
    pub process_generation: u64,
    pub pricing_revision: String,
}

/// One per-cell override entry in a batch update.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModelProtocolOverride {
    pub model_id: String,
    pub protocol: AccountUpstreamProtocol,
    pub state: ProtocolOverrideState,
    /// Remember this conversion-default protocol, even while disabled.
    /// Omitted or false preserves the saved choice; static reset clears it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "bool")]
    pub preferred: Option<bool>,
}

/// PUT a batch of per-model/per-protocol overrides for one contract scope.
/// An empty `overrides` array is rejected by handlers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModelProtocolOverridesUpdate {
    #[serde(flatten)]
    pub expectation: MutationExpectation,
    pub overrides: Vec<ModelProtocolOverride>,
    /// Explicit consent to add this batch's enabled builtin endpoint grants.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub authorize_credential_ids: Vec<String>,
}

/// POST protocol-probe body. `accountId` is a deprecated compatibility field
/// and is ignored; the provider chooses eligible accounts automatically.
/// `protocols` is the required explicit probe set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProtocolProbeRequest {
    #[serde(flatten)]
    pub expectation: MutationExpectation,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
    pub model_id: String,
    pub protocols: Vec<AccountUpstreamProtocol>,
}

/// One requested protocol's probe outcome.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProtocolProbeResult {
    pub protocol: AccountUpstreamProtocol,
    pub success: bool,
    pub skipped: bool,
    pub error: Option<String>,
}

/// Protocol-probe mutation result. Identity strings are always present;
/// `contract` is required `T | null` on the wire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProtocolProbeResponse {
    /// Deprecated compatibility field. Provider-level probes can use different
    /// accounts per protocol, so this is always `null`.
    pub account_id: Option<String>,
    pub provider_id: String,
    pub model_id: String,
    pub results: Vec<ProtocolProbeResult>,
    pub contract: Option<EffectiveModelContract>,
    pub revision: u64,
    pub process_generation: u64,
    pub pricing_revision: String,
}

/// POST `/custom/models/discover` body. Operational probe: CAS tokens must
/// not be sent. `apiKey` is write-only; an edit form may send `accountId`
/// instead so the handler can use the stored Custom key. Unknown fields are
/// rejected.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct CustomModelDiscoveryRequest {
    pub endpoint_url: String,
    pub upstream_protocol: AccountUpstreamProtocol,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
}

/// Custom model-list probe result. Identity tokens are the captured current
/// revision / process generation / pricing snapshot id; this response never
/// echoes the supplied key or mutates control state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct CustomModelDiscoveryResponse {
    pub models: Vec<String>,
    pub truncated: bool,
    pub revision: u64,
    pub process_generation: u64,
    pub pricing_revision: String,
}

/// GET `/settings/check-update` result. Identity tokens are captured before
/// the GitHub await and are not bumped. `releaseUrl` is the fixed public
/// GitHub latest-release page, never the outbound API URL or a loopback seam.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateCheck {
    pub current_version: String,
    pub latest_version: String,
    pub update_available: bool,
    pub release_url: String,
    pub install_supported: bool,
    pub revision: u64,
    pub process_generation: u64,
}

/// Desktop signed-update status machine as exposed by GET `/settings/update-status`
/// and POST `/settings/install-update`. `total` and `error` stay required `T | null`.
/// Identity tokens are the live CAS pair and are not bumped by polling or install.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct DesktopUpdate {
    pub phase: DesktopUpdatePhase,
    pub downloaded: u64,
    pub total: Option<u64>,
    pub error: Option<String>,
    pub current_version: String,
    pub install_supported: bool,
    pub revision: u64,
    pub process_generation: u64,
}

/// Update-status phase. Wire values stay lowercase, matching V2.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
#[schemars(rename_all = "lowercase")]
pub enum DesktopUpdatePhase {
    Idle,
    Checking,
    Downloading,
    Installing,
    Failed,
}

/// POST `/settings/install-update` body. CAS tokens and `expectedVersion` are
/// required. Unknown fields are rejected. This mutation does not bump revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct InstallUpdate {
    #[serde(flatten)]
    pub expectation: MutationExpectation,
    pub expected_version: String,
}

/// Contract scope kind. Wire values match V2 `provider` / `custom_endpoint`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[schemars(rename_all = "snake_case")]
pub enum ContractScopeKind {
    Provider,
    CustomEndpoint,
}

impl From<DomainContractScopeKind> for ContractScopeKind {
    fn from(value: DomainContractScopeKind) -> Self {
        match value {
            DomainContractScopeKind::Provider => Self::Provider,
            DomainContractScopeKind::CustomEndpoint => Self::CustomEndpoint,
        }
    }
}

/// How a protocol row was established. Wire values match V2 snake_case.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[schemars(rename_all = "snake_case")]
pub enum ContractEvidenceSource {
    Static,
    Preset,
    ProbeConfirmed,
    ProbeObserved,
}

impl From<DomainContractEvidenceSource> for ContractEvidenceSource {
    fn from(value: DomainContractEvidenceSource) -> Self {
        match value {
            DomainContractEvidenceSource::Static => Self::Static,
            DomainContractEvidenceSource::Preset => Self::Preset,
            DomainContractEvidenceSource::ProbeConfirmed => Self::ProbeConfirmed,
            DomainContractEvidenceSource::ProbeObserved => Self::ProbeObserved,
        }
    }
}

/// Last explicit probe outcome stored on evidence. Distinct from
/// [`ProtocolProbeResult`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[schemars(rename_all = "snake_case")]
pub enum ProbeResultKind {
    Success,
    Failure,
}

impl From<DomainProbeResultKind> for ProbeResultKind {
    fn from(value: DomainProbeResultKind) -> Self {
        match value {
            DomainProbeResultKind::Success => Self::Success,
            DomainProbeResultKind::Failure => Self::Failure,
        }
    }
}

/// Dashboard V3 pricing snapshot. Distinct from `kernel::pricing::PricingSnapshot`
/// and from the stored provider pricing blob.
///
/// `revision` is the settings CAS token. The official snapshot id is
/// `pricingRevision` and must never be named `revision` on this wire type.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct PricingSnapshot {
    pub revision: u64,
    pub process_generation: u64,
    pub pricing_revision: String,
    pub activated_at: String,
    pub document_updated_at: String,
    pub source_url: String,
    pub content_hash: String,
    pub adjustment_policy_version: String,
    pub limits: PricingLimits,
    pub models: Vec<PricingModel>,
}

/// OpenCode Go 5h / week / month usage windows.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct PricingLimits {
    pub window_5h: f64,
    pub window_week: f64,
    pub window_month: f64,
}

/// One official model row, including optional cache-write and token-tier bounds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct PricingModel {
    pub model_id: String,
    pub display_name: String,
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_write: Option<f64>,
    pub usage: f64,
    pub quota_multiplier: f64,
    pub min_input_tokens: Option<i64>,
    pub max_input_tokens: Option<i64>,
    pub time_window: PricingTimeWindow,
    pub adjustments: Vec<PricingAdjustment>,
}

/// One documented local adjustment on a model row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct PricingAdjustment {
    pub label: String,
    pub multiplier: f64,
    pub applies_to: String,
}

/// Official Peak / Off-Peak row. Wire values stay snake_case.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[schemars(rename_all = "snake_case")]
pub enum PricingTimeWindow {
    Always,
    OffPeak,
    Peak,
}

/// Pricing refresh result. Nested `snapshot` is required; nullable fields emit `T | null`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct PricingRefresh {
    pub snapshot: PricingSnapshot,
    pub refresh_status: PricingRefreshStatus,
    pub multiplier_changes: Vec<PricingMultiplierChange>,
    pub official_content_hash: Option<String>,
    pub error: Option<String>,
}

/// Refresh outcome. Wire values stay snake_case.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[schemars(rename_all = "snake_case")]
pub enum PricingRefreshStatus {
    Success,
    Unchanged,
    NeedsConfirmation,
    FailedNoChange,
}

/// One model whose official multiplier differs from the active snapshot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct PricingMultiplierChange {
    pub model_id: String,
    pub current_multiplier: f64,
    pub official_multiplier: f64,
}

/// POST pricing-refresh body. CAS tokens and `expectedPricingRevision` are
/// required; `policy` and `expectedOfficialContentHash` may be omitted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct PricingRefreshUpdate {
    #[serde(flatten)]
    pub expectation: MutationExpectation,
    pub expected_pricing_revision: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy: Option<PricingRefreshPolicy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_official_content_hash: Option<String>,
}

/// Refresh confirmation policy. Wire values stay snake_case.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[schemars(rename_all = "snake_case")]
pub enum PricingRefreshPolicy {
    KeepCurrent,
    UseOfficial,
}

/// PUT provider pricing-multipliers body. CAS tokens,
/// `expectedPricingRevision` (the selected provider's active revision), and
/// `multipliers` are required.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct PricingMultipliersUpdate {
    #[serde(flatten)]
    pub expectation: MutationExpectation,
    pub expected_pricing_revision: String,
    pub multipliers: Vec<PricingMultiplierWrite>,
}

/// One multiplier write. Handlers validate model id, range, and uniqueness.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct PricingMultiplierWrite {
    pub model_id: String,
    pub multiplier: f64,
}

/// Provider-scoped pricing. `snapshot` is required `T | null` and never
/// carries a raw stored pricing blob.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProviderPricing {
    pub provider_id: String,

    pub availability: PricingAvailability,
    pub snapshot: Option<PricingSnapshot>,
    pub provider_snapshot: Option<ProviderPricingSnapshot>,
    pub revision: u64,
    pub process_generation: u64,
    pub pricing_revision: String,
    pub provider_pricing_revision: String,
}

/// Result of refreshing the priced snapshot owned by one Provider. Provider
/// failures are isolated: this response never represents a cross-Provider
/// transaction.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProviderPricingRefresh {
    pub provider_id: String,
    pub refresh_status: PricingRefreshStatus,
    pub multiplier_changes: Vec<PricingMultiplierChange>,
    pub official_content_hash: Option<String>,
    pub error: Option<String>,
    /// The refreshed Go snapshot. Provider-neutral plans expose their active
    /// snapshot through the provider pricing read endpoint instead.
    pub snapshot: Option<PricingSnapshot>,
    pub revision: u64,
    pub process_generation: u64,
    pub pricing_revision: String,
    pub provider_pricing_revision: String,
}

/// POST Provider pricing-refresh body. The Provider-local pricing revision is
/// distinct from the global Go `pricingRevision` control token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProviderPricingRefreshUpdate {
    #[serde(flatten)]
    pub expectation: MutationExpectation,
    pub expected_provider_pricing_revision: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy: Option<PricingRefreshPolicy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_official_content_hash: Option<String>,
}

/// Provider-neutral immutable pricing snapshot. GOAT uses this shape both for
/// dashboard display and provider-scoped per-request cost attribution.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProviderPricingSnapshot {
    pub revision: String,
    pub activated_at: String,
    pub document_updated_at: Option<String>,
    pub source_url: String,
    pub content_hash: String,
    pub evidence: String,
    pub values: Vec<ProviderPricingValue>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProviderPricingValue {
    pub model_id: String,
    pub display_name: String,
    pub input_per_million: Option<f64>,
    pub output_per_million: Option<f64>,
    pub cache_read_per_million: Option<f64>,
    pub cache_write_per_million: Option<f64>,
    pub plan_limit: Option<f64>,
    pub model_allowance: Option<f64>,
    pub quota_multiplier: Option<f64>,
    pub paid_plan_price: Option<f64>,
    pub currency: Option<String>,
    pub min_input_tokens: Option<i64>,
    pub max_input_tokens: Option<i64>,
    pub time_window: PricingTimeWindow,
}

/// Registry pricing availability. Wire values stay snake_case.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[schemars(rename_all = "snake_case")]
pub enum PricingAvailability {
    Available,
    Unavailable,
    NotApplicable,
    Unpriced,
}

/// Secret-free gateway listener view. The plaintext Key lives only on
/// [`ConnectionInfo`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct GatewayStatus {
    pub running: bool,
    pub port: u16,
    pub upstream_base_url: String,
    pub last_error: Option<String>,
    pub revision: u64,
    pub process_generation: u64,
    pub pricing_revision: String,
}

/// Local Go-routable Alias ∩ current Go pricing snapshot (no request-time upstream).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct ApplicationModels {
    pub models: Vec<String>,
    pub revision: u64,
    pub process_generation: u64,
    pub pricing_revision: String,
}

/// Dashboard home totals. `availableAccounts` counts accounts that can
/// contribute at least one currently enabled production route.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct DashboardSummary {
    pub total_accounts: u64,
    pub available_accounts: u64,
    pub gateway_running: bool,
    pub today_cost: f64,
    pub week_cost: f64,
    pub month_cost: f64,
    pub revision: u64,
    pub process_generation: u64,
    pub pricing_revision: String,
}

/// One UTC day / model token bucket. `date` is `YYYY-MM-DD`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct DailyModelTokens {
    pub date: String,
    pub model: String,
    pub tokens: i64,
}

/// GET `/dashboard/daily-tokens-by-model` envelope.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct DailyTokensByModel {
    pub items: Vec<DailyModelTokens>,
    pub revision: u64,
    pub process_generation: u64,
    pub pricing_revision: String,
}

/// One redacted gateway log row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct GatewayLog {
    pub id: i64,
    pub level: String,
    pub category: String,
    pub message: String,
    pub created_at: String,
    pub request_id: Option<String>,
    pub attempt: Option<i64>,
    pub error_source: Option<String>,
    pub error_stage: Option<String>,
    pub duration_ms: Option<i64>,
    pub diagnostic: Option<Value>,
}

/// GET `/logs/gateway` envelope.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct GatewayLogs {
    pub items: Vec<GatewayLog>,
    pub revision: u64,
    pub process_generation: u64,
    pub pricing_revision: String,
}

/// One redacted forward-log row plus native model identity. There is no
/// `requestedAlias` field.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct ForwardLog {
    pub id: i64,
    pub timestamp: String,
    pub model: String,
    pub account_id: String,
    pub account_name: String,
    pub route_account_id: Option<String>,
    pub provider_id: Option<String>,

    pub credential_account_id: Option<String>,
    pub client_key_id: Option<String>,
    pub client_key_name: Option<String>,
    pub status: String,
    pub http_status: Option<i32>,
    pub route: String,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub cached_tokens: i64,
    pub cache_creation_tokens: i64,
    pub cost: Option<f64>,
    pub raw_cost_usd: Option<f64>,
    pub quota_debit: Option<f64>,
    pub effective_paid_cost_usd: Option<f64>,
    pub pricing_revision_id: Option<String>,
    pub quota_multiplier: Option<f64>,
    pub local_adjustment_multiplier: Option<f64>,
    pub service_tier: Option<String>,
    pub cost_state: String,
    pub error_message: Option<String>,
    pub request_id: Option<String>,
    pub attempt: Option<i64>,
    pub error_source: Option<String>,
    pub error_stage: Option<String>,
    pub duration_ms: Option<i64>,
    pub diagnostic: Option<Value>,
    pub requested_model: Option<String>,
    pub resolved_alias: Option<String>,
    pub upstream_model: Option<String>,
    pub native_cost_value: Option<f64>,
    pub native_cost_unit: Option<String>,
    pub native_cost_currency: Option<String>,
}

/// Aggregates for the current forward-log filter, computed before paging.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct ForwardLogSummary {
    pub total_requests: i64,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub cached_tokens: i64,
    pub cost: f64,
}

/// GET `/logs/forward` envelope.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct ForwardLogs {
    pub items: Vec<ForwardLog>,
    pub summary: ForwardLogSummary,
    pub revision: u64,
    pub process_generation: u64,
    pub pricing_revision: String,
}

/// One historical client-key identity observed in forward logs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct ForwardLogClientKey {
    pub id: String,
    pub name: String,
}

/// GET `/logs/forward/keys` envelope. Includes disabled, deleted, and
/// dangling identities so stored rows remain selectable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct ForwardLogKeys {
    pub keys: Vec<ForwardLogClientKey>,
    pub revision: u64,
    pub process_generation: u64,
    pub pricing_revision: String,
}

/// GET `/logs/forward/models` envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct ForwardLogModels {
    pub models: Vec<String>,
    pub revision: u64,
    pub process_generation: u64,
    pub pricing_revision: String,
}

/// GET `/logs/gateway` query. All fields may be omitted.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct GatewayLogQuery {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
}

/// GET `/logs/forward` query. Filter/sort tokens keep V2 values
/// (`prompt_tokens`, `asc`/`desc`). Unknown fields are rejected.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct ForwardLogQuery {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route_account_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_account_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_time: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_time: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sort_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sort_order: Option<String>,
}

/// GET `/dashboard/daily-tokens-by-model` query. `days` may be omitted.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct DailyTokensQuery {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub days: Option<i64>,
}

/// GET `/accounts/{id}/usage` body. Distinct from `models::UsageWindow`.
///
/// `revision` is the settings CAS token and is not advanced by calibration.
/// `pricingRevision` is present when the projection uses the live Go snapshot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct UsageWindow {
    pub account_id: String,
    pub window_5h: f64,
    pub window_week: f64,
    pub window_month: f64,
    pub resets_in_5h: Option<String>,
    pub resets_in_week: Option<String>,
    pub resets_in_month: Option<String>,
    pub revision: u64,
    pub process_generation: u64,
    pub pricing_revision: Option<String>,
}

/// PATCH `/accounts/{id}/usage` envelope. Calibration does not bump revision.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct UsageMutation {
    pub usage: UsageWindow,
    pub revision: u64,
    pub process_generation: u64,
}

/// PATCH `/accounts/{id}/usage` body. CAS tokens, `window`, and `percent` are
/// required; `resetsInMinutes` may be omitted. Unknown fields are rejected.
/// Window tokens stay the V2 identifiers `window_5h` / `window_week` /
/// `window_month`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccountUsageUpdate {
    #[serde(flatten)]
    pub expectation: MutationExpectation,
    pub window: String,
    pub percent: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resets_in_minutes: Option<i64>,
}

/// GET `/accounts/{id}/provider-usage` body. Distinct from stored quota rows.
///
/// `pricingRevision` is present when live Go quota windows use one captured
/// pricing snapshot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProviderUsage {
    pub account_id: String,
    pub provider_id: String,

    pub availability: UsageAvailability,
    pub experimental: bool,
    pub free_cooldown_until: Option<String>,
    pub quota_windows: Vec<QuotaWindow>,
    pub credit_balances: Vec<CreditBalance>,
    pub sync_state: Option<UsageSyncState>,
    pub revision: u64,
    pub process_generation: u64,
    pub pricing_revision: Option<String>,
}

/// One live or synthetic quota window. Distinct from `models::QuotaWindow`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct QuotaWindow {
    pub account_id: String,
    pub window_kind: String,
    pub used: f64,
    pub limit_value: Option<f64>,
    pub started_at: Option<String>,
    pub resets_at: Option<String>,
    pub calibration_offset: f64,
    pub unit: String,
    pub source: String,
    pub observed_at: Option<String>,
    pub updated_at: String,
}

/// One credit balance row as projected for provider usage. Distinct from the
/// stored provider credit row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreditBalance {
    pub account_id: String,
    pub balance_kind: String,
    pub amount: f64,
    pub unit: String,
    pub source: String,
    pub observed_at: Option<String>,
    pub updated_at: String,
}

/// Official-usage sync metadata as projected for provider usage. Distinct from
/// the stored `provider_usage_sync_state` row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct UsageSyncState {
    pub account_id: String,
    pub last_success_at: Option<String>,
    pub last_attempt_at: Option<String>,
    pub next_eligible_at: Option<String>,
    pub failure_streak: i64,
    pub last_expedited_at: Option<String>,
}

/// Registry usage availability. Wire values stay snake_case.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[schemars(rename_all = "snake_case")]
pub enum UsageAvailability {
    Available,
    Unavailable,
    LocalState,
}

/// Browser runtime mode. Wire values match V2 lowercase `native` / `remote` /
/// `unsupported`. Distinct from `browser::BrowserMode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
#[schemars(rename_all = "lowercase")]
pub enum BrowserMode {
    Native,
    Remote,
    Unsupported,
}

/// Managed-browser launch target. Wire values match V2 snake_case.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[schemars(rename_all = "snake_case")]
pub enum BrowserTarget {
    GoogleSignup,
    GoogleLogin,
    GithubSignup,
    GithubLogin,
    Invite,
    Console,
}

/// GET `/browser/capabilities` body. Read-only; no `expectedRevision`.
/// Distinct from `browser::BrowserCapabilities`. `reason` is required `T | null`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct BrowserCapabilities {
    pub mode: BrowserMode,
    pub reason: Option<String>,
    pub revision: u64,
    pub process_generation: u64,
}

/// POST `/accounts/{id}/usage/refresh` result. Nested `usage` is the V3
/// window projection; `revision` is captured after the shared coordinator
/// returns and is not advanced by official calibration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct UsageRefresh {
    pub usage: UsageWindow,
    pub source: String,
    pub last_success_at: String,
    pub next_allowed_at: String,
    pub revision: u64,
    pub process_generation: u64,
}

/// POST `/accounts/{id}/browser` body. CAS tokens and `target` are required.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct BrowserOpenRequest {
    #[serde(flatten)]
    pub expectation: MutationExpectation,
    pub target: BrowserTarget,
}

/// POST `/accounts/{id}/browser` result. Distinct from `browser::BrowserOpenResult`.
///
/// Native mode always emits `sessionToken: null`. Remote mode emits only the
/// opaque dashboard-bound display token —never a worker URL or control token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct BrowserOpen {
    pub mode: BrowserMode,
    pub session_token: Option<String>,
    pub revision: u64,
    pub process_generation: u64,
}

/// POST `/accounts/{id}/usage/refresh` body. CAS tokens are required;
/// unknown fields, including Key material, are rejected.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct UsageRefreshUpdate {
    #[serde(flatten)]
    pub expectation: MutationExpectation,
}

/// Typed 429 body for `POST /accounts/{id}/usage/refresh`.
///
/// This preserves the stable V3 error fields while making the endpoint-only
/// absolute retry time an explicit append-only contract instead of injecting
/// an undeclared property into `V3Error`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct UsageRefreshThrottleError {
    pub code: String,
    pub message: String,
    pub current_revision: Option<u64>,
    pub process_generation: Option<u64>,
    pub next_allowed_at: String,
}

/// Paid Ollama Cloud billing profile. Wire values are exactly `pro`, `max`,
/// and `team`. Absence of a stored row (migrated/unconfigured) and non-Ollama
/// accounts serialize the account field as `null`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[schemars(rename_all = "snake_case")]
pub enum OllamaBillingTier {
    Pro,
    Max,
    Team,
}

impl From<crate::provider::OllamaBillingTier> for OllamaBillingTier {
    fn from(tier: crate::provider::OllamaBillingTier) -> Self {
        match tier {
            crate::provider::OllamaBillingTier::Pro => Self::Pro,
            crate::provider::OllamaBillingTier::Max => Self::Max,
            crate::provider::OllamaBillingTier::Team => Self::Team,
        }
    }
}

impl From<OllamaBillingTier> for crate::provider::OllamaBillingTier {
    fn from(tier: OllamaBillingTier) -> Self {
        match tier {
            OllamaBillingTier::Pro => Self::Pro,
            OllamaBillingTier::Max => Self::Max,
            OllamaBillingTier::Team => Self::Team,
        }
    }
}

/// Secret-free singleton configuration for the local CPA external integration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct CpaIntegration {
    pub configured: bool,
    pub base_url: String,
    pub base_url_read_only: bool,
    pub management_key_configured: bool,
    pub inference_key_configured: bool,
    pub enabled: bool,
    pub account_id: Option<String>,
    pub model_count: usize,
    pub models_refreshed_at: Option<String>,
    pub runtime_supported: bool,
    pub runtime_owned: bool,
    pub runtime_running: bool,
    pub installed_version: Option<String>,
    pub latest_version: Option<String>,
    pub update_available: bool,
    pub current_operation: Option<String>,
    pub runtime_unavailable_reason: Option<String>,
    pub revision: u64,
    pub process_generation: u64,
}

/// PUT CPA configuration. Secret fields are write-only; omission preserves
/// their existing encrypted values.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct CpaIntegrationUpdate {
    #[serde(flatten)]
    pub expectation: MutationExpectation,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub management_key: Option<String>,
    #[serde(default)]
    pub inference_key: Option<String>,
    #[serde(default)]
    pub enabled: Option<bool>,
}

/// Operational connection test. Optional write-only values allow testing a
/// first-time configuration without persisting it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct CpaTestRequest {
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub management_key: Option<String>,
    #[serde(default)]
    pub inference_key: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct CpaConnectionReport {
    pub reachable: bool,
    pub management_ready: bool,
    pub inference_ready: bool,
    pub version: Option<String>,
    pub commit: Option<String>,
    pub build_date: Option<String>,
    pub model_count: usize,
    pub management_error: Option<String>,
    pub inference_error: Option<String>,
    pub revision: u64,
    pub process_generation: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct CpaModel {
    pub id: String,
    pub owned_by: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct CpaModels {
    pub models: Vec<CpaModel>,
    pub source_url: Option<String>,
    pub refreshed_at: Option<String>,
    pub revision: u64,
    pub process_generation: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct CpaAccount {
    pub name: String,
    pub auth_index: Option<String>,
    pub provider: String,
    pub label: Option<String>,
    pub status: Option<String>,
    pub status_message: Option<String>,
    pub disabled: bool,
    pub unavailable: bool,
    pub runtime_only: bool,
    pub mutable: bool,
    pub email: Option<String>,
    pub quota: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct CpaAccounts {
    pub accounts: Vec<CpaAccount>,
    pub version: String,
    pub revision: u64,
    pub process_generation: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct CpaAccountStatusUpdate {
    #[serde(flatten)]
    pub expectation: MutationExpectation,
    pub name: String,
    pub auth_index: String,
    pub disabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct CpaAccountDelete {
    #[serde(flatten)]
    pub expectation: MutationExpectation,
    pub name: String,
    pub auth_index: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct CpaQuotaReset {
    #[serde(flatten)]
    pub expectation: MutationExpectation,
    pub name: String,
    pub auth_index: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[schemars(rename_all = "snake_case")]
pub enum CpaOAuthProvider {
    Codex,
    Anthropic,
    Antigravity,
    Kimi,
    Xai,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct CpaOAuthStartRequest {
    #[serde(flatten)]
    pub expectation: MutationExpectation,
    pub provider: CpaOAuthProvider,
    /// Browser is the backward-compatible default; device is managed Codex only.
    #[serde(default)]
    pub method: CpaOAuthMethod,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CpaOAuthMethod {
    #[default]
    Browser,
    Device,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct CpaOAuthStart {
    pub provider: CpaOAuthProvider,
    pub state: String,
    pub url: String,
    pub flow: String,
    pub user_code: Option<String>,
    pub expires_in: Option<u64>,
    pub revision: u64,
    pub process_generation: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct CpaOAuthStatus {
    pub state: String,
    pub status: String,
    pub error: Option<String>,
    pub revision: u64,
    pub process_generation: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct CpaOAuthSessionDelete {
    #[serde(flatten)]
    pub expectation: MutationExpectation,
    pub state: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct CpaCliImportSource {
    pub provider: CpaOAuthProvider,
    pub source: String,
    pub supported: bool,
    /// File presence only. Credentials are read exclusively on explicit import.
    pub available: bool,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct CpaCliImports {
    pub sources: Vec<CpaCliImportSource>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct CpaCliImportRequest {
    #[serde(flatten)]
    pub expectation: MutationExpectation,
    pub provider: CpaOAuthProvider,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum CpaCliImportOutcome {
    Imported,
    AlreadyImported,
    Unconfirmed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct CpaCliImportResult {
    pub provider: CpaOAuthProvider,
    pub name: String,
    pub outcome: CpaCliImportOutcome,
    pub revision: u64,
    pub process_generation: u64,
}

/// Secret-free CPA runtime snapshot. `supported` is true only on an
/// installed desktop Host on Windows x64, macOS, or Linux x64.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct CpaRuntime {
    pub supported: bool,
    pub unavailable_reason: Option<String>,
    pub installed: bool,
    pub running: bool,
    pub owned: bool,
    pub current_version: Option<String>,
    pub previous_version: Option<String>,
    pub asset_sha256: Option<String>,
    pub port: Option<u16>,
    pub base_url: Option<String>,
    pub phase: CpaRuntimePhase,
    pub error: Option<String>,
    pub latest_version: Option<String>,
    pub update_available: bool,
    pub current_operation: Option<String>,
    pub revision: u64,
    pub process_generation: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
#[schemars(rename_all = "lowercase")]
pub enum CpaRuntimePhase {
    Idle,
    Checking,
    Downloading,
    Installing,
    Starting,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct CpaRuntimeCheck {
    pub current_version: Option<String>,
    pub latest_version: String,
    pub update_available: bool,
    pub release_url: String,
    pub revision: u64,
    pub process_generation: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct CpaRuntimeInstall {
    #[serde(flatten)]
    pub expectation: MutationExpectation,
    #[serde(default)]
    pub expected_version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct CpaRuntimeLogs {
    pub stdout: String,
    pub stderr: String,
    pub revision: u64,
    pub process_generation: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct CpaRuntimeKey {
    pub fingerprint: String,
    pub hint: String,
    pub protected: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct CpaRuntimeKeys {
    pub keys: Vec<CpaRuntimeKey>,
    pub revision: u64,
    pub process_generation: u64,
}

/// One-time CPA client inference secret. The value is never persisted in this
/// DTO after the creating response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct CpaRuntimeKeyCreated {
    pub fingerprint: String,
    pub hint: String,
    pub secret: String,
    pub revision: u64,
    pub process_generation: u64,
}

/// Auth kind owned by a Provider definition. Independent of protocol.
/// Nullable on the wire because builtin rows leave the field empty.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[schemars(rename_all = "snake_case")]
pub enum ProviderDefinitionAuthKind {
    Bearer,
    #[serde(rename = "x-api-key")]
    XApiKey,
    #[serde(rename = "api-key")]
    ApiKey,
    None,
}

impl From<ocg_domain::dynamic::DynamicAuthKind> for ProviderDefinitionAuthKind {
    fn from(value: ocg_domain::dynamic::DynamicAuthKind) -> Self {
        match value {
            ocg_domain::dynamic::DynamicAuthKind::Bearer => Self::Bearer,
            ocg_domain::dynamic::DynamicAuthKind::XApiKey => Self::XApiKey,
            ocg_domain::dynamic::DynamicAuthKind::ApiKey => Self::ApiKey,
            ocg_domain::dynamic::DynamicAuthKind::None => Self::None,
        }
    }
}

impl From<ProviderDefinitionAuthKind> for ocg_domain::dynamic::DynamicAuthKind {
    fn from(value: ProviderDefinitionAuthKind) -> Self {
        match value {
            ProviderDefinitionAuthKind::Bearer => Self::Bearer,
            ProviderDefinitionAuthKind::XApiKey => Self::XApiKey,
            ProviderDefinitionAuthKind::ApiKey => Self::ApiKey,
            ProviderDefinitionAuthKind::None => Self::None,
        }
    }
}

/// One public-to-upstream mapping owned by a Provider definition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProviderModelUpstreamOverride {
    pub protocol: AccountUpstreamProtocol,
    pub endpoint_url: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProviderDefinitionModel {
    pub public_model: String,
    pub upstream_model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream_override: Option<ProviderModelUpstreamOverride>,
}

/// Secret-free Provider definition. Surfaces both builtin and dynamic rows
/// through the same wire shape. `endpointUrl` / `upstreamProtocol` /
/// `authKind` are nullable because builtin rows leave them empty (the sealed
/// adapter drives those at call time).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProviderDefinition {
    /// Row provenance: `builtin` | `preset` | `custom`.
    pub origin: ProviderOrigin,
    /// Whether the dashboard may PATCH this row. Always `false` for builtin.
    pub editable: bool,
    /// Whether the dashboard may DELETE this row. Always `false` for builtin.
    pub deletable: bool,
    /// Plan/api offering label persisted alongside the row.
    pub offering: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preset_id: Option<String>,
    pub id: String,
    pub name: String,
    /// Nullable: builtin rows leave the field empty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint_url: Option<String>,
    /// Nullable: builtin rows leave the field empty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream_protocol: Option<AccountUpstreamProtocol>,
    /// Nullable: builtin rows leave the field empty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_kind: Option<ProviderDefinitionAuthKind>,
    /// Empty for builtin rows; builtin catalogs feed the contract projection.
    pub models: Vec<ProviderDefinitionModel>,
    pub created_at: String,
    pub updated_at: String,
    pub revision: u64,
    pub process_generation: u64,
}

/// POST `/providers` body. Creates the definition and mappings.
/// A Key on keyed auth creates the first account in the same write (Accounts add).
/// Keyed auth may omit `key` to save the definition only; add Keys on Accounts.
/// No-auth always creates the singleton account.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProviderDefinitionCreate {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preset_id: Option<String>,
    #[serde(flatten)]
    pub expectation: MutationExpectation,
    pub name: String,
    pub endpoint_url: String,
    pub upstream_protocol: AccountUpstreamProtocol,
    pub auth_kind: ProviderDefinitionAuthKind,
    pub models: Vec<ProviderDefinitionModel>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

/// PATCH `/providers/{providerId}` body. Full replacement of mutable config.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProviderDefinitionUpdate {
    /// Omitted/null preserves provenance; an empty string clears it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preset_id: Option<String>,
    #[serde(flatten)]
    pub expectation: MutationExpectation,
    pub name: String,
    pub endpoint_url: String,
    pub upstream_protocol: AccountUpstreamProtocol,
    pub auth_kind: ProviderDefinitionAuthKind,
    pub models: Vec<ProviderDefinitionModel>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
}

/// Mutation result for a Provider definition write.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProviderDefinitionMutation {
    pub provider: ProviderDefinition,
    pub revision: u64,
    pub process_generation: u64,
}

/// POST `/providers/models/discover` body. Operational probe; no CAS bump.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProviderDefinitionDiscoverRequest {
    pub endpoint_url: String,
    pub upstream_protocol: AccountUpstreamProtocol,
    pub auth_kind: ProviderDefinitionAuthKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
}

/// Discovery result. Never includes the submitted Key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProviderDefinitionDiscoverResponse {
    pub models: Vec<String>,
    pub truncated: bool,
    pub revision: u64,
    pub process_generation: u64,
}

/// POST `/providers/test` body. Operational probe; no CAS bump.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProviderDefinitionTestRequest {
    pub endpoint_url: String,
    pub upstream_protocol: AccountUpstreamProtocol,
    pub auth_kind: ProviderDefinitionAuthKind,
    pub public_model: String,
    pub upstream_model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
}

/// Model-test result. Never includes the submitted Key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProviderDefinitionTestResponse {
    pub ok: bool,
    pub error: Option<String>,
    pub revision: u64,
    pub process_generation: u64,
}

/// Deterministic JSON Schema catalog for the checked-in V3 contract.
///
/// Response types are generated with the serialize contract so `Option` fields
/// stay required `T | null`. Request types use the deserialize contract so
/// optional fields may be omitted. Adding a DTO later must append a `$defs`
/// entry without renaming existing definitions.
pub fn contract_schema() -> Value {
    let mut serialize = SchemaSettings::draft2020_12()
        .for_serialize()
        .into_generator();
    include_type::<ControlRevision>(&mut serialize);
    include_type::<MutationAck>(&mut serialize);
    include_type::<PricingRevision>(&mut serialize);
    include_type::<V3Error>(&mut serialize);
    include_type::<ConnectionInfo>(&mut serialize);
    include_type::<ConnectionSubKey>(&mut serialize);
    include_type::<Settings>(&mut serialize);
    include_type::<ProxySupportedModel>(&mut serialize);
    include_type::<Account>(&mut serialize);
    include_type::<AccountList>(&mut serialize);
    include_type::<AccountMutation>(&mut serialize);
    include_type::<AccountExport>(&mut serialize);
    include_type::<AccountImportPreview>(&mut serialize);
    include_type::<AccountImportPreviewItem>(&mut serialize);
    include_type::<AccountImportDisposition>(&mut serialize);
    include_type::<AccountImportResult>(&mut serialize);
    include_type::<AccountCustomConfig>(&mut serialize);
    include_type::<AccountModelCapability>(&mut serialize);
    include_type::<AccountModelTestResponse>(&mut serialize);
    include_type::<ProviderCatalog>(&mut serialize);
    include_type::<ProviderCatalogEntry>(&mut serialize);
    include_type::<ProviderCatalogFormField>(&mut serialize);
    include_type::<ProviderModelCapability>(&mut serialize);
    include_type::<ZenFreeSettings>(&mut serialize);
    include_type::<ZenFreeModels>(&mut serialize);
    include_type::<ZenFreeModel>(&mut serialize);
    include_type::<ProviderContracts>(&mut serialize);
    include_type::<ProviderContractGroup>(&mut serialize);
    include_type::<CustomEndpointContract>(&mut serialize);
    include_type::<ProviderAccountChoice>(&mut serialize);
    include_type::<EffectiveCatalog>(&mut serialize);
    include_type::<EffectiveModelContract>(&mut serialize);
    include_type::<EffectiveModelProtocols>(&mut serialize);
    include_type::<EffectiveProtocolEvidence>(&mut serialize);
    include_type::<CapabilitySummary>(&mut serialize);
    include_type::<CardCapabilitySummary>(&mut serialize);
    include_type::<ProtocolProbeResult>(&mut serialize);
    include_type::<ProtocolProbeResponse>(&mut serialize);
    include_type::<PricingSnapshot>(&mut serialize);
    include_type::<PricingLimits>(&mut serialize);
    include_type::<PricingModel>(&mut serialize);
    include_type::<PricingAdjustment>(&mut serialize);
    include_type::<PricingTimeWindow>(&mut serialize);
    include_type::<PricingRefresh>(&mut serialize);
    include_type::<PricingRefreshStatus>(&mut serialize);
    include_type::<PricingMultiplierChange>(&mut serialize);
    include_type::<ProviderPricing>(&mut serialize);
    include_type::<ProviderPricingSnapshot>(&mut serialize);
    include_type::<ProviderPricingValue>(&mut serialize);
    include_type::<ProviderPricingRefresh>(&mut serialize);
    include_type::<ProviderModels>(&mut serialize);
    include_type::<PricingAvailability>(&mut serialize);
    include_type::<GatewayStatus>(&mut serialize);
    include_type::<ApplicationModels>(&mut serialize);
    include_type::<DashboardSummary>(&mut serialize);
    include_type::<DailyModelTokens>(&mut serialize);
    include_type::<DailyTokensByModel>(&mut serialize);
    include_type::<GatewayLog>(&mut serialize);
    include_type::<GatewayLogs>(&mut serialize);
    include_type::<ForwardLog>(&mut serialize);
    include_type::<ForwardLogSummary>(&mut serialize);
    include_type::<ForwardLogs>(&mut serialize);
    include_type::<ForwardLogClientKey>(&mut serialize);
    include_type::<ForwardLogKeys>(&mut serialize);
    include_type::<ForwardLogModels>(&mut serialize);
    include_type::<UsageWindow>(&mut serialize);
    include_type::<UsageMutation>(&mut serialize);
    include_type::<ProviderUsage>(&mut serialize);
    include_type::<QuotaWindow>(&mut serialize);
    include_type::<CreditBalance>(&mut serialize);
    include_type::<UsageSyncState>(&mut serialize);
    include_type::<UsageAvailability>(&mut serialize);
    include_type::<AuthStatus>(&mut serialize);
    include_type::<ProxyTestResponse>(&mut serialize);
    include_type::<CustomModelDiscoveryResponse>(&mut serialize);
    include_type::<BrowserMode>(&mut serialize);
    include_type::<BrowserCapabilities>(&mut serialize);
    include_type::<BrowserOpen>(&mut serialize);
    include_type::<UpdateCheck>(&mut serialize);
    include_type::<DesktopUpdate>(&mut serialize);
    include_type::<UsageRefresh>(&mut serialize);
    include_type::<UsageRefreshThrottleError>(&mut serialize);
    include_type::<OllamaBillingTier>(&mut serialize);
    include_type::<CpaIntegration>(&mut serialize);
    include_type::<CpaConnectionReport>(&mut serialize);
    include_type::<CpaModel>(&mut serialize);
    include_type::<CpaModels>(&mut serialize);
    include_type::<CpaAccounts>(&mut serialize);
    include_type::<CpaAccount>(&mut serialize);
    include_type::<CpaOAuthProvider>(&mut serialize);
    include_type::<CpaOAuthStart>(&mut serialize);
    include_type::<CpaOAuthStatus>(&mut serialize);
    include_type::<CpaCliImportSource>(&mut serialize);
    include_type::<CpaCliImports>(&mut serialize);
    include_type::<CpaCliImportResult>(&mut serialize);
    include_type::<CpaRuntime>(&mut serialize);
    include_type::<CpaRuntimePhase>(&mut serialize);
    include_type::<CpaRuntimeCheck>(&mut serialize);
    include_type::<CpaRuntimeLogs>(&mut serialize);
    include_type::<CpaRuntimeKey>(&mut serialize);
    include_type::<CpaRuntimeKeys>(&mut serialize);
    include_type::<CpaRuntimeKeyCreated>(&mut serialize);
    include_type::<ProviderDefinitionAuthKind>(&mut serialize);
    include_type::<ProviderDefinitionModel>(&mut serialize);
    include_type::<ProviderDefinition>(&mut serialize);
    include_type::<ProviderDefinitionMutation>(&mut serialize);
    include_type::<ProviderDefinitionDiscoverResponse>(&mut serialize);
    include_type::<ProviderDefinitionTestResponse>(&mut serialize);
    include_type::<super::platforms::PlatformAccounts>(&mut serialize);
    let mut defs = serialize.take_definitions(true);

    let mut deserialize = SchemaSettings::draft2020_12().into_generator();
    include_type::<super::platforms::PlatformCreate>(&mut deserialize);
    include_type::<super::platforms::PlatformUpdate>(&mut deserialize);
    include_type::<super::platforms::PlatformLinkWrite>(&mut deserialize);
    include_type::<super::platforms::PlatformRefresh>(&mut deserialize);
    include_type::<MutationExpectation>(&mut deserialize);
    include_type::<SettingsUpdate>(&mut deserialize);
    include_type::<KeyCreate>(&mut deserialize);
    include_type::<KeyUpdate>(&mut deserialize);
    include_type::<AccountCreate>(&mut deserialize);
    include_type::<AccountExportRequest>(&mut deserialize);
    include_type::<AccountImportPreviewRequest>(&mut deserialize);
    include_type::<AccountImportRequest>(&mut deserialize);
    include_type::<AccountManagedCreate>(&mut deserialize);
    include_type::<AccountModelTestRequest>(&mut deserialize);
    include_type::<AccountUpdate>(&mut deserialize);
    include_type::<AccountOrder>(&mut deserialize);
    include_type::<AccountSetupUpdate>(&mut deserialize);
    include_type::<AccountManagedKeyVerify>(&mut deserialize);
    include_type::<AccountCustomConfigUpdate>(&mut deserialize);
    include_type::<AccountCustomConfigWrite>(&mut deserialize);
    include_type::<AccountModelCapabilitiesUpdate>(&mut deserialize);
    include_type::<AccountModelCapabilityWrite>(&mut deserialize);
    include_type::<AccountVerify>(&mut deserialize);
    include_type::<ZenFreeSettingsUpdate>(&mut deserialize);
    include_type::<ModelProtocolOverridesUpdate>(&mut deserialize);
    include_type::<ProtocolProbeRequest>(&mut deserialize);
    include_type::<PricingRefreshUpdate>(&mut deserialize);
    include_type::<PricingRefreshPolicy>(&mut deserialize);
    include_type::<ProviderPricingRefreshUpdate>(&mut deserialize);
    include_type::<PricingMultipliersUpdate>(&mut deserialize);
    include_type::<PricingMultiplierWrite>(&mut deserialize);
    include_type::<GatewayLogQuery>(&mut deserialize);
    include_type::<ForwardLogQuery>(&mut deserialize);
    include_type::<DailyTokensQuery>(&mut deserialize);
    include_type::<AccountUsageUpdate>(&mut deserialize);
    include_type::<AuthRegister>(&mut deserialize);
    include_type::<AuthLogin>(&mut deserialize);
    include_type::<AuthLogout>(&mut deserialize);
    include_type::<ProxyTestRequest>(&mut deserialize);
    include_type::<CustomModelDiscoveryRequest>(&mut deserialize);
    include_type::<BrowserOpenRequest>(&mut deserialize);
    include_type::<BrowserTarget>(&mut deserialize);
    include_type::<InstallUpdate>(&mut deserialize);
    include_type::<UsageRefreshUpdate>(&mut deserialize);
    include_type::<ProviderModelsRefreshUpdate>(&mut deserialize);
    include_type::<CpaIntegrationUpdate>(&mut deserialize);
    include_type::<CpaTestRequest>(&mut deserialize);
    include_type::<CpaAccountStatusUpdate>(&mut deserialize);
    include_type::<CpaAccountDelete>(&mut deserialize);
    include_type::<CpaQuotaReset>(&mut deserialize);
    include_type::<CpaOAuthStartRequest>(&mut deserialize);
    include_type::<CpaOAuthSessionDelete>(&mut deserialize);
    include_type::<CpaCliImportRequest>(&mut deserialize);
    include_type::<CpaRuntimeInstall>(&mut deserialize);
    include_type::<ProviderDefinitionCreate>(&mut deserialize);
    include_type::<ProviderDefinitionUpdate>(&mut deserialize);
    include_type::<ProviderDefinitionDiscoverRequest>(&mut deserialize);
    include_type::<ProviderDefinitionTestRequest>(&mut deserialize);
    for (name, schema) in deserialize.take_definitions(true) {
        defs.entry(name).or_insert(schema);
    }

    for name in CATALOG_TYPE_NAMES {
        if !defs.contains_key(*name) {
            panic!("dashboard v3 schema catalog is missing $defs/{name}");
        }
    }

    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "title": "DashboardApiV3",
        "$comment": "Extensible Dashboard V3 contract catalog. Add new $defs for later DTOs; do not rename or reshape existing definitions. ConnectionInfo is the only plaintext Key DTO.",
        "anyOf": catalog_refs(&defs),
        "$defs": defs })
}

/// Pretty-printed catalog JSON with a trailing newline.
pub fn contract_schema_pretty() -> String {
    let mut encoded = serde_json::to_string_pretty(&contract_schema())
        .expect("dashboard v3 schema should serialize");
    if !encoded.ends_with('\n') {
        encoded.push('\n');
    }
    encoded
}

fn include_type<T: JsonSchema>(generator: &mut SchemaGenerator) {
    generator.subschema_for::<T>();
}

fn catalog_refs(defs: &Map<String, Value>) -> Vec<Value> {
    CATALOG_TYPE_NAMES
        .iter()
        .filter(|name| defs.contains_key(**name))
        .map(|name| json!({ "$ref": format!("#/$defs/{name}") }))
        .collect()
}

#[cfg(test)]
mod tests;

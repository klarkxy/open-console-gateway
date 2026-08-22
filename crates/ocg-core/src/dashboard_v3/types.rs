//! Shared Dashboard V3 wire types and the JSON Schema catalog.
//!
//! Response objects always serialize nullable fields as `T | null` (never omitted).
//! Request optional fields may be omitted; `expectedRevision` is required on every
//! control-plane mutation. Pricing mutations also require `expectedPricingRevision`.
//! Plaintext keys must not appear on `Settings` or provider/Zen/contract DTOs —
//! `ConnectionInfo` is the only secret-bearing V3 DTO. Protocol path/switch tokens
//! stay `chat_completions`, `responses`, and `messages`. Pricing wire DTOs are
//! distinct from `kernel::pricing` and from stored provider pricing blobs.

use schemars::JsonSchema;
use schemars::generate::{SchemaGenerator, SchemaSettings};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::models::{AccountSetupStep as ModelAccountSetupStep, AccountType as ModelAccountType};
use crate::provider::{
    ConnectionVerificationStatus as ProviderVerificationStatus, CredentialKind, QuotaScope,
    UpstreamAuthScheme, UpstreamProtocolKind,
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
    "AccountAcknowledgement",
    "AccountCreate",
    "AccountManagedCreate",
    "AccountUpdate",
    "AccountOrder",
    "AccountSetupUpdate",
    "AccountCustomConfigUpdate",
    "AccountCustomConfigWrite",
    "AccountModelCapabilitiesUpdate",
    "AccountModelCapabilityWrite",
    "AccountAcknowledgementCreate",
    "AccountAcknowledgementWrite",
    "ProviderCatalog",
    "ProviderCatalogEntry",
    "ProviderCatalogFormField",
    "ProviderCatalogRiskNotice",
    "ProviderModelCapability",
    "ZenFreeSettings",
    "ZenFreeSettingsUpdate",
    "ZenFreeModels",
    "ZenFreeModel",
    "ProviderContracts",
    "ProviderContractGroup",
    "CustomEndpointContract",
    "ProviderOfferingChoice",
    "ProviderAccountChoice",
    "ProtocolSwitches",
    "EffectiveCatalog",
    "EffectiveModelContract",
    "EffectiveModelProtocols",
    "EffectiveProtocolEvidence",
    "CapabilitySummary",
    "CardCapabilitySummary",
    "ProtocolSwitchUpdate",
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
}

/// Lightweight connection-center payload. The only V3 DTO allowed to carry
/// plaintext primary and sub Key values.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConnectionInfo {
    pub gateway_port: u16,
    pub client_root_url: String,
    pub upstream_base_url: String,
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
    pub upstream_base_url: String,
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
    pub upstream_base_url: Option<String>,
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
    pub offering_id: String,
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
    pub acknowledgements: Vec<AccountAcknowledgement>,
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

/// Nested Custom HTTP destination as returned on an account.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccountCustomConfig {
    pub account_id: String,
    pub base_url: String,
    pub upstream_protocol: AccountUpstreamProtocol,
    pub auth_scheme: AccountAuthScheme,
    pub created_at: String,
    pub updated_at: String,
}

/// One declared Custom model capability as returned on an account.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccountModelCapability {
    pub account_id: String,
    pub model_id: String,
    pub protocol: AccountUpstreamProtocol,
    pub verified_at: Option<String>,
    pub source: String,
}

/// One persisted Plan risk acknowledgement as returned on an account.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccountAcknowledgement {
    pub account_id: String,
    pub acknowledgement_id: String,
    pub version: String,
    pub content_hash: String,
    pub accepted_at: String,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offering_id: Option<String>,
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
    pub acknowledgements: Vec<AccountAcknowledgementWrite>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub model_capabilities: Vec<AccountModelCapabilityWrite>,
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

/// PUT `/accounts/{id}/custom-config` body. Protocol and auth scheme are
/// immutable after create; the handler enforces that.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccountCustomConfigUpdate {
    #[serde(flatten)]
    pub expectation: MutationExpectation,
    pub base_url: String,
    pub upstream_protocol: AccountUpstreamProtocol,
    pub auth_scheme: AccountAuthScheme,
}

/// Create-time Custom destination (no timestamps). Nested under `AccountCreate`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccountCustomConfigWrite {
    pub base_url: String,
    pub upstream_protocol: AccountUpstreamProtocol,
    pub auth_scheme: AccountAuthScheme,
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

/// One declared Custom model capability on create or replace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccountModelCapabilityWrite {
    pub model_id: String,
    pub protocol: AccountUpstreamProtocol,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

/// POST `/accounts/{id}/acknowledgements` body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccountAcknowledgementCreate {
    #[serde(flatten)]
    pub expectation: MutationExpectation,
    pub acknowledgement_id: String,
    pub version: String,
}

/// Create-time Plan risk acknowledgement. Nested under `AccountCreate`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccountAcknowledgementWrite {
    pub acknowledgement_id: String,
    pub version: String,
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
}

impl From<UpstreamAuthScheme> for AccountAuthScheme {
    fn from(value: UpstreamAuthScheme) -> Self {
        match value {
            UpstreamAuthScheme::Bearer => Self::Bearer,
            UpstreamAuthScheme::XApiKey => Self::XApiKey,
        }
    }
}

impl From<AccountAuthScheme> for UpstreamAuthScheme {
    fn from(value: AccountAuthScheme) -> Self {
        match value {
            AccountAuthScheme::Bearer => Self::Bearer,
            AccountAuthScheme::XApiKey => Self::XApiKey,
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

/// One Provider Registry offering as a wire catalog row. Identity strings are
/// data copied from the static registry; this DTO does not define them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProviderCatalogEntry {
    pub provider_id: String,
    pub offering_id: String,
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
    pub risk_notice: Option<ProviderCatalogRiskNotice>,
    pub model_aliases: Vec<String>,
}

/// One create-form field advertised by a catalog offering.
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

/// One Go protocol-table capability. GOAT/SCNet must not reuse these rows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProviderModelCapability {
    pub model_id: String,
    pub provider_id: String,
    pub offering_id: String,
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

/// One built-in provider scope. SCNet is a single group with three offerings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProviderContractGroup {
    pub scope_kind: ContractScopeKind,
    pub scope_id: String,
    pub provider_id: String,
    pub offerings: Vec<ProviderOfferingChoice>,
    pub catalog: EffectiveCatalog,
    pub models: Vec<EffectiveModelContract>,
    pub protocols: ProtocolSwitches,
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
    pub protocols: ProtocolSwitches,
    pub pricing: CapabilitySummary,
    pub usage: CapabilitySummary,
    pub card: CardCapabilitySummary,
    pub catalog_routable: bool,
    pub production_inference: bool,
    pub disabled_reasons: Vec<String>,
    /// Display revision for this endpoint, distinct from the top-level CAS token.
    pub revision: u64,
}

/// One offering under a provider scope, with current account cards.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProviderOfferingChoice {
    pub offering_id: String,
    pub display_name: String,
    pub routable: bool,
    pub accounts: Vec<ProviderAccountChoice>,
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

/// Upstream protocol enablement. JSON keys stay the protocol tokens.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[schemars(rename_all = "snake_case", deny_unknown_fields)]
pub struct ProtocolSwitches {
    pub chat_completions: bool,
    pub responses: bool,
    pub messages: bool,
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

/// PUT one protocol switch. CAS tokens and `enabled` are required. The path
/// protocol token is not repeated here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProtocolSwitchUpdate {
    #[serde(flatten)]
    pub expectation: MutationExpectation,
    pub enabled: bool,
}

/// POST protocol-probe body. `accountId` may be omitted; `protocols` is the
/// required explicit probe set. Handlers validate membership and uniqueness.
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
    pub account_id: String,
    pub provider_id: String,
    pub model_id: String,
    pub results: Vec<ProtocolProbeResult>,
    pub contract: Option<EffectiveModelContract>,
    pub revision: u64,
    pub process_generation: u64,
    pub pricing_revision: String,
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

/// PUT pricing-multipliers body. CAS tokens, `expectedPricingRevision`, and
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
    pub offering_id: String,
    pub availability: PricingAvailability,
    pub snapshot: Option<PricingSnapshot>,
    pub revision: u64,
    pub process_generation: u64,
    pub pricing_revision: String,
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
    include_type::<AccountCustomConfig>(&mut serialize);
    include_type::<AccountModelCapability>(&mut serialize);
    include_type::<AccountAcknowledgement>(&mut serialize);
    include_type::<ProviderCatalog>(&mut serialize);
    include_type::<ProviderCatalogEntry>(&mut serialize);
    include_type::<ProviderCatalogFormField>(&mut serialize);
    include_type::<ProviderCatalogRiskNotice>(&mut serialize);
    include_type::<ProviderModelCapability>(&mut serialize);
    include_type::<ZenFreeSettings>(&mut serialize);
    include_type::<ZenFreeModels>(&mut serialize);
    include_type::<ZenFreeModel>(&mut serialize);
    include_type::<ProviderContracts>(&mut serialize);
    include_type::<ProviderContractGroup>(&mut serialize);
    include_type::<CustomEndpointContract>(&mut serialize);
    include_type::<ProviderOfferingChoice>(&mut serialize);
    include_type::<ProviderAccountChoice>(&mut serialize);
    include_type::<ProtocolSwitches>(&mut serialize);
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
    include_type::<PricingAvailability>(&mut serialize);
    let mut defs = serialize.take_definitions(true);

    let mut deserialize = SchemaSettings::draft2020_12().into_generator();
    include_type::<MutationExpectation>(&mut deserialize);
    include_type::<SettingsUpdate>(&mut deserialize);
    include_type::<KeyCreate>(&mut deserialize);
    include_type::<KeyUpdate>(&mut deserialize);
    include_type::<AccountCreate>(&mut deserialize);
    include_type::<AccountManagedCreate>(&mut deserialize);
    include_type::<AccountUpdate>(&mut deserialize);
    include_type::<AccountOrder>(&mut deserialize);
    include_type::<AccountSetupUpdate>(&mut deserialize);
    include_type::<AccountCustomConfigUpdate>(&mut deserialize);
    include_type::<AccountCustomConfigWrite>(&mut deserialize);
    include_type::<AccountModelCapabilitiesUpdate>(&mut deserialize);
    include_type::<AccountModelCapabilityWrite>(&mut deserialize);
    include_type::<AccountAcknowledgementCreate>(&mut deserialize);
    include_type::<AccountAcknowledgementWrite>(&mut deserialize);
    include_type::<ZenFreeSettingsUpdate>(&mut deserialize);
    include_type::<ProtocolSwitchUpdate>(&mut deserialize);
    include_type::<ProtocolProbeRequest>(&mut deserialize);
    include_type::<PricingRefreshUpdate>(&mut deserialize);
    include_type::<PricingRefreshPolicy>(&mut deserialize);
    include_type::<PricingMultipliersUpdate>(&mut deserialize);
    include_type::<PricingMultiplierWrite>(&mut deserialize);
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
        "$defs": defs,
    })
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
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn wire_fields_are_camel_case() {
        let revision = ControlRevision {
            revision: 7,
            process_generation: 9,
            pricing_revision: "seed".into(),
        };
        assert_eq!(
            serde_json::to_value(&revision).unwrap(),
            json!({
                "revision": 7,
                "processGeneration": 9,
                "pricingRevision": "seed",
            })
        );

        let parsed: MutationExpectation = serde_json::from_value(json!({
            "expectedRevision": 3,
            "processGeneration": 9,
        }))
        .unwrap();
        assert_eq!(parsed.expected_revision, 3);
        assert_eq!(parsed.process_generation, 9);
        assert!(
            serde_json::from_value::<MutationExpectation>(json!({ "expected_revision": 3 }))
                .is_err()
        );
        assert!(
            serde_json::from_value::<MutationExpectation>(json!({
                "expectedRevision": 3,
                "processGeneration": 7,
                "value": "must-not-be-accepted"
            }))
            .is_err()
        );
    }

    #[test]
    fn error_envelope_always_emits_nullable_fields() {
        let error = V3Error::missing_expected_revision();
        let value = serde_json::to_value(&error).unwrap();
        assert_eq!(value["code"], "missingExpectedRevision");
        assert_eq!(value["currentRevision"], Value::Null);
        assert_eq!(value["processGeneration"], Value::Null);
        assert!(!value.as_object().unwrap().contains_key("current_revision"));
    }

    #[test]
    fn schema_catalog_is_extensible_and_names_kernel_types() {
        let schema = contract_schema();
        let defs = schema["$defs"].as_object().expect("catalog $defs");
        for name in CATALOG_TYPE_NAMES {
            assert!(defs.contains_key(*name), "missing {name}");
        }
        let required_error = defs["V3Error"]["required"]
            .as_array()
            .expect("V3Error.required");
        for field in ["code", "message", "currentRevision", "processGeneration"] {
            assert!(
                required_error.iter().any(|value| value == field),
                "{field} must stay required so responses emit T|null"
            );
        }
        let expectation_required = defs["MutationExpectation"]["required"]
            .as_array()
            .expect("MutationExpectation.required");
        assert_eq!(
            expectation_required,
            &vec![json!("expectedRevision"), json!("processGeneration")]
        );
        assert_eq!(schema["title"], "DashboardApiV3");
    }

    #[test]
    fn connection_info_is_the_only_secret_bearing_dto() {
        let connection = ConnectionInfo {
            gateway_port: 9042,
            client_root_url: String::new(),
            upstream_base_url: "https://opencode.ai/zen/go".into(),
            primary_key: "ocg-secret".into(),
            sub_keys: vec![ConnectionSubKey {
                id: "sub".into(),
                name: "Laptop".into(),
                enabled: true,
                value: "ocg-sub-secret".into(),
            }],
            revision: 3,
            process_generation: 9,
        };
        let value = serde_json::to_value(&connection).unwrap();
        assert_eq!(value["primaryKey"], "ocg-secret");
        assert_eq!(value["subKeys"][0]["value"], "ocg-sub-secret");
        assert!(value.get("gatewayKey").is_none());
        assert!(value.get("key").is_none());
        assert!(value.get("gateway_key").is_none());
        assert_eq!(value["processGeneration"], 9);
    }

    #[test]
    fn settings_wire_omits_key_fields_and_nulls_unsupported_host_toggles() {
        let settings = Settings {
            revision: 4,
            process_generation: 9,
            gateway_port: 9042,
            upstream_base_url: "https://opencode.ai/zen/go".into(),
            proxy_mode: ProxyMode::Auto,
            proxy_url: String::new(),
            proxy_list_direction: ProxyListDirection::Whitelist,
            proxy_list_models: Vec::new(),
            proxy_supported_models: vec![ProxySupportedModel {
                id: "gpt-5.6-luna".into(),
                preferred_protocol: "responses".into(),
                zen_free: false,
            }],
            opencode_invite_url: String::new(),
            client_root_url: String::new(),
            client_root_url_from_env: false,
            auto_start: None,
            auto_start_supported: false,
            show_dock_icon: None,
            dock_visibility_supported: false,
            connect_timeout_secs: 30,
            non_stream_timeout_secs: 900,
            stream_idle_timeout_secs: 300,
            routing_mode: RoutingMode::StrictPriority,
            conversation_sticky: false,
        };
        let value = serde_json::to_value(&settings).unwrap();
        let object = value.as_object().unwrap();
        for forbidden in [
            "key",
            "gatewayKey",
            "gateway_key",
            "primaryKey",
            "primary_key",
        ] {
            assert!(
                !object.contains_key(forbidden),
                "settings must not expose {forbidden}"
            );
        }
        assert_eq!(value["autoStart"], Value::Null);
        assert_eq!(value["showDockIcon"], Value::Null);
        assert_eq!(value["autoStartSupported"], false);
        assert_eq!(value["proxyMode"], "auto");
        assert_eq!(value["routingMode"], "strict-priority");
        assert_eq!(
            value["proxySupportedModels"][0]["preferredProtocol"],
            "responses"
        );
    }

    #[test]
    fn settings_update_requires_cas_and_allows_omitted_patch_fields() {
        let parsed: SettingsUpdate = serde_json::from_value(json!({
            "expectedRevision": 7,
            "processGeneration": 9,
            "connectTimeoutSecs": 12
        }))
        .unwrap();
        assert_eq!(parsed.expectation.expected_revision, 7);
        assert_eq!(parsed.expectation.process_generation, 9);
        assert_eq!(parsed.connect_timeout_secs, Some(12));
        assert!(parsed.proxy_mode.is_none());
        assert!(
            serde_json::from_value::<SettingsUpdate>(json!({
                "expectedRevision": 7,
                "processGeneration": 9,
                "gatewayKey": "ocg-secret"
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<SettingsUpdate>(json!({
                "expected_revision": 7,
                "processGeneration": 9
            }))
            .is_err()
        );
    }

    #[test]
    fn key_mutation_dtos_require_cas_and_reject_secret_fields() {
        let created: KeyCreate = serde_json::from_value(json!({
            "expectedRevision": 4,
            "processGeneration": 9,
            "name": "Laptop"
        }))
        .unwrap();
        assert_eq!(created.expectation.expected_revision, 4);
        assert_eq!(created.expectation.process_generation, 9);
        assert_eq!(created.name, "Laptop");
        assert!(
            serde_json::from_value::<KeyCreate>(json!({
                "expectedRevision": 4,
                "processGeneration": 9,
                "name": "Laptop",
                "value": "ocg-secret"
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<KeyCreate>(json!({
                "expectedRevision": 4,
                "processGeneration": 9,
                "name": "Laptop",
                "key": "ocg-secret"
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<KeyCreate>(json!({
                "expectedRevision": 4,
                "processGeneration": 9,
                "name": "Laptop",
                "gatewayKey": "ocg-secret"
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<KeyCreate>(json!({
                "expectedRevision": 4,
                "processGeneration": 9,
                "name": "Laptop",
                "primaryKey": "ocg-secret"
            }))
            .is_err()
        );

        let patched: KeyUpdate = serde_json::from_value(json!({
            "expectedRevision": 5,
            "processGeneration": 9,
            "enabled": false
        }))
        .unwrap();
        assert_eq!(patched.expectation.expected_revision, 5);
        assert_eq!(patched.enabled, Some(false));
        assert!(patched.name.is_none());
        assert!(
            serde_json::from_value::<KeyUpdate>(json!({
                "expectedRevision": 5,
                "processGeneration": 9,
                "value": "ocg-secret"
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<KeyUpdate>(json!({
                "expected_revision": 5,
                "processGeneration": 9
            }))
            .is_err()
        );
    }

    #[test]
    fn mutation_ack_serializes_without_credential_fields() {
        let ack = MutationAck {
            revision: 8,
            process_generation: 9,
        };
        let value = serde_json::to_value(&ack).unwrap();
        let object = value.as_object().unwrap();
        assert_eq!(object.get("revision"), Some(&json!(8)));
        assert_eq!(object.get("processGeneration"), Some(&json!(9)));
        for forbidden in [
            "key",
            "gatewayKey",
            "gateway_key",
            "primaryKey",
            "primary_key",
            "value",
            "name",
            "id",
        ] {
            assert!(
                !object.contains_key(forbidden),
                "MutationAck must not expose {forbidden}"
            );
        }
    }

    #[test]
    fn account_response_emits_nulls_and_never_carries_secrets() {
        let account = Account {
            id: "acct-1".into(),
            provider_id: "opencode".into(),
            offering_id: "go".into(),
            credential_kind: AccountCredentialKind::ApiKey,
            quota_scope: AccountQuotaScope::Key,
            name: "main".into(),
            username: None,
            enabled: true,
            account_type: AccountType::Key,
            setup_step: AccountSetupStep::Ready,
            purchase_date: "2026-01-31".into(),
            expires_on: "2026-02-28".into(),
            cooldown_until: None,
            cooldown_generic_until: None,
            cooldown_5h_until: None,
            cooldown_week_until: None,
            cooldown_month_until: None,
            cooldown_free_until: None,
            last_error: None,
            auth_error: None,
            notes: None,
            usage_sync_last_success_at: None,
            usage_sync_next_allowed_at: None,
            created_at: "2026-01-31T00:00:00Z".into(),
            updated_at: "2026-01-31T00:00:00Z".into(),
            revision: 4,
            process_generation: 9,
            verification_status: AccountVerificationStatus::NotRequired,
            connection_verified_at: None,
            verification_error: None,
            plan_routable: true,
            custom_config: None,
            model_capabilities: Vec::new(),
            acknowledgements: Vec::new(),
        };
        let value = serde_json::to_value(&account).unwrap();
        let object = value.as_object().unwrap();
        for forbidden in [
            "key",
            "password",
            "passwordCipher",
            "keyCipher",
            "gatewayKey",
            "gateway_key",
            "primaryKey",
            "referralCode",
            "referral_code",
        ] {
            assert!(
                !object.contains_key(forbidden),
                "Account must not expose {forbidden}"
            );
        }
        assert_eq!(value["username"], Value::Null);
        assert_eq!(value["notes"], Value::Null);
        assert_eq!(value["customConfig"], Value::Null);
        assert_eq!(value["cooldown5hUntil"], Value::Null);
        assert_eq!(value["verificationStatus"], "not_required");
        assert_eq!(value["quotaScope"], "key");
        assert_eq!(value["processGeneration"], 9);

        let listed = AccountList {
            accounts: vec![account.clone()],
            revision: 4,
            process_generation: 9,
        };
        let listed_value = serde_json::to_value(&listed).unwrap();
        assert_eq!(listed_value["accounts"][0]["id"], "acct-1");
        assert_eq!(listed_value["revision"], 4);

        let deleted = AccountMutation {
            account: None,
            revision: 5,
            process_generation: 9,
        };
        let deleted_value = serde_json::to_value(&deleted).unwrap();
        assert_eq!(deleted_value["account"], Value::Null);
        assert_eq!(deleted_value["revision"], 5);
    }

    #[test]
    fn account_requests_accept_write_only_secrets_and_reject_unknown_fields() {
        let created: AccountCreate = serde_json::from_value(json!({
            "expectedRevision": 3,
            "processGeneration": 9,
            "name": "Go",
            "key": "sk-secret",
            "password": "pw-secret",
            "referralCode": "ref-1"
        }))
        .unwrap();
        assert_eq!(created.expectation.expected_revision, 3);
        assert_eq!(created.key, "sk-secret");
        assert_eq!(created.password.as_deref(), Some("pw-secret"));
        assert_eq!(created.referral_code.as_deref(), Some("ref-1"));
        assert!(created.provider_id.is_none());
        assert!(created.custom_config.is_none());
        assert!(
            serde_json::from_value::<AccountCreate>(json!({
                "expectedRevision": 3,
                "processGeneration": 9,
                "name": "Go",
                "key": "sk-secret",
                "gatewayKey": "ocg-secret"
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<AccountUpdate>(json!({
                "expectedRevision": 3,
                "processGeneration": 9,
                "providerId": "opencode"
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<AccountManagedCreate>(json!({
                "expectedRevision": 3,
                "processGeneration": 9,
                "name": "draft",
                "key": "sk-secret"
            }))
            .is_err()
        );

        let patched: AccountUpdate = serde_json::from_value(json!({
            "expectedRevision": 4,
            "processGeneration": 9,
            "enabled": false
        }))
        .unwrap();
        assert_eq!(patched.enabled, Some(false));
        assert!(patched.key.is_none());
        assert!(patched.name.is_none());
    }

    #[test]
    fn service_unavailable_error_emits_stable_code_and_cas_tokens() {
        let error = V3Error::service_unavailable("browser stop failed", 11, 9);
        let value = serde_json::to_value(&error).unwrap();
        assert_eq!(value["code"], ERROR_SERVICE_UNAVAILABLE);
        assert_eq!(value["code"], "serviceUnavailable");
        assert_eq!(value["message"], "browser stop failed");
        assert_eq!(value["currentRevision"], 11);
        assert_eq!(value["processGeneration"], 9);

        let schema = contract_schema();
        let defs = schema["$defs"].as_object().expect("catalog $defs");
        assert!(defs.contains_key("V3Error"));
        assert_eq!(
            defs["V3Error"]["properties"]["code"]["type"], "string",
            "new error codes must not reshape the V3Error catalog definition"
        );
    }

    #[test]
    fn not_implemented_error_emits_stable_code_and_cas_tokens() {
        let error = V3Error::not_implemented("protocol probes are not available", 11, 9);
        let value = serde_json::to_value(&error).unwrap();
        assert_eq!(value["code"], ERROR_NOT_IMPLEMENTED);
        assert_eq!(value["code"], "notImplemented");
        assert_eq!(value["message"], "protocol probes are not available");
        assert_eq!(value["currentRevision"], 11);
        assert_eq!(value["processGeneration"], 9);

        let schema = contract_schema();
        let defs = schema["$defs"].as_object().expect("catalog $defs");
        assert_eq!(
            defs["V3Error"]["properties"]["code"]["type"], "string",
            "open string error codes must not reshape the V3Error catalog definition"
        );
    }

    const ACCOUNTS_CATALOG_PREFIX: &[&str] = &[
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
        "AccountAcknowledgement",
        "AccountCreate",
        "AccountManagedCreate",
        "AccountUpdate",
        "AccountOrder",
        "AccountSetupUpdate",
        "AccountCustomConfigUpdate",
        "AccountCustomConfigWrite",
        "AccountModelCapabilitiesUpdate",
        "AccountModelCapabilityWrite",
        "AccountAcknowledgementCreate",
        "AccountAcknowledgementWrite",
    ];

    fn json_field_names(value: &Value) -> Vec<&str> {
        match value {
            Value::Object(map) => {
                let mut names: Vec<&str> = map.keys().map(String::as_str).collect();
                names.extend(map.values().flat_map(json_field_names));
                names
            }
            Value::Array(items) => items.iter().flat_map(json_field_names).collect(),
            _ => Vec::new(),
        }
    }

    fn json_string_values(value: &Value) -> Vec<&str> {
        match value {
            Value::String(text) => vec![text.as_str()],
            Value::Array(items) => items.iter().flat_map(json_string_values).collect(),
            Value::Object(map) => map.values().flat_map(json_string_values).collect(),
            _ => Vec::new(),
        }
    }

    fn assert_secret_free(value: &Value) {
        for name in json_field_names(value) {
            assert!(
                !matches!(
                    name,
                    "key"
                        | "password"
                        | "passwordCipher"
                        | "keyCipher"
                        | "gatewayKey"
                        | "gateway_key"
                        | "primaryKey"
                        | "primary_key"
                        | "referralCode"
                        | "referral_code"
                        | "cipher"
                        | "apiKey"
                        | "api_key"
                        | "token"
                        | "secret"
                ),
                "provider DTO leaked field {name}: {value}"
            );
        }
        for text in json_string_values(value) {
            assert!(
                !text.contains("sk-secret")
                    && !text.contains("ocg-secret")
                    && !text.contains("pw-secret")
                    && !text.contains("user:pass@"),
                "provider DTO leaked secret sample {text}: {value}"
            );
        }
    }

    fn chat_evidence() -> EffectiveProtocolEvidence {
        EffectiveProtocolEvidence {
            protocol: AccountUpstreamProtocol::ChatCompletions,
            available: true,
            enabled: true,
            source: ContractEvidenceSource::Static,
            verified_at: None,
            observed_at: None,
            last_probe_result: None,
            last_probe_at: None,
            last_probe_error: None,
        }
    }

    fn sample_catalog_entry() -> ProviderCatalogEntry {
        ProviderCatalogEntry {
            provider_id: "scnet".into(),
            offering_id: "token-plan-basic".into(),
            display_name: "SCNet Token Plan Basic".into(),
            display_family: "SCNet".into(),
            credential_kind: AccountCredentialKind::ApiKey,
            quota_scope: AccountQuotaScope::Key,
            singleton: false,
            creation_availability: "available".into(),
            creation_unavailable_reason: None,
            verification_policy: "required".into(),
            verification_runtime_availability: "unavailable".into(),
            routable: false,
            managed_registration: false,
            pricing_availability: "unavailable".into(),
            usage_availability: "unavailable".into(),
            manual_usage_calibration: false,
            quota_unit: "credits".into(),
            model_source: "official_token_plan_usable_models_2026_08_21".into(),
            key_prefix: Some("sk-tp-".into()),
            auth_schemes: vec![AccountAuthScheme::Bearer],
            upstream_protocols: vec![
                AccountUpstreamProtocol::ChatCompletions,
                AccountUpstreamProtocol::Messages,
            ],
            form_fields: vec![ProviderCatalogFormField {
                id: "name".into(),
                kind: "text".into(),
                required: true,
                immutable_after_create: false,
            }],
            risk_notice: Some(ProviderCatalogRiskNotice {
                acknowledgement_id: "scnet-token-plan-restrictions".into(),
                version: "2026-08-21".into(),
                source_url: "https://www.scnet.cn/docs".into(),
                body: "interactive use only".into(),
                content_hash: "abc".into(),
            }),
            model_aliases: Vec::new(),
        }
    }

    fn sample_contracts() -> ProviderContracts {
        let goat = ProviderContractGroup {
            scope_kind: ContractScopeKind::Provider,
            scope_id: "command-code".into(),
            provider_id: "command-code".into(),
            offerings: vec![ProviderOfferingChoice {
                offering_id: "goat".into(),
                display_name: "Command Code GOAT".into(),
                routable: false,
                accounts: vec![ProviderAccountChoice {
                    id: "goat-1".into(),
                    name: "draft".into(),
                    enabled: false,
                    verification_status: AccountVerificationStatus::Pending,
                }],
            }],
            catalog: EffectiveCatalog {
                source: "static".into(),
                source_url: String::new(),
                refreshed_at: None,
                models: Vec::new(),
                refresh_supported: false,
            },
            models: vec![EffectiveModelContract {
                model_id: "deepseek-v4-flash".into(),
                preferred_protocol: AccountUpstreamProtocol::ChatCompletions,
                protocols: EffectiveModelProtocols {
                    chat_completions: Some(chat_evidence()),
                    responses: None,
                    messages: Some(EffectiveProtocolEvidence {
                        protocol: AccountUpstreamProtocol::Messages,
                        available: false,
                        enabled: false,
                        source: ContractEvidenceSource::Preset,
                        verified_at: None,
                        observed_at: None,
                        last_probe_result: None,
                        last_probe_at: None,
                        last_probe_error: None,
                    }),
                },
                routable: false,
                disabled_reasons: vec!["offering is not routable".into()],
            }],
            protocols: ProtocolSwitches {
                chat_completions: true,
                responses: false,
                messages: true,
            },
            pricing: CapabilitySummary {
                availability: "unavailable".into(),
            },
            usage: CapabilitySummary {
                availability: "unavailable".into(),
            },
            card: CardCapabilitySummary {
                fetch_zen_models: false,
                discover_models: false,
                protocol_probe: false,
                catalog_refresh: false,
            },
            catalog_routable: false,
            production_inference: false,
            disabled_reasons: vec!["offering is not routable".into()],
            revision: 2,
        };
        let scnet = ProviderContractGroup {
            scope_kind: ContractScopeKind::Provider,
            scope_id: "scnet".into(),
            provider_id: "scnet".into(),
            offerings: vec![
                ProviderOfferingChoice {
                    offering_id: "token-plan-basic".into(),
                    display_name: "SCNet Token Plan Basic".into(),
                    routable: false,
                    accounts: Vec::new(),
                },
                ProviderOfferingChoice {
                    offering_id: "token-plan-standard".into(),
                    display_name: "SCNet Token Plan Standard".into(),
                    routable: false,
                    accounts: Vec::new(),
                },
                ProviderOfferingChoice {
                    offering_id: "token-plan-premium".into(),
                    display_name: "SCNet Token Plan Premium".into(),
                    routable: false,
                    accounts: Vec::new(),
                },
            ],
            catalog: EffectiveCatalog {
                source: "static".into(),
                source_url: String::new(),
                refreshed_at: None,
                models: Vec::new(),
                refresh_supported: false,
            },
            models: Vec::new(),
            protocols: ProtocolSwitches {
                chat_completions: true,
                responses: false,
                messages: true,
            },
            pricing: CapabilitySummary {
                availability: "unavailable".into(),
            },
            usage: CapabilitySummary {
                availability: "unavailable".into(),
            },
            card: CardCapabilitySummary {
                fetch_zen_models: false,
                discover_models: false,
                protocol_probe: false,
                catalog_refresh: false,
            },
            catalog_routable: false,
            production_inference: false,
            disabled_reasons: vec!["offering is not routable".into()],
            revision: 3,
        };
        ProviderContracts {
            providers: vec![goat, scnet],
            custom_endpoints: vec![CustomEndpointContract {
                scope_kind: ContractScopeKind::CustomEndpoint,
                scope_id: "custom-1".into(),
                provider_id: "custom".into(),
                account: ProviderAccountChoice {
                    id: "custom-1".into(),
                    name: "lan".into(),
                    enabled: false,
                    verification_status: AccountVerificationStatus::Pending,
                },
                catalog: EffectiveCatalog {
                    source: "account_declared".into(),
                    source_url: String::new(),
                    refreshed_at: None,
                    models: vec!["local-model".into()],
                    refresh_supported: true,
                },
                models: Vec::new(),
                protocols: ProtocolSwitches {
                    chat_completions: true,
                    responses: true,
                    messages: true,
                },
                pricing: CapabilitySummary {
                    availability: "unpriced".into(),
                },
                usage: CapabilitySummary {
                    availability: "unavailable".into(),
                },
                card: CardCapabilitySummary {
                    fetch_zen_models: false,
                    discover_models: true,
                    protocol_probe: true,
                    catalog_refresh: true,
                },
                catalog_routable: true,
                production_inference: false,
                disabled_reasons: vec!["account is not enabled".into()],
                revision: 8,
            }],
            revision: 11,
            process_generation: 9,
            pricing_revision: "seed".into(),
        }
    }

    #[test]
    fn catalog_type_names_keep_accounts_prefix_and_register_provider_dtos() {
        assert_eq!(
            &CATALOG_TYPE_NAMES[..ACCOUNTS_CATALOG_PREFIX.len()],
            ACCOUNTS_CATALOG_PREFIX
        );
        for name in [
            "ProviderCatalog",
            "ProviderCatalogEntry",
            "ProviderCatalogFormField",
            "ProviderCatalogRiskNotice",
            "ProviderModelCapability",
            "ZenFreeSettings",
            "ZenFreeSettingsUpdate",
            "ZenFreeModels",
            "ZenFreeModel",
            "ProviderContracts",
            "ProviderContractGroup",
            "CustomEndpointContract",
            "ProviderOfferingChoice",
            "ProviderAccountChoice",
            "ProtocolSwitches",
            "EffectiveCatalog",
            "EffectiveModelContract",
            "EffectiveModelProtocols",
            "EffectiveProtocolEvidence",
            "CapabilitySummary",
            "CardCapabilitySummary",
            "ProtocolSwitchUpdate",
            "ProtocolProbeRequest",
            "ProtocolProbeResult",
            "ProtocolProbeResponse",
        ] {
            assert!(
                CATALOG_TYPE_NAMES.contains(&name),
                "CATALOG_TYPE_NAMES missing {name}"
            );
        }

        let schema = contract_schema();
        let defs = schema["$defs"].as_object().expect("catalog $defs");
        for name in CATALOG_TYPE_NAMES {
            assert!(defs.contains_key(*name), "schema missing {name}");
        }
        let any_of = schema["anyOf"].as_array().expect("catalog anyOf");
        for (index, name) in ACCOUNTS_CATALOG_PREFIX.iter().enumerate() {
            assert_eq!(
                any_of[index]["$ref"],
                format!("#/$defs/{name}"),
                "anyOf prefix drifted at {index}"
            );
        }
        assert_eq!(
            defs["Account"]["required"],
            json!([
                "id",
                "providerId",
                "offeringId",
                "credentialKind",
                "quotaScope",
                "name",
                "username",
                "enabled",
                "accountType",
                "setupStep",
                "purchaseDate",
                "expiresOn",
                "cooldownUntil",
                "cooldownGenericUntil",
                "cooldown5hUntil",
                "cooldownWeekUntil",
                "cooldownMonthUntil",
                "cooldownFreeUntil",
                "lastError",
                "authError",
                "notes",
                "usageSyncLastSuccessAt",
                "usageSyncNextAllowedAt",
                "createdAt",
                "updatedAt",
                "revision",
                "processGeneration",
                "verificationStatus",
                "connectionVerifiedAt",
                "verificationError",
                "planRoutable",
                "customConfig",
                "modelCapabilities",
                "acknowledgements"
            ])
        );
    }

    #[test]
    fn provider_catalog_emits_nulls_camel_case_and_no_secrets() {
        let catalog = ProviderCatalog {
            entries: vec![sample_catalog_entry()],
            revision: 11,
            process_generation: 9,
            pricing_revision: "seed".into(),
        };
        let value = serde_json::to_value(&catalog).unwrap();
        assert_eq!(value["processGeneration"], 9);
        assert_eq!(value["pricingRevision"], "seed");
        assert_eq!(value["entries"][0]["providerId"], "scnet");
        assert_eq!(
            value["entries"][0]["creationUnavailableReason"],
            Value::Null
        );
        assert_eq!(value["entries"][0]["keyPrefix"], "sk-tp-");
        assert_eq!(value["entries"][0]["verificationPolicy"], "required");
        assert_eq!(
            value["entries"][0]["verificationRuntimeAvailability"],
            "unavailable"
        );
        assert_eq!(value["entries"][0]["routable"], false);
        assert_eq!(
            value["entries"][0]["upstreamProtocols"][0],
            "chat_completions"
        );
        assert!(value.get("modelCapabilities").is_none());
        assert!(value.get("creation_unavailable_reason").is_none());
        assert_secret_free(&value);

        let schema = contract_schema();
        let required = schema["$defs"]["ProviderCatalogEntry"]["required"]
            .as_array()
            .unwrap();
        for field in ["creationUnavailableReason", "keyPrefix", "riskNotice"] {
            assert!(
                required.iter().any(|value| value == field),
                "{field} must stay required so responses emit T|null"
            );
        }
    }

    #[test]
    fn zen_and_contract_responses_keep_cas_distinct_from_display_revisions() {
        let settings = ZenFreeSettings {
            account_id: "zen-free".into(),
            enabled: true,
            revision: 12,
            process_generation: 9,
            pricing_revision: "seed".into(),
        };
        let settings_value = serde_json::to_value(&settings).unwrap();
        assert_eq!(
            settings_value,
            json!({
                "accountId": "zen-free",
                "enabled": true,
                "revision": 12,
                "processGeneration": 9,
                "pricingRevision": "seed"
            })
        );
        assert_secret_free(&settings_value);

        let models = ZenFreeModels {
            account_id: "zen-free".into(),
            models: vec![ZenFreeModel {
                model_id: "hy3-free".into(),
                alias: "hy3".into(),
            }],
            refreshed_at: None,
            source_url: "https://opencode.ai/zen/v1/models".into(),
            revision: 12,
            process_generation: 9,
            pricing_revision: "seed".into(),
        };
        let models_value = serde_json::to_value(&models).unwrap();
        assert_eq!(models_value["refreshedAt"], Value::Null);
        assert_eq!(models_value["accountId"], "zen-free");
        assert_eq!(models_value["pricingRevision"], "seed");
        assert_secret_free(&models_value);

        let contracts = sample_contracts();
        let contracts_value = serde_json::to_value(&contracts).unwrap();
        assert_eq!(contracts_value["revision"], 11);
        assert_eq!(contracts_value["processGeneration"], 9);
        assert_eq!(contracts_value["providers"][0]["revision"], 2);
        assert_eq!(contracts_value["providers"][0]["scopeKind"], "provider");
        assert_eq!(
            contracts_value["customEndpoints"][0]["scopeKind"],
            "custom_endpoint"
        );
        assert_eq!(
            contracts_value["providers"][1]["offerings"]
                .as_array()
                .unwrap()
                .len(),
            3
        );
        assert_eq!(
            contracts_value["providers"][0]["protocols"],
            json!({
                "chat_completions": true,
                "responses": false,
                "messages": true
            })
        );
        assert!(
            contracts_value["providers"][0]["protocols"]
                .as_object()
                .unwrap()
                .get("chatCompletions")
                .is_none()
        );
        assert_eq!(
            contracts_value["providers"][0]["models"][0]["protocols"]["responses"],
            Value::Null
        );
        assert_eq!(
            contracts_value["providers"][0]["models"][0]["protocols"]["chat_completions"]["lastProbeError"],
            Value::Null
        );
        assert!(
            contracts_value["providers"][0]
                .as_object()
                .unwrap()
                .get("scopeRevision")
                .is_none()
        );
        assert_eq!(
            contracts_value["providers"][0]["offerings"][0]["accounts"][0]["verificationStatus"],
            "pending"
        );
        assert_eq!(
            contracts_value["providers"][0]["card"]["protocolProbe"],
            false
        );
        assert_eq!(
            contracts_value["customEndpoints"][0]["catalog"]["refreshedAt"],
            Value::Null
        );
        assert!(contracts_value.get("expectedRevision").is_none());
        assert_secret_free(&contracts_value);

        let schema = contract_schema();
        let switches = schema["$defs"]["ProtocolSwitches"]["properties"]
            .as_object()
            .unwrap();
        assert!(switches.contains_key("chat_completions"));
        assert!(switches.contains_key("responses"));
        assert!(switches.contains_key("messages"));
        assert!(!switches.contains_key("chatCompletions"));
        let protocol_required = schema["$defs"]["EffectiveModelProtocols"]["required"]
            .as_array()
            .unwrap();
        for field in ["chat_completions", "responses", "messages"] {
            assert!(
                protocol_required.iter().any(|value| value == field),
                "{field} must stay required so missing protocols emit null"
            );
        }
        assert!(schema["$defs"]["ProviderContractGroup"]["properties"]["revision"].is_object());
        assert!(
            schema["$defs"]["ProviderContractGroup"]["properties"]
                .as_object()
                .unwrap()
                .get("scopeRevision")
                .is_none()
        );
    }

    #[test]
    fn provider_mutation_requests_require_cas_allow_omission_and_reject_unknown_fields() {
        let zen: ZenFreeSettingsUpdate = serde_json::from_value(json!({
            "expectedRevision": 4,
            "processGeneration": 9,
            "enabled": true
        }))
        .unwrap();
        assert_eq!(zen.expectation.expected_revision, 4);
        assert!(zen.enabled);
        assert!(
            serde_json::from_value::<ZenFreeSettingsUpdate>(json!({
                "expectedRevision": 4,
                "processGeneration": 9,
                "enabled": true,
                "key": "sk-secret"
            }))
            .is_err()
        );

        let switch: ProtocolSwitchUpdate = serde_json::from_value(json!({
            "expectedRevision": 11,
            "processGeneration": 9,
            "enabled": false
        }))
        .unwrap();
        assert!(!switch.enabled);
        assert!(
            serde_json::from_value::<ProtocolSwitchUpdate>(json!({
                "expectedRevision": 11,
                "processGeneration": 9,
                "enabled": false,
                "scopeRevision": 2
            }))
            .is_err()
        );

        assert!(
            serde_json::from_value::<ProtocolProbeRequest>(json!({
                "expectedRevision": 11,
                "processGeneration": 9,
                "modelId": "gpt-5.6-luna"
            }))
            .is_err()
        );
        let with_account: ProtocolProbeRequest = serde_json::from_value(json!({
            "expectedRevision": 11,
            "processGeneration": 9,
            "accountId": "acct-1",
            "modelId": "gpt-5.6-luna",
            "protocols": ["chat_completions", "responses"]
        }))
        .unwrap();
        assert_eq!(with_account.account_id.as_deref(), Some("acct-1"));
        assert_eq!(
            with_account.protocols,
            vec![
                AccountUpstreamProtocol::ChatCompletions,
                AccountUpstreamProtocol::Responses
            ]
        );
        assert!(
            serde_json::from_value::<ProtocolProbeRequest>(json!({
                "expected_revision": 11,
                "processGeneration": 9,
                "modelId": "gpt-5.6-luna"
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<ProtocolProbeRequest>(json!({
                "expectedRevision": 11,
                "processGeneration": 9,
                "modelId": "gpt-5.6-luna",
                "key": "sk-secret"
            }))
            .is_err()
        );

        let probe = ProtocolProbeResponse {
            account_id: "acct-1".into(),
            provider_id: "custom".into(),
            model_id: "gpt-5.6-luna".into(),
            results: vec![ProtocolProbeResult {
                protocol: AccountUpstreamProtocol::ChatCompletions,
                success: false,
                skipped: true,
                error: None,
            }],
            contract: None,
            revision: 12,
            process_generation: 9,
            pricing_revision: "seed".into(),
        };
        let probe_value = serde_json::to_value(&probe).unwrap();
        assert_eq!(probe_value["accountId"], "acct-1");
        assert_eq!(probe_value["providerId"], "custom");
        assert_eq!(probe_value["contract"], Value::Null);
        assert_eq!(probe_value["pricingRevision"], "seed");
        assert_eq!(probe_value["results"][0]["protocol"], "chat_completions");
        assert_eq!(probe_value["results"][0]["error"], Value::Null);
        assert_secret_free(&probe_value);

        let schema = contract_schema();
        let probe_request = &schema["$defs"]["ProtocolProbeRequest"];
        assert_eq!(probe_request["additionalProperties"], false);
        let required = probe_request["required"].as_array().unwrap();
        assert!(required.iter().any(|value| value == "expectedRevision"));
        assert!(required.iter().any(|value| value == "processGeneration"));
        assert!(required.iter().any(|value| value == "modelId"));
        assert!(!required.iter().any(|value| value == "accountId"));
        assert!(required.iter().any(|value| value == "protocols"));
        let response_required = schema["$defs"]["ProtocolProbeResponse"]["required"]
            .as_array()
            .unwrap();
        assert!(response_required.iter().any(|value| value == "accountId"));
        assert!(response_required.iter().any(|value| value == "providerId"));
        assert!(response_required.iter().any(|value| value == "contract"));
        assert!(
            response_required
                .iter()
                .any(|value| value == "pricingRevision")
        );
    }

    const PROVIDER_CATALOG_TYPES: &[&str] = &[
        "ProviderCatalog",
        "ProviderCatalogEntry",
        "ProviderCatalogFormField",
        "ProviderCatalogRiskNotice",
        "ProviderModelCapability",
        "ZenFreeSettings",
        "ZenFreeSettingsUpdate",
        "ZenFreeModels",
        "ZenFreeModel",
        "ProviderContracts",
        "ProviderContractGroup",
        "CustomEndpointContract",
        "ProviderOfferingChoice",
        "ProviderAccountChoice",
        "ProtocolSwitches",
        "EffectiveCatalog",
        "EffectiveModelContract",
        "EffectiveModelProtocols",
        "EffectiveProtocolEvidence",
        "CapabilitySummary",
        "CardCapabilitySummary",
        "ProtocolSwitchUpdate",
        "ProtocolProbeRequest",
        "ProtocolProbeResult",
        "ProtocolProbeResponse",
    ];

    const PRICING_CATALOG_TYPES: &[&str] = &[
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
    ];

    fn sample_pricing_snapshot() -> PricingSnapshot {
        PricingSnapshot {
            revision: 11,
            process_generation: 9,
            pricing_revision: "seed-2026-08-16-local-v4".into(),
            activated_at: "2026-08-16T12:00:00.000Z".into(),
            document_updated_at: "2026-08-16T00:00:00.000Z".into(),
            source_url: "https://opencode.ai/docs/go/".into(),
            content_hash: "embedded-opencode-go-2026-08-16".into(),
            adjustment_policy_version: "local-v4".into(),
            limits: PricingLimits {
                window_5h: 12.0,
                window_week: 30.0,
                window_month: 60.0,
            },
            models: vec![PricingModel {
                model_id: "grok-4.5".into(),
                display_name: "Grok 4.5".into(),
                input: 2.0,
                output: 6.0,
                cache_read: 0.3,
                cache_write: None,
                usage: 15.0,
                quota_multiplier: 4.0,
                min_input_tokens: None,
                max_input_tokens: None,
                time_window: PricingTimeWindow::Always,
                adjustments: vec![PricingAdjustment {
                    label: "highspeed alias".into(),
                    multiplier: 2.0,
                    applies_to: "input,output".into(),
                }],
            }],
        }
    }

    #[test]
    fn catalog_type_names_append_pricing_dtos_after_the_provider_prefix() {
        let prefix_len = ACCOUNTS_CATALOG_PREFIX.len() + PROVIDER_CATALOG_TYPES.len();
        assert_eq!(
            &CATALOG_TYPE_NAMES[..ACCOUNTS_CATALOG_PREFIX.len()],
            ACCOUNTS_CATALOG_PREFIX
        );
        assert_eq!(
            &CATALOG_TYPE_NAMES[ACCOUNTS_CATALOG_PREFIX.len()..prefix_len],
            PROVIDER_CATALOG_TYPES
        );
        assert_eq!(&CATALOG_TYPE_NAMES[prefix_len..], PRICING_CATALOG_TYPES);
        assert_eq!(
            CATALOG_TYPE_NAMES.len(),
            prefix_len + PRICING_CATALOG_TYPES.len()
        );
    }

    #[test]
    fn pricing_snapshot_uses_cas_revision_and_emits_required_nulls() {
        let snapshot = sample_pricing_snapshot();
        let value = serde_json::to_value(&snapshot).unwrap();
        assert_eq!(value["revision"], 11);
        assert_eq!(value["processGeneration"], 9);
        assert_eq!(value["pricingRevision"], "seed-2026-08-16-local-v4");
        assert_eq!(value["limits"]["window5h"], 12.0);
        assert_eq!(value["limits"]["windowWeek"], 30.0);
        assert_eq!(value["limits"]["windowMonth"], 60.0);
        assert_eq!(value["models"][0]["modelId"], "grok-4.5");
        assert_eq!(value["models"][0]["cacheWrite"], Value::Null);
        assert_eq!(value["models"][0]["minInputTokens"], Value::Null);
        assert_eq!(value["models"][0]["maxInputTokens"], Value::Null);
        assert_eq!(value["models"][0]["timeWindow"], "always");
        assert_eq!(
            value["models"][0]["adjustments"][0]["appliesTo"],
            "input,output"
        );
        assert!(value.get("snapshotJson").is_none());
        assert!(
            value
                .as_object()
                .unwrap()
                .get("revision")
                .unwrap()
                .is_number()
        );
        assert!(
            serde_json::from_value::<PricingSnapshot>(json!({
                "revision": "seed-2026-08-16-local-v4",
                "processGeneration": 9,
                "pricingRevision": "seed-2026-08-16-local-v4",
                "activatedAt": "2026-08-16T12:00:00.000Z",
                "documentUpdatedAt": "2026-08-16T00:00:00.000Z",
                "sourceUrl": "https://opencode.ai/docs/go/",
                "contentHash": "embedded-opencode-go-2026-08-16",
                "adjustmentPolicyVersion": "local-v4",
                "limits": { "window5h": 12.0, "windowWeek": 30.0, "windowMonth": 60.0 },
                "models": []
            }))
            .is_err(),
            "kernel snapshot id must not occupy revision"
        );
        assert_secret_free(&value);

        let mut peak = snapshot.models[0].clone();
        peak.cache_write = Some(0.375);
        peak.min_input_tokens = Some(256_001);
        peak.max_input_tokens = None;
        peak.time_window = PricingTimeWindow::Peak;
        let peak_value = serde_json::to_value(&peak).unwrap();
        assert_eq!(peak_value["cacheWrite"], 0.375);
        assert_eq!(peak_value["minInputTokens"], 256_001);
        assert_eq!(peak_value["maxInputTokens"], Value::Null);
        assert_eq!(peak_value["timeWindow"], "peak");
        assert_eq!(
            serde_json::to_value(PricingTimeWindow::OffPeak).unwrap(),
            json!("off_peak")
        );
    }

    #[test]
    fn pricing_refresh_and_provider_pricing_emit_nullable_fields() {
        let refresh = PricingRefresh {
            snapshot: sample_pricing_snapshot(),
            refresh_status: PricingRefreshStatus::NeedsConfirmation,
            multiplier_changes: vec![PricingMultiplierChange {
                model_id: "grok-4.5".into(),
                current_multiplier: 4.0,
                official_multiplier: 5.0,
            }],
            official_content_hash: Some("official-hash".into()),
            error: None,
        };
        let refresh_value = serde_json::to_value(&refresh).unwrap();
        assert_eq!(refresh_value["refreshStatus"], "needs_confirmation");
        assert_eq!(refresh_value["officialContentHash"], "official-hash");
        assert_eq!(refresh_value["error"], Value::Null);
        assert_eq!(
            refresh_value["snapshot"]["pricingRevision"],
            "seed-2026-08-16-local-v4"
        );
        assert!(refresh_value.get("models").is_none());
        assert!(refresh_value.get("snapshotJson").is_none());
        assert_eq!(
            serde_json::to_value(PricingRefreshStatus::Success).unwrap(),
            json!("success")
        );
        assert_eq!(
            serde_json::to_value(PricingRefreshStatus::Unchanged).unwrap(),
            json!("unchanged")
        );
        assert_eq!(
            serde_json::to_value(PricingRefreshStatus::FailedNoChange).unwrap(),
            json!("failed_no_change")
        );
        assert_secret_free(&refresh_value);

        let provider = ProviderPricing {
            provider_id: "opencode".into(),
            offering_id: "go".into(),
            availability: PricingAvailability::Available,
            snapshot: None,
            revision: 11,
            process_generation: 9,
            pricing_revision: "seed-2026-08-16-local-v4".into(),
        };
        let provider_value = serde_json::to_value(&provider).unwrap();
        assert_eq!(provider_value["snapshot"], Value::Null);
        assert_eq!(provider_value["availability"], "available");
        assert_eq!(provider_value["providerId"], "opencode");
        assert_eq!(provider_value["revision"], 11);
        assert!(provider_value.get("snapshotJson").is_none());
        assert_eq!(
            serde_json::to_value(PricingAvailability::Unavailable).unwrap(),
            json!("unavailable")
        );
        assert_eq!(
            serde_json::to_value(PricingAvailability::NotApplicable).unwrap(),
            json!("not_applicable")
        );
        assert_eq!(
            serde_json::to_value(PricingAvailability::Unpriced).unwrap(),
            json!("unpriced")
        );
        assert_secret_free(&provider_value);
    }

    #[test]
    fn pricing_mutations_flatten_cas_require_pricing_revision_and_reject_unknown_fields() {
        let omitted: PricingRefreshUpdate = serde_json::from_value(json!({
            "expectedRevision": 11,
            "processGeneration": 9,
            "expectedPricingRevision": "seed-2026-08-16-local-v4"
        }))
        .unwrap();
        assert_eq!(omitted.expectation.expected_revision, 11);
        assert_eq!(omitted.expectation.process_generation, 9);
        assert_eq!(
            omitted.expected_pricing_revision,
            "seed-2026-08-16-local-v4"
        );
        assert!(omitted.policy.is_none());
        assert!(omitted.expected_official_content_hash.is_none());

        let confirmed: PricingRefreshUpdate = serde_json::from_value(json!({
            "expectedRevision": 11,
            "processGeneration": 9,
            "expectedPricingRevision": "seed-2026-08-16-local-v4",
            "policy": "keep_current",
            "expectedOfficialContentHash": "official-hash"
        }))
        .unwrap();
        assert_eq!(confirmed.policy, Some(PricingRefreshPolicy::KeepCurrent));
        assert_eq!(
            confirmed.expected_official_content_hash.as_deref(),
            Some("official-hash")
        );
        assert_eq!(
            serde_json::to_value(PricingRefreshPolicy::UseOfficial).unwrap(),
            json!("use_official")
        );

        assert!(
            serde_json::from_value::<PricingRefreshUpdate>(json!({
                "expectedRevision": 11,
                "processGeneration": 9
            }))
            .is_err(),
            "expectedPricingRevision is required"
        );
        assert!(
            serde_json::from_value::<PricingRefreshUpdate>(json!({
                "expected_revision": 11,
                "processGeneration": 9,
                "expectedPricingRevision": "seed"
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<PricingRefreshUpdate>(json!({
                "expectedRevision": 11,
                "processGeneration": 9,
                "expected_pricing_revision": "seed"
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<PricingRefreshUpdate>(json!({
                "expectedRevision": 11,
                "processGeneration": 9,
                "expectedPricingRevision": "seed",
                "snapshotJson": "{}"
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<PricingRefreshUpdate>(json!({
                "expectedRevision": 11,
                "processGeneration": 9,
                "expectedPricingRevision": "seed",
                "key": "sk-secret"
            }))
            .is_err()
        );

        let multipliers: PricingMultipliersUpdate = serde_json::from_value(json!({
            "expectedRevision": 11,
            "processGeneration": 9,
            "expectedPricingRevision": "seed-2026-08-16-local-v4",
            "multipliers": [{ "modelId": "grok-4.5", "multiplier": 4.0 }]
        }))
        .unwrap();
        assert_eq!(multipliers.multipliers[0].model_id, "grok-4.5");
        assert_eq!(multipliers.multipliers[0].multiplier, 4.0);
        assert!(
            serde_json::from_value::<PricingMultipliersUpdate>(json!({
                "expectedRevision": 11,
                "processGeneration": 9,
                "multipliers": []
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<PricingMultipliersUpdate>(json!({
                "expectedRevision": 11,
                "processGeneration": 9,
                "expectedPricingRevision": "seed",
                "multipliers": [{ "model_id": "grok-4.5", "multiplier": 4.0 }]
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<PricingMultiplierWrite>(json!({
                "modelId": "grok-4.5",
                "multiplier": 4.0,
                "token": "nope"
            }))
            .is_err()
        );
    }
}

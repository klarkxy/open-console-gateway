use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

pub use crate::kernel::catalog::{
    CatalogParseError, CredentialKind, QuotaScope, UpstreamAuthScheme, UpstreamProtocolKind,
};
pub use crate::kernel::ids::{
    ANONYMOUS_FREE_OFFERING_ID, COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_ALIAS,
    COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM, COMMAND_CODE_PROVIDER_ID, CUSTOM_API_OFFERING_ID,
    CUSTOM_PROVIDER_ID, GO_OFFERING_ID, GOAT_OFFERING_ID, OPENCODE_PROVIDER_ID,
    OPENCODE_ZEN_FREE_PROVIDER_ID, SCNET_PROVIDER_ID, SCNET_TOKEN_PLAN_BASIC_OFFERING_ID,
    SCNET_TOKEN_PLAN_OFFERING_IDS, SCNET_TOKEN_PLAN_PREMIUM_OFFERING_ID,
    SCNET_TOKEN_PLAN_STANDARD_OFFERING_ID, ZEN_FREE_ACCOUNT_ID, ZEN_FREE_ACCOUNT_NAME,
};

/// Official Command Code Provider API v1 base. Catalog `routable` stays false;
/// this constant is the transport contract, not a production enablement flag.
pub const COMMAND_CODE_GOAT_BASE_URL: &str = "https://api.commandcode.ai/provider/v1";
pub const COMMAND_CODE_GOAT_HOST: &str = "api.commandcode.ai";
/// Relative to [`COMMAND_CODE_GOAT_BASE_URL`].
pub const COMMAND_CODE_GOAT_CHAT_COMPLETIONS_PATH: &str = "/chat/completions";
/// Relative to [`COMMAND_CODE_GOAT_BASE_URL`]. Documented official endpoint;
/// the first supported GOAT model still converts client Messages to Chat.
pub const COMMAND_CODE_GOAT_MESSAGES_PATH: &str = "/messages";
/// Documented official discovery path. Billing/schema is unproven; must not be
/// used for connection verification or model enablement.
pub const COMMAND_CODE_GOAT_MODELS_PATH: &str = "/models";
pub const COMMAND_CODE_GOAT_QUOTA_5H: f64 = 14.0;
pub const COMMAND_CODE_GOAT_QUOTA_WEEK: f64 = 35.0;
pub const COMMAND_CODE_GOAT_QUOTA_MONTH: f64 = 70.0;

pub const SCNET_TOKEN_PLAN_KEY_PREFIX: &str = "sk-tp-";
pub const SCNET_RISK_ACKNOWLEDGEMENT_ID: &str = "scnet-token-plan-restrictions";
pub const SCNET_RISK_ACKNOWLEDGEMENT_VERSION: &str = "2026-08-21";
pub const SCNET_RISK_ACKNOWLEDGEMENT_SOURCE_URL: &str =
    "https://www.scnet.cn/ac/openapi/doc/2.0/moduleapi/plans/token-plan.html";
pub const SCNET_RISK_ACKNOWLEDGEMENT_BODY: &str = "SCNet Token Plan keys (sk-tp-) are limited to interactive use inside AI tools. Account sharing and using the API as a custom application backend, automation script, or non-interactive batch caller is prohibited and may suspend the subscription or revoke the key.";
/// Pinned SHA-256 of [`SCNET_RISK_ACKNOWLEDGEMENT_BODY`]. Changing the body
/// requires an explicit acknowledgement version bump and this hash update.
pub const SCNET_RISK_ACKNOWLEDGEMENT_CONTENT_HASH: &str =
    "d8d82fda982880016cf7f3c5e6a8e944cac4dcf900643f31eab3cdfd05aa6e60";

pub const SCNET_TOKEN_PLAN_OFFICIAL_BASIC_NAME: &str = "基础版";
pub const SCNET_TOKEN_PLAN_OFFICIAL_STANDARD_NAME: &str = "标准版";
pub const SCNET_TOKEN_PLAN_OFFICIAL_PREMIUM_NAME: &str = "高级版";
/// Catalog `model_source` shared by token-plan-basic/standard/premium.
/// Official usable-model table only; not a client alias registry.
pub const SCNET_TOKEN_PLAN_MODEL_SOURCE: &str = "official_token_plan_usable_models_2026_08_21";
pub const SCNET_TOKEN_PLAN_MODEL_SNAPSHOT_VERSION: &str = "2026-08-21";
pub const SCNET_TOKEN_PLAN_MODEL_SOURCE_URL: &str = SCNET_RISK_ACKNOWLEDGEMENT_SOURCE_URL;

/// Documented OpenAI/Anthropic bases and paths. Future adapter input only;
/// this crate must not issue live Token Plan requests.
pub const SCNET_TOKEN_PLAN_ENDPOINT_SOURCE_URL: &str =
    "https://www.scnet.cn/ac/openapi/doc/2.0/moduleapi/tutorial/quickstart.html";
pub const SCNET_TOKEN_PLAN_OPENAI_BASE_URL: &str = "https://api.scnet.cn/api/llm/v1";
pub const SCNET_TOKEN_PLAN_ANTHROPIC_BASE_URL: &str = "https://api.scnet.cn/api/llm/anthropic";
pub const SCNET_TOKEN_PLAN_CHAT_COMPLETIONS_PATH: &str = "/chat/completions";
pub const SCNET_TOKEN_PLAN_MESSAGES_PATH: &str = "/v1/messages";

/// Official Token Plan usable-model table, 2026-08-21, exact case and order.
/// Shared by 基础版/标准版/高级版. Do not publish as `model_aliases`.
/// Catalog aliases come from the Alias registry's currently routeable
/// mappings; these Plans stay unroutable, so that list stays empty.
pub const SCNET_TOKEN_PLAN_USABLE_MODELS: &[&str] = &[
    "GLM-5.2",
    "GLM-5",
    "GLM-5.1",
    "Kimi-K3",
    "Kimi-K2.7-Code",
    "Kimi-K2.6",
    "Kimi-K2.5",
    "DeepSeek-V4-Flash",
    "DeepSeek-V3.2",
    "MiniMax-M3",
    "MiniMax-M2.7",
    "MiniMax-M2.5",
    "MiMo-V2.5-Pro",
];

/// Pricing-table / FAQ extras that are not in the usable-model table.
pub const SCNET_TOKEN_PLAN_EXCLUDED_PRICING_TABLE_OR_FAQ_MODELS: &[&str] = &[
    "DeepSeek-V4-Pro",
    "DeepSeek-V4-Flash-0731",
    "Qwen3.8-max",
    "Qwen3-235B-A22B",
];

pub const QUOTA_WINDOW_FIVE_HOURS: &str = "five_hours";
pub const QUOTA_WINDOW_WEEK: &str = "week";
pub const QUOTA_WINDOW_MONTH: &str = "month";
pub const QUOTA_WINDOW_FREE: &str = "free";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuiltinOffering {
    pub provider_id: &'static str,
    pub offering_id: &'static str,
    pub credential_kind: CredentialKind,
    pub quota_scope: QuotaScope,
    pub singleton_account_id: Option<&'static str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CreationAvailability {
    Available,
    Unavailable,
}

impl CreationAvailability {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Available => "available",
            Self::Unavailable => "unavailable",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationPolicy {
    NotRequired,
    Required,
}

impl VerificationPolicy {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NotRequired => "not_required",
            Self::Required => "required",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionVerificationStatus {
    NotRequired,
    Pending,
    Verified,
    Failed,
}

impl ConnectionVerificationStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NotRequired => "not_required",
            Self::Pending => "pending",
            Self::Verified => "verified",
            Self::Failed => "failed",
        }
    }

    pub const fn allows_enablement(self) -> bool {
        matches!(self, Self::NotRequired | Self::Verified)
    }
}

impl TryFrom<&str> for ConnectionVerificationStatus {
    type Error = ProviderBindingError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "not_required" => Ok(Self::NotRequired),
            "pending" => Ok(Self::Pending),
            "verified" => Ok(Self::Verified),
            "failed" => Ok(Self::Failed),
            _ => Err(ProviderBindingError::UnknownVerificationStatus(
                value.to_string(),
            )),
        }
    }
}

/// Deterministic fallback when the preferred upstream protocol is disabled.
pub const PROTOCOL_FALLBACK_CHAT_RESPONSES_MESSAGES: &[UpstreamProtocolKind] = &[
    UpstreamProtocolKind::ChatCompletions,
    UpstreamProtocolKind::Responses,
    UpstreamProtocolKind::Messages,
];

/// Protocols whose OpenCode Go / known Zen endpoint, materialization, and
/// auth path the adapter can construct. This is the probe safety ceiling,
/// not static verified support (`MODEL_PROTOCOLS`).
pub const OPENCODE_CONSTRUCTABLE_PROTOCOLS: &[UpstreamProtocolKind] =
    PROTOCOL_FALLBACK_CHAT_RESPONSES_MESSAGES;

/// Command Code / SCNet documented surfaces have no Responses path.
pub const PROTOCOL_FALLBACK_CHAT_MESSAGES: &[UpstreamProtocolKind] = &[
    UpstreamProtocolKind::ChatCompletions,
    UpstreamProtocolKind::Messages,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct PlanFormField {
    pub id: &'static str,
    pub kind: &'static str,
    pub required: bool,
    pub immutable_after_create: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct PlanRiskNotice {
    pub acknowledgement_id: &'static str,
    pub version: &'static str,
    pub source_url: &'static str,
    pub body: &'static str,
}

impl PlanRiskNotice {
    pub fn content_hash(self) -> String {
        acknowledgement_content_hash(self.body)
    }
}

/// Official Token Plan usage restrictions pinned from the 2026-08-21 docs.
/// Adapter input only; these flags do not authorize outbound calls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScnetTokenPlanUsageRestrictions {
    pub interactive_ai_tools_only: bool,
    pub account_sharing_prohibited: bool,
    pub custom_application_backends_prohibited: bool,
    pub automation_scripts_prohibited: bool,
    pub non_interactive_batch_calls_prohibited: bool,
    pub curl_style_non_interactive_calls_prohibited: bool,
    pub quota_status_rest_established: bool,
    pub non_billable_verification_established: bool,
}

pub const SCNET_TOKEN_PLAN_USAGE_RESTRICTIONS: ScnetTokenPlanUsageRestrictions =
    ScnetTokenPlanUsageRestrictions {
        interactive_ai_tools_only: true,
        account_sharing_prohibited: true,
        custom_application_backends_prohibited: true,
        automation_scripts_prohibited: true,
        non_interactive_batch_calls_prohibited: true,
        curl_style_non_interactive_calls_prohibited: true,
        quota_status_rest_established: false,
        non_billable_verification_established: false,
    };

/// Documented Token Plan HTTP contract. Do not concatenate these into a live
/// client; official usage restrictions prohibit custom application backends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScnetTokenPlanDocumentedEndpoints {
    pub source_url: &'static str,
    pub openai_base_url: &'static str,
    pub anthropic_base_url: &'static str,
    pub chat_completions_path: &'static str,
    pub messages_path: &'static str,
    pub auth_scheme: UpstreamAuthScheme,
}

pub const SCNET_TOKEN_PLAN_DOCUMENTED_ENDPOINTS: ScnetTokenPlanDocumentedEndpoints =
    ScnetTokenPlanDocumentedEndpoints {
        source_url: SCNET_TOKEN_PLAN_ENDPOINT_SOURCE_URL,
        openai_base_url: SCNET_TOKEN_PLAN_OPENAI_BASE_URL,
        anthropic_base_url: SCNET_TOKEN_PLAN_ANTHROPIC_BASE_URL,
        chat_completions_path: SCNET_TOKEN_PLAN_CHAT_COMPLETIONS_PATH,
        messages_path: SCNET_TOKEN_PLAN_MESSAGES_PATH,
        auth_scheme: UpstreamAuthScheme::Bearer,
    };

/// Versioned official usable-model snapshot shared by all three offerings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScnetTokenPlanModelSnapshot {
    pub source: &'static str,
    pub version: &'static str,
    pub source_url: &'static str,
    pub upstream_models: &'static [&'static str],
    pub excluded_pricing_table_or_faq_models: &'static [&'static str],
}

pub const SCNET_TOKEN_PLAN_MODEL_SNAPSHOT: ScnetTokenPlanModelSnapshot =
    ScnetTokenPlanModelSnapshot {
        source: SCNET_TOKEN_PLAN_MODEL_SOURCE,
        version: SCNET_TOKEN_PLAN_MODEL_SNAPSHOT_VERSION,
        source_url: SCNET_TOKEN_PLAN_MODEL_SOURCE_URL,
        upstream_models: SCNET_TOKEN_PLAN_USABLE_MODELS,
        excluded_pricing_table_or_faq_models: SCNET_TOKEN_PLAN_EXCLUDED_PRICING_TABLE_OR_FAQ_MODELS,
    };

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuiltinPlan {
    pub offering: BuiltinOffering,
    pub display_name: &'static str,
    pub display_family: &'static str,
    pub creation_availability: CreationAvailability,
    pub creation_unavailable_reason: Option<&'static str>,
    pub verification_policy: VerificationPolicy,
    pub verification_runtime_availability: &'static str,
    pub routable: bool,
    pub managed_registration: bool,
    pub pricing_availability: &'static str,
    pub usage_availability: &'static str,
    /// Whether the dashboard may persist a user-entered quota percentage for
    /// display when the provider exposes no machine-readable usage endpoint.
    pub manual_usage_calibration: bool,
    pub quota_unit: &'static str,
    pub model_source: &'static str,
    pub key_prefix: Option<&'static str>,
    pub auth_schemes: &'static [UpstreamAuthScheme],
    pub upstream_protocols: &'static [UpstreamProtocolKind],
    pub form_fields: &'static [PlanFormField],
    pub risk_notice: Option<PlanRiskNotice>,
}

const NAME_FIELD: PlanFormField = PlanFormField {
    id: "name",
    kind: "text",
    required: true,
    immutable_after_create: false,
};
const KEY_FIELD: PlanFormField = PlanFormField {
    id: "key",
    kind: "secret",
    required: true,
    immutable_after_create: false,
};
const PURCHASE_DATE_FIELD: PlanFormField = PlanFormField {
    id: "purchase_date",
    kind: "date",
    required: false,
    immutable_after_create: false,
};
const NOTES_FIELD: PlanFormField = PlanFormField {
    id: "notes",
    kind: "text",
    required: false,
    immutable_after_create: false,
};
const ACKNOWLEDGEMENT_FIELD: PlanFormField = PlanFormField {
    id: "acknowledgement",
    kind: "acknowledgement",
    required: true,
    immutable_after_create: false,
};
const BASE_URL_FIELD: PlanFormField = PlanFormField {
    id: "base_url",
    kind: "url",
    required: true,
    immutable_after_create: false,
};
const PROTOCOL_FIELD: PlanFormField = PlanFormField {
    id: "upstream_protocol",
    kind: "select",
    required: true,
    immutable_after_create: true,
};
const AUTH_SCHEME_FIELD: PlanFormField = PlanFormField {
    id: "auth_scheme",
    kind: "select",
    required: true,
    immutable_after_create: true,
};
const MODELS_FIELD: PlanFormField = PlanFormField {
    id: "model_capabilities",
    kind: "models",
    required: true,
    immutable_after_create: false,
};

const GO_FORM_FIELDS: [PlanFormField; 4] =
    [NAME_FIELD, KEY_FIELD, PURCHASE_DATE_FIELD, NOTES_FIELD];
const GOAT_FORM_FIELDS: [PlanFormField; 4] =
    [NAME_FIELD, KEY_FIELD, PURCHASE_DATE_FIELD, NOTES_FIELD];
const SCNET_FORM_FIELDS: [PlanFormField; 5] = [
    NAME_FIELD,
    KEY_FIELD,
    PURCHASE_DATE_FIELD,
    NOTES_FIELD,
    ACKNOWLEDGEMENT_FIELD,
];
const CUSTOM_FORM_FIELDS: [PlanFormField; 7] = [
    NAME_FIELD,
    KEY_FIELD,
    NOTES_FIELD,
    BASE_URL_FIELD,
    PROTOCOL_FIELD,
    AUTH_SCHEME_FIELD,
    MODELS_FIELD,
];

const BEARER_AUTH: [UpstreamAuthScheme; 1] = [UpstreamAuthScheme::Bearer];
const CUSTOM_AUTH: [UpstreamAuthScheme; 2] =
    [UpstreamAuthScheme::Bearer, UpstreamAuthScheme::XApiKey];
const GO_PROTOCOLS: [UpstreamProtocolKind; 3] = [
    UpstreamProtocolKind::ChatCompletions,
    UpstreamProtocolKind::Responses,
    UpstreamProtocolKind::Messages,
];
const GOAT_PROTOCOLS: [UpstreamProtocolKind; 2] = [
    UpstreamProtocolKind::ChatCompletions,
    UpstreamProtocolKind::Messages,
];
const SCNET_PROTOCOLS: [UpstreamProtocolKind; 2] = [
    UpstreamProtocolKind::ChatCompletions,
    UpstreamProtocolKind::Messages,
];
const CUSTOM_PROTOCOLS: [UpstreamProtocolKind; 3] = [
    UpstreamProtocolKind::ChatCompletions,
    UpstreamProtocolKind::Responses,
    UpstreamProtocolKind::Messages,
];

const SCNET_RISK_NOTICE: PlanRiskNotice = PlanRiskNotice {
    acknowledgement_id: SCNET_RISK_ACKNOWLEDGEMENT_ID,
    version: SCNET_RISK_ACKNOWLEDGEMENT_VERSION,
    source_url: SCNET_RISK_ACKNOWLEDGEMENT_SOURCE_URL,
    body: SCNET_RISK_ACKNOWLEDGEMENT_BODY,
};

const fn key_offering(provider_id: &'static str, offering_id: &'static str) -> BuiltinOffering {
    BuiltinOffering {
        provider_id,
        offering_id,
        credential_kind: CredentialKind::ApiKey,
        quota_scope: QuotaScope::Key,
        singleton_account_id: None,
    }
}

pub const BUILTIN_PLANS: [BuiltinPlan; 7] = [
    BuiltinPlan {
        offering: key_offering(OPENCODE_PROVIDER_ID, GO_OFFERING_ID),
        display_name: "OpenCode Go",
        display_family: "OpenCode",
        creation_availability: CreationAvailability::Available,
        creation_unavailable_reason: None,
        verification_policy: VerificationPolicy::NotRequired,
        verification_runtime_availability: "optional",
        routable: true,
        managed_registration: true,
        pricing_availability: "available",
        usage_availability: "available",
        manual_usage_calibration: false,
        quota_unit: "usd",
        model_source: "builtin_go_protocol_table",
        key_prefix: None,
        auth_schemes: &BEARER_AUTH,
        upstream_protocols: &GO_PROTOCOLS,
        form_fields: &GO_FORM_FIELDS,
        risk_notice: None,
    },
    BuiltinPlan {
        offering: BuiltinOffering {
            provider_id: OPENCODE_ZEN_FREE_PROVIDER_ID,
            offering_id: ANONYMOUS_FREE_OFFERING_ID,
            credential_kind: CredentialKind::None,
            quota_scope: QuotaScope::EgressIp,
            singleton_account_id: Some(ZEN_FREE_ACCOUNT_ID),
        },
        display_name: "OpenCode Zen Free",
        display_family: "OpenCode",
        creation_availability: CreationAvailability::Unavailable,
        creation_unavailable_reason: Some(
            "Zen Free is a built-in singleton and cannot be created through the generic account API",
        ),
        verification_policy: VerificationPolicy::NotRequired,
        verification_runtime_availability: "not_applicable",
        routable: true,
        managed_registration: false,
        pricing_availability: "not_applicable",
        usage_availability: "local_state",
        manual_usage_calibration: false,
        quota_unit: "request",
        model_source: "builtin_zen_free_alias",
        key_prefix: None,
        auth_schemes: &[],
        upstream_protocols: &GO_PROTOCOLS,
        form_fields: &[],
        risk_notice: None,
    },
    BuiltinPlan {
        offering: key_offering(COMMAND_CODE_PROVIDER_ID, GOAT_OFFERING_ID),
        display_name: "Command Code GOAT",
        display_family: "Command Code",
        creation_availability: CreationAvailability::Available,
        creation_unavailable_reason: None,
        verification_policy: VerificationPolicy::Required,
        verification_runtime_availability: "unavailable",
        routable: false,
        managed_registration: false,
        pricing_availability: "unavailable",
        usage_availability: "unavailable",
        manual_usage_calibration: true,
        quota_unit: "credits",
        model_source: "builtin_command_code_protocol_table",
        key_prefix: None,
        auth_schemes: &BEARER_AUTH,
        upstream_protocols: &GOAT_PROTOCOLS,
        form_fields: &GOAT_FORM_FIELDS,
        risk_notice: None,
    },
    BuiltinPlan {
        offering: key_offering(SCNET_PROVIDER_ID, SCNET_TOKEN_PLAN_BASIC_OFFERING_ID),
        display_name: "SCNet Token Plan Basic",
        display_family: "SCNet",
        creation_availability: CreationAvailability::Available,
        creation_unavailable_reason: None,
        verification_policy: VerificationPolicy::Required,
        verification_runtime_availability: "unavailable",
        routable: false,
        managed_registration: false,
        pricing_availability: "unavailable",
        usage_availability: "unavailable",
        manual_usage_calibration: false,
        quota_unit: "credits",
        model_source: SCNET_TOKEN_PLAN_MODEL_SOURCE,
        key_prefix: Some(SCNET_TOKEN_PLAN_KEY_PREFIX),
        auth_schemes: &BEARER_AUTH,
        upstream_protocols: &SCNET_PROTOCOLS,
        form_fields: &SCNET_FORM_FIELDS,
        risk_notice: Some(SCNET_RISK_NOTICE),
    },
    BuiltinPlan {
        offering: key_offering(SCNET_PROVIDER_ID, SCNET_TOKEN_PLAN_STANDARD_OFFERING_ID),
        display_name: "SCNet Token Plan Standard",
        display_family: "SCNet",
        creation_availability: CreationAvailability::Available,
        creation_unavailable_reason: None,
        verification_policy: VerificationPolicy::Required,
        verification_runtime_availability: "unavailable",
        routable: false,
        managed_registration: false,
        pricing_availability: "unavailable",
        usage_availability: "unavailable",
        manual_usage_calibration: false,
        quota_unit: "credits",
        model_source: SCNET_TOKEN_PLAN_MODEL_SOURCE,
        key_prefix: Some(SCNET_TOKEN_PLAN_KEY_PREFIX),
        auth_schemes: &BEARER_AUTH,
        upstream_protocols: &SCNET_PROTOCOLS,
        form_fields: &SCNET_FORM_FIELDS,
        risk_notice: Some(SCNET_RISK_NOTICE),
    },
    BuiltinPlan {
        offering: key_offering(SCNET_PROVIDER_ID, SCNET_TOKEN_PLAN_PREMIUM_OFFERING_ID),
        display_name: "SCNet Token Plan Premium",
        display_family: "SCNet",
        creation_availability: CreationAvailability::Available,
        creation_unavailable_reason: None,
        verification_policy: VerificationPolicy::Required,
        verification_runtime_availability: "unavailable",
        routable: false,
        managed_registration: false,
        pricing_availability: "unavailable",
        usage_availability: "unavailable",
        manual_usage_calibration: false,
        quota_unit: "credits",
        model_source: SCNET_TOKEN_PLAN_MODEL_SOURCE,
        key_prefix: Some(SCNET_TOKEN_PLAN_KEY_PREFIX),
        auth_schemes: &BEARER_AUTH,
        upstream_protocols: &SCNET_PROTOCOLS,
        form_fields: &SCNET_FORM_FIELDS,
        risk_notice: Some(SCNET_RISK_NOTICE),
    },
    BuiltinPlan {
        offering: key_offering(CUSTOM_PROVIDER_ID, CUSTOM_API_OFFERING_ID),
        display_name: "Custom API",
        display_family: "Custom",
        creation_availability: CreationAvailability::Available,
        creation_unavailable_reason: None,
        verification_policy: VerificationPolicy::Required,
        verification_runtime_availability: "available",
        routable: true,
        managed_registration: false,
        pricing_availability: "unpriced",
        usage_availability: "unavailable",
        manual_usage_calibration: false,
        quota_unit: "token",
        model_source: "account_capabilities",
        key_prefix: None,
        auth_schemes: &CUSTOM_AUTH,
        upstream_protocols: &CUSTOM_PROTOCOLS,
        form_fields: &CUSTOM_FORM_FIELDS,
        risk_notice: None,
    },
];

pub const BUILTIN_OFFERINGS: [BuiltinOffering; 7] = [
    BUILTIN_PLANS[0].offering,
    BUILTIN_PLANS[1].offering,
    BUILTIN_PLANS[2].offering,
    BUILTIN_PLANS[3].offering,
    BUILTIN_PLANS[4].offering,
    BUILTIN_PLANS[5].offering,
    BUILTIN_PLANS[6].offering,
];

pub fn default_provider_id() -> String {
    OPENCODE_PROVIDER_ID.to_string()
}

pub fn default_offering_id() -> String {
    GO_OFFERING_ID.to_string()
}

pub fn default_credential_kind() -> CredentialKind {
    CredentialKind::ApiKey
}

pub fn default_quota_scope() -> QuotaScope {
    QuotaScope::Key
}

pub fn builtin_offering(provider_id: &str, offering_id: &str) -> Option<BuiltinOffering> {
    builtin_plan(provider_id, offering_id).map(|plan| plan.offering)
}

pub fn builtin_plan(provider_id: &str, offering_id: &str) -> Option<BuiltinPlan> {
    BUILTIN_PLANS.iter().copied().find(|plan| {
        plan.offering.provider_id == provider_id && plan.offering.offering_id == offering_id
    })
}

/// Exhaustive, code-owned adapter identity. Not a plugin slot, JSON DSL, or
/// user-defined implementation. Custom API is [`Self::ConfigurableHttp`], not
/// a base class other adapters inherit from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProviderAdapterKind {
    OpenCodeGo,
    ZenFree,
    CommandCodeGoat,
    Scnet,
    ConfigurableHttp,
}

impl ProviderAdapterKind {
    pub const ALL: [Self; 5] = [
        Self::OpenCodeGo,
        Self::ZenFree,
        Self::CommandCodeGoat,
        Self::Scnet,
        Self::ConfigurableHttp,
    ];

    pub fn from_offering(provider_id: &str, offering_id: &str) -> Option<Self> {
        match (provider_id, offering_id) {
            (OPENCODE_PROVIDER_ID, GO_OFFERING_ID) => Some(Self::OpenCodeGo),
            (OPENCODE_ZEN_FREE_PROVIDER_ID, ANONYMOUS_FREE_OFFERING_ID) => Some(Self::ZenFree),
            (COMMAND_CODE_PROVIDER_ID, GOAT_OFFERING_ID) => Some(Self::CommandCodeGoat),
            (SCNET_PROVIDER_ID, SCNET_TOKEN_PLAN_BASIC_OFFERING_ID)
            | (SCNET_PROVIDER_ID, SCNET_TOKEN_PLAN_STANDARD_OFFERING_ID)
            | (SCNET_PROVIDER_ID, SCNET_TOKEN_PLAN_PREMIUM_OFFERING_ID) => Some(Self::Scnet),
            (CUSTOM_PROVIDER_ID, CUSTOM_API_OFFERING_ID) => Some(Self::ConfigurableHttp),
            _ => None,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OpenCodeGo => "opencode_go",
            Self::ZenFree => "zen_free",
            Self::CommandCodeGoat => "command_code_goat",
            Self::Scnet => "scnet",
            Self::ConfigurableHttp => "configurable_http",
        }
    }

    /// Built-in provider id that owns this adapter's shared contract scope.
    /// Configurable HTTP is per-endpoint and has no provider scope.
    pub const fn provider_scope_id(self) -> Option<&'static str> {
        match self {
            Self::OpenCodeGo => Some(OPENCODE_PROVIDER_ID),
            Self::ZenFree => Some(OPENCODE_ZEN_FREE_PROVIDER_ID),
            Self::CommandCodeGoat => Some(COMMAND_CODE_PROVIDER_ID),
            Self::Scnet => Some(SCNET_PROVIDER_ID),
            Self::ConfigurableHttp => None,
        }
    }

    pub const fn catalog_refresh_supported(self) -> bool {
        match self {
            Self::ZenFree | Self::ConfigurableHttp => true,
            Self::OpenCodeGo | Self::CommandCodeGoat | Self::Scnet => false,
        }
    }

    pub const fn protocol_probe_supported(self) -> bool {
        match self {
            Self::OpenCodeGo | Self::ZenFree | Self::ConfigurableHttp => true,
            Self::CommandCodeGoat | Self::Scnet => false,
        }
    }
}

/// Zero-sized OpenCode Go adapter identity. Capability records are composed
/// from the sealed contracts below; this type is not a plugin slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct OpenCodeGoAdapter;

/// Zero-sized Zen Free adapter identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ZenFreeAdapter;

/// Zero-sized Command Code GOAT adapter identity. Production stays fail-closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CommandCodeGoatAdapter;

/// Zero-sized SCNet Token Plan adapter identity. Production stays fail-closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ScnetAdapter;

/// Zero-sized Configurable HTTP adapter identity (Custom API). Not a base class
/// other adapters inherit from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ConfigurableHttpAdapter;

pub fn is_scnet_token_plan(provider_id: &str, offering_id: &str) -> bool {
    matches!(
        ProviderAdapterKind::from_offering(provider_id, offering_id),
        Some(ProviderAdapterKind::Scnet)
    )
}

pub fn scnet_token_plan_official_offering_name(offering_id: &str) -> Option<&'static str> {
    match offering_id {
        SCNET_TOKEN_PLAN_BASIC_OFFERING_ID => Some(SCNET_TOKEN_PLAN_OFFICIAL_BASIC_NAME),
        SCNET_TOKEN_PLAN_STANDARD_OFFERING_ID => Some(SCNET_TOKEN_PLAN_OFFICIAL_STANDARD_NAME),
        SCNET_TOKEN_PLAN_PREMIUM_OFFERING_ID => Some(SCNET_TOKEN_PLAN_OFFICIAL_PREMIUM_NAME),
        _ => None,
    }
}

/// Shared official snapshot for every Token Plan offering. None for other Plans.
pub fn scnet_token_plan_model_snapshot(
    provider_id: &str,
    offering_id: &str,
) -> Option<ScnetTokenPlanModelSnapshot> {
    is_scnet_token_plan(provider_id, offering_id).then_some(SCNET_TOKEN_PLAN_MODEL_SNAPSHOT)
}

pub fn is_command_code_goat(provider_id: &str, offering_id: &str) -> bool {
    matches!(
        ProviderAdapterKind::from_offering(provider_id, offering_id),
        Some(ProviderAdapterKind::CommandCodeGoat)
    )
}

pub fn is_custom_api(provider_id: &str, offering_id: &str) -> bool {
    matches!(
        ProviderAdapterKind::from_offering(provider_id, offering_id),
        Some(ProviderAdapterKind::ConfigurableHttp)
    )
}

/// Relative path appended onto a Custom base URL prefix. Callers must join
/// without escaping the origin or persisted path prefix.
pub fn custom_endpoint_relative_path(protocol: UpstreamProtocolKind) -> &'static str {
    match protocol {
        UpstreamProtocolKind::ChatCompletions => "chat/completions",
        UpstreamProtocolKind::Responses => "responses",
        UpstreamProtocolKind::Messages => "messages",
    }
}

/// Static code-owned registry of built-in provider offerings. Lookup is by
/// `(provider_id, offering_id)`; unknown pairs fail closed.
pub struct ProviderRegistry;

impl ProviderRegistry {
    pub fn get(provider_id: &str, offering_id: &str) -> Option<ProviderDescriptor> {
        let plan = builtin_plan(provider_id, offering_id)?;
        let kind = ProviderAdapterKind::from_offering(provider_id, offering_id)?;
        Some(ProviderDescriptor::from_plan(kind, plan))
    }

    pub fn iter() -> impl Iterator<Item = ProviderDescriptor> {
        BUILTIN_PLANS
            .iter()
            .filter_map(|plan| Self::get(plan.offering.provider_id, plan.offering.offering_id))
    }
}

/// Composed capability records selected from one concrete adapter.
/// Built only through [`ProviderCapabilities::compose`] /
/// [`ProviderAdapterKind::compose_capabilities`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderCapabilities {
    pub model_catalog: ModelCatalogDescriptor,
    pub inference: InferenceRoutingDescriptor,
    pub protocol_probe: ProtocolProbeDescriptor,
    pub verification: VerificationDescriptor,
    pub usage: UsageDescriptor,
    pub pricing: PricingDescriptor,
    pub error_cooldown: ErrorCooldownDescriptor,
    pub card_actions: CardActionsDescriptor,
}

/// Composed capability surfaces for one catalog offering. These are facts for
/// later persistence/UI; this slice does not change dashboard DTOs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderDescriptor {
    pub kind: ProviderAdapterKind,
    pub provider_id: &'static str,
    pub offering_id: &'static str,
    pub model_catalog: ModelCatalogDescriptor,
    pub inference: InferenceRoutingDescriptor,
    pub protocol_probe: ProtocolProbeDescriptor,
    pub verification: VerificationDescriptor,
    pub usage: UsageDescriptor,
    pub pricing: PricingDescriptor,
    pub error_cooldown: ErrorCooldownDescriptor,
    pub card_actions: CardActionsDescriptor,
}

impl ProviderDescriptor {
    fn from_plan(kind: ProviderAdapterKind, plan: BuiltinPlan) -> Self {
        Self::from_capabilities(kind, plan, kind.compose_capabilities(plan))
    }

    fn from_capabilities(
        kind: ProviderAdapterKind,
        plan: BuiltinPlan,
        capabilities: ProviderCapabilities,
    ) -> Self {
        Self {
            kind,
            provider_id: plan.offering.provider_id,
            offering_id: plan.offering.offering_id,
            model_catalog: capabilities.model_catalog,
            inference: capabilities.inference,
            protocol_probe: capabilities.protocol_probe,
            verification: capabilities.verification,
            usage: capabilities.usage,
            pricing: capabilities.pricing,
            error_cooldown: capabilities.error_cooldown,
            card_actions: capabilities.card_actions,
        }
    }

    pub fn capabilities(self) -> ProviderCapabilities {
        ProviderCapabilities {
            model_catalog: self.model_catalog,
            inference: self.inference,
            protocol_probe: self.protocol_probe,
            verification: self.verification,
            usage: self.usage,
            pricing: self.pricing,
            error_cooldown: self.error_cooldown,
            card_actions: self.card_actions,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelCatalogKind {
    BuiltinGoProtocolTable,
    ZenFreePersistedSnapshot,
    BuiltinCommandCodeProtocolTable,
    OfficialTokenPlanUsableModels,
    AccountDeclaredCapabilities,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelCatalogDescriptor {
    pub kind: ModelCatalogKind,
    pub catalog_source: &'static str,
    pub publishes_client_aliases: bool,
    pub admin_explicit_refresh: bool,
    pub overlays_declared_ids: bool,
    pub snapshot_is_adapter_input_only: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InferenceChannelKind {
    Go,
    Free,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InferenceOriginKind {
    ConfigUpstreamBase,
    DerivedZenBase,
    OfficialFixed,
    AccountConfigured,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InferenceAuthDescriptor {
    OpenCodeProtocolDefault,
    Bearer,
    None,
    ConfigurableBearerOrXApiKey,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InferenceRoutingDescriptor {
    pub catalog_routable: bool,
    pub production_inference: bool,
    pub channel: Option<InferenceChannelKind>,
    pub credential_kind: CredentialKind,
    pub quota_scope: QuotaScope,
    pub auth: InferenceAuthDescriptor,
    pub follow_redirects: bool,
    pub origin: InferenceOriginKind,
    pub loopback_test_seam_only: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtocolMatrixKind {
    OpenCodeModelProtocols,
    CommandCodeNative,
    DocumentedChatAndMessages,
    AccountDeclaredProtocol,
}

/// Immutable adapter ceiling for explicit protocol probes. Distinct from
/// static/preset verified support, which still begins from `MODEL_PROTOCOLS`
/// (OpenCode/Zen), Command Code native rows, SCNet documented surfaces, or
/// the Custom account's declared protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StructuralProbeCeiling {
    /// GOAT/SCNet: probes are unavailable; production stays hard-unroutable.
    Unavailable,
    /// Known OpenCode Go models: Chat Completions, Responses, and Messages
    /// all have constructable `/v1/...` paths and OpenCode auth.
    OpenCodeConstructable,
    /// Known Zen models share OpenCode constructable paths. Unknown `-free`
    /// IDs stay Chat-only. Anything else is empty.
    ZenFreeConstructable,
    /// Configurable HTTP: only the account's immutable declared protocol.
    AccountDeclared,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProtocolProbeDescriptor {
    pub request_path_may_trial: bool,
    pub matrix: ProtocolMatrixKind,
    pub unknown_zen_free_defaults_to_chat: bool,
    pub fallback_priority: &'static [UpstreamProtocolKind],
    /// Dedicated admin probe surface. Request paths must stay false.
    pub explicit_probe: bool,
    pub structural_ceiling: StructuralProbeCeiling,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VerificationDescriptor {
    pub policy: VerificationPolicy,
    pub runtime_availability: &'static str,
    pub never_auto_enable: bool,
    pub probe_first_declared_model: bool,
    pub uses_get_models: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsageContractKind {
    Authoritative,
    LocalState,
    ExperimentalUnavailable,
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UsageDescriptor {
    pub catalog_availability: &'static str,
    pub contract: UsageContractKind,
    pub endpoint: Option<&'static str>,
    pub experimental: bool,
    pub automatic_sync: bool,
    pub authoritative_for_quota: bool,
    pub affects_inference_eligibility: bool,
    pub publishes_capability: bool,
    pub manual_calibration: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PricingDescriptor {
    pub availability: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ErrorCooldownDescriptor {
    pub parse_opencode_go_windows_on_429: bool,
    pub schedule_official_go_usage_after_429: bool,
    pub generic_provider_key_cooldown: bool,
    pub egress_ip_shared_free_cooldown: bool,
    pub inference_401_passthrough: bool,
    pub success_cost_state_free: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CardVerifyAction {
    NotApplicable,
    Optional,
    UnavailableNotImplemented,
    AvailableThenExplicitEnable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CardActionsDescriptor {
    pub persisted_enable_allowed: bool,
    pub enable_requires_verification: bool,
    pub managed_registration: bool,
    pub fetch_zen_models: bool,
    pub discover_models: bool,
    pub usage_refresh: bool,
    pub manual_usage_calibration: bool,
    pub connection_verify: CardVerifyAction,
    pub protocol_and_auth_immutable_after_create: bool,
    pub risk_acknowledgement: bool,
    pub protocol_probe: bool,
    pub catalog_refresh: bool,
}

mod sealed {
    pub trait Sealed {}
}

impl sealed::Sealed for OpenCodeGoAdapter {}
impl sealed::Sealed for ZenFreeAdapter {}
impl sealed::Sealed for CommandCodeGoatAdapter {}
impl sealed::Sealed for ScnetAdapter {}
impl sealed::Sealed for ConfigurableHttpAdapter {}

/// Static model-catalog capability contract. Sealed to the five concrete
/// adapter identities; not a plugin slot or runtime registry.
pub trait ModelCatalogAdapter: sealed::Sealed {
    fn model_catalog(plan: BuiltinPlan) -> ModelCatalogDescriptor;
}

/// Static inference routing capability contract.
pub trait InferenceAdapter: sealed::Sealed {
    fn inference(plan: BuiltinPlan) -> InferenceRoutingDescriptor;
}

/// Static protocol-probe capability contract. Request paths must not trial
/// billable inference.
pub trait ProtocolProbeAdapter: sealed::Sealed {
    fn protocol_probe(plan: BuiltinPlan) -> ProtocolProbeDescriptor;
}

/// Static connection-verification capability contract.
pub trait VerificationAdapter: sealed::Sealed {
    fn verification(plan: BuiltinPlan) -> VerificationDescriptor;
}

/// Static usage capability contract.
pub trait UsageAdapter: sealed::Sealed {
    fn usage(plan: BuiltinPlan) -> UsageDescriptor;
}

/// Static pricing capability contract.
pub trait PricingAdapter: sealed::Sealed {
    fn pricing(plan: BuiltinPlan) -> PricingDescriptor;
}

/// Static error/cooldown policy contract.
pub trait ErrorPolicyAdapter: sealed::Sealed {
    fn error_policy(plan: BuiltinPlan) -> ErrorCooldownDescriptor;
}

/// Static account-card capability contract.
pub trait CardCapabilities: sealed::Sealed {
    fn card_capabilities(plan: BuiltinPlan) -> CardActionsDescriptor;
}

impl ProviderCapabilities {
    /// Compose the eight sealed contracts from one concrete adapter. This is
    /// the only construction helper; callers must not re-match adapter kind
    /// per capability.
    pub fn compose<A>(_adapter: A, plan: BuiltinPlan) -> Self
    where
        A: ModelCatalogAdapter
            + InferenceAdapter
            + ProtocolProbeAdapter
            + VerificationAdapter
            + UsageAdapter
            + PricingAdapter
            + ErrorPolicyAdapter
            + CardCapabilities,
    {
        Self {
            model_catalog: A::model_catalog(plan),
            inference: A::inference(plan),
            protocol_probe: A::protocol_probe(plan),
            verification: A::verification(plan),
            usage: A::usage(plan),
            pricing: A::pricing(plan),
            error_cooldown: A::error_policy(plan),
            card_actions: A::card_capabilities(plan),
        }
    }
}

impl ProviderAdapterKind {
    /// Single registry-owned construction point. Adding a concrete adapter
    /// means implementing the eight contracts and one arm here.
    pub fn compose_capabilities(self, plan: BuiltinPlan) -> ProviderCapabilities {
        match self {
            Self::OpenCodeGo => ProviderCapabilities::compose(OpenCodeGoAdapter, plan),
            Self::ZenFree => ProviderCapabilities::compose(ZenFreeAdapter, plan),
            Self::CommandCodeGoat => ProviderCapabilities::compose(CommandCodeGoatAdapter, plan),
            Self::Scnet => ProviderCapabilities::compose(ScnetAdapter, plan),
            Self::ConfigurableHttp => ProviderCapabilities::compose(ConfigurableHttpAdapter, plan),
        }
    }
}

fn catalog_pricing(plan: BuiltinPlan) -> PricingDescriptor {
    PricingDescriptor {
        availability: plan.pricing_availability,
    }
}

impl ModelCatalogAdapter for OpenCodeGoAdapter {
    fn model_catalog(plan: BuiltinPlan) -> ModelCatalogDescriptor {
        ModelCatalogDescriptor {
            kind: ModelCatalogKind::BuiltinGoProtocolTable,
            catalog_source: plan.model_source,
            publishes_client_aliases: true,
            admin_explicit_refresh: false,
            overlays_declared_ids: false,
            snapshot_is_adapter_input_only: false,
        }
    }
}

impl InferenceAdapter for OpenCodeGoAdapter {
    fn inference(plan: BuiltinPlan) -> InferenceRoutingDescriptor {
        InferenceRoutingDescriptor {
            catalog_routable: plan.routable,
            production_inference: true,
            channel: Some(InferenceChannelKind::Go),
            credential_kind: plan.offering.credential_kind,
            quota_scope: plan.offering.quota_scope,
            auth: InferenceAuthDescriptor::OpenCodeProtocolDefault,
            follow_redirects: true,
            origin: InferenceOriginKind::ConfigUpstreamBase,
            loopback_test_seam_only: false,
        }
    }
}

impl ProtocolProbeAdapter for OpenCodeGoAdapter {
    fn protocol_probe(_plan: BuiltinPlan) -> ProtocolProbeDescriptor {
        ProtocolProbeDescriptor {
            request_path_may_trial: false,
            matrix: ProtocolMatrixKind::OpenCodeModelProtocols,
            unknown_zen_free_defaults_to_chat: false,
            fallback_priority: PROTOCOL_FALLBACK_CHAT_RESPONSES_MESSAGES,
            explicit_probe: true,
            structural_ceiling: StructuralProbeCeiling::OpenCodeConstructable,
        }
    }
}

impl VerificationAdapter for OpenCodeGoAdapter {
    fn verification(plan: BuiltinPlan) -> VerificationDescriptor {
        VerificationDescriptor {
            policy: plan.verification_policy,
            runtime_availability: plan.verification_runtime_availability,
            never_auto_enable: false,
            probe_first_declared_model: false,
            uses_get_models: false,
        }
    }
}

impl UsageAdapter for OpenCodeGoAdapter {
    fn usage(plan: BuiltinPlan) -> UsageDescriptor {
        UsageDescriptor {
            catalog_availability: plan.usage_availability,
            contract: UsageContractKind::Authoritative,
            endpoint: Some(crate::kernel::catalog::OPENCODE_GO_USAGE_URL),
            experimental: false,
            automatic_sync: true,
            authoritative_for_quota: true,
            affects_inference_eligibility: false,
            publishes_capability: true,
            manual_calibration: plan.manual_usage_calibration,
        }
    }
}

impl PricingAdapter for OpenCodeGoAdapter {
    fn pricing(plan: BuiltinPlan) -> PricingDescriptor {
        catalog_pricing(plan)
    }
}

impl ErrorPolicyAdapter for OpenCodeGoAdapter {
    fn error_policy(_plan: BuiltinPlan) -> ErrorCooldownDescriptor {
        ErrorCooldownDescriptor {
            parse_opencode_go_windows_on_429: true,
            schedule_official_go_usage_after_429: true,
            generic_provider_key_cooldown: false,
            egress_ip_shared_free_cooldown: false,
            inference_401_passthrough: false,
            success_cost_state_free: false,
        }
    }
}

impl CardCapabilities for OpenCodeGoAdapter {
    fn card_capabilities(plan: BuiltinPlan) -> CardActionsDescriptor {
        CardActionsDescriptor {
            persisted_enable_allowed: plan.routable,
            enable_requires_verification: false,
            managed_registration: plan.managed_registration,
            fetch_zen_models: false,
            discover_models: false,
            usage_refresh: true,
            manual_usage_calibration: plan.manual_usage_calibration,
            connection_verify: CardVerifyAction::Optional,
            protocol_and_auth_immutable_after_create: false,
            risk_acknowledgement: plan.risk_notice.is_some(),
            protocol_probe: true,
            catalog_refresh: false,
        }
    }
}

impl ModelCatalogAdapter for ZenFreeAdapter {
    fn model_catalog(plan: BuiltinPlan) -> ModelCatalogDescriptor {
        ModelCatalogDescriptor {
            kind: ModelCatalogKind::ZenFreePersistedSnapshot,
            catalog_source: plan.model_source,
            publishes_client_aliases: true,
            admin_explicit_refresh: true,
            overlays_declared_ids: false,
            snapshot_is_adapter_input_only: false,
        }
    }
}

impl InferenceAdapter for ZenFreeAdapter {
    fn inference(plan: BuiltinPlan) -> InferenceRoutingDescriptor {
        InferenceRoutingDescriptor {
            catalog_routable: plan.routable,
            production_inference: true,
            channel: Some(InferenceChannelKind::Free),
            credential_kind: plan.offering.credential_kind,
            quota_scope: plan.offering.quota_scope,
            auth: InferenceAuthDescriptor::None,
            follow_redirects: true,
            origin: InferenceOriginKind::DerivedZenBase,
            loopback_test_seam_only: false,
        }
    }
}

impl ProtocolProbeAdapter for ZenFreeAdapter {
    fn protocol_probe(_plan: BuiltinPlan) -> ProtocolProbeDescriptor {
        ProtocolProbeDescriptor {
            request_path_may_trial: false,
            matrix: ProtocolMatrixKind::OpenCodeModelProtocols,
            unknown_zen_free_defaults_to_chat: true,
            fallback_priority: PROTOCOL_FALLBACK_CHAT_RESPONSES_MESSAGES,
            explicit_probe: true,
            structural_ceiling: StructuralProbeCeiling::ZenFreeConstructable,
        }
    }
}

impl VerificationAdapter for ZenFreeAdapter {
    fn verification(plan: BuiltinPlan) -> VerificationDescriptor {
        VerificationDescriptor {
            policy: plan.verification_policy,
            runtime_availability: plan.verification_runtime_availability,
            never_auto_enable: false,
            probe_first_declared_model: false,
            uses_get_models: false,
        }
    }
}

impl UsageAdapter for ZenFreeAdapter {
    fn usage(plan: BuiltinPlan) -> UsageDescriptor {
        UsageDescriptor {
            catalog_availability: plan.usage_availability,
            contract: UsageContractKind::LocalState,
            endpoint: None,
            experimental: false,
            automatic_sync: false,
            authoritative_for_quota: false,
            affects_inference_eligibility: false,
            publishes_capability: true,
            manual_calibration: plan.manual_usage_calibration,
        }
    }
}

impl PricingAdapter for ZenFreeAdapter {
    fn pricing(plan: BuiltinPlan) -> PricingDescriptor {
        catalog_pricing(plan)
    }
}

impl ErrorPolicyAdapter for ZenFreeAdapter {
    fn error_policy(_plan: BuiltinPlan) -> ErrorCooldownDescriptor {
        ErrorCooldownDescriptor {
            parse_opencode_go_windows_on_429: false,
            schedule_official_go_usage_after_429: false,
            generic_provider_key_cooldown: false,
            egress_ip_shared_free_cooldown: true,
            inference_401_passthrough: true,
            success_cost_state_free: true,
        }
    }
}

impl CardCapabilities for ZenFreeAdapter {
    fn card_capabilities(plan: BuiltinPlan) -> CardActionsDescriptor {
        CardActionsDescriptor {
            persisted_enable_allowed: plan.routable,
            enable_requires_verification: false,
            managed_registration: plan.managed_registration,
            fetch_zen_models: true,
            discover_models: false,
            usage_refresh: false,
            manual_usage_calibration: plan.manual_usage_calibration,
            connection_verify: CardVerifyAction::NotApplicable,
            protocol_and_auth_immutable_after_create: false,
            risk_acknowledgement: plan.risk_notice.is_some(),
            protocol_probe: true,
            catalog_refresh: true,
        }
    }
}

impl ModelCatalogAdapter for CommandCodeGoatAdapter {
    fn model_catalog(plan: BuiltinPlan) -> ModelCatalogDescriptor {
        ModelCatalogDescriptor {
            kind: ModelCatalogKind::BuiltinCommandCodeProtocolTable,
            catalog_source: plan.model_source,
            publishes_client_aliases: false,
            admin_explicit_refresh: false,
            overlays_declared_ids: false,
            snapshot_is_adapter_input_only: false,
        }
    }
}

impl InferenceAdapter for CommandCodeGoatAdapter {
    fn inference(plan: BuiltinPlan) -> InferenceRoutingDescriptor {
        InferenceRoutingDescriptor {
            catalog_routable: plan.routable,
            production_inference: false,
            channel: Some(InferenceChannelKind::Go),
            credential_kind: plan.offering.credential_kind,
            quota_scope: plan.offering.quota_scope,
            auth: InferenceAuthDescriptor::Bearer,
            follow_redirects: false,
            origin: InferenceOriginKind::OfficialFixed,
            loopback_test_seam_only: true,
        }
    }
}

impl ProtocolProbeAdapter for CommandCodeGoatAdapter {
    fn protocol_probe(_plan: BuiltinPlan) -> ProtocolProbeDescriptor {
        ProtocolProbeDescriptor {
            request_path_may_trial: false,
            matrix: ProtocolMatrixKind::CommandCodeNative,
            unknown_zen_free_defaults_to_chat: false,
            fallback_priority: PROTOCOL_FALLBACK_CHAT_MESSAGES,
            explicit_probe: false,
            structural_ceiling: StructuralProbeCeiling::Unavailable,
        }
    }
}

impl VerificationAdapter for CommandCodeGoatAdapter {
    fn verification(plan: BuiltinPlan) -> VerificationDescriptor {
        VerificationDescriptor {
            policy: plan.verification_policy,
            runtime_availability: plan.verification_runtime_availability,
            never_auto_enable: true,
            probe_first_declared_model: false,
            uses_get_models: false,
        }
    }
}

impl UsageAdapter for CommandCodeGoatAdapter {
    fn usage(plan: BuiltinPlan) -> UsageDescriptor {
        UsageDescriptor {
            catalog_availability: plan.usage_availability,
            contract: UsageContractKind::ExperimentalUnavailable,
            endpoint: None,
            experimental: true,
            automatic_sync: false,
            authoritative_for_quota: false,
            affects_inference_eligibility: false,
            publishes_capability: true,
            manual_calibration: plan.manual_usage_calibration,
        }
    }
}

impl PricingAdapter for CommandCodeGoatAdapter {
    fn pricing(plan: BuiltinPlan) -> PricingDescriptor {
        catalog_pricing(plan)
    }
}

impl ErrorPolicyAdapter for CommandCodeGoatAdapter {
    fn error_policy(_plan: BuiltinPlan) -> ErrorCooldownDescriptor {
        ErrorCooldownDescriptor {
            parse_opencode_go_windows_on_429: false,
            schedule_official_go_usage_after_429: false,
            generic_provider_key_cooldown: true,
            egress_ip_shared_free_cooldown: false,
            inference_401_passthrough: false,
            success_cost_state_free: false,
        }
    }
}

impl CardCapabilities for CommandCodeGoatAdapter {
    fn card_capabilities(plan: BuiltinPlan) -> CardActionsDescriptor {
        CardActionsDescriptor {
            persisted_enable_allowed: plan.routable,
            enable_requires_verification: true,
            managed_registration: plan.managed_registration,
            fetch_zen_models: false,
            discover_models: false,
            usage_refresh: false,
            manual_usage_calibration: plan.manual_usage_calibration,
            connection_verify: CardVerifyAction::UnavailableNotImplemented,
            protocol_and_auth_immutable_after_create: false,
            risk_acknowledgement: plan.risk_notice.is_some(),
            protocol_probe: false,
            catalog_refresh: false,
        }
    }
}

impl ModelCatalogAdapter for ScnetAdapter {
    fn model_catalog(plan: BuiltinPlan) -> ModelCatalogDescriptor {
        ModelCatalogDescriptor {
            kind: ModelCatalogKind::OfficialTokenPlanUsableModels,
            catalog_source: plan.model_source,
            publishes_client_aliases: false,
            admin_explicit_refresh: false,
            overlays_declared_ids: false,
            snapshot_is_adapter_input_only: true,
        }
    }
}

impl InferenceAdapter for ScnetAdapter {
    fn inference(plan: BuiltinPlan) -> InferenceRoutingDescriptor {
        InferenceRoutingDescriptor {
            catalog_routable: plan.routable,
            production_inference: false,
            channel: Some(InferenceChannelKind::Go),
            credential_kind: plan.offering.credential_kind,
            quota_scope: plan.offering.quota_scope,
            auth: InferenceAuthDescriptor::Bearer,
            follow_redirects: false,
            origin: InferenceOriginKind::None,
            loopback_test_seam_only: false,
        }
    }
}

impl ProtocolProbeAdapter for ScnetAdapter {
    fn protocol_probe(_plan: BuiltinPlan) -> ProtocolProbeDescriptor {
        ProtocolProbeDescriptor {
            request_path_may_trial: false,
            matrix: ProtocolMatrixKind::DocumentedChatAndMessages,
            unknown_zen_free_defaults_to_chat: false,
            fallback_priority: PROTOCOL_FALLBACK_CHAT_MESSAGES,
            explicit_probe: false,
            structural_ceiling: StructuralProbeCeiling::Unavailable,
        }
    }
}

impl VerificationAdapter for ScnetAdapter {
    fn verification(plan: BuiltinPlan) -> VerificationDescriptor {
        VerificationDescriptor {
            policy: plan.verification_policy,
            runtime_availability: plan.verification_runtime_availability,
            never_auto_enable: true,
            probe_first_declared_model: false,
            uses_get_models: false,
        }
    }
}

impl UsageAdapter for ScnetAdapter {
    fn usage(plan: BuiltinPlan) -> UsageDescriptor {
        UsageDescriptor {
            catalog_availability: plan.usage_availability,
            contract: UsageContractKind::Unavailable,
            endpoint: None,
            experimental: false,
            automatic_sync: false,
            authoritative_for_quota: false,
            affects_inference_eligibility: false,
            publishes_capability: false,
            manual_calibration: plan.manual_usage_calibration,
        }
    }
}

impl PricingAdapter for ScnetAdapter {
    fn pricing(plan: BuiltinPlan) -> PricingDescriptor {
        catalog_pricing(plan)
    }
}

impl ErrorPolicyAdapter for ScnetAdapter {
    fn error_policy(_plan: BuiltinPlan) -> ErrorCooldownDescriptor {
        ErrorCooldownDescriptor {
            parse_opencode_go_windows_on_429: false,
            schedule_official_go_usage_after_429: false,
            generic_provider_key_cooldown: false,
            egress_ip_shared_free_cooldown: false,
            inference_401_passthrough: false,
            success_cost_state_free: false,
        }
    }
}

impl CardCapabilities for ScnetAdapter {
    fn card_capabilities(plan: BuiltinPlan) -> CardActionsDescriptor {
        CardActionsDescriptor {
            persisted_enable_allowed: plan.routable,
            enable_requires_verification: true,
            managed_registration: plan.managed_registration,
            fetch_zen_models: false,
            discover_models: false,
            usage_refresh: false,
            manual_usage_calibration: plan.manual_usage_calibration,
            connection_verify: CardVerifyAction::UnavailableNotImplemented,
            protocol_and_auth_immutable_after_create: false,
            risk_acknowledgement: plan.risk_notice.is_some(),
            protocol_probe: false,
            catalog_refresh: false,
        }
    }
}

impl ModelCatalogAdapter for ConfigurableHttpAdapter {
    fn model_catalog(plan: BuiltinPlan) -> ModelCatalogDescriptor {
        ModelCatalogDescriptor {
            kind: ModelCatalogKind::AccountDeclaredCapabilities,
            catalog_source: plan.model_source,
            publishes_client_aliases: false,
            admin_explicit_refresh: false,
            overlays_declared_ids: true,
            snapshot_is_adapter_input_only: false,
        }
    }
}

impl InferenceAdapter for ConfigurableHttpAdapter {
    fn inference(plan: BuiltinPlan) -> InferenceRoutingDescriptor {
        InferenceRoutingDescriptor {
            catalog_routable: plan.routable,
            production_inference: true,
            channel: Some(InferenceChannelKind::Go),
            credential_kind: plan.offering.credential_kind,
            quota_scope: plan.offering.quota_scope,
            auth: InferenceAuthDescriptor::ConfigurableBearerOrXApiKey,
            follow_redirects: false,
            origin: InferenceOriginKind::AccountConfigured,
            loopback_test_seam_only: false,
        }
    }
}

impl ProtocolProbeAdapter for ConfigurableHttpAdapter {
    fn protocol_probe(_plan: BuiltinPlan) -> ProtocolProbeDescriptor {
        ProtocolProbeDescriptor {
            request_path_may_trial: false,
            matrix: ProtocolMatrixKind::AccountDeclaredProtocol,
            unknown_zen_free_defaults_to_chat: false,
            fallback_priority: PROTOCOL_FALLBACK_CHAT_RESPONSES_MESSAGES,
            explicit_probe: true,
            structural_ceiling: StructuralProbeCeiling::AccountDeclared,
        }
    }
}

impl VerificationAdapter for ConfigurableHttpAdapter {
    fn verification(plan: BuiltinPlan) -> VerificationDescriptor {
        VerificationDescriptor {
            policy: plan.verification_policy,
            runtime_availability: plan.verification_runtime_availability,
            never_auto_enable: true,
            probe_first_declared_model: true,
            uses_get_models: false,
        }
    }
}

impl UsageAdapter for ConfigurableHttpAdapter {
    fn usage(plan: BuiltinPlan) -> UsageDescriptor {
        UsageDescriptor {
            catalog_availability: plan.usage_availability,
            contract: UsageContractKind::Unavailable,
            endpoint: None,
            experimental: false,
            automatic_sync: false,
            authoritative_for_quota: false,
            affects_inference_eligibility: false,
            publishes_capability: false,
            manual_calibration: plan.manual_usage_calibration,
        }
    }
}

impl PricingAdapter for ConfigurableHttpAdapter {
    fn pricing(plan: BuiltinPlan) -> PricingDescriptor {
        catalog_pricing(plan)
    }
}

impl ErrorPolicyAdapter for ConfigurableHttpAdapter {
    fn error_policy(_plan: BuiltinPlan) -> ErrorCooldownDescriptor {
        ErrorCooldownDescriptor {
            parse_opencode_go_windows_on_429: false,
            schedule_official_go_usage_after_429: false,
            generic_provider_key_cooldown: true,
            egress_ip_shared_free_cooldown: false,
            inference_401_passthrough: false,
            success_cost_state_free: false,
        }
    }
}

impl CardCapabilities for ConfigurableHttpAdapter {
    fn card_capabilities(plan: BuiltinPlan) -> CardActionsDescriptor {
        CardActionsDescriptor {
            persisted_enable_allowed: plan.routable,
            enable_requires_verification: true,
            managed_registration: plan.managed_registration,
            fetch_zen_models: false,
            discover_models: true,
            usage_refresh: false,
            manual_usage_calibration: plan.manual_usage_calibration,
            connection_verify: CardVerifyAction::AvailableThenExplicitEnable,
            protocol_and_auth_immutable_after_create: true,
            risk_acknowledgement: plan.risk_notice.is_some(),
            protocol_probe: true,
            catalog_refresh: true,
        }
    }
}

pub fn acknowledgement_content_hash(body: &str) -> String {
    format!("{:x}", Sha256::digest(body.as_bytes()))
}

pub fn default_verification_status(plan: BuiltinPlan) -> ConnectionVerificationStatus {
    match plan.verification_policy {
        VerificationPolicy::NotRequired => ConnectionVerificationStatus::NotRequired,
        VerificationPolicy::Required => ConnectionVerificationStatus::Pending,
    }
}

/// Catalog-backed enablement capability. Only `routable` offerings may persist
/// `enabled=true`. Unknown offerings fail closed.
pub const fn plan_allows_enablement(plan: BuiltinPlan) -> bool {
    plan.routable
}

pub fn offering_allows_enablement(provider_id: &str, offering_id: &str) -> bool {
    builtin_plan(provider_id, offering_id).is_some_and(plan_allows_enablement)
}

/// Reject `enabled=true` for catalogued-but-unroutable offerings. Disabled
/// drafts skip the check so they can still be created and edited.
pub fn ensure_enabled_offering_is_routable(
    provider_id: &str,
    offering_id: &str,
    enabled: bool,
) -> Result<(), ProviderBindingError> {
    if !enabled {
        return Ok(());
    }
    ensure_offering_can_enable(provider_id, offering_id)
}

pub fn ensure_offering_can_enable(
    provider_id: &str,
    offering_id: &str,
) -> Result<(), ProviderBindingError> {
    match builtin_plan(provider_id, offering_id) {
        Some(plan) if plan_allows_enablement(plan) => Ok(()),
        Some(plan) => Err(ProviderBindingError::EnablementNotRoutable {
            provider_id: plan.offering.provider_id,
            offering_id: plan.offering.offering_id,
            display_name: plan.display_name,
        }),
        None => Err(ProviderBindingError::UnknownOffering {
            provider_id: provider_id.to_string(),
            offering_id: offering_id.to_string(),
        }),
    }
}

pub fn plan_requires_custom_config(plan: BuiltinPlan) -> bool {
    matches!(
        ProviderAdapterKind::from_offering(plan.offering.provider_id, plan.offering.offering_id),
        Some(ProviderAdapterKind::ConfigurableHttp)
    )
}

pub fn validate_plan_key(plan: BuiltinPlan, key: &str) -> Result<(), ProviderBindingError> {
    if plan.offering.credential_kind == CredentialKind::None {
        return Ok(());
    }
    let trimmed = key.trim();
    if trimmed.is_empty() {
        return Err(ProviderBindingError::KeyRequired);
    }
    if let Some(prefix) = plan.key_prefix {
        if !trimmed.starts_with(prefix) {
            return Err(ProviderBindingError::KeyPrefixMismatch {
                provider_id: plan.offering.provider_id.to_string(),
                offering_id: plan.offering.offering_id.to_string(),
                prefix: prefix.to_string(),
            });
        }
    }
    Ok(())
}

/// Structured Custom URL host taken from [`reqwest::Url::host`], not `host_str`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CustomUrlHost {
    Ip(IpAddr),
    Domain(String),
}

/// Syntactic Custom URL inspection shared by persistence and HTTP joining.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustomUrlTarget {
    pub host: CustomUrlHost,
}

/// Syntactic Custom base-URL gate. Administrators explicitly trust Custom
/// destinations, so any http/https origin is accepted. Credentials and
/// non-HTTP(S) schemes stay rejected; DNS / IP / hostname policy is not applied.
pub fn validate_custom_base_url(value: &str) -> Result<String, ProviderBindingError> {
    let value = value.trim();
    if value.is_empty() {
        return Err(ProviderBindingError::InvalidCustomBaseUrl(
            "base URL is required".to_string(),
        ));
    }
    if value.len() > 2048 {
        return Err(ProviderBindingError::InvalidCustomBaseUrl(
            "base URL is too long".to_string(),
        ));
    }
    let parsed = reqwest::Url::parse(value).map_err(|error| {
        ProviderBindingError::InvalidCustomBaseUrl(format!("invalid base URL: {error}"))
    })?;
    inspect_custom_url(&parsed)?;
    if parsed.query().is_some() || parsed.fragment().is_some() {
        return Err(ProviderBindingError::InvalidCustomBaseUrl(
            "base URL must not include a query or fragment".to_string(),
        ));
    }
    Ok(parsed.as_str().trim_end_matches('/').to_string())
}

/// Inspect scheme, credentials, and host of a Custom URL.
///
/// Uses [`reqwest::Url::host`] so bracketed IPv6 and IPv4-mapped literals are
/// the parser's IP variants. `host_str().parse::<IpAddr>()` treats `[::ffff:…]`
/// as a hostname and is the bypass this function exists to close.
pub fn inspect_custom_url(parsed: &reqwest::Url) -> Result<CustomUrlTarget, ProviderBindingError> {
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(ProviderBindingError::InvalidCustomBaseUrl(
            "base URL must use http or https".to_string(),
        ));
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(ProviderBindingError::InvalidCustomBaseUrl(
            "base URL must not include credentials".to_string(),
        ));
    }
    Ok(CustomUrlTarget {
        host: custom_url_host(parsed)?,
    })
}

fn custom_url_host(parsed: &reqwest::Url) -> Result<CustomUrlHost, ProviderBindingError> {
    let host = parsed.host().ok_or_else(|| {
        ProviderBindingError::InvalidCustomBaseUrl("base URL must include a host".to_string())
    })?;
    // `url::Host` is not a direct dependency (manifests stay frozen). IPv6
    // Display includes brackets; strip them to recover the parsed `Ipv6Addr`.
    let rendered = host.to_string();
    if let Some(inside) = rendered
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
    {
        let ip = inside.parse::<Ipv6Addr>().map_err(|_| {
            ProviderBindingError::InvalidCustomBaseUrl("base URL IPv6 host is invalid".to_string())
        })?;
        return Ok(CustomUrlHost::Ip(IpAddr::V6(ip)));
    }
    if let Ok(ip) = rendered.parse::<Ipv4Addr>() {
        return Ok(CustomUrlHost::Ip(IpAddr::V4(ip)));
    }
    Ok(CustomUrlHost::Domain(rendered.to_ascii_lowercase()))
}

pub fn validate_custom_model_id(model_id: &str) -> Result<String, ProviderBindingError> {
    let trimmed = model_id.trim();
    if trimmed.is_empty() {
        return Err(ProviderBindingError::InvalidModelId(
            "model id is required".to_string(),
        ));
    }
    if trimmed.chars().count() > 200 {
        return Err(ProviderBindingError::InvalidModelId(
            "model id is too long".to_string(),
        ));
    }
    if trimmed.contains('\0') || trimmed.chars().any(char::is_control) {
        return Err(ProviderBindingError::InvalidModelId(
            "model id must not contain control characters".to_string(),
        ));
    }
    Ok(trimmed.to_string())
}

pub fn validate_account_binding(
    account_id: &str,
    provider_id: &str,
    offering_id: &str,
    credential_kind: CredentialKind,
    quota_scope: QuotaScope,
) -> Result<(), ProviderBindingError> {
    let offering = builtin_offering(provider_id, offering_id).ok_or_else(|| {
        ProviderBindingError::UnknownOffering {
            provider_id: provider_id.to_string(),
            offering_id: offering_id.to_string(),
        }
    })?;
    if offering.credential_kind != credential_kind || offering.quota_scope != quota_scope {
        return Err(ProviderBindingError::BindingMismatch {
            provider_id: provider_id.to_string(),
            offering_id: offering_id.to_string(),
        });
    }
    match offering.singleton_account_id {
        Some(singleton) if account_id != singleton => {
            Err(ProviderBindingError::SingletonAccountRequired(singleton))
        }
        None if account_id == ZEN_FREE_ACCOUNT_ID => {
            Err(ProviderBindingError::ReservedAccountId(ZEN_FREE_ACCOUNT_ID))
        }
        _ => Ok(()),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderBindingError {
    UnknownOffering {
        provider_id: String,
        offering_id: String,
    },
    UnknownCredentialKind(String),
    UnknownQuotaScope(String),
    BindingMismatch {
        provider_id: String,
        offering_id: String,
    },
    SingletonAccountRequired(&'static str),
    ReservedAccountId(&'static str),
    UnknownVerificationStatus(String),
    UnknownUpstreamProtocol(String),
    UnknownAuthScheme(String),
    KeyRequired,
    KeyPrefixMismatch {
        provider_id: String,
        offering_id: String,
        prefix: String,
    },
    InvalidCustomBaseUrl(String),
    InvalidModelId(String),
    EnablementNotRoutable {
        provider_id: &'static str,
        offering_id: &'static str,
        display_name: &'static str,
    },
}

impl fmt::Display for ProviderBindingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownOffering {
                provider_id,
                offering_id,
            } => write!(f, "unknown provider offering `{provider_id}/{offering_id}`"),
            Self::UnknownCredentialKind(value) => {
                write!(f, "unknown credential kind `{value}`")
            }
            Self::UnknownQuotaScope(value) => write!(f, "unknown quota scope `{value}`"),
            Self::BindingMismatch {
                provider_id,
                offering_id,
            } => write!(
                f,
                "provider binding does not match `{provider_id}/{offering_id}`"
            ),
            Self::SingletonAccountRequired(id) => {
                write!(f, "provider offering requires singleton account `{id}`")
            }
            Self::ReservedAccountId(id) => write!(f, "account id `{id}` is reserved"),
            Self::UnknownVerificationStatus(value) => {
                write!(f, "unknown verification status `{value}`")
            }
            Self::UnknownUpstreamProtocol(value) => {
                write!(f, "unknown upstream protocol `{value}`")
            }
            Self::UnknownAuthScheme(value) => write!(f, "unknown auth scheme `{value}`"),
            Self::KeyRequired => write!(f, "key is required"),
            Self::KeyPrefixMismatch {
                provider_id,
                offering_id,
                prefix,
            } => write!(
                f,
                "provider offering `{provider_id}/{offering_id}` requires key prefix `{prefix}`"
            ),
            Self::InvalidCustomBaseUrl(message) | Self::InvalidModelId(message) => {
                f.write_str(message)
            }
            Self::EnablementNotRoutable { display_name, .. } => write!(
                f,
                "{display_name} is catalogued but is not routable in this release"
            ),
        }
    }
}

impl std::error::Error for ProviderBindingError {}

impl From<CatalogParseError> for ProviderBindingError {
    fn from(error: CatalogParseError) -> Self {
        match error {
            CatalogParseError::UnknownCredentialKind(value) => Self::UnknownCredentialKind(value),
            CatalogParseError::UnknownQuotaScope(value) => Self::UnknownQuotaScope(value),
            CatalogParseError::UnknownUpstreamProtocol(value) => {
                Self::UnknownUpstreamProtocol(value)
            }
            CatalogParseError::UnknownAuthScheme(value) => Self::UnknownAuthScheme(value),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuotaWindow {
    pub account_id: String,
    pub window_kind: String,
    pub used: f64,
    pub limit_value: Option<f64>,
    pub started_at: Option<DateTime<Utc>>,
    pub resets_at: Option<DateTime<Utc>>,
    pub calibration_offset: f64,
    pub unit: String,
    pub source: String,
    pub observed_at: Option<DateTime<Utc>>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CreditBalance {
    pub account_id: String,
    pub balance_kind: String,
    pub amount: f64,
    pub unit: String,
    pub source: String,
    pub observed_at: Option<DateTime<Utc>>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderPricingSnapshot {
    pub provider_id: String,
    pub offering_id: String,
    pub revision: String,
    pub activated_at: String,
    pub document_updated_at: Option<String>,
    pub source_url: String,
    pub content_hash: String,
    pub snapshot_json: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderUsageSyncState {
    pub account_id: String,
    pub last_success_at: Option<DateTime<Utc>>,
    pub last_attempt_at: Option<DateTime<Utc>>,
    pub next_eligible_at: Option<DateTime<Utc>>,
    pub failure_streak: i64,
    pub last_expedited_at: Option<DateTime<Utc>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_parse_errors_map_to_provider_binding_errors() {
        assert!(matches!(
            ProviderBindingError::from(CredentialKind::try_from("cookie").unwrap_err()),
            ProviderBindingError::UnknownCredentialKind(value) if value == "cookie"
        ));
        assert!(matches!(
            ProviderBindingError::from(QuotaScope::try_from("account").unwrap_err()),
            ProviderBindingError::UnknownQuotaScope(value) if value == "account"
        ));
        assert!(matches!(
            ProviderBindingError::from(UpstreamProtocolKind::try_from("gemini").unwrap_err()),
            ProviderBindingError::UnknownUpstreamProtocol(value) if value == "gemini"
        ));
        assert!(matches!(
            ProviderBindingError::from(UpstreamAuthScheme::try_from("basic").unwrap_err()),
            ProviderBindingError::UnknownAuthScheme(value) if value == "basic"
        ));
    }

    #[test]
    fn builtin_pairs_derive_credential_and_quota_scope() {
        let goat = builtin_offering(COMMAND_CODE_PROVIDER_ID, GOAT_OFFERING_ID).unwrap();
        assert_eq!(goat.credential_kind, CredentialKind::ApiKey);
        assert_eq!(goat.quota_scope, QuotaScope::Key);

        let free =
            builtin_offering(OPENCODE_ZEN_FREE_PROVIDER_ID, ANONYMOUS_FREE_OFFERING_ID).unwrap();
        assert_eq!(free.credential_kind, CredentialKind::None);
        assert_eq!(free.quota_scope, QuotaScope::EgressIp);
        assert_eq!(free.singleton_account_id, Some(ZEN_FREE_ACCOUNT_ID));
    }

    #[test]
    fn singleton_and_pair_validation_is_fail_closed() {
        assert!(
            validate_account_binding(
                "account-1",
                OPENCODE_PROVIDER_ID,
                GO_OFFERING_ID,
                CredentialKind::ApiKey,
                QuotaScope::Key,
            )
            .is_ok()
        );
        assert!(
            validate_account_binding(
                "account-1",
                OPENCODE_ZEN_FREE_PROVIDER_ID,
                ANONYMOUS_FREE_OFFERING_ID,
                CredentialKind::None,
                QuotaScope::EgressIp,
            )
            .is_err()
        );
        assert!(
            validate_account_binding(
                ZEN_FREE_ACCOUNT_ID,
                OPENCODE_PROVIDER_ID,
                GO_OFFERING_ID,
                CredentialKind::ApiKey,
                QuotaScope::Key,
            )
            .is_err()
        );
        assert!(
            validate_account_binding(
                "account-1",
                "unknown-provider",
                "unknown-offering",
                CredentialKind::ApiKey,
                QuotaScope::Key,
            )
            .is_err()
        );
    }

    #[test]
    fn catalog_hardcodes_plans_and_keeps_unverified_offerings_unroutable() {
        assert_eq!(BUILTIN_PLANS.len(), 7);
        let goat = builtin_plan(COMMAND_CODE_PROVIDER_ID, GOAT_OFFERING_ID).unwrap();
        assert!(!goat.routable);
        assert_eq!(goat.verification_policy, VerificationPolicy::Required);
        assert_eq!(goat.verification_runtime_availability, "unavailable");
        assert_eq!(goat.creation_availability, CreationAvailability::Available);
        assert_eq!(goat.pricing_availability, "unavailable");
        assert_eq!(goat.usage_availability, "unavailable");
        assert_eq!(goat.auth_schemes, &BEARER_AUTH);
        assert_eq!(goat.upstream_protocols, &GOAT_PROTOCOLS);
        assert!(
            !goat
                .upstream_protocols
                .contains(&UpstreamProtocolKind::Responses)
        );
        assert_eq!(goat.model_source, "builtin_command_code_protocol_table");
        assert_eq!(
            COMMAND_CODE_GOAT_BASE_URL,
            "https://api.commandcode.ai/provider/v1"
        );
        assert_eq!(COMMAND_CODE_GOAT_HOST, "api.commandcode.ai");
        assert_eq!(COMMAND_CODE_GOAT_CHAT_COMPLETIONS_PATH, "/chat/completions");
        assert_eq!(COMMAND_CODE_GOAT_MESSAGES_PATH, "/messages");
        assert_eq!(COMMAND_CODE_GOAT_MODELS_PATH, "/models");
        assert_eq!(
            COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM,
            "deepseek/deepseek-v4-flash"
        );
        assert!(is_command_code_goat(
            COMMAND_CODE_PROVIDER_ID,
            GOAT_OFFERING_ID
        ));
        assert!(!is_command_code_goat(OPENCODE_PROVIDER_ID, GO_OFFERING_ID));

        let basic = builtin_plan(SCNET_PROVIDER_ID, SCNET_TOKEN_PLAN_BASIC_OFFERING_ID).unwrap();
        assert!(!basic.routable);
        assert_eq!(
            basic.risk_notice.unwrap().acknowledgement_id,
            SCNET_RISK_ACKNOWLEDGEMENT_ID
        );
        assert!(
            validate_plan_key(basic, "sk-live-not-token")
                .unwrap_err()
                .to_string()
                .contains(SCNET_TOKEN_PLAN_KEY_PREFIX)
        );
        assert!(validate_plan_key(basic, "sk-tp-live").is_ok());

        let custom = builtin_plan(CUSTOM_PROVIDER_ID, CUSTOM_API_OFFERING_ID).unwrap();
        assert!(custom.routable);
        assert_eq!(custom.verification_runtime_availability, "available");
        assert_eq!(custom.verification_policy, VerificationPolicy::Required);
        assert_eq!(custom.pricing_availability, "unpriced");
        assert_eq!(custom.usage_availability, "unavailable");
        assert!(plan_requires_custom_config(custom));
        assert!(is_custom_api(CUSTOM_PROVIDER_ID, CUSTOM_API_OFFERING_ID));
        assert!(!is_custom_api(OPENCODE_PROVIDER_ID, GO_OFFERING_ID));

        let go = builtin_plan(OPENCODE_PROVIDER_ID, GO_OFFERING_ID).unwrap();
        assert!(go.routable);
        assert_eq!(
            default_verification_status(go),
            ConnectionVerificationStatus::NotRequired
        );
    }

    #[test]
    fn catalog_enablement_gate_is_fail_closed_for_unroutable_plans() {
        for plan in BUILTIN_PLANS {
            let provider_id = plan.offering.provider_id;
            let offering_id = plan.offering.offering_id;
            assert_eq!(
                plan_allows_enablement(plan),
                plan.routable,
                "{provider_id}/{offering_id}"
            );
            assert_eq!(
                offering_allows_enablement(provider_id, offering_id),
                plan.routable,
                "{provider_id}/{offering_id}"
            );
            assert!(
                ensure_enabled_offering_is_routable(provider_id, offering_id, false).is_ok(),
                "disabled drafts must stay writable: {provider_id}/{offering_id}"
            );
            let enabled = ensure_enabled_offering_is_routable(provider_id, offering_id, true);
            if plan.routable {
                enabled.expect("routable offerings may enable");
                ensure_offering_can_enable(provider_id, offering_id).unwrap();
            } else {
                let error = enabled.expect_err("unroutable offerings must reject enabled=true");
                assert!(
                    matches!(
                        error,
                        ProviderBindingError::EnablementNotRoutable {
                            provider_id: rejected_provider,
                            offering_id: rejected_offering,
                            display_name,
                        } if rejected_provider == provider_id
                            && rejected_offering == offering_id
                            && display_name == plan.display_name
                    ),
                    "{error:?}"
                );
                assert!(error.to_string().contains("not routable"), "{}", error);
            }
        }
        assert!(!offering_allows_enablement(
            "unknown-provider",
            "unknown-offering"
        ));
        assert!(matches!(
            ensure_offering_can_enable("unknown-provider", "unknown-offering"),
            Err(ProviderBindingError::UnknownOffering { .. })
        ));
        let zen = builtin_plan(OPENCODE_ZEN_FREE_PROVIDER_ID, ANONYMOUS_FREE_OFFERING_ID).unwrap();
        assert!(plan_allows_enablement(zen));
        let go = builtin_plan(OPENCODE_PROVIDER_ID, GO_OFFERING_ID).unwrap();
        assert!(plan_allows_enablement(go));
    }

    #[test]
    fn custom_base_url_trusts_administrator_http_origins_and_rejects_credentials() {
        assert!(validate_custom_base_url("https://api.example.com/v1").is_ok());
        assert!(validate_custom_base_url("http://127.0.0.1:8080/v1").is_ok());
        assert!(validate_custom_base_url("http://localhost:3000").is_ok());
        assert!(validate_custom_base_url("http://app.localhost/v1").is_ok());
        assert!(validate_custom_base_url("http://api.example.com/v1").is_ok());
        assert!(validate_custom_base_url("https://192.168.1.8/v1").is_ok());
        assert!(validate_custom_base_url("http://10.0.0.1:9000/v1").is_ok());
        assert!(validate_custom_base_url("https://169.254.169.254/latest").is_ok());
        assert!(validate_custom_base_url("http://metadata.google.internal/").is_ok());
        assert!(validate_custom_base_url("https://[::ffff:169.254.169.254]/").is_ok());
        assert!(validate_custom_base_url("https://[2001:db8::1]/v1").is_ok());
        assert!(validate_custom_base_url("https://user:pass@api.example.com").is_err());
        assert!(validate_custom_base_url("https://api.example.com/v1?x=1").is_err());
        assert!(validate_custom_base_url("https://api.example.com/v1#frag").is_err());
        assert!(validate_custom_base_url("javascript:alert(1)").is_err());
        assert!(validate_custom_base_url("ftp://api.example.com/v1").is_err());
        assert_eq!(
            validate_custom_model_id("deepseek/deepseek-v4-flash").unwrap(),
            "deepseek/deepseek-v4-flash"
        );
        assert!(validate_custom_model_id("").is_err());
        assert_eq!(
            custom_endpoint_relative_path(UpstreamProtocolKind::ChatCompletions),
            "chat/completions"
        );
        assert_eq!(
            custom_endpoint_relative_path(UpstreamProtocolKind::Responses),
            "responses"
        );
        assert_eq!(
            custom_endpoint_relative_path(UpstreamProtocolKind::Messages),
            "messages"
        );
    }

    #[test]
    fn custom_url_host_uses_url_host_not_bracketed_host_str() {
        assert!(validate_custom_base_url("http://[::ffff:127.0.0.1]/v1").is_ok());
        assert!(validate_custom_base_url("http://[::1]/v1").is_ok());
        let mapped_loopback = validate_custom_base_url("http://[::ffff:127.0.0.1]/v1").unwrap();
        let parsed = reqwest::Url::parse(&mapped_loopback).unwrap();
        match inspect_custom_url(&parsed).unwrap().host {
            CustomUrlHost::Ip(ip) => {
                assert_eq!(ip, "::ffff:127.0.0.1".parse::<IpAddr>().unwrap());
            }
            CustomUrlHost::Domain(domain) => {
                panic!("mapped loopback must stay an IP host, got {domain}")
            }
        }
        let metadata = validate_custom_base_url("https://[::ffff:169.254.169.254]/latest").unwrap();
        let parsed = reqwest::Url::parse(&metadata).unwrap();
        match inspect_custom_url(&parsed).unwrap().host {
            CustomUrlHost::Ip(_) => {}
            CustomUrlHost::Domain(domain) => {
                panic!("mapped metadata IP must stay an IP host, got {domain}")
            }
        }
    }

    #[test]
    fn custom_base_url_normalizes_decimal_loopback_literals() {
        assert_eq!(
            validate_custom_base_url("http://127.1:8080/v1").unwrap(),
            "http://127.0.0.1:8080/v1"
        );
        assert_eq!(
            validate_custom_base_url("http://127.0.1/v1").unwrap(),
            "http://127.0.0.1/v1"
        );
        let parsed = reqwest::Url::parse("http://127.1/v1").unwrap();
        match inspect_custom_url(&parsed).unwrap().host {
            CustomUrlHost::Ip(ip) => assert_eq!(ip, "127.0.0.1".parse::<IpAddr>().unwrap()),
            CustomUrlHost::Domain(domain) => panic!("127.1 must not stay a domain: {domain}"),
        }
    }

    #[test]
    fn scnet_acknowledgement_hash_is_stable() {
        let notice = SCNET_RISK_NOTICE;
        assert_eq!(notice.acknowledgement_id, SCNET_RISK_ACKNOWLEDGEMENT_ID);
        assert_eq!(notice.version, SCNET_RISK_ACKNOWLEDGEMENT_VERSION);
        assert_eq!(notice.source_url, SCNET_RISK_ACKNOWLEDGEMENT_SOURCE_URL);
        assert_eq!(notice.body, SCNET_RISK_ACKNOWLEDGEMENT_BODY);
        assert_eq!(
            notice.content_hash(),
            SCNET_RISK_ACKNOWLEDGEMENT_CONTENT_HASH
        );
        assert_eq!(
            notice.content_hash(),
            acknowledgement_content_hash(SCNET_RISK_ACKNOWLEDGEMENT_BODY)
        );
        assert_ne!(notice.content_hash(), acknowledgement_content_hash("other"));
    }

    #[test]
    fn scnet_token_plans_share_official_usable_model_snapshot() {
        assert_eq!(
            SCNET_TOKEN_PLAN_USABLE_MODELS,
            [
                "GLM-5.2",
                "GLM-5",
                "GLM-5.1",
                "Kimi-K3",
                "Kimi-K2.7-Code",
                "Kimi-K2.6",
                "Kimi-K2.5",
                "DeepSeek-V4-Flash",
                "DeepSeek-V3.2",
                "MiniMax-M3",
                "MiniMax-M2.7",
                "MiniMax-M2.5",
                "MiMo-V2.5-Pro",
            ]
        );
        let expected = SCNET_TOKEN_PLAN_MODEL_SNAPSHOT;
        let mut previous: Option<BuiltinPlan> = None;
        for offering_id in SCNET_TOKEN_PLAN_OFFERING_IDS {
            let plan = builtin_plan(SCNET_PROVIDER_ID, offering_id).unwrap();
            assert!(is_scnet_token_plan(
                plan.offering.provider_id,
                plan.offering.offering_id
            ));
            assert_eq!(plan.model_source, SCNET_TOKEN_PLAN_MODEL_SOURCE);
            assert_eq!(
                scnet_token_plan_model_snapshot(
                    plan.offering.provider_id,
                    plan.offering.offering_id
                ),
                Some(expected)
            );
            assert!(std::ptr::eq(
                expected.upstream_models,
                SCNET_TOKEN_PLAN_USABLE_MODELS
            ));
            if let Some(previous) = previous {
                assert_eq!(previous.model_source, plan.model_source);
                assert_eq!(
                    scnet_token_plan_model_snapshot(
                        previous.offering.provider_id,
                        previous.offering.offering_id
                    )
                    .unwrap()
                    .upstream_models,
                    expected.upstream_models
                );
            }
            previous = Some(plan);
        }
        assert_eq!(
            scnet_token_plan_official_offering_name(SCNET_TOKEN_PLAN_BASIC_OFFERING_ID),
            Some(SCNET_TOKEN_PLAN_OFFICIAL_BASIC_NAME)
        );
        assert_eq!(
            scnet_token_plan_official_offering_name(SCNET_TOKEN_PLAN_STANDARD_OFFERING_ID),
            Some(SCNET_TOKEN_PLAN_OFFICIAL_STANDARD_NAME)
        );
        assert_eq!(
            scnet_token_plan_official_offering_name(SCNET_TOKEN_PLAN_PREMIUM_OFFERING_ID),
            Some(SCNET_TOKEN_PLAN_OFFICIAL_PREMIUM_NAME)
        );
    }

    #[test]
    fn scnet_token_plan_excludes_pricing_table_and_faq_extras() {
        for extra in SCNET_TOKEN_PLAN_EXCLUDED_PRICING_TABLE_OR_FAQ_MODELS {
            assert!(
                !SCNET_TOKEN_PLAN_USABLE_MODELS.contains(extra),
                "{extra} is a pricing-table/FAQ extra and must not enter the usable snapshot"
            );
        }
        assert!(!SCNET_TOKEN_PLAN_USABLE_MODELS.contains(&"glm-5.2"));
        assert!(!SCNET_TOKEN_PLAN_USABLE_MODELS.contains(&"DeepSeek-V4-Pro"));
        assert!(!SCNET_TOKEN_PLAN_USABLE_MODELS.contains(&"Qwen3-235B-A22B"));
    }

    #[test]
    fn scnet_token_plans_stay_fail_closed_without_quota_windows() {
        for offering_id in SCNET_TOKEN_PLAN_OFFERING_IDS {
            let plan = builtin_plan(SCNET_PROVIDER_ID, offering_id).unwrap();
            assert!(!plan.routable);
            assert_eq!(plan.verification_policy, VerificationPolicy::Required);
            assert_eq!(plan.verification_runtime_availability, "unavailable");
            assert_eq!(plan.pricing_availability, "unavailable");
            assert_eq!(plan.usage_availability, "unavailable");
            assert_eq!(plan.quota_unit, "credits");
            assert_ne!(plan.quota_unit, QUOTA_WINDOW_FIVE_HOURS);
            assert_ne!(plan.quota_unit, QUOTA_WINDOW_WEEK);
            assert_eq!(plan.key_prefix, Some(SCNET_TOKEN_PLAN_KEY_PREFIX));
            assert_eq!(plan.auth_schemes, &BEARER_AUTH);
            assert_eq!(
                plan.upstream_protocols,
                &[
                    UpstreamProtocolKind::ChatCompletions,
                    UpstreamProtocolKind::Messages
                ]
            );
            assert!(
                !plan
                    .upstream_protocols
                    .contains(&UpstreamProtocolKind::Responses)
            );
            assert!(validate_plan_key(plan, "sk-tp-live").is_ok());
            assert!(
                validate_plan_key(plan, "sk-live-not-token")
                    .unwrap_err()
                    .to_string()
                    .contains(SCNET_TOKEN_PLAN_KEY_PREFIX)
            );
        }
        const {
            assert!(!SCNET_TOKEN_PLAN_USAGE_RESTRICTIONS.quota_status_rest_established);
            assert!(!SCNET_TOKEN_PLAN_USAGE_RESTRICTIONS.non_billable_verification_established);
            assert!(SCNET_TOKEN_PLAN_USAGE_RESTRICTIONS.custom_application_backends_prohibited);
            assert!(SCNET_TOKEN_PLAN_USAGE_RESTRICTIONS.automation_scripts_prohibited);
            assert!(SCNET_TOKEN_PLAN_USAGE_RESTRICTIONS.non_interactive_batch_calls_prohibited);
            assert!(
                SCNET_TOKEN_PLAN_USAGE_RESTRICTIONS.curl_style_non_interactive_calls_prohibited
            );
        };
        assert_eq!(
            SCNET_TOKEN_PLAN_DOCUMENTED_ENDPOINTS.auth_scheme,
            UpstreamAuthScheme::Bearer
        );
        assert_eq!(
            SCNET_TOKEN_PLAN_DOCUMENTED_ENDPOINTS.openai_base_url,
            "https://api.scnet.cn/api/llm/v1"
        );
        assert_eq!(
            SCNET_TOKEN_PLAN_DOCUMENTED_ENDPOINTS.anthropic_base_url,
            "https://api.scnet.cn/api/llm/anthropic"
        );
        assert_eq!(
            SCNET_TOKEN_PLAN_DOCUMENTED_ENDPOINTS.chat_completions_path,
            "/chat/completions"
        );
        assert_eq!(
            SCNET_TOKEN_PLAN_DOCUMENTED_ENDPOINTS.messages_path,
            "/v1/messages"
        );
        let goat = builtin_plan(COMMAND_CODE_PROVIDER_ID, GOAT_OFFERING_ID).unwrap();
        assert_eq!(goat.model_source, "builtin_command_code_protocol_table");
        assert!(
            scnet_token_plan_model_snapshot(COMMAND_CODE_PROVIDER_ID, GOAT_OFFERING_ID).is_none()
        );
    }

    #[test]
    fn provider_registry_is_exhaustive_for_plans_and_adapter_kinds() {
        let mut seen = std::collections::HashSet::new();
        assert_eq!(ProviderRegistry::iter().count(), BUILTIN_PLANS.len());
        for plan in BUILTIN_PLANS {
            let kind = ProviderAdapterKind::from_offering(
                plan.offering.provider_id,
                plan.offering.offering_id,
            )
            .expect("every catalog plan has an adapter kind");
            seen.insert(kind);
            let descriptor =
                ProviderRegistry::get(plan.offering.provider_id, plan.offering.offering_id)
                    .expect("every catalog plan has a composed descriptor");
            assert_eq!(descriptor.kind, kind);
            assert_eq!(descriptor.provider_id, plan.offering.provider_id);
            assert_eq!(descriptor.offering_id, plan.offering.offering_id);
            assert_eq!(descriptor.inference.catalog_routable, plan.routable);
            assert_eq!(
                descriptor.inference.credential_kind,
                plan.offering.credential_kind
            );
            assert_eq!(descriptor.inference.quota_scope, plan.offering.quota_scope);
            assert_eq!(descriptor.verification.policy, plan.verification_policy);
            assert_eq!(
                descriptor.verification.runtime_availability,
                plan.verification_runtime_availability
            );
            assert_eq!(descriptor.pricing.availability, plan.pricing_availability);
            assert_eq!(
                descriptor.usage.catalog_availability,
                plan.usage_availability
            );
            assert_eq!(
                descriptor.usage.manual_calibration,
                plan.manual_usage_calibration
            );
            assert_eq!(descriptor.model_catalog.catalog_source, plan.model_source);
            assert_eq!(
                descriptor.card_actions.managed_registration,
                plan.managed_registration
            );
            assert_eq!(
                descriptor.card_actions.persisted_enable_allowed,
                plan.routable
            );
            assert!(!descriptor.protocol_probe.request_path_may_trial);
            assert!(!descriptor.protocol_probe.fallback_priority.is_empty());
            assert_eq!(
                descriptor.protocol_probe.explicit_probe,
                descriptor.card_actions.protocol_probe
            );
            assert_eq!(
                descriptor.card_actions.catalog_refresh,
                kind.catalog_refresh_supported()
            );
            assert_eq!(
                descriptor.card_actions.protocol_probe,
                kind.protocol_probe_supported()
            );
            assert!(!descriptor.verification.uses_get_models);
            match kind {
                ProviderAdapterKind::OpenCodeGo
                | ProviderAdapterKind::ZenFree
                | ProviderAdapterKind::ConfigurableHttp => {
                    assert!(descriptor.inference.production_inference);
                }
                ProviderAdapterKind::CommandCodeGoat | ProviderAdapterKind::Scnet => {
                    assert!(!descriptor.inference.production_inference);
                    assert!(!descriptor.inference.catalog_routable);
                    assert_eq!(descriptor.verification.runtime_availability, "unavailable");
                    assert_eq!(
                        descriptor.card_actions.connection_verify,
                        CardVerifyAction::UnavailableNotImplemented
                    );
                }
            }
        }
        for kind in ProviderAdapterKind::ALL {
            assert!(
                seen.contains(&kind),
                "{kind:?} must be wired to at least one catalog offering"
            );
        }
        assert_eq!(seen.len(), ProviderAdapterKind::ALL.len());
        assert!(ProviderAdapterKind::from_offering("unknown", "unknown").is_none());
        assert!(ProviderRegistry::get("unknown", "unknown").is_none());
        assert_eq!(ProviderAdapterKind::ALL.len(), 5);
    }

    #[test]
    fn adapter_descriptors_preserve_current_capability_decisions() {
        let go = ProviderRegistry::get(OPENCODE_PROVIDER_ID, GO_OFFERING_ID).unwrap();
        assert_eq!(go.kind, ProviderAdapterKind::OpenCodeGo);
        assert_eq!(
            go.inference.auth,
            InferenceAuthDescriptor::OpenCodeProtocolDefault
        );
        assert!(go.inference.follow_redirects);
        assert_eq!(go.inference.origin, InferenceOriginKind::ConfigUpstreamBase);
        assert!(go.usage.automatic_sync);
        assert!(go.usage.authoritative_for_quota);
        assert_eq!(
            go.usage.endpoint,
            Some(crate::kernel::catalog::OPENCODE_GO_USAGE_URL)
        );
        assert_eq!(go.usage.endpoint, Some(crate::go_usage::GO_USAGE_URL));
        assert_eq!(
            crate::go_usage::GO_USAGE_URL,
            "https://opencode.ai/zen/go/v1/usage"
        );
        assert_eq!(go.usage.contract, UsageContractKind::Authoritative);
        assert!(go.usage.publishes_capability);
        assert!(go.error_cooldown.parse_opencode_go_windows_on_429);
        assert!(go.error_cooldown.schedule_official_go_usage_after_429);
        assert_eq!(
            go.protocol_probe.matrix,
            ProtocolMatrixKind::OpenCodeModelProtocols
        );
        assert!(go.protocol_probe.explicit_probe);
        assert_eq!(
            go.protocol_probe.structural_ceiling,
            StructuralProbeCeiling::OpenCodeConstructable
        );
        assert_eq!(
            go.card_actions.connection_verify,
            CardVerifyAction::Optional
        );
        assert!(go.card_actions.usage_refresh);
        assert!(go.card_actions.protocol_probe);
        assert!(!go.card_actions.catalog_refresh);

        let zen = ProviderRegistry::get(OPENCODE_ZEN_FREE_PROVIDER_ID, ANONYMOUS_FREE_OFFERING_ID)
            .unwrap();
        assert_eq!(zen.kind, ProviderAdapterKind::ZenFree);
        assert_eq!(zen.inference.auth, InferenceAuthDescriptor::None);
        assert_eq!(zen.inference.credential_kind, CredentialKind::None);
        assert_eq!(zen.inference.quota_scope, QuotaScope::EgressIp);
        assert_eq!(zen.inference.channel, Some(InferenceChannelKind::Free));
        assert!(zen.inference.follow_redirects);
        assert!(zen.model_catalog.admin_explicit_refresh);
        assert!(zen.protocol_probe.unknown_zen_free_defaults_to_chat);
        assert!(zen.protocol_probe.explicit_probe);
        assert_eq!(
            zen.protocol_probe.structural_ceiling,
            StructuralProbeCeiling::ZenFreeConstructable
        );
        assert!(!zen.usage.experimental);
        assert!(zen.error_cooldown.egress_ip_shared_free_cooldown);
        assert!(zen.error_cooldown.inference_401_passthrough);
        assert!(zen.error_cooldown.success_cost_state_free);
        assert!(zen.card_actions.fetch_zen_models);
        assert!(zen.card_actions.protocol_probe);
        assert!(zen.card_actions.catalog_refresh);
        assert_eq!(
            zen.card_actions.connection_verify,
            CardVerifyAction::NotApplicable
        );

        let goat = ProviderRegistry::get(COMMAND_CODE_PROVIDER_ID, GOAT_OFFERING_ID).unwrap();
        assert_eq!(goat.kind, ProviderAdapterKind::CommandCodeGoat);
        assert!(goat.inference.loopback_test_seam_only);
        assert!(!goat.inference.follow_redirects);
        assert_eq!(goat.inference.auth, InferenceAuthDescriptor::Bearer);
        assert!(goat.usage.experimental);
        assert!(!goat.usage.publishes_capability || goat.usage.endpoint.is_none());
        assert!(goat.usage.publishes_capability);
        assert_eq!(
            goat.usage.contract,
            UsageContractKind::ExperimentalUnavailable
        );
        assert!(goat.usage.manual_calibration);
        assert!(goat.error_cooldown.generic_provider_key_cooldown);
        assert_eq!(
            goat.protocol_probe.matrix,
            ProtocolMatrixKind::CommandCodeNative
        );
        assert!(!goat.protocol_probe.explicit_probe);
        assert_eq!(
            goat.protocol_probe.structural_ceiling,
            StructuralProbeCeiling::Unavailable
        );
        assert!(!goat.card_actions.protocol_probe);
        assert!(!goat.card_actions.catalog_refresh);

        let scnet =
            ProviderRegistry::get(SCNET_PROVIDER_ID, SCNET_TOKEN_PLAN_BASIC_OFFERING_ID).unwrap();
        assert_eq!(scnet.kind, ProviderAdapterKind::Scnet);
        assert!(scnet.model_catalog.snapshot_is_adapter_input_only);
        assert!(!scnet.usage.publishes_capability);
        assert_eq!(scnet.inference.origin, InferenceOriginKind::None);
        assert!(scnet.card_actions.risk_acknowledgement);
        assert_eq!(
            scnet.protocol_probe.matrix,
            ProtocolMatrixKind::DocumentedChatAndMessages
        );
        assert!(!scnet.protocol_probe.explicit_probe);
        assert_eq!(
            scnet.protocol_probe.structural_ceiling,
            StructuralProbeCeiling::Unavailable
        );
        assert!(!scnet.card_actions.protocol_probe);

        let custom = ProviderRegistry::get(CUSTOM_PROVIDER_ID, CUSTOM_API_OFFERING_ID).unwrap();
        assert_eq!(custom.kind, ProviderAdapterKind::ConfigurableHttp);
        assert_eq!(
            custom.inference.auth,
            InferenceAuthDescriptor::ConfigurableBearerOrXApiKey
        );
        assert!(!custom.inference.follow_redirects);
        assert_eq!(
            custom.inference.origin,
            InferenceOriginKind::AccountConfigured
        );
        assert!(custom.model_catalog.overlays_declared_ids);
        assert!(custom.verification.never_auto_enable);
        assert!(custom.verification.probe_first_declared_model);
        assert!(!custom.usage.publishes_capability);
        assert_eq!(
            custom.card_actions.connection_verify,
            CardVerifyAction::AvailableThenExplicitEnable
        );
        assert!(custom.card_actions.protocol_and_auth_immutable_after_create);
        assert!(custom.card_actions.discover_models);
        assert!(custom.card_actions.protocol_probe);
        assert!(custom.card_actions.catalog_refresh);
        assert_eq!(
            custom.protocol_probe.structural_ceiling,
            StructuralProbeCeiling::AccountDeclared
        );
        assert!(custom.error_cooldown.generic_provider_key_cooldown);

        assert_ne!(go.kind, ProviderAdapterKind::ConfigurableHttp);
        assert_ne!(zen.kind, ProviderAdapterKind::ConfigurableHttp);
        assert_ne!(goat.kind, ProviderAdapterKind::ConfigurableHttp);
        assert_ne!(scnet.kind, ProviderAdapterKind::ConfigurableHttp);
        assert_eq!(
            ProviderAdapterKind::from_offering(CUSTOM_PROVIDER_ID, CUSTOM_API_OFFERING_ID),
            Some(ProviderAdapterKind::ConfigurableHttp)
        );
    }

    #[test]
    fn composed_capability_contracts_delegate_from_concrete_adapters() {
        fn compose<A>(adapter: A, plan: BuiltinPlan) -> ProviderCapabilities
        where
            A: ModelCatalogAdapter
                + InferenceAdapter
                + ProtocolProbeAdapter
                + VerificationAdapter
                + UsageAdapter
                + PricingAdapter
                + ErrorPolicyAdapter
                + CardCapabilities,
        {
            ProviderCapabilities::compose(adapter, plan)
        }

        for plan in BUILTIN_PLANS {
            let kind = ProviderAdapterKind::from_offering(
                plan.offering.provider_id,
                plan.offering.offering_id,
            )
            .expect("every catalog plan has an adapter kind");
            let from_adapter = match kind {
                ProviderAdapterKind::OpenCodeGo => compose(OpenCodeGoAdapter, plan),
                ProviderAdapterKind::ZenFree => compose(ZenFreeAdapter, plan),
                ProviderAdapterKind::CommandCodeGoat => compose(CommandCodeGoatAdapter, plan),
                ProviderAdapterKind::Scnet => compose(ScnetAdapter, plan),
                ProviderAdapterKind::ConfigurableHttp => compose(ConfigurableHttpAdapter, plan),
            };
            let descriptor =
                ProviderRegistry::get(plan.offering.provider_id, plan.offering.offering_id)
                    .expect("every catalog plan has a composed descriptor");
            assert_eq!(kind.compose_capabilities(plan), from_adapter);
            assert_eq!(descriptor.capabilities(), from_adapter);
            assert_eq!(descriptor.model_catalog, from_adapter.model_catalog);
            assert_eq!(descriptor.inference, from_adapter.inference);
            assert_eq!(descriptor.protocol_probe, from_adapter.protocol_probe);
            assert_eq!(descriptor.verification, from_adapter.verification);
            assert_eq!(descriptor.usage, from_adapter.usage);
            assert_eq!(descriptor.pricing, from_adapter.pricing);
            assert_eq!(descriptor.error_cooldown, from_adapter.error_cooldown);
            assert_eq!(descriptor.card_actions, from_adapter.card_actions);
        }

        let go_plan = builtin_plan(OPENCODE_PROVIDER_ID, GO_OFFERING_ID).unwrap();
        assert_eq!(
            OpenCodeGoAdapter::usage(go_plan).contract,
            UsageContractKind::Authoritative
        );
        assert!(OpenCodeGoAdapter::error_policy(go_plan).parse_opencode_go_windows_on_429);
        let custom_plan = builtin_plan(CUSTOM_PROVIDER_ID, CUSTOM_API_OFFERING_ID).unwrap();
        assert!(ConfigurableHttpAdapter::verification(custom_plan).probe_first_declared_model);
        assert!(ConfigurableHttpAdapter::model_catalog(custom_plan).overlays_declared_ids);
        assert!(!ConfigurableHttpAdapter::inference(custom_plan).follow_redirects);
        let goat_plan = builtin_plan(COMMAND_CODE_PROVIDER_ID, GOAT_OFFERING_ID).unwrap();
        assert!(!CommandCodeGoatAdapter::inference(goat_plan).production_inference);
        let scnet_plan =
            builtin_plan(SCNET_PROVIDER_ID, SCNET_TOKEN_PLAN_BASIC_OFFERING_ID).unwrap();
        assert!(!ScnetAdapter::inference(scnet_plan).production_inference);
        let zen_plan =
            builtin_plan(OPENCODE_ZEN_FREE_PROVIDER_ID, ANONYMOUS_FREE_OFFERING_ID).unwrap();
        assert!(ZenFreeAdapter::protocol_probe(zen_plan).unknown_zen_free_defaults_to_chat);
        assert!(ZenFreeAdapter::card_capabilities(zen_plan).fetch_zen_models);
    }
}

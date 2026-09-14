//! Pure immutable Provider catalog, adapter identities, capability registry,
//! and binding/enablement validation.
//!
//! Custom URL parsing (`reqwest`) and persistence-shaped quota, credit, pricing,
//! and usage-sync records stay in the host crate.

use crate::catalog::{
    CatalogParseError, CredentialKind, OPENCODE_GO_USAGE_URL, QuotaScope, UpstreamAuthScheme,
    UpstreamProtocolKind,
};
use crate::ids::{
    COMMAND_CODE_PROVIDER_ID, CPA_ACCOUNT_ID, CPA_PROVIDER_ID, CUSTOM_PROVIDER_ID,
    KIMI_PROVIDER_ID, MINIMAX_PROVIDER_ID, OLLAMA_PROVIDER_ID, OPENCODE_PROVIDER_ID,
    OPENCODE_ZEN_FREE_PROVIDER_ID, ZEN_FREE_ACCOUNT_ID,
};
#[cfg(feature = "schemars")]
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::fmt;

/// Official OpenCode Go inference origin. Production routes always use this
/// origin; loopback substitutes exist only as a test seam.
pub const OPENCODE_GO_BASE_URL: &str = "https://opencode.ai/zen/go";
pub const OPENCODE_GO_HOST: &str = "opencode.ai";
/// Official OpenCode Zen free-channel origin. Sibling of
/// [`OPENCODE_GO_BASE_URL`], not derived from settings.
pub const OPENCODE_ZEN_BASE_URL: &str = "https://opencode.ai/zen";

/// Official Command Code Provider API v1 base. Production GOAT routes this
/// fixed origin; loopback substitutes exist only as a test seam.
pub const COMMAND_CODE_GOAT_BASE_URL: &str = "https://api.commandcode.ai/provider/v1";
pub const COMMAND_CODE_GOAT_HOST: &str = "api.commandcode.ai";
/// Relative to [`COMMAND_CODE_GOAT_BASE_URL`].
pub const COMMAND_CODE_GOAT_CHAT_COMPLETIONS_PATH: &str = "/chat/completions";
/// Relative to [`COMMAND_CODE_GOAT_BASE_URL`]. Anthropic models use this path;
/// OpenAI and open-source models use Chat Completions.
pub const COMMAND_CODE_GOAT_MESSAGES_PATH: &str = "/messages";
/// Official public GET `/models` discovery path used for Provider catalog refresh.
pub const COMMAND_CODE_GOAT_MODELS_PATH: &str = "/models";
pub const COMMAND_CODE_GOAT_MODELS_SOURCE: &str = "command_code_get_models";
pub const COMMAND_CODE_GOAT_MODEL_SOURCE: &str = "command_code_verified_models";
/// Public GOAT plan windows. OCG uses these only to project locally priced
/// request logs; Command Code does not expose a machine-readable usage API.
pub const COMMAND_CODE_GOAT_QUOTA_5H: f64 = 14.0;
pub const COMMAND_CODE_GOAT_QUOTA_WEEK: f64 = 35.0;
pub const COMMAND_CODE_GOAT_QUOTA_MONTH: f64 = 70.0;
pub const MAX_COMMAND_CODE_MODELS_CATALOG: usize = 1_000;

/// Official MiniMax CN Token Plan endpoints. The Plan Key is sent as Bearer
/// auth to catalog, Chat, and Messages surfaces; redirects stay disabled.
pub const MINIMAX_CN_BASE_URL: &str = "https://api.minimaxi.com/v1";
pub const MINIMAX_CN_CHAT_COMPLETIONS_PATH: &str = "/chat/completions";
pub const MINIMAX_CN_ANTHROPIC_BASE_URL: &str = "https://api.minimaxi.com/anthropic";
pub const MINIMAX_CN_MESSAGES_PATH: &str = "/v1/messages";
pub const MINIMAX_CN_MODELS_PATH: &str = "/models";
pub const MINIMAX_CN_USAGE_URL: &str = "https://api.minimaxi.com/v1/token_plan/remains";
pub const MINIMAX_CN_MODEL_SOURCE: &str = "minimax_cn_get_models";
pub const MAX_MINIMAX_CN_MODELS_CATALOG: usize = 1_000;
pub const KIMI_CN_BASE_URL: &str = "https://api.kimi.com/coding/v1";
pub const KIMI_CN_CHAT_COMPLETIONS_PATH: &str = "/chat/completions";
pub const KIMI_CN_MESSAGES_PATH: &str = "/messages";
pub const KIMI_CN_MODELS_PATH: &str = "/models";
pub const KIMI_CN_USAGE_URL: &str = "https://api.kimi.com/coding/v1/usages";
pub const KIMI_CN_MODEL_SOURCE: &str = "kimi_cn_get_models";
pub const MAX_KIMI_CN_MODELS_CATALOG: usize = 1_000;

/// Ollama Cloud surface constants live in [`crate::ids`] next to the provider
/// identity. The model source below feeds Provider catalog refresh evidence.
pub const OLLAMA_CLOUD_MODEL_SOURCE: &str = "ollama_cloud_get_models";
pub const MAX_OLLAMA_CLOUD_MODELS_CATALOG: usize = 1_000;

/// Account-scoped Ollama Cloud billing profile. Credits are USD per billing
/// month. Absence of a stored row is unconfigured, not a Free tier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OllamaBillingTier {
    Pro,
    Max,
    Team,
}

impl OllamaBillingTier {
    pub const PRO_MONTHLY_CREDITS: f64 = 60.0;
    pub const MAX_MONTHLY_CREDITS: f64 = 300.0;
    pub const TEAM_MONTHLY_CREDITS: f64 = 1000.0;

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pro => "pro",
            Self::Max => "max",
            Self::Team => "team",
        }
    }

    pub fn parse(value: &str) -> Result<Self, String> {
        match value.trim() {
            "pro" => Ok(Self::Pro),
            "max" => Ok(Self::Max),
            "team" => Ok(Self::Team),
            other => Err(format!("unknown Ollama billing tier `{other}`")),
        }
    }

    /// Soft monthly USD-credit limit for the paid profile.
    pub fn monthly_credit_limit(self) -> f64 {
        match self {
            Self::Pro => Self::PRO_MONTHLY_CREDITS,
            Self::Max => Self::MAX_MONTHLY_CREDITS,
            Self::Team => Self::TEAM_MONTHLY_CREDITS,
        }
    }

    pub fn requires_purchase_date(self) -> bool {
        true
    }
}

/// Models included by the GOAT subscription page. These are the default-on
/// rows in the Provider model/protocol matrix. Models discovered beyond this
/// preset remain visible but default off until an administrator enables them.
pub const COMMAND_CODE_GOAT_INCLUDED_MODEL_IDS: &[&str] = &[
    "gpt-5.6-sol",
    "gpt-5.6-luna",
    "deepseek/deepseek-v4-pro",
    "deepseek/deepseek-v4-flash",
    "deepseek/deepseek-v4-flash-vision-exp",
    "moonshotai/Kimi-K3",
    "moonshotai/Kimi-K2.7-Code",
    "moonshotai/Kimi-K2.7-Code-Highspeed",
    "moonshotai/Kimi-K2.6",
    "moonshotai/Kimi-K2.5",
    "zai-org/GLM-5.3",
    "zai-org/GLM-5.2",
    "zai-org/GLM-5.2-Fast",
    "zai-org/GLM-5.1",
    "zai-org/GLM-5",
    "MiniMaxAI/MiniMax-M3",
    "MiniMaxAI/MiniMax-M2.7",
    "MiniMaxAI/MiniMax-M2.5",
    "xiaomi/mimo-v2.5-pro",
    "xiaomi/mimo-v2.5",
    "Qwen/Qwen3.8-Max",
    "Qwen/Qwen3.8-27B",
    "Qwen/Qwen3.7-Max",
    "Qwen/Qwen3.7-Plus",
    "Qwen/Qwen3.7-Flash",
    "Qwen/Qwen3.6-Max-Preview",
    "Qwen/Qwen3.6-Plus",
    "stepfun/Step-3.7-Flash",
    "stepfun/Step-3.5-Flash",
    "tencent/hy3-paid",
    "google/gemini-3.7-flash",
    "nvidia/nemotron-3-ultra-550b-a55b",
    "thinkingmachines/inkling",
    "thinkingmachines/inkling-small",
    "stealth/ox-alpha",
    "poolside/laguna-s-2.1-free",
    "meta/muse-spark-1.2",
    "meta/muse-spark-1.2-contributor",
    "xai/grok-4.5",
    "xai/grok-4.6",
];

pub fn command_code_goat_includes_model(model_id: &str) -> bool {
    let model_id = model_id.trim();
    !model_id.is_empty()
        && COMMAND_CODE_GOAT_INCLUDED_MODEL_IDS
            .iter()
            .any(|included| included.eq_ignore_ascii_case(model_id))
}

pub const QUOTA_WINDOW_FIVE_HOURS: &str = "five_hours";
pub const QUOTA_WINDOW_WEEK: &str = "week";
pub const QUOTA_WINDOW_MONTH: &str = "month";
pub const QUOTA_WINDOW_FREE: &str = "free";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CreationAvailability {
    Available,
    Unavailable,
}

/// Product location for a sealed Provider. Registry validation applies to both
/// surfaces; callers use this marker to keep external integrations out of the
/// Providers catalog and generic Add Account flow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderProductSurface {
    Provider,
    ExternalIntegration,
}

impl ProviderProductSurface {
    pub const fn is_external_integration(self) -> bool {
        matches!(self, Self::ExternalIntegration)
    }
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

/// Command Code documented surfaces have no Responses path.
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuiltinProvider {
    pub provider_id: &'static str,
    pub credential_kind: CredentialKind,
    pub quota_scope: QuotaScope,
    pub singleton_account_id: Option<&'static str>,
    /// Stable identifier for the persisted Provider-contract scope. External
    /// integrations and account-configured Custom API intentionally have none.
    pub contract_scope_id: Option<&'static str>,
    pub display_name: &'static str,
    pub display_family: &'static str,
    pub product_surface: ProviderProductSurface,
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
const OLLAMA_PURCHASE_DATE_FIELD: PlanFormField = PlanFormField {
    id: "purchase_date",
    kind: "date",
    required: true,
    immutable_after_create: false,
};
const NOTES_FIELD: PlanFormField = PlanFormField {
    id: "notes",
    kind: "text",
    required: false,
    immutable_after_create: false,
};
const ENDPOINT_URL_FIELD: PlanFormField = PlanFormField {
    id: "endpoint_url",
    kind: "url",
    required: true,
    immutable_after_create: false,
};
const PROTOCOL_FIELD: PlanFormField = PlanFormField {
    id: "upstream_protocol",
    kind: "select",
    required: true,
    immutable_after_create: false,
};
const OLLAMA_BILLING_TIER_FIELD: PlanFormField = PlanFormField {
    id: "ollama_billing_tier",
    kind: "select",
    required: true,
    immutable_after_create: false,
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
const MINIMAX_CN_FORM_FIELDS: [PlanFormField; 4] =
    [NAME_FIELD, KEY_FIELD, PURCHASE_DATE_FIELD, NOTES_FIELD];
const KIMI_CN_FORM_FIELDS: [PlanFormField; 4] =
    [NAME_FIELD, KEY_FIELD, PURCHASE_DATE_FIELD, NOTES_FIELD];
const OLLAMA_CLOUD_FORM_FIELDS: [PlanFormField; 5] = [
    NAME_FIELD,
    KEY_FIELD,
    OLLAMA_BILLING_TIER_FIELD,
    OLLAMA_PURCHASE_DATE_FIELD,
    NOTES_FIELD,
];
const CUSTOM_FORM_FIELDS: [PlanFormField; 6] = [
    NAME_FIELD,
    KEY_FIELD,
    NOTES_FIELD,
    ENDPOINT_URL_FIELD,
    PROTOCOL_FIELD,
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
const CHAT_MESSAGES_PROTOCOLS: [UpstreamProtocolKind; 2] = [
    UpstreamProtocolKind::ChatCompletions,
    UpstreamProtocolKind::Messages,
];
const CHAT_PROTOCOLS: [UpstreamProtocolKind; 1] = [UpstreamProtocolKind::ChatCompletions];
const CUSTOM_PROTOCOLS: [UpstreamProtocolKind; 3] = [
    UpstreamProtocolKind::ChatCompletions,
    UpstreamProtocolKind::Responses,
    UpstreamProtocolKind::Messages,
];

pub const BUILTIN_PROVIDERS: [BuiltinProvider; 8] = [
    BuiltinProvider {
        provider_id: OPENCODE_PROVIDER_ID,
        credential_kind: CredentialKind::ApiKey,
        quota_scope: QuotaScope::Key,
        singleton_account_id: None,
        contract_scope_id: Some(OPENCODE_PROVIDER_ID),
        display_name: "OpenCode Go",
        display_family: "OpenCode",
        product_surface: ProviderProductSurface::Provider,
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
    },
    BuiltinProvider {
        provider_id: OPENCODE_ZEN_FREE_PROVIDER_ID,
        credential_kind: CredentialKind::None,
        quota_scope: QuotaScope::EgressIp,
        singleton_account_id: Some(ZEN_FREE_ACCOUNT_ID),
        contract_scope_id: Some(OPENCODE_ZEN_FREE_PROVIDER_ID),
        display_name: "OpenCode Zen Free",
        display_family: "OpenCode",
        product_surface: ProviderProductSurface::Provider,
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
    },
    BuiltinProvider {
        provider_id: COMMAND_CODE_PROVIDER_ID,
        credential_kind: CredentialKind::ApiKey,
        quota_scope: QuotaScope::Key,
        singleton_account_id: None,
        contract_scope_id: Some(COMMAND_CODE_PROVIDER_ID),
        display_name: "Command Code GOAT",
        display_family: "Command Code",
        product_surface: ProviderProductSurface::Provider,
        creation_availability: CreationAvailability::Available,
        creation_unavailable_reason: None,
        verification_policy: VerificationPolicy::NotRequired,
        verification_runtime_availability: "not_applicable",
        routable: true,
        managed_registration: false,
        pricing_availability: "available",
        usage_availability: "local_state",
        manual_usage_calibration: true,
        quota_unit: "usd",
        model_source: COMMAND_CODE_GOAT_MODEL_SOURCE,
        key_prefix: None,
        auth_schemes: &BEARER_AUTH,
        upstream_protocols: &GOAT_PROTOCOLS,
        form_fields: &GOAT_FORM_FIELDS,
    },
    BuiltinProvider {
        provider_id: MINIMAX_PROVIDER_ID,
        credential_kind: CredentialKind::ApiKey,
        quota_scope: QuotaScope::Key,
        singleton_account_id: None,
        contract_scope_id: Some(MINIMAX_PROVIDER_ID),
        display_name: "MiniMax CN Token Plan",
        display_family: "MiniMax",
        product_surface: ProviderProductSurface::Provider,
        creation_availability: CreationAvailability::Available,
        creation_unavailable_reason: None,
        verification_policy: VerificationPolicy::NotRequired,
        verification_runtime_availability: "not_applicable",
        routable: true,
        managed_registration: false,
        pricing_availability: "unpriced",
        usage_availability: "available",
        manual_usage_calibration: false,
        quota_unit: "request",
        model_source: MINIMAX_CN_MODEL_SOURCE,
        key_prefix: Some("sk-cp"),
        auth_schemes: &BEARER_AUTH,
        upstream_protocols: &CHAT_MESSAGES_PROTOCOLS,
        form_fields: &MINIMAX_CN_FORM_FIELDS,
    },
    BuiltinProvider {
        provider_id: KIMI_PROVIDER_ID,
        credential_kind: CredentialKind::ApiKey,
        quota_scope: QuotaScope::Key,
        singleton_account_id: None,
        contract_scope_id: Some(KIMI_PROVIDER_ID),
        display_name: "Kimi Code CN",
        display_family: "Kimi",
        product_surface: ProviderProductSurface::Provider,
        creation_availability: CreationAvailability::Available,
        creation_unavailable_reason: None,
        verification_policy: VerificationPolicy::NotRequired,
        verification_runtime_availability: "not_applicable",
        routable: true,
        managed_registration: false,
        pricing_availability: "unpriced",
        usage_availability: "available",
        manual_usage_calibration: false,
        quota_unit: "request",
        model_source: KIMI_CN_MODEL_SOURCE,
        key_prefix: Some("sk-ki"),
        auth_schemes: &BEARER_AUTH,
        upstream_protocols: &CHAT_MESSAGES_PROTOCOLS,
        form_fields: &KIMI_CN_FORM_FIELDS,
    },
    BuiltinProvider {
        provider_id: OLLAMA_PROVIDER_ID,
        credential_kind: CredentialKind::ApiKey,
        quota_scope: QuotaScope::Key,
        singleton_account_id: None,
        contract_scope_id: Some(OLLAMA_PROVIDER_ID),
        display_name: "Ollama Cloud",
        display_family: "Ollama",
        product_surface: ProviderProductSurface::Provider,
        creation_availability: CreationAvailability::Available,
        creation_unavailable_reason: None,
        verification_policy: VerificationPolicy::NotRequired,
        verification_runtime_availability: "not_applicable",
        routable: true,
        managed_registration: false,
        pricing_availability: "available",
        usage_availability: "local_state",
        manual_usage_calibration: true,
        quota_unit: "usd_credits",
        model_source: OLLAMA_CLOUD_MODEL_SOURCE,
        key_prefix: None,
        auth_schemes: &BEARER_AUTH,
        upstream_protocols: &CHAT_PROTOCOLS,
        form_fields: &OLLAMA_CLOUD_FORM_FIELDS,
    },
    BuiltinProvider {
        provider_id: CUSTOM_PROVIDER_ID,
        credential_kind: CredentialKind::ApiKey,
        quota_scope: QuotaScope::Key,
        singleton_account_id: None,
        contract_scope_id: None,
        display_name: "Custom API",
        display_family: "Custom",
        product_surface: ProviderProductSurface::Provider,
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
    },
    BuiltinProvider {
        provider_id: CPA_PROVIDER_ID,
        credential_kind: CredentialKind::ApiKey,
        quota_scope: QuotaScope::Key,
        singleton_account_id: Some(CPA_ACCOUNT_ID),
        contract_scope_id: None,
        display_name: "CPA Subscription Pool",
        display_family: "CPA",
        product_surface: ProviderProductSurface::ExternalIntegration,
        creation_availability: CreationAvailability::Unavailable,
        creation_unavailable_reason: Some(
            "CPA is a local external integration and cannot be created through the generic account API",
        ),
        verification_policy: VerificationPolicy::Required,
        verification_runtime_availability: "external_integration",
        routable: true,
        managed_registration: false,
        pricing_availability: "unpriced",
        usage_availability: "unavailable",
        manual_usage_calibration: false,
        quota_unit: "request",
        model_source: "cpa_persisted_snapshot",
        key_prefix: None,
        auth_schemes: &BEARER_AUTH,
        upstream_protocols: &CUSTOM_PROTOCOLS,
        form_fields: &[],
    },
];

/// Migration-period mirror of `resources/provider-presets.json` `offering`
/// field, point-in-time. Plan-offering preset ids resolve to `"plan"`, every
/// other preset id (and unknown values) resolve to `"api"`. The persisted
/// `providers.offering` column is seeded from this map when a row carries a
/// `preset_id`; non-preset rows stay `"api"`.
const PRESET_OFFERINGS: &[(&str, &str)] = &[
    ("zhipu-coding", "plan"),
    ("zai-coding", "plan"),
    ("tencent-token", "plan"),
    ("tencent-token-intl", "plan"),
    ("tencent-enterprise-pro", "plan"),
    ("tencent-enterprise-pro-intl", "plan"),
    ("tencent-enterprise-lite", "plan"),
    ("tencent-enterprise-lite-intl", "plan"),
    ("bailian-coding", "plan"),
    ("qwencloud-coding", "plan"),
    ("qwencloud-token", "plan"),
    ("volcengine-agent", "plan"),
    ("volcengine-coding", "plan"),
    ("byteplus-coding", "plan"),
    ("qianfan-coding", "plan"),
    ("qianfan-token-team", "plan"),
    ("stepfun-plan", "plan"),
    ("stepfun-plan-intl", "plan"),
    ("xiaomi-mimo-token", "plan"),
    ("streamlake-coding", "plan"),
    ("compshare-coding", "plan"),
    ("atlascloud", "plan"),
];

/// Resolve the persisted `offering` for a dynamic Provider that carries a
/// `preset_id`. Plan-offering presets map to `"plan"`, every other value maps
/// to `"api"`. Stable identifier: builtin adapters do not consume this.
pub fn preset_offering(preset_id: &str) -> &'static str {
    let trimmed = preset_id.trim();
    if trimmed.is_empty() {
        return "api";
    }
    PRESET_OFFERINGS
        .iter()
        .find(|(id, _)| *id == trimmed)
        .map(|(_, offering)| *offering)
        .unwrap_or("api")
}

/// Sealed offering for builtin adapters: paid families are Plans, free and
/// account-owned surfaces are API. Point-in-time mirror of the v42 seed.
pub fn builtin_offering(provider_id: &str) -> &'static str {
    match provider_id {
        OPENCODE_PROVIDER_ID
        | COMMAND_CODE_PROVIDER_ID
        | MINIMAX_PROVIDER_ID
        | KIMI_PROVIDER_ID
        | OLLAMA_PROVIDER_ID => "plan",
        _ => "api",
    }
}

/// Provenance of a row in the unified `providers` table. The column is
/// additive in v42 and never feeds routing decisions: builtin adapters stay
/// sealed, and dynamic rows keep their `ConfigurableHttp` adapter path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schemars", schemars(rename_all = "snake_case"))]
pub enum ProviderOrigin {
    Builtin,
    Preset,
    Custom,
}

impl ProviderOrigin {
    pub const ALL: [Self; 3] = [Self::Builtin, Self::Preset, Self::Custom];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Builtin => "builtin",
            Self::Preset => "preset",
            Self::Custom => "custom",
        }
    }
}

impl fmt::Display for ProviderOrigin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl TryFrom<&str> for ProviderOrigin {
    type Error = ProviderBindingError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "builtin" => Ok(Self::Builtin),
            "preset" => Ok(Self::Preset),
            "custom" => Ok(Self::Custom),
            other => Err(ProviderBindingError::UnknownUpstreamProtocol(
                other.to_string(),
            )),
        }
    }
}

/// Resolve the persisted `origin` for a dynamic Provider. Preset-bearing rows
/// are `preset`; manual rows are `custom`. Builtin rows are never constructed
/// through this path.
pub fn provider_origin_from_preset(preset_id: Option<&str>) -> ProviderOrigin {
    match preset_id {
        Some(value) if !value.trim().is_empty() => ProviderOrigin::Preset,
        _ => ProviderOrigin::Custom,
    }
}

pub fn default_provider_id() -> String {
    OPENCODE_PROVIDER_ID.to_string()
}

pub fn default_credential_kind() -> CredentialKind {
    CredentialKind::ApiKey
}

pub fn default_quota_scope() -> QuotaScope {
    QuotaScope::Key
}

pub fn builtin_provider(provider_id: &str) -> Option<BuiltinProvider> {
    BUILTIN_PROVIDERS
        .iter()
        .copied()
        .find(|provider| provider.provider_id == provider_id)
}

/// Exhaustive, code-owned adapter identity. Not a plugin slot, JSON DSL, or
/// user-defined implementation. Custom API is [`Self::ConfigurableHttp`], not
/// a base class other adapters inherit from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProviderAdapterKind {
    OpenCodeGo,
    ZenFree,
    CommandCodeGoat,
    MiniMaxCn,
    KimiCn,
    OllamaCloud,
    ConfigurableHttp,
    Cpa,
}

impl ProviderAdapterKind {
    pub const ALL: [Self; 8] = [
        Self::OpenCodeGo,
        Self::ZenFree,
        Self::CommandCodeGoat,
        Self::MiniMaxCn,
        Self::KimiCn,
        Self::OllamaCloud,
        Self::ConfigurableHttp,
        Self::Cpa,
    ];

    pub fn from_provider_id(provider_id: &str) -> Option<Self> {
        match provider_id {
            OPENCODE_PROVIDER_ID => Some(Self::OpenCodeGo),
            OPENCODE_ZEN_FREE_PROVIDER_ID => Some(Self::ZenFree),
            COMMAND_CODE_PROVIDER_ID => Some(Self::CommandCodeGoat),
            MINIMAX_PROVIDER_ID => Some(Self::MiniMaxCn),
            KIMI_PROVIDER_ID => Some(Self::KimiCn),
            OLLAMA_PROVIDER_ID => Some(Self::OllamaCloud),
            CUSTOM_PROVIDER_ID => Some(Self::ConfigurableHttp),
            CPA_PROVIDER_ID => Some(Self::Cpa),
            _ => None,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OpenCodeGo => "opencode_go",
            Self::ZenFree => "zen_free",
            Self::CommandCodeGoat => "command_code_goat",
            Self::MiniMaxCn => "minimax_cn",
            Self::KimiCn => "kimi_cn",
            Self::OllamaCloud => "ollama_cloud",
            Self::ConfigurableHttp => "configurable_http",
            Self::Cpa => "cpa",
        }
    }

    pub const fn product_surface(self) -> ProviderProductSurface {
        match self {
            Self::Cpa => ProviderProductSurface::ExternalIntegration,
            Self::OpenCodeGo
            | Self::ZenFree
            | Self::CommandCodeGoat
            | Self::MiniMaxCn
            | Self::KimiCn
            | Self::OllamaCloud
            | Self::ConfigurableHttp => ProviderProductSurface::Provider,
        }
    }
}

pub fn is_command_code_goat(provider_id: &str) -> bool {
    matches!(
        ProviderAdapterKind::from_provider_id(provider_id),
        Some(ProviderAdapterKind::CommandCodeGoat)
    )
}

/// Normalize a GET `/models` JSON object into a de-duplicated, non-empty
/// Command Code catalog. Rejects arrays at the root, empty snapshots, and
/// oversized directories. First spelling of each case-insensitive ID wins.
pub fn parse_command_code_models_catalog(bytes: &[u8]) -> Result<Vec<String>, String> {
    parse_provider_models_catalog(bytes, "Command Code")
}

/// Normalize a Provider GET `/models` response while retaining the caller's
/// sealed Provider label in validation errors.
pub fn parse_provider_models_catalog(
    bytes: &[u8],
    provider_label: &str,
) -> Result<Vec<String>, String> {
    let value: serde_json::Value = serde_json::from_slice(bytes)
        .map_err(|_| format!("{provider_label} GET /models did not return JSON"))?;
    let object = value
        .as_object()
        .ok_or_else(|| format!("{provider_label} GET /models did not return a JSON object"))?;
    let items = object
        .get("data")
        .or_else(|| object.get("models"))
        .and_then(|value| value.as_array())
        .ok_or_else(|| {
            format!("{provider_label} GET /models did not include a data or models array")
        })?;
    let mut models = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for item in items {
        if models.len() >= MAX_COMMAND_CODE_MODELS_CATALOG {
            return Err(format!(
                "{provider_label} GET /models exceeded {MAX_COMMAND_CODE_MODELS_CATALOG} models"
            ));
        }
        let id = match item {
            serde_json::Value::String(value) => Some(value.as_str()),
            serde_json::Value::Object(object) => object
                .get("id")
                .and_then(|value| value.as_str())
                .or_else(|| object.get("model").and_then(|value| value.as_str())),
            _ => None,
        };
        let Some(id) = id else {
            continue;
        };
        let normalized = validate_custom_model_id(id).map_err(|error| error.to_string())?;
        let key = normalized.to_ascii_lowercase();
        if seen.insert(key) {
            models.push(normalized);
        }
    }
    if models.is_empty() {
        return Err(format!(
            "{provider_label} GET /models returned no usable model ids"
        ));
    }
    Ok(models)
}

pub fn is_custom_api(provider_id: &str) -> bool {
    provider_id == CUSTOM_PROVIDER_ID
}

pub fn is_cpa_external_integration(provider_id: &str) -> bool {
    matches!(
        ProviderAdapterKind::from_provider_id(provider_id),
        Some(ProviderAdapterKind::Cpa)
    )
}

/// Static code-owned registry of built-in providers. Lookup is by
/// `provider_id`; unknown identities fail closed.
pub struct ProviderRegistry;

impl ProviderRegistry {
    pub fn get(provider_id: &str) -> Option<ProviderDescriptor> {
        let plan = builtin_provider(provider_id)?;
        let kind = ProviderAdapterKind::from_provider_id(provider_id)?;
        Some(ProviderDescriptor::from_plan(kind, plan))
    }

    pub fn iter() -> impl Iterator<Item = ProviderDescriptor> {
        BUILTIN_PROVIDERS
            .iter()
            .filter_map(|plan| Self::get(plan.provider_id))
    }
}

/// Composed capability records selected by the sealed adapter kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ProviderCapabilities {
    pub model_catalog: ModelCatalogDescriptor,
    pub inference: InferenceRoutingDescriptor,
    pub protocol_probe: ProtocolProbeDescriptor,
    pub verification: VerificationDescriptor,
    pub usage: UsageDescriptor,
    pub pricing: PricingDescriptor,
    pub card_actions: CardActionsDescriptor,
}

/// Composed capability surfaces for one catalog Provider. These are facts for
/// later persistence/UI; this slice does not change dashboard DTOs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderDescriptor {
    pub kind: ProviderAdapterKind,
    pub provider_id: &'static str,
    pub contract_scope_id: Option<&'static str>,
    pub product_surface: ProviderProductSurface,
    pub model_catalog: ModelCatalogDescriptor,
    pub inference: InferenceRoutingDescriptor,
    pub protocol_probe: ProtocolProbeDescriptor,
    pub verification: VerificationDescriptor,
    pub usage: UsageDescriptor,
    pub pricing: PricingDescriptor,
    pub card_actions: CardActionsDescriptor,
}

impl ProviderDescriptor {
    fn from_plan(kind: ProviderAdapterKind, plan: BuiltinProvider) -> Self {
        Self::from_capabilities(kind, plan, kind.capabilities(plan))
    }

    fn from_capabilities(
        kind: ProviderAdapterKind,
        plan: BuiltinProvider,
        capabilities: ProviderCapabilities,
    ) -> Self {
        Self {
            kind,
            provider_id: plan.provider_id,
            contract_scope_id: plan.contract_scope_id,
            product_surface: plan.product_surface,
            model_catalog: capabilities.model_catalog,
            inference: capabilities.inference,
            protocol_probe: capabilities.protocol_probe,
            verification: capabilities.verification,
            usage: capabilities.usage,
            pricing: capabilities.pricing,
            card_actions: capabilities.card_actions,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelCatalogKind {
    BuiltinGoProtocolTable,
    ZenFreePersistedSnapshot,
    BuiltinCommandCodeProtocolTable,
    ProviderPersistedSnapshot,
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
    OfficialFixed,
    AccountConfigured,
    LocalExternalIntegration,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InferenceAuthDescriptor {
    OpenCodeProtocolDefault,
    Bearer,
    None,
    ProtocolDerivedBearerOrXApiKey,
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
    FixedProviderProtocols,
    AccountDeclaredProtocol,
    FixedStandardProtocols,
}

/// Immutable adapter ceiling for explicit protocol probes. Distinct from
/// static/preset verified support, which still begins from `MODEL_PROTOCOLS`
/// (OpenCode/Zen), Command Code native rows, or the Custom account's declared
/// protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StructuralProbeCeiling {
    /// This adapter does not expose provider-scoped request-path probes.
    Unavailable,
    /// Command Code GOAT has both route families, while each model's sealed
    /// family rule selects the one path worth probing.
    CommandCodeConstructable,
    /// This sealed provider exposes exactly the listed documented paths.
    Fixed(&'static [UpstreamProtocolKind]),
    /// Current-catalog OpenCode Go models: Chat Completions, Responses, and
    /// Messages all have constructable `/v1/...` paths and OpenCode auth.
    OpenCodeConstructable,
    /// Known Zen models share OpenCode constructable paths. Unknown `-free`
    /// IDs stay Chat-only. Anything else is empty.
    ZenFreeConstructable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProtocolProbeDescriptor {
    pub request_path_may_trial: bool,
    pub matrix: ProtocolMatrixKind,
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
    /// When true, dashboard usage projects the process-wide Zen Free
    /// egress-IP cooldown window. Only Zen Free sets this.
    pub egress_ip_shared_cooldown_window: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PricingDescriptor {
    pub availability: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CardVerifyAction {
    NotApplicable,
    Optional,
    UnavailableNotImplemented,
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
    pub protocol_probe: bool,
    pub catalog_refresh: bool,
}

impl ProviderAdapterKind {
    /// Single sealed construction path for each adapter kind. This stays
    /// private so callers consume the immutable [`ProviderDescriptor`].
    fn capabilities(self, plan: BuiltinProvider) -> ProviderCapabilities {
        match self {
            Self::OpenCodeGo => open_code_go_capabilities(plan),
            Self::ZenFree => zen_free_capabilities(plan),
            Self::CommandCodeGoat => command_code_goat_capabilities(plan),
            Self::MiniMaxCn => minimax_cn_capabilities(plan),
            Self::KimiCn => kimi_cn_capabilities(plan),
            Self::OllamaCloud => ollama_cloud_capabilities(plan),
            Self::ConfigurableHttp => configurable_http_capabilities(plan),
            Self::Cpa => cpa_capabilities(plan),
        }
    }
}

fn catalog_pricing(plan: BuiltinProvider) -> PricingDescriptor {
    PricingDescriptor {
        availability: plan.pricing_availability,
    }
}

fn open_code_go_capabilities(plan: BuiltinProvider) -> ProviderCapabilities {
    ProviderCapabilities {
        model_catalog: ModelCatalogDescriptor {
            kind: ModelCatalogKind::BuiltinGoProtocolTable,
            catalog_source: plan.model_source,
            publishes_client_aliases: true,
            admin_explicit_refresh: true,
            overlays_declared_ids: false,
            snapshot_is_adapter_input_only: false,
        },
        inference: InferenceRoutingDescriptor {
            catalog_routable: plan.routable,
            production_inference: true,
            channel: Some(InferenceChannelKind::Go),
            credential_kind: plan.credential_kind,
            quota_scope: plan.quota_scope,
            auth: InferenceAuthDescriptor::OpenCodeProtocolDefault,
            follow_redirects: true,
            origin: InferenceOriginKind::OfficialFixed,
            loopback_test_seam_only: false,
        },
        protocol_probe: ProtocolProbeDescriptor {
            request_path_may_trial: false,
            matrix: ProtocolMatrixKind::OpenCodeModelProtocols,
            fallback_priority: PROTOCOL_FALLBACK_CHAT_RESPONSES_MESSAGES,
            explicit_probe: true,
            structural_ceiling: StructuralProbeCeiling::OpenCodeConstructable,
        },
        verification: VerificationDescriptor {
            policy: plan.verification_policy,
            runtime_availability: plan.verification_runtime_availability,
            never_auto_enable: false,
            probe_first_declared_model: false,
            uses_get_models: false,
        },
        usage: UsageDescriptor {
            catalog_availability: plan.usage_availability,
            contract: UsageContractKind::Authoritative,
            endpoint: Some(OPENCODE_GO_USAGE_URL),
            experimental: false,
            automatic_sync: true,
            authoritative_for_quota: true,
            affects_inference_eligibility: false,
            publishes_capability: true,
            manual_calibration: plan.manual_usage_calibration,
            egress_ip_shared_cooldown_window: false,
        },
        pricing: catalog_pricing(plan),
        card_actions: CardActionsDescriptor {
            persisted_enable_allowed: plan.routable,
            enable_requires_verification: false,
            managed_registration: plan.managed_registration,
            fetch_zen_models: false,
            discover_models: false,
            usage_refresh: true,
            manual_usage_calibration: plan.manual_usage_calibration,
            connection_verify: CardVerifyAction::Optional,
            protocol_and_auth_immutable_after_create: false,
            protocol_probe: true,
            catalog_refresh: true,
        },
    }
}

fn zen_free_capabilities(plan: BuiltinProvider) -> ProviderCapabilities {
    ProviderCapabilities {
        model_catalog: ModelCatalogDescriptor {
            kind: ModelCatalogKind::ZenFreePersistedSnapshot,
            catalog_source: plan.model_source,
            publishes_client_aliases: true,
            admin_explicit_refresh: true,
            overlays_declared_ids: false,
            snapshot_is_adapter_input_only: false,
        },
        inference: InferenceRoutingDescriptor {
            catalog_routable: plan.routable,
            production_inference: true,
            channel: Some(InferenceChannelKind::Free),
            credential_kind: plan.credential_kind,
            quota_scope: plan.quota_scope,
            auth: InferenceAuthDescriptor::None,
            follow_redirects: true,
            origin: InferenceOriginKind::OfficialFixed,
            loopback_test_seam_only: false,
        },
        protocol_probe: ProtocolProbeDescriptor {
            request_path_may_trial: false,
            matrix: ProtocolMatrixKind::OpenCodeModelProtocols,
            fallback_priority: PROTOCOL_FALLBACK_CHAT_RESPONSES_MESSAGES,
            explicit_probe: true,
            structural_ceiling: StructuralProbeCeiling::ZenFreeConstructable,
        },
        verification: VerificationDescriptor {
            policy: plan.verification_policy,
            runtime_availability: plan.verification_runtime_availability,
            never_auto_enable: false,
            probe_first_declared_model: false,
            uses_get_models: false,
        },
        usage: UsageDescriptor {
            catalog_availability: plan.usage_availability,
            contract: UsageContractKind::Unavailable,
            endpoint: None,
            experimental: false,
            automatic_sync: false,
            authoritative_for_quota: false,
            affects_inference_eligibility: false,
            publishes_capability: false,
            manual_calibration: plan.manual_usage_calibration,
            egress_ip_shared_cooldown_window: true,
        },
        pricing: catalog_pricing(plan),
        card_actions: CardActionsDescriptor {
            persisted_enable_allowed: plan.routable,
            enable_requires_verification: false,
            managed_registration: plan.managed_registration,
            fetch_zen_models: true,
            discover_models: false,
            usage_refresh: false,
            manual_usage_calibration: plan.manual_usage_calibration,
            connection_verify: CardVerifyAction::NotApplicable,
            protocol_and_auth_immutable_after_create: false,
            protocol_probe: true,
            catalog_refresh: true,
        },
    }
}

fn command_code_goat_capabilities(plan: BuiltinProvider) -> ProviderCapabilities {
    ProviderCapabilities {
        model_catalog: ModelCatalogDescriptor {
            kind: ModelCatalogKind::BuiltinCommandCodeProtocolTable,
            catalog_source: plan.model_source,
            publishes_client_aliases: true,
            admin_explicit_refresh: true,
            overlays_declared_ids: false,
            snapshot_is_adapter_input_only: false,
        },
        inference: InferenceRoutingDescriptor {
            catalog_routable: plan.routable,
            production_inference: true,
            channel: Some(InferenceChannelKind::Go),
            credential_kind: plan.credential_kind,
            quota_scope: plan.quota_scope,
            auth: InferenceAuthDescriptor::Bearer,
            follow_redirects: false,
            origin: InferenceOriginKind::OfficialFixed,
            loopback_test_seam_only: false,
        },
        protocol_probe: ProtocolProbeDescriptor {
            request_path_may_trial: false,
            matrix: ProtocolMatrixKind::CommandCodeNative,
            fallback_priority: PROTOCOL_FALLBACK_CHAT_MESSAGES,
            explicit_probe: true,
            structural_ceiling: StructuralProbeCeiling::CommandCodeConstructable,
        },
        verification: VerificationDescriptor {
            policy: plan.verification_policy,
            runtime_availability: plan.verification_runtime_availability,
            never_auto_enable: false,
            probe_first_declared_model: false,
            uses_get_models: false,
        },
        usage: UsageDescriptor {
            catalog_availability: plan.usage_availability,
            contract: UsageContractKind::LocalState,
            endpoint: None,
            experimental: false,
            automatic_sync: false,
            authoritative_for_quota: false,
            affects_inference_eligibility: false,
            publishes_capability: true,
            manual_calibration: plan.manual_usage_calibration,
            egress_ip_shared_cooldown_window: false,
        },
        pricing: catalog_pricing(plan),
        card_actions: CardActionsDescriptor {
            persisted_enable_allowed: plan.routable,
            enable_requires_verification: false,
            managed_registration: plan.managed_registration,
            fetch_zen_models: false,
            discover_models: false,
            usage_refresh: false,
            manual_usage_calibration: plan.manual_usage_calibration,
            connection_verify: CardVerifyAction::NotApplicable,
            protocol_and_auth_immutable_after_create: false,
            protocol_probe: true,
            catalog_refresh: true,
        },
    }
}

fn minimax_cn_capabilities(plan: BuiltinProvider) -> ProviderCapabilities {
    ProviderCapabilities {
        model_catalog: ModelCatalogDescriptor {
            kind: ModelCatalogKind::ProviderPersistedSnapshot,
            catalog_source: plan.model_source,
            publishes_client_aliases: true,
            admin_explicit_refresh: true,
            overlays_declared_ids: false,
            snapshot_is_adapter_input_only: false,
        },
        inference: InferenceRoutingDescriptor {
            catalog_routable: plan.routable,
            production_inference: true,
            channel: Some(InferenceChannelKind::Go),
            credential_kind: plan.credential_kind,
            quota_scope: plan.quota_scope,
            auth: InferenceAuthDescriptor::Bearer,
            follow_redirects: false,
            origin: InferenceOriginKind::OfficialFixed,
            loopback_test_seam_only: false,
        },
        protocol_probe: ProtocolProbeDescriptor {
            request_path_may_trial: false,
            matrix: ProtocolMatrixKind::FixedProviderProtocols,
            fallback_priority: &CHAT_MESSAGES_PROTOCOLS,
            explicit_probe: true,
            structural_ceiling: StructuralProbeCeiling::Fixed(&CHAT_MESSAGES_PROTOCOLS),
        },
        verification: VerificationDescriptor {
            policy: plan.verification_policy,
            runtime_availability: plan.verification_runtime_availability,
            never_auto_enable: false,
            probe_first_declared_model: false,
            uses_get_models: false,
        },
        usage: UsageDescriptor {
            catalog_availability: plan.usage_availability,
            contract: UsageContractKind::Authoritative,
            endpoint: Some(MINIMAX_CN_USAGE_URL),
            experimental: false,
            automatic_sync: false,
            authoritative_for_quota: false,
            affects_inference_eligibility: false,
            publishes_capability: true,
            manual_calibration: false,
            egress_ip_shared_cooldown_window: false,
        },
        pricing: catalog_pricing(plan),
        card_actions: CardActionsDescriptor {
            persisted_enable_allowed: plan.routable,
            enable_requires_verification: false,
            managed_registration: false,
            fetch_zen_models: false,
            discover_models: false,
            usage_refresh: true,
            manual_usage_calibration: false,
            connection_verify: CardVerifyAction::NotApplicable,
            protocol_and_auth_immutable_after_create: false,
            protocol_probe: true,
            catalog_refresh: true,
        },
    }
}

fn ollama_cloud_capabilities(plan: BuiltinProvider) -> ProviderCapabilities {
    ProviderCapabilities {
        model_catalog: ModelCatalogDescriptor {
            kind: ModelCatalogKind::ProviderPersistedSnapshot,
            catalog_source: plan.model_source,
            publishes_client_aliases: true,
            admin_explicit_refresh: true,
            overlays_declared_ids: false,
            snapshot_is_adapter_input_only: false,
        },
        inference: InferenceRoutingDescriptor {
            catalog_routable: plan.routable,
            production_inference: true,
            channel: Some(InferenceChannelKind::Go),
            credential_kind: plan.credential_kind,
            quota_scope: plan.quota_scope,
            auth: InferenceAuthDescriptor::Bearer,
            follow_redirects: false,
            origin: InferenceOriginKind::OfficialFixed,
            loopback_test_seam_only: false,
        },
        protocol_probe: ProtocolProbeDescriptor {
            request_path_may_trial: false,
            matrix: ProtocolMatrixKind::FixedProviderProtocols,
            fallback_priority: &CHAT_PROTOCOLS,
            explicit_probe: false,
            structural_ceiling: StructuralProbeCeiling::Unavailable,
        },
        verification: VerificationDescriptor {
            policy: plan.verification_policy,
            runtime_availability: plan.verification_runtime_availability,
            never_auto_enable: false,
            probe_first_declared_model: false,
            uses_get_models: false,
        },
        usage: UsageDescriptor {
            catalog_availability: plan.usage_availability,
            contract: UsageContractKind::LocalState,
            endpoint: None,
            experimental: false,
            automatic_sync: false,
            authoritative_for_quota: false,
            affects_inference_eligibility: false,
            publishes_capability: true,
            manual_calibration: true,
            egress_ip_shared_cooldown_window: false,
        },
        pricing: catalog_pricing(plan),
        card_actions: CardActionsDescriptor {
            persisted_enable_allowed: plan.routable,
            enable_requires_verification: false,
            managed_registration: false,
            fetch_zen_models: false,
            discover_models: false,
            usage_refresh: false,
            manual_usage_calibration: true,
            connection_verify: CardVerifyAction::NotApplicable,
            protocol_and_auth_immutable_after_create: false,
            protocol_probe: false,
            catalog_refresh: true,
        },
    }
}

fn kimi_cn_capabilities(plan: BuiltinProvider) -> ProviderCapabilities {
    ProviderCapabilities {
        model_catalog: ModelCatalogDescriptor {
            kind: ModelCatalogKind::ProviderPersistedSnapshot,
            catalog_source: plan.model_source,
            publishes_client_aliases: true,
            admin_explicit_refresh: true,
            overlays_declared_ids: false,
            snapshot_is_adapter_input_only: false,
        },
        inference: InferenceRoutingDescriptor {
            catalog_routable: plan.routable,
            production_inference: true,
            channel: Some(InferenceChannelKind::Go),
            credential_kind: plan.credential_kind,
            quota_scope: plan.quota_scope,
            auth: InferenceAuthDescriptor::Bearer,
            follow_redirects: false,
            origin: InferenceOriginKind::OfficialFixed,
            loopback_test_seam_only: false,
        },
        protocol_probe: ProtocolProbeDescriptor {
            request_path_may_trial: false,
            matrix: ProtocolMatrixKind::FixedProviderProtocols,
            fallback_priority: &CHAT_MESSAGES_PROTOCOLS,
            explicit_probe: true,
            structural_ceiling: StructuralProbeCeiling::Fixed(&CHAT_MESSAGES_PROTOCOLS),
        },
        verification: VerificationDescriptor {
            policy: plan.verification_policy,
            runtime_availability: plan.verification_runtime_availability,
            never_auto_enable: false,
            probe_first_declared_model: false,
            uses_get_models: false,
        },
        usage: UsageDescriptor {
            catalog_availability: plan.usage_availability,
            contract: UsageContractKind::Authoritative,
            endpoint: Some(KIMI_CN_USAGE_URL),
            experimental: false,
            automatic_sync: false,
            authoritative_for_quota: false,
            affects_inference_eligibility: false,
            publishes_capability: true,
            manual_calibration: false,
            egress_ip_shared_cooldown_window: false,
        },
        pricing: catalog_pricing(plan),
        card_actions: CardActionsDescriptor {
            persisted_enable_allowed: plan.routable,
            enable_requires_verification: false,
            managed_registration: false,
            fetch_zen_models: false,
            discover_models: false,
            usage_refresh: true,
            manual_usage_calibration: false,
            connection_verify: CardVerifyAction::NotApplicable,
            protocol_and_auth_immutable_after_create: false,
            protocol_probe: true,
            catalog_refresh: true,
        },
    }
}

fn configurable_http_capabilities(plan: BuiltinProvider) -> ProviderCapabilities {
    ProviderCapabilities {
        model_catalog: ModelCatalogDescriptor {
            kind: ModelCatalogKind::AccountDeclaredCapabilities,
            catalog_source: plan.model_source,
            publishes_client_aliases: false,
            admin_explicit_refresh: false,
            overlays_declared_ids: true,
            snapshot_is_adapter_input_only: false,
        },
        inference: InferenceRoutingDescriptor {
            catalog_routable: plan.routable,
            production_inference: true,
            channel: Some(InferenceChannelKind::Go),
            credential_kind: plan.credential_kind,
            quota_scope: plan.quota_scope,
            auth: InferenceAuthDescriptor::ProtocolDerivedBearerOrXApiKey,
            follow_redirects: false,
            origin: InferenceOriginKind::AccountConfigured,
            loopback_test_seam_only: false,
        },
        protocol_probe: ProtocolProbeDescriptor {
            request_path_may_trial: false,
            matrix: ProtocolMatrixKind::AccountDeclaredProtocol,
            fallback_priority: PROTOCOL_FALLBACK_CHAT_RESPONSES_MESSAGES,
            explicit_probe: false,
            structural_ceiling: StructuralProbeCeiling::Unavailable,
        },
        verification: VerificationDescriptor {
            policy: plan.verification_policy,
            runtime_availability: plan.verification_runtime_availability,
            never_auto_enable: true,
            probe_first_declared_model: true,
            uses_get_models: false,
        },
        usage: UsageDescriptor {
            catalog_availability: plan.usage_availability,
            contract: UsageContractKind::Unavailable,
            endpoint: None,
            experimental: false,
            automatic_sync: false,
            authoritative_for_quota: false,
            affects_inference_eligibility: false,
            publishes_capability: false,
            manual_calibration: plan.manual_usage_calibration,
            egress_ip_shared_cooldown_window: false,
        },
        pricing: catalog_pricing(plan),
        card_actions: CardActionsDescriptor {
            persisted_enable_allowed: plan.routable,
            enable_requires_verification: false,
            managed_registration: plan.managed_registration,
            fetch_zen_models: false,
            discover_models: true,
            usage_refresh: false,
            manual_usage_calibration: plan.manual_usage_calibration,
            connection_verify: CardVerifyAction::Optional,
            protocol_and_auth_immutable_after_create: false,
            protocol_probe: false,
            catalog_refresh: false,
        },
    }
}

fn cpa_capabilities(plan: BuiltinProvider) -> ProviderCapabilities {
    ProviderCapabilities {
        model_catalog: ModelCatalogDescriptor {
            kind: ModelCatalogKind::ProviderPersistedSnapshot,
            catalog_source: plan.model_source,
            publishes_client_aliases: true,
            admin_explicit_refresh: true,
            overlays_declared_ids: false,
            snapshot_is_adapter_input_only: false,
        },
        inference: InferenceRoutingDescriptor {
            catalog_routable: plan.routable,
            production_inference: true,
            // CPA participates in ordinary keyed account selection rather than
            // the Zen egress-IP special channel.
            channel: Some(InferenceChannelKind::Go),
            credential_kind: plan.credential_kind,
            quota_scope: plan.quota_scope,
            auth: InferenceAuthDescriptor::Bearer,
            follow_redirects: false,
            origin: InferenceOriginKind::LocalExternalIntegration,
            loopback_test_seam_only: false,
        },
        protocol_probe: ProtocolProbeDescriptor {
            request_path_may_trial: false,
            matrix: ProtocolMatrixKind::FixedStandardProtocols,
            fallback_priority: PROTOCOL_FALLBACK_CHAT_RESPONSES_MESSAGES,
            explicit_probe: false,
            structural_ceiling: StructuralProbeCeiling::Unavailable,
        },
        verification: VerificationDescriptor {
            policy: plan.verification_policy,
            runtime_availability: plan.verification_runtime_availability,
            never_auto_enable: true,
            probe_first_declared_model: false,
            uses_get_models: true,
        },
        usage: UsageDescriptor {
            catalog_availability: plan.usage_availability,
            contract: UsageContractKind::Unavailable,
            endpoint: None,
            experimental: false,
            automatic_sync: false,
            authoritative_for_quota: false,
            affects_inference_eligibility: false,
            publishes_capability: false,
            manual_calibration: false,
            egress_ip_shared_cooldown_window: false,
        },
        pricing: catalog_pricing(plan),
        card_actions: CardActionsDescriptor {
            persisted_enable_allowed: plan.routable,
            enable_requires_verification: false,
            managed_registration: false,
            fetch_zen_models: false,
            discover_models: false,
            usage_refresh: false,
            manual_usage_calibration: false,
            connection_verify: CardVerifyAction::NotApplicable,
            protocol_and_auth_immutable_after_create: true,
            protocol_probe: false,
            catalog_refresh: true,
        },
    }
}

pub fn default_verification_status(plan: BuiltinProvider) -> ConnectionVerificationStatus {
    match plan.verification_policy {
        VerificationPolicy::NotRequired => ConnectionVerificationStatus::NotRequired,
        VerificationPolicy::Required => ConnectionVerificationStatus::Pending,
    }
}

/// Catalog-backed enablement capability. Only `routable` providers may persist
/// `enabled=true`. Unknown providers fail closed.
pub const fn plan_allows_enablement(plan: BuiltinProvider) -> bool {
    plan.routable
}

pub fn provider_allows_enablement(provider_id: &str) -> bool {
    builtin_provider(provider_id).is_some_and(plan_allows_enablement)
}

/// Reject `enabled=true` for catalogued-but-unroutable providers. Disabled
/// drafts skip the check so they can still be created and edited.
pub fn ensure_enabled_provider_is_routable(
    provider_id: &str,
    enabled: bool,
) -> Result<(), ProviderBindingError> {
    if !enabled {
        return Ok(());
    }
    ensure_provider_can_enable(provider_id)
}

pub fn ensure_provider_can_enable(provider_id: &str) -> Result<(), ProviderBindingError> {
    match builtin_provider(provider_id) {
        Some(plan) if plan_allows_enablement(plan) => Ok(()),
        Some(plan) => Err(ProviderBindingError::EnablementNotRoutable {
            provider_id: plan.provider_id,
            display_name: plan.display_name,
        }),
        None => Err(ProviderBindingError::UnknownProvider {
            provider_id: provider_id.to_string(),
        }),
    }
}

pub fn plan_requires_custom_config(plan: BuiltinProvider) -> bool {
    matches!(
        ProviderAdapterKind::from_provider_id(plan.provider_id),
        Some(ProviderAdapterKind::ConfigurableHttp)
    )
}

pub fn validate_plan_key(plan: BuiltinProvider, key: &str) -> Result<(), ProviderBindingError> {
    if plan.credential_kind == CredentialKind::None {
        return Ok(());
    }
    let trimmed = key.trim();
    if trimmed.is_empty() {
        return Err(ProviderBindingError::KeyRequired);
    }
    if let Some(prefix) = plan.key_prefix
        && !trimmed.starts_with(prefix)
    {
        return Err(ProviderBindingError::KeyPrefixMismatch {
            provider_id: plan.provider_id.to_string(),
            prefix: prefix.to_string(),
        });
    }
    Ok(())
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
    credential_kind: CredentialKind,
    quota_scope: QuotaScope,
) -> Result<(), ProviderBindingError> {
    let provider =
        builtin_provider(provider_id).ok_or_else(|| ProviderBindingError::UnknownProvider {
            provider_id: provider_id.to_string(),
        })?;
    if provider.credential_kind != credential_kind || provider.quota_scope != quota_scope {
        return Err(ProviderBindingError::BindingMismatch {
            provider_id: provider_id.to_string(),
        });
    }
    match provider.singleton_account_id {
        Some(singleton) if account_id != singleton => {
            Err(ProviderBindingError::SingletonAccountRequired(singleton))
        }
        None if account_id == ZEN_FREE_ACCOUNT_ID || account_id == CPA_ACCOUNT_ID => Err(
            ProviderBindingError::ReservedAccountId(if account_id == CPA_ACCOUNT_ID {
                CPA_ACCOUNT_ID
            } else {
                ZEN_FREE_ACCOUNT_ID
            }),
        ),
        _ => Ok(()),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderBindingError {
    UnknownProvider {
        provider_id: String,
    },
    UnknownCredentialKind(String),
    UnknownQuotaScope(String),
    BindingMismatch {
        provider_id: String,
    },
    SingletonAccountRequired(&'static str),
    ReservedAccountId(&'static str),
    UnknownVerificationStatus(String),
    UnknownUpstreamProtocol(String),
    UnknownAuthScheme(String),
    KeyRequired,
    KeyPrefixMismatch {
        provider_id: String,
        prefix: String,
    },
    InvalidCustomBaseUrl(String),
    InvalidProviderName(String),
    InvalidModelId(String),
    EnablementNotRoutable {
        provider_id: &'static str,
        display_name: &'static str,
    },
}

impl fmt::Display for ProviderBindingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownProvider { provider_id } => {
                write!(f, "unknown provider `{provider_id}`")
            }
            Self::UnknownCredentialKind(value) => {
                write!(f, "unknown credential kind `{value}`")
            }
            Self::UnknownQuotaScope(value) => write!(f, "unknown quota scope `{value}`"),
            Self::BindingMismatch { provider_id } => {
                write!(f, "provider binding does not match `{provider_id}`")
            }
            Self::SingletonAccountRequired(id) => {
                write!(f, "provider requires singleton account `{id}`")
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
                prefix,
            } => write!(f, "provider `{provider_id}` requires key prefix `{prefix}`"),
            Self::InvalidCustomBaseUrl(message)
            | Self::InvalidProviderName(message)
            | Self::InvalidModelId(message) => f.write_str(message),
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

#[cfg(test)]
mod tests;

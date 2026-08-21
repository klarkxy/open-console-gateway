use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

pub const OPENCODE_PROVIDER_ID: &str = "opencode";
pub const COMMAND_CODE_PROVIDER_ID: &str = "command-code";
pub const OPENCODE_ZEN_FREE_PROVIDER_ID: &str = "opencode-zen-free";
pub const SCNET_PROVIDER_ID: &str = "scnet";
pub const CUSTOM_PROVIDER_ID: &str = "custom";

pub const GO_OFFERING_ID: &str = "go";
pub const GOAT_OFFERING_ID: &str = "goat";
pub const ANONYMOUS_FREE_OFFERING_ID: &str = "anonymous-free";

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
/// Client-facing Alias. Go still owns the published kebab alias; GOAT maps it
/// internally to the slash raw ID and stays non-routeable.
pub const COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_ALIAS: &str = "deepseek-v4-flash";
/// Unique exact upstream raw ID for Command Code GOAT.
pub const COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM: &str = "deepseek/deepseek-v4-flash";
pub const SCNET_TOKEN_PLAN_BASIC_OFFERING_ID: &str = "token-plan-basic";
pub const SCNET_TOKEN_PLAN_STANDARD_OFFERING_ID: &str = "token-plan-standard";
pub const SCNET_TOKEN_PLAN_PREMIUM_OFFERING_ID: &str = "token-plan-premium";
pub const CUSTOM_API_OFFERING_ID: &str = "api";

pub const SCNET_TOKEN_PLAN_KEY_PREFIX: &str = "sk-tp-";
pub const SCNET_RISK_ACKNOWLEDGEMENT_ID: &str = "scnet-token-plan-restrictions";
pub const SCNET_RISK_ACKNOWLEDGEMENT_VERSION: &str = "2026-08-21";
pub const SCNET_RISK_ACKNOWLEDGEMENT_SOURCE_URL: &str =
    "https://www.scnet.cn/ac/openapi/doc/2.0/moduleapi/plans/token-plan.html";
pub const SCNET_RISK_ACKNOWLEDGEMENT_BODY: &str = "SCNet Token Plan keys (sk-tp-) are limited to interactive use inside AI tools. Account sharing and using the API as a custom application backend, automation script, or non-interactive batch caller is prohibited and may suspend the subscription or revoke the key.";

pub const QUOTA_WINDOW_FIVE_HOURS: &str = "five_hours";
pub const QUOTA_WINDOW_WEEK: &str = "week";
pub const QUOTA_WINDOW_MONTH: &str = "month";
pub const QUOTA_WINDOW_FREE: &str = "free";

/// Reserved account row representing the egress-IP-scoped OpenCode Zen free
/// route. It is created by schema migration, never by the generic account API.
pub const ZEN_FREE_ACCOUNT_ID: &str = "00000000-0000-0000-0000-000000000002";
pub const ZEN_FREE_ACCOUNT_NAME: &str = "OpenCode Zen Free";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CredentialKind {
    ApiKey,
    None,
}

impl CredentialKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ApiKey => "api_key",
            Self::None => "none",
        }
    }
}

impl TryFrom<&str> for CredentialKind {
    type Error = ProviderBindingError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "api_key" => Ok(Self::ApiKey),
            "none" => Ok(Self::None),
            _ => Err(ProviderBindingError::UnknownCredentialKind(
                value.to_string(),
            )),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum QuotaScope {
    Key,
    EgressIp,
}

impl QuotaScope {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Key => "key",
            Self::EgressIp => "egress-ip",
        }
    }
}

impl TryFrom<&str> for QuotaScope {
    type Error = ProviderBindingError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "key" => Ok(Self::Key),
            "egress-ip" => Ok(Self::EgressIp),
            _ => Err(ProviderBindingError::UnknownQuotaScope(value.to_string())),
        }
    }
}

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpstreamProtocolKind {
    ChatCompletions,
    Responses,
    Messages,
}

impl UpstreamProtocolKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ChatCompletions => "chat_completions",
            Self::Responses => "responses",
            Self::Messages => "messages",
        }
    }
}

impl TryFrom<&str> for UpstreamProtocolKind {
    type Error = ProviderBindingError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "chat_completions" => Ok(Self::ChatCompletions),
            "responses" => Ok(Self::Responses),
            "messages" => Ok(Self::Messages),
            _ => Err(ProviderBindingError::UnknownUpstreamProtocol(
                value.to_string(),
            )),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum UpstreamAuthScheme {
    Bearer,
    XApiKey,
}

impl UpstreamAuthScheme {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Bearer => "bearer",
            Self::XApiKey => "x-api-key",
        }
    }
}

impl TryFrom<&str> for UpstreamAuthScheme {
    type Error = ProviderBindingError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "bearer" => Ok(Self::Bearer),
            "x-api-key" => Ok(Self::XApiKey),
            _ => Err(ProviderBindingError::UnknownAuthScheme(value.to_string())),
        }
    }
}

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
        quota_unit: "credits",
        model_source: "pending_runtime",
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
        quota_unit: "credits",
        model_source: "pending_runtime",
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
        quota_unit: "credits",
        model_source: "pending_runtime",
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
        verification_runtime_availability: "unavailable",
        routable: false,
        managed_registration: false,
        pricing_availability: "unpriced",
        usage_availability: "unavailable",
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

pub fn is_command_code_goat(provider_id: &str, offering_id: &str) -> bool {
    provider_id == COMMAND_CODE_PROVIDER_ID && offering_id == GOAT_OFFERING_ID
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

pub fn plan_requires_custom_config(plan: BuiltinPlan) -> bool {
    plan.offering.provider_id == CUSTOM_PROVIDER_ID
        && plan.offering.offering_id == CUSTOM_API_OFFERING_ID
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

/// Syntactic Custom base-URL gate. DNS / connected-IP revalidation belongs to
/// the later Custom HTTP client slice and is intentionally not performed here.
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
    if parsed.query().is_some() || parsed.fragment().is_some() {
        return Err(ProviderBindingError::InvalidCustomBaseUrl(
            "base URL must not include a query or fragment".to_string(),
        ));
    }
    let host = parsed.host_str().ok_or_else(|| {
        ProviderBindingError::InvalidCustomBaseUrl("base URL must include a host".to_string())
    })?;
    if let Ok(ip) = host.parse::<IpAddr>() {
        if !ip_is_allowed_custom_target(ip) {
            return Err(ProviderBindingError::InvalidCustomBaseUrl(
                "base URL host is not a permitted public or loopback address".to_string(),
            ));
        }
        if parsed.scheme() == "http" && !ip.is_loopback() {
            return Err(ProviderBindingError::InvalidCustomBaseUrl(
                "non-loopback Custom base URLs must use https".to_string(),
            ));
        }
    } else {
        let host_lower = host.to_ascii_lowercase();
        if host_lower == "localhost" || host_lower.ends_with(".localhost") {
            // Loopback hostnames are persisted; runtime still re-checks the
            // connected address before any Custom request.
        } else if is_blocked_custom_hostname(&host_lower) {
            return Err(ProviderBindingError::InvalidCustomBaseUrl(
                "base URL host is not a permitted Custom target".to_string(),
            ));
        } else if parsed.scheme() != "https" {
            return Err(ProviderBindingError::InvalidCustomBaseUrl(
                "non-loopback Custom base URLs must use https".to_string(),
            ));
        }
    }
    Ok(parsed.as_str().trim_end_matches('/').to_string())
}

fn ip_is_allowed_custom_target(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(addr) => {
            addr.is_loopback()
                || !(addr.is_private()
                    || addr.is_link_local()
                    || addr.is_broadcast()
                    || addr.is_documentation()
                    || addr.is_unspecified()
                    || addr.is_multicast()
                    || is_carrier_grade_nat(addr)
                    || addr.octets()[0] == 0)
        }
        IpAddr::V6(addr) => {
            if let Some(v4) = addr.to_ipv4_mapped() {
                return ip_is_allowed_custom_target(IpAddr::V4(v4));
            }
            addr.is_loopback()
                || !(addr.is_unspecified()
                    || addr.is_multicast()
                    || is_unique_local_ipv6(addr)
                    || (addr.segments()[0] & 0xffc0) == 0xfe80)
        }
    }
}

fn is_carrier_grade_nat(addr: Ipv4Addr) -> bool {
    let octets = addr.octets();
    octets[0] == 100 && (octets[1] & 0b1100_0000) == 64
}

fn is_unique_local_ipv6(addr: Ipv6Addr) -> bool {
    (addr.segments()[0] & 0xfe00) == 0xfc00
}

fn is_blocked_custom_hostname(host: &str) -> bool {
    matches!(
        host,
        "metadata.google.internal"
            | "metadata"
            | "kubernetes"
            | "kubernetes.default"
            | "kubernetes.default.svc"
    ) || host.ends_with(".internal")
        || host.ends_with(".local")
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
        }
    }
}

impl std::error::Error for ProviderBindingError {}

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
        assert!(!custom.routable);
        assert_eq!(custom.pricing_availability, "unpriced");
        assert_eq!(custom.usage_availability, "unavailable");
        assert!(plan_requires_custom_config(custom));

        let go = builtin_plan(OPENCODE_PROVIDER_ID, GO_OFFERING_ID).unwrap();
        assert!(go.routable);
        assert_eq!(
            default_verification_status(go),
            ConnectionVerificationStatus::NotRequired
        );
    }

    #[test]
    fn custom_base_url_and_model_ids_fail_closed_without_dns() {
        assert!(validate_custom_base_url("https://api.example.com/v1").is_ok());
        assert!(validate_custom_base_url("http://127.0.0.1:8080/v1").is_ok());
        assert!(validate_custom_base_url("http://localhost:3000").is_ok());
        assert!(validate_custom_base_url("https://user:pass@api.example.com").is_err());
        assert!(validate_custom_base_url("https://api.example.com/v1?x=1").is_err());
        assert!(validate_custom_base_url("https://192.168.1.8/v1").is_err());
        assert!(validate_custom_base_url("https://169.254.169.254/latest").is_err());
        assert!(validate_custom_base_url("http://api.example.com/v1").is_err());
        assert_eq!(
            validate_custom_model_id("deepseek/deepseek-v4-flash").unwrap(),
            "deepseek/deepseek-v4-flash"
        );
        assert!(validate_custom_model_id("").is_err());
    }

    #[test]
    fn scnet_acknowledgement_hash_is_stable() {
        let notice = SCNET_RISK_NOTICE;
        assert_eq!(
            notice.content_hash(),
            acknowledgement_content_hash(SCNET_RISK_ACKNOWLEDGEMENT_BODY)
        );
        assert_ne!(notice.content_hash(), acknowledgement_content_hash("other"));
    }
}

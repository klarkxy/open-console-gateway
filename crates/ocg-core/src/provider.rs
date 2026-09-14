//! Provider catalog compatibility facade.
//!
//! Pure catalog, adapter, registry, and binding types live in
//! [`ocg_domain::provider`] and are re-exported here item-by-item so
//! `ocg_core::provider::*` paths stay stable. Host-specific records live with
//! their owners: Custom URL validation in [`crate::custom_http`] (re-exported
//! via [`crate::custom`]), quota/credit and usage-sync records in
//! [`crate::models`], pricing storage records in [`crate::pricing`], and GOAT
//! runtime records in [`crate::goat`].

pub use crate::kernel::catalog::{
    CatalogParseError, CredentialKind, QuotaScope, UpstreamAuthScheme, UpstreamProtocolKind,
};
pub use crate::kernel::ids::{
    COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_ALIAS, COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM,
    COMMAND_CODE_PROVIDER_ID, CPA_ACCOUNT_ID, CPA_ACCOUNT_NAME, CPA_PROVIDER_ID,
    CUSTOM_PROVIDER_ID, KIMI_PROVIDER_ID, MINIMAX_PROVIDER_ID, OLLAMA_CLOUD_BASE_URL,
    OLLAMA_CLOUD_CHAT_COMPLETIONS_PATH, OLLAMA_CLOUD_MODELS_PATH, OLLAMA_CLOUD_PRICING_URL,
    OLLAMA_PROVIDER_ID, OPENCODE_PROVIDER_ID, OPENCODE_ZEN_FREE_PROVIDER_ID, ZEN_FREE_ACCOUNT_ID,
    ZEN_FREE_ACCOUNT_NAME,
};

pub use ocg_domain::provider::{
    BUILTIN_PROVIDERS, BuiltinProvider, COMMAND_CODE_GOAT_BASE_URL,
    COMMAND_CODE_GOAT_CHAT_COMPLETIONS_PATH, COMMAND_CODE_GOAT_HOST,
    COMMAND_CODE_GOAT_INCLUDED_MODEL_IDS, COMMAND_CODE_GOAT_MESSAGES_PATH,
    COMMAND_CODE_GOAT_MODEL_SOURCE, COMMAND_CODE_GOAT_MODELS_PATH, COMMAND_CODE_GOAT_MODELS_SOURCE,
    COMMAND_CODE_GOAT_QUOTA_5H, COMMAND_CODE_GOAT_QUOTA_MONTH, COMMAND_CODE_GOAT_QUOTA_WEEK,
    CardActionsDescriptor, CardVerifyAction, ConnectionVerificationStatus, CreationAvailability,
    InferenceAuthDescriptor, InferenceChannelKind, InferenceOriginKind, InferenceRoutingDescriptor,
    KIMI_CN_BASE_URL, KIMI_CN_CHAT_COMPLETIONS_PATH, KIMI_CN_MESSAGES_PATH, KIMI_CN_MODELS_PATH,
    KIMI_CN_USAGE_URL, MAX_OLLAMA_CLOUD_MODELS_CATALOG, MINIMAX_CN_ANTHROPIC_BASE_URL,
    MINIMAX_CN_BASE_URL, MINIMAX_CN_CHAT_COMPLETIONS_PATH, MINIMAX_CN_MESSAGES_PATH,
    MINIMAX_CN_MODELS_PATH, MINIMAX_CN_USAGE_URL, ModelCatalogDescriptor, ModelCatalogKind,
    OLLAMA_CLOUD_MODEL_SOURCE, OPENCODE_CONSTRUCTABLE_PROTOCOLS, OPENCODE_GO_BASE_URL,
    OPENCODE_GO_HOST, OPENCODE_ZEN_BASE_URL, OllamaBillingTier, PROTOCOL_FALLBACK_CHAT_MESSAGES,
    PROTOCOL_FALLBACK_CHAT_RESPONSES_MESSAGES, PlanFormField, PricingDescriptor,
    ProtocolMatrixKind, ProtocolProbeDescriptor, ProviderAdapterKind, ProviderBindingError,
    ProviderDescriptor, ProviderOrigin, ProviderProductSurface, ProviderRegistry,
    QUOTA_WINDOW_FIVE_HOURS, QUOTA_WINDOW_FREE, QUOTA_WINDOW_MONTH, QUOTA_WINDOW_WEEK,
    StructuralProbeCeiling, UsageContractKind, UsageDescriptor, VerificationDescriptor,
    VerificationPolicy, builtin_offering, builtin_provider, command_code_goat_includes_model,
    default_credential_kind, default_provider_id, default_quota_scope, default_verification_status,
    ensure_enabled_provider_is_routable, ensure_provider_can_enable, is_command_code_goat,
    is_cpa_external_integration, is_custom_api, parse_command_code_models_catalog,
    parse_provider_models_catalog, plan_allows_enablement, plan_requires_custom_config,
    preset_offering, provider_allows_enablement, provider_origin_from_preset,
    validate_account_binding, validate_custom_model_id, validate_plan_key,
};

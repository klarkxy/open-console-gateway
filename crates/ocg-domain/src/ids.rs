//! Stable provider, account, key, and model identity constants.
//!
//! These values are the first crate-split seam: they do not depend on
//! persistence, HTTP, or gateway execution. Custom capability matching lives
//! here so alias overlay and provider contracts can stay I/O-free.

/// Default model for dashboard account ping and CLI `ping`.
pub const DEFAULT_ACCOUNT_TEST_MODEL: &str = "mimo-v2.5";

pub const OPENCODE_PROVIDER_ID: &str = "opencode";
pub const COMMAND_CODE_PROVIDER_ID: &str = "command-code";
pub const OPENCODE_ZEN_FREE_PROVIDER_ID: &str = "opencode-zen-free";
pub const CUSTOM_PROVIDER_ID: &str = "custom";
pub const MINIMAX_PROVIDER_ID: &str = "minimax";
pub const KIMI_PROVIDER_ID: &str = "kimi";
pub const OLLAMA_PROVIDER_ID: &str = "ollama";
/// Reserved sealed provider identity for the local CPA external integration.
/// This is not a user-defined Provider row or plugin identifier.
pub const CPA_PROVIDER_ID: &str = "cpa";

/// Fixed Ollama Cloud inference origin. Production routes always use this
/// origin; catalog ids (including `:` tags) ride the request body, not the URL.
pub const OLLAMA_CLOUD_BASE_URL: &str = "https://ollama.com";
/// Chat Completions path relative to [`OLLAMA_CLOUD_BASE_URL`]. Ollama Cloud
/// exposes only this inference surface; Responses/Messages do not exist.
pub const OLLAMA_CLOUD_CHAT_COMPLETIONS_PATH: &str = "/v1/chat/completions";
/// Public `GET /models` discovery path used for Provider catalog refresh.
pub const OLLAMA_CLOUD_MODELS_PATH: &str = "/v1/models";
/// Official per-token price table. Manual-only refresh; never a usage API.
pub const OLLAMA_CLOUD_PRICING_URL: &str = "https://ollama.com/pricing";

/// Client-facing Alias. Go still owns the published kebab alias; GOAT maps it
/// internally to the slash raw ID and stays non-routeable.
pub const COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_ALIAS: &str = "deepseek-v4-flash";
/// Unique exact upstream raw ID for Command Code GOAT.
pub const COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM: &str = "deepseek/deepseek-v4-flash";

/// Reserved account row representing the egress-IP-scoped OpenCode Zen free
/// route. It is created by schema migration, never by the generic account API.
pub const ZEN_FREE_ACCOUNT_ID: &str = "00000000-0000-0000-0000-000000000002";
pub const ZEN_FREE_ACCOUNT_NAME: &str = "OpenCode Zen Free";

/// Reserved singleton account row for the local CPA subscription pool. It is
/// created only by the external-integration control plane, never by generic
/// account creation.
pub const CPA_ACCOUNT_ID: &str = "00000000-0000-0000-0000-000000000003";
pub const CPA_ACCOUNT_NAME: &str = "CPA Subscription Pool";

/// Fixed attribution id for the primary key. The recognizable fixed pattern
/// keeps it visually distinct from generated v4 UUIDs and the nil UUID.
/// Stable from release onwards; it may change only through an explicit
/// migration that re-attributes historical forward log rows (a chunked
/// UPDATE, the same mechanism as the startup backfill).
pub const PRIMARY_KEY_ID: &str = "00000000-0000-0000-0000-000000000001";

/// Fixed display name for the primary key in snapshots and backfills; the UI
/// labels the entry with the localized "涓?Key".
pub const PRIMARY_KEY_NAME: &str = "Primary";

/// Canonicalize a client or catalog model name for table lookup.
///
/// Spaces, underscores, and slashes become `-`; case is folded. Callers that
/// must treat slash/underscore IDs as raw (alias resolution) must not use this
/// folding for identity, only for protocol/pricing table keys.
pub fn normalize_model_name(name: &str) -> String {
    name.trim().to_lowercase().replace([' ', '_', '/'], "-")
}

/// True for the Zen catalog naming contract. The discovered catalog remains
/// the routing allowlist; this helper classifies materialized `-free` routes.
pub fn is_free_model(model: &str) -> bool {
    normalize_model_name(model).ends_with("-free")
}

/// Slash, underscore, or whitespace means "treat as a raw ID": never fold those
/// characters into `-` and then hit a kebab alias (`glm/5.2` 鈮?`glm-5.2`).
///
/// Public only as the cross-crate bridge; `ocg_core::kernel::ids` keeps this
/// crate-private.
#[doc(hidden)]
pub fn looks_raw_shaped(name: &str) -> bool {
    name.chars()
        .any(|ch| ch == '/' || ch == '_' || ch.is_whitespace())
}

/// Match a client-requested name against a declared Custom capability ID.
///
/// Raw-shaped IDs (`/`, `_`, whitespace) never fold separators onto kebab
/// aliases. Otherwise matching is case-insensitive like published aliases.
pub fn custom_model_id_matches(declared: &str, requested: &str) -> bool {
    let declared = declared.trim();
    let requested = requested.trim();
    if declared.is_empty() || requested.is_empty() {
        return false;
    }
    if declared == requested {
        return true;
    }
    if looks_raw_shaped(declared) || looks_raw_shaped(requested) {
        return declared.eq_ignore_ascii_case(requested);
    }
    declared.eq_ignore_ascii_case(requested)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_model_name_folds_separators_and_case() {
        assert_eq!(normalize_model_name(" GLM 5.2 "), "glm-5.2");
        assert_eq!(normalize_model_name("GLM_5.2"), "glm-5.2");
        assert_eq!(normalize_model_name("glm/5.2"), "glm-5.2");
    }

    #[test]
    fn is_free_model_follows_zen_suffix() {
        assert!(is_free_model("mimo-v2.5-free"));
        assert!(is_free_model("brand-new-promo-free"));
        assert!(!is_free_model("deepseek-v4-flash"));
        assert!(!is_free_model("big-pickle"));
    }

    #[test]
    fn looks_raw_shaped_detects_slash_underscore_and_whitespace() {
        assert!(looks_raw_shaped("glm/5.2"));
        assert!(looks_raw_shaped("GLM_5.2"));
        assert!(looks_raw_shaped("Grok 4.5"));
        assert!(!looks_raw_shaped("glm-5.2"));
        assert!(!looks_raw_shaped("my-local"));
    }

    #[test]
    fn custom_model_id_matching_is_exact_or_case_folded_without_separator_folding() {
        assert!(custom_model_id_matches("glm-5.2", "GLM-5.2"));
        assert!(custom_model_id_matches("my-local", "my-local"));
        assert!(custom_model_id_matches(" glm-5.2 ", "GLM-5.2"));
        assert!(!custom_model_id_matches("glm-5.2", "glm/5.2"));
        assert!(custom_model_id_matches(
            "deepseek/deepseek-v4-flash",
            "DeepSeek/deepseek-v4-flash"
        ));
        assert!(!custom_model_id_matches(
            "deepseek/deepseek-v4-flash",
            "deepseek-v4-flash"
        ));
        assert!(!custom_model_id_matches("", "glm-5.2"));
        assert!(!custom_model_id_matches("glm-5.2", "   "));
    }

    #[test]
    fn ollama_cloud_pricing_page_stays_on_the_inference_origin() {
        // Pricing is a separate official page; keep it independent of the API
        // origin so a docs-host move cannot silently retarget inference.
        assert!(OLLAMA_CLOUD_PRICING_URL.starts_with(OLLAMA_CLOUD_BASE_URL));
        assert_ne!(OLLAMA_CLOUD_PRICING_URL, OLLAMA_CLOUD_BASE_URL);
    }
}

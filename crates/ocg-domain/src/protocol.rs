//! I/O-free client/upstream protocol identities and static model catalogs.
//!
//! Request conversion, HTTP, and adapter execution stay in the host crate's
//! gateway protocol module. This module holds only the enums and tables
//! later control-plane and GatewayExecutor work can share without pulling
//! gateway I/O.

use super::ids::{
    COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_ALIAS, COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM,
    normalize_model_name,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ApiFormat {
    ChatCompletions,
    Responses,
    Messages,
    /// Google Gemini generateContent wire format. This is client-only: OCG
    /// always translates it to a model's known native upstream protocol.
    Gemini,
}

impl ApiFormat {
    pub fn upstream_path(self) -> Option<&'static str> {
        match self {
            Self::ChatCompletions => Some("/v1/chat/completions"),
            Self::Responses => Some("/v1/responses"),
            Self::Messages => Some("/v1/messages"),
            Self::Gemini => None,
        }
    }
}

/// Hardcoded OpenCode-Go protocol profiles.
///
/// `preferred` matches the official Go docs endpoint table. `supported` is the
/// set of upstream protocols verified with a test account, including extra
/// 2026-08-27 `live_supported` paths; update only after a fresh probe.
/// Request paths never trial protocols (double-billing risk).
///
/// Public only as the cross-crate bridge; `ocg_core::kernel::protocol` keeps
/// this type and its fields crate-private.
#[derive(Debug, Clone, Copy)]
#[doc(hidden)]
pub struct ModelProtocol {
    #[doc(hidden)]
    pub id: &'static str,
    #[doc(hidden)]
    pub preferred: ApiFormat,
    #[doc(hidden)]
    pub supported: &'static [ApiFormat],
    /// Aliases applied to `reasoning.effort` / `reasoning_effort` before forwarding
    /// or converting, for models whose upstream rejects a standard OCG effort.
    /// Empty slice = pass through unchanged.
    #[doc(hidden)]
    pub effort_aliases: &'static [(&'static str, &'static str)],
}

const NO_EFFORT_ALIASES: &[(&str, &str)] = &[];
const MUSE_SPARK_EFFORT_ALIASES: &[(&str, &str)] = &[("max", "xhigh")];
const NO_PROTOCOLS: &[ApiFormat] = &[];

/// Date on which the checked-in official protocol defaults were reviewed.
/// Probe observations are persisted separately and never redefine this
/// development-time baseline.
pub const OFFICIAL_PROTOCOL_BASELINE_DATE: &str = "2026-09-06";

const CHAT_ONLY: &[ApiFormat] = &[ApiFormat::ChatCompletions];
const RESPONSES_ONLY: &[ApiFormat] = &[ApiFormat::Responses];
const MESSAGES_ONLY: &[ApiFormat] = &[ApiFormat::Messages];
/// 2026-08-27 Go probe: both Chat and Messages returned live_supported.
const CHAT_AND_MESSAGES: &[ApiFormat] = &[ApiFormat::ChatCompletions, ApiFormat::Messages];
/// 2026-08-27 Go probe: Chat, Responses, and Messages all returned live_supported.
const CHAT_RESPONSES_MESSAGES: &[ApiFormat] = &[
    ApiFormat::ChatCompletions,
    ApiFormat::Responses,
    ApiFormat::Messages,
];

const MODEL_PROTOCOLS: &[ModelProtocol] = &[
    ModelProtocol {
        id: "grok-4.6",
        preferred: ApiFormat::Responses,
        supported: RESPONSES_ONLY,
        effort_aliases: NO_EFFORT_ALIASES,
    },
    ModelProtocol {
        id: "grok-4.5",
        preferred: ApiFormat::Responses,
        supported: RESPONSES_ONLY,
        effort_aliases: NO_EFFORT_ALIASES,
    },
    ModelProtocol {
        id: "glm-5.3-flash",
        preferred: ApiFormat::ChatCompletions,
        supported: CHAT_ONLY,
        effort_aliases: NO_EFFORT_ALIASES,
    },
    ModelProtocol {
        id: "glm-5.3",
        preferred: ApiFormat::ChatCompletions,
        supported: CHAT_ONLY,
        effort_aliases: NO_EFFORT_ALIASES,
    },
    ModelProtocol {
        id: "glm-5.2",
        preferred: ApiFormat::ChatCompletions,
        supported: CHAT_ONLY,
        effort_aliases: NO_EFFORT_ALIASES,
    },
    ModelProtocol {
        id: "glm-5.1",
        preferred: ApiFormat::ChatCompletions,
        supported: CHAT_ONLY,
        effort_aliases: NO_EFFORT_ALIASES,
    },
    ModelProtocol {
        id: "glm-5",
        preferred: ApiFormat::ChatCompletions,
        supported: CHAT_ONLY,
        effort_aliases: NO_EFFORT_ALIASES,
    },
    ModelProtocol {
        id: "gpt-5.6-luna",
        preferred: ApiFormat::Responses,
        supported: RESPONSES_ONLY,
        effort_aliases: NO_EFFORT_ALIASES,
    },
    ModelProtocol {
        id: "muse-spark-1.2",
        preferred: ApiFormat::Responses,
        supported: RESPONSES_ONLY,
        effort_aliases: MUSE_SPARK_EFFORT_ALIASES,
    },
    ModelProtocol {
        id: "muse-spark-1.2-contributor",
        preferred: ApiFormat::Responses,
        supported: RESPONSES_ONLY,
        effort_aliases: MUSE_SPARK_EFFORT_ALIASES,
    },
    ModelProtocol {
        id: "muse-spark-1.2-contributor-free",
        preferred: ApiFormat::Responses,
        supported: RESPONSES_ONLY,
        effort_aliases: MUSE_SPARK_EFFORT_ALIASES,
    },
    ModelProtocol {
        id: "muse-spark-1.3-contributor-free",
        preferred: ApiFormat::Responses,
        supported: RESPONSES_ONLY,
        effort_aliases: MUSE_SPARK_EFFORT_ALIASES,
    },
    ModelProtocol {
        id: "kimi-k3",
        preferred: ApiFormat::ChatCompletions,
        supported: CHAT_AND_MESSAGES,
        effort_aliases: NO_EFFORT_ALIASES,
    },
    ModelProtocol {
        id: "kimi-k2.7-code",
        preferred: ApiFormat::ChatCompletions,
        supported: CHAT_ONLY,
        effort_aliases: NO_EFFORT_ALIASES,
    },
    ModelProtocol {
        id: "kimi-k2.6",
        preferred: ApiFormat::ChatCompletions,
        supported: CHAT_ONLY,
        effort_aliases: NO_EFFORT_ALIASES,
    },
    ModelProtocol {
        id: "kimi-k2.5",
        preferred: ApiFormat::ChatCompletions,
        supported: CHAT_ONLY,
        effort_aliases: NO_EFFORT_ALIASES,
    },
    ModelProtocol {
        id: "deepseek-v4-pro",
        preferred: ApiFormat::ChatCompletions,
        supported: CHAT_RESPONSES_MESSAGES,
        effort_aliases: NO_EFFORT_ALIASES,
    },
    ModelProtocol {
        id: "deepseek-v4-flash",
        preferred: ApiFormat::ChatCompletions,
        supported: CHAT_RESPONSES_MESSAGES,
        effort_aliases: NO_EFFORT_ALIASES,
    },
    ModelProtocol {
        id: "deepseek-v4-flash-vision-exp",
        preferred: ApiFormat::ChatCompletions,
        supported: CHAT_RESPONSES_MESSAGES,
        effort_aliases: NO_EFFORT_ALIASES,
    },
    ModelProtocol {
        id: "mimo-v2.5",
        preferred: ApiFormat::ChatCompletions,
        supported: CHAT_ONLY,
        effort_aliases: NO_EFFORT_ALIASES,
    },
    ModelProtocol {
        id: "mimo-v2.5-pro",
        preferred: ApiFormat::ChatCompletions,
        supported: CHAT_ONLY,
        effort_aliases: NO_EFFORT_ALIASES,
    },
    ModelProtocol {
        id: "hy3",
        preferred: ApiFormat::ChatCompletions,
        supported: CHAT_ONLY,
        effort_aliases: NO_EFFORT_ALIASES,
    },
    ModelProtocol {
        id: "longcat-2.0",
        preferred: ApiFormat::ChatCompletions,
        supported: CHAT_ONLY,
        effort_aliases: NO_EFFORT_ALIASES,
    },
    ModelProtocol {
        id: "minimax-m3",
        preferred: ApiFormat::Messages,
        supported: CHAT_AND_MESSAGES,
        effort_aliases: NO_EFFORT_ALIASES,
    },
    ModelProtocol {
        id: "minimax-m2.7",
        preferred: ApiFormat::Messages,
        supported: MESSAGES_ONLY,
        effort_aliases: NO_EFFORT_ALIASES,
    },
    ModelProtocol {
        id: "minimax-m2.7-highspeed",
        preferred: ApiFormat::Messages,
        supported: NO_PROTOCOLS,
        effort_aliases: NO_EFFORT_ALIASES,
    },
    ModelProtocol {
        id: "minimax-m2.5",
        preferred: ApiFormat::Messages,
        supported: CHAT_AND_MESSAGES,
        effort_aliases: NO_EFFORT_ALIASES,
    },
    ModelProtocol {
        id: "minimax-m2.5-highspeed",
        preferred: ApiFormat::Messages,
        supported: NO_PROTOCOLS,
        effort_aliases: NO_EFFORT_ALIASES,
    },
    ModelProtocol {
        id: "qwen3.8-max",
        preferred: ApiFormat::Messages,
        supported: CHAT_AND_MESSAGES,
        effort_aliases: NO_EFFORT_ALIASES,
    },
    ModelProtocol {
        id: "qwen3.8-flash",
        preferred: ApiFormat::Messages,
        supported: MESSAGES_ONLY,
        effort_aliases: NO_EFFORT_ALIASES,
    },
    ModelProtocol {
        id: "qwen3.7-max",
        preferred: ApiFormat::Messages,
        supported: CHAT_AND_MESSAGES,
        effort_aliases: NO_EFFORT_ALIASES,
    },
    ModelProtocol {
        id: "qwen3.7-plus",
        preferred: ApiFormat::Messages,
        supported: CHAT_AND_MESSAGES,
        effort_aliases: NO_EFFORT_ALIASES,
    },
    ModelProtocol {
        id: "qwen3.6-plus",
        preferred: ApiFormat::Messages,
        supported: CHAT_AND_MESSAGES,
        effort_aliases: NO_EFFORT_ALIASES,
    },
    ModelProtocol {
        id: "qwen3.5-plus",
        preferred: ApiFormat::Messages,
        supported: CHAT_AND_MESSAGES,
        effort_aliases: NO_EFFORT_ALIASES,
    },
    ModelProtocol {
        id: "big-pickle",
        preferred: ApiFormat::ChatCompletions,
        supported: CHAT_ONLY,
        effort_aliases: NO_EFFORT_ALIASES,
    },
    ModelProtocol {
        id: "deepseek-v4-flash-free",
        preferred: ApiFormat::ChatCompletions,
        supported: NO_PROTOCOLS,
        effort_aliases: NO_EFFORT_ALIASES,
    },
    ModelProtocol {
        id: "mimo-v2.5-free",
        preferred: ApiFormat::ChatCompletions,
        supported: CHAT_ONLY,
        effort_aliases: NO_EFFORT_ALIASES,
    },
    ModelProtocol {
        id: "nemotron-3-ultra-free",
        preferred: ApiFormat::ChatCompletions,
        supported: CHAT_ONLY,
        effort_aliases: NO_EFFORT_ALIASES,
    },
    ModelProtocol {
        id: "nemotron-3.5-lightning-free",
        preferred: ApiFormat::ChatCompletions,
        supported: CHAT_ONLY,
        effort_aliases: NO_EFFORT_ALIASES,
    },
    ModelProtocol {
        id: "ling-3.0-flash-fin-free",
        preferred: ApiFormat::ChatCompletions,
        supported: CHAT_ONLY,
        effort_aliases: NO_EFFORT_ALIASES,
    },
    ModelProtocol {
        id: "hy4-preview",
        preferred: ApiFormat::ChatCompletions,
        supported: CHAT_ONLY,
        effort_aliases: NO_EFFORT_ALIASES,
    },
];

/// Returns every model ID with a known preferred upstream protocol.
pub fn supported_model_ids() -> impl Iterator<Item = &'static str> {
    MODEL_PROTOCOLS.iter().map(|profile| profile.id)
}

/// Provider adapters use the checked-in official OpenCode protocol row.
/// Probe-confirmed compatibility is persisted by the host and is not folded
/// back into this development-time default.
pub fn opencode_supports_upstream(model: &str, upstream: ApiFormat) -> bool {
    model_protocol(model).is_some_and(|profile| profile.supported.contains(&upstream))
}

/// Command Code GOAT protocol profiles, independent of OpenCode `MODEL_PROTOCOLS`.
/// Lookup is exact (case-insensitive) on the upstream raw ID. Slash IDs are
/// never folded onto kebab OpenCode aliases, so `deepseek/deepseek-v4-flash`
/// cannot steal Go's `deepseek-v4-flash` protocol row.
///
/// Models outside this seed table still follow the official split: Anthropic
/// IDs use Messages; OpenAI and open-source IDs use Chat Completions. There is
/// no Responses upstream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandCodeModelProtocol {
    pub alias: &'static str,
    pub upstream_id: &'static str,
    pub preferred: ApiFormat,
    pub supported_upstream: &'static [ApiFormat],
}

const COMMAND_CODE_MODEL_PROTOCOLS: &[CommandCodeModelProtocol] = &[CommandCodeModelProtocol {
    alias: COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_ALIAS,
    upstream_id: COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM,
    preferred: ApiFormat::ChatCompletions,
    supported_upstream: CHAT_ONLY,
}];

/// Exact Command Code raw-ID lookup. Does not consult OpenCode `MODEL_PROTOCOLS`
/// and does not slash-fold onto a kebab alias.
pub fn command_code_model_protocol(model: &str) -> Option<&'static CommandCodeModelProtocol> {
    let trimmed = model.trim();
    if trimmed.is_empty() {
        return None;
    }
    COMMAND_CODE_MODEL_PROTOCOLS
        .iter()
        .find(|profile| profile.upstream_id.eq_ignore_ascii_case(trimmed))
}

/// Official Command Code family split: Anthropic models speak Messages;
/// everything else speaks Chat Completions.
pub fn command_code_is_anthropic_model(model: &str) -> bool {
    let trimmed = model.trim();
    if trimmed.is_empty() {
        return false;
    }
    let lower = trimmed.to_ascii_lowercase();
    let leaf = lower.rsplit('/').next().unwrap_or(lower.as_str());
    leaf.starts_with("claude") || lower.starts_with("anthropic/")
}

/// Preferred upstream for a Command Code model ID. Seed-table rows win;
/// unknown non-empty IDs follow the Anthropic/Chat family rule.
pub fn command_code_preferred_format(model: &str) -> Option<ApiFormat> {
    if let Some(profile) = command_code_model_protocol(model) {
        return Some(profile.preferred);
    }
    if model.trim().is_empty() {
        return None;
    }
    Some(if command_code_is_anthropic_model(model) {
        ApiFormat::Messages
    } else {
        ApiFormat::ChatCompletions
    })
}

pub fn command_code_supported_formats(model: &str) -> &'static [ApiFormat] {
    if let Some(profile) = command_code_model_protocol(model) {
        return profile.supported_upstream;
    }
    if model.trim().is_empty() {
        return &[];
    }
    if command_code_is_anthropic_model(model) {
        MESSAGES_ONLY
    } else {
        CHAT_ONLY
    }
}

pub fn command_code_supports_upstream(model: &str, upstream: ApiFormat) -> bool {
    command_code_supported_formats(model).contains(&upstream)
}

/// Ollama Cloud protocol seed, independent of OpenCode `MODEL_PROTOCOLS`.
///
/// Verification basis (2026-08-31): Ollama Cloud (`https://ollama.com`)
/// publishes an OpenAI-compatible surface where Chat Completions is the only
/// inference protocol; Responses and Messages endpoints do not exist. The
/// family rule is therefore fixed Chat for every catalog id — the seed rows
/// below additionally record the code-owned preset ids so the model matrix
/// starts from verified facts instead of an empty table:
/// - bare stems `deepseek-v4-flash` / `deepseek-v4-pro` join Go-owned shared
///   aliases through the gateway stem guard (date-tagged snapshot ids such as
///   `deepseek-v4-flash:0731` are runtime catalog data and MUST stay out of
///   source code);
/// - size variants `gpt-oss:20b` / `gpt-oss:120b` are exact preset ids whose
///   shared stem `gpt-oss` is not a code-owned alias, so they never produce a
///   stem alias and coexist as independent raw pins.
///
/// These rows MUST NOT be copied into `MODEL_PROTOCOLS`: that table derives
/// Go's published aliases via `supported_model_ids()` and an Ollama row there
/// would forge Go routing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OllamaCloudModelProtocol {
    pub id: &'static str,
    pub preferred: ApiFormat,
    pub supported_upstream: &'static [ApiFormat],
}

/// Provider-specific date for the Ollama Cloud seed below; protocol-evidence
/// metadata, not a model-catalog refresh timestamp.
pub const OLLAMA_CLOUD_STATIC_PROTOCOL_SNAPSHOT_DATE: &str = "2026-08-31";

const OLLAMA_CLOUD_PROTOCOL_SEED: &[OllamaCloudModelProtocol] = &[
    OllamaCloudModelProtocol {
        id: "deepseek-v4-flash",
        preferred: ApiFormat::ChatCompletions,
        supported_upstream: CHAT_ONLY,
    },
    OllamaCloudModelProtocol {
        id: "deepseek-v4-pro",
        preferred: ApiFormat::ChatCompletions,
        supported_upstream: CHAT_ONLY,
    },
    OllamaCloudModelProtocol {
        id: "gpt-oss:20b",
        preferred: ApiFormat::ChatCompletions,
        supported_upstream: CHAT_ONLY,
    },
    OllamaCloudModelProtocol {
        id: "gpt-oss:120b",
        preferred: ApiFormat::ChatCompletions,
        supported_upstream: CHAT_ONLY,
    },
];

/// Exact Ollama Cloud preset lookup. Does not consult OpenCode
/// `MODEL_PROTOCOLS` and never strips a `:` tag.
pub fn ollama_cloud_model_protocol(model: &str) -> Option<&'static OllamaCloudModelProtocol> {
    let trimmed = model.trim();
    if trimmed.is_empty() {
        return None;
    }
    OLLAMA_CLOUD_PROTOCOL_SEED
        .iter()
        .find(|profile| profile.id.eq_ignore_ascii_case(trimmed))
}

/// True for the default-on preset rows in the Provider model/protocol matrix.
pub fn ollama_cloud_includes_model(model_id: &str) -> bool {
    ollama_cloud_model_protocol(model_id).is_some()
}

/// Preset ids backing the pre-refresh catalog view (stems plus size
/// variants). Snapshot ids with date tags are refresh-time data and never
/// appear here.
pub fn ollama_cloud_protocol_seed_ids() -> Vec<&'static str> {
    OLLAMA_CLOUD_PROTOCOL_SEED
        .iter()
        .map(|profile| profile.id)
        .collect()
}

/// Supported upstream protocols for an Ollama Cloud model id. The family is
/// fixed Chat for every non-empty id (seed rows and discovered catalog ids
/// alike); only the empty id is unsupported.
pub fn ollama_cloud_supported_formats(model: &str) -> &'static [ApiFormat] {
    if model.trim().is_empty() {
        return &[];
    }
    CHAT_ONLY
}

pub fn ollama_cloud_supports_upstream(model: &str, upstream: ApiFormat) -> bool {
    ollama_cloud_supported_formats(model).contains(&upstream)
}

/// Code-owned alias stems the Ollama Cloud catalog overlay may append a
/// mapping to (stems of the seed rows without a `:` tag). The list is the
/// subset of seed ids that are Go-owned published aliases.
pub fn ollama_cloud_shared_alias_stems() -> Vec<&'static str> {
    OLLAMA_CLOUD_PROTOCOL_SEED
        .iter()
        .map(|profile| profile.id)
        .filter(|id| !id.contains(':'))
        .collect()
}

/// Returns (id, preferred protocol) for every known OpenCode catalog model;
/// backs the proxy list picker's protocol hints.
pub fn supported_model_protocols() -> impl Iterator<Item = (&'static str, ApiFormat)> {
    MODEL_PROTOCOLS
        .iter()
        .map(|profile| (profile.id, profile.preferred))
}

/// Returns the canonical model ID, official preferred protocol, and the
/// checked-in verified protocols (official preferred plus 2026-08-27
/// live_supported extra paths). Later probe observations persist separately.
pub fn supported_model_protocol_profiles()
-> impl Iterator<Item = (&'static str, ApiFormat, &'static [ApiFormat])> {
    MODEL_PROTOCOLS
        .iter()
        .map(|profile| (profile.id, profile.preferred, profile.supported))
}

/// True when the OpenCode protocol catalog contains the model ID.
pub fn is_known_model(model: &str) -> bool {
    model_protocol(model).is_some()
}

#[doc(hidden)]
pub fn model_protocol(model: &str) -> Option<&'static ModelProtocol> {
    let normalized = normalize_model_name(model);
    MODEL_PROTOCOLS
        .iter()
        .find(|profile| profile.id == normalized)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn official_support_matches_each_model_preference() {
        for profile in MODEL_PROTOCOLS {
            if profile.supported.is_empty() {
                continue;
            }
            assert!(
                profile.supported.contains(&profile.preferred),
                "{} preferred must be in supported",
                profile.id
            );
        }
        // 2026-08-27 Go live_supported matrix: extra Chat/Messages/Responses
        // paths stay available alongside the official preferred endpoint.
        assert!(opencode_supports_upstream(
            "deepseek-v4-flash",
            ApiFormat::ChatCompletions
        ));
        assert!(opencode_supports_upstream(
            "deepseek-v4-flash",
            ApiFormat::Responses
        ));
        assert!(opencode_supports_upstream(
            "deepseek-v4-flash",
            ApiFormat::Messages
        ));
        assert!(opencode_supports_upstream("kimi-k3", ApiFormat::Messages));
        assert!(!opencode_supports_upstream("kimi-k3", ApiFormat::Responses));
        assert!(opencode_supports_upstream(
            "minimax-m3",
            ApiFormat::ChatCompletions
        ));
        assert!(!opencode_supports_upstream(
            "grok-4.6",
            ApiFormat::ChatCompletions
        ));
    }

    #[test]
    fn command_code_family_rules_split_anthropic_from_chat() {
        assert!(command_code_is_anthropic_model("claude-sonnet-4-6"));
        assert!(command_code_is_anthropic_model("anthropic/claude-opus-4-6"));
        assert!(command_code_is_anthropic_model("Claude-Haiku-4-5"));
        assert!(!command_code_is_anthropic_model(
            COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM
        ));
        assert!(!command_code_is_anthropic_model("gpt-5.4"));
        assert_eq!(
            command_code_preferred_format("claude-sonnet-4-6"),
            Some(ApiFormat::Messages)
        );
        assert_eq!(
            command_code_preferred_format(COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM),
            Some(ApiFormat::ChatCompletions)
        );
        assert!(command_code_supports_upstream(
            "claude-sonnet-4-6",
            ApiFormat::Messages
        ));
        assert!(!command_code_supports_upstream(
            "claude-sonnet-4-6",
            ApiFormat::ChatCompletions
        ));
        assert!(command_code_supports_upstream(
            COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM,
            ApiFormat::ChatCompletions
        ));
        assert!(!command_code_supports_upstream(
            COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM,
            ApiFormat::Responses
        ));
        assert!(command_code_supports_upstream(
            "minimax-m2.7",
            ApiFormat::ChatCompletions
        ));
        assert!(!command_code_supports_upstream(
            "",
            ApiFormat::ChatCompletions
        ));
        assert!(
            command_code_model_protocol(COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_ALIAS).is_none(),
            "kebab Go aliases must not resolve through the Command Code seed table"
        );
    }

    #[test]
    fn ollama_cloud_seed_is_locked_and_never_enters_model_protocols() {
        for profile in OLLAMA_CLOUD_PROTOCOL_SEED {
            assert!(!profile.id.contains(':') || profile.id.starts_with("gpt-oss:"));
        }
        assert!(ollama_cloud_model_protocol("deepseek-v4-flash").is_some());
        assert!(ollama_cloud_model_protocol("DeepSeek-V4-Pro").is_some());
        assert!(ollama_cloud_model_protocol("gpt-oss:120b").is_some());
        assert!(!ollama_cloud_includes_model("deepseek-v4-flash:0731"));
        assert!(ollama_cloud_includes_model("deepseek-v4-flash"));
        assert_eq!(ollama_cloud_supported_formats("brand-new:0915"), CHAT_ONLY);
        assert!(ollama_cloud_supported_formats("").is_empty());
        assert!(ollama_cloud_supports_upstream(
            "gpt-oss:20b",
            ApiFormat::ChatCompletions
        ));
        assert!(!ollama_cloud_supports_upstream(
            "gpt-oss:20b",
            ApiFormat::Responses
        ));
        assert!(!ollama_cloud_supports_upstream(
            "deepseek-v4-flash",
            ApiFormat::Messages
        ));
        assert_eq!(
            ollama_cloud_shared_alias_stems(),
            vec!["deepseek-v4-flash", "deepseek-v4-pro"]
        );
        for ollama_only in ["gpt-oss:20b", "gpt-oss:120b"] {
            assert!(
                !supported_model_ids().any(|id| id == ollama_only),
                "MODEL_PROTOCOLS must stay free of Ollama-only ids ({ollama_only})"
            );
        }
    }
}

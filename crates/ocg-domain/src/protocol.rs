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
/// set of upstream protocols verified with a test account; update only after a
/// fresh probe. Request paths never trial protocols (double-billing risk).
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

// Probe matrix (direct OpenCode-Go, 2026-08-14, test account):
// preferred stays on the official docs endpoint; supported is live stream +
// non-stream 2xx with a usable body. 5-turn preferred conversations completed
// 5/5 HTTP OK on every paid model that accepted its preferred endpoint.
const CHAT_ONLY: &[ApiFormat] = &[ApiFormat::ChatCompletions];
const RESPONSES_ONLY: &[ApiFormat] = &[ApiFormat::Responses];
const CHAT_AND_RESPONSES: &[ApiFormat] = &[ApiFormat::ChatCompletions, ApiFormat::Responses];
const CHAT_AND_MESSAGES: &[ApiFormat] = &[ApiFormat::ChatCompletions, ApiFormat::Messages];
const ALL_THREE: &[ApiFormat] = &[
    ApiFormat::ChatCompletions,
    ApiFormat::Responses,
    ApiFormat::Messages,
];

const MODEL_PROTOCOLS: &[ModelProtocol] = &[
    ModelProtocol {
        id: "grok-4.5",
        preferred: ApiFormat::Responses,
        supported: RESPONSES_ONLY,
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
        supported: ALL_THREE,
        effort_aliases: NO_EFFORT_ALIASES,
    },
    ModelProtocol {
        id: "glm-5.1",
        preferred: ApiFormat::ChatCompletions,
        supported: ALL_THREE,
        effort_aliases: NO_EFFORT_ALIASES,
    },
    ModelProtocol {
        id: "glm-5",
        preferred: ApiFormat::ChatCompletions,
        supported: ALL_THREE,
        effort_aliases: NO_EFFORT_ALIASES,
    },
    ModelProtocol {
        id: "gpt-5.6-luna",
        preferred: ApiFormat::Responses,
        supported: CHAT_AND_RESPONSES,
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
        supported: CHAT_AND_RESPONSES,
        effort_aliases: MUSE_SPARK_EFFORT_ALIASES,
    },
    ModelProtocol {
        id: "muse-spark-1.2-contributor-free",
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
        supported: ALL_THREE,
        effort_aliases: NO_EFFORT_ALIASES,
    },
    ModelProtocol {
        id: "deepseek-v4-flash",
        preferred: ApiFormat::ChatCompletions,
        supported: ALL_THREE,
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
        // Official Go docs: Ox Alpha Free, Chat Completions on `/zen/go`.
        // The id contains `free` but this is not a Zen promo model.
        id: "ox-alpha-free",
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
        supported: CHAT_AND_MESSAGES,
        effort_aliases: NO_EFFORT_ALIASES,
    },
    ModelProtocol {
        id: "minimax-m2.7-highspeed",
        preferred: ApiFormat::Messages,
        supported: CHAT_AND_MESSAGES,
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
        supported: CHAT_AND_MESSAGES,
        effort_aliases: NO_EFFORT_ALIASES,
    },
    ModelProtocol {
        id: "qwen3.8-max",
        preferred: ApiFormat::Messages,
        supported: CHAT_AND_MESSAGES,
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
        id: "hy3-free",
        preferred: ApiFormat::ChatCompletions,
        supported: CHAT_ONLY,
        effort_aliases: NO_EFFORT_ALIASES,
    },
    ModelProtocol {
        id: "deepseek-v4-flash-free",
        preferred: ApiFormat::ChatCompletions,
        supported: CHAT_ONLY,
        effort_aliases: NO_EFFORT_ALIASES,
    },
    ModelProtocol {
        id: "mimo-v2.5-free",
        preferred: ApiFormat::ChatCompletions,
        supported: CHAT_ONLY,
        effort_aliases: NO_EFFORT_ALIASES,
    },
    ModelProtocol {
        id: "ling-3.0-flash-free",
        preferred: ApiFormat::ChatCompletions,
        supported: CHAT_ONLY,
        effort_aliases: NO_EFFORT_ALIASES,
    },
    ModelProtocol {
        id: "laguna-s-2.1-free",
        preferred: ApiFormat::ChatCompletions,
        supported: CHAT_ONLY,
        effort_aliases: NO_EFFORT_ALIASES,
    },
    ModelProtocol {
        id: "longcat-2.0-free",
        preferred: ApiFormat::ChatCompletions,
        supported: CHAT_ONLY,
        effort_aliases: NO_EFFORT_ALIASES,
    },
    ModelProtocol {
        id: "north-mini-code-free",
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
];

/// Returns every model ID with a known preferred upstream protocol.
pub fn supported_model_ids() -> impl Iterator<Item = &'static str> {
    MODEL_PROTOCOLS.iter().map(|profile| profile.id)
}

/// Provider adapters use the same probed OpenCode matrix as request planning;
/// this prevents a mixed-provider selector from probing an unsupported
/// model/protocol pair merely because that account appears earlier.
pub fn opencode_supports_upstream(model: &str, upstream: ApiFormat) -> bool {
    model_protocol(model).is_some_and(|profile| profile.supported.contains(&upstream))
}

/// Command Code GOAT protocol profiles, independent of OpenCode `MODEL_PROTOCOLS`.
/// Lookup is exact (case-insensitive) on the upstream raw ID. Slash IDs are
/// never folded onto kebab OpenCode aliases, so `deepseek/deepseek-v4-flash`
/// cannot steal Go's `deepseek-v4-flash` protocol row.
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

pub fn command_code_protocol_profiles() -> impl Iterator<Item = &'static CommandCodeModelProtocol> {
    COMMAND_CODE_MODEL_PROTOCOLS.iter()
}

pub fn command_code_supports_upstream(model: &str, upstream: ApiFormat) -> bool {
    command_code_model_protocol(model)
        .is_some_and(|profile| profile.supported_upstream.contains(&upstream))
}

/// Returns (id, preferred protocol) for every known OpenCode catalog model;
/// backs the proxy list picker's protocol hints.
pub fn supported_model_protocols() -> impl Iterator<Item = (&'static str, ApiFormat)> {
    MODEL_PROTOCOLS
        .iter()
        .map(|profile| (profile.id, profile.preferred))
}

/// Returns the canonical model ID, preferred protocol, and every directly
/// supported OpenCode Go upstream protocol. Dashboard account tests consume
/// this same probed matrix so the UI never offers a billable trial pair that
/// request routing itself considers unsupported.
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

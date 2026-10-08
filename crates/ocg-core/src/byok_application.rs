//! Fixed downstream BYOK clients. Hosts own filesystem effects; V4 owns ordinary Keys.
use crate::model_metadata::ModelMetadata;
pub use crate::model_metadata::{PublishedModelProtocolProfile, PublishedUpstreamProtocol};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::{fmt, sync::Arc};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ByokClient {
    Codex,
    Kimi,
    Minimax,
    Zcode,
    Copilot,
}

impl ByokClient {
    pub const ALL: [Self; 5] = [
        Self::Codex,
        Self::Kimi,
        Self::Minimax,
        Self::Zcode,
        Self::Copilot,
    ];
    pub fn id(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Kimi => "kimi",
            Self::Minimax => "minimax",
            Self::Zcode => "zcode",
            Self::Copilot => "copilot",
        }
    }
    pub fn requires_closed_client(self) -> bool {
        matches!(self, Self::Codex | Self::Kimi | Self::Copilot)
    }
    pub fn key_name(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Kimi => "kimi-code",
            Self::Minimax => "minimax-code",
            Self::Zcode => "zcode",
            Self::Copilot => "copilot",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ByokStatus {
    UnsupportedRuntime,
    NotDetected,
    Ready,
    Configured,
    Incompatible,
    Conflict,
    RecoveryRequired,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ByokInspection {
    pub client: ByokClient,
    pub status: ByokStatus,
    pub detected: bool,
    pub config_path: String,
    pub discovery_source: String,
    pub target_paths: Vec<String>,
    pub configure_supported: bool,
    pub remove_supported: bool,
    pub recovery_supported: bool,
    pub requires_closed_client: bool,
    pub activation_required: bool,
    pub fingerprint: Option<String>,
    pub configured_model_ids: Vec<String>,
    pub default_model_id: Option<String>,
    pub backup_path: Option<String>,
    pub detail: Option<String>,
    #[serde(default)]
    pub adopted: bool,
    pub copilot_token_budget: Option<CopilotTokenBudget>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<ByokPreview>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ByokPreview {
    pub plan_fingerprint: String,
    pub added_model_ids: Vec<String>,
    pub removed_model_ids: Vec<String>,
    pub updated_model_ids: Vec<String>,
    pub previous_default_model_id: Option<String>,
    pub default_model_id: Option<String>,
    pub requires_takeover: bool,
    pub requires_overwrite: bool,
    pub removed_models_with_customizations: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct ByokReview {
    pub preview_fingerprint: Option<String>,
    pub acknowledge_takeover: bool,
    pub acknowledge_overwrite: bool,
    pub acknowledge_removal: bool,
}

impl ByokInspection {
    pub fn unsupported(client: ByokClient) -> Self {
        Self {
            client,
            status: ByokStatus::UnsupportedRuntime,
            detected: false,
            config_path: String::new(),
            discovery_source: "unsupported".into(),
            target_paths: vec![],
            configure_supported: false,
            remove_supported: false,
            recovery_supported: false,
            requires_closed_client: client.requires_closed_client(),
            activation_required: false,
            fingerprint: None,
            configured_model_ids: vec![],
            default_model_id: None,
            backup_path: None,
            adopted: false,
            copilot_token_budget: None,
            preview: None,
            detail: Some(
                "Use a native OCG host on the client computer to configure this application."
                    .into(),
            ),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ByokModel {
    pub id: String,
    pub metadata: ModelMetadata,
    /// Saved upstream profile. Required for configure; Chat is not assumed.
    pub protocols: PublishedModelProtocolProfile,
}

/// A secret never participates in wire serialization or readable Debug output.
pub struct ByokSecret(String);
impl ByokSecret {
    pub fn new(value: String) -> Self {
        Self(value)
    }
    pub fn expose_to_host(&self) -> &str {
        &self.0
    }
}
impl fmt::Debug for ByokSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ByokSecret([redacted])")
    }
}

#[derive(Debug)]
pub enum ByokHostRequest {
    Inspect {
        client: ByokClient,
        target_path: Option<String>,
    },
    Preview {
        client: ByokClient,
        target_path: Option<String>,
        gateway_v1_url: String,
        models: Vec<ByokModel>,
        copilot_token_budget: Option<CopilotTokenBudget>,
    },
    ValidateReviewed {
        client: ByokClient,
        target_path: Option<String>,
        expected_fingerprint: String,
        gateway_v1_url: String,
        models: Vec<ByokModel>,
        client_closed: bool,
        copilot_token_budget: Option<CopilotTokenBudget>,
        review: ByokReview,
    },
    ConfigureReviewed {
        client: ByokClient,
        target_path: Option<String>,
        expected_fingerprint: String,
        gateway_v1_url: String,
        secret: ByokSecret,
        models: Vec<ByokModel>,
        client_closed: bool,
        copilot_token_budget: Option<CopilotTokenBudget>,
        review: ByokReview,
    },
    Configure {
        client: ByokClient,
        target_path: Option<String>,
        expected_fingerprint: String,
        gateway_v1_url: String,
        secret: ByokSecret,
        models: Vec<ByokModel>,
        default_model_id: Option<String>,
        client_closed: bool,
    },
    Remove {
        client: ByokClient,
        target_path: Option<String>,
        expected_fingerprint: String,
        client_closed: bool,
    },
    Recover {
        client: ByokClient,
        target_path: Option<String>,
        expected_fingerprint: String,
        client_closed: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ByokErrorKind {
    Invalid,
    Precondition,
    Conflict,
    Internal,
}
#[derive(Debug, Clone)]
pub struct ByokError {
    pub kind: ByokErrorKind,
    pub message: String,
}
impl ByokError {
    pub fn invalid(message: impl Into<String>) -> Self {
        Self {
            kind: ByokErrorKind::Invalid,
            message: message.into(),
        }
    }
    pub fn precondition(message: impl Into<String>) -> Self {
        Self {
            kind: ByokErrorKind::Precondition,
            message: message.into(),
        }
    }
    pub fn conflict(message: impl Into<String>) -> Self {
        Self {
            kind: ByokErrorKind::Conflict,
            message: message.into(),
        }
    }
    pub fn internal(message: impl Into<String>) -> Self {
        Self {
            kind: ByokErrorKind::Internal,
            message: message.into(),
        }
    }
}
impl fmt::Display for ByokError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for ByokError {}
pub type ByokResult<T> = Result<T, ByokError>;
pub type ByokApplicationHost =
    Arc<dyn Fn(ByokHostRequest) -> ByokResult<ByokInspection> + Send + Sync + 'static>;

/// Local Copilot request budgets, not declarations about upstream capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CopilotTokenBudget {
    pub max_input_tokens: u32,
    pub max_output_tokens: u32,
}
impl Default for CopilotTokenBudget {
    fn default() -> Self {
        Self {
            max_input_tokens: 100_000,
            max_output_tokens: 8_192,
        }
    }
}

/// Apply only to this export snapshot. Published metadata and saved catalogs
/// remain unchanged. Known limits constrain the operator's client budgets.
pub fn with_copilot_token_budget(
    mut models: Vec<ByokModel>,
    budget: CopilotTokenBudget,
) -> ByokResult<Vec<ByokModel>> {
    if budget.max_input_tokens == 0 || budget.max_output_tokens == 0 {
        return Err(ByokError::invalid(
            "Copilot token budgets must be positive integers",
        ));
    }
    for model in &mut models {
        let context = model.metadata.context_window;
        let mut output = model
            .metadata
            .max_output_tokens
            .unwrap_or(u64::from(budget.max_output_tokens))
            .min(u64::from(budget.max_output_tokens));
        let mut input = u64::from(budget.max_input_tokens);
        if let Some(context) = context {
            if context < 2 {
                return Err(ByokError::precondition(
                    "A published model has no positive Copilot input/output budget; check its token metadata",
                ));
            }
            if input + output > context {
                // Keep both sides usable when a known context is smaller than
                // the requested envelope, rather than reserving all for output.
                output = ((u128::from(context) * u128::from(output)) / u128::from(input + output))
                    .max(1) as u64;
                input = (context - output).min(input);
            }
        }
        if input == 0 || output == 0 {
            return Err(ByokError::precondition(
                "A published model has no positive Copilot input/output budget; check its token metadata",
            ));
        }
        model.metadata.context_window = Some(input + output);
        model.metadata.max_output_tokens = Some(output);
    }
    Ok(models)
}

#[cfg(test)]
mod tests;

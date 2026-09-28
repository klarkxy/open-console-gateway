//! Fixed downstream BYOK clients. Hosts own filesystem effects; V4 owns ordinary Keys.
use crate::model_metadata::ModelMetadata;
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
}

impl ByokClient {
    pub const ALL: [Self; 4] = [Self::Codex, Self::Kimi, Self::Minimax, Self::Zcode];
    pub fn id(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Kimi => "kimi",
            Self::Minimax => "minimax",
            Self::Zcode => "zcode",
        }
    }
    pub fn requires_closed_client(self) -> bool {
        matches!(self, Self::Codex | Self::Kimi)
    }
    pub fn key_name(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Kimi => "kimi-code",
            Self::Minimax => "minimax-code",
            Self::Zcode => "zcode",
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

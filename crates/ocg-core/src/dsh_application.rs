//! Narrow host seam for the new DSH application integration.
//!
//! The HTTP control plane owns authentication, CAS, and Key selection. The
//! native host owns local process and filesystem effects. Desktop and native
//! CLI builds register it; builds without the local-host capability report an
//! unsupported runtime.

use std::fmt;
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DshApplicationPhase {
    UnsupportedRuntime,
    NotDetected,
    Ready,
    Installed,
    Incompatible,
    Conflict,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DshApplicationInspection {
    pub phase: DshApplicationPhase,
    pub detected: bool,
    pub installed: bool,
    pub install_supported: bool,
    pub activation_required: bool,
    pub version: Option<String>,
    pub detail: Option<String>,
    pub target_paths: Vec<String>,
    pub fingerprint: Option<String>,
}

impl DshApplicationInspection {
    pub fn unsupported() -> Self {
        Self {
            phase: DshApplicationPhase::UnsupportedRuntime,
            detected: false,
            installed: false,
            install_supported: false,
            activation_required: false,
            version: None,
            detail: Some(
                "DSH installation is unavailable in this build; use the Desktop app or a native CLI on the DSH host"
                    .into(),
            ),
            target_paths: Vec::new(),
            fingerprint: None,
        }
    }
}

/// A Gateway Key may cross only the authenticated HTTP handler -> Desktop
/// host boundary. It deliberately has no `Clone`, `Serialize`, or readable
/// `Debug` implementation.
pub struct DshGatewaySecret(String);

impl DshGatewaySecret {
    pub fn new(value: String) -> Self {
        Self(value)
    }

    pub fn expose_to_host(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for DshGatewaySecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("DshGatewaySecret([redacted])")
    }
}

#[derive(Debug)]
pub enum DshApplicationHostRequest {
    Inspect {
        gateway_v1_url: String,
    },
    Install {
        expected_fingerprint: String,
        gateway_v1_url: String,
        secret: DshGatewaySecret,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DshApplicationErrorKind {
    Invalid,
    Precondition,
    Conflict,
    Internal,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DshApplicationError {
    pub kind: DshApplicationErrorKind,
    pub message: String,
}

impl DshApplicationError {
    pub fn invalid(message: impl Into<String>) -> Self {
        Self {
            kind: DshApplicationErrorKind::Invalid,
            message: message.into(),
        }
    }

    pub fn precondition(message: impl Into<String>) -> Self {
        Self {
            kind: DshApplicationErrorKind::Precondition,
            message: message.into(),
        }
    }

    pub fn conflict(message: impl Into<String>) -> Self {
        Self {
            kind: DshApplicationErrorKind::Conflict,
            message: message.into(),
        }
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self {
            kind: DshApplicationErrorKind::Internal,
            message: message.into(),
        }
    }
}

impl fmt::Display for DshApplicationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for DshApplicationError {}

pub type DshApplicationResult<T> = Result<T, DshApplicationError>;

pub type DshApplicationHost = Arc<
    dyn Fn(DshApplicationHostRequest) -> DshApplicationResult<DshApplicationInspection>
        + Send
        + Sync
        + 'static,
>;

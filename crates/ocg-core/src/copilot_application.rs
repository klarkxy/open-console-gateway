//! Copilot provider installation and profile-specific connection boundary.
use crate::byok_application::ByokResult;
use crate::dsh_application::DshGatewaySecret;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CopilotTarget {
    pub installation: Option<String>,
    pub profile: Option<String>,
    pub user_data_dir: Option<String>,
    pub extensions_dir: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CopilotStatus {
    UnsupportedRuntime,
    NotDetected,
    Ready,
    InstalledPending,
    Connected,
    Disconnected,
    ConnectionError,
    Conflict,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CopilotInstallation {
    pub id: String,
    pub label: String,
    pub executable: String,
    pub version: Option<String>,
    pub user_data_dir: String,
    pub extensions_dir: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CopilotInspection {
    pub target: CopilotTarget,
    pub status: CopilotStatus,
    pub installed: bool,
    pub install_supported: bool,
    pub uninstall_supported: bool,
    pub activation_required: bool,
    pub discovered_installations: Vec<CopilotInstallation>,
    pub fingerprint: Option<String>,
    pub extension_version: Option<String>,
    pub detail: Option<String>,
    pub connection_status: Option<String>,
    pub model_count: Option<u32>,
    pub metadata_missing: Vec<String>,
}
impl CopilotInspection {
    pub fn unsupported(target: CopilotTarget) -> Self {
        Self {
            target,
            status: CopilotStatus::UnsupportedRuntime,
            installed: false,
            install_supported: false,
            uninstall_supported: false,
            activation_required: false,
            discovered_installations: vec![],
            fingerprint: None,
            extension_version: None,
            detail: None,
            connection_status: None,
            model_count: None,
            metadata_missing: vec![],
        }
    }
}

#[derive(Debug)]
pub enum CopilotApplicationHostRequest {
    Inspect {
        target: CopilotTarget,
    },
    Install {
        target: CopilotTarget,
        expected_fingerprint: String,
        gateway_v1_url: String,
        secret: DshGatewaySecret,
    },
    Disconnect {
        target: CopilotTarget,
        expected_fingerprint: String,
    },
    Uninstall {
        target: CopilotTarget,
        expected_fingerprint: String,
    },
}
pub type CopilotApplicationHost = Arc<
    dyn Fn(CopilotApplicationHostRequest) -> ByokResult<CopilotInspection> + Send + Sync + 'static,
>;

/// Match the extension connection policy before creating a Key or installing.
pub(crate) fn validated_gateway(value: &str) -> crate::byok_application::ByokResult<String> {
    use crate::byok_application::ByokError;
    let url = reqwest::Url::parse(value).map_err(|_| ByokError::invalid("OCG URL is invalid"))?;
    if !["http", "https"].contains(&url.scheme())
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !url.path().trim_end_matches('/').ends_with("/v1")
    {
        return Err(ByokError::invalid(
            "OCG URL must be a /v1 endpoint without embedded credentials",
        ));
    }
    let host = url.host_str().unwrap_or("").trim_matches(['[', ']']);
    let private = host == "localhost"
        || host
            .parse::<std::net::IpAddr>()
            .ok()
            .is_some_and(|ip| match ip {
                std::net::IpAddr::V4(ip) => ip.is_loopback() || ip.is_private(),
                std::net::IpAddr::V6(ip) => {
                    ip.is_loopback() || ip.is_unique_local() || ip.is_unicast_link_local()
                }
            });
    if url.scheme() == "http" && !private {
        return Err(ByokError::precondition(
            "Remote OCG connections require HTTPS",
        ));
    }
    Ok(value.trim_end_matches('/').to_owned())
}

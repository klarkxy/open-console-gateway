//! I/O-free provider catalog identity enums and canonical catalog endpoints.
//!
//! Binding, enablement, and URL/key validation stay in `crate::provider`.
//! This module holds the closed identity vocabularies, a kernel-local
//! parse error, and I/O-free catalog endpoint literals so later hosts can
//! share the same strings without pulling catalog policy or HTTP.

use serde::{Deserialize, Serialize};
use std::fmt;

/// Production OpenCode Go usage endpoint. Callers must not substitute another URL.
pub const OPENCODE_GO_USAGE_URL: &str = "https://opencode.ai/zen/go/v1/usage";

/// Parse failure for a catalog identity string.
///
/// Compatibility wrappers map this into the public provider binding error so
/// dashboard, CLI, and Tauri paths keep the same variants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CatalogParseError {
    UnknownCredentialKind(String),
    UnknownQuotaScope(String),
    UnknownUpstreamProtocol(String),
    UnknownAuthScheme(String),
}

impl fmt::Display for CatalogParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownCredentialKind(value) => {
                write!(f, "unknown credential kind `{value}`")
            }
            Self::UnknownQuotaScope(value) => write!(f, "unknown quota scope `{value}`"),
            Self::UnknownUpstreamProtocol(value) => {
                write!(f, "unknown upstream protocol `{value}`")
            }
            Self::UnknownAuthScheme(value) => write!(f, "unknown auth scheme `{value}`"),
        }
    }
}

impl std::error::Error for CatalogParseError {}

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
    type Error = CatalogParseError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "api_key" => Ok(Self::ApiKey),
            "none" => Ok(Self::None),
            _ => Err(CatalogParseError::UnknownCredentialKind(value.to_string())),
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
    type Error = CatalogParseError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "key" => Ok(Self::Key),
            "egress-ip" => Ok(Self::EgressIp),
            _ => Err(CatalogParseError::UnknownQuotaScope(value.to_string())),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpstreamProtocolKind {
    #[default]
    ChatCompletions,
    Responses,
    Messages,
}

impl UpstreamProtocolKind {
    pub const ALL: [Self; 3] = [Self::ChatCompletions, Self::Responses, Self::Messages];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ChatCompletions => "chat_completions",
            Self::Responses => "responses",
            Self::Messages => "messages",
        }
    }
}

impl TryFrom<&str> for UpstreamProtocolKind {
    type Error = CatalogParseError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "chat_completions" => Ok(Self::ChatCompletions),
            "responses" => Ok(Self::Responses),
            "messages" => Ok(Self::Messages),
            _ => Err(CatalogParseError::UnknownUpstreamProtocol(
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
    type Error = CatalogParseError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "bearer" => Ok(Self::Bearer),
            "x-api-key" => Ok(Self::XApiKey),
            _ => Err(CatalogParseError::UnknownAuthScheme(value.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_identities_round_trip_and_reject_unknown_values() {
        assert_eq!(CredentialKind::ApiKey.as_str(), "api_key");
        assert_eq!(
            CredentialKind::try_from("none").unwrap(),
            CredentialKind::None
        );
        assert!(matches!(
            CredentialKind::try_from("cookie"),
            Err(CatalogParseError::UnknownCredentialKind(value)) if value == "cookie"
        ));

        assert_eq!(QuotaScope::EgressIp.as_str(), "egress-ip");
        assert_eq!(QuotaScope::try_from("key").unwrap(), QuotaScope::Key);
        assert!(matches!(
            QuotaScope::try_from("account"),
            Err(CatalogParseError::UnknownQuotaScope(value)) if value == "account"
        ));

        assert_eq!(UpstreamProtocolKind::ALL.len(), 3);
        assert_eq!(UpstreamProtocolKind::Responses.as_str(), "responses");
        assert_eq!(
            UpstreamProtocolKind::try_from("messages").unwrap(),
            UpstreamProtocolKind::Messages
        );
        assert!(matches!(
            UpstreamProtocolKind::try_from("gemini"),
            Err(CatalogParseError::UnknownUpstreamProtocol(value)) if value == "gemini"
        ));

        assert_eq!(UpstreamAuthScheme::XApiKey.as_str(), "x-api-key");
        assert_eq!(
            UpstreamAuthScheme::try_from("bearer").unwrap(),
            UpstreamAuthScheme::Bearer
        );
        assert!(matches!(
            UpstreamAuthScheme::try_from("basic"),
            Err(CatalogParseError::UnknownAuthScheme(value)) if value == "basic"
        ));
    }

    #[test]
    fn opencode_go_usage_url_is_the_fixed_official_endpoint() {
        assert_eq!(OPENCODE_GO_USAGE_URL, "https://opencode.ai/zen/go/v1/usage");
    }
}

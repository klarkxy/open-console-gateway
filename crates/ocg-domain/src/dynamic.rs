//! Typed dynamic Provider definitions. These are data, not adapter plugins.
//!
//! Every dynamic Provider binds the sealed Configurable HTTP adapter. Custom
//! API remains a distinct account-owned route (`custom`) and is not a dynamic
//! Provider.

use crate::catalog::{
    CatalogParseError, CredentialKind, QuotaScope, UpstreamAuthScheme, UpstreamProtocolKind,
};
use crate::ids::CUSTOM_PROVIDER_ID;
use crate::provider::{ProviderAdapterKind, ProviderBindingError, validate_custom_model_id};
use serde::{Deserialize, Serialize};

/// Provider-owned auth for a dynamic Provider. Independent of protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DynamicAuthKind {
    Bearer,
    XApiKey,
    None,
}

impl DynamicAuthKind {
    pub const ALL: [Self; 3] = [Self::Bearer, Self::XApiKey, Self::None];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Bearer => "bearer",
            Self::XApiKey => "x-api-key",
            Self::None => "none",
        }
    }

    pub const fn credential_kind(self) -> CredentialKind {
        match self {
            Self::Bearer | Self::XApiKey => CredentialKind::ApiKey,
            Self::None => CredentialKind::None,
        }
    }

    pub const fn quota_scope(self) -> QuotaScope {
        QuotaScope::Key
    }

    pub const fn requires_key(self) -> bool {
        !matches!(self, Self::None)
    }

    pub const fn is_singleton(self) -> bool {
        matches!(self, Self::None)
    }

    pub fn upstream_auth(self) -> Option<UpstreamAuthScheme> {
        match self {
            Self::Bearer => Some(UpstreamAuthScheme::Bearer),
            Self::XApiKey => Some(UpstreamAuthScheme::XApiKey),
            Self::None => None,
        }
    }
}

impl TryFrom<&str> for DynamicAuthKind {
    type Error = CatalogParseError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "bearer" => Ok(Self::Bearer),
            "x-api-key" | "x_api_key" => Ok(Self::XApiKey),
            "none" => Ok(Self::None),
            _ => Err(CatalogParseError::UnknownAuthScheme(value.to_string())),
        }
    }
}

/// One public-to-upstream mapping owned by a dynamic Provider.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DynamicModelUpstreamOverride {
    pub protocol: UpstreamProtocolKind,
    pub endpoint_url: String,
}

/// Absent overrides inherit the Provider's endpoint and protocol. Authentication
/// remains Provider-owned; protocol names never imply an authentication scheme.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DynamicModelMapping {
    pub public_model: String,
    pub upstream_model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream_override: Option<DynamicModelUpstreamOverride>,
}

/// Normalized dynamic Provider definition used by persistence and routing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DynamicProviderDefinition {
    /// Optional configuration-template provenance; never a routing identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preset_id: Option<String>,
    pub id: String,
    pub name: String,
    pub endpoint_url: String,
    pub upstream_protocol: UpstreamProtocolKind,
    pub auth_kind: DynamicAuthKind,
    pub mappings: Vec<DynamicModelMapping>,
}

impl DynamicProviderDefinition {
    pub fn adapter_kind(&self) -> ProviderAdapterKind {
        ProviderAdapterKind::ConfigurableHttp
    }

    pub fn credential_kind(&self) -> CredentialKind {
        self.auth_kind.credential_kind()
    }

    pub fn quota_scope(&self) -> QuotaScope {
        self.auth_kind.quota_scope()
    }
}

/// True when `provider_id` is the account-owned Custom API identity.
pub fn is_custom_api_id(provider_id: &str) -> bool {
    provider_id == CUSTOM_PROVIDER_ID
}

pub fn normalize_dynamic_provider_name(name: &str) -> Result<String, ProviderBindingError> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(ProviderBindingError::InvalidProviderName(
            "provider name is required".to_string(),
        ));
    }
    if trimmed.chars().count() > 200 {
        return Err(ProviderBindingError::InvalidProviderName(
            "provider name is too long".to_string(),
        ));
    }
    if trimmed.contains('\0') || trimmed.chars().any(char::is_control) {
        return Err(ProviderBindingError::InvalidProviderName(
            "provider name must not contain control characters".to_string(),
        ));
    }
    Ok(trimmed.to_string())
}

pub fn normalize_dynamic_mappings(
    mappings: &[DynamicModelMapping],
) -> Result<Vec<DynamicModelMapping>, ProviderBindingError> {
    if mappings.is_empty() {
        return Err(ProviderBindingError::InvalidModelId(
            "at least one model mapping is required".to_string(),
        ));
    }
    let mut seen = std::collections::HashSet::new();
    let mut normalized = Vec::with_capacity(mappings.len());
    for mapping in mappings {
        let public_model = validate_custom_model_id(&mapping.public_model)?;
        let upstream_model = validate_custom_model_id(&mapping.upstream_model)?;
        let key = public_model.to_ascii_lowercase();
        if !seen.insert(key) {
            return Err(ProviderBindingError::InvalidModelId(format!(
                "duplicate public model `{public_model}`"
            )));
        }
        normalized.push(DynamicModelMapping {
            public_model,
            upstream_model,
            upstream_override: mapping.upstream_override.clone(),
        });
    }
    Ok(normalized)
}

pub fn provider_ids_equal(left: &str, right: &str) -> bool {
    left.eq_ignore_ascii_case(right)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mappings_are_unique_by_public_model_case_insensitively() {
        let error = normalize_dynamic_mappings(&[
            DynamicModelMapping {
                public_model: "Gpt-4".into(),
                upstream_model: "gpt-4-upstream".into(),
                upstream_override: None,
            },
            DynamicModelMapping {
                public_model: "gpt-4".into(),
                upstream_model: "other".into(),
                upstream_override: None,
            },
        ])
        .unwrap_err();
        assert!(matches!(error, ProviderBindingError::InvalidModelId(_)));
    }

    #[test]
    fn none_auth_is_singleton_without_key() {
        assert!(DynamicAuthKind::None.is_singleton());
        assert!(!DynamicAuthKind::None.requires_key());
        assert_eq!(
            DynamicAuthKind::None.credential_kind(),
            CredentialKind::None
        );
        assert!(DynamicAuthKind::Bearer.requires_key());
        assert!(!DynamicAuthKind::Bearer.is_singleton());
    }

    #[test]
    fn blank_names_are_rejected() {
        assert!(normalize_dynamic_provider_name("   ").is_err());
        assert_eq!(normalize_dynamic_provider_name(" Lab ").unwrap(), "Lab");
    }
}

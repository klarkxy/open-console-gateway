//! Origin grant and Custom URL destination trust (S01 / S03).
//!
//! Sealed adapter origins stay sealed. Isolated Custom/dynamic secret-bearing
//! sends match stored binding `allowedOrigins` (and endpoint ids on the live
//! send path). Editing the current Provider or Custom URL is not a grant.
//! Metadata, link-local, and opaque IPv4 tricks are never Custom destinations.
//! Documented loopback / LAN local-model targets stay allowed and do not widen
//! those exceptions to metadata hosts.

use super::{CustomUrlHost, CustomUrlTarget, custom_url_host, inspect_custom_url};
use crate::provider::{
    COMMAND_CODE_GOAT_BASE_URL, KIMI_CN_BASE_URL, MINIMAX_CN_ANTHROPIC_BASE_URL,
    MINIMAX_CN_BASE_URL, OLLAMA_CLOUD_BASE_URL, OPENCODE_GO_BASE_URL, ProviderBindingError,
};
use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

const BLOCKED_METADATA_HOSTS: &[&str] = &[
    "metadata.google.internal",
    "metadata.google.com",
    "metadata.goog",
    "metadata",
];

const SEALED_OFFICIAL_BASES: &[&str] = &[
    OPENCODE_GO_BASE_URL,
    COMMAND_CODE_GOAT_BASE_URL,
    MINIMAX_CN_BASE_URL,
    MINIMAX_CN_ANTHROPIC_BASE_URL,
    KIMI_CN_BASE_URL,
    OLLAMA_CLOUD_BASE_URL,
];

/// Secret-bearing inference never follows `Location`. Keyless adapters may
/// still follow when their descriptor allows it.
pub const fn follows_redirects_with_secret(adapter_follow: bool, has_secret: bool) -> bool {
    adapter_follow && !has_secret
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InferenceOrigin {
    pub scheme: String,
    pub host: String,
    pub port: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OriginGrantError {
    message: String,
}

impl OriginGrantError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for OriginGrantError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for OriginGrantError {}

impl From<ProviderBindingError> for OriginGrantError {
    fn from(error: ProviderBindingError) -> Self {
        Self::new(error.to_string())
    }
}

pub fn inference_origin(url: &str) -> Option<InferenceOrigin> {
    let parsed = reqwest::Url::parse(url.trim()).ok()?;
    origin_of(&parsed)
}

pub fn origins_match(left: &str, right: &str) -> bool {
    match (inference_origin(left), inference_origin(right)) {
        (Some(left), Some(right)) => left == right,
        _ => false,
    }
}

/// Host-side grant for one secret-bearing attempt.
///
/// User-defined Custom / dynamic routes pass stored binding Origins.
/// Sealed adapters pass `None` and are checked against the official origin or
/// the documented loopback test seam.
pub fn enforce_attempt_secret_origin(
    target_url: &str,
    persisted_user_endpoint: Option<&str>,
    adapter_base: &str,
) -> Result<(), OriginGrantError> {
    match persisted_user_endpoint {
        Some(persisted) => ensure_secret_origin_granted(target_url, &[persisted.to_string()]),
        None => ensure_sealed_secret_origin(target_url, adapter_base),
    }
}

/// Refuse to attach a stored Key when `target_url` is outside the grant.
///
/// Isolated callers pass stored binding `allowedOrigins`. Changing a model
/// override to another Origin does not inherit the Key unless that Origin was
/// explicitly granted. Editing the current connection URL does not grant.
pub fn ensure_secret_origin_granted(
    target_url: &str,
    granted_urls: &[String],
) -> Result<(), OriginGrantError> {
    let parsed = reqwest::Url::parse(target_url.trim()).map_err(|error| {
        OriginGrantError::new(format!(
            "refusing to send credentials: invalid endpoint URL: {error}"
        ))
    })?;
    inspect_custom_url(&parsed)?;
    let target = origin_of(&parsed).ok_or_else(|| {
        OriginGrantError::new("refusing to send credentials: endpoint URL must include an origin")
    })?;
    let mut authorized = granted_urls.iter().filter_map(|url| inference_origin(url));
    if authorized.any(|granted| granted == target) {
        return Ok(());
    }
    Err(OriginGrantError::new(
        "refusing to send credentials: the endpoint origin is not authorized for this Key",
    ))
}

/// Sealed adapters may send a Key only to their official origin or the
/// documented loopback test seam. A request-time override to another Origin
/// is not an automatic grant.
pub fn ensure_sealed_secret_origin(
    target_url: &str,
    adapter_base: &str,
) -> Result<(), OriginGrantError> {
    if !is_loopback_inference_origin(adapter_base) && !matches_sealed_official_origin(adapter_base)
    {
        return Err(OriginGrantError::new(
            "refusing to send credentials: sealed adapter origin is not authorized",
        ));
    }
    ensure_secret_origin_granted(target_url, &[adapter_base.to_string()])
}

pub fn is_loopback_inference_origin(url: &str) -> bool {
    let Ok(parsed) = reqwest::Url::parse(url.trim()) else {
        return false;
    };
    if !matches!(parsed.scheme(), "http" | "https") {
        return false;
    }
    match custom_url_host(&parsed) {
        Ok(CustomUrlHost::Ip(ip)) => ip.to_canonical().is_loopback(),
        Ok(CustomUrlHost::Domain(domain)) => domain == "localhost",
        Err(_) => false,
    }
}

pub fn matches_sealed_official_origin(url: &str) -> bool {
    SEALED_OFFICIAL_BASES
        .iter()
        .any(|official| origins_match(url, official))
}

pub(super) fn reject_unauthorized_custom_target(
    target: &CustomUrlTarget,
) -> Result<(), ProviderBindingError> {
    match &target.host {
        CustomUrlHost::Ip(ip) => {
            if is_blocked_custom_ip(*ip) {
                return Err(blocked_custom_target());
            }
        }
        CustomUrlHost::Domain(domain) => {
            if is_blocked_metadata_host(domain) || domain_is_opaque_ip_trick(domain) {
                return Err(blocked_custom_target());
            }
        }
    }
    Ok(())
}

fn blocked_custom_target() -> ProviderBindingError {
    ProviderBindingError::InvalidCustomBaseUrl(
        "endpoint URL must not target a metadata or link-local address".to_string(),
    )
}

fn origin_of(parsed: &reqwest::Url) -> Option<InferenceOrigin> {
    if !matches!(parsed.scheme(), "http" | "https") {
        return None;
    }
    let host = match custom_url_host(parsed).ok()? {
        CustomUrlHost::Ip(ip) => ip.to_canonical().to_string(),
        CustomUrlHost::Domain(domain) => domain,
    };
    Some(InferenceOrigin {
        scheme: parsed.scheme().to_ascii_lowercase(),
        host,
        port: parsed.port_or_known_default()?,
    })
}

/// Shared destination IP policy for URL-host inspection and the IsolatedTrustedAdmin
/// connector DNS guard. Do not duplicate this list.
pub(super) fn is_blocked_custom_ip(ip: IpAddr) -> bool {
    match ip.to_canonical() {
        IpAddr::V4(ip) => is_blocked_ipv4(ip),
        IpAddr::V6(ip) => is_blocked_ipv6(ip),
    }
}

fn is_blocked_ipv4(ip: Ipv4Addr) -> bool {
    ip.is_link_local() || ip.is_unspecified() || ip.is_broadcast() || ip.is_multicast()
}

fn is_blocked_ipv6(ip: Ipv6Addr) -> bool {
    if let Some(mapped) = ip.to_ipv4_mapped() {
        return is_blocked_ipv4(mapped);
    }
    ip.is_unicast_link_local()
        || ip.is_unspecified()
        || ip.is_multicast()
        || ip == Ipv6Addr::new(0xfd00, 0xec2, 0, 0, 0, 0, 0, 0x254)
}

fn is_blocked_metadata_host(domain: &str) -> bool {
    let domain = domain.trim_end_matches('.');
    BLOCKED_METADATA_HOSTS
        .iter()
        .any(|blocked| domain == *blocked || domain.ends_with(&format!(".{blocked}")))
}

fn domain_is_opaque_ip_trick(domain: &str) -> bool {
    let domain = domain.trim_end_matches('.');
    if domain.is_empty() {
        return false;
    }
    let lower = domain.to_ascii_lowercase();
    if lower.split('.').any(|label| label.starts_with("0x")) {
        return true;
    }
    if domain.bytes().all(|byte| byte.is_ascii_digit()) {
        return true;
    }
    dotted_looks_like_obscured_ipv4(domain)
}

fn dotted_looks_like_obscured_ipv4(domain: &str) -> bool {
    let labels: Vec<&str> = domain.split('.').collect();
    if !(2..=4).contains(&labels.len()) {
        return false;
    }
    if !labels
        .iter()
        .all(|label| !label.is_empty() && label.bytes().all(|byte| byte.is_ascii_digit()))
    {
        return false;
    }
    labels
        .iter()
        .any(|label| label.len() > 1 && label.starts_with('0'))
}

#[cfg(test)]
mod tests;

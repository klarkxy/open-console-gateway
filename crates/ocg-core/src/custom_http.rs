//! Dedicated Custom API HTTP foundation.
//!
//! Phase 1 is intentionally unroutable: this module does not wire inference,
//! verify, usage, or the gateway selector. It exists so later slices reuse one
//! fail-closed client instead of `http_client::build`.
//!
//! Local DNS preflight runs on **every** Direct or Manual request. The
//! connect-time [`reqwest::dns::Resolve`] applies the same
//! [`evaluate_resolved_ips`] host-kind policy and **fails the entire
//! resolution** on empty, mixed, blocked, or host-kind-changing answers. It
//! never filters-and-continues, and never uses `ClientBuilder::resolve` /
//! `resolve_to_addrs` (those skip a custom resolver).
//!
//! Under a Manual proxy the proxy performs its own DNS; this crate cannot pin
//! that lookup and does **not** claim CONNECT-to-pinned-IP. Origin host-kind
//! policy is **not** applied to the configured Manual proxy hostname, so named
//! LAN proxies stay reachable. Auto mode is explicitly unavailable in Phase 1:
//! reqwest does not expose the effective system-proxy endpoint to this resolver,
//! so it cannot safely distinguish proxy DNS from origin DNS.

use crate::models::{AppConfig, ProxyMode};
use crate::provider::UpstreamAuthScheme;
use crate::provider::{
    CustomHostPolicy, CustomIpClass, CustomUrlHost, classify_custom_ip, custom_origin_host_policy,
    inspect_custom_url, validate_custom_base_url,
};
use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderName, HeaderValue};
use reqwest::redirect::Policy;
use std::collections::{HashMap, VecDeque};
use std::error::Error;
use std::fmt;
use std::future::Future;
use std::net::{IpAddr, SocketAddr, ToSocketAddrs};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

const CUSTOM_MIN_TIMEOUT_SECS: u64 = 5;
const CUSTOM_MAX_TIMEOUT_SECS: u64 = 60;

type HostLookupFuture = Pin<Box<dyn Future<Output = Result<Vec<IpAddr>, CustomHttpError>> + Send>>;

pub trait HostLookup: Send + Sync + 'static {
    fn resolve_host(&self, host: &str) -> HostLookupFuture;
}

/// Production lookup: IP literals are parsed locally; names use GAI on a
/// blocking thread. Tests inject [`ScriptedHostLookup`] instead of real DNS.
#[derive(Debug, Default)]
pub struct GaiHostLookup;

impl HostLookup for GaiHostLookup {
    fn resolve_host(&self, host: &str) -> HostLookupFuture {
        let host = host.to_string();
        Box::pin(async move {
            if let Some(ip) = parse_literal_ip(&host) {
                return Ok(vec![ip]);
            }
            let lookup_host = host.clone();
            tokio::task::spawn_blocking(move || gai_lookup(&lookup_host))
                .await
                .map_err(|error| CustomHttpError::Resolution(error.to_string()))?
        })
    }
}

fn gai_lookup(host: &str) -> Result<Vec<IpAddr>, CustomHttpError> {
    let mut ips = Vec::new();
    let addrs = (host, 0u16)
        .to_socket_addrs()
        .map_err(|error| CustomHttpError::Resolution(error.to_string()))?;
    for addr in addrs {
        if !ips.contains(&addr.ip()) {
            ips.push(addr.ip());
        }
    }
    Ok(ips)
}

fn parse_literal_ip(host: &str) -> Option<IpAddr> {
    if let Some(inside) = host
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
    {
        return inside.parse::<IpAddr>().ok();
    }
    host.parse::<IpAddr>().ok()
}

/// Scripted answers for deterministic rebinding tests. Never used as production DNS.
#[derive(Debug, Default)]
pub struct ScriptedHostLookup {
    answers: Mutex<HashMap<String, VecDeque<Vec<IpAddr>>>>,
}

impl ScriptedHostLookup {
    pub fn enqueue(&self, host: &str, addrs: Vec<IpAddr>) {
        self.answers
            .lock()
            .expect("scripted DNS mutex")
            .entry(normalize_dns_name(host))
            .or_default()
            .push_back(addrs);
    }
}

impl HostLookup for ScriptedHostLookup {
    fn resolve_host(&self, host: &str) -> HostLookupFuture {
        let host = normalize_dns_name(host);
        let next = self
            .answers
            .lock()
            .expect("scripted DNS mutex")
            .get_mut(&host)
            .and_then(|queue| queue.pop_front());
        Box::pin(async move {
            next.ok_or_else(|| {
                CustomHttpError::Resolution(format!("no scripted DNS answers for {host}"))
            })
        })
    }
}

fn normalize_dns_name(host: &str) -> String {
    let host = host.trim().trim_end_matches('.');
    let host = host
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
        .unwrap_or(host);
    host.to_ascii_lowercase()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CustomProxyDirective {
    Unavailable,
    Explicit,
    Disabled,
}

pub fn custom_proxy_directive(config: &AppConfig) -> CustomProxyDirective {
    match config.proxy_mode {
        ProxyMode::Auto => CustomProxyDirective::Unavailable,
        ProxyMode::Manual => CustomProxyDirective::Explicit,
        ProxyMode::Direct => CustomProxyDirective::Disabled,
    }
}

#[derive(Clone)]
pub struct CustomHttpClient {
    client: reqwest::Client,
    lookup: Arc<dyn HostLookup>,
    proxy_mode: ProxyMode,
}

impl fmt::Debug for CustomHttpClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CustomHttpClient")
            .field("proxy_mode", &self.proxy_mode)
            .finish_non_exhaustive()
    }
}

impl CustomHttpClient {
    pub fn proxy_mode(&self) -> ProxyMode {
        self.proxy_mode
    }

    pub async fn preflight(&self, url: &reqwest::Url) -> Result<Vec<IpAddr>, CustomHttpError> {
        preflight_custom_url(url, self.lookup.as_ref()).await
    }

    pub async fn send_preflighted(
        &self,
        method: reqwest::Method,
        url: reqwest::Url,
        scheme: UpstreamAuthScheme,
        api_key: &str,
        body: Option<Vec<u8>>,
    ) -> Result<reqwest::Response, CustomHttpError> {
        self.preflight(&url).await?;
        let headers = isolated_custom_headers(scheme, api_key)?;
        let mut builder = self.client.request(method, url);
        for (name, value) in &headers {
            builder = builder.header(name, value);
        }
        if let Some(body) = body {
            builder = builder.body(body);
        }
        builder.send().await.map_err(map_reqwest_send_error)
    }
}

fn map_reqwest_send_error(error: reqwest::Error) -> CustomHttpError {
    let mut current: Option<&dyn Error> = Some(&error);
    while let Some(err) = current {
        if let Some(custom) = err.downcast_ref::<CustomHttpError>() {
            return custom.clone();
        }
        current = err.source();
    }
    CustomHttpError::Network(error.to_string())
}

pub fn build_custom_http_client(config: &AppConfig) -> Result<CustomHttpClient, CustomHttpError> {
    build_custom_http_client_with_lookup(config, Arc::new(GaiHostLookup))
}

pub fn build_custom_http_client_with_lookup(
    config: &AppConfig,
    lookup: Arc<dyn HostLookup>,
) -> Result<CustomHttpClient, CustomHttpError> {
    if custom_proxy_directive(config) == CustomProxyDirective::Unavailable {
        return Err(CustomHttpError::ProxyUnavailable(
            "Custom HTTP Auto proxy mode is unavailable because the effective system proxy cannot be distinguished from the origin during connect-time DNS validation; use Direct or Manual mode"
                .to_string(),
        ));
    }
    let timeout_secs = config
        .connect_timeout_secs
        .clamp(CUSTOM_MIN_TIMEOUT_SECS, CUSTOM_MAX_TIMEOUT_SECS);
    let resolver = FilteringResolve {
        lookup: lookup.clone(),
        proxy_hosts: manual_proxy_resolution_hosts(config),
    };
    let client = crate::http_client::configured_builder(config)
        .map_err(|error| CustomHttpError::Build(error.to_string()))?
        .redirect(Policy::none())
        .connect_timeout(Duration::from_secs(timeout_secs))
        .timeout(Duration::from_secs(timeout_secs))
        .dns_resolver(Arc::new(resolver))
        .build()
        .map_err(|error| CustomHttpError::Build(error.to_string()))?;
    Ok(CustomHttpClient {
        client,
        lookup,
        proxy_mode: config.proxy_mode,
    })
}

fn manual_proxy_resolution_hosts(config: &AppConfig) -> Vec<String> {
    if config.proxy_mode != ProxyMode::Manual {
        return Vec::new();
    }
    let Ok(url) = reqwest::Url::parse(config.proxy_url.trim()) else {
        return Vec::new();
    };
    url.host_str().map(normalize_dns_name).into_iter().collect()
}

struct FilteringResolve {
    lookup: Arc<dyn HostLookup>,
    /// Normalized Manual proxy hostname/IP. Origin host-kind policy is never
    /// applied to these names so a named LAN proxy is not weakened or bypassed.
    proxy_hosts: Vec<String>,
}

impl fmt::Debug for FilteringResolve {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FilteringResolve").finish_non_exhaustive()
    }
}

impl Resolve for FilteringResolve {
    fn resolve(&self, name: Name) -> Resolving {
        let host = name.as_str().to_string();
        let lookup = self.lookup.clone();
        let proxy_hosts = self.proxy_hosts.clone();
        Box::pin(async move {
            resolve_connect_addrs(lookup.as_ref(), &proxy_hosts, &host)
                .await
                .map(|addrs| Box::new(addrs.into_iter()) as Addrs)
                .map_err(|error| Box::new(error) as _)
        })
    }
}

async fn resolve_connect_addrs(
    lookup: &dyn HostLookup,
    proxy_hosts: &[String],
    host: &str,
) -> Result<Vec<SocketAddr>, CustomHttpError> {
    if is_manual_proxy_resolution_host(proxy_hosts, host) {
        let ips = lookup.resolve_host(host).await?;
        if ips.is_empty() {
            return Err(CustomHttpError::EmptyResolution);
        }
        return Ok(ips.into_iter().map(|ip| SocketAddr::new(ip, 0)).collect());
    }
    let policy = custom_origin_host_policy(host)
        .map_err(|error| CustomHttpError::UnsafeResolution(error.to_string()))?;
    let ips = lookup.resolve_host(host).await?;
    evaluate_resolved_ips(policy, &ips)?;
    Ok(ips.into_iter().map(|ip| SocketAddr::new(ip, 0)).collect())
}

fn is_manual_proxy_resolution_host(proxy_hosts: &[String], host: &str) -> bool {
    let host = normalize_dns_name(host);
    proxy_hosts.iter().any(|candidate| candidate == &host)
}

/// Re-evaluate the destination on every request. Empty, mixed, and all-unsafe
/// sets fail closed. Proxied requests still run this local view; proxy-side DNS
/// remains residual risk and is never bypassed.
pub async fn preflight_custom_url(
    url: &reqwest::Url,
    lookup: &dyn HostLookup,
) -> Result<Vec<IpAddr>, CustomHttpError> {
    let target = inspect_custom_url(url).map_err(CustomHttpError::from)?;
    let ips = match &target.host {
        CustomUrlHost::Ip(ip) => vec![*ip],
        CustomUrlHost::Domain(domain) => lookup.resolve_host(domain).await?,
    };
    evaluate_resolved_ips(target.policy, &ips)?;
    Ok(ips)
}

fn evaluate_resolved_ips(policy: CustomHostPolicy, ips: &[IpAddr]) -> Result<(), CustomHttpError> {
    if ips.is_empty() {
        return Err(CustomHttpError::EmptyResolution);
    }
    let classes: Vec<CustomIpClass> = ips.iter().copied().map(classify_custom_ip).collect();
    if classes.contains(&CustomIpClass::Blocked) {
        return Err(CustomHttpError::UnsafeResolution(
            "resolved set contains a blocked address".to_string(),
        ));
    }
    let all_loopback = classes
        .iter()
        .all(|class| *class == CustomIpClass::Loopback);
    let all_public = classes.iter().all(|class| *class == CustomIpClass::Public);
    match policy {
        CustomHostPolicy::LoopbackOnly if all_loopback => Ok(()),
        CustomHostPolicy::PublicOnly if all_public => Ok(()),
        CustomHostPolicy::LoopbackOnly => Err(CustomHttpError::UnsafeResolution(
            "declared localhost names must resolve only to loopback".to_string(),
        )),
        CustomHostPolicy::PublicOnly => Err(CustomHttpError::UnsafeResolution(
            "public Custom hosts must resolve only to public addresses".to_string(),
        )),
    }
}

/// Join `path` onto a persisted Custom base URL while keeping the origin and
/// path prefix. Absolute URLs, protocol-relative targets, decoded dot-segments,
/// encoded slash/backslash, and nested percent-encoding are rejected as
/// endpoint override.
pub fn join_custom_endpoint(base_url: &str, path: &str) -> Result<reqwest::Url, CustomHttpError> {
    let canonical = validate_custom_base_url(base_url).map_err(CustomHttpError::from)?;
    let base = reqwest::Url::parse(&canonical)
        .map_err(|error| CustomHttpError::InvalidUrl(error.to_string()))?;
    let relative = path.trim();
    if relative.is_empty() {
        return Ok(base);
    }
    if is_endpoint_override(relative) {
        return Err(CustomHttpError::EndpointOverride(relative.to_string()));
    }
    let stripped = relative.trim_start_matches('/');
    let joined = format!("{canonical}/{stripped}");
    let parsed = reqwest::Url::parse(&joined)
        .map_err(|error| CustomHttpError::InvalidUrl(error.to_string()))?;
    if parsed.scheme() != base.scheme()
        || parsed.host() != base.host()
        || parsed.port_or_known_default() != base.port_or_known_default()
    {
        return Err(CustomHttpError::EndpointOverride(relative.to_string()));
    }
    if parsed.query().is_some() || parsed.fragment().is_some() {
        return Err(CustomHttpError::EndpointOverride(
            "joined endpoint must not include a query or fragment".to_string(),
        ));
    }
    if !path_has_prefix(parsed.path(), base.path()) {
        return Err(CustomHttpError::EndpointOverride(
            "joined path escaped the Custom base prefix".to_string(),
        ));
    }
    if path_has_unsafe_segments(parsed.path()) {
        return Err(CustomHttpError::EndpointOverride(
            "joined path must not contain unsafe or recursively encoded segments".to_string(),
        ));
    }
    Ok(parsed)
}

fn is_endpoint_override(path: &str) -> bool {
    let trimmed = path.trim();
    if trimmed.contains("://")
        || trimmed.starts_with("//")
        || trimmed.starts_with('\\')
        || trimmed.contains('\\')
    {
        return true;
    }
    if trimmed.contains('\0') || trimmed.chars().any(char::is_control) {
        return true;
    }
    if path_has_unsafe_segments(trimmed) {
        return true;
    }
    matches!(
        reqwest::Url::parse(trimmed).ok().map(|url| url.scheme().to_string()),
        Some(scheme) if matches!(
            scheme.as_str(),
            "http" | "https" | "ftp" | "file" | "ws" | "wss" | "javascript" | "data"
        )
    )
}

fn path_has_unsafe_segments(path: &str) -> bool {
    for segment in path.split(['/', '\\']) {
        if segment.is_empty() {
            continue;
        }
        if segment == "."
            || segment == ".."
            || segment.contains('\0')
            || segment.chars().any(char::is_control)
        {
            return true;
        }
        match percent_decode_utf8(segment) {
            Some(decoded)
                if decoded == "."
                    || decoded == ".."
                    || decoded.contains('/')
                    || decoded.contains('\\')
                    || decoded.contains('\0')
                    || decoded.chars().any(char::is_control)
                    || contains_percent_escape(&decoded) =>
            {
                return true;
            }
            None => return true,
            Some(_) => {}
        }
    }
    false
}

fn contains_percent_escape(input: &str) -> bool {
    input.as_bytes().windows(3).any(|window| {
        window[0] == b'%' && hex_val(window[1]).is_some() && hex_val(window[2]).is_some()
    })
}

fn percent_decode_utf8(input: &str) -> Option<String> {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            if index + 2 >= bytes.len() {
                return None;
            }
            let value = hex_pair(bytes[index + 1], bytes[index + 2])?;
            out.push(value);
            index += 3;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(out).ok()
}

fn hex_pair(high: u8, low: u8) -> Option<u8> {
    Some((hex_val(high)? << 4) | hex_val(low)?)
}

fn hex_val(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn path_has_prefix(path: &str, prefix: &str) -> bool {
    match prefix {
        "" | "/" => path.starts_with('/'),
        other => {
            let prefix = other.trim_end_matches('/');
            path == prefix || path.starts_with(&format!("{prefix}/"))
        }
    }
}

const FORBIDDEN_CLIENT_HEADERS: &[&str] = &[
    "cookie",
    "set-cookie",
    "authorization",
    "proxy-authorization",
    "x-api-key",
    "x-goog-api-key",
    "x-ocg-session",
];

/// Build Custom upstream auth headers. Callers cannot supply inbound client or
/// dashboard headers; [`CustomHttpClient::send_preflighted`] is the only send
/// path and always composes this map.
pub fn isolated_custom_headers(
    scheme: UpstreamAuthScheme,
    api_key: &str,
) -> Result<HeaderMap, CustomHttpError> {
    let mut headers = HeaderMap::new();
    match scheme {
        UpstreamAuthScheme::Bearer => {
            let value = HeaderValue::from_str(&format!("Bearer {api_key}"))
                .map_err(|error| CustomHttpError::InvalidUrl(error.to_string()))?;
            headers.insert(AUTHORIZATION, value);
        }
        UpstreamAuthScheme::XApiKey => {
            let value = HeaderValue::from_str(api_key)
                .map_err(|error| CustomHttpError::InvalidUrl(error.to_string()))?;
            headers.insert(HeaderName::from_static("x-api-key"), value);
        }
    }
    Ok(headers)
}

pub fn forbidden_forwarded_header_names() -> &'static [&'static str] {
    FORBIDDEN_CLIENT_HEADERS
}

pub fn header_map_contains_forbidden_client_credentials(
    headers: &HeaderMap,
    scheme: UpstreamAuthScheme,
) -> bool {
    headers.keys().any(|name| {
        let lower = name.as_str();
        match scheme {
            UpstreamAuthScheme::Bearer if lower == "authorization" => false,
            UpstreamAuthScheme::XApiKey if lower == "x-api-key" => false,
            _ => FORBIDDEN_CLIENT_HEADERS.contains(&lower),
        }
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CustomHttpError {
    InvalidUrl(String),
    EmptyResolution,
    UnsafeResolution(String),
    EndpointOverride(String),
    Resolution(String),
    ProxyUnavailable(String),
    Build(String),
    Network(String),
}

impl fmt::Display for CustomHttpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidUrl(message)
            | Self::UnsafeResolution(message)
            | Self::EndpointOverride(message)
            | Self::Resolution(message)
            | Self::ProxyUnavailable(message)
            | Self::Build(message)
            | Self::Network(message) => f.write_str(message),
            Self::EmptyResolution => f.write_str("Custom DNS resolution returned no addresses"),
        }
    }
}

impl std::error::Error for CustomHttpError {}

impl From<crate::provider::ProviderBindingError> for CustomHttpError {
    fn from(error: crate::provider::ProviderBindingError) -> Self {
        Self::InvalidUrl(error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::AppConfig;
    use reqwest::StatusCode;
    use std::net::Ipv4Addr;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn test_config(mode: ProxyMode, proxy_url: &str) -> AppConfig {
        let mut config = AppConfig::default();
        config.proxy_mode = mode;
        config.proxy_url = proxy_url.to_string();
        config.connect_timeout_secs = 5;
        config
    }

    async fn send_get(
        client: &CustomHttpClient,
        url: reqwest::Url,
    ) -> Result<reqwest::Response, CustomHttpError> {
        client
            .send_preflighted(
                reqwest::Method::GET,
                url,
                UpstreamAuthScheme::Bearer,
                "test-key",
                None,
            )
            .await
    }

    async fn serve_http(
        status: u16,
        reason: &str,
        headers: &[(&str, String)],
        body: &str,
        hits: Arc<AtomicUsize>,
    ) -> std::net::SocketAddr {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("loopback listener");
        let addr = listener.local_addr().unwrap();
        let reason = reason.to_string();
        let body = body.to_string();
        let headers = headers
            .iter()
            .map(|(name, value)| (name.to_string(), value.clone()))
            .collect::<Vec<_>>();
        tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    break;
                };
                hits.fetch_add(1, Ordering::SeqCst);
                let mut buf = vec![0_u8; 4096];
                let _ = stream.read(&mut buf).await;
                let mut response = format!("HTTP/1.1 {status} {reason}\r\n");
                for (name, value) in &headers {
                    response.push_str(&format!("{name}: {value}\r\n"));
                }
                response.push_str(&format!(
                    "Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                ));
                let _ = stream.write_all(response.as_bytes()).await;
            }
        });
        addr
    }

    async fn serve_counting_proxy(hits: Arc<AtomicUsize>) -> std::net::SocketAddr {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("proxy listener");
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    break;
                };
                hits.fetch_add(1, Ordering::SeqCst);
                let mut buf = vec![0_u8; 4096];
                let _ = stream.read(&mut buf).await;
                let body = "proxy";
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes()).await;
            }
        });
        addr
    }

    #[test]
    fn custom_proxy_directives_fail_closed_when_auto_cannot_be_proven() {
        let auto = test_config(ProxyMode::Auto, "");
        assert_eq!(
            custom_proxy_directive(&auto),
            CustomProxyDirective::Unavailable
        );
        assert!(matches!(
            build_custom_http_client(&auto),
            Err(CustomHttpError::ProxyUnavailable(_))
        ));

        let direct = test_config(ProxyMode::Direct, "");
        assert_eq!(
            custom_proxy_directive(&direct),
            CustomProxyDirective::Disabled
        );
        assert!(build_custom_http_client(&direct).is_ok());

        let manual = test_config(ProxyMode::Manual, "http://127.0.0.1:8080");
        assert_eq!(
            custom_proxy_directive(&manual),
            CustomProxyDirective::Explicit
        );
        assert!(build_custom_http_client(&manual).is_ok());
    }

    #[test]
    fn join_custom_endpoint_preserves_prefix_and_rejects_override() {
        let joined =
            join_custom_endpoint("https://api.example.com/v1", "chat/completions").unwrap();
        assert_eq!(
            joined.as_str(),
            "https://api.example.com/v1/chat/completions"
        );
        let absolute_slash =
            join_custom_endpoint("https://api.example.com/v1", "/chat/completions").unwrap();
        assert_eq!(
            absolute_slash.as_str(),
            "https://api.example.com/v1/chat/completions"
        );
        assert_eq!(
            join_custom_endpoint("https://api.example.com", "v1/models")
                .unwrap()
                .as_str(),
            "https://api.example.com/v1/models"
        );
        assert!(
            join_custom_endpoint("https://api.example.com/v1", "https://evil.example/x").is_err()
        );
        assert!(join_custom_endpoint("https://api.example.com/v1", "//evil.example/x").is_err());
        assert!(join_custom_endpoint("https://api.example.com/v1", "../admin").is_err());
        assert!(join_custom_endpoint("https://api.example.com/v1", "foo/../admin").is_err());
        assert!(join_custom_endpoint("https://api.example.com/v1", "foo/./bar").is_err());
        assert!(join_custom_endpoint("https://api.example.com/v1", "%2e%2e/admin").is_err());
        assert!(join_custom_endpoint("https://api.example.com/v1", "foo/%2e%2e/admin").is_err());
        assert!(join_custom_endpoint("https://api.example.com/v1", "foo%2fadmin").is_err());
        assert!(join_custom_endpoint("https://api.example.com/v1", "foo%2Fadmin").is_err());
        assert!(join_custom_endpoint("https://api.example.com/v1", "foo%5cadmin").is_err());
        assert!(join_custom_endpoint("https://api.example.com/v1", "foo%5Cadmin").is_err());
        assert!(join_custom_endpoint("https://api.example.com/v1", "%252e%252e/admin").is_err());
        assert!(
            join_custom_endpoint("https://api.example.com/v1", "foo/%252E%252E/admin").is_err()
        );
        assert!(
            join_custom_endpoint("https://api.example.com/v1", "%252f%252fevil.example/x").is_err()
        );
        assert!(join_custom_endpoint("https://api.example.com/v1", "foo%255cadmin").is_err());
        assert!(
            join_custom_endpoint("https://api.example.com/v1", "%25252e%25252e/admin").is_err()
        );
        assert!(join_custom_endpoint("https://api.example.com/v1", "nested%2520space").is_err());
        assert_eq!(
            join_custom_endpoint("https://api.example.com/v1", "hello%20world")
                .unwrap()
                .as_str(),
            "https://api.example.com/v1/hello%20world"
        );
    }

    #[test]
    fn isolated_headers_do_not_copy_client_or_dashboard_credentials() {
        let bearer = isolated_custom_headers(UpstreamAuthScheme::Bearer, "sk-custom").unwrap();
        assert_eq!(bearer.get(AUTHORIZATION).unwrap(), "Bearer sk-custom");
        assert!(!header_map_contains_forbidden_client_credentials(
            &bearer,
            UpstreamAuthScheme::Bearer
        ));
        assert!(bearer.get("cookie").is_none());
        assert!(bearer.get("x-api-key").is_none());
        assert!(bearer.get("x-goog-api-key").is_none());
        assert_eq!(bearer.len(), 1);

        let x_api = isolated_custom_headers(UpstreamAuthScheme::XApiKey, "sk-custom").unwrap();
        assert_eq!(x_api.get("x-api-key").unwrap(), "sk-custom");
        assert!(x_api.get(AUTHORIZATION).is_none());
        assert!(!header_map_contains_forbidden_client_credentials(
            &x_api,
            UpstreamAuthScheme::XApiKey
        ));
        assert_eq!(x_api.len(), 1);
        assert!(forbidden_forwarded_header_names().contains(&"cookie"));
        assert!(forbidden_forwarded_header_names().contains(&"authorization"));
    }

    #[tokio::test]
    async fn preflight_rejects_empty_mixed_and_rebinding_sets() {
        let lookup = ScriptedHostLookup::default();
        lookup.enqueue("localhost", vec![IpAddr::V4(Ipv4Addr::LOCALHOST)]);
        lookup.enqueue(
            "localhost",
            vec![
                IpAddr::V4(Ipv4Addr::LOCALHOST),
                IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8)),
            ],
        );
        lookup.enqueue("localhost", Vec::new());
        lookup.enqueue(
            "localhost",
            vec![IpAddr::V4(Ipv4Addr::new(169, 254, 169, 254))],
        );
        lookup.enqueue(
            "api.example.test",
            vec![IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8))],
        );
        lookup.enqueue(
            "api.example.test",
            vec![
                IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8)),
                IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)),
            ],
        );
        lookup.enqueue("api.example.test", vec![IpAddr::V4(Ipv4Addr::LOCALHOST)]);

        let loopback = reqwest::Url::parse("http://localhost:9/v1").unwrap();
        assert!(preflight_custom_url(&loopback, &lookup).await.is_ok());
        assert!(matches!(
            preflight_custom_url(&loopback, &lookup).await,
            Err(CustomHttpError::UnsafeResolution(_))
        ));
        assert!(matches!(
            preflight_custom_url(&loopback, &lookup).await,
            Err(CustomHttpError::EmptyResolution)
        ));
        assert!(matches!(
            preflight_custom_url(&loopback, &lookup).await,
            Err(CustomHttpError::UnsafeResolution(_))
        ));

        let public = reqwest::Url::parse("https://api.example.test/v1").unwrap();
        assert!(preflight_custom_url(&public, &lookup).await.is_ok());
        assert!(matches!(
            preflight_custom_url(&public, &lookup).await,
            Err(CustomHttpError::UnsafeResolution(_))
        ));
        assert!(matches!(
            preflight_custom_url(&public, &lookup).await,
            Err(CustomHttpError::UnsafeResolution(_))
        ));
        assert!(crate::provider::is_declared_loopback_hostname("localhost"));
        assert!(crate::provider::is_declared_loopback_hostname(
            "app.localhost"
        ));
        assert!(!crate::provider::is_declared_loopback_hostname(
            "example.com"
        ));
    }

    #[tokio::test]
    async fn scripted_dns_rebinding_is_re_evaluated_per_request() {
        let hits = Arc::new(AtomicUsize::new(0));
        let addr = serve_http(200, "OK", &[], "ok", hits.clone()).await;
        let lookup = Arc::new(ScriptedHostLookup::default());
        // Preflight and the injected Resolve each consume one scripted answer
        // per successful request. The rebind answer is only seen by preflight.
        lookup.enqueue("localhost", vec![IpAddr::V4(Ipv4Addr::LOCALHOST)]);
        lookup.enqueue("localhost", vec![IpAddr::V4(Ipv4Addr::LOCALHOST)]);
        lookup.enqueue(
            "localhost",
            vec![IpAddr::V4(Ipv4Addr::new(169, 254, 169, 254))],
        );

        let client =
            build_custom_http_client_with_lookup(&test_config(ProxyMode::Direct, ""), lookup)
                .unwrap();
        let url = reqwest::Url::parse(&format!("http://localhost:{}/v1", addr.port())).unwrap();
        let first = send_get(&client, url.clone())
            .await
            .expect("first loopback resolution should connect");
        assert_eq!(first.status(), StatusCode::OK);
        let rebound = send_get(&client, url)
            .await
            .expect_err("rebind to link-local metadata must fail closed");
        assert!(matches!(rebound, CustomHttpError::UnsafeResolution(_)));
        assert_eq!(hits.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn redirects_are_not_followed_for_301_302_307_308() {
        for status in [301_u16, 302, 307, 308] {
            let second_hits = Arc::new(AtomicUsize::new(0));
            let second = serve_http(200, "OK", &[], "second", second_hits.clone()).await;
            let first_hits = Arc::new(AtomicUsize::new(0));
            let location = format!("http://127.0.0.1:{}/next", second.port());
            let first = serve_http(
                status,
                "Redirect",
                &[("Location", location)],
                "",
                first_hits.clone(),
            )
            .await;
            let client = build_custom_http_client(&test_config(ProxyMode::Direct, "")).unwrap();
            let url =
                reqwest::Url::parse(&format!("http://127.0.0.1:{}/start", first.port())).unwrap();
            let response = send_get(&client, url).await.unwrap();
            assert_eq!(response.status().as_u16(), status, "status {status}");
            tokio::time::sleep(Duration::from_millis(50)).await;
            assert_eq!(first_hits.load(Ordering::SeqCst), 1, "first hop {status}");
            assert_eq!(
                second_hits.load(Ordering::SeqCst),
                0,
                "redirect {status} must not open a second connection"
            );
        }
    }

    #[tokio::test]
    async fn direct_does_not_use_manual_proxy_and_manual_does_not_bypass_it() {
        let upstream_hits = Arc::new(AtomicUsize::new(0));
        let upstream = serve_http(200, "OK", &[], "direct", upstream_hits.clone()).await;
        let proxy_hits = Arc::new(AtomicUsize::new(0));
        let proxy = serve_counting_proxy(proxy_hits.clone()).await;

        let target =
            reqwest::Url::parse(&format!("http://127.0.0.1:{}/v1", upstream.port())).unwrap();
        let direct = build_custom_http_client(&test_config(ProxyMode::Direct, "")).unwrap();
        let response = send_get(&direct, target.clone()).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(upstream_hits.load(Ordering::SeqCst), 1);
        assert_eq!(proxy_hits.load(Ordering::SeqCst), 0);

        let manual = build_custom_http_client(&test_config(
            ProxyMode::Manual,
            &format!("http://127.0.0.1:{}", proxy.port()),
        ))
        .unwrap();
        // Local preflight still runs for the loopback target even though the
        // proxy would otherwise be the hop that talks to the origin. Manual
        // mode must not bypass that proxy to reach `upstream` directly.
        let proxied = send_get(&manual, target).await.unwrap();
        assert_eq!(proxied.status(), StatusCode::OK);
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(proxy_hits.load(Ordering::SeqCst), 1);
        assert_eq!(upstream_hits.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn resolve_connect_addrs_fails_closed_and_returns_complete_allowed_set() {
        let lookup = ScriptedHostLookup::default();
        lookup.enqueue(
            "api.example.test",
            vec![
                IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8)),
                IpAddr::V4(Ipv4Addr::new(1, 1, 1, 1)),
            ],
        );
        lookup.enqueue(
            "api.example.test",
            vec![
                IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8)),
                IpAddr::V4(Ipv4Addr::LOCALHOST),
            ],
        );
        lookup.enqueue(
            "api.example.test",
            vec![
                IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8)),
                IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)),
            ],
        );
        lookup.enqueue("localhost", vec![IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8))]);
        lookup.enqueue("proxy.local", vec![IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1))]);

        let allowed = resolve_connect_addrs(&lookup, &[], "api.example.test")
            .await
            .expect("all-public set must be returned in full");
        assert_eq!(allowed.len(), 2);
        assert!(
            allowed
                .iter()
                .all(|addr| classify_custom_ip(addr.ip()) == CustomIpClass::Public)
        );

        assert!(matches!(
            resolve_connect_addrs(&lookup, &[], "api.example.test").await,
            Err(CustomHttpError::UnsafeResolution(_))
        ));
        assert!(matches!(
            resolve_connect_addrs(&lookup, &[], "api.example.test").await,
            Err(CustomHttpError::UnsafeResolution(_))
        ));
        assert!(matches!(
            resolve_connect_addrs(&lookup, &[], "localhost").await,
            Err(CustomHttpError::UnsafeResolution(_))
        ));
        assert!(matches!(
            resolve_connect_addrs(&lookup, &[], "proxy.local").await,
            Err(CustomHttpError::UnsafeResolution(_))
        ));

        let proxy_addrs =
            resolve_connect_addrs(&lookup, &[normalize_dns_name("proxy.local")], "proxy.local")
                .await
                .expect("Manual proxy host must not use origin host-kind policy");
        assert_eq!(proxy_addrs[0].ip(), IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)));
    }

    #[tokio::test]
    async fn connect_resolver_rejects_host_kind_change_and_mixed_sets() {
        let public = IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8));
        let loopback = IpAddr::V4(Ipv4Addr::LOCALHOST);
        let private = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1));
        let cases = [
            (
                "api.example.test",
                "https",
                vec![public],
                vec![loopback],
                "preflight public then resolver loopback",
            ),
            (
                "api.example.test",
                "https",
                vec![public],
                vec![public, loopback],
                "preflight public then mixed public+loopback",
            ),
            (
                "api.example.test",
                "https",
                vec![public],
                vec![public, private],
                "preflight public then mixed public+private",
            ),
            (
                "localhost",
                "http",
                vec![loopback],
                vec![public],
                "localhost loopback then resolver public",
            ),
        ];
        for (host, scheme, preflight, connect, label) in cases {
            let hits = Arc::new(AtomicUsize::new(0));
            let addr = serve_http(200, "OK", &[], "ok", hits.clone()).await;
            let lookup = Arc::new(ScriptedHostLookup::default());
            lookup.enqueue(host, preflight);
            lookup.enqueue(host, connect);
            let client =
                build_custom_http_client_with_lookup(&test_config(ProxyMode::Direct, ""), lookup)
                    .unwrap();
            let url =
                reqwest::Url::parse(&format!("{scheme}://{host}:{}/v1", addr.port())).unwrap();
            let err = send_get(&client, url).await.expect_err(label);
            assert!(
                matches!(err, CustomHttpError::UnsafeResolution(_)),
                "{label}: expected UnsafeResolution, got {err:?}"
            );
            assert_eq!(
                hits.load(Ordering::SeqCst),
                0,
                "{label}: origin must not be contacted"
            );
        }
    }

    #[tokio::test]
    async fn connect_time_empty_and_blocked_answers_never_reach_origin() {
        let public = IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8));
        let cases = [
            (Vec::new(), true, "empty connect-time DNS answer"),
            (
                vec![IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1))],
                false,
                "blocked connect-time DNS answer",
            ),
        ];

        for (connect_answer, expect_empty, label) in cases {
            let hits = Arc::new(AtomicUsize::new(0));
            let origin = serve_http(200, "OK", &[], "origin", hits.clone()).await;
            let lookup = Arc::new(ScriptedHostLookup::default());
            lookup.enqueue("api.example.test", vec![public]);
            lookup.enqueue("api.example.test", connect_answer);
            let client =
                build_custom_http_client_with_lookup(&test_config(ProxyMode::Direct, ""), lookup)
                    .unwrap();
            let url =
                reqwest::Url::parse(&format!("https://api.example.test:{}/v1", origin.port()))
                    .unwrap();

            let error = send_get(&client, url).await.expect_err(label);
            if expect_empty {
                assert!(
                    matches!(error, CustomHttpError::EmptyResolution),
                    "{label}: expected EmptyResolution, got {error:?}"
                );
            } else {
                assert!(
                    matches!(error, CustomHttpError::UnsafeResolution(_)),
                    "{label}: expected UnsafeResolution, got {error:?}"
                );
            }
            assert_eq!(
                hits.load(Ordering::SeqCst),
                0,
                "{label}: origin must not be contacted"
            );
        }
    }

    #[tokio::test]
    async fn manual_named_lan_proxy_is_not_subject_to_origin_host_policy() {
        let upstream_hits = Arc::new(AtomicUsize::new(0));
        let upstream = serve_http(200, "OK", &[], "direct", upstream_hits.clone()).await;
        let proxy_hits = Arc::new(AtomicUsize::new(0));
        let proxy = serve_counting_proxy(proxy_hits.clone()).await;
        let lookup = Arc::new(ScriptedHostLookup::default());
        lookup.enqueue("proxy.local", vec![IpAddr::V4(Ipv4Addr::LOCALHOST)]);

        let client = build_custom_http_client_with_lookup(
            &test_config(
                ProxyMode::Manual,
                &format!("http://proxy.local:{}", proxy.port()),
            ),
            lookup,
        )
        .unwrap();
        let target =
            reqwest::Url::parse(&format!("http://127.0.0.1:{}/v1", upstream.port())).unwrap();
        let proxied = send_get(&client, target)
            .await
            .expect("named LAN proxy must not be rejected by origin host-kind policy");
        assert_eq!(proxied.status(), StatusCode::OK);
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(proxy_hits.load(Ordering::SeqCst), 1);
        assert_eq!(upstream_hits.load(Ordering::SeqCst), 0);
        assert!(crate::provider::custom_origin_host_policy("proxy.local").is_err());
    }
}

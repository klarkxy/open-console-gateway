//! Dedicated Custom API HTTP client and reusable inference HTTP primitives.
//!
//! Custom destinations are administrator-trusted. Direct, Manual, and Auto all
//! inherit the process-wide proxy policy from [`crate::http_client`]. The
//! client never follows redirects, never forwards dashboard/client auth, and
//! always composes isolated Bearer / `x-api-key` / `api-key` headers. IsolatedTrustedAdmin
//! connectors attach a destination DNS guard; an explicit or system proxy still
//! owns destination DNS and is not forced Direct.
//!
//! Catalog-free transport mechanics live in [`ocg_infra::inference_http`]. This
//! module maps [`AppConfig`] through [`crate::http_client::outbound_proxy_spec`]
//! and [`UpstreamAuthScheme`] onto that transport. Custom product policy stays
//! here: URL trust, 5-60s connect clamp, isolated send gating, and JSON /
//! forbidden-header helpers.

use crate::models::{AppConfig, ProxyMode};
use crate::provider::{ProviderBindingError, UpstreamAuthScheme, UpstreamProtocolKind};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::sync::Arc;
use std::time::Duration;

pub use ocg_infra::inference_http::{
    InferenceHttpError, InferenceRedirectPolicy, apply_inference_request_timeout,
    join_inference_endpoint,
};

/// Structured Custom URL host taken from [`reqwest::Url::host`], not `host_str`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CustomUrlHost {
    Ip(IpAddr),
    Domain(String),
}

/// Syntactic Custom URL inspection shared by persistence and HTTP joining.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustomUrlTarget {
    pub host: CustomUrlHost,
}

mod dns;
mod origin_grant;

pub use origin_grant::{
    OriginGrantError, enforce_attempt_secret_origin, ensure_sealed_secret_origin,
    ensure_secret_origin_granted, follows_redirects_with_secret, inference_origin,
    is_loopback_inference_origin, origins_match,
};

/// Syntactic Custom inference-endpoint gate. Administrators may select a public,
/// LAN, or loopback local-model destination. Embedded credentials and
/// non-HTTP(S) schemes stay rejected. Metadata, link-local, unspecified, and
/// opaque IPv4-trick hosts are refused here so they never become an outbound
/// Custom or dynamic URL.
pub fn validate_custom_endpoint_url(value: &str) -> Result<String, ProviderBindingError> {
    let value = value.trim();
    if value.is_empty() {
        return Err(ProviderBindingError::InvalidCustomBaseUrl(
            "endpoint URL is required".to_string(),
        ));
    }
    if value.len() > 2048 {
        return Err(ProviderBindingError::InvalidCustomBaseUrl(
            "endpoint URL is too long".to_string(),
        ));
    }
    let parsed = reqwest::Url::parse(value).map_err(|error| {
        ProviderBindingError::InvalidCustomBaseUrl(format!("invalid endpoint URL: {error}"))
    })?;
    inspect_custom_url(&parsed)?;
    if parsed.query().is_some() || parsed.fragment().is_some() {
        return Err(ProviderBindingError::InvalidCustomBaseUrl(
            "endpoint URL must not include a query or fragment".to_string(),
        ));
    }
    Ok(parsed.as_str().trim_end_matches('/').to_string())
}

pub fn parse_custom_endpoint_url(value: &str) -> Result<reqwest::Url, CustomHttpError> {
    let canonical = validate_custom_endpoint_url(value).map_err(CustomHttpError::from)?;
    reqwest::Url::parse(&canonical)
        .map_err(|error| CustomHttpError::InvalidUrl(format!("invalid endpoint URL: {error}")))
}

/// Runtime interpretation of one persisted Custom URL.
///
/// Root URLs and bases ending in `/v1` use the common OpenAI-compatible base
/// convention. Existing complete standard endpoints and opaque non-standard
/// endpoints remain callable verbatim, so this policy needs no data migration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedCustomEndpoints {
    pub inference: reqwest::Url,
    pub models: Option<reqwest::Url>,
}

pub fn resolve_custom_endpoints(
    value: &str,
    protocol: UpstreamProtocolKind,
) -> Result<ResolvedCustomEndpoints, CustomHttpError> {
    let endpoint = parse_custom_endpoint_url(value)?;
    let protocol_path = match protocol {
        UpstreamProtocolKind::ChatCompletions => "chat/completions",
        UpstreamProtocolKind::Responses => "responses",
        UpstreamProtocolKind::Messages => "messages",
    };
    let protocol_suffix = format!("/{protocol_path}");
    let path = endpoint.path().trim_end_matches('/');

    if path.is_empty() {
        return Ok(ResolvedCustomEndpoints {
            inference: join_inference_endpoint(endpoint.as_str(), &format!("v1/{protocol_path}"))
                .map_err(CustomHttpError::from)?,
            models: Some(
                join_inference_endpoint(endpoint.as_str(), "v1/models")
                    .map_err(CustomHttpError::from)?,
            ),
        });
    }

    if path.ends_with("/v1") {
        return Ok(ResolvedCustomEndpoints {
            inference: join_inference_endpoint(endpoint.as_str(), protocol_path)
                .map_err(CustomHttpError::from)?,
            models: Some(
                join_inference_endpoint(endpoint.as_str(), "models")
                    .map_err(CustomHttpError::from)?,
            ),
        });
    }

    if let Some(prefix) = path.strip_suffix(&protocol_suffix) {
        let mut models = endpoint.clone();
        let models_path = if prefix.is_empty() {
            "/models".to_string()
        } else {
            format!("{}/models", prefix.trim_end_matches('/'))
        };
        models.set_path(&models_path);
        return Ok(ResolvedCustomEndpoints {
            inference: endpoint,
            models: Some(models),
        });
    }

    Ok(ResolvedCustomEndpoints {
        inference: endpoint,
        models: None,
    })
}

/// Custom authentication is a protocol invariant, not user configuration.
pub const fn custom_auth_scheme(protocol: UpstreamProtocolKind) -> UpstreamAuthScheme {
    match protocol {
        UpstreamProtocolKind::ChatCompletions | UpstreamProtocolKind::Responses => {
            UpstreamAuthScheme::Bearer
        }
        UpstreamProtocolKind::Messages => UpstreamAuthScheme::XApiKey,
    }
}

/// Inspect scheme, credentials, and host of a Custom URL.
///
/// Uses [`reqwest::Url::host`] so bracketed IPv6 and IPv4-mapped literals are
/// the parser's IP variants. `host_str().parse::<IpAddr>()` treats `[::ffff:…]`
/// as a hostname and is the bypass this function exists to close.
pub fn inspect_custom_url(parsed: &reqwest::Url) -> Result<CustomUrlTarget, ProviderBindingError> {
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(ProviderBindingError::InvalidCustomBaseUrl(
            "endpoint URL must use http or https".to_string(),
        ));
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(ProviderBindingError::InvalidCustomBaseUrl(
            "endpoint URL must not include credentials".to_string(),
        ));
    }
    let target = CustomUrlTarget {
        host: custom_url_host(parsed)?,
    };
    origin_grant::reject_unauthorized_custom_target(&target)?;
    Ok(target)
}

pub(crate) fn custom_url_host(
    parsed: &reqwest::Url,
) -> Result<CustomUrlHost, ProviderBindingError> {
    let host = parsed.host().ok_or_else(|| {
        ProviderBindingError::InvalidCustomBaseUrl("endpoint URL must include a host".to_string())
    })?;
    // `url::Host` is not a direct dependency (manifests stay frozen). IPv6
    // Display includes brackets; strip them to recover the parsed `Ipv6Addr`.
    let rendered = host.to_string();
    if let Some(inside) = rendered
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
    {
        let ip = inside.parse::<Ipv6Addr>().map_err(|_| {
            ProviderBindingError::InvalidCustomBaseUrl("base URL IPv6 host is invalid".to_string())
        })?;
        return Ok(CustomUrlHost::Ip(IpAddr::V6(ip)));
    }
    if let Ok(ip) = rendered.parse::<Ipv4Addr>() {
        return Ok(CustomUrlHost::Ip(IpAddr::V4(ip)));
    }
    Ok(CustomUrlHost::Domain(rendered.to_ascii_lowercase()))
}

/// Build isolated upstream auth headers. Callers supply the configured scheme
/// and key; this never copies dashboard or client credentials.
pub fn isolated_inference_headers(
    scheme: UpstreamAuthScheme,
    api_key: &str,
) -> Result<HeaderMap, InferenceHttpError> {
    ocg_infra::inference_http::isolated_inference_headers(inference_auth_scheme(scheme), api_key)
}

/// Connect timeout for the provider-neutral inference HTTP adapter.
pub fn inference_connect_timeout(config: &AppConfig) -> Duration {
    Duration::from_secs(config.connect_timeout_secs)
}

/// Custom verification and forwarding bound connection setup independently of
/// the provider-neutral transport's process-wide timeout setting.
fn custom_connect_timeout(config: &AppConfig) -> Duration {
    Duration::from_secs(config.connect_timeout_secs.clamp(5, 60))
}

fn inference_auth_scheme(
    scheme: UpstreamAuthScheme,
) -> ocg_infra::inference_http::InferenceAuthScheme {
    match scheme {
        UpstreamAuthScheme::Bearer => ocg_infra::inference_http::InferenceAuthScheme::Bearer,
        UpstreamAuthScheme::XApiKey => ocg_infra::inference_http::InferenceAuthScheme::XApiKey,
        UpstreamAuthScheme::ApiKey => ocg_infra::inference_http::InferenceAuthScheme::ApiKey,
    }
}

fn core_proxy_mode(mode: ocg_infra::http::ProxyMode) -> ProxyMode {
    match mode {
        ocg_infra::http::ProxyMode::Auto => ProxyMode::Auto,
        ocg_infra::http::ProxyMode::Manual => ProxyMode::Manual,
        ocg_infra::http::ProxyMode::Direct => ProxyMode::Direct,
        ocg_infra::http::ProxyMode::List => ProxyMode::List,
    }
}

/// Construction spec for a provider-neutral inference HTTP client.
/// Redirect policy is chosen by the owning adapter; proxy/default routing and
/// connect timeout come from process-wide [`AppConfig`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HttpInferenceTransportSpec {
    pub redirect: InferenceRedirectPolicy,
}

impl HttpInferenceTransportSpec {
    pub const fn follow_redirects() -> Self {
        Self {
            redirect: InferenceRedirectPolicy::Follow,
        }
    }

    pub const fn no_redirects() -> Self {
        Self {
            redirect: InferenceRedirectPolicy::None,
        }
    }

    fn to_infra(self) -> ocg_infra::inference_http::HttpInferenceTransportSpec {
        ocg_infra::inference_http::HttpInferenceTransportSpec::new(self.redirect)
    }

    fn from_infra(spec: ocg_infra::inference_http::HttpInferenceTransportSpec) -> Self {
        Self {
            redirect: spec.redirect(),
        }
    }
}

/// One outbound inference attempt. Auth is optional so keyless adapters can
/// reuse the same send path; callers that need isolated Bearer / `x-api-key` / `api-key`
/// supply the scheme and key here.
#[derive(Debug)]
pub struct InferenceHttpRequest<'a> {
    pub method: reqwest::Method,
    pub url: reqwest::Url,
    pub auth: Option<(UpstreamAuthScheme, &'a str)>,
    pub extra_headers: HeaderMap,
    pub body: Option<Vec<u8>>,
    pub request_timeout: Option<Duration>,
}

/// Neutral inference HTTP wrapper around [`ocg_infra::inference_http`].
/// Provider policy (Custom URL trust, permitted auth, redirect prohibition,
/// endpoint prefix isolation, verify lifecycle) stays in the owning adapter.
#[derive(Clone)]
pub struct HttpInferenceTransport {
    inner: ocg_infra::inference_http::HttpInferenceTransport,
}

impl fmt::Debug for HttpInferenceTransport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HttpInferenceTransport")
            .field("proxy_mode", &self.proxy_mode())
            .field("redirect", &self.spec().redirect)
            .finish_non_exhaustive()
    }
}

impl HttpInferenceTransport {
    pub fn build(
        config: &AppConfig,
        spec: HttpInferenceTransportSpec,
    ) -> Result<Self, InferenceHttpError> {
        Self::build_with_connect_timeout(config, spec, Self::connect_timeout(config))
    }

    /// IsolatedTrustedAdmin Custom/dynamic inference, stored/draft probes, and
    /// discovery. Attaches the destination DNS guard on the connector actually
    /// used to connect. Does not change proxy routing.
    pub(crate) fn build_isolated_trusted_admin(
        config: &AppConfig,
        spec: HttpInferenceTransportSpec,
    ) -> Result<Self, InferenceHttpError> {
        Self::build_with_connect_timeout_and_dns(
            config,
            spec,
            Self::connect_timeout(config),
            Some(dns::isolated_destination_resolver()),
        )
    }

    fn build_with_connect_timeout(
        config: &AppConfig,
        spec: HttpInferenceTransportSpec,
        connect_timeout: Duration,
    ) -> Result<Self, InferenceHttpError> {
        Self::build_with_connect_timeout_and_dns(config, spec, connect_timeout, None)
    }

    fn build_with_connect_timeout_and_dns(
        config: &AppConfig,
        spec: HttpInferenceTransportSpec,
        connect_timeout: Duration,
        dns_resolver: Option<Arc<dyn reqwest::dns::Resolve>>,
    ) -> Result<Self, InferenceHttpError> {
        let mut proxy = crate::http_client::outbound_proxy_spec(config);
        proxy.connect_timeout = connect_timeout;
        Ok(Self {
            inner: ocg_infra::inference_http::HttpInferenceTransport::build_with_dns_resolver(
                &proxy,
                spec.to_infra(),
                dns_resolver,
            )?,
        })
    }

    #[cfg(test)]
    pub(crate) fn build_isolated_trusted_admin_with_dns_resolver(
        config: &AppConfig,
        spec: HttpInferenceTransportSpec,
        connect_timeout: Duration,
        resolver: Arc<dyn reqwest::dns::Resolve>,
    ) -> Result<Self, InferenceHttpError> {
        Self::build_with_connect_timeout_and_dns(config, spec, connect_timeout, Some(resolver))
    }

    pub fn spec(&self) -> HttpInferenceTransportSpec {
        HttpInferenceTransportSpec::from_infra(self.inner.spec())
    }

    pub fn proxy_mode(&self) -> ProxyMode {
        core_proxy_mode(self.inner.proxy_mode())
    }

    pub fn redirect_policy(&self) -> InferenceRedirectPolicy {
        self.inner.redirect_policy()
    }

    pub fn connect_timeout(config: &AppConfig) -> Duration {
        inference_connect_timeout(config)
    }

    pub fn join_endpoint(base_url: &str, path: &str) -> Result<reqwest::Url, InferenceHttpError> {
        ocg_infra::inference_http::HttpInferenceTransport::join_endpoint(base_url, path)
    }

    pub fn isolated_headers(
        scheme: UpstreamAuthScheme,
        api_key: &str,
    ) -> Result<HeaderMap, InferenceHttpError> {
        isolated_inference_headers(scheme, api_key)
    }

    pub(crate) fn request(
        &self,
        method: reqwest::Method,
        url: reqwest::Url,
    ) -> reqwest::RequestBuilder {
        self.inner.request(method, url)
    }

    pub async fn send(
        &self,
        request: InferenceHttpRequest<'_>,
    ) -> Result<reqwest::Response, InferenceHttpError> {
        self.inner
            .send(ocg_infra::inference_http::InferenceHttpRequest::new(
                request.method,
                request.url,
                request
                    .auth
                    .map(|(scheme, key)| (inference_auth_scheme(scheme), key)),
                request.extra_headers,
                request.body,
                request.request_timeout,
            ))
            .await
    }

    pub async fn read_body_limited(
        response: reqwest::Response,
        max_bytes: usize,
    ) -> Result<Vec<u8>, InferenceHttpError> {
        ocg_infra::inference_http::HttpInferenceTransport::read_body_limited(response, max_bytes)
            .await
    }
}

#[derive(Clone)]
pub struct CustomHttpClient {
    transport: HttpInferenceTransport,
}

impl fmt::Debug for CustomHttpClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CustomHttpClient")
            .field("proxy_mode", &self.transport.proxy_mode())
            .finish_non_exhaustive()
    }
}

impl CustomHttpClient {
    pub fn proxy_mode(&self) -> ProxyMode {
        self.transport.proxy_mode()
    }

    pub(crate) fn request(
        &self,
        method: reqwest::Method,
        url: reqwest::Url,
    ) -> reqwest::RequestBuilder {
        self.transport.request(method, url)
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn send_isolated(
        &self,
        method: reqwest::Method,
        url: reqwest::Url,
        scheme: UpstreamAuthScheme,
        api_key: &str,
        extra_headers: HeaderMap,
        body: Option<Vec<u8>>,
        request_timeout: Option<Duration>,
    ) -> Result<reqwest::Response, CustomHttpError> {
        self.send_isolated_optional(
            method,
            url,
            Some((scheme, api_key)),
            extra_headers,
            body,
            request_timeout,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn send_isolated_optional(
        &self,
        method: reqwest::Method,
        url: reqwest::Url,
        auth: Option<(UpstreamAuthScheme, &str)>,
        extra_headers: HeaderMap,
        body: Option<Vec<u8>>,
        request_timeout: Option<Duration>,
    ) -> Result<reqwest::Response, CustomHttpError> {
        let forbidden = match auth {
            Some((scheme, _)) => {
                header_map_contains_forbidden_client_credentials(&extra_headers, scheme)
            }
            None => extra_headers
                .keys()
                .any(|name| FORBIDDEN_CLIENT_HEADERS.contains(&name.as_str())),
        };
        if forbidden {
            return Err(CustomHttpError::InvalidUrl(
                "Custom upstream request must not forward dashboard or client credentials"
                    .to_string(),
            ));
        }
        inspect_custom_url(&url).map_err(CustomHttpError::from)?;
        self.transport
            .send(InferenceHttpRequest {
                method,
                url,
                auth,
                extra_headers,
                body,
                request_timeout,
            })
            .await
            .map_err(CustomHttpError::from)
    }
}

pub fn build_custom_http_client(config: &AppConfig) -> Result<CustomHttpClient, CustomHttpError> {
    // Connect timeout only. Non-stream callers apply `non_stream_timeout_secs`
    // per request; streaming must be able to outlive that total duration.
    // Custom keeps redirect prohibition on this wrapper; the transport can
    // follow redirects when another adapter selects that spec. IsolatedTrustedAdmin
    // attaches the destination DNS guard on this connector.
    Ok(CustomHttpClient {
        transport: HttpInferenceTransport::build_with_connect_timeout_and_dns(
            config,
            HttpInferenceTransportSpec::no_redirects(),
            custom_connect_timeout(config),
            Some(dns::isolated_destination_resolver()),
        )?,
    })
}

pub(crate) fn build_custom_http_client_for_route(
    config: &AppConfig,
    route: crate::http_client::RouteLabel,
) -> Result<CustomHttpClient, CustomHttpError> {
    let mut proxy = crate::http_client::outbound_proxy_spec(config);
    proxy.connect_timeout = custom_connect_timeout(config);
    Ok(CustomHttpClient {
        transport: HttpInferenceTransport {
            inner: ocg_infra::inference_http::HttpInferenceTransport::build_for_route_with_dns_resolver(
                &proxy,
                route,
                HttpInferenceTransportSpec::no_redirects().to_infra(),
                Some(dns::isolated_destination_resolver()),
            )?,
        },
    })
}

#[cfg(test)]
pub(crate) fn build_custom_http_client_with_dns_resolver(
    config: &AppConfig,
    resolver: Arc<dyn reqwest::dns::Resolve>,
) -> Result<CustomHttpClient, CustomHttpError> {
    Ok(CustomHttpClient {
        transport: HttpInferenceTransport::build_isolated_trusted_admin_with_dns_resolver(
            config,
            HttpInferenceTransportSpec::no_redirects(),
            custom_connect_timeout(config),
            resolver,
        )?,
    })
}

#[cfg(test)]
pub(crate) use dns::{DestinationResolveLog, guarded_destination_resolver, recording_resolver};

const FORBIDDEN_CLIENT_HEADERS: &[&str] = &[
    "cookie",
    "set-cookie",
    "authorization",
    "proxy-authorization",
    "x-api-key",
    "api-key",
    "x-goog-api-key",
    "x-ocg-session",
];

/// Build Custom upstream auth headers. Callers cannot supply inbound client or
/// dashboard headers; [`CustomHttpClient::send_isolated`] is the only send
/// path and always composes this map first.
pub fn isolated_custom_headers(
    scheme: UpstreamAuthScheme,
    api_key: &str,
) -> Result<HeaderMap, CustomHttpError> {
    isolated_inference_headers(scheme, api_key).map_err(CustomHttpError::from)
}

pub fn json_content_headers(include_anthropic_version: bool) -> Result<HeaderMap, CustomHttpError> {
    let mut headers = HeaderMap::new();
    headers.insert(
        reqwest::header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    headers.insert(
        reqwest::header::ACCEPT,
        HeaderValue::from_static("application/json"),
    );
    if include_anthropic_version {
        headers.insert(
            HeaderName::from_static("anthropic-version"),
            HeaderValue::from_static("2023-06-01"),
        );
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
            UpstreamAuthScheme::ApiKey if lower == "api-key" => false,
            _ => FORBIDDEN_CLIENT_HEADERS.contains(&lower),
        }
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CustomHttpError {
    InvalidUrl(String),
    EndpointOverride(String),
    Build(String),
    Network(String),
}

impl fmt::Display for CustomHttpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidUrl(message)
            | Self::EndpointOverride(message)
            | Self::Build(message)
            | Self::Network(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for CustomHttpError {}

impl From<crate::provider::ProviderBindingError> for CustomHttpError {
    fn from(error: crate::provider::ProviderBindingError) -> Self {
        Self::InvalidUrl(error.to_string())
    }
}

impl From<InferenceHttpError> for CustomHttpError {
    fn from(error: InferenceHttpError) -> Self {
        match error {
            InferenceHttpError::InvalidUrl(message) => Self::InvalidUrl(message),
            InferenceHttpError::EndpointOverride(message) => Self::EndpointOverride(message),
            InferenceHttpError::Build(message) => Self::Build(message),
            InferenceHttpError::Network(message) => Self::Network(message),
            InferenceHttpError::Oversize { limit } => {
                Self::Network(format!("response exceeded the {limit}-byte limit"))
            }
        }
    }
}

#[cfg(test)]
mod tests;

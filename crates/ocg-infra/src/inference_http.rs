//! Catalog-stripped inference HTTP transport.
//!
//! Owns reusable client construction, default-leg proxy routing, connect
//! timeout, redirect policy, endpoint join, isolated auth headers, per-request
//! timeout/body, and bounded response reading. Product catalogs, process
//! config, Custom URL trust, and provider auth enums stay in the owning adapter.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderName, HeaderValue};

use crate::http::{
    OutboundProxySpec, ProxyMode, RouteLabel, attach_dns_resolver, configured_builder,
    configured_builder_for_label, no_redirect_policy,
};

/// Redirect policy for an inference HTTP client. Follow versus none is chosen
/// by the owning adapter; this crate does not attach a product-specific policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InferenceRedirectPolicy {
    Follow,
    None,
}

impl InferenceRedirectPolicy {
    pub fn reqwest_policy(self) -> reqwest::redirect::Policy {
        match self {
            Self::Follow => reqwest::redirect::Policy::default(),
            Self::None => no_redirect_policy(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InferenceHttpError {
    InvalidUrl(String),
    EndpointOverride(String),
    Build(String),
    Network(String),
    Oversize { limit: usize },
}

impl fmt::Display for InferenceHttpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidUrl(message)
            | Self::EndpointOverride(message)
            | Self::Build(message)
            | Self::Network(message) => f.write_str(message),
            Self::Oversize { limit } => {
                write!(f, "response exceeded the {limit}-byte limit")
            }
        }
    }
}

impl std::error::Error for InferenceHttpError {}

/// Infra-local auth scheme for isolated upstream headers. Not a catalog enum
/// and not serialized.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InferenceAuthScheme {
    Bearer,
    XApiKey,
    ApiKey,
}

/// Join `path` onto an already-canonical http(s) base while keeping the origin
/// and path prefix. Absolute URLs, protocol-relative targets, decoded
/// dot-segments, encoded slash/backslash, and nested percent-encoding are
/// rejected as endpoint override. Does not apply Custom URL trust policy.
pub fn join_inference_endpoint(
    base_url: &str,
    path: &str,
) -> Result<reqwest::Url, InferenceHttpError> {
    let canonical = base_url.trim().trim_end_matches('/');
    let base = reqwest::Url::parse(canonical)
        .map_err(|error| InferenceHttpError::InvalidUrl(error.to_string()))?;
    if !matches!(base.scheme(), "http" | "https") {
        return Err(InferenceHttpError::InvalidUrl(
            "base URL must use http or https".to_string(),
        ));
    }
    let relative = path.trim();
    if relative.is_empty() {
        return Ok(base);
    }
    if is_endpoint_override(relative) {
        return Err(InferenceHttpError::EndpointOverride(relative.to_string()));
    }
    let stripped = relative.trim_start_matches('/');
    let joined = format!("{canonical}/{stripped}");
    let parsed = reqwest::Url::parse(&joined)
        .map_err(|error| InferenceHttpError::InvalidUrl(error.to_string()))?;
    if parsed.scheme() != base.scheme()
        || parsed.host() != base.host()
        || parsed.port_or_known_default() != base.port_or_known_default()
    {
        return Err(InferenceHttpError::EndpointOverride(relative.to_string()));
    }
    if parsed.query().is_some() || parsed.fragment().is_some() {
        return Err(InferenceHttpError::EndpointOverride(
            "joined endpoint must not include a query or fragment".to_string(),
        ));
    }
    if !path_has_prefix(parsed.path(), base.path()) {
        return Err(InferenceHttpError::EndpointOverride(
            "joined path escaped the Custom base prefix".to_string(),
        ));
    }
    if path_has_unsafe_segments(parsed.path()) {
        return Err(InferenceHttpError::EndpointOverride(
            "joined path must not contain unsafe or recursively encoded segments".to_string(),
        ));
    }
    Ok(parsed)
}

/// Build isolated upstream auth headers. Callers supply the configured scheme
/// and key; this never copies dashboard or client credentials.
pub fn isolated_inference_headers(
    scheme: InferenceAuthScheme,
    api_key: &str,
) -> Result<HeaderMap, InferenceHttpError> {
    let mut headers = HeaderMap::new();
    match scheme {
        InferenceAuthScheme::Bearer => {
            let value = HeaderValue::from_str(&format!("Bearer {api_key}"))
                .map_err(|error| InferenceHttpError::InvalidUrl(error.to_string()))?;
            headers.insert(AUTHORIZATION, value);
        }
        InferenceAuthScheme::XApiKey => {
            let value = HeaderValue::from_str(api_key)
                .map_err(|error| InferenceHttpError::InvalidUrl(error.to_string()))?;
            headers.insert(HeaderName::from_static("x-api-key"), value);
        }
        InferenceAuthScheme::ApiKey => {
            let value = HeaderValue::from_str(api_key)
                .map_err(|error| InferenceHttpError::InvalidUrl(error.to_string()))?;
            headers.insert(HeaderName::from_static("api-key"), value);
        }
    }
    Ok(headers)
}

pub fn apply_inference_request_timeout(
    builder: reqwest::RequestBuilder,
    request_timeout: Option<Duration>,
) -> reqwest::RequestBuilder {
    match request_timeout {
        Some(request_timeout) => builder.timeout(request_timeout),
        None => builder,
    }
}

/// Construction spec for a provider-neutral inference HTTP client.
/// Redirect policy is chosen by the owning adapter; proxy routing and connect
/// timeout come from [`OutboundProxySpec`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HttpInferenceTransportSpec {
    redirect: InferenceRedirectPolicy,
}

impl HttpInferenceTransportSpec {
    pub const fn new(redirect: InferenceRedirectPolicy) -> Self {
        Self { redirect }
    }

    pub const fn follow_redirects() -> Self {
        Self::new(InferenceRedirectPolicy::Follow)
    }

    pub const fn no_redirects() -> Self {
        Self::new(InferenceRedirectPolicy::None)
    }

    pub const fn redirect(self) -> InferenceRedirectPolicy {
        self.redirect
    }
}

/// One outbound inference attempt. Auth is optional so keyless adapters can
/// reuse the same send path; callers that need isolated Bearer / `x-api-key` / `api-key`
/// supply the scheme and key here.
#[derive(Debug)]
pub struct InferenceHttpRequest<'a> {
    method: reqwest::Method,
    url: reqwest::Url,
    auth: Option<(InferenceAuthScheme, &'a str)>,
    extra_headers: HeaderMap,
    body: Option<Vec<u8>>,
    request_timeout: Option<Duration>,
}

impl<'a> InferenceHttpRequest<'a> {
    pub fn new(
        method: reqwest::Method,
        url: reqwest::Url,
        auth: Option<(InferenceAuthScheme, &'a str)>,
        extra_headers: HeaderMap,
        body: Option<Vec<u8>>,
        request_timeout: Option<Duration>,
    ) -> Self {
        Self {
            method,
            url,
            auth,
            extra_headers,
            body,
            request_timeout,
        }
    }
}

/// Neutral inference HTTP wrapper. Owns reusable client construction,
/// default-leg proxy routing, connect timeout, redirect policy, endpoint join,
/// isolated auth headers, per-request timeout/body, and bounded response
/// reading. Build uses [`configured_builder`] (List default leg), not a
/// per-model exception client.
#[derive(Clone)]
pub struct HttpInferenceTransport {
    client: reqwest::Client,
    proxy_mode: ProxyMode,
    spec: HttpInferenceTransportSpec,
}

impl fmt::Debug for HttpInferenceTransport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HttpInferenceTransport")
            .field("proxy_mode", &self.proxy_mode)
            .field("redirect", &self.spec.redirect())
            .finish_non_exhaustive()
    }
}

impl HttpInferenceTransport {
    pub fn build(
        proxy: &OutboundProxySpec,
        spec: HttpInferenceTransportSpec,
    ) -> Result<Self, InferenceHttpError> {
        Self::build_with_dns_resolver(proxy, spec, None)
    }

    /// Same client as [`Self::build`], with an optional connector DNS resolver.
    /// IsolatedTrustedAdmin Custom/dynamic callers pass a destination guard.
    /// Sealed-adapter callers keep `None`. Proxy routing is unchanged: a
    /// configured or system proxy still owns destination DNS.
    pub fn build_with_dns_resolver(
        proxy: &OutboundProxySpec,
        spec: HttpInferenceTransportSpec,
        dns_resolver: Option<Arc<dyn reqwest::dns::Resolve>>,
    ) -> Result<Self, InferenceHttpError> {
        let mut builder = configured_builder(proxy)
            .map_err(|error| InferenceHttpError::Build(error.to_string()))?
            .redirect(spec.redirect().reqwest_policy())
            .connect_timeout(proxy.connect_timeout);
        if let Some(resolver) = dns_resolver {
            builder = attach_dns_resolver(builder, resolver);
        }
        let client = builder
            .build()
            .map_err(|error| InferenceHttpError::Build(error.to_string()))?;
        Ok(Self {
            client,
            proxy_mode: proxy.mode,
            spec,
        })
    }

    /// Build on an already-selected request-entry proxy/direct leg while
    /// retaining this transport's redirect and destination-DNS policy.
    pub fn build_for_route_with_dns_resolver(
        proxy: &OutboundProxySpec,
        route: RouteLabel,
        spec: HttpInferenceTransportSpec,
        dns_resolver: Option<Arc<dyn reqwest::dns::Resolve>>,
    ) -> Result<Self, InferenceHttpError> {
        let mut builder = configured_builder_for_label(proxy, route)
            .map_err(|error| InferenceHttpError::Build(error.to_string()))?
            .redirect(spec.redirect().reqwest_policy())
            .connect_timeout(proxy.connect_timeout);
        if let Some(resolver) = dns_resolver {
            builder = attach_dns_resolver(builder, resolver);
        }
        let client = builder
            .build()
            .map_err(|error| InferenceHttpError::Build(error.to_string()))?;
        Ok(Self {
            client,
            proxy_mode: proxy.mode,
            spec,
        })
    }

    pub fn spec(&self) -> HttpInferenceTransportSpec {
        self.spec
    }

    pub fn proxy_mode(&self) -> ProxyMode {
        self.proxy_mode
    }

    pub fn redirect_policy(&self) -> InferenceRedirectPolicy {
        self.spec.redirect()
    }

    pub fn join_endpoint(base_url: &str, path: &str) -> Result<reqwest::Url, InferenceHttpError> {
        join_inference_endpoint(base_url, path)
    }

    pub fn isolated_headers(
        scheme: InferenceAuthScheme,
        api_key: &str,
    ) -> Result<HeaderMap, InferenceHttpError> {
        isolated_inference_headers(scheme, api_key)
    }

    pub fn request(&self, method: reqwest::Method, url: reqwest::Url) -> reqwest::RequestBuilder {
        self.client.request(method, url)
    }

    pub async fn send(
        &self,
        request: InferenceHttpRequest<'_>,
    ) -> Result<reqwest::Response, InferenceHttpError> {
        if request.auth.is_some() && self.spec.redirect() == InferenceRedirectPolicy::Follow {
            return Err(InferenceHttpError::InvalidUrl(
                "secret-bearing inference requests must not follow redirects".to_string(),
            ));
        }
        let mut builder = self.client.request(request.method, request.url);
        if let Some((scheme, api_key)) = request.auth {
            let headers = isolated_inference_headers(scheme, api_key)?;
            for (name, value) in &headers {
                builder = builder.header(name, value);
            }
        }
        for (name, value) in &request.extra_headers {
            builder = builder.header(name, value);
        }
        if let Some(body) = request.body {
            builder = builder.body(body);
        }
        let builder = apply_inference_request_timeout(builder, request.request_timeout);
        builder.send().await.map_err(map_inference_send_error)
    }

    pub async fn read_body_limited(
        response: reqwest::Response,
        max_bytes: usize,
    ) -> Result<Vec<u8>, InferenceHttpError> {
        if let Some(length) = response.content_length()
            && length > max_bytes as u64
        {
            return Err(InferenceHttpError::Oversize { limit: max_bytes });
        }
        let mut body = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(map_inference_send_error)?;
            if body.len().saturating_add(chunk.len()) > max_bytes {
                return Err(InferenceHttpError::Oversize { limit: max_bytes });
            }
            body.extend_from_slice(&chunk);
        }
        Ok(body)
    }
}

fn map_inference_send_error(error: reqwest::Error) -> InferenceHttpError {
    if error.is_timeout() {
        InferenceHttpError::Network(format!("upstream request timed out: {error}"))
    } else {
        InferenceHttpError::Network(error.to_string())
    }
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
        reqwest::Url::parse(trimmed)
            .ok()
            .map(|url| url.scheme().to_string()),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::ProxyListDirection;

    const PROXY_URL: &str = "http://127.0.0.1:7890";
    const INVALID_PROXY: &str = "not a url";

    fn proxy_spec(
        mode: ProxyMode,
        direction: ProxyListDirection,
        proxy_url: &str,
    ) -> OutboundProxySpec {
        OutboundProxySpec {
            mode,
            proxy_url: proxy_url.to_string(),
            connect_timeout: Duration::from_secs(5),
            list_direction: direction,
        }
    }

    #[test]
    fn join_preserves_prefix_and_rejects_override_query_fragment_and_unsafe_segments() {
        let joined = join_inference_endpoint("https://api.example.com/v1", "chat/completions")
            .expect("prefix join");
        assert_eq!(
            joined.as_str(),
            "https://api.example.com/v1/chat/completions"
        );
        assert_eq!(
            join_inference_endpoint("https://api.example.com/v1", "/chat/completions")
                .unwrap()
                .as_str(),
            "https://api.example.com/v1/chat/completions"
        );
        assert_eq!(
            join_inference_endpoint("https://api.example.com", "v1/models")
                .unwrap()
                .as_str(),
            "https://api.example.com/v1/models"
        );
        assert_eq!(
            join_inference_endpoint("http://127.0.0.1:9/v1", "messages")
                .unwrap()
                .as_str(),
            "http://127.0.0.1:9/v1/messages"
        );
        assert_eq!(
            join_inference_endpoint("http://10.0.0.8/prefix", "responses")
                .unwrap()
                .as_str(),
            "http://10.0.0.8/prefix/responses"
        );
        assert_eq!(
            HttpInferenceTransport::join_endpoint("https://api.example.com/v1", "hello%20world")
                .unwrap()
                .as_str(),
            "https://api.example.com/v1/hello%20world"
        );
        assert_eq!(
            join_inference_endpoint("https://api.example.com/v1", "")
                .unwrap()
                .as_str(),
            "https://api.example.com/v1"
        );

        assert!(
            join_inference_endpoint("https://api.example.com/v1", "https://evil.example/x")
                .is_err()
        );
        assert!(join_inference_endpoint("https://api.example.com/v1", "//evil.example/x").is_err());
        assert!(join_inference_endpoint("https://api.example.com/v1", "../admin").is_err());
        assert!(join_inference_endpoint("https://api.example.com/v1", "foo/../admin").is_err());
        assert!(join_inference_endpoint("https://api.example.com/v1", "foo/./bar").is_err());
        assert!(join_inference_endpoint("https://api.example.com/v1", "%2e%2e/admin").is_err());
        assert!(join_inference_endpoint("https://api.example.com/v1", "foo/%2e%2e/admin").is_err());
        assert!(join_inference_endpoint("https://api.example.com/v1", "foo%2fadmin").is_err());
        assert!(join_inference_endpoint("https://api.example.com/v1", "foo%2Fadmin").is_err());
        assert!(join_inference_endpoint("https://api.example.com/v1", "foo%5cadmin").is_err());
        assert!(join_inference_endpoint("https://api.example.com/v1", "foo%5Cadmin").is_err());
        assert!(join_inference_endpoint("https://api.example.com/v1", "%252e%252e/admin").is_err());
        assert!(
            join_inference_endpoint("https://api.example.com/v1", "foo/%252E%252E/admin").is_err()
        );
        assert!(
            join_inference_endpoint("https://api.example.com/v1", "%252f%252fevil.example/x")
                .is_err()
        );
        assert!(join_inference_endpoint("https://api.example.com/v1", "foo%255cadmin").is_err());
        assert!(
            join_inference_endpoint("https://api.example.com/v1", "%25252e%25252e/admin").is_err()
        );
        assert!(join_inference_endpoint("https://api.example.com/v1", "nested%2520space").is_err());
        assert!(join_inference_endpoint("https://api.example.com/v1", "chat?x=1").is_err());
        assert!(join_inference_endpoint("https://api.example.com/v1", "chat#frag").is_err());
        assert!(join_inference_endpoint("ftp://api.example.com/v1", "chat").is_err());
    }

    #[test]
    fn join_allows_userinfo_without_custom_trust_validation() {
        let with_userinfo =
            join_inference_endpoint("https://user:pass@api.example.com/v1", "chat/completions");
        assert!(
            with_userinfo.is_ok(),
            "neutral join must not apply Custom URL trust validation"
        );
        assert_eq!(
            with_userinfo.unwrap().as_str(),
            "https://user:pass@api.example.com/v1/chat/completions"
        );
    }

    #[test]
    fn isolated_headers_are_one_header_maps() {
        let bearer = isolated_inference_headers(InferenceAuthScheme::Bearer, "sk-test").unwrap();
        assert_eq!(bearer.get(AUTHORIZATION).unwrap(), "Bearer sk-test");
        assert!(bearer.get("x-api-key").is_none());
        assert!(bearer.get("cookie").is_none());
        assert_eq!(bearer.len(), 1);

        let x_api =
            HttpInferenceTransport::isolated_headers(InferenceAuthScheme::XApiKey, "sk-test")
                .unwrap();
        assert_eq!(x_api.get("x-api-key").unwrap(), "sk-test");
        assert!(x_api.get(AUTHORIZATION).is_none());
        assert_eq!(x_api.len(), 1);

        let api_key = isolated_inference_headers(InferenceAuthScheme::ApiKey, "sk-test").unwrap();
        assert_eq!(api_key.get("api-key").unwrap(), "sk-test");
        assert!(api_key.get(AUTHORIZATION).is_none());
        assert!(api_key.get("x-api-key").is_none());
        assert_eq!(api_key.len(), 1);
        assert!(isolated_inference_headers(InferenceAuthScheme::ApiKey, "bad\nkey").is_err());
    }

    #[test]
    fn transport_builds_for_direct_manual_auto_list_and_rejects_invalid_proxy() {
        let direct = HttpInferenceTransport::build(
            &proxy_spec(ProxyMode::Direct, ProxyListDirection::Whitelist, ""),
            HttpInferenceTransportSpec::no_redirects(),
        )
        .unwrap();
        assert_eq!(direct.proxy_mode(), ProxyMode::Direct);

        assert!(
            HttpInferenceTransport::build(
                &proxy_spec(ProxyMode::Manual, ProxyListDirection::Whitelist, PROXY_URL),
                HttpInferenceTransportSpec::no_redirects(),
            )
            .is_ok()
        );

        let auto = HttpInferenceTransport::build(
            &proxy_spec(ProxyMode::Auto, ProxyListDirection::Whitelist, ""),
            HttpInferenceTransportSpec::follow_redirects(),
        )
        .unwrap();
        assert_eq!(auto.proxy_mode(), ProxyMode::Auto);

        let list = HttpInferenceTransport::build(
            &proxy_spec(ProxyMode::List, ProxyListDirection::Whitelist, PROXY_URL),
            HttpInferenceTransportSpec::no_redirects(),
        )
        .unwrap();
        assert_eq!(list.proxy_mode(), ProxyMode::List);

        assert!(
            HttpInferenceTransport::build(
                &proxy_spec(
                    ProxyMode::Manual,
                    ProxyListDirection::Whitelist,
                    INVALID_PROXY,
                ),
                HttpInferenceTransportSpec::no_redirects(),
            )
            .is_err()
        );
        assert!(
            HttpInferenceTransport::build(
                &proxy_spec(
                    ProxyMode::List,
                    ProxyListDirection::Blacklist,
                    INVALID_PROXY,
                ),
                HttpInferenceTransportSpec::no_redirects(),
            )
            .is_err(),
            "blacklist default leg still needs a valid proxy URL"
        );
        assert!(
            HttpInferenceTransport::build(
                &proxy_spec(
                    ProxyMode::List,
                    ProxyListDirection::Whitelist,
                    INVALID_PROXY,
                ),
                HttpInferenceTransportSpec::no_redirects(),
            )
            .is_ok(),
            "whitelist default leg is direct and does not parse the proxy URL"
        );
        assert!(
            HttpInferenceTransport::build(
                &proxy_spec(
                    ProxyMode::Direct,
                    ProxyListDirection::Whitelist,
                    INVALID_PROXY
                ),
                HttpInferenceTransportSpec::no_redirects(),
            )
            .is_ok()
        );
        assert!(
            HttpInferenceTransport::build(
                &proxy_spec(
                    ProxyMode::Auto,
                    ProxyListDirection::Blacklist,
                    INVALID_PROXY
                ),
                HttpInferenceTransportSpec::no_redirects(),
            )
            .is_ok()
        );
    }

    #[test]
    fn redirect_none_and_follow_are_owned_by_the_spec() {
        let none = HttpInferenceTransportSpec::no_redirects();
        let follow = HttpInferenceTransportSpec::follow_redirects();
        assert_eq!(none.redirect(), InferenceRedirectPolicy::None);
        assert_eq!(follow.redirect(), InferenceRedirectPolicy::Follow);
        assert_ne!(none, follow);
        let _ = InferenceRedirectPolicy::None.reqwest_policy();
        let _ = InferenceRedirectPolicy::Follow.reqwest_policy();

        let blocked = HttpInferenceTransport::build(
            &proxy_spec(ProxyMode::Direct, ProxyListDirection::Whitelist, ""),
            none,
        )
        .unwrap();
        assert_eq!(blocked.redirect_policy(), InferenceRedirectPolicy::None);
        assert_eq!(blocked.spec(), none);

        let followed = HttpInferenceTransport::build(
            &proxy_spec(ProxyMode::Direct, ProxyListDirection::Whitelist, ""),
            follow,
        )
        .unwrap();
        assert_eq!(followed.redirect_policy(), InferenceRedirectPolicy::Follow);
        assert_eq!(followed.spec(), follow);
    }
}

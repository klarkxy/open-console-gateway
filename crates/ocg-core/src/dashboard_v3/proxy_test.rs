//! POST `/settings/test-proxy` — operational outbound proxy diagnostic.
//!
//! Reuses [`crate::http_client::configured_builder`] (list mode = direction
//! default leg), the fixed diagnostic target, connect/request timeouts, and
//! no-redirect policy. The request may overlay mode/URL/direction; it cannot
//! choose an arbitrary target. Captured revision/generation are returned and
//! never bumped.

use axum::Json;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::StatusCode;
#[cfg(debug_assertions)]
use parking_lot::Mutex;
use serde_json::Value;
use std::time::{Duration, Instant};

use crate::models::{
    AppConfig, ProxyListDirection as AppProxyListDirection, ProxyMode as AppProxyMode,
    normalize_proxy_url,
};
use crate::redaction::{redact_known_secret, redact_text};
use crate::state::CoreState;

use super::V3ApiError;
use super::types::{ProxyListDirection, ProxyMode, ProxyTestRequest, ProxyTestResponse, V3Error};

/// Safe public HTTPS origin used by the production diagnostic GET.
pub const PROXY_TEST_TARGET: &str = "https://opencode.ai/zen/go";
const PROXY_TEST_TIMEOUT_SECS: u64 = 30;

#[cfg(debug_assertions)]
static PROXY_TEST_TARGET_OVERRIDES: Mutex<std::collections::BTreeMap<u64, String>> =
    Mutex::new(std::collections::BTreeMap::new());

/// Test-only guard that restores the production diagnostic target when dropped.
#[cfg(debug_assertions)]
pub struct ProxyTestTargetGuard {
    process_generation: u64,
}

#[cfg(debug_assertions)]
impl Drop for ProxyTestTargetGuard {
    fn drop(&mut self) {
        PROXY_TEST_TARGET_OVERRIDES
            .lock()
            .remove(&self.process_generation);
    }
}

/// Bind a loopback diagnostic URL to one `CoreState` process generation.
///
/// Compiled out of release production. Non-loopback, credentialed, query, or
/// fragment URLs are rejected and do not install an override.
#[cfg(debug_assertions)]
#[must_use]
pub fn install_proxy_test_target_for_tests(
    process_generation: u64,
    url: impl Into<String>,
) -> ProxyTestTargetGuard {
    let mut overrides = PROXY_TEST_TARGET_OVERRIDES.lock();
    match parse_loopback_http_url(&url.into()) {
        Some(canonical) => {
            overrides.insert(process_generation, canonical);
        }
        None => {
            overrides.remove(&process_generation);
        }
    }
    ProxyTestTargetGuard { process_generation }
}

#[cfg(debug_assertions)]
fn debug_proxy_test_target(process_generation: u64) -> Option<String> {
    PROXY_TEST_TARGET_OVERRIDES
        .lock()
        .get(&process_generation)
        .cloned()
}

/// Accept only an unambiguous loopback HTTP(S) origin: parsed host must be
/// exactly `127.0.0.1`, `localhost`, or `::1`, with no userinfo, query, or
/// fragment.
#[cfg(debug_assertions)]
fn parse_loopback_http_url(url: &str) -> Option<String> {
    let parsed = reqwest::Url::parse(url.trim()).ok()?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return None;
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return None;
    }
    if parsed.query().is_some() || parsed.fragment().is_some() {
        return None;
    }
    if !host_is_exact_loopback(&parsed) {
        return None;
    }
    Some(parsed.as_str().to_string())
}

#[cfg(debug_assertions)]
fn host_is_exact_loopback(parsed: &reqwest::Url) -> bool {
    use std::net::{Ipv4Addr, Ipv6Addr};

    let Some(host) = parsed.host() else {
        return false;
    };
    let rendered = host.to_string();
    if let Some(inside) = rendered
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
    {
        return inside
            .parse::<Ipv6Addr>()
            .is_ok_and(|ip| ip == Ipv6Addr::LOCALHOST);
    }
    if let Ok(ip) = rendered.parse::<Ipv4Addr>() {
        return ip == Ipv4Addr::LOCALHOST;
    }
    rendered.eq_ignore_ascii_case("localhost")
}

pub(super) async fn test_proxy(
    State(state): State<CoreState>,
    body: Bytes,
) -> Result<Json<ProxyTestResponse>, V3ApiError> {
    let request = parse_proxy_test_request(&body, &state)?;
    let snapshot = {
        let _settings_update = state.settings_update.lock();
        CapturedProxyTest {
            config: state.config(),
            revision: state.settings_revision(),
            process_generation: state.process_generation(),
        }
    };

    let revision = snapshot.revision;
    let process_generation = snapshot.process_generation;
    let mut config = snapshot.config;
    config.proxy_mode = app_proxy_mode(request.proxy_mode);
    if let Some(direction) = request.proxy_list_direction {
        config.proxy_list_direction = app_proxy_list_direction(direction);
    }
    config.proxy_url =
        normalize_proxy_url(config.proxy_mode, &request.proxy_url).map_err(|message| {
            V3ApiError {
                status: StatusCode::BAD_REQUEST,
                body: V3Error::invalid_request_at(message, revision, process_generation),
            }
        })?;

    let connect_timeout_secs = config.connect_timeout_secs;
    let request_timeout_secs = connect_timeout_secs.min(PROXY_TEST_TIMEOUT_SECS);
    let secrets = [config.gateway_key.as_str(), config.proxy_url.as_str()];
    let target = diagnostic_target(process_generation);

    let client = crate::http_client::configured_builder(&config)
        .and_then(|builder| {
            builder
                .connect_timeout(Duration::from_secs(connect_timeout_secs))
                .redirect(crate::http_client::no_redirect_policy())
                .build()
                .map_err(Into::into)
        })
        .map_err(|error| {
            V3ApiError::internal(sanitize_proxy_test_detail(&error.to_string(), &secrets))
        })?;

    let started = Instant::now();
    let response = client
        .get(&target)
        .timeout(Duration::from_secs(request_timeout_secs))
        .send()
        .await
        .map_err(|error| {
            V3ApiError::outbound_failed_at(
                revision,
                process_generation,
                proxy_test_error_message(&error, &secrets),
            )
        })?;
    let latency_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    let status = response.status().as_u16();
    drop(response);

    Ok(Json(ProxyTestResponse {
        proxy_mode: request.proxy_mode,
        status,
        latency_ms,
        revision,
        process_generation,
    }))
}

struct CapturedProxyTest {
    config: AppConfig,
    revision: u64,
    process_generation: u64,
}

fn parse_proxy_test_request(
    bytes: &[u8],
    state: &CoreState,
) -> Result<ProxyTestRequest, V3ApiError> {
    let value: Value = serde_json::from_slice(bytes).map_err(|_| V3ApiError::invalid_json())?;
    let Some(object) = value.as_object() else {
        return Err(V3ApiError::invalid_json());
    };
    match object.get("proxyMode") {
        Some(Value::String(mode))
            if matches!(mode.as_str(), "auto" | "manual" | "direct" | "list") => {}
        Some(Value::String(_)) => {
            return Err(V3ApiError::invalid_request_at(
                state,
                "proxyMode must be auto, manual, direct, or list",
            ));
        }
        _ => {}
    }
    match object.get("proxyListDirection") {
        Some(Value::String(direction))
            if matches!(direction.as_str(), "whitelist" | "blacklist") => {}
        Some(Value::String(_)) => {
            return Err(V3ApiError::invalid_request_at(
                state,
                "proxyListDirection must be whitelist or blacklist",
            ));
        }
        _ => {}
    }
    serde_json::from_value(value).map_err(|_| V3ApiError::invalid_json())
}

fn diagnostic_target(process_generation: u64) -> String {
    #[cfg(debug_assertions)]
    if let Some(url) = debug_proxy_test_target(process_generation) {
        return url;
    }
    #[cfg(not(debug_assertions))]
    let _ = process_generation;
    PROXY_TEST_TARGET.to_string()
}

fn proxy_test_error_message(error: &reqwest::Error, secrets: &[&str]) -> String {
    let category = if error.is_timeout() {
        "request timed out"
    } else if error.is_connect() {
        "connection failed"
    } else {
        "request failed"
    };
    format!(
        "outbound connection test {category}: {}",
        sanitize_proxy_test_detail(&format_error_chain(error), secrets)
    )
}

fn format_error_chain(error: &(dyn std::error::Error + 'static)) -> String {
    let mut message = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        message.push_str(": ");
        message.push_str(&cause.to_string());
        source = cause.source();
    }
    message
}

fn sanitize_proxy_test_detail(text: &str, secrets: &[&str]) -> String {
    let mut redacted = strip_url_userinfo(text);
    redacted = redact_text(&redacted);
    for secret in secrets {
        if !secret.is_empty() {
            redacted = redact_known_secret(&redacted, secret);
        }
    }
    redacted
}

fn strip_url_userinfo(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(scheme) = rest.find("://") {
        output.push_str(&rest[..scheme + 3]);
        let after = &rest[scheme + 3..];
        if let Some(at) = after.find('@') {
            let userinfo = &after[..at];
            if !userinfo.is_empty() && !userinfo.contains('/') && !userinfo.contains(' ') {
                output.push_str("<redacted>");
                rest = &after[at..];
                continue;
            }
        }
        rest = after;
    }
    output.push_str(rest);
    output
}

fn app_proxy_mode(mode: ProxyMode) -> AppProxyMode {
    match mode {
        ProxyMode::Auto => AppProxyMode::Auto,
        ProxyMode::Manual => AppProxyMode::Manual,
        ProxyMode::Direct => AppProxyMode::Direct,
        ProxyMode::List => AppProxyMode::List,
    }
}

fn app_proxy_list_direction(direction: ProxyListDirection) -> AppProxyListDirection {
    match direction {
        ProxyListDirection::Whitelist => AppProxyListDirection::Whitelist,
        ProxyListDirection::Blacklist => AppProxyListDirection::Blacklist,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn production_source_gates_the_proxy_test_seam_and_keeps_the_fixed_target() {
        let production = include_str!("proxy_test.rs")
            .split("#[cfg(test)]")
            .next()
            .expect("production source precedes tests");
        assert!(
            production.contains("configured_builder(&config)"),
            "production diagnostic must reuse the global proxy builder"
        );
        assert!(production.contains("PROXY_TEST_TARGET"));
        assert!(production.contains("https://opencode.ai/zen/go"));
        assert!(production.contains("no_redirect_policy"));
        assert!(production.contains("#[cfg(debug_assertions)]"));
        assert!(production.contains("#[cfg(not(debug_assertions))]"));
        for needle in [
            "ProxyTestTargetGuard",
            "install_proxy_test_target_for_tests",
            "PROXY_TEST_TARGET_OVERRIDES",
            "debug_proxy_test_target",
            "parse_loopback_http_url",
        ] {
            let idx = production
                .find(needle)
                .unwrap_or_else(|| panic!("{needle} must exist behind debug_assertions"));
            let before = &production[..idx];
            let cfg_idx = before
                .rfind("#[cfg(debug_assertions)]")
                .unwrap_or_else(|| panic!("{needle} must be gated by debug_assertions"));
            assert!(
                !before[cfg_idx..].contains("#[cfg(not(debug_assertions))]"),
                "{needle} compiled into release"
            );
        }
        let release_idx = production
            .find("#[cfg(not(debug_assertions))]")
            .expect("release diagnostic path");
        let release = &production[release_idx..];
        assert!(release.contains("PROXY_TEST_TARGET"));
        assert!(!release.contains("PROXY_TEST_TARGET_OVERRIDES"));
        assert!(!release.contains("install_proxy_test_target_for_tests"));
        assert!(!release.contains("debug_proxy_test_target"));
    }

    #[test]
    fn strip_url_userinfo_redacts_credentials_and_leaves_hosts() {
        assert_eq!(
            strip_url_userinfo("error sending request for url (http://user:pass@127.0.0.1:9/)"),
            "error sending request for url (http://<redacted>@127.0.0.1:9/)"
        );
        assert_eq!(
            strip_url_userinfo("direct http://127.0.0.1:9/ path"),
            "direct http://127.0.0.1:9/ path"
        );
    }

    #[test]
    fn sanitize_proxy_test_detail_redacts_known_secrets() {
        let detail = sanitize_proxy_test_detail(
            "outbound connection test request failed: sk-secret at http://user:pass@proxy.example/",
            &["sk-secret"],
        );
        assert!(!detail.contains("sk-secret"));
        assert!(!detail.contains("user:pass"));
        assert!(detail.contains("<redacted>"));
    }
}

#[cfg(all(test, debug_assertions))]
mod target_override_tests {
    use super::{
        debug_proxy_test_target, install_proxy_test_target_for_tests, parse_loopback_http_url,
    };

    fn unique_generation() -> u64 {
        uuid::Uuid::new_v4().as_u128() as u64
    }

    #[test]
    fn parse_loopback_http_url_requires_exact_host_without_userinfo_query_or_fragment() {
        assert_eq!(
            parse_loopback_http_url("http://127.0.0.1:9/").as_deref(),
            Some("http://127.0.0.1:9/")
        );
        assert_eq!(
            parse_loopback_http_url("http://localhost:9/").as_deref(),
            Some("http://localhost:9/")
        );
        assert_eq!(
            parse_loopback_http_url("http://[::1]:9/").as_deref(),
            Some("http://[::1]:9/")
        );
        assert!(parse_loopback_http_url("http://127.0.0.1:9/?x=1").is_none());
        assert!(parse_loopback_http_url("http://127.0.0.1:9/#frag").is_none());
        assert!(parse_loopback_http_url("http://user@127.0.0.1:9/").is_none());
        assert!(parse_loopback_http_url("http://:pass@127.0.0.1:9/").is_none());
        assert!(parse_loopback_http_url("https://opencode.ai/zen/go").is_none());
        assert!(parse_loopback_http_url("http://127.0.0.2:9/").is_none());
        assert!(parse_loopback_http_url("http://127.0.0.1.example.com:9/").is_none());
    }

    #[test]
    fn overrides_are_isolated_by_process_generation_and_reject_ambiguous_urls() {
        let first = unique_generation();
        let second = unique_generation();
        let _guard_a = install_proxy_test_target_for_tests(first, "http://127.0.0.1:11/");
        let _guard_b = install_proxy_test_target_for_tests(second, "http://127.0.0.1:12/");
        assert_eq!(
            debug_proxy_test_target(first).as_deref(),
            Some("http://127.0.0.1:11/")
        );
        assert_eq!(
            debug_proxy_test_target(second).as_deref(),
            Some("http://127.0.0.1:12/")
        );

        drop(_guard_a);
        let _cleared =
            install_proxy_test_target_for_tests(first, "http://127.0.0.1:11@example.com/");
        assert!(debug_proxy_test_target(first).is_none());
        assert_eq!(
            debug_proxy_test_target(second).as_deref(),
            Some("http://127.0.0.1:12/")
        );
    }
}

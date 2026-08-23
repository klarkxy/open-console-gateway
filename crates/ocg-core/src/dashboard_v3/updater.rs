//! Session-protected desktop update check, status, and signed install.
//!
//! `GET /settings/check-update` and `GET /settings/update-status` are
//! operational reads: they capture the live CAS pair and never bump it.
//! Check-update snapshots config and identity under `settings_update`, drops
//! every lock, then GETs the fixed GitHub latest-release API through the
//! configured default proxy (list mode = direction default leg). Status is a
//! local snapshot of the process update machine.
//!
//! `POST /settings/install-update` is an in-memory control-plane mutation:
//! it requires `expectedRevision` / `processGeneration` under
//! `settings_update`, starts the host installer atomically, returns
//! `DesktopUpdate`, and does not bump identity tokens. It never holds a
//! network or DB lock.
//!
//! Production always uses the public GitHub latest API. Debug tests may bind
//! a processGeneration-keyed loopback seam; that installer, map, and URL
//! parser are absent from release. `releaseUrl` on the wire is always the
//! public GitHub latest-release page, never the outbound API or a seam.

use axum::Json;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::StatusCode;
use serde::Deserialize;
use std::time::Duration;

use crate::desktop::DesktopUpdateStartError;
use crate::models::AppConfig;
use crate::redaction::{redact_known_secret, redact_text};
use crate::state::CoreState;

use super::types::{DesktopUpdate, DesktopUpdatePhase, InstallUpdate, UpdateCheck};
use super::{V3ApiError, check_expectation, parse_mutation_json};

/// Outbound GitHub latest-release API. Never copied onto `UpdateCheck.releaseUrl`.
pub const GITHUB_LATEST_RELEASE_API: &str =
    "https://api.github.com/repos/klarkxy/opencode-go-mgr/releases/latest";
/// Public latest-release page returned to clients.
pub const GITHUB_LATEST_RELEASE_URL: &str =
    "https://github.com/klarkxy/opencode-go-mgr/releases/latest";
const UPDATE_CHECK_TIMEOUT: Duration = Duration::from_secs(30);

#[cfg(debug_assertions)]
static UPDATE_CHECK_URL_OVERRIDES: parking_lot::Mutex<std::collections::BTreeMap<u64, String>> =
    parking_lot::Mutex::new(std::collections::BTreeMap::new());

/// Test-only guard that restores the production GitHub latest API when dropped.
#[cfg(debug_assertions)]
pub struct UpdateCheckUrlGuard {
    process_generation: u64,
}

#[cfg(debug_assertions)]
impl Drop for UpdateCheckUrlGuard {
    fn drop(&mut self) {
        UPDATE_CHECK_URL_OVERRIDES
            .lock()
            .remove(&self.process_generation);
    }
}

/// Bind a loopback GitHub-latest stand-in to one `CoreState` process generation.
///
/// Compiled out of release production. Non-loopback, credentialed, query, or
/// fragment URLs are rejected and do not install an override.
#[cfg(debug_assertions)]
#[must_use]
pub fn install_update_check_url_for_tests(
    process_generation: u64,
    url: impl Into<String>,
) -> UpdateCheckUrlGuard {
    let mut overrides = UPDATE_CHECK_URL_OVERRIDES.lock();
    match parse_loopback_http_url(&url.into()) {
        Some(canonical) => {
            overrides.insert(process_generation, canonical);
        }
        None => {
            overrides.remove(&process_generation);
        }
    }
    UpdateCheckUrlGuard { process_generation }
}

#[cfg(debug_assertions)]
fn debug_update_check_url(process_generation: u64) -> Option<String> {
    UPDATE_CHECK_URL_OVERRIDES
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

pub(super) async fn check_update(
    State(state): State<CoreState>,
) -> Result<Json<UpdateCheck>, V3ApiError> {
    let snapshot = {
        let _settings_update = state.settings_update.lock();
        CapturedUpdateCheck {
            config: state.config(),
            revision: state.settings_revision(),
            process_generation: state.process_generation(),
            install_supported: state.desktop_update_supported(),
        }
    };

    let revision = snapshot.revision;
    let process_generation = snapshot.process_generation;
    let install_supported = snapshot.install_supported;
    let secrets = [
        snapshot.config.gateway_key.as_str(),
        snapshot.config.proxy_url.as_str(),
    ];
    let target = update_check_url(process_generation);
    let client = build_update_check_client(&snapshot.config).map_err(|error| {
        V3ApiError::internal(sanitize_update_detail(&error.to_string(), &secrets))
    })?;

    let response = client
        .get(&target)
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .header(
            reqwest::header::USER_AGENT,
            concat!("ocg-manager/", env!("CARGO_PKG_VERSION")),
        )
        .timeout(UPDATE_CHECK_TIMEOUT)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|error| {
            V3ApiError::outbound_failed_at(
                revision,
                process_generation,
                update_check_error_message(&error, &secrets),
            )
        })?;
    let release = response.json::<GithubRelease>().await.map_err(|error| {
        V3ApiError::outbound_failed_at(
            revision,
            process_generation,
            update_check_error_message(&error, &secrets),
        )
    })?;

    let current_version = env!("CARGO_PKG_VERSION");
    let (current_version_parts, current_version) = parse_semver_version(current_version)
        .ok_or_else(|| V3ApiError::internal("application version is not valid SemVer"))?;
    let (latest_version_parts, latest_version) = parse_semver_version(&release.tag_name)
        .ok_or_else(|| {
            V3ApiError::outbound_failed_at(
                revision,
                process_generation,
                "GitHub latest release has an invalid SemVer tag",
            )
        })?;

    Ok(Json(UpdateCheck {
        current_version: current_version.to_string(),
        latest_version: latest_version.to_string(),
        update_available: is_update_available(&current_version_parts, &latest_version_parts),
        release_url: GITHUB_LATEST_RELEASE_URL.to_string(),
        install_supported,
        revision,
        process_generation,
    }))
}

pub(super) async fn get_update_status(State(state): State<CoreState>) -> Json<DesktopUpdate> {
    let _settings_update = state.settings_update.lock();
    Json(desktop_update_from_state(&state))
}

pub(super) async fn install_update(
    State(state): State<CoreState>,
    body: Bytes,
) -> Result<(StatusCode, Json<DesktopUpdate>), V3ApiError> {
    let input = parse_mutation_json::<InstallUpdate>(&body)?;
    let _settings_update = state.settings_update.lock();
    check_expectation(&state, &input.expectation)?;

    let status = state.desktop_update_status();
    let (current_version_parts, _) = parse_semver_version(&status.current_version)
        .ok_or_else(|| V3ApiError::internal("application version is not valid SemVer"))?;
    let (expected_version_parts, expected_version) = parse_semver_version(&input.expected_version)
        .ok_or_else(|| {
            V3ApiError::invalid_request_at(&state, "expectedVersion must be a valid SemVer version")
        })?;
    if !is_update_available(&current_version_parts, &expected_version_parts) {
        return Err(V3ApiError::invalid_request_at(
            &state,
            "expectedVersion must be newer than the current version",
        ));
    }

    match state.start_desktop_update(expected_version.to_string()) {
        Ok(()) => Ok((
            StatusCode::ACCEPTED,
            Json(desktop_update_from_state(&state)),
        )),
        Err(DesktopUpdateStartError::Unsupported) => Err(V3ApiError::invalid_request_at(
            &state,
            "desktop update installation is unavailable in this runtime",
        )),
        Err(DesktopUpdateStartError::Busy) => Err(V3ApiError::conflict_at(
            &state,
            "a desktop update is already in progress",
        )),
        Err(DesktopUpdateStartError::Starter(error)) => {
            let secrets = {
                let config = state.config();
                [config.gateway_key, config.proxy_url]
            };
            Err(V3ApiError::internal(sanitize_update_detail(
                &error.to_string(),
                &[secrets[0].as_str(), secrets[1].as_str()],
            )))
        }
    }
}

struct CapturedUpdateCheck {
    config: AppConfig,
    revision: u64,
    process_generation: u64,
    install_supported: bool,
}

#[derive(Deserialize)]
struct GithubRelease {
    tag_name: String,
}

fn desktop_update_from_state(state: &CoreState) -> DesktopUpdate {
    let status = state.desktop_update_status();
    let config = state.config();
    let secrets = [config.gateway_key.as_str(), config.proxy_url.as_str()];
    DesktopUpdate {
        phase: wire_phase(status.phase),
        downloaded: status.downloaded,
        total: status.total,
        error: status
            .error
            .map(|error| sanitize_update_detail(&error, &secrets)),
        current_version: status.current_version,
        install_supported: status.install_supported,
        revision: state.settings_revision(),
        process_generation: state.process_generation(),
    }
}

fn wire_phase(phase: crate::desktop::DesktopUpdatePhase) -> DesktopUpdatePhase {
    match phase {
        crate::desktop::DesktopUpdatePhase::Idle => DesktopUpdatePhase::Idle,
        crate::desktop::DesktopUpdatePhase::Checking => DesktopUpdatePhase::Checking,
        crate::desktop::DesktopUpdatePhase::Downloading => DesktopUpdatePhase::Downloading,
        crate::desktop::DesktopUpdatePhase::Installing => DesktopUpdatePhase::Installing,
        crate::desktop::DesktopUpdatePhase::Failed => DesktopUpdatePhase::Failed,
    }
}

fn build_update_check_client(config: &AppConfig) -> crate::Result<reqwest::Client> {
    Ok(crate::http_client::configured_builder(config)?
        .connect_timeout(Duration::from_secs(config.connect_timeout_secs))
        .build()?)
}

fn update_check_url(process_generation: u64) -> String {
    #[cfg(debug_assertions)]
    if let Some(url) = debug_update_check_url(process_generation) {
        return url;
    }
    #[cfg(not(debug_assertions))]
    let _ = process_generation;
    GITHUB_LATEST_RELEASE_API.to_string()
}

fn update_check_error_message(error: &reqwest::Error, secrets: &[&str]) -> String {
    let category = if error.is_timeout() {
        format!(
            "request timed out after {} seconds",
            UPDATE_CHECK_TIMEOUT.as_secs()
        )
    } else if error.is_connect() {
        "connection failed".to_string()
    } else if let Some(status) = error.status() {
        format!("GitHub returned HTTP {status}")
    } else if error.is_decode() {
        "GitHub returned an invalid response".to_string()
    } else {
        "request failed".to_string()
    };
    format!(
        "failed to check GitHub releases ({category}): {}",
        sanitize_update_detail(&format_error_chain(error), secrets)
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

fn sanitize_update_detail(text: &str, secrets: &[&str]) -> String {
    let mut redacted = redact_http_urls(text);
    redacted = strip_url_userinfo(&redacted);
    redacted = redacted.replace(GITHUB_LATEST_RELEASE_API, "<redacted>");
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

fn redact_http_urls(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut rest = text;
    loop {
        let http = rest.find("http://");
        let https = rest.find("https://");
        let found = match (http, https) {
            (Some(http_idx), Some(https_idx)) if http_idx < https_idx => Some((http_idx, 7)),
            (Some(_), Some(https_idx)) => Some((https_idx, 8)),
            (Some(http_idx), None) => Some((http_idx, 7)),
            (None, Some(https_idx)) => Some((https_idx, 8)),
            (None, None) => None,
        };
        let Some((idx, scheme_len)) = found else {
            output.push_str(rest);
            break;
        };
        output.push_str(&rest[..idx]);
        output.push_str("<redacted>");
        let after_scheme = &rest[idx + scheme_len..];
        let end = after_scheme
            .find(|ch: char| ch.is_whitespace() || matches!(ch, ')' | '"' | '\'' | ',' | ';' | ']'))
            .unwrap_or(after_scheme.len());
        rest = &after_scheme[end..];
    }
    output
}

#[derive(Debug)]
struct SemverVersion<'a> {
    core: [u64; 3],
    prerelease: Option<Vec<PrereleaseIdentifier<'a>>>,
}

#[derive(Debug)]
struct PrereleaseIdentifier<'a> {
    value: &'a str,
    numeric: Option<u64>,
}

fn parse_semver_version(version: &str) -> Option<(SemverVersion<'_>, &str)> {
    let version = version.strip_prefix('v').unwrap_or(version);
    let display_version = version;
    let (version, build) = match version.split_once('+') {
        Some((version, build)) => (version, Some(build)),
        None => (version, None),
    };
    build
        .is_none_or(|build| build.split('.').all(is_semver_identifier))
        .then_some(())?;

    let (core, prerelease) = match version.split_once('-') {
        Some((core, prerelease)) => (core, Some(prerelease)),
        None => (version, None),
    };
    let mut core_parts = core.split('.');
    let core = [
        parse_semver_number(core_parts.next()?)?,
        parse_semver_number(core_parts.next()?)?,
        parse_semver_number(core_parts.next()?)?,
    ];
    core_parts.next().is_none().then_some(())?;

    let prerelease = match prerelease {
        Some(prerelease) => Some(
            prerelease
                .split('.')
                .map(parse_prerelease_identifier)
                .collect::<Option<Vec<_>>>()?,
        ),
        None => None,
    };
    Some((SemverVersion { core, prerelease }, display_version))
}

fn is_semver_identifier(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
}

fn parse_prerelease_identifier(value: &str) -> Option<PrereleaseIdentifier<'_>> {
    is_semver_identifier(value).then_some(())?;
    let numeric = if value.bytes().all(|byte| byte.is_ascii_digit()) {
        Some(parse_semver_number(value)?)
    } else {
        None
    };
    Some(PrereleaseIdentifier { value, numeric })
}

fn parse_semver_number(value: &str) -> Option<u64> {
    (!value.is_empty()
        && (value == "0" || !value.starts_with('0'))
        && value.bytes().all(|byte| byte.is_ascii_digit()))
    .then(|| value.parse().ok())
    .flatten()
}

fn is_update_available(current: &SemverVersion<'_>, latest: &SemverVersion<'_>) -> bool {
    use std::cmp::Ordering;

    let core_ordering = latest.core.cmp(&current.core);
    if core_ordering != Ordering::Equal {
        return core_ordering == Ordering::Greater;
    }
    match (&current.prerelease, &latest.prerelease) {
        (None, None) => false,
        (Some(_), None) => true,
        (None, Some(_)) => false,
        (Some(current), Some(latest)) => {
            latest
                .iter()
                .zip(current)
                .map(
                    |(latest, current)| match (latest.numeric, current.numeric) {
                        (Some(latest), Some(current)) => latest.cmp(&current),
                        (Some(_), None) => Ordering::Less,
                        (None, Some(_)) => Ordering::Greater,
                        (None, None) => latest.value.cmp(current.value),
                    },
                )
                .find(|ordering| *ordering != Ordering::Equal)
                .unwrap_or_else(|| latest.len().cmp(&current.len()))
                == Ordering::Greater
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn production_source_gates_the_update_check_seam_and_keeps_the_fixed_github_api() {
        let production = include_str!("updater.rs")
            .split("#[cfg(test)]")
            .next()
            .expect("production source precedes tests");
        assert!(
            production.contains("configured_builder(config)"),
            "production check-update must reuse the global proxy builder"
        );
        assert!(production.contains("GITHUB_LATEST_RELEASE_API"));
        assert!(
            production
                .contains("https://api.github.com/repos/klarkxy/opencode-go-mgr/releases/latest")
        );
        assert!(production.contains("GITHUB_LATEST_RELEASE_URL"));
        assert!(production.contains("https://github.com/klarkxy/opencode-go-mgr/releases/latest"));
        assert!(
            !production.contains("no_redirect_policy"),
            "update checks must keep the shared client's default redirect behavior"
        );
        assert!(production.contains("application/vnd.github+json"));
        assert!(production.contains("ocg-manager/"));
        assert!(production.contains("UPDATE_CHECK_TIMEOUT"));
        assert!(production.contains("Duration::from_secs(30)"));
        assert!(production.contains("settings_update.lock()"));
        assert!(production.contains("parse_mutation_json::<InstallUpdate>"));
        assert!(production.contains("check_expectation"));
        assert!(production.contains("start_desktop_update"));
        assert!(production.contains("StatusCode::ACCEPTED"));
        assert!(!production.contains("bump_settings_revision"));
        assert!(!production.contains("set_config"));
        let install_idx = production
            .find("pub(super) async fn install_update")
            .expect("install_update handler");
        let install = &production[install_idx..];
        let install = install
            .split("struct CapturedUpdateCheck")
            .next()
            .expect("install_update precedes capture struct");
        assert!(!install.contains("db.lock"));
        assert!(!install.contains("GITHUB_LATEST_RELEASE_API"));
        assert!(!install.contains(".send()"));
        assert!(production.contains("#[cfg(debug_assertions)]"));
        assert!(production.contains("#[cfg(not(debug_assertions))]"));
        for needle in [
            "UpdateCheckUrlGuard",
            "install_update_check_url_for_tests",
            "UPDATE_CHECK_URL_OVERRIDES",
            "debug_update_check_url",
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
        assert!(release.contains("GITHUB_LATEST_RELEASE_API"));
        assert!(!release.contains("UPDATE_CHECK_URL_OVERRIDES"));
        assert!(!release.contains("install_update_check_url_for_tests"));
        assert!(!release.contains("debug_update_check_url"));
    }

    #[test]
    fn strip_url_userinfo_redacts_credentials_and_leaves_hosts() {
        assert_eq!(
            strip_url_userinfo("error sending request for url (http://user:pass@127.0.0.1:9/)"),
            "error sending request for url (http://<redacted>@127.0.0.1:9/)"
        );
    }

    #[test]
    fn sanitize_update_detail_redacts_secrets_and_control_urls() {
        let detail = sanitize_update_detail(
            "failed to check GitHub releases (request failed): sk-secret at http://user:pass@127.0.0.1:9/ via https://api.github.com/repos/klarkxy/opencode-go-mgr/releases/latest",
            &["sk-secret"],
        );
        assert!(!detail.contains("sk-secret"));
        assert!(!detail.contains("user:pass"));
        assert!(!detail.contains("127.0.0.1"));
        assert!(!detail.contains("api.github.com"));
        assert!(detail.contains("<redacted>"));
    }

    #[test]
    fn semver_latest_is_strictly_newer_and_strips_v_prefix() {
        let (current, display) = parse_semver_version("1.8.2").unwrap();
        assert_eq!(display, "1.8.2");
        let (tagged, tagged_display) = parse_semver_version("v1.9.0-beta.1").unwrap();
        assert_eq!(tagged_display, "1.9.0-beta.1");
        assert!(is_update_available(&current, &tagged));
        let (same, _) = parse_semver_version("1.8.2").unwrap();
        assert!(!is_update_available(&current, &same));
        let (older, _) = parse_semver_version("1.0.0").unwrap();
        assert!(!is_update_available(&current, &older));
        assert!(parse_semver_version("not-a-version").is_none());
    }
}

#[cfg(all(test, debug_assertions))]
mod target_override_tests {
    use super::{
        debug_update_check_url, install_update_check_url_for_tests, parse_loopback_http_url,
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
        assert!(
            parse_loopback_http_url(
                "https://api.github.com/repos/klarkxy/opencode-go-mgr/releases/latest"
            )
            .is_none()
        );
        assert!(parse_loopback_http_url("http://127.0.0.2:9/").is_none());
        assert!(parse_loopback_http_url("http://127.0.0.1.example.com:9/").is_none());
    }

    #[test]
    fn overrides_are_isolated_by_process_generation_and_reject_ambiguous_urls() {
        let first = unique_generation();
        let second = unique_generation();
        let _guard_a = install_update_check_url_for_tests(first, "http://127.0.0.1:11/");
        let _guard_b = install_update_check_url_for_tests(second, "http://127.0.0.1:12/");
        assert_eq!(
            debug_update_check_url(first).as_deref(),
            Some("http://127.0.0.1:11/")
        );
        assert_eq!(
            debug_update_check_url(second).as_deref(),
            Some("http://127.0.0.1:12/")
        );

        drop(_guard_a);
        let _cleared =
            install_update_check_url_for_tests(first, "http://127.0.0.1:11@example.com/");
        assert!(debug_update_check_url(first).is_none());
        assert_eq!(
            debug_update_check_url(second).as_deref(),
            Some("http://127.0.0.1:12/")
        );
    }
}

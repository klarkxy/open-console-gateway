//! Test-only rewrite of canonical OpenCode Go, Zen, Command Code, and native URLs.
//!
//! Product builds, and any build without `ollama-cloud-loopback-test`, ignore
//! this seam and do not read `OCG_CPA_TEST_ENDPOINTS`. The feature-on reader
//! is [`rewrite_url`]. Tests call [`rewrite_url_with_mapping`] so they do not
//! touch the process environment. The mapping may replace only the scheme,
//! host, and port. Go, Zen, and Command Code keep their official path prefixes.
//! Native keys rewrite only the finite exact full URLs for that key. This
//! module does not read credentials, state, a selector, or a database.

use ocg_domain::ids::{
    COMMAND_CODE_PROVIDER_ID, OPENCODE_PROVIDER_ID, OPENCODE_ZEN_FREE_PROVIDER_ID,
};
use ocg_domain::provider::{
    COMMAND_CODE_GOAT_BASE_URL, COMMAND_CODE_GOAT_HOST, COMMAND_CODE_GOAT_USAGE_URL,
    OPENCODE_GO_BASE_URL, OPENCODE_GO_HOST, OPENCODE_ZEN_BASE_URL,
};
use std::sync::{Arc, OnceLock};

pub const TEST_ENDPOINTS_ENV: &str = "OCG_CPA_TEST_ENDPOINTS";

/// Official URLs. Feature-off and absent mappings share this value.
pub(crate) static OFFICIAL_ENDPOINTS: EndpointAuthority = EndpointAuthority::Official;

const MALFORMED: &str = "OCG_CPA_TEST_ENDPOINTS is malformed";
const ORIGIN_ERROR: &str =
    "OCG_CPA_TEST_ENDPOINTS value must be an HTTP loopback origin with an explicit port";
const UNKNOWN_KEY: &str = "OCG_CPA_TEST_ENDPOINTS contains an unknown provider";
const UNKNOWN_PROVIDER: &str = "unknown provider";
const WRONG_ORIGIN: &str = "canonical URL is not the official origin for this provider";
const ALREADY_SEALED: &str = "CPA test endpoints are already sealed";
const MISSING_REQUIRED: &str = "OCG_CPA_TEST_ENDPOINTS is missing a required provider";
#[cfg(not(feature = "ollama-cloud-loopback-test"))]
const FEATURE_OFF: &str = "CPA test endpoints are not enabled";
const GO_PATH_PREFIX: &str = "/zen/go";
const ZEN_PATH_PREFIX: &str = "/zen";

/// Validated loopback origins keyed by provider or native family.
#[derive(Clone, Debug)]
pub(crate) struct ParsedEndpointMap {
    origins: std::collections::BTreeMap<String, reqwest::Url>,
}

/// One frozen URL authority for a CoreState invocation.
#[derive(Clone, Debug)]
pub(crate) enum EndpointAuthority {
    Official,
    ChildEnvironment(ParsedEndpointMap),
    InstanceFixture(ParsedEndpointMap),
}

/// OnceLock linearization for instance install versus first-render seal.
pub(crate) struct EndpointFixtureSlot {
    finalized: OnceLock<Result<Arc<EndpointAuthority>, String>>,
    fallback: Result<Arc<EndpointAuthority>, String>,
}

impl EndpointAuthority {
    /// Rewrite one approved canonical URL. `Ok(None)` leaves the sealed URL.
    pub(crate) fn rewrite_url(
        &self,
        provider_id: &str,
        canonical_url: &str,
    ) -> Result<Option<String>, String> {
        #[cfg(not(feature = "ollama-cloud-loopback-test"))]
        {
            let _ = (provider_id, canonical_url);
            return Ok(None);
        }
        #[cfg(feature = "ollama-cloud-loopback-test")]
        {
            match self {
                Self::Official => Ok(None),
                Self::ChildEnvironment(map) => {
                    rewrite_from_origins(&map.origins, provider_id, canonical_url, false)
                }
                Self::InstanceFixture(map) => {
                    rewrite_from_origins(&map.origins, provider_id, canonical_url, true)
                }
            }
        }
    }
}

impl EndpointFixtureSlot {
    /// Capture the child environment once. Feature-off is Official and does
    /// not read the process environment. Constructor must not fail.
    pub(crate) fn capture_child_environment() -> Self {
        #[cfg(not(feature = "ollama-cloud-loopback-test"))]
        {
            return Self {
                finalized: OnceLock::new(),
                fallback: Ok(Arc::new(EndpointAuthority::Official)),
            };
        }
        #[cfg(feature = "ollama-cloud-loopback-test")]
        {
            let fallback = match std::env::var(TEST_ENDPOINTS_ENV) {
                Err(std::env::VarError::NotPresent) => Ok(Arc::new(EndpointAuthority::Official)),
                Err(std::env::VarError::NotUnicode(_)) => Err(MALFORMED.to_string()),
                Ok(value) => {
                    let trimmed = value.trim();
                    if trimmed.is_empty() {
                        Ok(Arc::new(EndpointAuthority::Official))
                    } else {
                        match parse_mapping(trimmed) {
                            Ok(origins) => Ok(Arc::new(EndpointAuthority::ChildEnvironment(
                                ParsedEndpointMap { origins },
                            ))),
                            Err(error) => Err(error),
                        }
                    }
                }
            };
            Self {
                finalized: OnceLock::new(),
                fallback,
            }
        }
    }

    /// Seal an instance map. Duplicate, late, and racing losers reject.
    /// Poisoned child environment cannot be hidden. Malformed text seals error.
    pub(crate) fn install_instance(&self, raw: &str) -> Result<(), String> {
        #[cfg(not(feature = "ollama-cloud-loopback-test"))]
        {
            let _ = raw;
            return Err(FEATURE_OFF.to_string());
        }
        #[cfg(feature = "ollama-cloud-loopback-test")]
        {
            if let Err(poison) = &self.fallback {
                return Err(poison.clone());
            }
            let prepared = match parse_instance_map(raw) {
                Ok(map) => Ok(Arc::new(EndpointAuthority::InstanceFixture(map))),
                Err(error) => Err(error),
            };
            match self.finalized.set(prepared.clone()) {
                Ok(()) => prepared.map(|_| ()),
                Err(_) => Err(ALREADY_SEALED.to_string()),
            }
        }
    }

    /// Installed value or captured fallback. Absence is not sealed here.
    pub(crate) fn authority(&self) -> Result<Arc<EndpointAuthority>, String> {
        if let Some(value) = self.finalized.get() {
            return value.clone();
        }
        self.fallback.clone()
    }

    /// Freeze one authority for this invocation. First render wins.
    pub(crate) fn seal_for_render(&self) -> Result<Arc<EndpointAuthority>, String> {
        self.finalized.get_or_init(|| self.fallback.clone()).clone()
    }
}

/// Complete Go/Goat/Zen instance JSON. Origins must already be loopback HTTP.
pub(crate) fn complete_instance_map_json(go: &str, goat: &str, zen: &str) -> String {
    format!(
        r#"{{"{OPENCODE_PROVIDER_ID}":"{go}","{COMMAND_CODE_PROVIDER_ID}":"{goat}","{OPENCODE_ZEN_FREE_PROVIDER_ID}":"{zen}"}}"#
    )
}

/// Same loopback origin for every required builtin participant.
pub(crate) fn complete_instance_map_for_origin(origin: &str) -> String {
    let origin = origin.trim_end_matches('/');
    complete_instance_map_json(origin, origin, origin)
}

#[cfg(feature = "ollama-cloud-loopback-test")]
pub(crate) fn parse_child_authority(raw: &str) -> Result<EndpointAuthority, String> {
    Ok(EndpointAuthority::ChildEnvironment(ParsedEndpointMap {
        origins: parse_mapping(raw)?,
    }))
}

#[cfg(test)]
pub(crate) fn slot_from_fallback(
    fallback: Result<Arc<EndpointAuthority>, String>,
) -> EndpointFixtureSlot {
    EndpointFixtureSlot {
        finalized: OnceLock::new(),
        fallback,
    }
}

/// Rewrite one approved canonical URL when the test feature and the child
/// environment supply a mapping. `Ok(None)` means the sealed URL is unchanged.
pub fn rewrite_url(provider_id: &str, canonical_url: &str) -> Result<Option<String>, String> {
    #[cfg(not(feature = "ollama-cloud-loopback-test"))]
    {
        let _ = (provider_id, canonical_url);
        return Ok(None);
    }
    #[cfg(feature = "ollama-cloud-loopback-test")]
    {
        match std::env::var(TEST_ENDPOINTS_ENV) {
            Ok(value) => rewrite_url_with_mapping(provider_id, canonical_url, Some(&value)),
            Err(std::env::VarError::NotPresent) => Ok(None),
            Err(std::env::VarError::NotUnicode(_)) => Err(MALFORMED.to_string()),
        }
    }
}

/// Pure mapping parser. `mapping_json` is the raw environment value, or
/// `None` when the variable is absent. Feature-off builds return `Ok(None)`
/// without inspecting the text.
pub fn rewrite_url_with_mapping(
    provider_id: &str,
    canonical_url: &str,
    mapping_json: Option<&str>,
) -> Result<Option<String>, String> {
    #[cfg(not(feature = "ollama-cloud-loopback-test"))]
    {
        let _ = (provider_id, canonical_url, mapping_json);
        return Ok(None);
    }
    #[cfg(feature = "ollama-cloud-loopback-test")]
    {
        apply_mapping(provider_id, canonical_url, mapping_json)
    }
}

#[cfg(feature = "ollama-cloud-loopback-test")]
fn apply_mapping(
    provider_id: &str,
    canonical_url: &str,
    mapping_json: Option<&str>,
) -> Result<Option<String>, String> {
    let Some(raw) = mapping_json
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(None);
    };
    let origins = parse_mapping(raw)?;
    rewrite_from_origins(&origins, provider_id, canonical_url, false)
}

#[cfg(feature = "ollama-cloud-loopback-test")]
fn parse_instance_map(raw: &str) -> Result<ParsedEndpointMap, String> {
    let origins = parse_mapping(raw)?;
    for required in [
        OPENCODE_PROVIDER_ID,
        COMMAND_CODE_PROVIDER_ID,
        OPENCODE_ZEN_FREE_PROVIDER_ID,
    ] {
        if !origins.contains_key(required) {
            return Err(format!("{MISSING_REQUIRED} `{required}`"));
        }
    }
    Ok(ParsedEndpointMap { origins })
}

#[cfg(feature = "ollama-cloud-loopback-test")]
fn participating_builtin(provider_id: &str) -> bool {
    provider_id == OPENCODE_PROVIDER_ID
        || provider_id == COMMAND_CODE_PROVIDER_ID
        || provider_id == OPENCODE_ZEN_FREE_PROVIDER_ID
}

#[cfg(feature = "ollama-cloud-loopback-test")]
fn rewrite_from_origins(
    origins: &std::collections::BTreeMap<String, reqwest::Url>,
    provider_id: &str,
    canonical_url: &str,
    require_participants: bool,
) -> Result<Option<String>, String> {
    if native_exact_urls(provider_id).is_some() {
        return rewrite_native(origins, provider_id, canonical_url);
    }
    if !participating_builtin(provider_id) {
        return Err(format!("{UNKNOWN_PROVIDER} `{provider_id}`"));
    }
    let Some(origin) = origins.get(provider_id) else {
        if require_participants {
            return Err(format!("{MISSING_REQUIRED} `{provider_id}`"));
        }
        return Ok(None);
    };
    let canonical = parse_canonical(provider_id, canonical_url)?;
    let mut rewritten = origin.clone();
    rewritten.set_path(canonical.path());
    rewritten.set_query(canonical.query());
    rewritten.set_fragment(None);
    Ok(Some(rewritten.to_string()))
}

#[cfg(feature = "ollama-cloud-loopback-test")]
fn rewrite_native(
    origins: &std::collections::BTreeMap<String, reqwest::Url>,
    provider_id: &str,
    canonical_url: &str,
) -> Result<Option<String>, String> {
    let Some(origin) = origins.get(provider_id) else {
        return Ok(None);
    };
    let Some(urls) = native_exact_urls(provider_id) else {
        return Err(format!("{UNKNOWN_PROVIDER} `{provider_id}`"));
    };
    if !urls.iter().any(|item| *item == canonical_url) {
        return Ok(None);
    }
    let path = path_and_query(canonical_url).ok_or_else(|| WRONG_ORIGIN.to_string())?;
    Ok(Some(splice_origin(origin, path)))
}

#[cfg(feature = "ollama-cloud-loopback-test")]
fn path_and_query(canonical_url: &str) -> Option<&str> {
    let rest = canonical_url.split_once("://")?.1;
    let path = rest.get(rest.find('/')?..)?;
    if path.contains('#') {
        return None;
    }
    Some(path)
}

#[cfg(feature = "ollama-cloud-loopback-test")]
fn splice_origin(origin: &reqwest::Url, path_and_query: &str) -> String {
    let mut prefix = origin.clone();
    prefix.set_path("/");
    prefix.set_query(None);
    prefix.set_fragment(None);
    let mut text = prefix.to_string();
    if let Some(stripped) = text.strip_suffix('/') {
        text = stripped.to_string();
    }
    text.push_str(path_and_query);
    text
}

#[cfg(feature = "ollama-cloud-loopback-test")]
const CODEX_URLS: [&str; 2] = [
    "https://chatgpt.com/backend-api/codex/responses",
    "https://chatgpt.com/backend-api/codex/responses/compact",
];
#[cfg(feature = "ollama-cloud-loopback-test")]
const ANTHROPIC_URLS: [&str; 2] = [
    "https://api.anthropic.com/v1/messages?beta=true",
    "https://api.anthropic.com/v1/messages/count_tokens?beta=true",
];
#[cfg(feature = "ollama-cloud-loopback-test")]
const KIMI_COM_URLS: [&str; 4] = [
    "https://api.kimi.com/coding/v1/chat/completions",
    "https://api.kimi.com/coding/v1/responses",
    "https://api.kimi.com/coding/v1/messages?beta=true",
    "https://api.kimi.com/coding/v1/messages/count_tokens?beta=true",
];
#[cfg(feature = "ollama-cloud-loopback-test")]
const KIMI_AI_URLS: [&str; 4] = [
    "https://api.kimi.ai/coding/v1/chat/completions",
    "https://api.kimi.ai/coding/v1/responses",
    "https://api.kimi.ai/coding/v1/messages?beta=true",
    "https://api.kimi.ai/coding/v1/messages/count_tokens?beta=true",
];
#[cfg(feature = "ollama-cloud-loopback-test")]
const XAI_CLI_URLS: [&str; 2] = [
    "https://cli-chat-proxy.grok.com/v1/responses",
    "https://api.x.ai/v1/responses/compact",
];
#[cfg(feature = "ollama-cloud-loopback-test")]
const XAI_API_URLS: [&str; 2] = [
    "https://api.x.ai/v1/responses",
    "https://api.x.ai/v1/responses/compact",
];
#[cfg(feature = "ollama-cloud-loopback-test")]
const ANTIGRAVITY_URLS: [&str; 3] = [
    "https://daily-cloudcode-pa.googleapis.com/v1internal:generateContent",
    "https://daily-cloudcode-pa.googleapis.com/v1internal:streamGenerateContent?alt=sse",
    "https://daily-cloudcode-pa.googleapis.com/v1internal:countTokens",
];

#[cfg(feature = "ollama-cloud-loopback-test")]
fn native_exact_urls(key: &str) -> Option<&'static [&'static str]> {
    match key {
        "codex" => Some(&CODEX_URLS),
        "anthropic" => Some(&ANTHROPIC_URLS),
        "kimi.com" => Some(&KIMI_COM_URLS),
        "kimi.ai" => Some(&KIMI_AI_URLS),
        "xai.cli" => Some(&XAI_CLI_URLS),
        "xai.api" => Some(&XAI_API_URLS),
        "antigravity" => Some(&ANTIGRAVITY_URLS),
        _ => None,
    }
}

#[cfg(feature = "ollama-cloud-loopback-test")]
fn parse_mapping(raw: &str) -> Result<std::collections::BTreeMap<String, reqwest::Url>, String> {
    let pairs = parse_string_map(raw)?;
    let mut origins = std::collections::BTreeMap::new();
    for (key, text) in pairs {
        if native_exact_urls(&key).is_none() && !participating_builtin(&key) {
            return Err(format!("{UNKNOWN_KEY} `{key}`"));
        }
        origins.insert(key, parse_loopback_origin(&text)?);
    }
    Ok(origins)
}

#[cfg(feature = "ollama-cloud-loopback-test")]
fn parse_string_map(raw: &str) -> Result<std::collections::BTreeMap<String, String>, String> {
    let parsed: DuplicateFreeStringMap = serde_json::from_str(raw).map_err(|error| {
        if error.to_string().contains("duplicate key") {
            format!("{MALFORMED}: duplicate key")
        } else {
            MALFORMED.to_string()
        }
    })?;
    Ok(parsed.entries)
}

#[cfg(feature = "ollama-cloud-loopback-test")]
struct DuplicateFreeStringMap {
    entries: std::collections::BTreeMap<String, String>,
}

#[cfg(feature = "ollama-cloud-loopback-test")]
impl<'de> serde::Deserialize<'de> for DuplicateFreeStringMap {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visit;
        impl<'de> serde::de::Visitor<'de> for Visit {
            type Value = std::collections::BTreeMap<String, String>;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("an object of string origins")
            }

            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> Result<Self::Value, A::Error> {
                let mut entries = std::collections::BTreeMap::new();
                while let Some((key, value)) = map.next_entry::<String, String>()? {
                    if entries.contains_key(&key) {
                        return Err(serde::de::Error::custom("duplicate key"));
                    }
                    entries.insert(key, value);
                }
                Ok(entries)
            }
        }
        deserializer
            .deserialize_map(Visit)
            .map(|entries| DuplicateFreeStringMap { entries })
    }
}

/// Parsed loopback host, plus an explicit nonzero port from the original authority.
///
/// url 2.5.8 `host_str` serializes IPv6 with brackets, and `port` stores an
/// explicit default port 80 as `None`. Neither value decides this check.
#[cfg(feature = "ollama-cloud-loopback-test")]
fn parse_loopback_origin(text: &str) -> Result<reqwest::Url, String> {
    let url = reqwest::Url::parse(text).map_err(|_| ORIGIN_ERROR.to_string())?;
    let path = url.path();
    if url.scheme() != "http"
        || !is_loopback_host(&url)
        || !explicit_nonzero_port(text)
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || (path != "/" && !path.is_empty())
    {
        return Err(ORIGIN_ERROR.to_string());
    }
    Ok(url)
}

#[cfg(feature = "ollama-cloud-loopback-test")]
fn is_loopback_host(url: &reqwest::Url) -> bool {
    let Some(host) = url.host() else {
        return false;
    };
    for probe in ["http://127.0.0.1/", "http://localhost/", "http://[::1]/"] {
        let Ok(expected) = reqwest::Url::parse(probe) else {
            continue;
        };
        if expected.host() == Some(host.clone()) {
            return true;
        }
    }
    false
}

/// Numeric port written in the original authority. Bracketed IPv6 keeps its
/// colons inside `[]`, so the port is only the digits after `]`.
#[cfg(feature = "ollama-cloud-loopback-test")]
fn explicit_nonzero_port(text: &str) -> bool {
    let Some(authority) = http_authority(text) else {
        return false;
    };
    let digits = if let Some(rest) = authority.strip_prefix('[') {
        let Some((_, after)) = rest.split_once(']') else {
            return false;
        };
        let Some(digits) = after.strip_prefix(':') else {
            return false;
        };
        digits
    } else {
        if authority.contains('@') {
            return false;
        }
        let Some((host, digits)) = authority.split_once(':') else {
            return false;
        };
        if host.is_empty() || host.contains(':') {
            return false;
        }
        digits
    };
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return false;
    }
    matches!(digits.parse::<u16>(), Ok(port) if port != 0)
}

#[cfg(feature = "ollama-cloud-loopback-test")]
fn http_authority(text: &str) -> Option<&str> {
    let (scheme, rest) = text.split_once("://")?;
    if !scheme.eq_ignore_ascii_case("http") || rest.is_empty() {
        return None;
    }
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..end];
    if authority.is_empty() {
        None
    } else {
        Some(authority)
    }
}

#[cfg(feature = "ollama-cloud-loopback-test")]
fn parse_canonical(provider_id: &str, canonical_url: &str) -> Result<reqwest::Url, String> {
    let url = reqwest::Url::parse(canonical_url).map_err(|_| WRONG_ORIGIN.to_string())?;
    let host = url.host_str().unwrap_or("").to_ascii_lowercase();
    let (expected_host, prefixes) = match provider_id {
        id if id == OPENCODE_PROVIDER_ID => {
            (OPENCODE_GO_HOST, official_paths(&[OPENCODE_GO_BASE_URL]))
        }
        id if id == OPENCODE_ZEN_FREE_PROVIDER_ID => {
            ("opencode.ai", official_paths(&[OPENCODE_ZEN_BASE_URL]))
        }
        id if id == COMMAND_CODE_PROVIDER_ID => (
            COMMAND_CODE_GOAT_HOST,
            official_paths(&[COMMAND_CODE_GOAT_BASE_URL, COMMAND_CODE_GOAT_USAGE_URL]),
        ),
        _ => return Err(format!("{UNKNOWN_PROVIDER} `{provider_id}`")),
    };
    let path = url.path();
    let allowed = prefixes
        .iter()
        .any(|prefix| path == prefix || path.starts_with(&format!("{prefix}/")))
        && zen_go_paths_agree(provider_id, path);
    if url.scheme() != "https"
        || host != expected_host
        || url.port().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || !allowed
    {
        return Err(WRONG_ORIGIN.to_string());
    }
    Ok(url)
}

#[cfg(feature = "ollama-cloud-loopback-test")]
fn zen_go_paths_agree(provider_id: &str, path: &str) -> bool {
    let go = path == GO_PATH_PREFIX || path.starts_with(&format!("{GO_PATH_PREFIX}/"));
    if provider_id == OPENCODE_PROVIDER_ID {
        return go;
    }
    if provider_id == OPENCODE_ZEN_FREE_PROVIDER_ID {
        let zen = path == ZEN_PATH_PREFIX || path.starts_with(&format!("{ZEN_PATH_PREFIX}/"));
        return zen && !go;
    }
    true
}

#[cfg(feature = "ollama-cloud-loopback-test")]
fn official_paths(urls: &[&str]) -> Vec<String> {
    urls.iter()
        .map(|url| {
            reqwest::Url::parse(url)
                .unwrap_or_else(|_| panic!("official provider URL is not absolute: {url}"))
                .path()
                .trim_end_matches('/')
                .to_string()
        })
        .collect()
}

#[cfg(test)]
mod tests;

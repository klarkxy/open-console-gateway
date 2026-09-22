//! Command Code account runtime and public Provider catalog refresh.
//!
//! Model supply is governed by the Provider model/protocol contract. Accounts
//! contribute credentials and ordering only; the public `/models` response is
//! not treated as proof that a stored Key is valid.

use crate::http_client;
use crate::models::AppConfig;
use crate::official_protocols::parse_catalog_supported_endpoints_baseline;
use crate::provider::{
    COMMAND_CODE_GOAT_BASE_URL, COMMAND_CODE_GOAT_MODELS_PATH, ConnectionVerificationStatus,
    parse_provider_models_catalog,
};
use std::collections::HashMap;
use std::fmt;
#[cfg(debug_assertions)]
use std::sync::{LazyLock, RwLock};
use std::time::Duration;

/// Data-only GOAT routing state loaded from persistence for one account.
#[derive(Debug, Clone)]
pub struct GoatAccountRuntime {
    pub account_id: String,
    pub enabled: bool,
    pub verification_status: ConnectionVerificationStatus,
    pub setup_ready: bool,
    pub has_key: bool,
}

pub const MAX_PROVIDER_CATALOG_BODY_BYTES: usize = 256 * 1024;

/// Public GET `/models` snapshot plus any per-model protocol evidence the JSON
/// actually listed. Missing endpoint fields leave
/// [`OfficialProtocolBaseline::Unavailable`] so saved evidence survives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderCatalogDiscovery {
    pub models: Vec<String>,
    pub protocol_baseline: OfficialProtocolBaseline,
}

pub use crate::official_protocols::OfficialProtocolBaseline;

#[cfg(debug_assertions)]
static GOAT_CATALOG_ORIGINS: LazyLock<RwLock<HashMap<u64, String>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

/// RAII guard for the debug-only Command Code public catalog origin substitute.
#[cfg(debug_assertions)]
#[doc(hidden)]
pub struct GoatCatalogOriginGuard {
    process_generation: u64,
    origin: String,
}

#[cfg(debug_assertions)]
impl Drop for GoatCatalogOriginGuard {
    fn drop(&mut self) {
        if let Ok(mut origins) = GOAT_CATALOG_ORIGINS.write()
            && origins
                .get(&self.process_generation)
                .is_some_and(|origin| origin == &self.origin)
        {
            origins.remove(&self.process_generation);
        }
    }
}

/// Installs a loopback-only origin used by Command Code GET `/models` tests.
#[cfg(debug_assertions)]
#[doc(hidden)]
pub fn install_goat_catalog_origin_for_test(
    process_generation: u64,
    origin: impl Into<String>,
) -> Result<GoatCatalogOriginGuard, String> {
    let origin = origin.into();
    ensure_loopback_origin(&origin)?;
    let origin = origin.trim_end_matches('/').to_string();
    let guard = GoatCatalogOriginGuard {
        process_generation,
        origin: origin.clone(),
    };
    GOAT_CATALOG_ORIGINS
        .write()
        .map_err(|_| "Command Code catalog origin lock is poisoned".to_string())?
        .insert(process_generation, origin);
    Ok(guard)
}

#[cfg(debug_assertions)]
pub fn goat_catalog_base_url(process_generation: Option<u64>) -> String {
    if let Some(generation) = process_generation
        && let Ok(origins) = GOAT_CATALOG_ORIGINS.read()
        && let Some(origin) = origins.get(&generation)
    {
        return format!("{}/provider/v1", origin.trim_end_matches('/'));
    }
    COMMAND_CODE_GOAT_BASE_URL.to_string()
}

#[cfg(debug_assertions)]
fn ensure_loopback_origin(origin: &str) -> Result<(), String> {
    let url = reqwest::Url::parse(origin).map_err(|error| error.to_string())?;
    if url.scheme() != "http"
        || !matches!(
            url.host_str(),
            Some("localhost") | Some("127.0.0.1") | Some("::1") | Some("[::1]")
        )
    {
        return Err("catalog test origin must be an HTTP loopback URL".to_string());
    }
    Ok(())
}

impl GoatAccountRuntime {
    pub fn eligible(&self) -> bool {
        self.enabled && self.setup_ready && self.has_key
    }

    /// Compatibility alias for [`Self::eligible`]. The requested model id is
    /// unused: GOAT model supply is the Provider contract, not this runtime.
    pub fn serves(&self, requested: &str) -> bool {
        let _ = requested;
        self.eligible()
    }
}

pub fn goat_runtimes_by_account(
    runtimes: &[GoatAccountRuntime],
) -> HashMap<String, GoatAccountRuntime> {
    runtimes
        .iter()
        .cloned()
        .map(|runtime| (runtime.account_id.clone(), runtime))
        .collect()
}

#[derive(Debug, Clone)]
pub struct GoatVerifyFailure {
    pub message: String,
}

impl fmt::Display for GoatVerifyFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for GoatVerifyFailure {}

pub fn official_goat_models_url() -> String {
    format!(
        "{}{}",
        COMMAND_CODE_GOAT_BASE_URL.trim_end_matches('/'),
        COMMAND_CODE_GOAT_MODELS_PATH
    )
}

pub fn goat_models_url_for_base(base: &str) -> String {
    format!(
        "{}{}",
        base.trim_end_matches('/'),
        COMMAND_CODE_GOAT_MODELS_PATH
    )
}

pub fn opencode_go_models_url_for_base(base: &str) -> String {
    let base = base.trim_end_matches('/');
    if base.ends_with("/v1") {
        format!("{base}/models")
    } else {
        format!("{base}/v1/models")
    }
}

pub fn ollama_cloud_models_url_for_base(base: &str) -> String {
    format!(
        "{}{}",
        base.trim_end_matches('/'),
        crate::kernel::ids::OLLAMA_CLOUD_MODELS_PATH
    )
}

/// Public, keyless Ollama Cloud GET `/models` refresh. Auth-free by design:
/// the endpoint is the catalog discovery surface, never a Key check.
pub async fn refresh_ollama_cloud_models(
    config: &AppConfig,
    base_url: &str,
) -> Result<Vec<String>, GoatVerifyFailure> {
    let url = ollama_cloud_models_url_for_base(base_url);
    probe_public_provider_models_at_url(config, &url, "Ollama Cloud").await
}

/// Debug-only loopback origin substitute for Ollama Cloud GET `/models`
/// tests. Mirrors the Command Code catalog seam but never appends a provider path.
#[cfg(debug_assertions)]
static OLLAMA_MODELS_ORIGINS: LazyLock<RwLock<HashMap<u64, String>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

#[cfg(debug_assertions)]
#[doc(hidden)]
pub struct OllamaModelsOriginGuard {
    process_generation: u64,
    origin: String,
}

#[cfg(debug_assertions)]
impl Drop for OllamaModelsOriginGuard {
    fn drop(&mut self) {
        if let Ok(mut origins) = OLLAMA_MODELS_ORIGINS.write()
            && origins
                .get(&self.process_generation)
                .is_some_and(|origin| origin == &self.origin)
        {
            origins.remove(&self.process_generation);
        }
    }
}

#[cfg(debug_assertions)]
#[doc(hidden)]
pub fn install_ollama_models_origin_for_test(
    process_generation: u64,
    origin: impl Into<String>,
) -> Result<OllamaModelsOriginGuard, String> {
    let origin = origin.into();
    ensure_loopback_origin(&origin)?;
    let origin = origin.trim_end_matches('/').to_string();
    let guard = OllamaModelsOriginGuard {
        process_generation,
        origin: origin.clone(),
    };
    OLLAMA_MODELS_ORIGINS
        .write()
        .map_err(|_| "Ollama models origin lock is poisoned".to_string())?
        .insert(process_generation, origin);
    Ok(guard)
}

#[cfg(debug_assertions)]
pub fn ollama_cloud_models_base_url(process_generation: Option<u64>) -> String {
    if let Some(generation) = process_generation
        && let Ok(origins) = OLLAMA_MODELS_ORIGINS.read()
        && let Some(origin) = origins.get(&generation)
    {
        return origin.trim_end_matches('/').to_string();
    }
    crate::kernel::ids::OLLAMA_CLOUD_BASE_URL.to_string()
}

pub async fn refresh_command_code_catalog_discovery(
    config: &AppConfig,
    base_url: &str,
) -> Result<ProviderCatalogDiscovery, GoatVerifyFailure> {
    let url = goat_models_url_for_base(base_url);
    probe_public_provider_catalog_at_url(config, &url, "Command Code").await
}

pub async fn refresh_command_code_models(
    config: &AppConfig,
    base_url: &str,
) -> Result<Vec<String>, GoatVerifyFailure> {
    Ok(refresh_command_code_catalog_discovery(config, base_url)
        .await?
        .models)
}

pub fn parse_provider_catalog_discovery(
    bytes: &[u8],
    provider_label: &str,
) -> Result<ProviderCatalogDiscovery, String> {
    let models = parse_provider_models_catalog(bytes, provider_label)?;
    let protocol_baseline = if provider_label == "Command Code" {
        parse_catalog_supported_endpoints_baseline(bytes)
    } else {
        OfficialProtocolBaseline::Unavailable
    };
    Ok(ProviderCatalogDiscovery {
        models,
        protocol_baseline,
    })
}

async fn probe_public_provider_catalog_at_url(
    config: &AppConfig,
    url: &str,
    provider_label: &str,
) -> Result<ProviderCatalogDiscovery, GoatVerifyFailure> {
    let client = http_client::configured_builder(config)
        .and_then(|builder| {
            builder
                .connect_timeout(Duration::from_secs(config.connect_timeout_secs))
                .redirect(http_client::no_redirect_policy())
                .build()
                .map_err(Into::into)
        })
        .map_err(|error| GoatVerifyFailure {
            message: format!("failed to build {provider_label} model refresh client: {error}"),
        })?;
    let response = client
        .get(url)
        .header(
            reqwest::header::USER_AGENT,
            concat!("OpenConsoleGateway/", env!("CARGO_PKG_VERSION")),
        )
        .header(reqwest::header::ACCEPT, "application/json")
        .timeout(Duration::from_secs(config.non_stream_timeout_secs))
        .send()
        .await
        .map_err(|error| GoatVerifyFailure {
            message: format!("{provider_label} GET /models failed: {error}"),
        })?;
    parse_provider_catalog_response(response, provider_label).await
}

async fn probe_public_provider_models_at_url(
    config: &AppConfig,
    url: &str,
    provider_label: &str,
) -> Result<Vec<String>, GoatVerifyFailure> {
    Ok(
        probe_public_provider_catalog_at_url(config, url, provider_label)
            .await?
            .models,
    )
}

async fn parse_provider_catalog_response(
    response: reqwest::Response,
    provider_label: &str,
) -> Result<ProviderCatalogDiscovery, GoatVerifyFailure> {
    let status = response.status();
    let bytes = read_limited_body(response, provider_label).await?;
    if !status.is_success() {
        return Err(GoatVerifyFailure {
            message: format!("{provider_label} GET /models returned {}", status.as_u16()),
        });
    }
    parse_provider_catalog_discovery(&bytes, provider_label)
        .map_err(|message| GoatVerifyFailure { message })
}

async fn parse_provider_models_response(
    response: reqwest::Response,
    provider_label: &str,
) -> Result<Vec<String>, GoatVerifyFailure> {
    Ok(parse_provider_catalog_response(response, provider_label)
        .await?
        .models)
}

/// Read the public Go directory without sending an account credential.
/// Protocol evidence stays on the official docs table; this JSON list has ids only.
pub async fn refresh_opencode_go_catalog_discovery(
    config: &AppConfig,
    base_url: &str,
) -> Result<ProviderCatalogDiscovery, GoatVerifyFailure> {
    let url = opencode_go_models_url_for_base(base_url);
    probe_public_provider_catalog_at_url(config, &url, "OpenCode Go").await
}

pub async fn refresh_opencode_go_models(
    config: &AppConfig,
    base_url: &str,
) -> Result<Vec<String>, GoatVerifyFailure> {
    Ok(refresh_opencode_go_catalog_discovery(config, base_url)
        .await?
        .models)
}

pub async fn probe_provider_models(
    config: &AppConfig,
    api_key: &str,
    base_url: &str,
    provider_label: &str,
) -> Result<Vec<String>, GoatVerifyFailure> {
    let url = goat_models_url_for_base(base_url);
    probe_provider_models_at_url(config, api_key, &url, provider_label).await
}

async fn probe_provider_models_at_url(
    config: &AppConfig,
    api_key: &str,
    url: &str,
    provider_label: &str,
) -> Result<Vec<String>, GoatVerifyFailure> {
    let key = api_key.trim();
    if key.is_empty() {
        return Err(GoatVerifyFailure {
            message: format!("{provider_label} model refresh requires a stored Key"),
        });
    }
    let client = http_client::configured_builder(config)
        .and_then(|builder| {
            builder
                .connect_timeout(Duration::from_secs(config.connect_timeout_secs))
                .redirect(http_client::no_redirect_policy())
                .build()
                .map_err(Into::into)
        })
        .map_err(|error| GoatVerifyFailure {
            message: format!("failed to build {provider_label} model client: {error}"),
        })?;
    let response = client
        .get(url)
        .bearer_auth(key)
        .header(reqwest::header::ACCEPT, "application/json")
        .timeout(Duration::from_secs(config.non_stream_timeout_secs))
        .send()
        .await
        .map_err(|error| GoatVerifyFailure {
            message: format!("{provider_label} GET /models failed: {error}"),
        })?;
    parse_provider_models_response(response, provider_label).await
}

async fn read_limited_body(
    response: reqwest::Response,
    provider_label: &str,
) -> Result<Vec<u8>, GoatVerifyFailure> {
    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();
    use futures_util::StreamExt;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| GoatVerifyFailure {
            message: format!("{provider_label} GET /models body failed: {error}"),
        })?;
        if bytes.len() + chunk.len() > MAX_PROVIDER_CATALOG_BODY_BYTES {
            return Err(GoatVerifyFailure {
                message: format!(
                    "{provider_label} GET /models exceeded the {MAX_PROVIDER_CATALOG_BODY_BYTES}-byte limit"
                ),
            });
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn runtime(
        enabled: bool,
        verification_status: ConnectionVerificationStatus,
    ) -> GoatAccountRuntime {
        GoatAccountRuntime {
            account_id: "goat-1".into(),
            enabled,
            verification_status,
            setup_ready: true,
            has_key: true,
        }
    }

    #[test]
    fn account_eligibility_does_not_reinterpret_the_provider_model_preset() {
        let pending = runtime(true, ConnectionVerificationStatus::Pending);
        assert!(pending.eligible());
        assert!(pending.serves("any-model-in-the-provider-contract"));
        assert_eq!(
            pending.serves("any-model-in-the-provider-contract"),
            pending.eligible()
        );
        let disabled = runtime(false, ConnectionVerificationStatus::Verified);
        assert!(!disabled.eligible());
        assert!(!disabled.serves("any-model-in-the-provider-contract"));
        let mut missing_key = runtime(true, ConnectionVerificationStatus::Verified);
        missing_key.has_key = false;
        assert!(!missing_key.eligible());
        assert_eq!(missing_key.serves("catalog-model"), missing_key.eligible());
    }

    #[test]
    fn opencode_go_models_url_keeps_the_official_v1_segment() {
        assert_eq!(
            opencode_go_models_url_for_base("https://opencode.ai/zen/go"),
            "https://opencode.ai/zen/go/v1/models"
        );
        assert_eq!(
            opencode_go_models_url_for_base("http://127.0.0.1:9/provider/v1/"),
            "http://127.0.0.1:9/provider/v1/models"
        );
    }

    #[test]
    fn goat_catalog_discovery_keeps_supported_endpoints_metadata() {
        let discovery = parse_provider_catalog_discovery(
            br#"{
                "object":"list",
                "data":[
                    {"id":"xiaomi/mimo-v2.6-flash","supported_endpoints":["/chat/completions","/responses"]},
                    {"id":"claude-sonnet-4-6","supported_endpoints":["/messages"]}
                ]
            }"#,
            "Command Code",
        )
        .unwrap();
        assert_eq!(
            discovery.models,
            vec![
                "xiaomi/mimo-v2.6-flash".to_string(),
                "claude-sonnet-4-6".to_string()
            ]
        );
        assert_eq!(
            discovery
                .protocol_baseline
                .protocols_for("command-code", "xiaomi/mimo-v2.6-flash"),
            Some(vec![
                crate::provider::UpstreamProtocolKind::ChatCompletions,
                crate::provider::UpstreamProtocolKind::Responses
            ])
        );
    }

    #[test]
    fn go_catalog_discovery_does_not_invent_protocol_lists() {
        let discovery = parse_provider_catalog_discovery(
            br#"{"object":"list","data":[{"id":"mimo-v2.6-flash"}]}"#,
            "OpenCode Go",
        )
        .unwrap();
        assert_eq!(discovery.models, vec!["mimo-v2.6-flash".to_string()]);
        assert_eq!(
            discovery.protocol_baseline,
            OfficialProtocolBaseline::Unavailable
        );
    }
}

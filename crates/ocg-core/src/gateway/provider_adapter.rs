//! Sealed provider transport construction. Production receives an execution
//! credential, a destination and an already-selected protocol/model. Account
//! interfaces below are operational probe and compatibility boundaries.

use crate::custom_http::{join_inference_endpoint, resolve_custom_endpoints};
use crate::gateway::attempt::{AttemptSpec, CredentialHandle, ProxyRoutingModel};
use crate::gateway::free_models::resolve_upstream_base;
use crate::gateway::protocol::{
    ApiFormat, RequestPlan, command_code_supports_upstream, command_code_upstream_path,
    opencode_supports_upstream,
};
use crate::gateway::wire::WireNormalization;
use crate::kernel::ids::{OLLAMA_CLOUD_BASE_URL, OLLAMA_CLOUD_CHAT_COMPLETIONS_PATH};
use crate::models::{Account, AppConfig, UpstreamChannel};
use crate::provider::{
    COMMAND_CODE_GOAT_BASE_URL, COMMAND_CODE_GOAT_CHAT_COMPLETIONS_PATH, COMMAND_CODE_GOAT_HOST,
    COMMAND_CODE_GOAT_MESSAGES_PATH, COMMAND_CODE_GOAT_MODELS_PATH, CredentialKind,
    InferenceAuthDescriptor, KIMI_CN_BASE_URL, KIMI_CN_CHAT_COMPLETIONS_PATH,
    KIMI_CN_MESSAGES_PATH, MINIMAX_CN_ANTHROPIC_BASE_URL, MINIMAX_CN_BASE_URL,
    MINIMAX_CN_CHAT_COMPLETIONS_PATH, MINIMAX_CN_MESSAGES_PATH, MINIMAX_CN_RESPONSES_PATH,
    ProviderAdapterKind, ProviderRegistry, QuotaScope, UpstreamAuthScheme,
};
#[cfg(any(debug_assertions, feature = "ollama-cloud-loopback-test"))]
use std::collections::HashMap;
#[cfg(any(debug_assertions, feature = "ollama-cloud-loopback-test"))]
use std::sync::{LazyLock, RwLock};

/// Construct transport from explicit destination facts after catalog protocol
/// selection. This is the production seam; Account adapters below serve probes.
pub(crate) fn resolve_execution_route(
    credential: &crate::routing_snapshot::ExecutionCredential,
    destination: &ocg_domain::destination::Destination,
    config: &AppConfig,
    plan: &RequestPlan,
) -> Result<AttemptSpec, String> {
    use ocg_domain::destination::AdapterKind;
    let id = credential.id.as_str();
    let (transport, handle) = match destination.adapter {
        AdapterKind::Http => {
            let route = plan.custom_route.as_ref().ok_or("missing HTTP route")?;
            (
                configurable_http_transport(&route.endpoint_url, route.auth_kind, plan.upstream)?,
                http_credential(id, route.auth_kind),
            )
        }
        AdapterKind::OpencodeGo => (
            opencode_go_transport(
                resolve_upstream_base(plan.channel, &config.upstream_base_url)?,
                plan.upstream,
            )?,
            keyed_credential(id),
        ),
        AdapterKind::Zen => (
            zen_free_transport(
                zen_resolved_base(config, plan.upstream_base_override.as_deref(), plan.channel)?,
                plan.upstream,
            )?,
            CredentialHandle::None,
        ),
        AdapterKind::Goat => (goat_transport(id, plan.upstream)?, keyed_credential(id)),
        AdapterKind::Minimax => (minimax_cn_transport(plan.upstream)?, keyed_credential(id)),
        AdapterKind::Kimi => (kimi_cn_transport(plan.upstream)?, keyed_credential(id)),
        AdapterKind::Ollama => (
            ollama_cloud_transport(id, plan.upstream)?,
            keyed_credential(id),
        ),
        AdapterKind::Cpa => {
            let base_url = plan
                .upstream_base_override
                .clone()
                .ok_or("CPA is not configured")?;
            (
                cpa_transport(base_url, plan.upstream)?,
                keyed_credential(id),
            )
        }
    };
    Ok(transport.into_spec(plan.upstream, handle))
}

pub(crate) use crate::gateway::attempt::UpstreamAuth;

/// Deterministic official Command Code GOAT transport. Production inference
/// uses this origin after an account is enabled, verified, and catalogued.
/// Loopback substitutes exist only as a test seam.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandCodeGoatTransportSpec {
    pub base_url: &'static str,
    pub host: &'static str,
    pub chat_completions_path: &'static str,
    pub messages_path: &'static str,
    pub models_path: &'static str,
    pub auth_scheme: UpstreamAuthScheme,
    pub follow_redirects: bool,
    pub zdr_header_name: Option<&'static str>,
    pub public_catalog_refresh: bool,
}

pub fn command_code_goat_transport_spec() -> CommandCodeGoatTransportSpec {
    CommandCodeGoatTransportSpec {
        base_url: COMMAND_CODE_GOAT_BASE_URL,
        host: COMMAND_CODE_GOAT_HOST,
        chat_completions_path: COMMAND_CODE_GOAT_CHAT_COMPLETIONS_PATH,
        messages_path: COMMAND_CODE_GOAT_MESSAGES_PATH,
        models_path: COMMAND_CODE_GOAT_MODELS_PATH,
        auth_scheme: UpstreamAuthScheme::Bearer,
        follow_redirects: false,
        zdr_header_name: None,
        public_catalog_refresh: true,
    }
}

pub fn command_code_goat_join_url(base: &str, upstream: ApiFormat) -> Result<String, String> {
    let path = command_code_upstream_path(upstream)
        .ok_or_else(|| format!("Command Code GOAT has no upstream path for {upstream:?}"))?;
    join_inference_endpoint(base, path)
        .map(|url| url.to_string())
        .map_err(|error| error.to_string())
}

pub fn command_code_goat_official_url(upstream: ApiFormat) -> Result<String, String> {
    command_code_goat_join_url(COMMAND_CODE_GOAT_BASE_URL, upstream)
}

pub fn command_code_goat_loopback_base(origin: &str) -> String {
    format!("{}/provider/v1", origin.trim_end_matches('/'))
}

/// GOAT loopback substitutes exist only in non-release builds so integration
/// tests can link them. Release transport uses the official origin and does
/// not read this table. `cfg(test)` would hide it from integration tests,
/// which link the library without that cfg.
#[cfg(debug_assertions)]
#[derive(Debug, Clone)]
struct GoatLoopbackRoute {
    origin: String,
}

#[cfg(debug_assertions)]
static GOAT_LOOPBACK_ROUTES: LazyLock<RwLock<HashMap<String, GoatLoopbackRoute>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

#[cfg(feature = "ollama-cloud-loopback-test")]
static OLLAMA_LOOPBACK_ROUTES: LazyLock<RwLock<HashMap<String, String>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

/// RAII guard for the integration-only Ollama Cloud seam. Compiled only with
/// the default-off `ollama-cloud-loopback-test` feature. Builds without that
/// feature always use the fixed `https://ollama.com` origin; without a live
/// guard, tests cannot reach a fake upstream.
#[cfg(feature = "ollama-cloud-loopback-test")]
#[doc(hidden)]
pub struct OllamaCloudLoopbackRouteGuard {
    account_id: String,
    origin: String,
}

#[cfg(feature = "ollama-cloud-loopback-test")]
impl Drop for OllamaCloudLoopbackRouteGuard {
    fn drop(&mut self) {
        if let Ok(mut routes) = OLLAMA_LOOPBACK_ROUTES.write()
            && routes
                .get(&self.account_id)
                .is_some_and(|origin| *origin == self.origin)
        {
            routes.remove(&self.account_id);
        }
    }
}

/// Installs a loopback-only origin substitute used by gateway integration
/// tests. Path, protocol, Bearer auth, and the wire normalization marker come
/// from the official Ollama Cloud contract; this cannot configure a remote
/// production endpoint.
#[cfg(feature = "ollama-cloud-loopback-test")]
#[doc(hidden)]
pub fn install_ollama_cloud_loopback_route_for_test(
    account_id: impl Into<String>,
    origin: impl Into<String>,
) -> Result<OllamaCloudLoopbackRouteGuard, String> {
    let account_id = account_id.into();
    let origin = origin.into();
    ensure_loopback_base(&origin)?;
    let trimmed = origin.trim_end_matches('/').to_string();
    let guard = OllamaCloudLoopbackRouteGuard {
        account_id: account_id.clone(),
        origin: trimmed.clone(),
    };
    OLLAMA_LOOPBACK_ROUTES
        .write()
        .map_err(|_| "Ollama Cloud loopback route lock is poisoned".to_string())?
        .insert(account_id, trimmed);
    Ok(guard)
}

#[cfg(feature = "ollama-cloud-loopback-test")]
fn ollama_cloud_base_url_for_id(account_id: &str) -> Result<String, String> {
    let routes = OLLAMA_LOOPBACK_ROUTES
        .read()
        .map_err(|_| "Ollama Cloud loopback route lock is poisoned".to_string())?;
    Ok(routes
        .get(account_id)
        .cloned()
        .unwrap_or_else(|| OLLAMA_CLOUD_BASE_URL.to_string()))
}

#[cfg(not(feature = "ollama-cloud-loopback-test"))]
fn ollama_cloud_base_url_for_id(_account_id: &str) -> Result<String, String> {
    Ok(OLLAMA_CLOUD_BASE_URL.to_string())
}

#[cfg(debug_assertions)]
#[doc(hidden)]
pub use crate::goat::{GoatCatalogOriginGuard, install_goat_catalog_origin_for_test};

/// RAII guard for the integration-only GOAT seam. The production adapter has
/// no endpoint or protocol guesses: without a live guard, GOAT is unsupported.
#[cfg(debug_assertions)]
#[doc(hidden)]
pub struct GoatLoopbackRouteGuard {
    account_id: String,
    base_url: String,
}

#[cfg(debug_assertions)]
impl Drop for GoatLoopbackRouteGuard {
    fn drop(&mut self) {
        if let Ok(mut routes) = GOAT_LOOPBACK_ROUTES.write()
            && routes
                .get(&self.account_id)
                .is_some_and(|route| route.origin == self.base_url)
        {
            routes.remove(&self.account_id);
        }
    }
}

/// Installs a loopback-only origin substitute used by gateway integration tests.
/// Models, protocol, path, and Bearer auth come from the official Command Code
/// contract; this cannot configure a remote production endpoint.
#[cfg(debug_assertions)]
#[doc(hidden)]
pub fn install_goat_loopback_route_for_test(
    account_id: impl Into<String>,
    origin: impl Into<String>,
) -> Result<GoatLoopbackRouteGuard, String> {
    let account_id = account_id.into();
    let origin = origin.into();
    ensure_loopback_base(&origin)?;
    let route = GoatLoopbackRoute {
        origin: origin.trim_end_matches('/').to_string(),
    };
    let guard = GoatLoopbackRouteGuard {
        account_id: account_id.clone(),
        base_url: route.origin.clone(),
    };
    GOAT_LOOPBACK_ROUTES
        .write()
        .map_err(|_| "GOAT loopback route lock is poisoned".to_string())?
        .insert(account_id, route);
    Ok(guard)
}

/// Shared transport construction for production `resolve_execution_route` and
/// probe/account-test resolvers. Eligibility, credential/grant checks, model
/// identity, Custom PublicOnly, and dynamic mapping selection stay with the
/// caller.
#[derive(Debug, Clone, PartialEq, Eq)]
struct TransportConstruction {
    base_url: String,
    path: String,
    auth: UpstreamAuth,
    follow_redirects: bool,
    proxy_routing: ProxyRoutingModel,
    wire_normalization: WireNormalization,
}

impl TransportConstruction {
    fn into_spec(self, upstream: ApiFormat, credential: CredentialHandle) -> AttemptSpec {
        AttemptSpec {
            base_url: self.base_url,
            path: self.path,
            upstream,
            auth: self.auth,
            follow_redirects: self.follow_redirects,
            credential,
            proxy_routing: self.proxy_routing,
            wire_normalization: self.wire_normalization,
        }
    }
}

fn keyed_credential(id: &str) -> CredentialHandle {
    CredentialHandle::Account { id: id.to_string() }
}

fn http_credential(id: &str, auth_kind: ocg_domain::dynamic::DynamicAuthKind) -> CredentialHandle {
    if auth_kind.requires_key() {
        keyed_credential(id)
    } else {
        CredentialHandle::None
    }
}

fn http_auth(kind: ocg_domain::dynamic::DynamicAuthKind) -> UpstreamAuth {
    match kind {
        ocg_domain::dynamic::DynamicAuthKind::Bearer => UpstreamAuth::Bearer,
        ocg_domain::dynamic::DynamicAuthKind::XApiKey => UpstreamAuth::XApiKey,
        ocg_domain::dynamic::DynamicAuthKind::ApiKey => UpstreamAuth::ApiKey,
        ocg_domain::dynamic::DynamicAuthKind::None => UpstreamAuth::None,
    }
}

fn http_inference_origin(
    endpoint_url: &str,
    protocol: crate::provider::UpstreamProtocolKind,
) -> Result<(String, String), String> {
    let endpoint = resolve_custom_endpoints(endpoint_url, protocol)
        .map_err(|error| error.to_string())?
        .inference;
    let path = endpoint.path().to_string();
    let mut base = endpoint;
    base.set_path("");
    base.set_query(None);
    base.set_fragment(None);
    Ok((base.as_str().trim_end_matches('/').to_string(), path))
}

fn configurable_http_transport(
    endpoint_url: &str,
    auth_kind: ocg_domain::dynamic::DynamicAuthKind,
    upstream: ApiFormat,
) -> Result<TransportConstruction, String> {
    let protocol = protocol_kind_for(upstream)?;
    let (base_url, path) = http_inference_origin(endpoint_url, protocol)?;
    Ok(TransportConstruction {
        base_url,
        path,
        auth: http_auth(auth_kind),
        follow_redirects: false,
        proxy_routing: ProxyRoutingModel::IsolatedTrustedAdmin,
        wire_normalization: WireNormalization::None,
    })
}

fn sealed_transport(
    kind: ProviderAdapterKind,
    base_url: String,
    path: String,
    proxy_routing: ProxyRoutingModel,
    wire_normalization: WireNormalization,
) -> Result<TransportConstruction, String> {
    let descriptor = sealed_descriptor(kind)?;
    let auth = match descriptor.inference.auth {
        InferenceAuthDescriptor::OpenCodeProtocolDefault => UpstreamAuth::OpenCodeProtocolDefault,
        InferenceAuthDescriptor::Bearer => UpstreamAuth::Bearer,
        InferenceAuthDescriptor::None => UpstreamAuth::None,
        InferenceAuthDescriptor::ProtocolDerivedBearerOrXApiKey => {
            return Err("HTTP authentication must come from the selected route".into());
        }
    };
    Ok(TransportConstruction {
        base_url,
        path,
        auth,
        follow_redirects: descriptor.inference.follow_redirects,
        proxy_routing,
        wire_normalization,
    })
}

fn opencode_go_transport(
    base_url: String,
    upstream: ApiFormat,
) -> Result<TransportConstruction, String> {
    sealed_transport(
        ProviderAdapterKind::OpenCodeGo,
        base_url,
        opencode_upstream_path(upstream)?,
        ProxyRoutingModel::RequestEntrySnapshot,
        WireNormalization::None,
    )
}

fn zen_resolved_base(
    config: &AppConfig,
    override_url: Option<&str>,
    channel: UpstreamChannel,
) -> Result<String, String> {
    match override_url {
        Some(url) => Ok(url.to_string()),
        None => resolve_upstream_base(channel, &config.upstream_base_url),
    }
}

fn zen_free_transport(
    base_url: String,
    upstream: ApiFormat,
) -> Result<TransportConstruction, String> {
    sealed_transport(
        ProviderAdapterKind::ZenFree,
        base_url,
        opencode_upstream_path(upstream)?,
        ProxyRoutingModel::RequestEntrySnapshot,
        WireNormalization::None,
    )
}

#[cfg(debug_assertions)]
fn goat_base_url_for(account_id: &str) -> Result<String, String> {
    let routes = GOAT_LOOPBACK_ROUTES
        .read()
        .map_err(|_| "GOAT loopback route lock is poisoned".to_string())?;
    Ok(routes.get(account_id).map_or_else(
        || COMMAND_CODE_GOAT_BASE_URL.to_string(),
        |route| command_code_goat_loopback_base(&route.origin),
    ))
}

#[cfg(not(debug_assertions))]
fn goat_base_url_for(_account_id: &str) -> Result<String, String> {
    Ok(COMMAND_CODE_GOAT_BASE_URL.to_string())
}

fn goat_transport(account_id: &str, upstream: ApiFormat) -> Result<TransportConstruction, String> {
    let path = command_code_upstream_path(upstream)
        .ok_or_else(|| format!("Command Code GOAT has no upstream path for {upstream:?}"))?;
    sealed_transport(
        ProviderAdapterKind::CommandCodeGoat,
        goat_base_url_for(account_id)?,
        path.to_string(),
        ProxyRoutingModel::ProcessWideNoRedirect,
        WireNormalization::None,
    )
}

fn minimax_cn_transport(upstream: ApiFormat) -> Result<TransportConstruction, String> {
    let (base_url, path) = match upstream {
        ApiFormat::ChatCompletions => (MINIMAX_CN_BASE_URL, MINIMAX_CN_CHAT_COMPLETIONS_PATH),
        ApiFormat::Messages => (MINIMAX_CN_ANTHROPIC_BASE_URL, MINIMAX_CN_MESSAGES_PATH),
        ApiFormat::Responses => (MINIMAX_CN_BASE_URL, MINIMAX_CN_RESPONSES_PATH),
        ApiFormat::Gemini => {
            return Err(
                "MiniMax CN Token Plan has no official upstream path for this protocol".into(),
            );
        }
    };
    sealed_transport(
        ProviderAdapterKind::MiniMaxCn,
        base_url.to_string(),
        path.to_string(),
        ProxyRoutingModel::ProcessWideNoRedirect,
        WireNormalization::None,
    )
}

fn kimi_cn_transport(upstream: ApiFormat) -> Result<TransportConstruction, String> {
    let path = match upstream {
        ApiFormat::ChatCompletions => KIMI_CN_CHAT_COMPLETIONS_PATH,
        ApiFormat::Messages => KIMI_CN_MESSAGES_PATH,
        ApiFormat::Responses | ApiFormat::Gemini => {
            return Err("Kimi Code CN has no official upstream path for this protocol".into());
        }
    };
    sealed_transport(
        ProviderAdapterKind::KimiCn,
        KIMI_CN_BASE_URL.to_string(),
        path.to_string(),
        ProxyRoutingModel::ProcessWideNoRedirect,
        WireNormalization::None,
    )
}

fn ollama_cloud_transport(
    account_id: &str,
    upstream: ApiFormat,
) -> Result<TransportConstruction, String> {
    if upstream != ApiFormat::ChatCompletions {
        return Err("Ollama Cloud has no official upstream path for this protocol".into());
    }
    sealed_transport(
        ProviderAdapterKind::OllamaCloud,
        ollama_cloud_base_url_for_id(account_id)?,
        OLLAMA_CLOUD_CHAT_COMPLETIONS_PATH.to_string(),
        ProxyRoutingModel::ProcessWideNoRedirect,
        WireNormalization::OllamaCloud,
    )
}

fn cpa_transport(base_url: String, upstream: ApiFormat) -> Result<TransportConstruction, String> {
    crate::cpa::normalize_base_url(&base_url, true).map_err(|error| error.to_string())?;
    let path = upstream
        .upstream_path()
        .ok_or_else(|| "CPA has no native Gemini inference path".to_string())?;
    sealed_transport(
        ProviderAdapterKind::Cpa,
        base_url,
        path.to_string(),
        ProxyRoutingModel::LocalExternalIntegration,
        WireNormalization::None,
    )
}

#[derive(Clone, Copy)]
enum RoutePolicy {
    /// Explicit admin probe: validate the structural ceiling and construct
    /// the endpoint/auth path without requiring prior verified support.
    Probe,
    /// Account-scoped operational test. This keeps the production route shape
    /// (including GOAT and CN routes) while deliberately bypassing normal
    /// availability selection: the dashboard has already locked one account.
    AccountTest,
}

pub(crate) fn resolve_probe_route(
    account: &Account,
    adapter: ProviderAdapterKind,
    config: &AppConfig,
    plan: &RequestPlan,
) -> Result<AttemptSpec, String> {
    resolve_route_with_policy(account, adapter, config, plan, RoutePolicy::Probe)
}

pub(crate) fn resolve_account_test_route(
    account: &Account,
    adapter: ProviderAdapterKind,
    config: &AppConfig,
    plan: &RequestPlan,
) -> Result<AttemptSpec, String> {
    resolve_route_with_policy(account, adapter, config, plan, RoutePolicy::AccountTest)
}

fn resolve_route_with_policy(
    account: &Account,
    adapter: ProviderAdapterKind,
    config: &AppConfig,
    plan: &RequestPlan,
    policy: RoutePolicy,
) -> Result<AttemptSpec, String> {
    match adapter {
        ProviderAdapterKind::OpenCodeGo => resolve_open_code_go(account, config, plan, policy),
        ProviderAdapterKind::ZenFree => resolve_zen_free(account, config, plan, policy),
        ProviderAdapterKind::CommandCodeGoat => {
            resolve_command_code_goat(account, config, plan, policy)
        }
        ProviderAdapterKind::MiniMaxCn => resolve_minimax_cn(account, config, plan, policy),
        ProviderAdapterKind::KimiCn => resolve_kimi_cn(account, config, plan, policy),
        ProviderAdapterKind::OllamaCloud => resolve_ollama_cloud(account, config, plan, policy),
        ProviderAdapterKind::ConfigurableHttp if matches!(policy, RoutePolicy::AccountTest) => {
            resolve_prepared_http(account, plan)
        }
        ProviderAdapterKind::ConfigurableHttp => {
            resolve_configurable_http(account, config, plan, policy)
        }
        ProviderAdapterKind::Cpa => resolve_cpa(account, config, plan, policy),
    }
}

fn resolve_open_code_go(
    account: &Account,
    config: &AppConfig,
    plan: &RequestPlan,
    policy: RoutePolicy,
) -> Result<AttemptSpec, String> {
    let descriptor = sealed_descriptor(ProviderAdapterKind::OpenCodeGo)?;
    require_binding(
        account,
        descriptor.inference.credential_kind,
        descriptor.inference.quota_scope,
    )?;
    if plan.channel != UpstreamChannel::Go {
        return Err("OpenCode Go does not serve the Zen free channel".to_string());
    }
    let transport = opencode_go_transport(
        resolve_upstream_base(UpstreamChannel::Go, &config.upstream_base_url)?,
        plan.upstream,
    )?;
    require_opencode_protocol_policy(descriptor, account, plan, policy, "OpenCode Go")?;
    Ok(transport.into_spec(plan.upstream, credential_handle(account, descriptor)))
}

fn resolve_zen_free(
    account: &Account,
    config: &AppConfig,
    plan: &RequestPlan,
    policy: RoutePolicy,
) -> Result<AttemptSpec, String> {
    let descriptor = sealed_descriptor(ProviderAdapterKind::ZenFree)?;
    require_binding(
        account,
        descriptor.inference.credential_kind,
        descriptor.inference.quota_scope,
    )?;
    if plan.channel != UpstreamChannel::Free {
        return Err(format!(
            "Zen Free does not support routed model `{}` on this channel",
            plan.model
        ));
    }
    let transport = zen_free_transport(
        zen_resolved_base(
            config,
            plan.upstream_base_override.as_deref(),
            UpstreamChannel::Free,
        )?,
        plan.upstream,
    )?;
    require_opencode_protocol_policy(descriptor, account, plan, policy, "Zen Free")?;
    Ok(transport.into_spec(plan.upstream, credential_handle(account, descriptor)))
}

fn resolve_command_code_goat(
    account: &Account,
    _config: &AppConfig,
    plan: &RequestPlan,
    policy: RoutePolicy,
) -> Result<AttemptSpec, String> {
    let descriptor = sealed_descriptor(ProviderAdapterKind::CommandCodeGoat)?;
    require_binding(
        account,
        descriptor.inference.credential_kind,
        descriptor.inference.quota_scope,
    )?;
    if plan.channel != UpstreamChannel::Go {
        return Err("Command Code GOAT does not serve the Zen free channel".to_string());
    }
    let transport = goat_transport(&account.id, plan.upstream)?;
    require_opencode_protocol_policy(descriptor, account, plan, policy, "Command Code GOAT")?;
    Ok(transport.into_spec(plan.upstream, credential_handle(account, descriptor)))
}

fn resolve_fixed_provider_plan(
    account: &Account,
    plan: &RequestPlan,
    policy: RoutePolicy,
    adapter: ProviderAdapterKind,
    label: &str,
    transport: TransportConstruction,
) -> Result<AttemptSpec, String> {
    let descriptor = sealed_descriptor(adapter)?;
    require_binding(
        account,
        descriptor.inference.credential_kind,
        descriptor.inference.quota_scope,
    )?;
    if plan.channel != UpstreamChannel::Go {
        return Err(format!("{label} does not serve the Zen free channel"));
    }
    require_opencode_protocol_policy(descriptor, account, plan, policy, label)?;
    Ok(transport.into_spec(plan.upstream, credential_handle(account, descriptor)))
}

fn resolve_minimax_cn(
    account: &Account,
    _config: &AppConfig,
    plan: &RequestPlan,
    policy: RoutePolicy,
) -> Result<AttemptSpec, String> {
    resolve_fixed_provider_plan(
        account,
        plan,
        policy,
        ProviderAdapterKind::MiniMaxCn,
        "MiniMax CN Token Plan",
        minimax_cn_transport(plan.upstream)?,
    )
}

fn resolve_ollama_cloud(
    account: &Account,
    _config: &AppConfig,
    plan: &RequestPlan,
    policy: RoutePolicy,
) -> Result<AttemptSpec, String> {
    resolve_fixed_provider_plan(
        account,
        plan,
        policy,
        ProviderAdapterKind::OllamaCloud,
        "Ollama Cloud",
        ollama_cloud_transport(&account.id, plan.upstream)?,
    )
}

fn resolve_kimi_cn(
    account: &Account,
    _config: &AppConfig,
    plan: &RequestPlan,
    policy: RoutePolicy,
) -> Result<AttemptSpec, String> {
    resolve_fixed_provider_plan(
        account,
        plan,
        policy,
        ProviderAdapterKind::KimiCn,
        "Kimi Code CN",
        kimi_cn_transport(plan.upstream)?,
    )
}

/// Account tests already chose the endpoint and auth. This builds transport
/// from that route and does not look the mapping up again.
fn resolve_prepared_http(account: &Account, plan: &RequestPlan) -> Result<AttemptSpec, String> {
    if plan.channel != UpstreamChannel::Go {
        return Err("Custom API does not serve the Zen free channel".to_string());
    }
    let custom = plan.custom_route.as_ref().ok_or_else(|| {
        "Custom API account is missing a persisted endpoint URL and upstream protocol".to_string()
    })?;
    if custom.auth_kind.requires_key() && account.key_cipher.trim().is_empty() {
        return Err(format!("account `{}` has no stored Key", account.name));
    }
    Ok(
        configurable_http_transport(&custom.endpoint_url, custom.auth_kind, plan.upstream)?
            .into_spec(
                plan.upstream,
                http_credential(&account.id, custom.auth_kind),
            ),
    )
}

fn resolve_configurable_http(
    account: &Account,
    _config: &AppConfig,
    plan: &RequestPlan,
    _policy: RoutePolicy,
) -> Result<AttemptSpec, String> {
    let descriptor = registered_descriptor(ProviderAdapterKind::ConfigurableHttp, account)?;
    require_binding(
        account,
        descriptor.inference.credential_kind,
        descriptor.inference.quota_scope,
    )?;
    if plan.channel != UpstreamChannel::Go {
        return Err("Custom API does not serve the Zen free channel".to_string());
    }
    let custom = plan.custom_route.as_ref().ok_or_else(|| {
        "Custom API account is missing a persisted endpoint URL and upstream protocol".to_string()
    })?;
    Ok(
        configurable_http_transport(&custom.endpoint_url, custom.auth_kind, plan.upstream)?
            .into_spec(
                plan.upstream,
                http_credential(&account.id, custom.auth_kind),
            ),
    )
}

fn resolve_cpa(
    _account: &Account,
    _config: &AppConfig,
    _plan: &RequestPlan,
    _policy: RoutePolicy,
) -> Result<AttemptSpec, String> {
    Err("CPA protocol probes and account tests are not available".to_string())
}

fn sealed_descriptor(
    kind: ProviderAdapterKind,
) -> Result<crate::provider::ProviderDescriptor, String> {
    ProviderRegistry::get_by_kind(kind).ok_or_else(|| format!("unsupported adapter `{kind:?}`"))
}

fn registered_descriptor(
    expected: ProviderAdapterKind,
    account: &Account,
) -> Result<crate::provider::ProviderDescriptor, String> {
    let descriptor = ProviderRegistry::get(&account.provider_id).ok_or_else(|| {
        format!(
            "unsupported provider offering `{}/{}`",
            account.provider_id, account.provider_id
        )
    })?;
    if descriptor.kind != expected {
        return Err(format!(
            "unsupported provider offering `{}/{}`",
            account.provider_id, account.provider_id
        ));
    }
    Ok(descriptor)
}

fn credential_handle(
    account: &Account,
    descriptor: crate::provider::ProviderDescriptor,
) -> CredentialHandle {
    match descriptor.inference.credential_kind {
        CredentialKind::ApiKey => CredentialHandle::Account {
            id: account.id.clone(),
        },
        CredentialKind::None => CredentialHandle::None,
    }
}

fn require_opencode_protocol_policy(
    descriptor: crate::provider::ProviderDescriptor,
    account: &Account,
    plan: &RequestPlan,
    policy: RoutePolicy,
    label: &str,
) -> Result<(), String> {
    let protocol = protocol_kind_for(plan.upstream)?;
    let ceiling =
        crate::provider_contracts::safety_ceiling_protocols(descriptor.protocol_probe, &plan.model);
    let static_verified =
        crate::provider_contracts::static_verified_protocols(descriptor.kind, &plan.model, &[]);
    match policy {
        RoutePolicy::Probe => {
            // Dashboard V3 admits probe requests only for models present in
            // the selected provider's effective catalog. Once admitted, an
            // explicit admin probe must reach every constructible protocol
            // endpoint; the static model table is evidence, not an admission
            // gate for freshly fetched catalog models.
            let _ = (protocol, ceiling, label, account);
            Ok(())
        }
        RoutePolicy::AccountTest => {
            let opencode_ok = matches!(
                descriptor.kind,
                ProviderAdapterKind::OpenCodeGo | ProviderAdapterKind::ZenFree
            ) && opencode_supports_upstream(&plan.model, plan.upstream);
            let statically_ok = static_verified.contains(&protocol)
                || opencode_ok
                || (descriptor.kind == ProviderAdapterKind::CommandCodeGoat
                    && command_code_supports_upstream(&plan.model, plan.upstream))
                || (descriptor.kind == ProviderAdapterKind::OllamaCloud
                    && crate::kernel::protocol::ollama_cloud_supports_upstream(
                        &plan.model,
                        plan.upstream,
                    ))
                || (descriptor.kind == ProviderAdapterKind::ZenFree
                    && !crate::gateway::protocol::is_known_model(&plan.model)
                    && plan.model.ends_with("-free")
                    && plan.upstream == ApiFormat::ChatCompletions);
            // Forwarder has no request-scoped contract. Static MODEL_PROTOCOLS
            // remains the default policy; ceiling-only extras are still
            // constructable so a probe-confirmed plan selected by materialize
            // can be forwarded. Anything outside the ceiling is rejected.
            if statically_ok || ceiling.contains(&protocol) {
                Ok(())
            } else {
                Err(format!(
                    "{label} has no verified support for model `{}` over {:?}",
                    plan.model, plan.upstream
                ))
            }
        }
    }
}

fn protocol_kind_for(upstream: ApiFormat) -> Result<crate::provider::UpstreamProtocolKind, String> {
    match upstream {
        ApiFormat::ChatCompletions => Ok(crate::provider::UpstreamProtocolKind::ChatCompletions),
        ApiFormat::Responses => Ok(crate::provider::UpstreamProtocolKind::Responses),
        ApiFormat::Messages => Ok(crate::provider::UpstreamProtocolKind::Messages),
        ApiFormat::Gemini => Err("Gemini is a client-only protocol".to_string()),
    }
}

fn opencode_upstream_path(upstream: ApiFormat) -> Result<String, String> {
    upstream
        .upstream_path()
        .map(str::to_string)
        .ok_or_else(|| "Gemini is a client-only protocol".to_string())
}

fn require_binding(
    account: &Account,
    credential_kind: CredentialKind,
    quota_scope: QuotaScope,
) -> Result<(), String> {
    if account.credential_kind != credential_kind || account.quota_scope != quota_scope {
        return Err(format!(
            "provider binding mismatch for account `{}`",
            account.id
        ));
    }
    Ok(())
}

#[cfg(any(debug_assertions, feature = "ollama-cloud-loopback-test"))]
fn ensure_loopback_base(base_url: &str) -> Result<(), String> {
    let url = reqwest::Url::parse(base_url).map_err(|error| error.to_string())?;
    if url.scheme() != "http"
        || !matches!(
            url.host_str(),
            Some("localhost") | Some("127.0.0.1") | Some("::1") | Some("[::1]")
        )
    {
        return Err("test route must be an HTTP loopback URL".to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests;

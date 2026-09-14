//! Provider offering adapters: endpoint, auth, and capability checks.
//!
//! Authentication belongs to the provider/offering, not the wire protocol.
//! [`resolve_route_with_dynamics`] dispatches exhaustively on
//! [`crate::provider::ProviderAdapterKind`] onto sealed route helpers. Alias resolution
//! stays ahead of this seam: Alias and PinnedRaw candidates both materialize a
//! [`RequestPlan`] then call here. Adapters must not probe a billable inference
//! path to discover protocol support.
//!
//! Route resolution returns a data-only [`crate::gateway::attempt::AttemptSpec`]:
//! endpoint, path, upstream protocol, auth scheme, redirect policy, an opaque
//! credential handle, and the proxy-routing model. Adapters take an account,
//! config, and request plan. They do not decrypt keys, open databases, or
//! build HTTP clients; the Host resolver and single-attempt executor do that.
//!
//! Production Command Code GOAT uses the official Provider API origin after
//! explicit verification. [`command_code_goat_transport_spec`] proves
//! host/path/auth construction. The GOAT loopback helper substitutes a
//! loopback origin only and still uses `/provider/v1/...`.
//! Configurable HTTP is the Custom API identity, not a base class.

use crate::custom_http::{custom_auth_scheme, join_inference_endpoint, resolve_custom_endpoints};
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
    COMMAND_CODE_GOAT_MESSAGES_PATH, COMMAND_CODE_GOAT_MODELS_PATH, CPA_ACCOUNT_ID, CredentialKind,
    InferenceAuthDescriptor, KIMI_CN_BASE_URL, KIMI_CN_CHAT_COMPLETIONS_PATH,
    KIMI_CN_MESSAGES_PATH, MINIMAX_CN_ANTHROPIC_BASE_URL, MINIMAX_CN_BASE_URL,
    MINIMAX_CN_CHAT_COMPLETIONS_PATH, MINIMAX_CN_MESSAGES_PATH, ProviderAdapterKind,
    ProviderRegistry, QuotaScope, UpstreamAuthScheme, ZEN_FREE_ACCOUNT_ID,
};
use crate::provider_contracts::EffectiveContractSet;
use std::collections::HashMap;
use std::sync::{LazyLock, RwLock};

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

#[derive(Debug, Clone)]
struct GoatLoopbackRoute {
    origin: String,
}

static GOAT_LOOPBACK_ROUTES: LazyLock<RwLock<HashMap<String, GoatLoopbackRoute>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

static OLLAMA_LOOPBACK_ROUTES: LazyLock<RwLock<HashMap<String, String>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

/// RAII guard for the integration-only Ollama Cloud seam. The production
/// adapter always uses the fixed `https://ollama.com` origin; without a live
/// guard, tests cannot reach a fake upstream.
#[doc(hidden)]
pub struct OllamaCloudLoopbackRouteGuard {
    account_id: String,
    origin: String,
}

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

fn ollama_cloud_base_url_for(account: &Account) -> String {
    OLLAMA_LOOPBACK_ROUTES
        .read()
        .map(|routes| {
            routes
                .get(&account.id)
                .cloned()
                .unwrap_or_else(|| OLLAMA_CLOUD_BASE_URL.to_string())
        })
        .unwrap_or_else(|_| OLLAMA_CLOUD_BASE_URL.to_string())
}

#[cfg(debug_assertions)]
#[doc(hidden)]
pub use crate::goat::{GoatCatalogOriginGuard, install_goat_catalog_origin_for_test};

/// RAII guard for the integration-only GOAT seam. The production adapter has
/// no endpoint or protocol guesses: without a live guard, GOAT is unsupported.
#[doc(hidden)]
pub struct GoatLoopbackRouteGuard {
    account_id: String,
    base_url: String,
}

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

#[derive(Clone, Copy)]
enum RoutePolicy<'a> {
    /// Production inference. When `contracts` is present, the effective
    /// contract (static/preset/probe-confirmed + switches) is required.
    /// The forwarder keeps the historical three-argument signature and
    /// still refuses protocols outside the adapter safety ceiling.
    Production {
        contracts: Option<&'a EffectiveContractSet>,
    },
    /// Explicit admin probe: validate the structural ceiling and construct
    /// the endpoint/auth path without requiring prior verified support.
    Probe,
    /// Account-scoped operational test. This keeps the production route shape
    /// (including GOAT and CN routes) while deliberately bypassing normal
    /// availability selection: the dashboard has already locked one account.
    AccountTest,
}

pub(crate) fn supports_production_plan(
    account: &Account,
    config: &AppConfig,
    plan: &RequestPlan,
    contracts: &EffectiveContractSet,
    dynamics: &[crate::dynamic::DynamicProviderRuntime],
) -> Result<(), String> {
    resolve_route_with_policy(
        account,
        config,
        plan,
        RoutePolicy::Production {
            contracts: Some(contracts),
        },
        dynamics,
    )
    .map(|_| ())
}

pub(crate) fn resolve_route_with_dynamics(
    account: &Account,
    config: &AppConfig,
    plan: &RequestPlan,
    dynamics: &[crate::dynamic::DynamicProviderRuntime],
) -> Result<AttemptSpec, String> {
    resolve_route_with_policy(
        account,
        config,
        plan,
        RoutePolicy::Production { contracts: None },
        dynamics,
    )
}

pub(crate) fn resolve_probe_route(
    account: &Account,
    config: &AppConfig,
    plan: &RequestPlan,
) -> Result<AttemptSpec, String> {
    resolve_route_with_policy(account, config, plan, RoutePolicy::Probe, &[])
}

pub(crate) fn resolve_account_test_route_with_dynamics(
    account: &Account,
    config: &AppConfig,
    plan: &RequestPlan,
    dynamics: &[crate::dynamic::DynamicProviderRuntime],
) -> Result<AttemptSpec, String> {
    resolve_route_with_policy(account, config, plan, RoutePolicy::AccountTest, dynamics)
}

fn resolve_route_with_policy(
    account: &Account,
    config: &AppConfig,
    plan: &RequestPlan,
    policy: RoutePolicy<'_>,
    dynamics: &[crate::dynamic::DynamicProviderRuntime],
) -> Result<AttemptSpec, String> {
    match crate::dynamic::adapter_kind_for(&account.provider_id, dynamics) {
        Some(ProviderAdapterKind::OpenCodeGo) => {
            resolve_open_code_go(account, config, plan, policy)
        }
        Some(ProviderAdapterKind::ZenFree) => resolve_zen_free(account, config, plan, policy),
        Some(ProviderAdapterKind::CommandCodeGoat) => {
            resolve_command_code_goat(account, config, plan, policy)
        }
        Some(ProviderAdapterKind::MiniMaxCn) => resolve_minimax_cn(account, config, plan, policy),
        Some(ProviderAdapterKind::KimiCn) => resolve_kimi_cn(account, config, plan, policy),
        Some(ProviderAdapterKind::OllamaCloud) => {
            resolve_ollama_cloud(account, config, plan, policy)
        }
        Some(ProviderAdapterKind::ConfigurableHttp)
            if crate::provider::is_custom_api(&account.provider_id) =>
        {
            resolve_configurable_http(account, config, plan, policy)
        }
        Some(ProviderAdapterKind::ConfigurableHttp) => {
            resolve_dynamic_http(account, plan, policy, dynamics)
        }
        Some(ProviderAdapterKind::Cpa) => resolve_cpa(account, config, plan, policy),
        None => Err(format!(
            "unsupported provider offering `{}/{}`",
            account.provider_id, account.provider_id
        )),
    }
}

fn resolve_open_code_go(
    account: &Account,
    config: &AppConfig,
    plan: &RequestPlan,
    policy: RoutePolicy<'_>,
) -> Result<AttemptSpec, String> {
    let descriptor = registered_descriptor(ProviderAdapterKind::OpenCodeGo, account)?;
    require_binding(
        account,
        descriptor.inference.credential_kind,
        descriptor.inference.quota_scope,
    )?;
    if plan.channel != UpstreamChannel::Go {
        return Err("OpenCode Go does not serve the Zen free channel".to_string());
    }
    require_opencode_protocol_policy(descriptor, account, plan, policy, "OpenCode Go")?;
    Ok(AttemptSpec {
        base_url: crate::gateway::free_models::opencode_go_base_url(&config.upstream_base_url),
        path: opencode_upstream_path(plan.upstream)?,
        upstream: plan.upstream,
        auth: descriptor_auth(descriptor.inference.auth)?,
        follow_redirects: descriptor.inference.follow_redirects,
        credential: credential_handle(account, descriptor),
        proxy_routing: ProxyRoutingModel::RequestEntrySnapshot,
        wire_normalization: WireNormalization::None,
    })
}

fn resolve_zen_free(
    account: &Account,
    config: &AppConfig,
    plan: &RequestPlan,
    policy: RoutePolicy<'_>,
) -> Result<AttemptSpec, String> {
    let descriptor = registered_descriptor(ProviderAdapterKind::ZenFree, account)?;
    require_binding(
        account,
        descriptor.inference.credential_kind,
        descriptor.inference.quota_scope,
    )?;
    if account.id != ZEN_FREE_ACCOUNT_ID {
        return Err("Zen Free route must use the reserved singleton account".to_string());
    }
    if plan.channel != UpstreamChannel::Free {
        return Err(format!(
            "Zen Free does not support routed model `{}` on this channel",
            plan.model
        ));
    }
    require_opencode_protocol_policy(descriptor, account, plan, policy, "Zen Free")?;
    let base_url = plan.upstream_base_override.clone().map_or_else(
        || resolve_upstream_base(UpstreamChannel::Free, &config.upstream_base_url),
        Ok,
    )?;
    Ok(AttemptSpec {
        base_url,
        path: opencode_upstream_path(plan.upstream)?,
        upstream: plan.upstream,
        auth: descriptor_auth(descriptor.inference.auth)?,
        follow_redirects: descriptor.inference.follow_redirects,
        credential: credential_handle(account, descriptor),
        proxy_routing: ProxyRoutingModel::RequestEntrySnapshot,
        wire_normalization: WireNormalization::None,
    })
}

fn resolve_command_code_goat(
    account: &Account,
    _config: &AppConfig,
    plan: &RequestPlan,
    policy: RoutePolicy<'_>,
) -> Result<AttemptSpec, String> {
    let descriptor = registered_descriptor(ProviderAdapterKind::CommandCodeGoat, account)?;
    require_binding(
        account,
        descriptor.inference.credential_kind,
        descriptor.inference.quota_scope,
    )?;
    if plan.channel != UpstreamChannel::Go {
        return Err("Command Code GOAT does not serve the Zen free channel".to_string());
    }
    if !command_code_supports_upstream(&plan.model, plan.upstream) {
        return Err(format!(
            "Command Code GOAT has no verified support for model `{}` over {:?}",
            plan.model, plan.upstream
        ));
    }
    require_opencode_protocol_policy(descriptor, account, plan, policy, "Command Code GOAT")?;
    let path = command_code_upstream_path(plan.upstream).ok_or_else(|| {
        format!(
            "Command Code GOAT has no upstream path for {:?}",
            plan.upstream
        )
    })?;
    let routes = GOAT_LOOPBACK_ROUTES
        .read()
        .map_err(|_| "GOAT loopback route lock is poisoned".to_string())?;
    let base_url = routes.get(&account.id).map_or_else(
        || COMMAND_CODE_GOAT_BASE_URL.to_string(),
        |route| command_code_goat_loopback_base(&route.origin),
    );
    Ok(AttemptSpec {
        base_url,
        path: path.to_string(),
        upstream: plan.upstream,
        auth: descriptor_auth(descriptor.inference.auth)?,
        follow_redirects: descriptor.inference.follow_redirects,
        credential: credential_handle(account, descriptor),
        proxy_routing: ProxyRoutingModel::ProcessWideNoRedirect,
        wire_normalization: WireNormalization::None,
    })
}

fn resolve_fixed_provider_plan(
    account: &Account,
    plan: &RequestPlan,
    policy: RoutePolicy<'_>,
    adapter: ProviderAdapterKind,
    label: &str,
    base_url: &str,
    path: &str,
) -> Result<AttemptSpec, String> {
    let descriptor = registered_descriptor(adapter, account)?;
    require_binding(
        account,
        descriptor.inference.credential_kind,
        descriptor.inference.quota_scope,
    )?;
    if plan.channel != UpstreamChannel::Go {
        return Err(format!("{label} does not serve the Zen free channel"));
    }
    require_opencode_protocol_policy(descriptor, account, plan, policy, label)?;
    Ok(AttemptSpec {
        base_url: base_url.to_string(),
        path: path.to_string(),
        upstream: plan.upstream,
        auth: descriptor_auth(descriptor.inference.auth)?,
        follow_redirects: descriptor.inference.follow_redirects,
        credential: credential_handle(account, descriptor),
        proxy_routing: ProxyRoutingModel::ProcessWideNoRedirect,
        wire_normalization: WireNormalization::None,
    })
}

fn resolve_minimax_cn(
    account: &Account,
    _config: &AppConfig,
    plan: &RequestPlan,
    policy: RoutePolicy<'_>,
) -> Result<AttemptSpec, String> {
    let (base_url, path) = match plan.upstream {
        ApiFormat::ChatCompletions => (MINIMAX_CN_BASE_URL, MINIMAX_CN_CHAT_COMPLETIONS_PATH),
        ApiFormat::Messages => (MINIMAX_CN_ANTHROPIC_BASE_URL, MINIMAX_CN_MESSAGES_PATH),
        ApiFormat::Responses | ApiFormat::Gemini => {
            return Err(
                "MiniMax CN Token Plan has no official upstream path for this protocol".into(),
            );
        }
    };
    resolve_fixed_provider_plan(
        account,
        plan,
        policy,
        ProviderAdapterKind::MiniMaxCn,
        "MiniMax CN Token Plan",
        base_url,
        path,
    )
}

fn resolve_ollama_cloud(
    account: &Account,
    _config: &AppConfig,
    plan: &RequestPlan,
    policy: RoutePolicy<'_>,
) -> Result<AttemptSpec, String> {
    if plan.upstream != ApiFormat::ChatCompletions {
        return Err("Ollama Cloud has no official upstream path for this protocol".into());
    }
    let mut spec = resolve_fixed_provider_plan(
        account,
        plan,
        policy,
        ProviderAdapterKind::OllamaCloud,
        "Ollama Cloud",
        &ollama_cloud_base_url_for(account),
        OLLAMA_CLOUD_CHAT_COMPLETIONS_PATH,
    )?;
    spec.wire_normalization = WireNormalization::OllamaCloud;
    Ok(spec)
}

fn resolve_kimi_cn(
    account: &Account,
    _config: &AppConfig,
    plan: &RequestPlan,
    policy: RoutePolicy<'_>,
) -> Result<AttemptSpec, String> {
    let path = match plan.upstream {
        ApiFormat::ChatCompletions => KIMI_CN_CHAT_COMPLETIONS_PATH,
        ApiFormat::Messages => KIMI_CN_MESSAGES_PATH,
        ApiFormat::Responses | ApiFormat::Gemini => {
            return Err("Kimi Code CN has no official upstream path for this protocol".into());
        }
    };
    resolve_fixed_provider_plan(
        account,
        plan,
        policy,
        ProviderAdapterKind::KimiCn,
        "Kimi Code CN",
        KIMI_CN_BASE_URL,
        path,
    )
}

fn resolve_configurable_http(
    account: &Account,
    _config: &AppConfig,
    plan: &RequestPlan,
    policy: RoutePolicy<'_>,
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
    let protocol = protocol_kind_for(plan.upstream)?;
    if let RoutePolicy::Production {
        contracts: Some(contracts),
    } = policy
        && !contracts.production_protocol_allowed(
            account,
            plan.resolved_alias.as_deref().unwrap_or(&plan.model),
            protocol,
        )
    {
        return Err(format!(
            "Custom API has no verified support for public model `{}` over {:?}",
            plan.resolved_alias.as_deref().unwrap_or(&plan.model),
            plan.upstream
        ));
    }
    let endpoint = resolve_custom_endpoints(&custom.endpoint_url, protocol)
        .map_err(|error| error.to_string())?
        .inference;
    let endpoint_path = endpoint.path().to_string();
    let mut base = endpoint;
    base.set_path("");
    base.set_query(None);
    base.set_fragment(None);
    Ok(AttemptSpec {
        base_url: base.as_str().trim_end_matches('/').to_string(),
        path: endpoint_path,
        upstream: plan.upstream,
        auth: match custom_auth_scheme(protocol) {
            UpstreamAuthScheme::Bearer => UpstreamAuth::Bearer,
            UpstreamAuthScheme::XApiKey => UpstreamAuth::XApiKey,
        },
        follow_redirects: descriptor.inference.follow_redirects,
        credential: credential_handle(account, descriptor),
        proxy_routing: ProxyRoutingModel::IsolatedTrustedAdmin,
        wire_normalization: WireNormalization::None,
    })
}

fn resolve_dynamic_http(
    account: &Account,
    plan: &RequestPlan,
    policy: RoutePolicy<'_>,
    dynamics: &[crate::dynamic::DynamicProviderRuntime],
) -> Result<AttemptSpec, String> {
    let runtime =
        crate::dynamic::find_runtime(dynamics, &account.provider_id).ok_or_else(|| {
            format!(
                "dynamic provider `{}` is not in the request snapshot",
                account.provider_id
            )
        })?;
    if matches!(policy, RoutePolicy::Probe) {
        return Err("dynamic provider protocol probes use POST /providers/test".to_string());
    }
    if runtime.auth_kind.requires_key() && account.key_cipher.trim().is_empty() {
        return Err(format!("account `{}` has no stored Key", account.name));
    }
    let selected = runtime
        .mapping_for_upstream(&plan.model)
        .or_else(|| runtime.mapping_for_public(&plan.model))
        .or_else(|| {
            plan.resolved_alias
                .as_deref()
                .and_then(|alias| runtime.mapping_for_public(alias))
        })
        .ok_or_else(|| {
            format!(
                "dynamic provider `{}` has no mapping for `{}`",
                runtime.name, plan.model
            )
        })?;
    let route = runtime.effective_route(selected);
    let protocol = protocol_kind_for(plan.upstream)?;
    if protocol != route.protocol {
        return Err(format!(
            "dynamic provider `{}` model `{}` only supports {:?}",
            runtime.name, selected.public_model, route.protocol
        ));
    }
    let endpoint = resolve_custom_endpoints(&route.endpoint_url, protocol)
        .map_err(|error| error.to_string())?
        .inference;
    let endpoint_path = endpoint.path().to_string();
    let mut base = endpoint;
    base.set_path("");
    base.set_query(None);
    base.set_fragment(None);
    let auth = match runtime.auth_kind {
        ocg_domain::dynamic::DynamicAuthKind::Bearer => UpstreamAuth::Bearer,
        ocg_domain::dynamic::DynamicAuthKind::XApiKey => UpstreamAuth::XApiKey,
        ocg_domain::dynamic::DynamicAuthKind::None => UpstreamAuth::None,
    };
    Ok(AttemptSpec {
        base_url: base.as_str().trim_end_matches('/').to_string(),
        path: endpoint_path,
        upstream: plan.upstream,
        auth,
        follow_redirects: false,
        credential: if runtime.auth_kind.requires_key() {
            CredentialHandle::Account {
                id: account.id.clone(),
            }
        } else {
            CredentialHandle::None
        },
        proxy_routing: ProxyRoutingModel::IsolatedTrustedAdmin,
        wire_normalization: WireNormalization::None,
    })
}

fn resolve_cpa(
    account: &Account,
    _config: &AppConfig,
    plan: &RequestPlan,
    policy: RoutePolicy<'_>,
) -> Result<AttemptSpec, String> {
    if matches!(policy, RoutePolicy::Probe | RoutePolicy::AccountTest) {
        return Err("CPA protocol probes and account tests are not available".to_string());
    }
    let descriptor = registered_descriptor(ProviderAdapterKind::Cpa, account)?;
    require_binding(
        account,
        descriptor.inference.credential_kind,
        descriptor.inference.quota_scope,
    )?;
    if account.id != CPA_ACCOUNT_ID {
        return Err("CPA route must use the reserved singleton account".to_string());
    }
    if plan.channel != UpstreamChannel::Go {
        return Err("CPA does not serve the Zen free channel".to_string());
    }
    let path = plan
        .upstream
        .upstream_path()
        .ok_or_else(|| "CPA has no native Gemini inference path".to_string())?;
    let base_url = plan
        .upstream_base_override
        .clone()
        .ok_or_else(|| "CPA is not configured".to_string())?;
    crate::cpa::normalize_base_url(&base_url, true).map_err(|error| error.to_string())?;
    Ok(AttemptSpec {
        base_url,
        path: path.to_string(),
        upstream: plan.upstream,
        auth: UpstreamAuth::Bearer,
        follow_redirects: false,
        credential: credential_handle(account, descriptor),
        proxy_routing: ProxyRoutingModel::LocalExternalIntegration,
        wire_normalization: WireNormalization::None,
    })
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

fn descriptor_auth(auth: InferenceAuthDescriptor) -> Result<UpstreamAuth, String> {
    match auth {
        InferenceAuthDescriptor::OpenCodeProtocolDefault => {
            Ok(UpstreamAuth::OpenCodeProtocolDefault)
        }
        InferenceAuthDescriptor::Bearer => Ok(UpstreamAuth::Bearer),
        InferenceAuthDescriptor::None => Ok(UpstreamAuth::None),
        InferenceAuthDescriptor::ProtocolDerivedBearerOrXApiKey => {
            Err("Configurable HTTP auth is derived from the account protocol".to_string())
        }
    }
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
    policy: RoutePolicy<'_>,
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
            let _ = (protocol, ceiling, label);
            Ok(())
        }
        RoutePolicy::Production {
            contracts: Some(contracts),
        } => {
            if !contracts.production_protocol_allowed(account, &plan.model, protocol) {
                return Err(format!(
                    "{label} has no verified support for model `{}` over {:?}",
                    plan.model, plan.upstream
                ));
            }
            Ok(())
        }
        RoutePolicy::AccountTest | RoutePolicy::Production { contracts: None } => {
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

fn ensure_loopback_base(base_url: &str) -> Result<(), String> {
    let url = reqwest::Url::parse(base_url).map_err(|error| error.to_string())?;
    if url.scheme() != "http"
        || !matches!(
            url.host_str(),
            Some("localhost") | Some("127.0.0.1") | Some("::1") | Some("[::1]")
        )
    {
        return Err("GOAT test route must be an HTTP loopback URL".to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests;

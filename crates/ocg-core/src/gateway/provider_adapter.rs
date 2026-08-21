//! Provider offering adapters: endpoint, auth, and capability checks.
//!
//! Authentication belongs to the provider/offering, not the wire protocol.
//! [`resolve_route`] is the seam later GOAT / SCNet / Custom adapters should
//! implement. Alias and PinnedRaw candidates both materialize a [`RequestPlan`]
//! then call this seam. They must not probe a billable inference path to
//! discover protocol support.
//!
//! Production Command Code GOAT stays fail-closed here (catalog unroutable,
//! verification runtime unavailable). The official transport constants and
//! [`command_code_goat_transport_spec`] prove host/path/auth construction
//! without live network. The GOAT loopback helper substitutes a loopback
//! origin only and still uses `/provider/v1/chat/completions`.

use crate::gateway::free_models::{is_free_model, resolve_upstream_base};
use crate::gateway::protocol::{
    ApiFormat, RequestPlan, command_code_model_protocol, command_code_supports_upstream,
    command_code_upstream_path, opencode_supports_upstream,
};
use crate::models::{Account, AppConfig, UpstreamChannel};
use crate::provider::{
    ANONYMOUS_FREE_OFFERING_ID, COMMAND_CODE_GOAT_BASE_URL,
    COMMAND_CODE_GOAT_CHAT_COMPLETIONS_PATH, COMMAND_CODE_GOAT_HOST,
    COMMAND_CODE_GOAT_MESSAGES_PATH, COMMAND_CODE_GOAT_MODELS_PATH, COMMAND_CODE_PROVIDER_ID,
    CredentialKind, GO_OFFERING_ID, GOAT_OFFERING_ID, OPENCODE_PROVIDER_ID,
    OPENCODE_ZEN_FREE_PROVIDER_ID, QuotaScope, UpstreamAuthScheme, ZEN_FREE_ACCOUNT_ID,
};
use std::collections::HashMap;
use std::sync::{LazyLock, RwLock};

/// Authentication belongs to the provider/offering adapter, not to the wire
/// protocol. In particular, a Messages endpoint does not imply `x-api-key`
/// for every future provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UpstreamAuth {
    OpenCodeProtocolDefault,
    Bearer,
    XApiKey,
    None,
}

#[derive(Debug, Clone)]
pub(crate) struct ResolvedProviderRoute {
    pub base_url: String,
    pub path: String,
    pub upstream: ApiFormat,
    pub auth: UpstreamAuth,
    pub credential_account_id: Option<String>,
    pub follow_redirects: bool,
}

/// Deterministic official Command Code GOAT transport. Used by tests and the
/// loopback origin substitute; production `resolve_route` still fail-closes
/// without a loopback guard so no live account is selected.
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
    pub uses_get_models_for_verification: bool,
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
        uses_get_models_for_verification: false,
    }
}

pub fn command_code_goat_join_url(base: &str, upstream: ApiFormat) -> Result<String, String> {
    let path = command_code_upstream_path(upstream)
        .ok_or_else(|| format!("Command Code GOAT has no upstream path for {upstream:?}"))?;
    Ok(format!("{}{}", base.trim_end_matches('/'), path))
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

pub(crate) fn supports_plan(
    account: &Account,
    config: &AppConfig,
    plan: &RequestPlan,
) -> Result<(), String> {
    resolve_route(account, config, plan).map(|_| ())
}

pub(crate) fn resolve_route(
    account: &Account,
    config: &AppConfig,
    plan: &RequestPlan,
) -> Result<ResolvedProviderRoute, String> {
    match (account.provider_id.as_str(), account.offering_id.as_str()) {
        (OPENCODE_PROVIDER_ID, GO_OFFERING_ID) => {
            require_binding(account, CredentialKind::ApiKey, QuotaScope::Key)?;
            if plan.channel != UpstreamChannel::Go {
                return Err("OpenCode Go does not serve the Zen free channel".to_string());
            }
            if !opencode_supports_upstream(&plan.model, plan.upstream) {
                return Err(format!(
                    "OpenCode Go has no verified support for model `{}` over {:?}",
                    plan.model, plan.upstream
                ));
            }
            Ok(ResolvedProviderRoute {
                base_url: config.upstream_base_url.trim_end_matches('/').to_string(),
                path: opencode_upstream_path(plan.upstream)?,
                upstream: plan.upstream,
                auth: UpstreamAuth::OpenCodeProtocolDefault,
                credential_account_id: Some(account.id.clone()),
                follow_redirects: true,
            })
        }
        (OPENCODE_ZEN_FREE_PROVIDER_ID, ANONYMOUS_FREE_OFFERING_ID) => {
            require_binding(account, CredentialKind::None, QuotaScope::EgressIp)?;
            if account.id != ZEN_FREE_ACCOUNT_ID {
                return Err("Zen Free route must use the reserved singleton account".to_string());
            }
            if plan.channel != UpstreamChannel::Free || !is_free_model(&plan.model) {
                return Err(format!(
                    "Zen Free does not support routed model `{}` on this channel",
                    plan.model
                ));
            }
            if !opencode_supports_upstream(&plan.model, plan.upstream) {
                return Err(format!(
                    "Zen Free has no verified support for model `{}` over {:?}",
                    plan.model, plan.upstream
                ));
            }
            let base_url = plan.upstream_base_override.clone().map_or_else(
                || resolve_upstream_base(UpstreamChannel::Free, &config.upstream_base_url),
                Ok,
            )?;
            Ok(ResolvedProviderRoute {
                base_url,
                path: opencode_upstream_path(plan.upstream)?,
                upstream: plan.upstream,
                auth: UpstreamAuth::None,
                credential_account_id: None,
                follow_redirects: true,
            })
        }
        (COMMAND_CODE_PROVIDER_ID, GOAT_OFFERING_ID) => {
            require_binding(account, CredentialKind::ApiKey, QuotaScope::Key)?;
            if plan.channel != UpstreamChannel::Go {
                return Err("Command Code GOAT does not serve the Zen free channel".to_string());
            }
            if command_code_model_protocol(&plan.model).is_none()
                || !command_code_supports_upstream(&plan.model, plan.upstream)
            {
                return Err(format!(
                    "Command Code GOAT has no verified support for model `{}` over {:?}",
                    plan.model, plan.upstream
                ));
            }
            let path = command_code_upstream_path(plan.upstream).ok_or_else(|| {
                format!(
                    "Command Code GOAT has no upstream path for {:?}",
                    plan.upstream
                )
            })?;
            let routes = GOAT_LOOPBACK_ROUTES
                .read()
                .map_err(|_| "GOAT loopback route lock is poisoned".to_string())?;
            let route = routes.get(&account.id).ok_or_else(|| {
                "Command Code GOAT production inference endpoint, auth, protocol, and model catalog are not verified; route is disabled"
                    .to_string()
            })?;
            Ok(ResolvedProviderRoute {
                base_url: command_code_goat_loopback_base(&route.origin),
                path: path.to_string(),
                upstream: plan.upstream,
                auth: UpstreamAuth::Bearer,
                credential_account_id: Some(account.id.clone()),
                follow_redirects: false,
            })
        }
        _ => Err(format!(
            "unsupported provider offering `{}/{}`",
            account.provider_id, account.offering_id
        )),
    }
}

pub(crate) fn supports_model_discovery(account: &Account) -> bool {
    account.provider_id == OPENCODE_PROVIDER_ID
        && account.offering_id == GO_OFFERING_ID
        && account.credential_kind == CredentialKind::ApiKey
        && account.quota_scope == QuotaScope::Key
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
mod tests {
    use super::*;

    #[test]
    fn official_transport_is_fixed_bearer_chat_without_redirects_or_zdr() {
        let spec = command_code_goat_transport_spec();
        assert_eq!(spec.base_url, "https://api.commandcode.ai/provider/v1");
        assert_eq!(spec.host, "api.commandcode.ai");
        assert_eq!(spec.chat_completions_path, "/chat/completions");
        assert_eq!(spec.messages_path, "/messages");
        assert_eq!(spec.models_path, "/models");
        assert_eq!(spec.auth_scheme, UpstreamAuthScheme::Bearer);
        assert!(!spec.follow_redirects);
        assert_eq!(spec.zdr_header_name, None);
        assert!(!spec.uses_get_models_for_verification);
        assert_eq!(
            command_code_goat_official_url(ApiFormat::ChatCompletions).unwrap(),
            "https://api.commandcode.ai/provider/v1/chat/completions"
        );
        assert_eq!(
            command_code_goat_official_url(ApiFormat::Messages).unwrap(),
            "https://api.commandcode.ai/provider/v1/messages"
        );
        assert!(command_code_goat_official_url(ApiFormat::Responses).is_err());
        let loopback = command_code_goat_loopback_base("http://127.0.0.1:9");
        assert_eq!(loopback, "http://127.0.0.1:9/provider/v1");
        assert_eq!(
            command_code_goat_join_url(&loopback, ApiFormat::ChatCompletions).unwrap(),
            "http://127.0.0.1:9/provider/v1/chat/completions"
        );
        assert!(crate::provider::is_command_code_goat(
            COMMAND_CODE_PROVIDER_ID,
            GOAT_OFFERING_ID
        ));
        assert_eq!(
            crate::provider::COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_ALIAS,
            "deepseek-v4-flash"
        );
        assert_eq!(
            crate::provider::COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM,
            "deepseek/deepseek-v4-flash"
        );
    }
}

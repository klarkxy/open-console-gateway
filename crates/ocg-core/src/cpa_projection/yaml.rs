//! Canonical private config.yaml (projection-yaml-v1).
//!
//! One openai-compatibility entry per credential. Root `ocg` carries provenance
//! and ordered routes. The wire digest is the SHA-256 of these bytes and is not
//! written back into the document. Request-retry is omitted: it is not the
//! admit/result policy.

use super::digest::wire_digest;
use super::types::{ProductProjection, ProjectedAuth, ProjectionError, assigned_priorities};
use crate::models::{ProxyListDirection, ProxyMode, RoutingMode};
use serde::Serialize;
use std::collections::BTreeSet;

pub(crate) struct RenderedYaml {
    pub yaml: String,
    pub wire_digest: String,
}

impl std::fmt::Debug for RenderedYaml {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RenderedYaml")
            .field("yaml", &"[redacted]")
            .field("wire_digest", &self.wire_digest)
            .finish()
    }
}

pub(crate) fn render_canonical_yaml(
    projection: &ProductProjection,
) -> Result<RenderedYaml, ProjectionError> {
    let mut names = BTreeSet::new();
    let mut priorities = BTreeSet::new();
    let mut providers = Vec::with_capacity(projection.auths.len());
    let mut credentials = Vec::with_capacity(projection.auths.len());
    let proxy = key_proxy(projection);
    let proxy_list = list_proxy(projection)?;
    let assigned = assigned_priorities(projection)?;
    let share_tier = projection.routing_mode == RoutingMode::RoundRobin;
    for (auth, priority) in projection.auths.iter().zip(&assigned.api) {
        if !names.insert(auth.auth_id.as_str()) {
            return Err(ProjectionError::new(
                "duplicate_namespace",
                "two credentials share one namespace",
            ));
        }
        if !share_tier && !priorities.insert(*priority) {
            return Err(ProjectionError::new(
                "duplicate_priority",
                "two credentials share one priority",
            ));
        }
        providers.push(provider_entry(auth, *priority, proxy.clone())?);
        credentials.push(credential_entry(auth)?);
    }
    let mut bindings = Vec::with_capacity(projection.oauth_refs.len());
    for (item, priority) in projection.oauth_refs.iter().zip(&assigned.oauth) {
        if !share_tier && !priorities.insert(*priority) {
            return Err(ProjectionError::new(
                "duplicate_priority",
                "two credentials share one priority",
            ));
        }
        bindings.push(YamlOAuthBinding {
            relative_path: item.relative_path.clone(),
            auth_id: item.auth_id.clone(),
            credential_id: item.credential_id.clone(),
            credential_version: item.credential_version.to_string(),
            material_revision: item.material_revision.clone(),
            provider_id: item.product_provider_id.clone(),
            native_provider: oauth_provider_id(item.provider).to_string(),
            priority: *priority,
            models: item.models.clone(),
            native_route_fingerprint: native_route_fingerprint(projection, item)?,
        });
    }
    let document = YamlDocument {
        host: "127.0.0.1".to_string(),
        port: projection.listen_port,
        auth_dir: projection.auth_dir.clone(),
        debug: false,
        logging_to_file: false,
        remote_management: YamlRemoteManagement {
            allow_remote: false,
            secret_key: String::new(),
            disable_control_panel: true,
            disable_auto_update_panel: true,
        },
        api_keys: vec![projection.hop_secret.expose().to_string()],
        routing: YamlRouting {
            strategy: strategy(projection.routing_mode),
        },
        openai_compatibility: providers,
        ocg: YamlOcg {
            protocol_version: 1,
            process_generation: projection.process_generation.to_string(),
            projection_revision: projection.revisions.desired.to_string(),
            ready_key: projection.ready_key.expose().to_string(),
            policy: YamlPolicy {
                url: projection.policy_url.clone(),
                token: projection.policy_token.expose().to_string(),
                origin: projection.policy_origin.clone(),
            },
            routing: YamlOcgRouting {
                sticky_global: projection.routing_mode == RoutingMode::StickyGlobal,
                conversation_sticky: projection.conversation_sticky,
                conversation_ttl_seconds: projection.conversation_ttl_secs,
            },
            proxy_list,
            credentials,
            oauth_bindings: bindings,
        },
    };
    let yaml = serde_yaml_ng::to_string(&document).map_err(|_| {
        ProjectionError::new(
            "yaml_render_failed",
            "canonical CPA YAML could not be encoded",
        )
    })?;
    let digest = wire_digest(&yaml);
    Ok(RenderedYaml {
        yaml,
        wire_digest: digest,
    })
}

pub(crate) fn render_standard_yaml(
    projection: &ProductProjection,
) -> Result<String, ProjectionError> {
    Ok(render_canonical_yaml(projection)?.yaml)
}

fn strategy(mode: RoutingMode) -> &'static str {
    match mode {
        RoutingMode::RoundRobin => "round-robin",
        RoutingMode::StrictPriority | RoutingMode::StickyGlobal => "fill-first",
    }
}

fn provider_entry(
    auth: &ProjectedAuth,
    priority: i64,
    proxy_url: Option<String>,
) -> Result<YamlProvider, ProjectionError> {
    let mut seen: Vec<(&str, &str)> = Vec::new();
    let mut models = Vec::new();
    for model in &auth.models {
        if let Some((_, upstream)) = seen
            .iter()
            .find(|(alias, _)| *alias == model.public_alias.as_str())
        {
            if *upstream != model.upstream_name {
                return Err(ProjectionError::new(
                    "invalid_alias",
                    "one public alias maps to two upstream names",
                ));
            }
            continue;
        }
        seen.push((model.public_alias.as_str(), model.upstream_name.as_str()));
        models.push(YamlModel {
            name: model.upstream_name.clone(),
            alias: model.public_alias.clone(),
        });
    }
    if models.is_empty() {
        return Err(ProjectionError::new(
            "invalid_endpoint",
            "auth has no positive model",
        ));
    }
    let endpoint = auth
        .models
        .iter()
        .flat_map(|model| model.routes.iter())
        .find(|route| route.protocol == "chat_completions")
        .or_else(|| {
            auth.models
                .iter()
                .flat_map(|model| model.routes.iter())
                .next()
        })
        .map(|route| route.endpoint_url.clone())
        .ok_or_else(|| ProjectionError::new("invalid_endpoint", "auth has no route"))?;
    let api_key = encoded_api_key(auth)?;
    Ok(YamlProvider {
        name: auth.auth_id.clone(),
        priority,
        base_url: compat_base(&endpoint)?,
        api_key_entries: vec![YamlKey { api_key, proxy_url }],
        models,
    })
}

fn compat_base(endpoint: &str) -> Result<String, ProjectionError> {
    let mut url = reqwest::Url::parse(endpoint)
        .map_err(|_| ProjectionError::new("invalid_endpoint", "auth route URL is invalid"))?;
    url.set_query(None);
    url.set_fragment(None);
    let path = url.path().trim_end_matches('/').to_string();
    let stripped = path
        .strip_suffix("/chat/completions")
        .or_else(|| path.strip_suffix("/responses"))
        .or_else(|| path.strip_suffix("/messages"))
        .unwrap_or(path.as_str());
    url.set_path(if stripped.is_empty() { "/" } else { stripped });
    Ok(url.as_str().trim_end_matches('/').to_string())
}

/// All-none credentials persist an empty api-key. The Go host may substitute a
/// non-secret marker in memory and must not send Authorization. A keyed route
/// still requires the real secret.
fn encoded_api_key(auth: &ProjectedAuth) -> Result<String, ProjectionError> {
    if let Some(secret) = auth.material.as_ref() {
        let key = secret.expose();
        if key.trim().is_empty() {
            return Err(ProjectionError::new(
                "missing_material",
                "upstream api key is empty",
            ));
        }
        return Ok(key.to_string());
    }
    let keyed = auth
        .models
        .iter()
        .flat_map(|model| model.routes.iter())
        .any(|route| route.auth != "none");
    if keyed {
        return Err(ProjectionError::new(
            "missing_material",
            "keyed route has no upstream api key",
        ));
    }
    Ok(String::new())
}

/// `validateProxyList` accepts a missing block and rejects an empty proxy-url.
/// Only list mode has a configured leg, so other modes omit the block.
fn list_proxy(projection: &ProductProjection) -> Result<Option<YamlProxyList>, ProjectionError> {
    if projection.proxy_mode != ProxyMode::List {
        return Ok(None);
    }
    let proxy_url = projection.proxy_url.trim();
    if proxy_url.is_empty() {
        return Err(ProjectionError::new(
            "invalid_proxy",
            "list proxy mode has no proxy URL",
        ));
    }
    Ok(Some(YamlProxyList {
        direction: match projection.proxy_list_direction {
            ProxyListDirection::Whitelist => "whitelist",
            ProxyListDirection::Blacklist => "blacklist",
        },
        models: projection.proxy_list_models.clone(),
        proxy_url: proxy_url.to_string(),
    }))
}

fn key_proxy(projection: &ProductProjection) -> Option<String> {
    match projection.proxy_mode {
        ProxyMode::Auto => None,
        ProxyMode::Direct => Some("direct".to_string()),
        ProxyMode::Manual => Some(projection.proxy_url.clone()),
        ProxyMode::List => match projection.proxy_list_direction {
            ProxyListDirection::Whitelist => Some("direct".to_string()),
            ProxyListDirection::Blacklist => Some(projection.proxy_url.clone()),
        },
    }
}

fn credential_entry(auth: &ProjectedAuth) -> Result<YamlCredential, ProjectionError> {
    let mut routes = Vec::new();
    for model in &auth.models {
        for route in &model.routes {
            routes.push(YamlRoute {
                public_model: model.public_alias.clone(),
                upstream_model: model.upstream_name.clone(),
                protocol: yaml_protocol(&route.protocol)?.to_string(),
                endpoint: route.endpoint_url.clone(),
                auth_scheme: yaml_auth(&route.auth)?.to_string(),
                request_identity: match auth.request_identity {
                    super::types::RequestIdentityFact::None => "none",
                    super::types::RequestIdentityFact::OpenCodeSession => "opencode-session",
                }
                .to_string(),
                wire: match auth.wire {
                    super::types::WireFact::None => "none",
                    super::types::WireFact::OllamaReasoning => "ollama-reasoning",
                }
                .to_string(),
                validation_only: route.validation_only,
            });
        }
    }
    if routes.is_empty() {
        return Err(ProjectionError::new(
            "invalid_endpoint",
            "auth has no route",
        ));
    }
    Ok(YamlCredential {
        namespace: auth.auth_id.clone(),
        auth_id: auth.auth_id.clone(),
        credential_id: auth.credential_id.clone(),
        credential_version: auth.credential_version.to_string(),
        binding_id: auth.binding_id.clone(),
        material_revision: auth
            .material
            .as_ref()
            .map(|secret| secret.fingerprint())
            .unwrap_or_else(|| "no-material".to_string()),
        provider_id: auth.provenance.provider_id.clone(),
        // Historical remote CPA is omitted before an auth is built.
        opaque_remote: false,
        routes,
    })
}

fn yaml_protocol(value: &str) -> Result<&str, ProjectionError> {
    match value {
        "chat_completions" | "messages" | "responses" => Ok(value),
        _ => Err(ProjectionError::new(
            "unsupported_protocol",
            "route protocol is not a v1 protocol",
        )),
    }
}

fn yaml_auth(value: &str) -> Result<&'static str, ProjectionError> {
    match value {
        "bearer" => Ok("bearer"),
        "none" => Ok("none"),
        "x_api_key" => Ok("x-api-key"),
        "api_key" => Ok("api-key"),
        _ => Err(ProjectionError::new(
            "invalid_endpoint",
            "route auth scheme is not a v1 scheme",
        )),
    }
}

fn oauth_provider_id(provider: crate::cpa::CpaOAuthProvider) -> &'static str {
    match provider {
        crate::cpa::CpaOAuthProvider::Codex => "codex",
        crate::cpa::CpaOAuthProvider::Anthropic => "anthropic",
        crate::cpa::CpaOAuthProvider::Antigravity => "antigravity",
        crate::cpa::CpaOAuthProvider::Kimi => "kimi",
        crate::cpa::CpaOAuthProvider::Xai => "xai",
    }
}

#[derive(Serialize)]
struct YamlDocument {
    host: String,
    port: u16,
    #[serde(rename = "auth-dir")]
    auth_dir: String,
    debug: bool,
    #[serde(rename = "logging-to-file")]
    logging_to_file: bool,
    #[serde(rename = "remote-management")]
    remote_management: YamlRemoteManagement,
    #[serde(rename = "api-keys")]
    api_keys: Vec<String>,
    routing: YamlRouting,
    #[serde(rename = "openai-compatibility")]
    openai_compatibility: Vec<YamlProvider>,
    ocg: YamlOcg,
}

#[derive(Serialize)]
struct YamlRemoteManagement {
    #[serde(rename = "allow-remote")]
    allow_remote: bool,
    #[serde(rename = "secret-key")]
    secret_key: String,
    #[serde(rename = "disable-control-panel")]
    disable_control_panel: bool,
    #[serde(rename = "disable-auto-update-panel")]
    disable_auto_update_panel: bool,
}

#[derive(Serialize)]
struct YamlRouting {
    strategy: &'static str,
}

#[derive(Serialize)]
struct YamlProvider {
    name: String,
    priority: i64,
    #[serde(rename = "base-url")]
    base_url: String,
    #[serde(rename = "api-key-entries")]
    api_key_entries: Vec<YamlKey>,
    models: Vec<YamlModel>,
}

#[derive(Serialize)]
struct YamlKey {
    #[serde(rename = "api-key")]
    api_key: String,
    #[serde(rename = "proxy-url", skip_serializing_if = "Option::is_none")]
    proxy_url: Option<String>,
}

#[derive(Serialize)]
struct YamlModel {
    name: String,
    alias: String,
}

#[derive(Serialize)]
struct YamlOcg {
    #[serde(rename = "protocol-version")]
    protocol_version: u64,
    #[serde(rename = "process-generation")]
    process_generation: String,
    #[serde(rename = "projection-revision")]
    projection_revision: String,
    #[serde(rename = "ready-key")]
    ready_key: String,
    policy: YamlPolicy,
    routing: YamlOcgRouting,
    #[serde(rename = "proxy-list", skip_serializing_if = "Option::is_none")]
    proxy_list: Option<YamlProxyList>,
    credentials: Vec<YamlCredential>,
    #[serde(rename = "oauth-bindings")]
    oauth_bindings: Vec<YamlOAuthBinding>,
}

#[derive(Serialize)]
struct YamlPolicy {
    url: String,
    token: String,
    origin: String,
}

#[derive(Serialize)]
struct YamlOcgRouting {
    #[serde(rename = "sticky-global")]
    sticky_global: bool,
    #[serde(rename = "conversation-sticky")]
    conversation_sticky: bool,
    #[serde(rename = "conversation-ttl-seconds")]
    conversation_ttl_seconds: u64,
}

#[derive(Serialize)]
struct YamlProxyList {
    direction: &'static str,
    models: Vec<String>,
    #[serde(rename = "proxy-url")]
    proxy_url: String,
}

#[derive(Serialize)]
struct YamlCredential {
    namespace: String,
    #[serde(rename = "auth-id")]
    auth_id: String,
    #[serde(rename = "credential-id")]
    credential_id: String,
    #[serde(rename = "credential-version")]
    credential_version: String,
    #[serde(rename = "binding-id")]
    binding_id: String,
    #[serde(rename = "material-revision")]
    material_revision: String,
    #[serde(rename = "provider-id")]
    provider_id: String,
    #[serde(rename = "opaque-remote")]
    opaque_remote: bool,
    routes: Vec<YamlRoute>,
}

#[derive(Serialize)]
struct YamlRoute {
    #[serde(rename = "public-model")]
    public_model: String,
    #[serde(rename = "upstream-model")]
    upstream_model: String,
    protocol: String,
    endpoint: String,
    #[serde(rename = "auth-scheme")]
    auth_scheme: String,
    #[serde(rename = "request-identity")]
    request_identity: String,
    wire: String,
    #[serde(rename = "validation-only", skip_serializing_if = "is_false")]
    validation_only: bool,
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Serialize)]
struct YamlOAuthBinding {
    #[serde(rename = "relative-path")]
    relative_path: String,
    #[serde(rename = "auth-id")]
    auth_id: String,
    #[serde(rename = "credential-id")]
    credential_id: String,
    #[serde(rename = "credential-version")]
    credential_version: String,
    #[serde(rename = "material-revision")]
    material_revision: String,
    #[serde(rename = "provider-id")]
    provider_id: String,
    #[serde(rename = "native-provider")]
    native_provider: String,
    priority: i64,
    models: Vec<String>,
    /// Lowercase SHA-256 of this binding's route set. Absent until that set exists.
    #[serde(
        rename = "native-route-fingerprint",
        skip_serializing_if = "Option::is_none"
    )]
    native_route_fingerprint: Option<String>,
}

fn native_route_fingerprint(
    projection: &ProductProjection,
    item: &super::types::OAuthFileRef,
) -> Result<Option<String>, ProjectionError> {
    let mut matched = projection.route_sets.iter().filter(|set| {
        set.auth_id == item.auth_id
            && set.credential_id == item.credential_id
            && set.credential_version == item.credential_version
    });
    let Some(set) = matched.next() else {
        return Ok(None);
    };
    if matched.next().is_some() {
        return Err(ProjectionError::new(
            "invalid_oauth_ref",
            "native binding matches more than one route set",
        ));
    }
    let fingerprint = set.fingerprint.to_ascii_lowercase();
    if fingerprint.len() != 64
        || !fingerprint.bytes().all(|byte| byte.is_ascii_hexdigit())
        || fingerprint != set.fingerprint
    {
        return Err(ProjectionError::new(
            "invalid_oauth_ref",
            "native route fingerprint is not a lowercase SHA-256",
        ));
    }
    Ok(Some(fingerprint))
}

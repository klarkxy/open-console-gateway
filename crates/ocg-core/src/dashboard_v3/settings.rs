//! GET/PUT `/settings` — application settings contract and process-scoped CAS write.

use axum::Json;
use axum::body::Bytes;
use axum::extract::State;
use serde_json::Value;

use crate::kernel::ids::is_free_model;
use crate::kernel::protocol::{ApiFormat, supported_model_protocols};
use crate::models::{
    AppConfig, ProxyListDirection as AppProxyListDirection, ProxyMode as AppProxyMode,
    RoutingMode as AppRoutingMode, normalize_client_root_url,
};
use crate::state::{CoreState, HostSettingsError};

use super::types::{
    ProxyListDirection, ProxyMode, ProxySupportedModel, RoutingMode, Settings, SettingsUpdate,
};
use super::{MutationAck, V3ApiError};

pub(super) async fn get_settings(State(state): State<CoreState>) -> Json<Settings> {
    let _settings_update = state.settings_update.lock();
    Json(settings_from_state(&state))
}

pub(super) async fn put_settings(
    State(state): State<CoreState>,
    body: Bytes,
) -> Result<Json<MutationAck>, V3ApiError> {
    let update = parse_settings_update(&body)?;
    update_settings(&state, update).map(Json)
}

fn parse_settings_update(bytes: &[u8]) -> Result<SettingsUpdate, V3ApiError> {
    let value: Value = serde_json::from_slice(bytes).map_err(|_| V3ApiError::invalid_json())?;
    let Some(object) = value.as_object() else {
        return Err(V3ApiError::invalid_json());
    };
    if !object.contains_key("expectedRevision") {
        return Err(V3ApiError::missing_expected_revision());
    }
    serde_json::from_value(value).map_err(|_| V3ApiError::invalid_json())
}

/// Validates, then commits one settings patch. Bumps the unified revision
/// exactly once on success (`set_config`). The primary Key is preserved from
/// the live config; this path never accepts Key plaintext.
fn update_settings(state: &CoreState, update: SettingsUpdate) -> Result<MutationAck, V3ApiError> {
    let _settings_update = state.settings_update.lock();
    if update.expectation.expected_revision != state.settings_revision()
        || update.expectation.process_generation != state.process_generation()
    {
        return Err(V3ApiError::revision_conflict(state));
    }

    let previous_config = state.config();
    let mut config = previous_config.clone();
    apply_settings_patch(&mut config, &update);
    {
        let db = state.db.lock();
        crate::gateway_keys::ensure_primary_value_allowed(&db, &config.gateway_key).map_err(
            |error| match error {
                crate::gateway_keys::KeyError::BadRequest(message) => {
                    V3ApiError::invalid_request_at(state, message)
                }
                crate::gateway_keys::KeyError::Internal(message) => V3ApiError::internal(message),
            },
        )?;
    }
    config
        .validate()
        .map_err(|message| V3ApiError::invalid_request_at(state, message))?;
    validate_proxy_list(state, &mut config)
        .map_err(|message| V3ApiError::invalid_request_at(state, message))?;
    validate_upstream_url(&config.upstream_base_url)
        .map_err(|message| V3ApiError::invalid_request_at(state, message))?;
    config.client_root_url = normalize_client_root_url(&config.client_root_url)
        .map_err(|message| V3ApiError::invalid_request_at(state, message))?;

    state
        .apply_host_settings(&previous_config, config)
        .map_err(|error| map_host_settings_error(state, error))?;

    Ok(MutationAck {
        revision: state.settings_revision(),
        process_generation: state.process_generation(),
    })
}

fn map_host_settings_error(state: &CoreState, error: HostSettingsError) -> V3ApiError {
    match error {
        HostSettingsError::AutoStartUnsupported => {
            V3ApiError::invalid_request_at(state, HostSettingsError::AUTO_START_UNAVAILABLE)
        }
        HostSettingsError::DockVisibilityUnsupported => {
            V3ApiError::invalid_request_at(state, HostSettingsError::DOCK_VISIBILITY_UNAVAILABLE)
        }
        HostSettingsError::Persist(error) => V3ApiError::internal(error),
        HostSettingsError::Sync(message) => V3ApiError::internal(message),
    }
}

fn apply_settings_patch(config: &mut AppConfig, update: &SettingsUpdate) {
    if let Some(gateway_port) = update.gateway_port {
        config.gateway_port = gateway_port;
    }
    if let Some(upstream_base_url) = &update.upstream_base_url {
        config.upstream_base_url = upstream_base_url.clone();
    }
    if let Some(proxy_mode) = update.proxy_mode {
        config.proxy_mode = app_proxy_mode(proxy_mode);
    }
    if let Some(proxy_url) = &update.proxy_url {
        config.proxy_url = proxy_url.clone();
    }
    if let Some(proxy_list_direction) = update.proxy_list_direction {
        config.proxy_list_direction = app_proxy_list_direction(proxy_list_direction);
    }
    if let Some(proxy_list_models) = &update.proxy_list_models {
        config.proxy_list_models = proxy_list_models.clone();
    }
    if let Some(opencode_invite_url) = &update.opencode_invite_url {
        config.opencode_invite_url = opencode_invite_url.clone();
    }
    if let Some(client_root_url) = &update.client_root_url {
        config.client_root_url = client_root_url.clone();
    }
    if let Some(auto_start) = update.auto_start {
        config.auto_start = auto_start;
    }
    if let Some(show_dock_icon) = update.show_dock_icon {
        config.show_dock_icon = show_dock_icon;
    }
    if let Some(connect_timeout_secs) = update.connect_timeout_secs {
        config.connect_timeout_secs = connect_timeout_secs;
    }
    if let Some(non_stream_timeout_secs) = update.non_stream_timeout_secs {
        config.non_stream_timeout_secs = non_stream_timeout_secs;
    }
    if let Some(stream_idle_timeout_secs) = update.stream_idle_timeout_secs {
        config.stream_idle_timeout_secs = stream_idle_timeout_secs;
    }
    if let Some(routing_mode) = update.routing_mode {
        config.routing_mode = app_routing_mode(routing_mode);
    }
    if let Some(conversation_sticky) = update.conversation_sticky {
        config.conversation_sticky = conversation_sticky;
    }
}

fn settings_from_state(state: &CoreState) -> Settings {
    let config = state.settings_config();
    let auto_start_supported = state.auto_start_supported();
    let dock_visibility_supported = state.dock_visibility_supported();
    Settings {
        revision: state.settings_revision(),
        process_generation: state.process_generation(),
        gateway_port: config.gateway_port,
        upstream_base_url: config.upstream_base_url,
        proxy_mode: v3_proxy_mode(config.proxy_mode),
        proxy_url: config.proxy_url,
        proxy_list_direction: v3_proxy_list_direction(config.proxy_list_direction),
        proxy_list_models: config.proxy_list_models,
        proxy_supported_models: proxy_supported_models(state),
        opencode_invite_url: config.opencode_invite_url,
        client_root_url: config.client_root_url,
        client_root_url_from_env: state.client_root_url_from_env(),
        auto_start: auto_start_supported.then_some(config.auto_start),
        auto_start_supported,
        show_dock_icon: dock_visibility_supported.then_some(config.show_dock_icon),
        dock_visibility_supported,
        connect_timeout_secs: config.connect_timeout_secs,
        non_stream_timeout_secs: config.non_stream_timeout_secs,
        stream_idle_timeout_secs: config.stream_idle_timeout_secs,
        routing_mode: v3_routing_mode(config.routing_mode),
        conversation_sticky: config.conversation_sticky,
    }
}

fn proxy_supported_models(state: &CoreState) -> Vec<ProxySupportedModel> {
    let zen_catalog = state.zen_free_model_catalog();
    let zen_ids = zen_catalog
        .models
        .iter()
        .cloned()
        .collect::<std::collections::HashSet<_>>();
    let mut models = supported_model_protocols()
        .filter_map(|(id, preferred)| {
            let legacy_zen = id == "big-pickle" || is_free_model(id);
            (!legacy_zen || zen_ids.contains(id)).then(|| ProxySupportedModel {
                id: id.to_string(),
                preferred_protocol: preferred_protocol_name(preferred).to_string(),
                zen_free: legacy_zen,
            })
        })
        .collect::<Vec<_>>();
    let mut known = models
        .iter()
        .map(|model| model.id.clone())
        .collect::<std::collections::HashSet<_>>();
    for id in &zen_catalog.models {
        if known.insert(id.clone()) {
            models.push(ProxySupportedModel {
                id: id.clone(),
                preferred_protocol: preferred_protocol_name(ApiFormat::ChatCompletions).to_string(),
                zen_free: true,
            });
        }
    }
    models.sort_by(|left, right| left.id.cmp(&right.id));
    models
}

fn validate_proxy_list(state: &CoreState, config: &mut AppConfig) -> Result<(), String> {
    if config.proxy_mode != AppProxyMode::List {
        return Ok(());
    }
    if config.proxy_list_models.is_empty() {
        return Err("list proxy mode requires at least one model".to_string());
    }
    let known = proxy_supported_models(state)
        .into_iter()
        .map(|model| model.id)
        .collect::<std::collections::HashSet<_>>();
    let mut deduped: Vec<String> = Vec::new();
    for model in config.proxy_list_models.iter() {
        let model = model.trim();
        if !known.contains(model) {
            return Err(format!("unknown model in proxy list: `{model}`"));
        }
        if !deduped.iter().any(|existing| existing == model) {
            deduped.push(model.to_string());
        }
    }
    config.proxy_list_models = deduped;
    Ok(())
}

fn validate_upstream_url(url: &str) -> Result<(), String> {
    let parsed =
        reqwest::Url::parse(url).map_err(|error| format!("invalid upstream URL: {error}"))?;
    match parsed.scheme() {
        "https" => Ok(()),
        "http" if is_loopback(&parsed) => Ok(()),
        _ => Err("upstream must use https, except loopback http".to_string()),
    }
}

fn preferred_protocol_name(format: ApiFormat) -> &'static str {
    match format {
        ApiFormat::ChatCompletions => "chat_completions",
        ApiFormat::Responses => "responses",
        ApiFormat::Messages => "messages",
        ApiFormat::Gemini => "gemini",
    }
}

fn is_loopback(url: &reqwest::Url) -> bool {
    matches!(
        url.host_str(),
        Some("localhost") | Some("127.0.0.1") | Some("::1") | Some("[::1]")
    )
}

fn v3_proxy_mode(mode: AppProxyMode) -> ProxyMode {
    match mode {
        AppProxyMode::Auto => ProxyMode::Auto,
        AppProxyMode::Manual => ProxyMode::Manual,
        AppProxyMode::Direct => ProxyMode::Direct,
        AppProxyMode::List => ProxyMode::List,
    }
}

fn app_proxy_mode(mode: ProxyMode) -> AppProxyMode {
    match mode {
        ProxyMode::Auto => AppProxyMode::Auto,
        ProxyMode::Manual => AppProxyMode::Manual,
        ProxyMode::Direct => AppProxyMode::Direct,
        ProxyMode::List => AppProxyMode::List,
    }
}

fn v3_proxy_list_direction(direction: AppProxyListDirection) -> ProxyListDirection {
    match direction {
        AppProxyListDirection::Whitelist => ProxyListDirection::Whitelist,
        AppProxyListDirection::Blacklist => ProxyListDirection::Blacklist,
    }
}

fn app_proxy_list_direction(direction: ProxyListDirection) -> AppProxyListDirection {
    match direction {
        ProxyListDirection::Whitelist => AppProxyListDirection::Whitelist,
        ProxyListDirection::Blacklist => AppProxyListDirection::Blacklist,
    }
}

fn v3_routing_mode(mode: AppRoutingMode) -> RoutingMode {
    match mode {
        AppRoutingMode::StrictPriority => RoutingMode::StrictPriority,
        AppRoutingMode::StickyGlobal => RoutingMode::StickyGlobal,
        AppRoutingMode::RoundRobin => RoutingMode::RoundRobin,
    }
}

fn app_routing_mode(mode: RoutingMode) -> AppRoutingMode {
    match mode {
        RoutingMode::StrictPriority => AppRoutingMode::StrictPriority,
        RoutingMode::StickyGlobal => AppRoutingMode::StickyGlobal,
        RoutingMode::RoundRobin => AppRoutingMode::RoundRobin,
    }
}

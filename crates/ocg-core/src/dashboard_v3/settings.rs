//! GET/PUT `/settings` — application settings contract and process-scoped CAS write.

use axum::Json;
use axum::body::Bytes;
use axum::extract::State;

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
use super::{MutationAck, V3ApiError, check_expectation, parse_mutation_json};

pub(super) async fn get_settings(State(state): State<CoreState>) -> Json<Settings> {
    let _settings_update = state.settings_update.lock();
    Json(settings_from_state(&state))
}

pub(super) async fn put_settings(
    State(state): State<CoreState>,
    body: Bytes,
) -> Result<Json<MutationAck>, V3ApiError> {
    let update = parse_mutation_json::<SettingsUpdate>(&body)?;
    update_settings(&state, update).await.map(Json)
}

/// Validates, then commits one settings patch. Bumps the unified revision
/// exactly once on success (`set_config`). The primary Key is preserved from
/// the live config; this path never accepts Key plaintext. A Gateway port
/// change rebinds a running listener through `CoreState` after persist;
/// `settings_host_effects` serializes persist → rebind → compensation
/// without holding `settings_update` across the await.
async fn update_settings(
    state: &CoreState,
    update: SettingsUpdate,
) -> Result<MutationAck, V3ApiError> {
    let _effects = state.lock_settings_host_effects().await;
    let changed_fields = changed_setting_fields(&update).join(",");
    let (previous_config, config, committed_revision) = {
        let _settings_update = state.settings_update.lock();
        check_expectation(state, &update.expectation)?;
        if state.gateway_port_from_env() && update.gateway_port.is_some() {
            return Err(V3ApiError::invalid_request_at(
                state,
                "Gateway port is managed by OCG_GATEWAY_PORT",
            ));
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
                    crate::gateway_keys::KeyError::Internal(message) => {
                        V3ApiError::internal(message)
                    }
                },
            )?;
        }
        config
            .validate()
            .map_err(|message| V3ApiError::invalid_request_at(state, message))?;
        validate_proxy_list(state, &mut config)
            .map_err(|message| V3ApiError::invalid_request_at(state, message))?;
        config.client_root_url = normalize_client_root_url(&config.client_root_url)
            .map_err(|message| V3ApiError::invalid_request_at(state, message))?;

        state
            .apply_host_settings(&previous_config, config.clone())
            .map_err(|error| {
                state.log_runtime_event(
                    "error",
                    "settings",
                    &format!(
                        "event=settings_update_failed fields={changed_fields} reason={}",
                        host_settings_error_kind(&error)
                    ),
                );
                map_host_settings_error(state, error)
            })?;
        let committed_revision = state.settings_revision();
        (previous_config, config, committed_revision)
    };

    if let Err(error) = state
        .rebind_listener_after_settings_commit(previous_config, config, committed_revision, false)
        .await
    {
        state.log_runtime_event(
            "error",
            "settings",
            &format!(
                "event=settings_update_failed fields={changed_fields} revision={} reason={}",
                state.settings_revision(),
                host_settings_error_kind(&error)
            ),
        );
        return Err(map_host_settings_error(state, error));
    }

    state.log_runtime_event(
        "info",
        "settings",
        &format!(
            "event=settings_updated fields={changed_fields} revision={}",
            state.settings_revision()
        ),
    );

    Ok(MutationAck {
        revision: state.settings_revision(),
        process_generation: state.process_generation(),
    })
}

fn changed_setting_fields(update: &SettingsUpdate) -> Vec<&'static str> {
    let mut fields = Vec::new();
    for (present, name) in [
        (update.gateway_port.is_some(), "gateway_port"),
        (update.proxy_mode.is_some(), "proxy_mode"),
        (update.proxy_url.is_some(), "proxy_url"),
        (
            update.proxy_list_direction.is_some(),
            "proxy_list_direction",
        ),
        (update.proxy_list_models.is_some(), "proxy_list_models"),
        (update.opencode_invite_url.is_some(), "opencode_invite_url"),
        (update.client_root_url.is_some(), "client_root_url"),
        (update.auto_start.is_some(), "auto_start"),
        (update.show_dock_icon.is_some(), "show_dock_icon"),
        (
            update.connect_timeout_secs.is_some(),
            "connect_timeout_secs",
        ),
        (
            update.non_stream_timeout_secs.is_some(),
            "non_stream_timeout_secs",
        ),
        (
            update.stream_idle_timeout_secs.is_some(),
            "stream_idle_timeout_secs",
        ),
        (update.routing_mode.is_some(), "routing_mode"),
        (update.conversation_sticky.is_some(), "conversation_sticky"),
    ] {
        if present {
            fields.push(name);
        }
    }
    if fields.is_empty() {
        fields.push("none");
    }
    fields
}

fn host_settings_error_kind(error: &HostSettingsError) -> &'static str {
    match error {
        HostSettingsError::AutoStartUnsupported => "auto_start_unsupported",
        HostSettingsError::DockVisibilityUnsupported => "dock_visibility_unsupported",
        HostSettingsError::Persist(_) => "persist_failed",
        HostSettingsError::Sync(_) => "host_sync_failed",
        HostSettingsError::GatewayBind(_) => "gateway_bind_failed",
    }
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
        HostSettingsError::GatewayBind(error) => V3ApiError::internal(error),
    }
}

fn apply_settings_patch(config: &mut AppConfig, update: &SettingsUpdate) {
    if let Some(gateway_port) = update.gateway_port {
        config.gateway_port = gateway_port;
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
        gateway_port_from_env: state.gateway_port_from_env(),
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
    let contracts = state.provider_contracts();
    for provider_id in [
        crate::kernel::ids::MINIMAX_PROVIDER_ID,
        crate::kernel::ids::KIMI_PROVIDER_ID,
    ] {
        let Some(contract) = contracts.provider_offering(provider_id) else {
            continue;
        };
        for id in &contract.catalog.models {
            if known.insert(id.clone()) {
                models.push(ProxySupportedModel {
                    id: id.clone(),
                    preferred_protocol: preferred_protocol_name(ApiFormat::ChatCompletions)
                        .to_string(),
                    zen_free: false,
                });
            }
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

fn preferred_protocol_name(format: ApiFormat) -> &'static str {
    match format {
        ApiFormat::ChatCompletions => "chat_completions",
        ApiFormat::Responses => "responses",
        ApiFormat::Messages => "messages",
        ApiFormat::Gemini => "gemini",
    }
}

fn v3_proxy_mode(mode: AppProxyMode) -> ProxyMode {
    match mode {
        AppProxyMode::Auto => ProxyMode::Auto,
        AppProxyMode::Manual => ProxyMode::Manual,
        AppProxyMode::Direct => ProxyMode::Direct,
        AppProxyMode::List => ProxyMode::List,
    }
}

pub(crate) fn app_proxy_mode(mode: ProxyMode) -> AppProxyMode {
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

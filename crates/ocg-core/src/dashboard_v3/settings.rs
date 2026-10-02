//! GET/PUT `/settings` — application settings contract and process-scoped CAS write.

use axum::Json;
use axum::body::Bytes;
use axum::extract::State;

use crate::log_types::{OperationMetadata, OperationOutcome};
use crate::models::{
    AppConfig, ProxyListDirection as AppProxyListDirection, ProxyMode as AppProxyMode,
    RoutingMode as AppRoutingMode, normalize_client_root_url,
};
use crate::state::{
    CoreState, HostSettingsEffects, HostSettingsError, HostSettingsFailure,
    persisted_proxy_model_candidates,
};
use crate::user_operation::UserOperation;

use super::types::{
    ProxyListDirection, ProxyMode, ProxySupportedModel, RoutingMode, Settings, SettingsUpdate,
};
use super::{MutationAck, V3ApiError, check_expectation, parse_mutation_json};

/// Stable reason for a committed settings write whose CPA proxy sync failed.
/// The HTTP response stays 200; the receipt uses this stable semantic code.
pub(super) const CPA_PROXY_SYNC_FAILED: &str = "cpaProxySyncFailed";

pub(super) fn open_dashboard(
    state: &CoreState,
    action: &'static str,
    subject_type: &'static str,
    subject_id: Option<String>,
) -> UserOperation {
    UserOperation::dashboard(state, action, subject_type, subject_id)
}

pub(super) fn outcome_for_api_reason(reason: &str) -> OperationOutcome {
    match reason {
        "unauthorized"
        | "invalidJson"
        | "missingExpectedRevision"
        | "revisionConflict"
        | "invalidRequest"
        | "notFound"
        | "conflict"
        | "preconditionFailed"
        | "notImplemented"
        | "forbidden"
        | "gone"
        | "throttled"
        | "builtinProviderImmutable"
        | "operationPayloadMismatch" => OperationOutcome::Rejected,
        _ => OperationOutcome::Failed,
    }
}

pub(super) fn metadata_for(
    state: &CoreState,
    fields: &[&str],
    requested: Option<u32>,
    completed: Option<u32>,
    failed: Option<u32>,
    compensated: Option<bool>,
) -> OperationMetadata {
    OperationMetadata {
        changed_fields: fields.iter().map(|field| (*field).to_string()).collect(),
        requested_count: requested,
        completed_count: completed,
        failed_count: failed,
        revision: Some(state.settings_revision()),
        compensated,
        related_ids: Vec::new(),
    }
}

pub(super) fn count_u32(value: u64) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

/// Subject ids that storage can keep. Anything else is omitted so the receipt
/// still records; the caller does not invent a replacement id.
pub(super) fn known_subject(value: &str) -> Option<String> {
    let mut chars = value.chars();
    let opaque = matches!(chars.next(), Some(ch) if ch.is_ascii_alphanumeric())
        && value.len() <= crate::log_types::MAX_OPAQUE_ID_LEN
        && chars.all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '.' | ':' | '@' | '-'));
    opaque.then(|| value.to_string())
}

/// Effect observed for this operation while its own guard was still held.
/// An atomic error keeps its semantic code. A later settings revision from
/// some other writer is not evidence that this operation committed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CommittedEffect {
    Atomic,
    Partial {
        reason: &'static str,
    },
    /// The durable write succeeded and the business result is a failure.
    Failed {
        reason: &'static str,
    },
}

impl CommittedEffect {
    /// The durable write already landed. A following error is this operation's
    /// partial effect, not an atomic rejection.
    pub(super) fn note_follow_up<T>(
        &mut self,
        result: Result<T, V3ApiError>,
    ) -> Result<T, V3ApiError> {
        if let Err(error) = &result
            && matches!(self, Self::Atomic)
        {
            *self = Self::Partial {
                reason: static_reason(error.operation_reason()),
            };
        }
        result
    }
}

pub(super) fn static_reason(reason: &str) -> &'static str {
    match reason {
        "unauthorized" => "unauthorized",
        "invalidJson" => "invalidJson",
        "missingExpectedRevision" => "missingExpectedRevision",
        "revisionConflict" => "revisionConflict",
        "invalidRequest" => "invalidRequest",
        "notFound" => "notFound",
        "conflict" => "conflict",
        "preconditionFailed" => "preconditionFailed",
        "notImplemented" => "notImplemented",
        "forbidden" => "forbidden",
        "gone" => "gone",
        "throttled" => "throttled",
        "builtinProviderImmutable" => "builtinProviderImmutable",
        "operationPayloadMismatch" => "operationPayloadMismatch",
        "internal" => "internal",
        "outboundFailed" => "outboundFailed",
        "serviceUnavailable" => "serviceUnavailable",
        "persistFailed" => "persistFailed",
        _ => "internal",
    }
}

/// One terminal receipt after every guard in the business call has been dropped.
pub(super) fn record_after<T>(
    op: UserOperation,
    state: &CoreState,
    fields: &[&str],
    counts: (Option<u32>, Option<u32>, Option<u32>),
    ok_outcome: Option<(OperationOutcome, &'static str)>,
    result: Result<T, V3ApiError>,
) -> Result<T, V3ApiError> {
    record_effect(
        op,
        state,
        fields,
        counts,
        ok_outcome,
        CommittedEffect::Atomic,
        result,
    )
}

pub(super) fn record_effect<T>(
    op: UserOperation,
    state: &CoreState,
    fields: &[&str],
    counts: (Option<u32>, Option<u32>, Option<u32>),
    ok_outcome: Option<(OperationOutcome, &'static str)>,
    effect: CommittedEffect,
    result: Result<T, V3ApiError>,
) -> Result<T, V3ApiError> {
    let (requested, completed, failed) = counts;
    match &result {
        Ok(_) => {
            let (outcome, reason, compensated) = match ok_outcome {
                Some((outcome, reason)) => {
                    let compensated = match outcome {
                        OperationOutcome::Compensated => Some(true),
                        OperationOutcome::Partial => Some(false),
                        _ => None,
                    };
                    (outcome, Some(reason), compensated)
                }
                None => (OperationOutcome::Success, None, None),
            };
            op.complete(
                outcome,
                reason,
                metadata_for(state, fields, requested, completed, failed, compensated),
            );
        }
        Err(_) => match effect {
            CommittedEffect::Atomic => op.result(&result),
            CommittedEffect::Partial { reason } => op.complete(
                OperationOutcome::Partial,
                Some(reason),
                metadata_for(
                    state,
                    fields,
                    requested,
                    completed.or(Some(1)),
                    failed.or(Some(1)),
                    Some(false),
                ),
            ),
            CommittedEffect::Failed { reason } => op.complete(
                OperationOutcome::Failed,
                Some(reason),
                metadata_for(
                    state,
                    fields,
                    requested,
                    completed,
                    failed.or(Some(1)),
                    None,
                ),
            ),
        },
    }
    result
}

pub(super) fn probe_batch_outcome(
    succeeded: u32,
    failed: u32,
) -> (OperationOutcome, Option<&'static str>) {
    if failed == 0 {
        (OperationOutcome::Success, None)
    } else if succeeded == 0 {
        (OperationOutcome::Failed, Some("outboundFailed"))
    } else {
        (OperationOutcome::Partial, Some("outboundFailed"))
    }
}

pub(super) async fn get_settings(State(state): State<CoreState>) -> Json<Settings> {
    let _settings_update = state.settings_update.lock();
    Json(settings_from_state(&state))
}

pub(super) async fn put_settings(
    State(state): State<CoreState>,
    body: Bytes,
) -> Result<Json<MutationAck>, V3ApiError> {
    let op = open_dashboard(&state, "settings.update", "settings", None);
    let update = match parse_mutation_json::<SettingsUpdate>(&body) {
        Ok(update) => update,
        Err(error) => {
            let result = Err(error);
            op.result(&result);
            return result.map(Json);
        }
    };
    let fields = changed_setting_fields(&update);
    let outcome = update_settings(&state, update).await;
    finish_settings(op, &state, &fields, outcome)
}

/// Validates, then commits one settings patch. Bumps the unified revision
/// exactly once on success (`set_config`). The primary Key is preserved from
/// the live config; this path never accepts Key plaintext. A Gateway port
/// change rebinds a running listener through `CoreState` after persist;
/// `settings_host_effects` serializes persist → rebind → compensation
/// without holding `settings_update` across the await.
struct SettingsUpdateOutcome {
    result: Result<MutationAck, V3ApiError>,
    receipt: SettingsReceipt,
}

enum SettingsReceipt {
    FromResult,
    Success,
    Partial { reason: &'static str },
    Compensated { reason: &'static str },
}

impl SettingsUpdateOutcome {
    fn from_err(error: V3ApiError) -> Self {
        Self {
            result: Err(error),
            receipt: SettingsReceipt::FromResult,
        }
    }
}

async fn update_settings(state: &CoreState, update: SettingsUpdate) -> SettingsUpdateOutcome {
    let _effects = state.lock_settings_host_effects().await;
    let changed_fields = changed_setting_fields(&update).join(",");
    let (previous_config, config, committed_revision) = {
        let _settings_update = state.settings_update.lock();
        if let Err(error) = check_expectation(state, &update.expectation) {
            return SettingsUpdateOutcome::from_err(error);
        }
        if state.gateway_port_from_env() && update.gateway_port.is_some() {
            return SettingsUpdateOutcome::from_err(V3ApiError::invalid_request_at(
                state,
                "Gateway port is managed by OCG_GATEWAY_PORT",
            ));
        }

        let previous_config = state.config();
        let mut config = previous_config.clone();
        apply_settings_patch(&mut config, &update);
        {
            let db = state.db.lock();
            if let Err(error) =
                crate::gateway_keys::ensure_primary_value_allowed(&db, &config.gateway_key)
            {
                return SettingsUpdateOutcome::from_err(match error {
                    crate::gateway_keys::KeyError::BadRequest(message) => {
                        V3ApiError::invalid_request_at(state, message)
                    }
                    crate::gateway_keys::KeyError::Internal(message) => {
                        V3ApiError::internal(message)
                    }
                });
            }
        }
        if let Err(message) = config.validate() {
            return SettingsUpdateOutcome::from_err(V3ApiError::invalid_request_at(state, message));
        }
        if let Err(message) = validate_proxy_list(state, &mut config) {
            return SettingsUpdateOutcome::from_err(V3ApiError::invalid_request_at(state, message));
        }
        match normalize_client_root_url(&config.client_root_url) {
            Ok(url) => config.client_root_url = url,
            Err(message) => {
                return SettingsUpdateOutcome::from_err(V3ApiError::invalid_request_at(
                    state, message,
                ));
            }
        }

        if let Err(failure) = state.apply_host_settings_recorded(&previous_config, config.clone()) {
            let receipt = host_failure_receipt(&failure, false);
            log_settings_failure(state, "settings_update_failed", &changed_fields, &failure);
            return SettingsUpdateOutcome {
                result: Err(map_host_settings_error(state, failure.error)),
                receipt,
            };
        }
        let committed_revision = state.settings_revision();
        (previous_config, config, committed_revision)
    };

    // A changed outbound proxy policy must reach the managed CPA process too;
    // sync failure leaves the settings commit intact and is surfaced in
    // Runtime Logs instead of rolling back unrelated fields.
    let mut cpa_proxy_sync_failed = false;
    if crate::cpa_runtime::cpa_requests_proxy_url(&previous_config)
        != crate::cpa_runtime::cpa_requests_proxy_url(&config)
        && state.sync_cpa_proxy_settings().await.is_err()
    {
        cpa_proxy_sync_failed = true;
        log_static_reason(
            state,
            "cpa_proxy_sync_failed",
            &changed_fields,
            CPA_PROXY_SYNC_FAILED,
        );
    }

    if let Err(failure) = state
        .rebind_listener_after_settings_commit_recorded(
            previous_config.clone(),
            config,
            committed_revision,
            false,
        )
        .await
    {
        let receipt = host_failure_receipt(&failure, cpa_proxy_sync_failed);
        log_settings_failure(state, "settings_update_failed", &changed_fields, &failure);
        return SettingsUpdateOutcome {
            result: Err(map_host_settings_error(state, failure.error)),
            receipt,
        };
    }

    state.log_runtime_event(
        "info",
        "settings",
        &format!(
            "event=settings_updated fields={changed_fields} revision={}",
            state.settings_revision()
        ),
    );

    SettingsUpdateOutcome {
        result: Ok(MutationAck {
            revision: state.settings_revision(),
            process_generation: state.process_generation(),
        }),
        receipt: prefer_proxy_partial(SettingsReceipt::Success, cpa_proxy_sync_failed),
    }
}

fn finish_settings(
    op: UserOperation,
    state: &CoreState,
    fields: &[&str],
    outcome: SettingsUpdateOutcome,
) -> Result<Json<MutationAck>, V3ApiError> {
    let fields: Vec<&str> = fields
        .iter()
        .copied()
        .filter(|field| *field != "none")
        .collect();
    match outcome.receipt {
        SettingsReceipt::FromResult => op.result(&outcome.result),
        SettingsReceipt::Success => op.complete(
            OperationOutcome::Success,
            None,
            metadata_for(state, &fields, None, Some(1), None, None),
        ),
        SettingsReceipt::Partial { reason } => op.complete(
            OperationOutcome::Partial,
            Some(reason),
            metadata_for(state, &fields, None, Some(1), Some(1), Some(false)),
        ),
        SettingsReceipt::Compensated { reason } => op.complete(
            OperationOutcome::Compensated,
            Some(reason),
            metadata_for(state, &fields, None, Some(1), Some(1), Some(true)),
        ),
    }
    outcome.result.map(Json)
}

fn host_failure_receipt(
    failure: &HostSettingsFailure,
    cpa_proxy_sync_failed: bool,
) -> SettingsReceipt {
    let base = match failure.effects {
        HostSettingsEffects::None => SettingsReceipt::FromResult,
        HostSettingsEffects::Partial => SettingsReceipt::Partial {
            reason: host_effect_reason(&failure.error),
        },
        HostSettingsEffects::Compensated => SettingsReceipt::Compensated {
            reason: host_effect_reason(&failure.error),
        },
    };
    prefer_proxy_partial(base, cpa_proxy_sync_failed)
}

/// A failed CPA proxy sync leaves the other committed proxy fields in place.
/// Port compensation does not turn that receipt into Compensated.
fn prefer_proxy_partial(receipt: SettingsReceipt, cpa_proxy_sync_failed: bool) -> SettingsReceipt {
    if !cpa_proxy_sync_failed {
        return receipt;
    }
    match receipt {
        SettingsReceipt::Partial { .. } => receipt,
        _ => SettingsReceipt::Partial {
            reason: CPA_PROXY_SYNC_FAILED,
        },
    }
}

fn host_effect_reason(error: &HostSettingsError) -> &'static str {
    match error {
        HostSettingsError::AutoStartUnsupported | HostSettingsError::DockVisibilityUnsupported => {
            "invalidRequest"
        }
        HostSettingsError::Persist(_) => "persistFailed",
        HostSettingsError::Sync(_) => "hostSyncFailed",
        HostSettingsError::GatewayBind(_) => "gatewayBindFailed",
    }
}

fn log_settings_failure(
    state: &CoreState,
    event: &str,
    fields: &str,
    failure: &HostSettingsFailure,
) {
    log_static_reason(state, event, fields, host_effect_reason(&failure.error));
}

fn log_static_reason(state: &CoreState, event: &str, fields: &str, reason: &str) {
    state.log_runtime_event(
        "error",
        "settings",
        &format!(
            "event={event} fields={fields} revision={} reason={reason}",
            state.settings_revision()
        ),
    );
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
    let projection = crate::destination_projection::load_runtime(&state.db.lock());
    let mut models = projection
        .as_ref()
        .map(persisted_proxy_model_candidates)
        .unwrap_or_default()
        .into_iter()
        .map(|candidate| ProxySupportedModel {
            id: candidate.id,
            preferred_protocol: candidate.preferred_protocol,
            zen_free: candidate.zen_free,
        })
        .collect::<Vec<_>>();
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
        .map(|model| model.id.to_lowercase())
        .collect::<std::collections::HashSet<_>>();
    let persisted = state
        .config()
        .proxy_list_models
        .into_iter()
        .map(|model| model.trim().to_lowercase())
        .collect::<std::collections::HashSet<_>>();
    let mut deduped: Vec<String> = Vec::new();
    for model in config.proxy_list_models.iter() {
        let model = model.trim();
        if model.is_empty() {
            continue;
        }
        if !known.contains(&model.to_lowercase()) {
            if persisted.contains(&model.to_lowercase()) {
                // A once-valid persisted entry may disappear with its
                // catalog. It stays inert on read and is pruned by the next
                // save, but a newly submitted unknown id still fails closed.
                continue;
            }
            return Err(format!("proxy list model `{model}` is not supported"));
        }
        if !deduped
            .iter()
            .any(|existing| existing.to_lowercase() == model.to_lowercase())
        {
            deduped.push(model.to_string());
        }
    }
    if deduped.is_empty() {
        return Err("list proxy mode requires at least one model".to_string());
    }
    config.proxy_list_models = deduped;
    Ok(())
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

#[cfg(test)]
mod tests;

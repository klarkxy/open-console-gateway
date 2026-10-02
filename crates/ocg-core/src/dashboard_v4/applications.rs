//! DSH-specific application installation control plane.
//!
//! This is intentionally not a generic application/plugin registry. The
//! Desktop host owns all local filesystem and process effects; the HTTP layer
//! owns the dashboard session, CAS, and selection of an enabled Gateway Key.

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Query, State};
use serde::Deserialize;
use serde::de::DeserializeOwned;

use crate::dashboard_v3::{ControlRevision, V3ApiError, check_expectation, parse_mutation_json};
use crate::dsh_application::{
    DshApplicationError, DshApplicationErrorKind, DshApplicationHostRequest,
    DshApplicationInspection, DshApplicationOutcome as HostOutcome, DshApplicationPhase,
    DshGatewaySecret,
};
use crate::gateway_keys::PRIMARY_KEY_ID;
use crate::log_types::{OperationMetadata, OperationOutcome};
use crate::state::CoreState;
use crate::user_operation::UserOperation;

use super::types::{
    DshApplication, DshApplicationInstallRequest, DshApplicationOutcome, DshApplicationStatus,
    DshApplicationUninstallRequest, DshDiscoveredProfile,
};

/// Extra-field check without `flatten`, so serde `deny_unknown_fields` works.
/// `parse_mutation_json` then maps those failures to the shared invalid-JSON
/// envelope without widening V3 constructor visibility.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[allow(dead_code)]
struct DshInstallMutationCheck {
    expected_revision: u64,
    process_generation: u64,
    key_id: Option<String>,
    profile_path: Option<String>,
    runtime_url: Option<String>,
    expected_fingerprint: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[allow(dead_code)]
struct DshUninstallMutationCheck {
    expected_revision: u64,
    process_generation: u64,
    profile_path: Option<String>,
    runtime_url: Option<String>,
    expected_fingerprint: String,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct DshProfileQuery {
    profile_path: Option<String>,
    runtime_url: Option<String>,
}

pub(super) async fn get_dsh(
    State(state): State<CoreState>,
    Query(query): Query<DshProfileQuery>,
) -> Result<Json<DshApplication>, V3ApiError> {
    let Some(host) = state.dsh_application_host() else {
        return Ok(Json(payload(
            &state,
            DshApplicationInspection::unsupported(),
        )));
    };
    let gateway_v1_url = {
        let _settings_update = state.settings_update.lock();
        gateway_v1_url(&state)
    };
    let inspection = tokio::task::spawn_blocking(move || {
        host(DshApplicationHostRequest::Inspect {
            gateway_v1_url,
            profile_path: empty_to_none(query.profile_path),
            runtime_url: empty_to_none(query.runtime_url),
        })
    })
    .await
    .map_err(|error| V3ApiError::internal(format!("DSH inspection task failed: {error}")))?
    .map_err(|error| map_host_error(&state, error))?;
    Ok(Json(payload(&state, inspection)))
}

pub(super) async fn install_dsh(
    State(state): State<CoreState>,
    body: Bytes,
) -> Result<Json<DshApplication>, V3ApiError> {
    let mut receipt = DashboardReceipt::open(
        &state,
        "application.install",
        "application",
        Some("dsh".to_string()),
    );
    let mut created = None;
    let result = install_dsh_work(&state, body, &mut created).await;
    note_dsh(&mut receipt, &result, created, true);
    receipt.finish(result).map(Json)
}

async fn install_dsh_work(
    state: &CoreState,
    body: Bytes,
    created: &mut Option<(String, u64)>,
) -> Result<DshApplication, V3ApiError> {
    let input = parse_dsh_mutation::<DshInstallMutationCheck, DshApplicationInstallRequest>(&body)?;
    let host = state.dsh_application_host().ok_or_else(|| {
        V3ApiError::precondition_failed_at(
            state,
            "DSH installation is unavailable in this build; use the Desktop app or a native CLI on the DSH host",
        )
    })?;

    let (gateway_v1_url, secret) = {
        let _settings_update = state.settings_update.lock();
        check_expectation(state, &input.expectation)?;
        let secret = match input.key_id.as_deref() {
            Some(id) => selected_gateway_key(state, id)?,
            None => {
                let effect = application_gateway_key_effect(state, "dsh")?;
                if let (Some(id), Some(revision)) = (effect.created_id, effect.revision) {
                    *created = Some((id, revision));
                }
                effect.key
            }
        };
        (gateway_v1_url(state), secret)
    };

    let request = DshApplicationHostRequest::Install {
        expected_fingerprint: input.expected_fingerprint,
        gateway_v1_url,
        profile_path: empty_to_none(input.profile_path),
        runtime_url: empty_to_none(input.runtime_url),
        secret: DshGatewaySecret::new(secret),
    };
    let inspection = tokio::task::spawn_blocking(move || host(request))
        .await
        .map_err(|error| V3ApiError::internal(format!("DSH installation task failed: {error}")))?
        .map_err(|error| map_host_error(state, error))?;
    Ok(payload(state, inspection))
}

pub(super) async fn uninstall_dsh(
    State(state): State<CoreState>,
    body: Bytes,
) -> Result<Json<DshApplication>, V3ApiError> {
    let mut receipt = DashboardReceipt::open(
        &state,
        "application.uninstall",
        "application",
        Some("dsh".to_string()),
    );
    let result = uninstall_dsh_work(&state, body).await;
    note_dsh(&mut receipt, &result, None, false);
    receipt.finish(result).map(Json)
}

async fn uninstall_dsh_work(state: &CoreState, body: Bytes) -> Result<DshApplication, V3ApiError> {
    let input =
        parse_dsh_mutation::<DshUninstallMutationCheck, DshApplicationUninstallRequest>(&body)?;
    let host = state.dsh_application_host().ok_or_else(|| {
        V3ApiError::precondition_failed_at(
            state,
            "DSH uninstallation is unavailable in this build; use the Desktop app or a native CLI on the DSH host",
        )
    })?;

    let gateway_v1_url = {
        let _settings_update = state.settings_update.lock();
        check_expectation(state, &input.expectation)?;
        gateway_v1_url(state)
    };

    let request = DshApplicationHostRequest::Uninstall {
        expected_fingerprint: input.expected_fingerprint,
        gateway_v1_url,
        profile_path: empty_to_none(input.profile_path),
        runtime_url: empty_to_none(input.runtime_url),
    };
    let inspection = tokio::task::spawn_blocking(move || host(request))
        .await
        .map_err(|error| V3ApiError::internal(format!("DSH uninstallation task failed: {error}")))?
        .map_err(|error| map_host_error(state, error))?;
    Ok(payload(state, inspection))
}

fn parse_dsh_mutation<Check: DeserializeOwned, T: DeserializeOwned>(
    bytes: &[u8],
) -> Result<T, V3ApiError> {
    let _checked = parse_mutation_json::<Check>(bytes)?;
    parse_mutation_json::<T>(bytes)
}

fn empty_to_none(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_owned())
        }
    })
}

fn gateway_v1_url(state: &CoreState) -> String {
    let settings = state.settings_config();
    let root = settings.client_root_url.trim_end_matches('/');
    if root.is_empty() {
        format!("http://127.0.0.1:{}/v1", state.active_gateway_port())
    } else {
        format!("{root}/v1")
    }
}

fn selected_gateway_key(state: &CoreState, key_id: &str) -> Result<String, V3ApiError> {
    if key_id == PRIMARY_KEY_ID {
        let key = state.config().gateway_key;
        if key.is_empty() {
            return Err(V3ApiError::precondition_failed_at(
                state,
                "the primary Key is unavailable",
            ));
        }
        return Ok(key);
    }
    let key = state
        .db
        .lock()
        .get_sub_gateway_key(key_id)
        .map_err(V3ApiError::internal)?
        .filter(|key| key.authenticates())
        .ok_or_else(|| {
            V3ApiError::precondition_failed_at(state, "the selected Key is missing or disabled")
        })?;
    Ok(key.key)
}

/// Called under settings_update. A newly created Key stays usable when a later
/// native file operation fails. The effect reports that id so the caller can
/// receipt Partial; this function does not write an operation row.
pub(super) struct ApplicationKeyEffect {
    pub key: String,
    pub created_id: Option<String>,
    pub revision: Option<u64>,
}

pub(super) fn application_gateway_key_effect(
    state: &CoreState,
    name: &str,
) -> Result<ApplicationKeyEffect, V3ApiError> {
    let (key, created) = crate::gateway_keys::get_or_create_named_sub_key(state, name).map_err(
        |error| match error {
            crate::gateway_keys::KeyError::BadRequest(message) => {
                V3ApiError::precondition_failed_at(state, message)
            }
            crate::gateway_keys::KeyError::Internal(message) => V3ApiError::internal(message),
        },
    )?;
    let (created_id, revision) = if created {
        (Some(key.id), Some(state.bump_settings_revision()))
    } else {
        (None, None)
    };
    Ok(ApplicationKeyEffect {
        key: key.key,
        created_id,
        revision,
    })
}

fn payload(state: &CoreState, inspection: DshApplicationInspection) -> DshApplication {
    DshApplication {
        selected_profile_path: inspection.selected_profile_path,
        status: match inspection.phase {
            DshApplicationPhase::UnsupportedRuntime => DshApplicationStatus::UnsupportedRuntime,
            DshApplicationPhase::NotDetected => DshApplicationStatus::NotDetected,
            DshApplicationPhase::Ready => DshApplicationStatus::Ready,
            DshApplicationPhase::Installed => DshApplicationStatus::Installed,
            DshApplicationPhase::Incompatible => DshApplicationStatus::Incompatible,
            DshApplicationPhase::Conflict => DshApplicationStatus::Conflict,
        },
        detected: inspection.detected,
        installed: inspection.installed,
        install_supported: inspection.install_supported,
        activation_required: inspection.activation_required,
        version: inspection.version,
        detail: inspection.detail,
        target_paths: inspection.target_paths,
        discovered_profiles: inspection
            .discovered_profiles
            .into_iter()
            .map(|profile| DshDiscoveredProfile {
                home: profile.home,
                name: profile.name,
                path: profile.path,
            })
            .collect(),
        fingerprint: inspection.fingerprint,
        revision: ControlRevision::from_state(state),
        runtime_url: inspection.runtime_url,
        uninstall_supported: inspection.uninstall_supported,
        enabled: inspection.enabled,
        application: inspection.application.map(|outcome| match outcome {
            HostOutcome::Applied => DshApplicationOutcome::Applied,
            HostOutcome::RestartRequired => DshApplicationOutcome::RestartRequired,
            HostOutcome::Overridden => DshApplicationOutcome::Overridden,
            HostOutcome::Failed => DshApplicationOutcome::Failed,
            HostOutcome::Cancelled => DshApplicationOutcome::Cancelled,
        }),
    }
}

fn note_dsh(
    receipt: &mut DashboardReceipt,
    result: &Result<DshApplication, V3ApiError>,
    created: Option<(String, u64)>,
    install: bool,
) {
    match result {
        Ok(payload) => {
            let mut metadata = OperationMetadata {
                revision: Some(payload.revision.revision),
                ..OperationMetadata::default()
            };
            if let Some((id, _)) = &created {
                metadata.related_ids.push(id.clone());
                metadata.completed_count = Some(1);
            }
            if dsh_failed(payload, install) && created.is_some() {
                metadata.requested_count = Some(2);
                metadata.completed_count = Some(1);
                metadata.failed_count = Some(1);
                receipt.decide(OperationOutcome::Partial, Some("business.failed"), metadata);
            } else if dsh_failed(payload, install) {
                receipt.decide(OperationOutcome::Failed, Some("business.failed"), metadata);
            } else {
                receipt.succeed(metadata);
            }
        }
        Err(_) => {
            if let Some((id, revision)) = created {
                receipt.decide(
                    OperationOutcome::Partial,
                    None,
                    OperationMetadata {
                        revision: Some(revision),
                        requested_count: Some(2),
                        completed_count: Some(1),
                        failed_count: Some(1),
                        related_ids: vec![id],
                        ..OperationMetadata::default()
                    },
                );
            }
        }
    }
}

fn dsh_failed(payload: &DshApplication, install: bool) -> bool {
    if matches!(
        payload.application,
        Some(DshApplicationOutcome::Failed | DshApplicationOutcome::Cancelled)
    ) {
        return true;
    }
    match payload.status {
        DshApplicationStatus::Conflict
        | DshApplicationStatus::Incompatible
        | DshApplicationStatus::UnsupportedRuntime => true,
        DshApplicationStatus::NotDetected => install,
        DshApplicationStatus::Installed | DshApplicationStatus::Ready => false,
    }
}

fn map_host_error(state: &CoreState, error: DshApplicationError) -> V3ApiError {
    match error.kind {
        DshApplicationErrorKind::Invalid => V3ApiError::invalid_request_at(state, error.message),
        DshApplicationErrorKind::Precondition => {
            V3ApiError::precondition_failed_at(state, error.message)
        }
        DshApplicationErrorKind::Conflict => V3ApiError::conflict_at(state, error.message),
        DshApplicationErrorKind::Internal => V3ApiError::internal_at(state, error.message),
    }
}

/// Manual dashboard receipt. Construction stores intent only. `finish` writes
/// the one terminal row after caller locks have been dropped.
///
/// Error classification keeps the business response unchanged and stores a
/// bounded semantic reason through [`persistable_reason`].
pub(super) struct DashboardReceipt {
    op: UserOperation,
    decision: Option<ReceiptDecision>,
    /// Facts captured inside `commit_configuration_update_recorded` after the
    /// SQLite commit. Memory only; the row is written later by `finish`.
    committed: Option<OperationMetadata>,
}

struct ReceiptDecision {
    outcome: OperationOutcome,
    reason: Option<String>,
    metadata: OperationMetadata,
}

pub(super) trait ReceiptError {
    fn receipt_reason(&self) -> &str;
}

impl ReceiptError for V3ApiError {
    fn receipt_reason(&self) -> &str {
        self.operation_reason()
    }
}

/// Counts for a write that landed and a later step that did not.
pub(super) struct DurableEffect {
    pub revision: u64,
    pub completed: u32,
    pub failed: u32,
    pub related_ids: Vec<String>,
}

impl DurableEffect {
    pub(super) fn into_metadata(self) -> OperationMetadata {
        OperationMetadata {
            revision: Some(self.revision),
            requested_count: Some(self.completed.saturating_add(self.failed)),
            completed_count: Some(self.completed),
            failed_count: Some(self.failed),
            related_ids: self.related_ids,
            ..OperationMetadata::default()
        }
    }
}

/// The committed write is completed. The returned error is the one failed
/// follow-up step. Counts already supplied by the callback stay as captured.
fn partial_from_commit(metadata: OperationMetadata) -> OperationMetadata {
    let completed = metadata.completed_count.unwrap_or(0);
    let failed = metadata.failed_count.unwrap_or(1);
    OperationMetadata {
        completed_count: Some(completed),
        failed_count: Some(failed),
        requested_count: Some(
            metadata
                .requested_count
                .unwrap_or(completed.saturating_add(failed)),
        ),
        ..metadata
    }
}

impl DashboardReceipt {
    pub(super) fn open(
        state: &crate::state::CoreState,
        action: &'static str,
        subject_type: &'static str,
        subject_id: Option<String>,
    ) -> Self {
        let subject_id = subject_id.and_then(|id| opaque_subject(&id));
        Self {
            op: UserOperation::dashboard(state, action, subject_type, subject_id),
            decision: None,
            committed: None,
        }
    }

    /// Remember a configuration write that has already committed. `revision`
    /// is the value passed by `commit_configuration_update_recorded`.
    pub(super) fn note_committed(&mut self, metadata: OperationMetadata) {
        self.committed = Some(scrub_metadata(metadata));
    }

    /// Run a configuration mutation. The callback stores receipt facts after
    /// the owner's commit and before fallible publication. It does not log or
    /// take the database lock.
    pub(super) fn commit_recorded<T>(
        &mut self,
        state: &CoreState,
        facts: OperationMetadata,
        mutation: impl FnOnce(&crate::db::Database) -> crate::Result<T>,
    ) -> crate::Result<T> {
        state.commit_configuration_update_recorded(mutation, |revision| {
            let mut facts = facts;
            facts.revision = Some(revision);
            self.note_committed(facts);
        })
    }

    pub(super) fn use_operation_id(&mut self, id: uuid::Uuid) {
        self.op.use_operation_id(id);
    }

    pub(super) fn subject(&mut self, id: impl AsRef<str>) {
        if let Some(id) = opaque_subject(id.as_ref()) {
            self.op.subject(id);
        }
    }

    pub(super) fn decide(
        &mut self,
        outcome: OperationOutcome,
        reason: Option<&str>,
        metadata: OperationMetadata,
    ) {
        self.decision = Some(ReceiptDecision {
            outcome,
            reason: reason.map(persistable_reason),
            metadata: scrub_metadata(metadata),
        });
    }

    pub(super) fn succeed(&mut self, metadata: OperationMetadata) {
        self.decide(OperationOutcome::Success, None, metadata);
    }

    pub(super) fn finish<T, E: ReceiptError>(self, result: Result<T, E>) -> Result<T, E> {
        let DashboardReceipt {
            op,
            decision,
            committed,
        } = self;
        match decision {
            Some(decision) => {
                let reason = decision.reason.or_else(|| match &result {
                    Err(error) => Some(persistable_reason(error.receipt_reason())),
                    Ok(_) => None,
                });
                op.complete(decision.outcome, reason.as_deref(), decision.metadata);
            }
            None => match &result {
                Ok(_) => op.complete(
                    OperationOutcome::Success,
                    None,
                    committed.unwrap_or_default(),
                ),
                Err(error) => {
                    let raw = error.receipt_reason();
                    let reason = persistable_reason(raw);
                    if let Some(metadata) = committed {
                        op.complete(
                            OperationOutcome::Partial,
                            Some(reason.as_str()),
                            partial_from_commit(metadata),
                        );
                    } else {
                        op.complete(
                            classify_reason(raw),
                            Some(reason.as_str()),
                            OperationMetadata::default(),
                        );
                    }
                }
            },
        }
        result
    }

    /// Success uses `success`. An explicit durable effect on `Err` is Partial.
    /// A bare error is Partial when this receipt captured a committed
    /// configuration write, and otherwise uses the atomic V3 classification.
    pub(super) fn observe<T, E: ReceiptError>(
        mut self,
        result: Result<T, E>,
        durable: Option<DurableEffect>,
        success: impl FnOnce(&T) -> OperationMetadata,
    ) -> Result<T, E> {
        match &result {
            Ok(value) => self.succeed(success(value)),
            Err(_) => {
                if let Some(effect) = durable {
                    self.decide(OperationOutcome::Partial, None, effect.into_metadata());
                }
            }
        }
        self.finish(result)
    }
}

pub(super) fn opaque_subject(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed.len() > crate::log_types::MAX_OPAQUE_ID_LEN {
        return None;
    }
    let mut chars = trimmed.chars();
    let shape = matches!(chars.next(), Some(ch) if ch.is_ascii_alphanumeric())
        && chars.all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '.' | ':' | '@' | '-'));
    let lower = trimmed.to_ascii_lowercase();
    if !shape
        || lower.contains("sk-")
        || lower.contains("bearer")
        || lower.contains("api_key")
        || lower.contains("api-key")
        || lower.starts_with("eyj")
    {
        return None;
    }
    Some(trimmed.to_string())
}

pub(super) fn classify_reason(reason: &str) -> OperationOutcome {
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

pub(super) fn persistable_reason(code: &str) -> String {
    if !code
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '.')
    {
        return "failed".to_string();
    }
    let mut out = String::new();
    for ch in code.chars() {
        if ch.is_ascii_uppercase() {
            if !out.is_empty() && !out.ends_with('.') {
                out.push('.');
            }
            out.push(ch.to_ascii_lowercase());
        } else if ch.is_ascii_lowercase() || ch.is_ascii_digit() {
            out.push(ch);
        } else if ch == '.' && !out.is_empty() && !out.ends_with('.') {
            out.push('.');
        }
    }
    let out = out.trim_matches('.').to_string();
    if out.is_empty() || out.len() > crate::log_types::MAX_CODE_LEN || !stable_code(&out) {
        "failed".to_string()
    } else {
        out
    }
}

pub(super) fn batch_outcome(completed: u32, failed: u32) -> OperationOutcome {
    if failed == 0 {
        OperationOutcome::Success
    } else if completed == 0 {
        OperationOutcome::Failed
    } else {
        OperationOutcome::Partial
    }
}

pub(super) fn count_u32(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

fn stable_code(value: &str) -> bool {
    !value.is_empty()
        && value.split('.').all(|part| {
            let mut chars = part.chars();
            matches!(chars.next(), Some(ch) if ch.is_ascii_lowercase())
                && chars.all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit())
        })
}

fn scrub_metadata(metadata: OperationMetadata) -> OperationMetadata {
    OperationMetadata {
        changed_fields: metadata
            .changed_fields
            .into_iter()
            .filter(|name| field_name_ok(name))
            .take(crate::log_types::MAX_CHANGED_FIELDS)
            .collect(),
        related_ids: metadata
            .related_ids
            .into_iter()
            .filter(|id| opaque_subject(id).is_some())
            .take(crate::log_types::MAX_RELATED_IDS)
            .collect(),
        ..metadata
    }
}

fn field_name_ok(value: &str) -> bool {
    let mut chars = value.chars();
    value.len() <= crate::log_types::MAX_FIELD_NAME_LEN
        && matches!(chars.next(), Some(ch) if ch.is_ascii_alphabetic())
        && chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
        && !crate::redaction::is_sensitive_key(value)
        && !value.to_ascii_lowercase().contains("sk-")
}

#[cfg(test)]
pub(super) fn operation_receipts(
    state: &crate::state::CoreState,
) -> Vec<crate::log_types::OperationLog> {
    state
        .db
        .lock()
        .query_operation_logs(&crate::log_types::OperationLogQuery::default())
        .unwrap()
        .items
}

#[cfg(test)]
mod tests;

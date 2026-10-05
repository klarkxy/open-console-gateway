//! Owned CPA execution plane: one pinned host, one private config, one policy callback.
//!
//! Product control saves schedule one serialized desired-to-applied pass after
//! their own locks drop. Apply writes SQLite, drops that lock, and only then
//! starts the child.
//! Lock order: never take this plane lock while holding the database lock.
//! The policy callback takes its gate, then the database. Apply writes SQLite,
//! drops that lock, and only then starts the child.

mod artifact;
mod callback;
pub(crate) mod device;
pub(crate) mod explain;
mod identity;
mod io;
mod native;
mod project;
mod ready;
mod store;

#[cfg(test)]
mod tests;

use crate::cpa_policy::{Decision, OpportunityPolicy, PolicyService};
use crate::cpa_runtime::{CpaRuntimePhase, CpaRuntimeProcessSpec, CpaRuntimeSecret, ManagedCpa};
use crate::routing_snapshot::RoutingSnapshot;
use crate::state::{CoreState, CoreStateInner};
use artifact::{PINNED_VERSION, PINNED_VERSION_CANONICAL};
use callback::{CallbackContext, CorrelationView, LivePolicy, SqlitePolicyStore};
use identity::PinnedAttempt;
use io::MAX_CONFIG_BYTES;
use parking_lot::Mutex;
use project::origin_of;
use ready::AcceptedReady;
use rusqlite::OptionalExtension;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use store::Record;
use uuid::Uuid;
use zeroize::Zeroize;

const CORRELATION_CAP: usize = 1024;
const ATTEMPT_PIN_CAP: usize = 32;

pub struct ExecutionPlane {
    inner: Mutex<Inner>,
}

struct Inner {
    poisoned: bool,
    record: Record,
    child_generation: u64,
    verified_ready: bool,
    artifact_dir: Option<PathBuf>,
    secrets: Option<Secrets>,
    policy: Option<Arc<LivePolicy>>,
    token_stamp: String,
    public_origin: String,
    phase: CpaRuntimePhase,
    error: Option<String>,
    correlations: HashMap<Uuid, Correlation>,
}

struct Correlation {
    deadline: chrono::DateTime<chrono::Utc>,
    retain_until: chrono::DateTime<chrono::Utc>,
    cancelled: bool,
    decision: Option<Decision>,
    /// Identity captured for each attempt UUID. A later attempt must not reuse an earlier pin.
    pins: Arc<Mutex<HashMap<Uuid, PinnedAttempt>>>,
    /// Attempt UUIDs that already published one result. Duplicate bodies are not retained.
    published: Arc<Mutex<HashSet<Uuid>>>,
    /// Absent on the deadline helper. An admit without this stops the logical request.
    authorization: Option<CorrelationAuthorization>,
    requested_model: String,
    /// Immutable public trace. Absent until `register_correlation_trace`.
    client_trace_id: Option<String>,
}

/// Caller captured with the logical request. Not a public DTO and not a credential selector.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CorrelationAuthorization {
    Client {
        key_id: String,
        captured_key_fingerprint: String,
    },
    Validated {
        credential_id: String,
        credential_version: u64,
        requested_protocol: String,
    },
}

#[derive(Clone, Debug)]
pub struct CorrelationIntent {
    pub request_id: Uuid,
    pub deadline: chrono::DateTime<chrono::Utc>,
    pub requested_model: String,
    pub authorization: CorrelationAuthorization,
}

#[derive(Clone)]
pub(crate) struct Secrets {
    hop: Secret,
    policy: Secret,
    ready: Secret,
}

struct Secret(String);

impl Secret {
    fn expose(&self) -> &str {
        &self.0
    }
}

impl Clone for Secret {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("[redacted]")
    }
}

impl std::fmt::Debug for Secrets {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Secrets([redacted])")
    }
}

impl std::fmt::Debug for ExecutionPlane {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let inner = self.inner.lock();
        formatter
            .debug_struct("ExecutionPlane")
            .field("poisoned", &inner.poisoned)
            .field("apply_status", &inner.record.apply_status)
            .field("desired_revision", &inner.record.desired_revision)
            .field("applied_revision", &inner.record.applied_revision)
            .field("child_generation", &inner.child_generation)
            .field("verified_ready", &inner.verified_ready)
            .field("secrets", &"[redacted]")
            .finish()
    }
}

#[derive(Debug)]
pub enum ExecutionError {
    Unavailable(String),
    Invalid(String),
    ApplyFailed(String),
    ApplyConflict(String),
    RollbackUnavailable,
}

impl std::fmt::Display for ExecutionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable(message)
            | Self::Invalid(message)
            | Self::ApplyFailed(message)
            | Self::ApplyConflict(message) => formatter.write_str(message),
            Self::RollbackUnavailable => formatter.write_str("rollback_unavailable"),
        }
    }
}

impl std::error::Error for ExecutionError {}

#[derive(Clone, Debug)]
pub struct ExecutionReport {
    pub installed: bool,
    pub running: bool,
    pub desired_running: bool,
    pub owned: bool,
    pub current_version: Option<String>,
    pub previous_version: Option<String>,
    pub asset_sha256: Option<String>,
    pub port: Option<u16>,
    pub base_url: Option<String>,
    pub phase: CpaRuntimePhase,
    pub error: Option<String>,
    pub latest_version: Option<String>,
    pub update_available: bool,
    pub current_operation: Option<String>,
    pub apply_status: String,
    pub policy_ready: bool,
    pub inference_ready: bool,
    pub listener_bound: bool,
    pub child_generation: u64,
    pub desired_revision: u64,
    pub applied_revision: u64,
    pub desired_digest: String,
    pub applied_digest: String,
    pub unavailable: bool,
}

impl ExecutionPlane {
    pub(crate) fn open(db: &crate::db::Database, public_generation: u64) -> Self {
        let (poisoned, record) = match store::load(&db.conn) {
            Ok(Some(record)) => (false, record),
            Ok(None) => (false, Record::empty()),
            Err(()) => (true, Record::empty()),
        };
        Self {
            inner: Mutex::new(Inner {
                poisoned,
                record,
                child_generation: mint_child(public_generation),
                verified_ready: false,
                artifact_dir: None,
                secrets: None,
                policy: None,
                token_stamp: String::new(),
                public_origin: String::new(),
                phase: CpaRuntimePhase::Idle,
                error: None,
                correlations: HashMap::new(),
            }),
        }
    }

    fn callback_context(&self) -> Option<CallbackContext> {
        let inner = self.inner.lock();
        let service = inner.policy.clone()?;
        let secrets = inner.secrets.as_ref()?;
        if inner.public_origin.is_empty() {
            return None;
        }
        Some(CallbackContext {
            service,
            origin: inner.public_origin.clone(),
            token: secrets.policy.expose().to_string(),
        })
    }

    fn correlation_for(&self, request_id: Uuid) -> Option<CorrelationView> {
        let mut inner = self.inner.lock();
        let now = chrono::Utc::now();
        inner.correlations.retain(|_, item| now < item.retain_until);
        inner
            .correlations
            .get(&request_id)
            .map(|item| CorrelationView {
                deadline: item.deadline,
                cancelled: item.cancelled,
                pins: Arc::clone(&item.pins),
                published: Arc::clone(&item.published),
                authorization: item.authorization.clone(),
                requested_model: item.requested_model.clone(),
                client_trace_id: item.client_trace_id.clone(),
            })
    }

    fn remember_decision(&self, request_id: Uuid, decision: Decision) {
        let mut inner = self.inner.lock();
        if let Some(item) = inner.correlations.get_mut(&request_id) {
            item.decision = Some(decision);
        }
    }

    fn note_published(&self, request_id: Uuid, attempt_id: Uuid) {
        let inner = self.inner.lock();
        if let Some(item) = inner.correlations.get(&request_id) {
            item.published.lock().insert(attempt_id);
        }
    }

    fn mark_unavailable(&self) {
        let mut inner = self.inner.lock();
        inner.record.unavailable = true;
        inner.verified_ready = false;
        inner.record.policy_ready = false;
    }
}

pub(super) fn note_unavailable(state: &CoreState) {
    state.cpa_execution.mark_unavailable();
    let record = state.cpa_execution.inner.lock().record.clone();
    let _ = persist(state, &record);
}

pub fn execution_report(state: &CoreState) -> ExecutionReport {
    let previous_version = crate::cpa_runtime::load_managed(&state.data_dir())
        .ok()
        .flatten()
        .and_then(|item| item.previous_version);
    let inner = state.cpa_execution.inner.lock();
    let running = state
        .cpa_runtime
        .installed_host()
        .ok()
        .is_some_and(|host| host.owned_running());
    let listener_bound = state.gateway.lock().is_some();
    let installed =
        artifact::installed_executable(&state.data_dir, &inner.record.artifact_sha256).is_some();
    let operation = inner.record.apply_status.clone();
    let port = (inner.record.listen_port != 0).then_some(inner.record.listen_port);
    ExecutionReport {
        installed,
        running,
        desired_running: inner.record.desired_running,
        owned: installed,
        current_version: installed.then(|| PINNED_VERSION_CANONICAL.to_string()),
        previous_version,
        asset_sha256: installed.then(|| inner.record.artifact_sha256.clone()),
        port,
        base_url: port.map(|port| format!("http://127.0.0.1:{port}")),
        phase: inner.phase.clone(),
        error: inner.error.clone(),
        latest_version: Some(PINNED_VERSION.to_string()),
        update_available: false,
        current_operation: Some(operation),
        apply_status: inner.record.apply_status.clone(),
        policy_ready: inner.record.policy_ready,
        inference_ready: inner.verified_ready
            && inner.record.policy_ready
            && running
            && !inner.record.unavailable,
        listener_bound,
        child_generation: inner.child_generation,
        desired_revision: inner.record.desired_revision,
        applied_revision: inner.record.applied_revision,
        desired_digest: inner.record.desired_digest.clone(),
        applied_digest: inner.record.applied_digest.clone(),
        unavailable: inner.record.unavailable || inner.poisoned,
    }
}

pub fn set_artifact_dir(state: &CoreState, dir: PathBuf) {
    state.cpa_execution.inner.lock().artifact_dir = Some(dir);
}

pub fn install(
    state: &CoreState,
    expected_revision: u64,
    expected_generation: u64,
    expected_version: Option<&str>,
) -> Result<ExecutionReport, ExecutionError> {
    artifact::version_accepted(expected_version)?;
    let _settings = state.settings_update.lock();
    check_cas(state, expected_revision, expected_generation)?;
    let artifact = load_artifact(state)?;
    let executable = artifact::install(&state.data_dir, &artifact)?;
    let _ = executable;
    let mut record = {
        let inner = state.cpa_execution.inner.lock();
        if inner.poisoned {
            return Err(ExecutionError::Unavailable(
                "cpa execution record is unreadable".into(),
            ));
        }
        inner.record.clone()
    };
    record.artifact_sha256 = artifact.sha256;
    if record.apply_status == "not_prepared" {
        record.apply_status = "installed".into();
    }
    record.desired_running = false;
    persist(state, &record)?;
    {
        let mut inner = state.cpa_execution.inner.lock();
        inner.record = record;
        inner.phase = CpaRuntimePhase::Idle;
        inner.error = None;
    }
    drop(_settings);
    Ok(execution_report(state))
}

pub async fn start(
    state: &CoreState,
    expected_revision: u64,
    expected_generation: u64,
) -> Result<ExecutionReport, ExecutionError> {
    // A process that is already up, with no projection to install, only records
    // the run intent. A prepared plane still applies so a new projection can
    // replace the running child.
    let already_running = state
        .cpa_runtime
        .installed_host()
        .ok()
        .is_some_and(|host| host.owned_running());
    let unprepared = {
        let record = &state.cpa_execution.inner.lock().record;
        record.apply_status == "not_prepared" && record.artifact_sha256.is_empty()
    };
    if already_running && unprepared {
        return persist_already_running_intent(state, expected_revision, expected_generation);
    }
    apply(state, Some((expected_revision, expected_generation)), true).await
}

fn persist_already_running_intent(
    state: &CoreState,
    expected_revision: u64,
    expected_generation: u64,
) -> Result<ExecutionReport, ExecutionError> {
    let _settings = state.settings_update.lock();
    check_cas(state, expected_revision, expected_generation)?;
    let mut record = state.cpa_execution.inner.lock().record.clone();
    record.desired_running = true;
    persist(state, &record)?;
    {
        let mut inner = state.cpa_execution.inner.lock();
        inner.record = record;
        inner.phase = CpaRuntimePhase::Idle;
        inner.error = None;
    }
    state.bump_settings_revision();
    Ok(execution_report(state))
}

pub async fn update(
    state: &CoreState,
    expected_revision: u64,
    expected_generation: u64,
    expected_version: Option<&str>,
) -> Result<ExecutionReport, ExecutionError> {
    install(
        state,
        expected_revision,
        expected_generation,
        expected_version,
    )?;
    state.cpa_runtime.cancel_device_login();
    if let Ok(host) = state.cpa_runtime.installed_host() {
        let _ = host.stop_owned();
    }
    let revision = state.settings_revision();
    start(state, revision, expected_generation).await
}

pub fn stop(
    state: &CoreState,
    expected_revision: u64,
    expected_generation: u64,
) -> Result<ExecutionReport, ExecutionError> {
    let _settings = state.settings_update.lock();
    check_cas(state, expected_revision, expected_generation)?;
    state.cpa_runtime.cancel_device_login();
    if let Ok(host) = state.cpa_runtime.installed_host() {
        host.stop_owned().map_err(|_| {
            ExecutionError::Unavailable("owned CPA host could not be stopped".into())
        })?;
    }
    let mut record = state.cpa_execution.inner.lock().record.clone();
    record.desired_running = false;
    record.policy_ready = false;
    if record.apply_status == "applied" || record.apply_status == "apply_pending" {
        record.apply_status = "stopped".into();
    }
    persist(state, &record)?;
    {
        let mut inner = state.cpa_execution.inner.lock();
        inner.record = record;
        inner.verified_ready = false;
        inner.phase = CpaRuntimePhase::Idle;
        inner.error = None;
    }
    let _ = write_managed(state, false);
    state.bump_settings_revision();
    Ok(execution_report(state))
}

pub fn remove(
    state: &CoreState,
    expected_revision: u64,
    expected_generation: u64,
) -> Result<ExecutionReport, ExecutionError> {
    let _settings = state.settings_update.lock();
    check_cas(state, expected_revision, expected_generation)?;
    state.cpa_runtime.cancel_device_login();
    if let Ok(host) = state.cpa_runtime.installed_host() {
        let _ = host.stop_owned();
    }
    let data_dir = state.data_dir();
    let sha = state
        .cpa_execution
        .inner
        .lock()
        .record
        .artifact_sha256
        .clone();
    let _ = std::fs::remove_file(io::config_path(&data_dir));
    let _ = std::fs::remove_file(io::previous_config_path(&data_dir));
    let _ = std::fs::remove_file(crate::cpa_runtime::managed_path(&data_dir));
    if sha.len() == 64 {
        let _ = std::fs::remove_dir_all(io::version_dir(&data_dir, &sha));
    }
    delete_secrets(&data_dir);
    {
        let db = state.db.lock();
        db.conn
            .execute("DELETE FROM settings WHERE key = ?1", [store::SETTINGS_KEY])
            .map_err(|_| {
                ExecutionError::Unavailable("CPA execution record could not be removed".into())
            })?;
    }
    {
        let mut inner = state.cpa_execution.inner.lock();
        inner.record = Record::empty();
        inner.secrets = None;
        inner.policy = None;
        inner.verified_ready = false;
        inner.phase = CpaRuntimePhase::Idle;
        inner.error = None;
        inner.public_origin.clear();
    }
    Ok(execution_report(state))
}

pub(crate) const EXECUTION_RECORD_KEY: &str = store::SETTINGS_KEY;

pub(crate) fn rebase_archived_execution_record(
    json: &str,
    current_before: &str,
    current_after: &str,
    previous_before: Option<&str>,
    previous_after: Option<&str>,
) -> Result<Option<String>, ExecutionError> {
    store::rebase_archived_execution_record(
        json,
        current_before,
        current_after,
        previous_before,
        previous_after,
    )
}

struct RollbackSource {
    generation: u64,
    revision: u64,
    digest: String,
    auth: Vec<store::AuthStamp>,
    routes: Vec<crate::cpa_projection::CredentialRouteSet>,
    listen_port: u16,
}

fn select_rollback_source(record: &Record, yaml: &str) -> Result<RollbackSource, ExecutionError> {
    if store::applied_matches_yaml(record, yaml) {
        return Ok(RollbackSource {
            generation: record.applied_generation,
            revision: record.applied_revision,
            digest: record.applied_digest.clone(),
            auth: record.applied_auth.clone(),
            routes: record.applied_routes.clone(),
            listen_port: record.listen_port,
        });
    }
    if let Some(snapshot) = &record.previous_accepted {
        if store::snapshot_matches_yaml(snapshot, yaml) {
            if !snapshot
                .artifact_sha256
                .eq_ignore_ascii_case(&record.artifact_sha256)
            {
                return Err(ExecutionError::RollbackUnavailable);
            }
            return Ok(RollbackSource {
                generation: snapshot.generation,
                revision: snapshot.revision,
                digest: snapshot.wire_digest.clone(),
                auth: snapshot.auth.clone(),
                routes: snapshot.routes.clone(),
                listen_port: snapshot.listen_port,
            });
        }
    }
    Err(ExecutionError::RollbackUnavailable)
}

fn accepted_file_bytes(record: &Record, bytes: Vec<u8>) -> Option<Vec<u8>> {
    let text = String::from_utf8(bytes.clone()).ok()?;
    let matches = store::applied_matches_yaml(record, &text)
        || record
            .previous_accepted
            .as_ref()
            .is_some_and(|snapshot| store::snapshot_matches_yaml(snapshot, &text));
    matches.then_some(bytes)
}

pub async fn rollback(
    state: &CoreState,
    expected_revision: u64,
    expected_generation: u64,
) -> Result<ExecutionReport, ExecutionError> {
    let hooks = current_hooks();
    {
        let _settings = state.settings_update.lock();
        check_cas(state, expected_revision, expected_generation)?;
    }
    let previous_bytes = io::read_bounded(&io::previous_config_path(&state.data_dir))?
        .ok_or(ExecutionError::RollbackUnavailable)?;
    let previous_text = String::from_utf8(previous_bytes)
        .map_err(|_| ExecutionError::Invalid("CPA config is not UTF-8".into()))?;
    yaml_identity(&previous_text)?;
    let persisted = {
        let db = state.db.lock();
        match store::load(&db.conn) {
            Ok(Some(record)) => record,
            Ok(None) => Record::empty(),
            Err(()) => {
                return Err(ExecutionError::Unavailable(
                    "cpa execution record is unreadable".into(),
                ));
            }
        }
    };
    let source = select_rollback_source(&persisted, &previous_text)?;
    let recovery_yaml = io::read_bounded(&io::config_path(&state.data_dir))?
        .and_then(|bytes| accepted_file_bytes(&persisted, bytes));
    let origin = listener_origin(state)?;
    let password = management_password(state)?;
    ensure_secrets(state)?;
    ensure_policy(state, &origin)?;
    let auth_dir = io::portable_auth_dir(&state.data_dir)?;
    let child_generation = state.cpa_execution.inner.lock().child_generation;
    let secrets = state
        .cpa_execution
        .inner
        .lock()
        .secrets
        .clone()
        .ok_or_else(|| ExecutionError::Unavailable("CPA secrets are unavailable".into()))?;
    // The previous file matched one accepted plane. Mutation starts here.
    state.cpa_runtime.cancel_device_login();
    let mut envelope = project::OwnedEnvelope {
        process_generation: child_generation,
        listen_port: source.listen_port,
        auth_dir,
        policy_url: format!("{origin}/_internal/ocg/cpa-policy"),
        policy_token: secrets.policy.expose().to_string(),
        policy_origin: origin.clone(),
        ready_key: secrets.ready.expose().to_string(),
        hop_secret: secrets.hop.expose().to_string(),
    };
    let prepared = if project::envelope_matches(&previous_text, &envelope) {
        Ok((previous_text, source.digest.clone(), source.listen_port))
    } else {
        match stop_host(state) {
            Ok(()) => match io::reserve_loopback_port(source.listen_port) {
                Ok(port) => {
                    envelope.listen_port = port;
                    match project::rebind_proven_envelope(
                        &previous_text,
                        source.generation,
                        source.revision,
                        &source.digest,
                        &envelope,
                    ) {
                        Ok(rebound) => Ok((rebound.yaml, rebound.wire_digest, port)),
                        Err(error) => Err(error),
                    }
                }
                Err(error) => Err(error),
            },
            Err(error) => Err(error),
        }
    };
    let (output, digest, port) = match prepared {
        Ok(prepared) => prepared,
        Err(error) => {
            return Err(restore_rejected_rollback(
                state,
                &persisted,
                recovery_yaml.as_deref(),
                &persisted,
                error,
                &password,
                &hooks,
            )
            .await);
        }
    };
    let mut working = persisted.clone();
    working.desired_auth = source.auth;
    working.desired_routes = source.routes;
    working.desired_revision = source.revision;
    working.desired_digest = digest;
    working.child_generation = child_generation;
    working.listen_port = port;
    working.owned_origin = format!("http://127.0.0.1:{port}");
    working.public_origin = origin.clone();
    working.apply_status = "apply_pending".into();
    working.policy_ready = false;
    working.host_capabilities.clear();
    working.unavailable = false;
    if let Err(error) = persist(state, &working) {
        return Err(restore_rejected_rollback(
            state,
            &persisted,
            recovery_yaml.as_deref(),
            &working,
            error,
            &password,
            &hooks,
        )
        .await);
    }
    {
        let mut inner = state.cpa_execution.inner.lock();
        inner.record = working.clone();
        inner.public_origin = origin.clone();
        inner.verified_ready = false;
        inner.phase = CpaRuntimePhase::Starting;
        inner.error = None;
    }
    if let Err(error) =
        io::atomic_write_private(&io::config_path(&state.data_dir), output.as_bytes())
    {
        return Err(restore_rejected_rollback(
            state,
            &persisted,
            recovery_yaml.as_deref(),
            &working,
            error,
            &password,
            &hooks,
        )
        .await);
    }
    let started = launch(state, &password, port, &hooks).await;
    let started = if hooks.fail_ready {
        Err(ExecutionError::ApplyFailed(
            "cpa_apply_failed: ready identity rejected".into(),
        ))
    } else {
        started
    };
    let body = match started {
        Ok(body) => body,
        Err(error) => {
            return Err(restore_rejected_rollback(
                state,
                &persisted,
                recovery_yaml.as_deref(),
                &working,
                error,
                &password,
                &hooks,
            )
            .await);
        }
    };
    let accepted = match ready::accept_ready(&body, &working, &secrets) {
        Ok(accepted) => accepted,
        Err(error) => {
            return Err(restore_rejected_rollback(
                state,
                &persisted,
                recovery_yaml.as_deref(),
                &working,
                error,
                &password,
                &hooks,
            )
            .await);
        }
    };
    if let Err(error) = ready::note_keyed_registration_epochs(&mut working.desired_auth, &body) {
        return Err(restore_rejected_rollback(
            state,
            &persisted,
            recovery_yaml.as_deref(),
            &working,
            error,
            &password,
            &hooks,
        )
        .await);
    }
    if let Some(ref hook) = hooks.before_commit {
        hook();
    }
    let committed = {
        let _settings = state.settings_update.lock();
        if let Err(error) = check_cas(state, expected_revision, expected_generation) {
            Err(error)
        } else {
            working.applied_generation = working.child_generation;
            working.applied_revision = working.desired_revision;
            working.applied_digest = working.desired_digest.clone();
            working.applied_auth = working.desired_auth.clone();
            working.applied_routes = working.desired_routes.clone();
            working.apply_status = "applied".into();
            working.policy_ready = true;
            working.unavailable = false;
            working.desired_running = true;
            working.host_capabilities = accepted.capabilities;
            match persist(state, &working) {
                Ok(()) => Ok(working.clone()),
                Err(error) => Err(error),
            }
        }
    };
    let committed = match committed {
        Ok(record) => record,
        Err(error) => {
            return Err(restore_rejected_rollback(
                state,
                &persisted,
                recovery_yaml.as_deref(),
                &working,
                error,
                &password,
                &hooks,
            )
            .await);
        }
    };
    {
        let mut inner = state.cpa_execution.inner.lock();
        inner.record = committed;
        inner.verified_ready = true;
        inner.phase = CpaRuntimePhase::Idle;
        inner.error = None;
    }
    let _ = write_managed(state, true);
    Ok(execution_report(state))
}

async fn restore_rejected_rollback(
    state: &CoreState,
    recovery: &Record,
    recovery_yaml: Option<&[u8]>,
    attempted: &Record,
    original: ExecutionError,
    password: &str,
    hooks: &Hooks,
) -> ExecutionError {
    let mut failure = None;
    if let Some(bytes) = recovery_yaml {
        if let Err(error) = io::atomic_write_private(&io::config_path(&state.data_dir), bytes) {
            failure = Some(error);
        }
    } else {
        failure = Some(ExecutionError::RollbackUnavailable);
    }
    let _ = stop_host(state);
    if failure.is_none() {
        let mut probe = recovery.clone();
        probe.child_generation = recovery.applied_generation;
        probe.desired_revision = recovery.applied_revision;
        probe.desired_digest = recovery.applied_digest.clone();
        {
            let mut inner = state.cpa_execution.inner.lock();
            inner.record = probe;
            inner.verified_ready = false;
        }
        let port = recovery_yaml
            .and_then(|bytes| std::str::from_utf8(bytes).ok())
            .and_then(yaml_listen_port)
            .unwrap_or(recovery.listen_port);
        match launch(
            state,
            password,
            port,
            &Hooks {
                fail_ready: false,
                skip_spawn: hooks.skip_spawn,
                before_commit: None,
            },
        )
        .await
        {
            Ok(body) => {
                let secrets = state.cpa_execution.inner.lock().secrets.clone();
                match secrets
                    .as_ref()
                    .map(|secrets| ready::accept_restored(&body, recovery, secrets))
                {
                    Some(Ok(accepted)) => {
                        let mut restored = recovery.clone();
                        match ready::note_applied_keyed_epochs(&mut restored, &body) {
                            Ok(()) => {
                                restored.apply_status = "applied".into();
                                restored.policy_ready = true;
                                restored.unavailable = false;
                                restored.host_capabilities = accepted.capabilities;
                                if persist(state, &restored).is_ok() {
                                    let mut inner = state.cpa_execution.inner.lock();
                                    inner.record = restored;
                                    inner.verified_ready = true;
                                    inner.phase = CpaRuntimePhase::Idle;
                                    inner.error = None;
                                    return original;
                                }
                                failure = Some(ExecutionError::Unavailable(
                                    "CPA execution record could not be saved".into(),
                                ));
                            }
                            Err(error) => {
                                let mut failed = recovery.clone();
                                failed.applied_auth = restored.applied_auth;
                                failure = Some(error);
                                mark_rollback_unrestored(state, &mut failed, attempted);
                                return failure.unwrap_or(original);
                            }
                        }
                    }
                    Some(Err(error)) => failure = Some(error),
                    None => {
                        failure = Some(ExecutionError::Unavailable(
                            "CPA secrets are unavailable".into(),
                        ));
                    }
                }
            }
            Err(error) => failure = Some(error),
        }
    }
    let mut failed = recovery.clone();
    mark_rollback_unrestored(state, &mut failed, attempted);
    failure.unwrap_or(original)
}

fn mark_rollback_unrestored(state: &CoreState, failed: &mut Record, attempted: &Record) {
    failed.desired_auth = attempted.desired_auth.clone();
    failed.desired_routes = attempted.desired_routes.clone();
    failed.desired_revision = attempted.desired_revision;
    failed.desired_digest = attempted.desired_digest.clone();
    failed.child_generation = attempted.child_generation;
    failed.listen_port = attempted.listen_port;
    failed.owned_origin = attempted.owned_origin.clone();
    failed.public_origin = attempted.public_origin.clone();
    failed.apply_status = "apply_failed".into();
    failed.policy_ready = false;
    failed.unavailable = true;
    failed.host_capabilities.clear();
    let _ = persist(state, failed);
    let mut inner = state.cpa_execution.inner.lock();
    inner.record = failed.clone();
    inner.verified_ready = false;
    inner.phase = CpaRuntimePhase::Failed;
    inner.error = Some("rollback_unavailable".into());
}

/// Serialized desired-to-applied pass. A fresh or stopped plane is a no-op.
/// Callers must not already hold `cpa_operations`.
pub async fn schedule_owned_apply(state: &CoreState) -> Result<ExecutionReport, ExecutionError> {
    let _operation = state.cpa_operations.lock().await;
    let skipped = {
        let inner = state.cpa_execution.inner.lock();
        inner.poisoned
            || !inner.record.desired_running
            || inner.record.apply_status == "not_prepared"
    };
    if skipped {
        return Ok(execution_report(state));
    }
    apply(
        state,
        Some((state.settings_revision(), state.process_generation())),
        true,
    )
    .await
}

/// Device-completion apply. Rechecks the noted completion and the poll guard
/// after `cpa_operations` is acquired, then calls private `apply`.
/// A cancel ack's newer settings revision is not absorbed.
/// Callers must not already hold `cpa_operations`.
pub(crate) async fn schedule_owned_device_apply(
    state: &CoreState,
    completion: &device::DeviceCompletion,
    guard: &device::DeviceOperationGuard,
) -> Result<ExecutionReport, ExecutionError> {
    #[cfg(test)]
    await_device_apply_handoff_pause().await;
    let _operation = state.cpa_operations.lock().await;
    if guard.cancelled() {
        return Err(ExecutionError::ApplyConflict("revisionConflict".into()));
    }
    device::completion_current(state, completion)?;
    let skipped = {
        let inner = state.cpa_execution.inner.lock();
        inner.poisoned
            || !inner.record.desired_running
            || inner.record.apply_status == "not_prepared"
    };
    if skipped {
        return Ok(execution_report(state));
    }
    let revision = state.settings_revision();
    let generation = state.process_generation();
    apply(state, Some((revision, generation)), true).await
}

/// Product save receipt stays successful when the later apply fails.
pub async fn note_product_apply(state: &CoreState) {
    if let Err(error) = schedule_owned_apply(state).await {
        state.log_runtime_event(
            "error",
            "cpa",
            &format!("event=cpa_apply_failed reason={error}"),
        );
    }
}

pub async fn restore_pinned_if_desired(state: &CoreState) {
    let _operation = state.cpa_operations.lock().await;
    let ready = {
        let inner = state.cpa_execution.inner.lock();
        !inner.poisoned && inner.record.desired_running && !inner.record.artifact_sha256.is_empty()
    };
    if !ready {
        return;
    }
    let revision = state.settings_revision();
    let generation = state.process_generation();
    if let Err(error) = apply(state, Some((revision, generation)), true).await {
        eprintln!("CPA execution is unavailable: {error}");
        note_unavailable(state);
    }
}

pub fn register_correlation(
    state: &CoreState,
    request_id: Uuid,
    deadline: chrono::DateTime<chrono::Utc>,
) -> Result<(), ExecutionError> {
    insert_correlation(state, request_id, deadline, None, String::new())
}

/// Typed caller intent. The same id, deadline, model, and authorization may repeat.
/// A different deadline, model, or authorization is rejected.
pub fn register_correlation_intent(
    state: &CoreState,
    intent: CorrelationIntent,
) -> Result<(), ExecutionError> {
    if !intent_present(&intent) {
        return Err(ExecutionError::Invalid("correlation_intent".into()));
    }
    insert_correlation(
        state,
        intent.request_id,
        intent.deadline,
        Some(intent.authorization),
        intent.requested_model,
    )
}

fn intent_present(intent: &CorrelationIntent) -> bool {
    if intent.requested_model.trim().is_empty() {
        return false;
    }
    match &intent.authorization {
        CorrelationAuthorization::Client {
            key_id,
            captured_key_fingerprint,
        } => !key_id.trim().is_empty() && !captured_key_fingerprint.is_empty(),
        CorrelationAuthorization::Validated {
            credential_id,
            requested_protocol,
            ..
        } => !credential_id.trim().is_empty() && !requested_protocol.trim().is_empty(),
    }
}

fn insert_correlation(
    state: &CoreState,
    request_id: Uuid,
    deadline: chrono::DateTime<chrono::Utc>,
    authorization: Option<CorrelationAuthorization>,
    requested_model: String,
) -> Result<(), ExecutionError> {
    let retain_until = deadline
        .checked_add_signed(chrono::Duration::seconds(
            crate::cpa_policy::RESULT_PUBLICATION_GRACE_SECONDS,
        ))
        .ok_or_else(|| ExecutionError::Invalid("correlation_deadline".into()))?;
    let now = chrono::Utc::now();
    if retain_until <= now {
        return Err(ExecutionError::Invalid("correlation_deadline".into()));
    }
    let mut inner = state.cpa_execution.inner.lock();
    inner.correlations.retain(|_, item| now < item.retain_until);
    if let Some(existing) = inner.correlations.get(&request_id) {
        if existing.deadline == deadline
            && existing.requested_model == requested_model
            && existing.authorization == authorization
        {
            return Ok(());
        }
        return Err(ExecutionError::Invalid("correlation_conflict".into()));
    }
    if inner.correlations.len() >= CORRELATION_CAP {
        return Err(ExecutionError::Invalid("correlation_capacity".into()));
    }
    inner.correlations.insert(
        request_id,
        Correlation {
            deadline,
            retain_until,
            cancelled: false,
            decision: None,
            pins: Arc::new(Mutex::new(HashMap::new())),
            published: Arc::new(Mutex::new(HashSet::new())),
            authorization,
            requested_model,
            client_trace_id: None,
        },
    );
    Ok(())
}

/// Store one immutable public trace on an existing correlation.
///
/// `generated_trace_id` is the ingress `ocg-` request id. A missing correlation
/// or a second different trace is an error. The same trace may repeat.
pub(crate) fn register_correlation_trace(
    state: &CoreState,
    request_id: Uuid,
    generated_trace_id: &str,
) -> Result<(), ExecutionError> {
    let canonical = canonical_public_trace(generated_trace_id)
        .ok_or_else(|| ExecutionError::Invalid("correlation_trace".into()))?;
    let mut inner = state.cpa_execution.inner.lock();
    let Some(existing) = inner.correlations.get_mut(&request_id) else {
        return Err(ExecutionError::Invalid("correlation_missing".into()));
    };
    match existing.client_trace_id.as_deref() {
        Some(current) if current == canonical => Ok(()),
        Some(_) => Err(ExecutionError::Invalid("correlation_trace".into())),
        None => {
            existing.client_trace_id = Some(canonical);
            Ok(())
        }
    }
}

pub(crate) fn correlation_client_trace(state: &CoreState, request_id: Uuid) -> Option<String> {
    state
        .cpa_execution
        .inner
        .lock()
        .correlations
        .get(&request_id)
        .and_then(|item| item.client_trace_id.clone())
}

pub(crate) fn correlation_authorization_kind(
    state: &CoreState,
    request_id: Uuid,
) -> Option<&'static str> {
    let inner = state.cpa_execution.inner.lock();
    inner
        .correlations
        .get(&request_id)
        .map(|item| match &item.authorization {
            Some(CorrelationAuthorization::Client { .. }) => "client",
            Some(CorrelationAuthorization::Validated { .. }) => "validated",
            None => "absent",
        })
}

pub(crate) fn correlation_cancelled(state: &CoreState, request_id: Uuid) -> Option<bool> {
    state
        .cpa_execution
        .inner
        .lock()
        .correlations
        .get(&request_id)
        .map(|item| item.cancelled)
}

fn canonical_public_trace(trace: &str) -> Option<String> {
    let rest = trace.strip_prefix("ocg-")?;
    if rest.len() != 36 {
        return None;
    }
    let parsed = Uuid::parse_str(rest).ok()?;
    Some(format!("ocg-{parsed}"))
}

pub fn cancel_correlation(state: &CoreState, request_id: Uuid) {
    if let Some(item) = state
        .cpa_execution
        .inner
        .lock()
        .correlations
        .get_mut(&request_id)
    {
        item.cancelled = true;
    }
}

pub fn correlation_decision(state: &CoreState, request_id: Uuid) -> Option<Decision> {
    state
        .cpa_execution
        .inner
        .lock()
        .correlations
        .get(&request_id)
        .and_then(|item| item.decision.clone())
}

/// Private hop for the owned child. Debug is redacted and this type is not serialized.
pub(crate) struct OwnedInferenceHop(Secret);

impl OwnedInferenceHop {
    pub(crate) fn expose(&self) -> &str {
        self.0.expose()
    }
}

impl std::fmt::Debug for OwnedInferenceHop {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("[redacted]")
    }
}

#[derive(Debug)]
pub(crate) struct OwnedInferenceConnection {
    pub base_url: String,
    pub hop: OwnedInferenceHop,
    pub child_generation: u64,
    pub applied_revision: u64,
    pub applied_digest: String,
}

/// Verified loopback of the owned child, its redacted hop, and the applied identity.
/// A missing host, a stopped child, or an origin that is not this plane's loopback refuses.
pub(crate) fn owned_inference_connection(
    state: &CoreState,
) -> Result<OwnedInferenceConnection, ExecutionError> {
    let running = state
        .cpa_runtime
        .installed_host()
        .map(|host| host.owned_running())
        .unwrap_or(false);
    let view = applied_view(state);
    if view.poisoned || view.unavailable || !running {
        return Err(ExecutionError::Unavailable(
            "owned CPA child is missing".into(),
        ));
    }
    if !applied_tuple_ready(&view, state.process_generation()) {
        return Err(ExecutionError::Unavailable(
            "owned CPA child is not the applied projection".into(),
        ));
    }
    let base_url =
        verified_owned_origin(&view.owned_origin, view.listen_port, &view.public_origin)?;
    let hop = view
        .hop
        .clone()
        .ok_or_else(|| ExecutionError::Unavailable("owned CPA hop is missing".into()))?;
    let again = applied_view(state);
    if !same_applied_view(&view, &again) {
        return Err(ExecutionError::Unavailable(
            "owned CPA child is not the applied projection".into(),
        ));
    }
    Ok(OwnedInferenceConnection {
        base_url,
        hop: OwnedInferenceHop(hop),
        child_generation: view.child_generation,
        applied_revision: view.applied_revision,
        applied_digest: view.applied_digest,
    })
}

/// Control-send pin. The owned host does not yet enforce the validated headers,
/// so this refuses instead of returning a pin the child would ignore.
pub(crate) struct ValidatedInferencePin {
    pub connection: OwnedInferenceConnection,
    pub auth_id: String,
    pub credential_id: String,
    pub credential_version: u64,
    pub binding_id: String,
    pub material_revision: String,
    pub public_model: String,
    pub protocol: String,
}

pub(crate) fn validated_inference_pin(
    state: &CoreState,
    credential_id: &str,
    credential_version: u64,
    public_model: &str,
    protocol: &str,
) -> Result<ValidatedInferencePin, ExecutionError> {
    let refused = || ExecutionError::Unavailable("validated protocol pin is not enforced".into());
    let view = applied_view(state);
    if !applied_tuple_ready(&view, state.process_generation())
        || !pin_capabilities_ready(&view.host_capabilities)
    {
        return Err(refused());
    }
    let connection = owned_inference_connection(state)?;
    let (stamp, route) = {
        let db = state.db.lock();
        let tx = db.conn.unchecked_transaction().map_err(|_| refused())?;
        let record = store::load_tx(&tx).map_err(|_| refused())?;
        if !pin_capabilities_ready(&record.host_capabilities) {
            return Err(refused());
        }
        if record.oauth.iter().any(|stamp| {
            stamp.credential_id == credential_id && stamp.presence != store::OAuthPresence::Present
        }) {
            return Err(refused());
        }
        identity::validated_route_pin_on(
            &tx,
            &record,
            credential_id,
            credential_version,
            public_model,
            protocol,
            state.cipher.as_ref(),
        )
        .map_err(|_| refused())?
    };
    let again = applied_view(state);
    if !same_applied_view(&view, &again) || !pin_capabilities_ready(&again.host_capabilities) {
        return Err(refused());
    }
    let auth_current = again.applied_auth.iter().any(|item| {
        item.auth_id == stamp.auth_id
            && item.credential_id == stamp.credential_id
            && item.credential_version == stamp.credential_version
            && item.material_revision == stamp.material_revision
    });
    let route_current = again.applied_routes.iter().any(|set| {
        set.credential_id == stamp.credential_id
            && set.credential_version == stamp.credential_version
            && set.auth_id == stamp.auth_id
            && set.routes.iter().any(|item| {
                item.public_model == route.public_model
                    && item.protocol == route.protocol
                    && item.endpoint_id == route.endpoint_id
                    && item.endpoint_fingerprint == route.endpoint_fingerprint
            })
    });
    if !auth_current || !route_current {
        return Err(refused());
    }
    Ok(ValidatedInferencePin {
        connection,
        auth_id: stamp.auth_id,
        credential_id: stamp.credential_id,
        credential_version: stamp.credential_version,
        binding_id: stamp.binding_id,
        material_revision: stamp.material_revision,
        public_model: route.public_model,
        protocol: route.protocol,
    })
}

fn pin_capabilities_ready(capabilities: &[String]) -> bool {
    [
        "validated-protocol-pin-v1",
        "validation-only-routes-v1",
        "absolute-request-deadline-v1",
    ]
    .iter()
    .all(|required| capabilities.iter().any(|item| item == required))
}

pub(crate) struct OwnedControlAccess {
    origin: String,
    management: Secret,
    hop: Secret,
}

impl OwnedControlAccess {
    pub(crate) fn origin(&self) -> &str {
        &self.origin
    }

    pub(crate) fn management(&self) -> &str {
        self.management.expose()
    }

    pub(crate) fn hop(&self) -> &str {
        self.hop.expose()
    }
}

impl std::fmt::Debug for OwnedControlAccess {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("OwnedControlAccess([redacted])")
    }
}

pub(crate) fn owned_control_access(
    state: &CoreState,
) -> Result<OwnedControlAccess, ExecutionError> {
    let connection = owned_inference_connection(state)?;
    let management = management_password(state)?;
    Ok(OwnedControlAccess {
        origin: connection.base_url,
        management: Secret(management),
        hop: Secret(connection.hop.expose().to_string()),
    })
}

pub(crate) async fn fetch_owned_ready_once(state: &CoreState) -> Result<String, ExecutionError> {
    let (port, token, origin) = {
        let inner = state.cpa_execution.inner.lock();
        let token = inner
            .secrets
            .as_ref()
            .map(|secrets| secrets.ready.expose().to_string());
        (inner.record.listen_port, token, inner.public_origin.clone())
    };
    let Some(token) = token.filter(|value| !value.is_empty()) else {
        return Err(ExecutionError::Unavailable(
            "owned CPA ready token is missing".into(),
        ));
    };
    if port == 0 || origin.is_empty() {
        return Err(ExecutionError::Unavailable(
            "owned CPA child is missing".into(),
        ));
    }
    ready::fetch_ready_once(port, &token, &origin).await
}

pub(crate) use native::{
    DiscoveredNativeRef, DiscoverySnapshot, FencedTarget, NativeLease, ReconcileReport,
    capture_native_lease, discovery_from_ready_body, fence_mapped_target, lease_current,
    note_external_failure, persisted_child_generation, reconcile_owned_discovery,
    specialized_native_binding_allowed, sync_owned_enabled,
};

struct AppliedView {
    poisoned: bool,
    unavailable: bool,
    verified_ready: bool,
    policy_ready: bool,
    apply_status: String,
    child_generation: u64,
    record_child: u64,
    applied_generation: u64,
    applied_revision: u64,
    applied_digest: String,
    owned_origin: String,
    listen_port: u16,
    public_origin: String,
    hop: Option<Secret>,
    applied_auth: Vec<store::AuthStamp>,
    applied_routes: Vec<crate::cpa_projection::CredentialRouteSet>,
    host_capabilities: Vec<String>,
}

fn applied_view(state: &CoreState) -> AppliedView {
    let inner = state.cpa_execution.inner.lock();
    AppliedView {
        poisoned: inner.poisoned,
        unavailable: inner.record.unavailable,
        verified_ready: inner.verified_ready,
        policy_ready: inner.record.policy_ready,
        apply_status: inner.record.apply_status.clone(),
        child_generation: inner.child_generation,
        record_child: inner.record.child_generation,
        applied_generation: inner.record.applied_generation,
        applied_revision: inner.record.applied_revision,
        applied_digest: inner.record.applied_digest.clone(),
        owned_origin: inner.record.owned_origin.clone(),
        listen_port: inner.record.listen_port,
        public_origin: inner.public_origin.clone(),
        hop: inner.secrets.as_ref().map(|secrets| secrets.hop.clone()),
        applied_auth: inner.record.applied_auth.clone(),
        applied_routes: inner.record.applied_routes.clone(),
        host_capabilities: inner.record.host_capabilities.clone(),
    }
}

fn applied_tuple_ready(view: &AppliedView, public_generation: u64) -> bool {
    view.verified_ready
        && view.policy_ready
        && view.apply_status == "applied"
        && view.child_generation != 0
        && view.child_generation != public_generation
        && view.child_generation == view.record_child
        && view.child_generation == view.applied_generation
        && view.applied_revision != 0
        && lowercase_hex_64(&view.applied_digest)
}

fn same_applied_view(left: &AppliedView, right: &AppliedView) -> bool {
    left.poisoned == right.poisoned
        && left.unavailable == right.unavailable
        && left.verified_ready == right.verified_ready
        && left.policy_ready == right.policy_ready
        && left.apply_status == right.apply_status
        && left.child_generation == right.child_generation
        && left.record_child == right.record_child
        && left.applied_generation == right.applied_generation
        && left.applied_revision == right.applied_revision
        && left.applied_digest == right.applied_digest
        && left.owned_origin == right.owned_origin
        && left.listen_port == right.listen_port
        && left.public_origin == right.public_origin
        && left.applied_auth == right.applied_auth
        && left.applied_routes == right.applied_routes
        && left.host_capabilities == right.host_capabilities
        && left.hop.as_ref().map(Secret::expose) == right.hop.as_ref().map(Secret::expose)
}

pub(super) fn verified_owned_origin(
    owned: &str,
    listen_port: u16,
    public_origin: &str,
) -> Result<String, ExecutionError> {
    let origin = crate::cpa::normalize_base_url(owned, false).map_err(|_| {
        ExecutionError::Unavailable("owned CPA child is not a loopback origin".into())
    })?;
    let port = reqwest::Url::parse(&origin)
        .ok()
        .and_then(|url| url.port())
        .ok_or_else(|| ExecutionError::Unavailable("owned CPA child port does not match".into()))?;
    if port != listen_port {
        return Err(ExecutionError::Unavailable(
            "owned CPA child port does not match".into(),
        ));
    }
    if origin == public_origin || project::origin_of(&origin).as_deref() == Some(public_origin) {
        return Err(ExecutionError::Unavailable(
            "owned CPA child collides with the public origin".into(),
        ));
    }
    Ok(origin)
}

pub(super) fn lowercase_hex_64(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

pub use callback::policy_callback;

async fn apply(
    state: &CoreState,
    cas: Option<(u64, u64)>,
    desired_running: bool,
) -> Result<ExecutionReport, ExecutionError> {
    let hooks = current_hooks();
    let password = management_password(state)?;
    let origin = listener_origin(state)?;
    // skip_spawn skips only the child process. The selected artifact is still
    // verified and installed. A manifest SHA, an empty stand-in, and the
    // historical placeholder are not accepted.
    let artifact = load_artifact(state)?;
    artifact::install(&state.data_dir, &artifact)?;
    ensure_secrets(state)?;
    ensure_policy(state, &origin)?;
    let was_running = state
        .cpa_runtime
        .installed_host()
        .ok()
        .is_some_and(|host| host.owned_running());
    let port = {
        let preferred = state.cpa_execution.inner.lock().record.listen_port;
        io::reserve_loopback_port(preferred)?
    };
    let (child_generation, secrets) = {
        let inner = state.cpa_execution.inner.lock();
        if inner.poisoned {
            return Err(ExecutionError::Unavailable(
                "cpa execution record is unreadable".into(),
            ));
        }
        let secrets = inner
            .secrets
            .clone()
            .ok_or_else(|| ExecutionError::Unavailable("CPA secrets are unavailable".into()))?;
        (inner.child_generation, secrets)
    };
    let child_origin = format!("http://127.0.0.1:{port}");
    let current_yaml = io::read_bounded(&io::config_path(&state.data_dir))?;
    let (record, yaml, revision_at_write, previous_is_applied) = {
        let _settings = state.settings_update.lock();
        if let Some((revision, generation)) = cas {
            check_cas(state, revision, generation)?;
        }
        let db = state.db.lock();
        let mut record = match store::load(&db.conn) {
            Ok(Some(record)) => record,
            Ok(None) => Record::empty(),
            Err(()) => {
                return Err(ExecutionError::Unavailable(
                    "cpa execution record is unreadable".into(),
                ));
            }
        };
        let current_text = match &current_yaml {
            Some(bytes) => Some(
                String::from_utf8(bytes.clone())
                    .map_err(|_| ExecutionError::Invalid("CPA config is not UTF-8".into()))?,
            ),
            None => None,
        };
        let captured = current_text
            .as_deref()
            .and_then(|text| store::snapshot_matching_applied(&record, text));
        let previous_is_applied = captured.is_some();
        let snapshot = RoutingSnapshot::load(&db).map_err(|_| {
            ExecutionError::Unavailable("routing snapshot could not be read".into())
        })?;
        let config = state.config();
        let desired_revision = record.desired_revision.saturating_add(1);
        let rendered = project::render(
            &db,
            &snapshot,
            &config,
            &record,
            child_generation,
            desired_revision,
            port,
            &origin,
            &child_origin,
            &secrets,
            state.cipher.as_ref(),
            &state.data_dir,
        )?;
        if current_text.is_some() {
            record.previous_accepted = captured;
        }
        record.child_generation = child_generation;
        record.desired_revision = desired_revision;
        record.desired_digest = rendered.digest;
        record.desired_auth = rendered.desired_auth;
        record.desired_routes = rendered.routes;
        record.oauth = rendered.oauth;
        record.artifact_sha256 = artifact.sha256.clone();
        record.listen_port = port;
        record.public_origin = origin.clone();
        record.owned_origin = child_origin;
        record.apply_status = "apply_pending".into();
        record.policy_ready = false;
        record.host_capabilities.clear();
        record.desired_running = desired_running;
        store::save(&db.conn, &record)?;
        let revision = state.settings_revision();
        drop(db);
        drop(_settings);
        (record, rendered.yaml, revision, previous_is_applied)
    };
    {
        let mut inner = state.cpa_execution.inner.lock();
        inner.record = record.clone();
        inner.public_origin = origin.clone();
        inner.phase = CpaRuntimePhase::Starting;
        inner.verified_ready = false;
    }
    if let Some(bytes) = &current_yaml {
        io::atomic_write_private(&io::previous_config_path(&state.data_dir), bytes)?;
    }
    io::atomic_write_private(&io::config_path(&state.data_dir), yaml.as_bytes())?;
    let launched = launch(state, &password, port, &hooks).await;
    let failed = if hooks.fail_ready {
        Err(ExecutionError::ApplyFailed(
            "cpa_apply_failed: ready identity rejected".into(),
        ))
    } else {
        launched
    };
    let body = match failed {
        Ok(body) => body,
        Err(error) => {
            fail_apply(
                state,
                &record,
                current_yaml.clone(),
                previous_is_applied,
                was_running,
                &password,
                port,
                &hooks,
            )
            .await;
            return Err(error);
        }
    };
    let secrets = state
        .cpa_execution
        .inner
        .lock()
        .secrets
        .clone()
        .ok_or_else(|| ExecutionError::Unavailable("CPA secrets are unavailable".into()))?;
    let accepted = match ready::accept_ready(&body, &record, &secrets) {
        Ok(accepted) => accepted,
        Err(error) => {
            fail_apply(
                state,
                &record,
                current_yaml.clone(),
                previous_is_applied,
                was_running,
                &password,
                port,
                &hooks,
            )
            .await;
            return Err(error);
        }
    };
    // In-progress poll window: verified_ready is still false. Not a new hook.
    #[cfg(test)]
    state.await_device_apply_pause().await;
    if let Some(ref hook) = hooks.before_commit {
        hook();
    }
    let committed = {
        let _settings = state.settings_update.lock();
        if state.settings_revision() != revision_at_write {
            Err(ExecutionError::ApplyConflict("cpa_apply_conflict".into()))
        } else {
            let db = state.db.lock();
            let mut committed = record.clone();
            committed.host_capabilities = accepted.capabilities.clone();
            merge_oauth(&mut committed, accepted);
            if let Err(error) =
                ready::note_keyed_registration_epochs(&mut committed.desired_auth, &body)
            {
                Err(error)
            } else {
                committed.applied_generation = committed.child_generation;
                committed.applied_revision = committed.desired_revision;
                committed.applied_digest = committed.desired_digest.clone();
                committed.applied_auth = committed.desired_auth.clone();
                committed.applied_routes = committed.desired_routes.clone();
                committed.apply_status = "applied".into();
                committed.policy_ready = true;
                committed.unavailable = false;
                committed.desired_running = true;
                store::save(&db.conn, &committed)?;
                Ok(committed)
            }
        }
    };
    let committed = match committed {
        Ok(record) => record,
        Err(error) => {
            fail_apply(
                state,
                &record,
                current_yaml.clone(),
                previous_is_applied,
                was_running,
                &password,
                port,
                &hooks,
            )
            .await;
            return Err(error);
        }
    };
    {
        let mut inner = state.cpa_execution.inner.lock();
        inner.record = committed;
        inner.verified_ready = true;
        inner.phase = CpaRuntimePhase::Idle;
        inner.error = None;
    }
    let _ = write_managed(state, true);
    Ok(execution_report(state))
}

async fn fail_apply(
    state: &CoreState,
    record: &Record,
    previous: Option<Vec<u8>>,
    previous_is_applied: bool,
    was_running: bool,
    password: &str,
    _port: u16,
    hooks: &Hooks,
) {
    let file_ok = match previous.as_ref() {
        Some(bytes) => io::atomic_write_private(&io::config_path(&state.data_dir), bytes).is_ok(),
        None => false,
    };
    let _ = stop_host(state);
    let mut failed = record.clone();
    failed.desired_running = was_running;
    if previous_is_applied {
        restore_accepted_envelope(&mut failed);
    }
    let restore_port = if failed.listen_port != 0 {
        failed.listen_port
    } else {
        previous
            .as_deref()
            .and_then(|bytes| std::str::from_utf8(bytes).ok())
            .and_then(yaml_listen_port)
            .unwrap_or(0)
    };
    let restored = if previous_is_applied && file_ok && (was_running || hooks.skip_spawn) {
        let mut probe = failed.clone();
        probe.child_generation = failed.applied_generation;
        probe.desired_revision = failed.applied_revision;
        probe.desired_digest = failed.applied_digest.clone();
        {
            let mut inner = state.cpa_execution.inner.lock();
            inner.record = probe;
            inner.verified_ready = false;
        }
        launch(
            state,
            password,
            restore_port,
            &Hooks {
                fail_ready: false,
                skip_spawn: hooks.skip_spawn,
                before_commit: None,
            },
        )
        .await
        .ok()
    } else {
        None
    };
    let secrets = state.cpa_execution.inner.lock().secrets.clone();
    let restored_ready = match (restored.as_deref(), secrets.as_ref()) {
        (Some(body), Some(secrets)) => ready::accept_restored(body, &failed, secrets).ok(),
        _ => None,
    };
    let mut coherent = false;
    if let (Some(accepted), Some(body)) = (restored_ready, restored.as_deref()) {
        match ready::note_applied_keyed_epochs(&mut failed, body) {
            Ok(()) => {
                failed.apply_status = "applied".into();
                failed.policy_ready = true;
                failed.unavailable = false;
                failed.host_capabilities = accepted.capabilities;
                coherent = true;
            }
            Err(_) => {
                failed.apply_status = "apply_failed".into();
                failed.policy_ready = false;
                failed.unavailable = true;
                failed.host_capabilities.clear();
            }
        }
    } else {
        failed.apply_status = "apply_failed".into();
        failed.policy_ready = false;
        failed.host_capabilities.clear();
    }
    if coherent {
        if persist(state, &failed).is_err() {
            failed.apply_status = "apply_failed".into();
            failed.policy_ready = false;
            failed.unavailable = true;
            coherent = false;
            let _ = persist(state, &failed);
        }
    } else {
        let _ = persist(state, &failed);
    }
    let mut inner = state.cpa_execution.inner.lock();
    inner.record = failed;
    if coherent {
        inner.verified_ready = true;
        inner.phase = CpaRuntimePhase::Idle;
        inner.error = None;
    } else {
        inner.verified_ready = false;
        inner.phase = CpaRuntimePhase::Failed;
        inner.error = Some("cpa_apply_failed".into());
    }
}

fn restore_accepted_envelope(failed: &mut Record) {
    let Some(snapshot) = failed.previous_accepted.clone() else {
        return;
    };
    failed.listen_port = snapshot.listen_port;
    failed.owned_origin = snapshot.owned_origin;
    failed.public_origin = snapshot.public_origin;
    failed.applied_auth = snapshot.auth;
    failed.applied_routes = snapshot.routes;
    failed.applied_generation = snapshot.generation;
    failed.applied_revision = snapshot.revision;
    failed.applied_digest = snapshot.wire_digest;
}

fn merge_oauth(record: &mut Record, accepted: AcceptedReady) {
    for incoming in accepted.oauth {
        if incoming.credential_id.is_empty() {
            let revived = record.oauth.iter().any(|stamp| {
                stamp.native_provider == incoming.native_provider
                    && stamp.relative_path == incoming.relative_path
                    && matches!(
                        stamp.presence,
                        store::OAuthPresence::Absent | store::OAuthPresence::Present
                    )
            });
            if !revived {
                record.oauth.push(incoming);
            }
            continue;
        }
        if let Some(existing) = record
            .oauth
            .iter_mut()
            .find(|stamp| stamp.credential_id == incoming.credential_id)
        {
            if matches!(
                existing.presence,
                store::OAuthPresence::Absent | store::OAuthPresence::Pending
            ) {
                continue;
            }
            if existing.registration_epoch != incoming.registration_epoch {
                existing.presence = store::OAuthPresence::Pending;
                continue;
            }
            existing.material_revision = incoming.material_revision.clone();
            existing.registration_epoch = incoming.registration_epoch;
            existing.relative_path = incoming.relative_path.clone();
            existing.auth_id = incoming.auth_id.clone();
            existing.models = incoming.models.clone();
            existing.provider_id = incoming.provider_id.clone();
            existing.native_provider = incoming.native_provider.clone();
        } else if incoming.presence == store::OAuthPresence::Present {
            record.oauth.push(incoming.clone());
        }
        let fenced = record.oauth.iter().any(|stamp| {
            stamp.credential_id == incoming.credential_id
                && matches!(
                    stamp.presence,
                    store::OAuthPresence::Absent | store::OAuthPresence::Pending
                )
        });
        if fenced {
            continue;
        }
        for stamp in record
            .desired_auth
            .iter_mut()
            .chain(record.applied_auth.iter_mut())
        {
            if stamp.credential_id == incoming.credential_id
                && stamp.credential_version == incoming.credential_version
            {
                stamp.material_revision = incoming.material_revision.clone();
                stamp.registration_epoch = incoming.registration_epoch;
            }
        }
    }
}

async fn launch(
    state: &CoreState,
    password: &str,
    port: u16,
    hooks: &Hooks,
) -> Result<String, ExecutionError> {
    if hooks.skip_spawn {
        let record = state.cpa_execution.inner.lock().record.clone();
        return Ok(ready::synthetic_ready(&record));
    }
    let host = state
        .cpa_runtime
        .installed_host()
        .map_err(|_| ExecutionError::Unavailable("owned CPA host is not registered".into()))?;
    let executable = {
        let sha = state
            .cpa_execution
            .inner
            .lock()
            .record
            .artifact_sha256
            .clone();
        artifact::installed_executable(&state.data_dir, &sha).ok_or_else(|| {
            ExecutionError::Invalid("pinned CPA executable is not installed".into())
        })?
    };
    let secrets = state
        .cpa_execution
        .inner
        .lock()
        .secrets
        .clone()
        .ok_or_else(|| ExecutionError::Unavailable("CPA secrets are unavailable".into()))?;
    let spec = CpaRuntimeProcessSpec {
        codex_device_login: false,
        executable: executable.clone(),
        config_path: io::config_path(&state.data_dir),
        working_dir: executable.parent().unwrap_or(Path::new(".")).to_path_buf(),
        management_password: CpaRuntimeSecret::new(password.to_string()),
        log_secrets: vec![
            CpaRuntimeSecret::new(secrets.hop.expose().to_string()),
            CpaRuntimeSecret::new(secrets.policy.expose().to_string()),
            CpaRuntimeSecret::new(secrets.ready.expose().to_string()),
        ],
    };
    host.start_owned(&spec).map_err(|_| {
        ExecutionError::ApplyFailed("cpa_apply_failed: owned host did not start".into())
    })?;
    let origin = state.cpa_execution.inner.lock().public_origin.clone();
    ready::poll_ready(port, secrets.ready.expose(), &origin).await
}

fn ensure_secrets(state: &CoreState) -> Result<(), ExecutionError> {
    let directory = io::private_dir(&state.data_dir);
    io::ensure_dir(&directory)?;
    let hop_path = directory.join("hop.key");
    let policy_path = directory.join("policy.token");
    let ready_path = directory.join("ready.key");
    let gateway = state.config().gateway_key;
    if let (Some(hop), Some(policy), Some(ready)) = (
        read_secret(&hop_path)?,
        read_secret(&policy_path)?,
        read_secret(&ready_path)?,
    ) {
        if distinct_secrets(&hop, &policy, &ready, &gateway) {
            state.cpa_execution.inner.lock().secrets = Some(Secrets {
                hop: Secret(hop),
                policy: Secret(policy),
                ready: Secret(ready),
            });
            return Ok(());
        }
    }
    let hop = fresh_secret(&[&gateway]);
    let policy = fresh_secret(&[&gateway, &hop]);
    let ready = fresh_secret(&[&gateway, &hop, &policy]);
    io::atomic_write_private(&hop_path, hop.as_bytes())?;
    io::atomic_write_private(&policy_path, policy.as_bytes())?;
    io::atomic_write_private(&ready_path, ready.as_bytes())?;
    state.cpa_execution.inner.lock().secrets = Some(Secrets {
        hop: Secret(hop),
        policy: Secret(policy),
        ready: Secret(ready),
    });
    Ok(())
}

fn ensure_policy(state: &CoreState, origin: &str) -> Result<(), ExecutionError> {
    let token = state
        .cpa_execution
        .inner
        .lock()
        .secrets
        .as_ref()
        .map(|secrets| secrets.policy.expose().to_string())
        .ok_or_else(|| ExecutionError::Unavailable("CPA policy token is unavailable".into()))?;
    let stamp = hex::encode(Sha256::digest(token.as_bytes()));
    let current = state.cpa_execution.inner.lock().token_stamp.clone();
    if current == stamp && state.cpa_execution.inner.lock().policy.is_some() {
        state.cpa_execution.inner.lock().public_origin = origin.to_string();
        return Ok(());
    }
    let service = PolicyService::new(
        &token,
        OpportunityPolicy::default_bounded(),
        SqlitePolicyStore::new(state),
    )
    .map_err(|_| ExecutionError::Invalid("CPA policy service could not be created".into()))?;
    let mut inner = state.cpa_execution.inner.lock();
    inner.policy = Some(Arc::new(service));
    inner.token_stamp = stamp;
    inner.public_origin = origin.to_string();
    Ok(())
}

fn read_secret(path: &Path) -> Result<Option<String>, ExecutionError> {
    let Some(bytes) = io::read_bounded(path)? else {
        return Ok(None);
    };
    let text = String::from_utf8(bytes)
        .map_err(|_| ExecutionError::Invalid("CPA secret file is invalid".into()))?;
    let text = text.trim().to_string();
    if text.is_empty() || text.len() > 256 {
        return Err(ExecutionError::Invalid("CPA secret file is invalid".into()));
    }
    Ok(Some(text))
}

fn distinct_secrets(hop: &str, policy: &str, ready: &str, gateway: &str) -> bool {
    hop != policy
        && hop != ready
        && policy != ready
        && hop != "ocg-keyless"
        && policy != "ocg-keyless"
        && ready != "ocg-keyless"
        && hop != gateway
        && policy != gateway
        && ready != gateway
        && !hop.is_empty()
}

fn fresh_secret(reject: &[&str]) -> String {
    loop {
        let value = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
        if reject.iter().all(|item| *item != value) && value != "ocg-keyless" {
            return value;
        }
    }
}

fn delete_secrets(data_dir: &Path) {
    let directory = io::private_dir(data_dir);
    let _ = std::fs::remove_file(directory.join("hop.key"));
    let _ = std::fs::remove_file(directory.join("policy.token"));
    let _ = std::fs::remove_file(directory.join("ready.key"));
}

fn write_managed(state: &CoreState, desired_running: bool) -> Result<(), ExecutionError> {
    if !io::config_path(&state.data_dir).is_file() {
        return Ok(());
    }
    let previous = crate::cpa_runtime::load_managed(&state.data_dir)
        .ok()
        .flatten();
    let port = state.cpa_execution.inner.lock().record.listen_port;
    let managed = ManagedCpa {
        current_version: PINNED_VERSION_CANONICAL.to_string(),
        previous_version: previous.as_ref().and_then(|item| {
            if item.current_version == PINNED_VERSION_CANONICAL {
                item.previous_version.clone()
            } else {
                Some(item.current_version.clone())
            }
        }),
        asset_sha256: artifact::selected_trusted_sha()?,
        port,
        desired_running,
    };
    crate::cpa_runtime::save_managed(&state.data_dir, &managed).map_err(|_| {
        ExecutionError::ApplyFailed(
            "cpa_apply_failed: managed runtime record could not be saved".into(),
        )
    })
}

fn persist(state: &CoreState, record: &Record) -> Result<(), ExecutionError> {
    #[cfg(test)]
    if FAIL_PERSIST.with(|cell| cell.get()) {
        return Err(ExecutionError::Unavailable(
            "CPA execution record could not be saved".into(),
        ));
    }
    let db = state.db.lock();
    store::save(&db.conn, record)
}

fn load_artifact(state: &CoreState) -> Result<artifact::Artifact, ExecutionError> {
    let explicit = state.cpa_execution.inner.lock().artifact_dir.clone();
    let dir = artifact::resolve_dir(explicit.as_deref())?;
    artifact::verify(&dir)
}

fn check_cas(
    state: &CoreState,
    expected_revision: u64,
    expected_generation: u64,
) -> Result<(), ExecutionError> {
    if state.settings_revision() != expected_revision
        || state.process_generation() != expected_generation
    {
        Err(ExecutionError::ApplyConflict("revisionConflict".into()))
    } else {
        Ok(())
    }
}

fn listener_origin(state: &CoreState) -> Result<String, ExecutionError> {
    let bound = state
        .gateway
        .lock()
        .as_ref()
        .map(|handle| handle.listen_addr)
        .ok_or_else(|| ExecutionError::Invalid("public listener is not bound".into()))?;
    Ok(origin_from_addr(bound))
}

fn origin_from_addr(addr: SocketAddr) -> String {
    let ip = if addr.ip().is_unspecified() {
        if addr.is_ipv6() {
            IpAddr::V6(Ipv6Addr::LOCALHOST)
        } else {
            IpAddr::V4(Ipv4Addr::LOCALHOST)
        }
    } else {
        addr.ip()
    };
    match ip {
        IpAddr::V6(ip) => format!("http://[{ip}]:{}", addr.port()),
        IpAddr::V4(ip) => format!("http://{ip}:{}", addr.port()),
    }
}

fn yaml_listen_port(yaml: &str) -> Option<u16> {
    let value: serde_yaml_ng::Value = serde_yaml_ng::from_str(yaml).ok()?;
    let port = match value.get("port")? {
        serde_yaml_ng::Value::Number(number) => number.as_u64()?,
        serde_yaml_ng::Value::String(text) => text.parse().ok()?,
        _ => return None,
    };
    u16::try_from(port).ok().filter(|value| *value != 0)
}

fn yaml_identity(yaml: &str) -> Result<(u64, u64, String), ExecutionError> {
    let value: serde_yaml_ng::Value = serde_yaml_ng::from_str(yaml)
        .map_err(|_| ExecutionError::Invalid("CPA config could not be read".into()))?;
    let ocg = value
        .get("ocg")
        .ok_or_else(|| ExecutionError::Invalid("CPA config has no projection".into()))?;
    let generation = yaml_decimal(ocg.get("process-generation"))?;
    let revision = yaml_decimal(ocg.get("projection-revision"))?;
    if generation == 0 || revision == 0 {
        return Err(ExecutionError::Invalid(
            "CPA config projection identity is empty".into(),
        ));
    }
    Ok((
        generation,
        revision,
        crate::cpa_projection::wire_digest(yaml),
    ))
}

fn yaml_decimal(value: Option<&serde_yaml_ng::Value>) -> Result<u64, ExecutionError> {
    let invalid = ExecutionError::Invalid("CPA config projection identity is invalid".into());
    match value {
        Some(serde_yaml_ng::Value::String(text)) => text.parse().map_err(|_| invalid),
        Some(serde_yaml_ng::Value::Number(number)) => number.as_u64().ok_or(invalid),
        _ => Err(invalid),
    }
}

fn stop_host(state: &CoreState) -> Result<(), ExecutionError> {
    if let Ok(host) = state.cpa_runtime.installed_host() {
        host.stop_owned().map_err(|_| {
            ExecutionError::Unavailable("owned CPA host could not be stopped".into())
        })?;
    }
    Ok(())
}

fn mint_child(public_generation: u64) -> u64 {
    loop {
        let value = (Uuid::new_v4().as_u128() as u64) & 0x0000_FFFF_FFFF_FFFF;
        if value != 0 && value != public_generation {
            return value;
        }
    }
}

struct Hooks {
    fail_ready: bool,
    skip_spawn: bool,
    before_commit: Option<Arc<dyn Fn() + Send + Sync>>,
}

fn current_hooks() -> Hooks {
    #[cfg(test)]
    {
        Hooks {
            fail_ready: FAIL_READY.with(|cell| cell.get()),
            skip_spawn: SKIP_SPAWN.with(|cell| cell.get()),
            before_commit: BEFORE_COMMIT.with(|cell| cell.borrow().clone()),
        }
    }
    #[cfg(not(test))]
    {
        Hooks {
            fail_ready: false,
            skip_spawn: false,
            before_commit: None,
        }
    }
}

fn management_password(state: &CoreStateInner) -> Result<String, ExecutionError> {
    let directory = io::private_dir(&state.data_dir);
    io::ensure_dir(&directory)?;
    let path = directory.join("management.key");
    let hop = read_secret(&directory.join("hop.key"))?.unwrap_or_default();
    let policy = read_secret(&directory.join("policy.token"))?.unwrap_or_default();
    let ready = read_secret(&directory.join("ready.key"))?.unwrap_or_default();
    let gateway = state.config().gateway_key;
    let file_secret = match read_secret(&path)? {
        Some(value) if private_management_secret(&value, &hop, &policy, &ready, &gateway) => value,
        _ => {
            let fresh = fresh_secret(&[&gateway, hop.as_str(), policy.as_str(), ready.as_str()]);
            io::atomic_write_private(&path, fresh.as_bytes())?;
            fresh
        }
    };
    if let Some(override_password) = operator_management_password()? {
        return Ok(override_password);
    }
    Ok(file_secret)
}

fn private_management_secret(
    value: &str,
    hop: &str,
    policy: &str,
    ready: &str,
    gateway: &str,
) -> bool {
    !value.is_empty()
        && value != "ocg-keyless"
        && value != hop
        && value != policy
        && value != ready
        && (gateway.is_empty() || value != gateway)
}

fn operator_management_password() -> Result<Option<String>, ExecutionError> {
    #[cfg(test)]
    {
        if let Some(value) = TEST_PASSWORD.with(|cell| cell.borrow().clone()) {
            if value.is_empty() {
                return Err(ExecutionError::Unavailable(
                    "MANAGEMENT_PASSWORD is not set".into(),
                ));
            }
            return Ok(Some(value));
        }
    }
    match std::env::var("MANAGEMENT_PASSWORD") {
        Ok(value) if !value.trim().is_empty() => Ok(Some(value)),
        _ => Ok(None),
    }
}

#[cfg(test)]
use std::cell::{Cell, RefCell};

#[cfg(test)]
thread_local! {
    static FAIL_READY: Cell<bool> = const { Cell::new(false) };
    static FAIL_PERSIST: Cell<bool> = const { Cell::new(false) };
    static SKIP_SPAWN: Cell<bool> = const { Cell::new(false) };
    static TEST_PASSWORD: RefCell<Option<String>> = const { RefCell::new(None) };
    static BEFORE_COMMIT: RefCell<Option<Arc<dyn Fn() + Send + Sync>>> = const { RefCell::new(None) };
}

#[cfg(test)]
pub(super) fn test_secrets(hop: &str, policy: &str, ready: &str) -> Secrets {
    Secrets {
        hop: Secret(hop.to_string()),
        policy: Secret(policy.to_string()),
        ready: Secret(ready.to_string()),
    }
}

#[cfg(test)]
pub(crate) fn set_fail_ready(value: bool) {
    FAIL_READY.with(|cell| cell.set(value));
}

#[cfg(test)]
pub(crate) fn set_fail_persist(value: bool) {
    FAIL_PERSIST.with(|cell| cell.set(value));
}

/// Locates the documented runtime-build directory. Not a trust decision.
/// The loopback-fixture feature selects that variant's subdirectory.
/// Feature off stays on the production base directory.
#[cfg(test)]
pub(crate) fn documented_runtime_dir() -> PathBuf {
    // Pop the crate and workspace directories. A joined `..` is a ParentDir
    // component, and the artifact check rejects that as an unsafe pin path.
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.pop();
    path.pop();
    path.push("tmp");
    path.push("ocg3-cli-delivery");
    path.push("orchestration-20261004");
    path.push("runtime-build");
    if cfg!(feature = "ollama-cloud-loopback-test") {
        path.push("native-loopback-fixture");
    }
    path
}

#[cfg(test)]
pub(crate) fn set_skip_spawn(value: bool) {
    SKIP_SPAWN.with(|cell| cell.set(value));
}

#[cfg(test)]
pub(crate) fn set_test_password(value: Option<String>) {
    TEST_PASSWORD.with(|cell| *cell.borrow_mut() = value);
}

#[cfg(test)]
pub(crate) fn set_before_apply_commit(hook: Option<Arc<dyn Fn() + Send + Sync>>) {
    BEFORE_COMMIT.with(|cell| *cell.borrow_mut() = hook);
}

#[cfg(test)]
struct DeviceApplyHandoffBits {
    entered: Arc<std::sync::atomic::AtomicBool>,
    release: Arc<std::sync::atomic::AtomicBool>,
}

#[cfg(test)]
thread_local! {
    static DEVICE_APPLY_HANDOFF_PAUSE: RefCell<Option<DeviceApplyHandoffBits>> =
        const { RefCell::new(None) };
}

/// Test seam before `schedule_owned_device_apply` takes `cpa_operations`.
#[cfg(test)]
pub(crate) struct DeviceApplyHandoffPause {
    entered: Arc<std::sync::atomic::AtomicBool>,
    release: Arc<std::sync::atomic::AtomicBool>,
}

#[cfg(test)]
impl DeviceApplyHandoffPause {
    pub(crate) fn entered(&self) -> bool {
        self.entered.load(std::sync::atomic::Ordering::SeqCst)
    }

    pub(crate) fn release(&self) {
        self.release
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

#[cfg(test)]
impl Drop for DeviceApplyHandoffPause {
    fn drop(&mut self) {
        self.release
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let release = Arc::clone(&self.release);
        DEVICE_APPLY_HANDOFF_PAUSE.with(|cell| {
            let mut guard = cell.borrow_mut();
            if guard
                .as_ref()
                .is_some_and(|bits| Arc::ptr_eq(&bits.release, &release))
            {
                *guard = None;
            }
        });
    }
}

#[cfg(test)]
pub(crate) fn arm_device_apply_handoff_pause() -> DeviceApplyHandoffPause {
    let entered = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let release = Arc::new(std::sync::atomic::AtomicBool::new(false));
    DEVICE_APPLY_HANDOFF_PAUSE.with(|cell| {
        *cell.borrow_mut() = Some(DeviceApplyHandoffBits {
            entered: Arc::clone(&entered),
            release: Arc::clone(&release),
        });
    });
    DeviceApplyHandoffPause { entered, release }
}

#[cfg(test)]
async fn await_device_apply_handoff_pause() {
    let bits = DEVICE_APPLY_HANDOFF_PAUSE.with(|cell| cell.borrow_mut().take());
    if let Some(bits) = bits {
        bits.entered
            .store(true, std::sync::atomic::Ordering::SeqCst);
        while !bits.release.load(std::sync::atomic::Ordering::SeqCst) {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    }
}

#[cfg(test)]
fn preview_attempt(
    state: &CoreState,
    legacy_account_id: &str,
) -> Result<store::AuthStamp, ExecutionError> {
    let db = state.db.lock();
    let tx = db
        .conn
        .unchecked_transaction()
        .map_err(|_| ExecutionError::Unavailable("credential could not be read".into()))?;
    identity::preview_stamp(&tx, legacy_account_id, state.cipher.as_ref())
}

#[allow(dead_code)]
fn _origin_used(value: &str) -> Option<String> {
    origin_of(value)
}

#[allow(dead_code)]
fn _bound(bytes: usize) -> bool {
    bytes <= MAX_CONFIG_BYTES
}

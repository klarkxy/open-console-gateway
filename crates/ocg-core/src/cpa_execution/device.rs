//! Owned launch facts for the pinned CPA device-login CLI.
//!
//! The one-shot helper stays in `cpa_runtime::device`. This module decides
//! whether that helper may start, and whether a later authenticator completion
//! still names the same applied child. A historical integration row and
//! `OCG_CPA_BASE_URL` are not selectors. It does not read token files, call the
//! ready endpoint, or import a native binding.

use super::ExecutionError;
use crate::state::CoreStateInner;
use std::path::{Path, PathBuf};
use std::sync::Arc;
#[cfg(test)]
use std::sync::atomic::AtomicBool;
use zeroize::Zeroize;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DeviceCompletion {
    pub child_generation: u64,
    pub applied_revision: u64,
    pub applied_digest: String,
    /// Settings revision captured at launch. The prompt loop's clone keeps it.
    pub settings_revision: u64,
    /// Nonzero only after this session notes its own settings commit.
    pub committed_settings_revision: u64,
    /// Set after this session's fenced reconcile and apply have committed.
    pub admitted: bool,
    pub lease: super::NativeLease,
}

pub(crate) struct OwnedDeviceLaunch {
    pub executable: PathBuf,
    pub config_path: PathBuf,
    pub auth_dir: PathBuf,
    pub working_dir: PathBuf,
    pub management_password: String,
    pub log_secrets: Vec<String>,
    pub completion: DeviceCompletion,
}

impl std::fmt::Debug for OwnedDeviceLaunch {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OwnedDeviceLaunch")
            .field("executable", &self.executable)
            .field("config_path", &self.config_path)
            .field("auth_dir", &self.auth_dir)
            .field("working_dir", &self.working_dir)
            .field("management_password", &"[redacted]")
            .field("log_secrets", &"[redacted]")
            .field("completion", &self.completion)
            .finish()
    }
}

impl Drop for OwnedDeviceLaunch {
    fn drop(&mut self) {
        self.management_password.zeroize();
        for secret in &mut self.log_secrets {
            secret.zeroize();
        }
    }
}

struct PlaneIdentity {
    view: super::AppliedView,
    artifact_sha256: String,
}

pub(crate) fn owned_device_launch(
    state: &CoreStateInner,
) -> Result<OwnedDeviceLaunch, ExecutionError> {
    let first = plane_snapshot(state);
    let host = state.cpa_runtime.installed_host().map_err(|_| {
        ExecutionError::Unavailable(crate::cpa_runtime::UNAVAILABLE_REASON.to_string())
    })?;
    if !host.owned_running() {
        return Err(ExecutionError::Invalid(
            "Start the OCG-managed CPA runtime before device login.".into(),
        ));
    }
    if !projection_ready(state, &first) {
        return Err(ExecutionError::Unavailable(
            "owned CPA child is not the applied projection".into(),
        ));
    }
    let trusted = super::artifact::selected_trusted_sha()?;
    if first.artifact_sha256 != trusted {
        return Err(ExecutionError::Invalid(
            "pinned CPA executable is not installed".into(),
        ));
    }
    let executable = super::artifact::installed_executable(&state.data_dir, &trusted)
        .ok_or_else(|| ExecutionError::Invalid("pinned CPA executable is not installed".into()))?;
    let config_path = super::io::config_path(&state.data_dir);
    let auth_dir = owned_auth_dir(state, &config_path)?;
    let working_dir = executable
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| ExecutionError::Invalid("pinned CPA executable is not installed".into()))?;
    let management_password = super::management_password(state)?;
    let second = plane_snapshot(state);
    if !same_plane(&first, &second) || !projection_ready(state, &second) {
        return Err(ExecutionError::ApplyConflict(
            "native reconciliation moved".into(),
        ));
    }
    let settings_revision = state.settings_revision();
    let lease = {
        let db = state.db.lock();
        super::capture_native_lease(&db.conn, second.view.child_generation, settings_revision)?
    };
    if state.settings_revision() != settings_revision {
        return Err(ExecutionError::ApplyConflict("revisionConflict".into()));
    }
    let third = plane_snapshot(state);
    if !same_plane(&second, &third) || !projection_ready(state, &third) {
        return Err(ExecutionError::ApplyConflict(
            "native reconciliation moved".into(),
        ));
    }
    let current = {
        let db = state.db.lock();
        super::lease_current(&db.conn, &lease)?
    };
    if !current {
        return Err(ExecutionError::ApplyConflict(
            "native reconciliation moved".into(),
        ));
    }
    let (hop, policy, ready) = log_material(state)?;
    let mut log_secrets = Vec::new();
    push_log_secret(&mut log_secrets, management_password.clone());
    push_log_secret(&mut log_secrets, hop);
    push_log_secret(&mut log_secrets, policy);
    push_log_secret(&mut log_secrets, ready);
    Ok(OwnedDeviceLaunch {
        executable,
        config_path,
        auth_dir,
        working_dir,
        management_password,
        log_secrets,
        completion: DeviceCompletion {
            child_generation: third.view.child_generation,
            applied_revision: third.view.applied_revision,
            applied_digest: third.view.applied_digest.clone(),
            settings_revision,
            committed_settings_revision: 0,
            admitted: false,
            lease,
        },
    })
}

pub(crate) fn completion_current(
    state: &CoreStateInner,
    completion: &DeviceCompletion,
) -> Result<(), ExecutionError> {
    let expected = if completion.committed_settings_revision != 0 {
        completion.committed_settings_revision
    } else {
        completion.settings_revision
    };
    if state.settings_revision() != expected {
        return Err(ExecutionError::ApplyConflict("revisionConflict".into()));
    }
    let identity = plane_snapshot(state);
    if !projection_ready(state, &identity)
        || identity.view.child_generation != completion.child_generation
        || identity.view.applied_revision != completion.applied_revision
        || identity.view.applied_digest != completion.applied_digest
    {
        return Err(ExecutionError::ApplyConflict(
            "native reconciliation moved".into(),
        ));
    }
    let db = state.db.lock();
    if !super::lease_current(&db.conn, &completion.lease)? {
        return Err(ExecutionError::ApplyConflict(
            "native reconciliation moved".into(),
        ));
    }
    Ok(())
}

fn plane_snapshot(state: &CoreStateInner) -> PlaneIdentity {
    let inner = state.cpa_execution.inner.lock();
    PlaneIdentity {
        artifact_sha256: inner.record.artifact_sha256.clone(),
        view: super::AppliedView {
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
        },
    }
}

fn projection_ready(state: &CoreStateInner, identity: &PlaneIdentity) -> bool {
    super::applied_tuple_ready(&identity.view, state.process_generation())
}

fn same_plane(left: &PlaneIdentity, right: &PlaneIdentity) -> bool {
    left.artifact_sha256 == right.artifact_sha256
        && left.view.poisoned == right.view.poisoned
        && left.view.unavailable == right.view.unavailable
        && left.view.verified_ready == right.view.verified_ready
        && left.view.policy_ready == right.view.policy_ready
        && left.view.apply_status == right.view.apply_status
        && left.view.child_generation == right.view.child_generation
        && left.view.record_child == right.view.record_child
        && left.view.applied_generation == right.view.applied_generation
        && left.view.applied_revision == right.view.applied_revision
        && left.view.applied_digest == right.view.applied_digest
        && left.view.owned_origin == right.view.owned_origin
}

fn owned_auth_dir(state: &CoreStateInner, config_path: &Path) -> Result<PathBuf, ExecutionError> {
    let bytes = super::io::read_bounded(config_path)?
        .ok_or_else(|| ExecutionError::Invalid("CPA config could not be read".into()))?;
    let value: serde_yaml_ng::Value = serde_yaml_ng::from_slice(&bytes)
        .map_err(|_| ExecutionError::Invalid("CPA config could not be read".into()))?;
    let configured = value
        .get("auth-dir")
        .and_then(serde_yaml_ng::Value::as_str)
        .unwrap_or("");
    let expected = super::io::portable_auth_dir(&state.data_dir)?;
    let auth_dir = super::io::auth_dir(&state.data_dir);
    if configured != expected || !auth_dir.is_dir() {
        return Err(ExecutionError::Invalid(
            "CPA config auth directory is not the owned auth directory".into(),
        ));
    }
    Ok(auth_dir)
}

fn log_material(state: &CoreStateInner) -> Result<(String, String, String), ExecutionError> {
    let inner = state.cpa_execution.inner.lock();
    let secrets = inner
        .secrets
        .as_ref()
        .ok_or_else(|| ExecutionError::Unavailable("CPA secrets are unavailable".into()))?;
    Ok((
        secrets.hop.expose().to_string(),
        secrets.policy.expose().to_string(),
        secrets.ready.expose().to_string(),
    ))
}

fn push_log_secret(secrets: &mut Vec<String>, mut value: String) {
    if value.is_empty() || secrets.iter().any(|item| item == &value) {
        value.zeroize();
        return;
    }
    secrets.push(value);
}

#[cfg(test)]
pub(crate) fn arm_synthetic_owned_plane(state: &crate::state::CoreState) {
    let child = {
        let generation = state.process_generation();
        let candidate = generation.wrapping_add(1);
        if candidate == 0 { 1 } else { candidate }
    };
    let projection = "ab".repeat(32);
    let source = super::documented_runtime_dir();
    let verified = super::artifact::verify(&source)
        .expect("documented runtime must verify before a device fixture");
    // Apply resolves the host directory again. The test executable is not beside it.
    super::set_artifact_dir(state, source);
    let trusted = super::artifact::selected_trusted_sha().expect("selected digest");
    assert_eq!(verified.sha256, trusted);
    assert_ne!(trusted, super::artifact::PINNED_SHA256);
    assert_ne!(projection, trusted);
    super::artifact::install(&state.data_dir, &verified)
        .expect("install selected bytes and marker");
    assert!(
        super::artifact::installed_executable(&state.data_dir, &trusted).is_some(),
        "installed lookup did not accept the selected bytes"
    );
    std::fs::create_dir_all(super::io::auth_dir(&state.data_dir))
        .expect("synthetic auth directory");
    let portable = super::io::portable_auth_dir(&state.data_dir).expect("portable auth dir");
    let yaml = format!("auth-dir: {}\n", yaml_quote(&portable));
    super::io::atomic_write_private(&super::io::config_path(&state.data_dir), yaml.as_bytes())
        .expect("synthetic owned config");
    let mut record = super::store::Record::empty();
    record.child_generation = child;
    record.applied_generation = child;
    record.desired_revision = 1;
    record.applied_revision = 1;
    record.desired_digest = projection.clone();
    record.applied_digest = projection;
    record.artifact_sha256 = trusted;
    record.listen_port = 9;
    record.apply_status = "applied".into();
    record.desired_running = true;
    record.owned_origin = "http://127.0.0.1:9".into();
    record.policy_ready = true;
    {
        let db = state.db.lock();
        super::store::save(&db.conn, &record).expect("synthetic execution record");
    }
    {
        let mut inner = state.cpa_execution.inner.lock();
        inner.child_generation = child;
        inner.verified_ready = true;
        inner.poisoned = false;
        inner.record = record;
    }
    super::ensure_secrets(state).expect("synthetic owned secrets");
}

#[cfg(test)]
pub(crate) fn test_move_applied_child(state: &CoreStateInner) {
    let mut record = state.cpa_execution.inner.lock().record.clone();
    let generation = state.process_generation();
    let moved = record.child_generation.wrapping_add(5);
    let moved = if moved == 0 || moved == generation {
        5
    } else {
        moved
    };
    record.child_generation = moved;
    {
        let db = state.db.lock();
        super::store::save(&db.conn, &record).expect("moved child persists");
    }
    let mut inner = state.cpa_execution.inner.lock();
    inner.record.child_generation = moved;
    let mut memory = moved.wrapping_add(1);
    if memory == 0 || memory == generation {
        memory = 7;
    }
    inner.child_generation = memory;
}

#[cfg(test)]
fn yaml_quote(value: &str) -> String {
    let mut quoted = String::from("\"");
    for ch in value.chars() {
        if ch == '\\' || ch == '"' {
            quoted.push('\\');
        }
        quoted.push(ch);
    }
    quoted.push('"');
    quoted
}

#[cfg(test)]
pub(crate) fn prepare_owned_ready_fetch(state: &crate::state::CoreState, port: u16, origin: &str) {
    let mut inner = state.cpa_execution.inner.lock();
    inner.record.listen_port = port;
    inner.public_origin = origin.to_string();
    inner.record.public_origin = origin.to_string();
}

#[cfg(test)]
pub(crate) fn write_synthetic_auth_placeholder(state: &crate::state::CoreState, name: &str) {
    let path = super::io::auth_dir(&state.data_dir).join(name);
    std::fs::write(&path, b"synthetic-auth-file-not-a-token").expect("synthetic auth placeholder");
}

#[cfg(test)]
pub(crate) fn synthetic_codex_credential_id() -> String {
    let account =
        crate::db::native_binding::account_id_for("codex", "codex.json").expect("codex account id");
    ocg_domain::credential::credential_id_for_legacy_account(&account).to_string()
}

/// Object-safe binding so the guard can name a `DeviceSession` that lives in
/// the private `cpa_runtime::device` module. The session `Arc` and epoch stay
/// inside the fence object created next to that session.
pub(crate) trait DevicePollFence: Send + Sync {
    fn owner_status(&self, state: &CoreStateInner) -> crate::cpa::CpaOAuthStatus;
    fn poll_cancelled(&self) -> bool;
    fn begin_ready_fetch(&self);
    fn end_ready_fetch(&self);
    fn release_flight(&self);
}

/// RAII owner of one device poll. Drop releases only the fence it captured.
pub(crate) struct DeviceOperationGuard {
    fence: Option<Arc<dyn DevicePollFence>>,
}

impl DeviceOperationGuard {
    pub(crate) fn from_fence(fence: Arc<dyn DevicePollFence>) -> Self {
        Self { fence: Some(fence) }
    }

    pub(crate) fn owner_status(&self, state: &CoreStateInner) -> crate::cpa::CpaOAuthStatus {
        self.fence
            .as_ref()
            .expect("device poll guard")
            .owner_status(state)
    }

    pub(crate) fn cancelled(&self) -> bool {
        self.fence
            .as_ref()
            .is_some_and(|fence| fence.poll_cancelled())
    }

    pub(crate) fn begin_ready_fetch(&self) {
        if let Some(fence) = &self.fence {
            fence.begin_ready_fetch();
        }
    }

    pub(crate) fn end_ready_fetch(&self) {
        if let Some(fence) = &self.fence {
            fence.end_ready_fetch();
        }
    }
}

impl Drop for DeviceOperationGuard {
    fn drop(&mut self) {
        if let Some(fence) = self.fence.take() {
            fence.release_flight();
        }
    }
}

pub(crate) enum DevicePollGate {
    NotDevice,
    Ready(DeviceOperationGuard),
    Busy(crate::cpa::CpaOAuthStatus),
}

#[cfg(test)]
struct PauseBits {
    entered: Arc<AtomicBool>,
    release: Arc<AtomicBool>,
}

#[cfg(test)]
thread_local! {
    static RECONCILE_PAUSE: std::cell::RefCell<Option<PauseBits>> = const { std::cell::RefCell::new(None) };
    static APPLY_PAUSE: std::cell::RefCell<Option<PauseBits>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
#[derive(Clone, Copy)]
enum PauseSlot {
    Reconcile,
    Apply,
}

#[cfg(test)]
pub(crate) struct DevicePollPause {
    entered: Arc<AtomicBool>,
    release: Arc<AtomicBool>,
    slot: PauseSlot,
}

#[cfg(test)]
impl DevicePollPause {
    pub(crate) fn entered(&self) -> bool {
        self.entered.load(std::sync::atomic::Ordering::SeqCst)
    }

    pub(crate) fn release(&self) {
        self.release
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

#[cfg(test)]
impl Drop for DevicePollPause {
    fn drop(&mut self) {
        self.release
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let release = Arc::clone(&self.release);
        let slot: &'static std::thread::LocalKey<std::cell::RefCell<Option<PauseBits>>> =
            match self.slot {
                PauseSlot::Reconcile => &RECONCILE_PAUSE,
                PauseSlot::Apply => &APPLY_PAUSE,
            };
        slot.with(|cell| {
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
fn arm_pause(
    slot: &'static std::thread::LocalKey<std::cell::RefCell<Option<PauseBits>>>,
    kind: PauseSlot,
) -> DevicePollPause {
    let entered = Arc::new(AtomicBool::new(false));
    let release = Arc::new(AtomicBool::new(false));
    slot.with(|cell| {
        *cell.borrow_mut() = Some(PauseBits {
            entered: Arc::clone(&entered),
            release: Arc::clone(&release),
        });
    });
    DevicePollPause {
        entered,
        release,
        slot: kind,
    }
}

#[cfg(test)]
pub(crate) fn arm_device_reconcile_pause() -> DevicePollPause {
    arm_pause(&RECONCILE_PAUSE, PauseSlot::Reconcile)
}

#[cfg(test)]
pub(crate) fn arm_device_apply_pause() -> DevicePollPause {
    arm_pause(&APPLY_PAUSE, PauseSlot::Apply)
}

#[cfg(test)]
async fn await_pause_slot(
    slot: &'static std::thread::LocalKey<std::cell::RefCell<Option<PauseBits>>>,
) {
    let bits = slot.with(|cell| cell.borrow_mut().take());
    if let Some(bits) = bits {
        bits.entered
            .store(true, std::sync::atomic::Ordering::SeqCst);
        while !bits.release.load(std::sync::atomic::Ordering::SeqCst) {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    }
}

#[cfg(test)]
impl CoreStateInner {
    pub(crate) async fn await_device_reconcile_pause(&self) {
        let _ = self;
        await_pause_slot(&RECONCILE_PAUSE).await;
    }

    pub(crate) async fn await_device_apply_pause(&self) {
        let _ = self;
        await_pause_slot(&APPLY_PAUSE).await;
    }
}

#[cfg(test)]
mod tests;

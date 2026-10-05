//! Managed Codex device login using CPA's own CLI. Tokens never enter OCG.
//! A separate owned Host preserves the running gateway and process containment.

use super::*;
use crate::cpa::{CpaOAuthProvider, CpaOAuthStart, CpaOAuthStatus};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

const STATE_PREFIX: &str = "ocg-device-";
const DEVICE_URL: &str = "https://auth.openai.com/codex/device";
const AUTH_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const PROMPT_TIMEOUT: Duration = Duration::from_secs(15);

pub(super) struct DeviceSession {
    state: String,
    host: CpaRuntimeHost,
    deadline: Instant,
    result: Mutex<DeviceResult>,
    /// Present when this session was started from the owned execution plane.
    /// Session fixtures leave it empty so stdout parsing stays local.
    /// The handler notes its own commits on this copy. The prompt loop keeps
    /// the clone taken before that note.
    completion: Mutex<Option<crate::cpa_execution::device::DeviceCompletion>>,
    /// Even values are idle. An odd value is the epoch of the current owner.
    flight_epoch: AtomicU64,
    /// Set to the owner's odd epoch when cancel lands during that poll.
    cancelled_epoch: AtomicU64,
    /// The odd epoch whose owner is inside ready IO, or zero.
    /// A newer poll may take that flight. Reconcile and apply leave this zero,
    /// so those sections stay exclusive.
    ready_epoch: AtomicU64,
}

#[derive(Default)]
struct DeviceResult {
    code: Option<String>,
    terminal: Option<CpaOAuthStatus>,
}

impl DeviceSession {
    fn refresh(&self) {
        let mut result = self.result.lock();
        if result.terminal.is_some() {
            return;
        }
        // Only stdout protocol markers are consumed. Neither stream is exposed
        // via runtime logs or forwarded in errors (upstream errors may contain bodies).
        let running = self.host.owned_running();
        if !running {
            let _ = self.host.stop_owned();
        }
        let logs = self.host.logs();
        if result.code.is_none() {
            result.code = parse_device_code(&logs.stdout);
        }
        let status = if logs
            .stdout
            .lines()
            .any(|line| line.trim() == "Codex device authentication successful!")
            && logs
                .stdout
                .lines()
                .any(|line| line.starts_with("Authentication saved to "))
        {
            Some(CpaOAuthStatus {
                status: "ok".into(),
                error: None,
            })
        } else if !running {
            Some(failed(
                "CPA device login failed. Check network/proxy connectivity and enable device code login in ChatGPT security or workspace settings.",
            ))
        } else if Instant::now() >= self.deadline {
            Some(CpaOAuthStatus {
                status: "expired".into(),
                error: Some("CPA device login expired; start again for a new code.".into()),
            })
        } else {
            None
        };
        if let Some(status) = status {
            result.terminal = Some(status);
            let _ = self.host.stop_owned();
        }
    }

    /// `true` is a new cancellation. A newly claimed in-flight epoch whose
    /// terminal is already set, including raw `ok`, keeps that terminal and
    /// does not stop the helper again. A repeat of the same epoch is `false`.
    /// A finished terminal with no owner is `false` and stores nothing.
    fn cancel(&self) -> bool {
        let mut result = self.result.lock();
        let newly_marked = self.claim_inflight_cancel();
        if result.terminal.is_some() {
            return newly_marked;
        }
        result.terminal = Some(CpaOAuthStatus {
            status: "cancelled".into(),
            error: None,
        });
        let _ = self.host.stop_owned();
        true
    }

    fn operation_guard(
        self: &Arc<Self>,
        epoch: u64,
    ) -> crate::cpa_execution::device::DeviceOperationGuard {
        crate::cpa_execution::device::DeviceOperationGuard::from_fence(Arc::new(SessionFence {
            session: Arc::clone(self),
            epoch,
        }))
    }

    /// A poll blocked in ready IO does not own reconcile. A newer poll takes
    /// that flight. The previous owner sees the moved epoch and must not write.
    fn preempt_ready_fetch(
        self: &Arc<Self>,
        observed: u64,
    ) -> Option<crate::cpa_execution::device::DeviceOperationGuard> {
        if self
            .ready_epoch
            .compare_exchange(observed, 0, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return None;
        }
        let next = observed.wrapping_add(2);
        if self
            .flight_epoch
            .compare_exchange(observed, next, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            let _ =
                self.ready_epoch
                    .compare_exchange(0, observed, Ordering::AcqRel, Ordering::Acquire);
            return None;
        }
        Some(self.operation_guard(next))
    }

    fn begin_operation(
        self: &Arc<Self>,
    ) -> Option<crate::cpa_execution::device::DeviceOperationGuard> {
        let observed = self.flight_epoch.load(Ordering::Acquire);
        if observed & 1 == 1 {
            return self.preempt_ready_fetch(observed);
        }
        let epoch = observed.wrapping_add(1);
        if self
            .flight_epoch
            .compare_exchange(observed, epoch, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return None;
        }
        Some(self.operation_guard(epoch))
    }

    fn operation_in_flight(&self) -> bool {
        self.flight_epoch.load(Ordering::Acquire) & 1 == 1
    }

    fn poll_cancelled(&self) -> bool {
        let epoch = self.flight_epoch.load(Ordering::Acquire);
        epoch & 1 == 1 && self.cancelled_epoch.load(Ordering::Acquire) == epoch
    }

    /// Records the current odd poll epoch once. An even epoch, a repeat of the
    /// same epoch, or a flight that ended during the store writes nothing that
    /// remains visible and returns `false`.
    fn claim_inflight_cancel(&self) -> bool {
        let epoch = self.flight_epoch.load(Ordering::Acquire);
        if epoch & 1 != 1 {
            return false;
        }
        let previous = self.cancelled_epoch.load(Ordering::Acquire);
        if previous == epoch {
            return false;
        }
        if self
            .cancelled_epoch
            .compare_exchange(previous, epoch, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return false;
        }
        if self.flight_epoch.load(Ordering::Acquire) == epoch {
            return true;
        }
        let _ = self.cancelled_epoch.compare_exchange(
            epoch,
            previous,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
        false
    }

    fn force_stale(&self) {
        let mut result = self.result.lock();
        if result
            .terminal
            .as_ref()
            .is_some_and(|status| status.status == "ok")
        {
            result.terminal = Some(failed("CPA device login completion is no longer current."));
            let _ = self.host.stop_owned();
        }
    }

    fn status(&self) -> CpaOAuthStatus {
        self.refresh();
        self.result
            .lock()
            .terminal
            .clone()
            .unwrap_or(CpaOAuthStatus {
                status: "wait".into(),
                error: None,
            })
    }
}

impl Drop for DeviceSession {
    fn drop(&mut self) {
        let _ = self.host.stop_owned();
    }
}

fn failed(message: &str) -> CpaOAuthStatus {
    CpaOAuthStatus {
        status: "error".into(),
        error: Some(message.into()),
    }
}

fn parse_device_code(stdout: &str) -> Option<String> {
    if !stdout
        .lines()
        .any(|line| line.trim() == format!("Codex device URL: {DEVICE_URL}"))
    {
        return None;
    }
    stdout
        .split_inclusive('\n')
        .filter(|line| line.ends_with('\n'))
        .find_map(|line| {
            let code = line.strip_prefix("Codex device code: ")?.trim();
            (code.len() >= 4
                && code.len() <= 32
                && code.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'))
            .then(|| code.to_string())
        })
}

// Cancellation of an HTTP start future also releases the child.
struct PendingStart(Option<Arc<DeviceSession>>);
impl Drop for PendingStart {
    fn drop(&mut self) {
        if let Some(session) = self.0.take() {
            session.cancel();
        }
    }
}

impl CpaRuntimeCapabilities {
    pub(crate) fn cancel_device_login(&self) {
        if let Some(session) = self.device.lock().as_ref() {
            session.cancel();
        }
    }
}

impl CoreStateInner {
    pub async fn start_cpa_device_oauth(&self) -> Result<CpaOAuthStart, CpaRuntimeError> {
        let mut launch = crate::cpa_execution::device::owned_device_launch(self)
            .map_err(map_device_execution)?;
        reject_reparse_tree(&launch.working_dir)?;
        reject_reparse_ancestors(&launch.executable)?;
        reject_reparse_ancestors(&launch.config_path)?;
        reject_reparse_ancestors(&launch.auth_dir)?;
        ensure_unix_executable(&launch.executable)?;
        let completion = launch.completion.clone();
        let executable = std::mem::take(&mut launch.executable);
        let config_path = std::mem::take(&mut launch.config_path);
        let working_dir = std::mem::take(&mut launch.working_dir);
        let management_password = std::mem::take(&mut launch.management_password);
        let log_secrets = std::mem::take(&mut launch.log_secrets)
            .into_iter()
            .map(CpaRuntimeSecret::new)
            .collect();
        drop(launch);
        let session = {
            let mut slot = self.cpa_runtime.device.lock();
            if slot.as_ref().is_some_and(|s| s.status().status == "wait") {
                return Err(CpaRuntimeError::Conflict(
                    "A Codex device login is already waiting for authorization.".into(),
                ));
            }
            let host = open_device_host()?;
            host.start_owned(&CpaRuntimeProcessSpec {
                codex_device_login: true,
                executable,
                config_path,
                working_dir,
                management_password: CpaRuntimeSecret::new(management_password),
                log_secrets,
            })?;
            let session = Arc::new(DeviceSession {
                state: format!("{STATE_PREFIX}{}", uuid::Uuid::new_v4().simple()),
                host,
                deadline: Instant::now() + AUTH_TIMEOUT,
                result: Mutex::new(DeviceResult::default()),
                completion: Mutex::new(Some(completion.clone())),
                flight_epoch: AtomicU64::new(0),
                cancelled_epoch: AtomicU64::new(0),
                ready_epoch: AtomicU64::new(0),
            });
            *slot = Some(session.clone());
            session
        };
        let mut pending = PendingStart(Some(session.clone()));
        let weak = Arc::downgrade(&session);
        std::thread::Builder::new()
            .name("cpa-device-login".into())
            .spawn(move || {
                loop {
                    let Some(session) = weak.upgrade() else {
                        break;
                    };
                    session.refresh();
                    if session.result.lock().terminal.is_some() {
                        break;
                    }
                    drop(session);
                    std::thread::sleep(Duration::from_millis(100));
                }
            })
            .map_err(|_| CpaRuntimeError::Failed("Could not monitor CPA device login.".into()))?;
        let prompt_deadline = Instant::now() + PROMPT_TIMEOUT;
        loop {
            if let Err(error) = crate::cpa_execution::device::completion_current(self, &completion)
            {
                session.force_stale();
                session.cancel();
                return Err(map_device_execution(error));
            }
            let (code, terminal) = {
                let result = session.result.lock();
                (result.code.clone(), result.terminal.clone())
            };
            if let Some(status) = terminal {
                return Err(CpaRuntimeError::Failed(status.error.unwrap_or_else(|| {
                    "CPA device login ended before its prompt was ready.".into()
                })));
            }
            if let Some(code) = code {
                pending.0.take();
                return Ok(CpaOAuthStart {
                    provider: CpaOAuthProvider::Codex,
                    state: session.state.clone(),
                    url: DEVICE_URL.into(),
                    flow: "device".into(),
                    user_code: Some(code),
                    expires_in: Some(
                        session
                            .deadline
                            .saturating_duration_since(Instant::now())
                            .as_secs(),
                    ),
                });
            }
            if Instant::now() >= prompt_deadline {
                return Err(CpaRuntimeError::Failed("CPA did not return a device code in time. Check network/proxy connectivity and CPA device-login support.".into()));
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    pub fn cpa_device_oauth_status(
        &self,
        state: &str,
    ) -> Option<Result<CpaOAuthStatus, CpaRuntimeError>> {
        let session = session_for(self, state)?;
        Some(session.map(|session| {
            if session.operation_in_flight() {
                session.status()
            } else {
                fenced_status_unchecked(self, &session)
            }
        }))
    }

    pub fn cancel_cpa_device_oauth(&self, state: &str) -> Option<Result<bool, CpaRuntimeError>> {
        let session = session_for(self, state)?;
        Some(session.map(|session| {
            if !session.operation_in_flight() {
                let _ = fenced_status_unchecked(self, &session);
            }
            session.cancel()
        }))
    }

    pub(crate) fn try_begin_device_poll(
        &self,
        oauth_state: &str,
    ) -> Result<crate::cpa_execution::device::DevicePollGate, CpaRuntimeError> {
        let Some(found) = session_for(self, oauth_state) else {
            return Ok(crate::cpa_execution::device::DevicePollGate::NotDevice);
        };
        let session = found?;
        match session.begin_operation() {
            Some(guard) => Ok(crate::cpa_execution::device::DevicePollGate::Ready(guard)),
            None => Ok(crate::cpa_execution::device::DevicePollGate::Busy(
                session.status(),
            )),
        }
    }

    /// Unchecked fence for the poll that already holds the operation guard.
    /// Shared status reads stay on `cpa_device_oauth_status`.
    pub(crate) fn device_owner_oauth_status(
        &self,
        oauth_state: &str,
    ) -> Option<Result<CpaOAuthStatus, CpaRuntimeError>> {
        let session = session_for(self, oauth_state)?;
        Some(session.map(|session| fenced_status_unchecked(self, &session)))
    }

    pub(crate) fn owned_device_captured_revision(&self, oauth_state: &str) -> Option<u64> {
        let session = session_for(self, oauth_state)?.ok()?;
        session
            .completion
            .lock()
            .as_ref()
            .map(|completion| completion.settings_revision)
    }

    pub(crate) fn owned_device_completion(
        &self,
        oauth_state: &str,
    ) -> Option<crate::cpa_execution::device::DeviceCompletion> {
        let session = session_for(self, oauth_state)?.ok()?;
        session.completion.lock().clone()
    }

    /// Records this session's own settings bump. A foreign revision is refused.
    pub(crate) fn note_owned_device_settings(
        &self,
        oauth_state: &str,
        observed: u64,
        committed: u64,
    ) -> bool {
        let Some(Ok(session)) = session_for(self, oauth_state) else {
            return false;
        };
        let mut slot = session.completion.lock();
        let Some(completion) = slot.as_mut() else {
            return false;
        };
        let current = if completion.committed_settings_revision != 0 {
            completion.committed_settings_revision
        } else {
            completion.settings_revision
        };
        if current != observed || committed != observed.wrapping_add(1) {
            return false;
        }
        completion.committed_settings_revision = committed;
        true
    }

    /// Stores the plane and lease this session just committed.
    pub(crate) fn accept_owned_device_write(
        &self,
        oauth_state: &str,
        child_generation: u64,
        applied_revision: u64,
        applied_digest: String,
        settings_revision: u64,
        lease: crate::cpa_execution::NativeLease,
    ) -> bool {
        let Some(Ok(session)) = session_for(self, oauth_state) else {
            return false;
        };
        if self.settings_revision() != settings_revision {
            return false;
        }
        if lease.child_generation != 0 && lease.child_generation != child_generation {
            return false;
        }
        let mut slot = session.completion.lock();
        let Some(completion) = slot.as_mut() else {
            return false;
        };
        let expected = if completion.committed_settings_revision != 0 {
            completion.committed_settings_revision
        } else {
            completion.settings_revision
        };
        if expected != settings_revision {
            return false;
        }
        completion.child_generation = child_generation;
        completion.applied_revision = applied_revision;
        completion.applied_digest = applied_digest;
        completion.lease = lease;
        completion.admitted = true;
        true
    }

    /// Stores the lease of a reconcile this session just committed.
    /// The child and applied tuple stay until `accept_owned_device_write`.
    pub(crate) fn note_owned_device_lease(
        &self,
        oauth_state: &str,
        settings_revision: u64,
        child_generation: u64,
        lease: crate::cpa_execution::NativeLease,
    ) -> bool {
        let Some(Ok(session)) = session_for(self, oauth_state) else {
            return false;
        };
        if self.settings_revision() != settings_revision {
            return false;
        }
        if lease.child_generation != 0 && lease.child_generation != child_generation {
            return false;
        }
        let mut slot = session.completion.lock();
        let Some(completion) = slot.as_mut() else {
            return false;
        };
        let expected = if completion.committed_settings_revision != 0 {
            completion.committed_settings_revision
        } else {
            completion.settings_revision
        };
        if expected != settings_revision || completion.child_generation != child_generation {
            return false;
        }
        completion.lease = lease;
        true
    }
}

fn session_for(
    state: &CoreStateInner,
    oauth_state: &str,
) -> Option<Result<Arc<DeviceSession>, CpaRuntimeError>> {
    if !oauth_state.starts_with(STATE_PREFIX) {
        return None;
    }
    Some(
        state
            .cpa_runtime
            .device
            .lock()
            .as_ref()
            .filter(|session| session.state == oauth_state)
            .cloned()
            .ok_or_else(|| {
                CpaRuntimeError::Invalid("Unknown or replaced CPA device login session.".into())
            }),
    )
}

fn fenced_status_unchecked(state: &CoreStateInner, session: &DeviceSession) -> CpaOAuthStatus {
    let status = session.status();
    if status.status != "ok" {
        return status;
    }
    let Some(completion) = session.completion.lock().clone() else {
        return status;
    };
    if crate::cpa_execution::device::completion_current(state, &completion).is_ok() {
        return status;
    }
    session.force_stale();
    session.status()
}

struct SessionFence {
    session: Arc<DeviceSession>,
    epoch: u64,
}

impl crate::cpa_execution::device::DevicePollFence for SessionFence {
    fn owner_status(&self, state: &CoreStateInner) -> CpaOAuthStatus {
        fenced_status_unchecked(state, &self.session)
    }

    fn poll_cancelled(&self) -> bool {
        self.session.cancelled_epoch.load(Ordering::Acquire) == self.epoch
            || self.session.flight_epoch.load(Ordering::Acquire) != self.epoch
    }

    fn begin_ready_fetch(&self) {
        self.session
            .ready_epoch
            .store(self.epoch, Ordering::Release);
    }

    fn end_ready_fetch(&self) {
        let _ = self.session.ready_epoch.compare_exchange(
            self.epoch,
            0,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
    }

    fn release_flight(&self) {
        let _ = self.session.flight_epoch.compare_exchange(
            self.epoch,
            self.epoch.wrapping_add(1),
            Ordering::AcqRel,
            Ordering::Acquire,
        );
    }
}

fn map_device_execution(error: crate::cpa_execution::ExecutionError) -> CpaRuntimeError {
    use crate::cpa_execution::ExecutionError;
    match error {
        ExecutionError::ApplyConflict(message) => CpaRuntimeError::Conflict(message),
        ExecutionError::Unavailable(message) => CpaRuntimeError::Unavailable(message),
        ExecutionError::Invalid(message) => CpaRuntimeError::Invalid(message),
        ExecutionError::ApplyFailed(message) => CpaRuntimeError::Failed(message),
        ExecutionError::RollbackUnavailable => {
            CpaRuntimeError::Failed("rollback_unavailable".into())
        }
    }
}

#[cfg(test)]
tokio::task_local! {
    static TEST_DEVICE_HOST: CpaRuntimeHost;
}

fn open_device_host() -> Result<CpaRuntimeHost, CpaRuntimeError> {
    #[cfg(test)]
    {
        return TEST_DEVICE_HOST.try_with(|host| host.clone()).map_err(|_| {
            CpaRuntimeError::Unavailable("device login test host is not installed".into())
        });
    }
    #[cfg(not(test))]
    {
        host::new_device_host()
    }
}

impl CoreStateInner {
    #[cfg(test)]
    pub(crate) async fn with_test_device_host<T>(
        host: CpaRuntimeHost,
        future: impl std::future::Future<Output = T>,
    ) -> T {
        TEST_DEVICE_HOST.scope(host, future).await
    }
}

#[cfg(test)]
mod tests;

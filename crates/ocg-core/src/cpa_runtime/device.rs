//! Managed Codex device login using CPA's own CLI. Tokens never enter OCG.
//! A separate owned Host preserves the running gateway and process containment.

use super::*;
use crate::cpa::{CpaOAuthProvider, CpaOAuthStart, CpaOAuthStatus};
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
    operation: Mutex<Option<crate::user_operation::UserOperation>>,
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

    fn cancel(&self) -> bool {
        let mut result = self.result.lock();
        if result.terminal.is_some() {
            return false;
        }
        result.terminal = Some(CpaOAuthStatus {
            status: "cancelled".into(),
            error: None,
        });
        let _ = self.host.stop_owned();
        true
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

    pub(super) fn attach_operation(&self, operation: crate::user_operation::UserOperation) {
        *self.operation.lock() = Some(operation);
        self.finish_receipt_if_terminal();
    }

    /// Finish the attached operation only after the result mutex is released.
    /// Unknown and `wait` statuses stay pending. Drop does not guess.
    pub(super) fn finish_receipt_if_terminal(&self) {
        let status = self.result.lock().terminal.clone();
        let Some(status) = status else {
            return;
        };
        let Some((outcome, reason)) = device_terminal_receipt(&status.status) else {
            return;
        };
        let Some(operation) = self.operation.lock().take() else {
            return;
        };
        operation.complete(
            outcome,
            reason,
            crate::log_types::OperationMetadata::default(),
        );
    }
}

fn device_terminal_receipt(
    status: &str,
) -> Option<(crate::log_types::OperationOutcome, Option<&'static str>)> {
    use crate::log_types::OperationOutcome::{Failed, Rejected, Success};
    match status {
        "ok" => Some((Success, None)),
        "error" => Some((Failed, Some("outboundFailed"))),
        "expired" => Some((Failed, Some("expired"))),
        "cancelled" => Some((Rejected, Some("cancelled"))),
        _ => None,
    }
}

impl Drop for DeviceSession {
    fn drop(&mut self) {
        let _ = self.host.stop_owned();
    }
}

/// The session Arc returned by start. Attachment does not look up the slot.
pub struct CpaDeviceLoginSession {
    session: Arc<DeviceSession>,
}

impl CpaDeviceLoginSession {
    pub fn attach(self, operation: crate::user_operation::UserOperation) {
        self.session.attach_operation(operation);
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
    pub(super) fn cancel_device_login(&self) {
        if let Some(session) = self.device.lock().as_ref() {
            session.cancel();
        }
    }
}

impl CoreStateInner {
    pub async fn start_cpa_device_oauth(
        &self,
    ) -> Result<(CpaOAuthStart, CpaDeviceLoginSession), CpaRuntimeError> {
        self.require_supported()?;
        if std::env::var_os(crate::cpa::CPA_BASE_URL_ENV).is_some() {
            return Err(CpaRuntimeError::Invalid(
                "Device login requires OCG-managed CPA, not an external endpoint.".into(),
            ));
        }
        let managed = require_managed(&self.data_dir)?;
        if !self.cpa_runtime.host()?.owned_running() {
            return Err(CpaRuntimeError::Invalid(
                "Start the OCG-managed CPA runtime before device login.".into(),
            ));
        }
        let working_dir = version_dir(&self.data_dir, &managed.current_version)?;
        reject_reparse_tree(&working_dir)?;
        let executable = find_managed_executable(&working_dir)?;
        ensure_unix_executable(&executable)?;
        let config_path = runtime_dir(&self.data_dir).join(CONFIG_NAME);
        reject_reparse_ancestors(&config_path)?;
        let secrets = self.load_saved_secrets()?;
        let config = fs::read_to_string(&config_path).map_err(fs_error)?;
        let log_secrets = parse_api_keys_from_yaml(&config)?
            .into_iter()
            .chain(std::iter::once(secrets.management_key.clone()))
            .map(CpaRuntimeSecret::new)
            .collect();
        let session = {
            let mut slot = self.cpa_runtime.device.lock();
            if slot.as_ref().is_some_and(|s| s.status().status == "wait") {
                return Err(CpaRuntimeError::Conflict(
                    "A Codex device login is already waiting for authorization.".into(),
                ));
            }
            let host = host::new_device_host()?;
            host.start_owned(&CpaRuntimeProcessSpec {
                codex_device_login: true,
                executable,
                config_path,
                working_dir,
                management_password: CpaRuntimeSecret::new(secrets.management_key),
                log_secrets,
            })?;
            let session = Arc::new(DeviceSession {
                state: format!("{STATE_PREFIX}{}", uuid::Uuid::new_v4().simple()),
                host,
                deadline: Instant::now() + AUTH_TIMEOUT,
                result: Mutex::new(DeviceResult::default()),
                operation: Mutex::new(None),
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
                    session.finish_receipt_if_terminal();
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
                return Ok((
                    CpaOAuthStart {
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
                    },
                    CpaDeviceLoginSession { session },
                ));
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
        if !state.starts_with(STATE_PREFIX) {
            return None;
        }
        Some(
            self.cpa_runtime
                .device
                .lock()
                .as_ref()
                .filter(|s| s.state == state)
                .map(|s| s.status())
                .ok_or_else(|| {
                    CpaRuntimeError::Invalid("Unknown or replaced CPA device login session.".into())
                }),
        )
    }

    pub fn finish_cpa_device_oauth_operation(&self, oauth_state: &str) {
        let session = self
            .cpa_runtime
            .device
            .lock()
            .as_ref()
            .filter(|session| session.state == oauth_state)
            .cloned();
        if let Some(session) = session {
            session.finish_receipt_if_terminal();
        }
    }

    pub fn cancel_cpa_device_oauth(&self, state: &str) -> Option<Result<bool, CpaRuntimeError>> {
        if !state.starts_with(STATE_PREFIX) {
            return None;
        }
        Some(
            self.cpa_runtime
                .device
                .lock()
                .as_ref()
                .filter(|s| s.state == state)
                .map(|s| {
                    s.refresh();
                    s.cancel()
                })
                .ok_or_else(|| {
                    CpaRuntimeError::Invalid("Unknown or replaced CPA device login session.".into())
                }),
        )
    }
}

#[cfg(test)]
mod tests;

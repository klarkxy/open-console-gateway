use super::*;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

struct FakeHost {
    running: AtomicBool,
    stops: AtomicUsize,
    logs: Mutex<CpaRuntimeLogTail>,
}

impl Default for FakeHost {
    fn default() -> Self {
        Self {
            running: AtomicBool::new(false),
            stops: AtomicUsize::new(0),
            logs: Mutex::new(CpaRuntimeLogTail {
                stdout: String::new(),
                stderr: String::new(),
            }),
        }
    }
}

impl CpaRuntimeProcessHost for FakeHost {
    fn start_owned(&self, _: &CpaRuntimeProcessSpec) -> Result<(), CpaRuntimeError> {
        Ok(())
    }
    fn stop_owned(&self) -> Result<(), CpaRuntimeError> {
        self.running.store(false, Ordering::SeqCst);
        self.stops.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    fn owned_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }
    fn logs(&self) -> CpaRuntimeLogTail {
        self.logs.lock().clone()
    }
    fn add_log_secret(&self, _: &CpaRuntimeSecret) {}
}

fn fixture(stdout: &str, running: bool, deadline: Instant) -> (Arc<FakeHost>, Arc<DeviceSession>) {
    let host = Arc::new(FakeHost::default());
    host.running.store(running, Ordering::SeqCst);
    host.logs.lock().stdout = stdout.into();
    let session = Arc::new(DeviceSession {
        state: "ocg-device-test".into(),
        host: host.clone(),
        deadline,
        result: Mutex::new(DeviceResult::default()),
        operation: Mutex::new(None),
    });
    (host, session)
}

#[test]
fn prompt_requires_official_url_and_complete_safe_code() {
    let prompt = format!("Codex device URL: {DEVICE_URL}\r\nCodex device code: ABCD-1234\r\n");
    assert_eq!(parse_device_code(&prompt).as_deref(), Some("ABCD-1234"));
    assert_eq!(
        parse_device_code(&prompt.replace(DEVICE_URL, "https://example.com")),
        None
    );
    assert_eq!(
        parse_device_code(&prompt.replace("ABCD-1234", "<secret>")),
        None
    );
    assert_eq!(parse_device_code("Codex device code: ABCD-1234\n"), None);
    assert_eq!(
        parse_device_code(&format!(
            "Codex device URL: {DEVICE_URL}\nCodex device code: ABCD"
        )),
        None
    );
}

#[test]
fn authenticated_before_save_is_not_success_and_errors_are_sanitized() {
    let (host, session) = fixture(
        "Codex authentication successful\n",
        true,
        Instant::now() + AUTH_TIMEOUT,
    );
    assert_eq!(session.status().status, "wait");
    host.logs.lock().stderr = "access_token=do-not-expose".into();
    host.running.store(false, Ordering::SeqCst);
    let result = session.status();
    assert_eq!(result.status, "error");
    assert!(!result.error.unwrap().contains("do-not-expose"));
}

#[test]
fn saved_success_stops_helper_and_terminal_result_survives_cancel() {
    let (host, session) = fixture(
        "Authentication saved to private-path\nCodex device authentication successful!\n",
        true,
        Instant::now() + AUTH_TIMEOUT,
    );
    assert_eq!(session.status().status, "ok");
    assert!(!host.owned_running());
    assert!(!session.cancel());
    assert_eq!(session.status().status, "ok");
}

#[test]
fn timeout_and_cancel_terminate_only_device_host() {
    let (host, session) = fixture("", true, Instant::now() - Duration::from_secs(1));
    assert_eq!(session.status().status, "expired");
    assert!(!host.owned_running());
    let (host, session) = fixture("", true, Instant::now() + AUTH_TIMEOUT);
    assert!(session.cancel());
    assert!(!session.cancel());
    assert!(!host.owned_running());
    assert_eq!(session.status().status, "cancelled");
}

#[test]
fn lifecycle_and_abandoned_start_cancel_device_without_stopping_gateway() {
    let gateway = Arc::new(FakeHost::default());
    gateway.running.store(true, Ordering::SeqCst);
    let capabilities = CpaRuntimeCapabilities::new();
    capabilities.set_host(gateway.clone());
    let (helper, session) = fixture("", true, Instant::now() + AUTH_TIMEOUT);
    *capabilities.device.lock() = Some(session.clone());
    {
        let _operation = capabilities.begin_lifecycle_operation("update");
    }
    assert!(!helper.owned_running());
    assert!(gateway.owned_running());
    let (helper, session) = fixture("", true, Instant::now() + AUTH_TIMEOUT);
    drop(PendingStart(Some(session)));
    assert!(!helper.owned_running());
}

#[test]
fn dropping_session_releases_owned_helper() {
    let (host, session) = fixture("", true, Instant::now() + AUTH_TIMEOUT);
    drop(session);
    assert!(!host.owned_running());
}

fn open_receipt_state(tag: &str) -> (std::path::PathBuf, crate::state::CoreState) {
    let dir =
        std::env::temp_dir().join(format!("ocg-device-receipt-{tag}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let state = std::sync::Arc::new(
        crate::state::CoreStateInner::new(
            crate::db::Database::open(dir.clone()).unwrap(),
            dir.clone(),
            std::sync::Arc::new(crate::crypto::StaticKeyCipher::new(tag)),
        )
        .unwrap(),
    );
    (dir, state)
}

fn accepted_device_operation(
    state: &crate::state::CoreState,
) -> (String, crate::user_operation::UserOperation) {
    let mut operation = crate::user_operation::UserOperation::dashboard(
        state,
        "cpa.oauth.start",
        "cpa",
        Some("codex".into()),
    );
    let id = operation.operation_id().to_string();
    operation.accepted(crate::log_types::OperationMetadata::default());
    (id, operation)
}

fn stored_receipt(state: &crate::state::CoreState, id: &str) -> (String, Option<String>, String) {
    state
        .db
        .lock()
        .conn
        .query_row(
            "SELECT outcome, reason_code, metadata_json FROM operation_logs WHERE operation_id = ?1",
            [id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap()
}

#[test]
fn device_terminal_status_finishes_the_attached_operation() {
    let cases = [
        (
            "ok",
            "success",
            None,
            true,
            "Codex device authentication successful!\nAuthentication saved to memory\n",
        ),
        ("error", "failed", Some("outboundFailed"), false, ""),
        ("expired", "failed", Some("expired"), true, ""),
    ];
    for (label, outcome, reason, running, stdout) in cases {
        let (dir, state) = open_receipt_state(label);
        let deadline = if label == "expired" {
            Instant::now() - Duration::from_secs(1)
        } else {
            Instant::now() + AUTH_TIMEOUT
        };
        let (_host, session) = fixture(stdout, running, deadline);
        let (id, operation) = accepted_device_operation(&state);
        session.attach_operation(operation);
        session.refresh();
        session.finish_receipt_if_terminal();
        let (stored_outcome, stored_reason, metadata) = stored_receipt(&state, &id);
        assert_eq!(stored_outcome, outcome, "{label}");
        assert_eq!(stored_reason.as_deref(), reason, "{label}");
        assert!(!metadata.contains("CPA device"));
        assert!(!metadata.contains("network"));
        drop(state);
        std::fs::remove_dir_all(dir).ok();
    }

    let (dir, state) = open_receipt_state("cancelled");
    let (_host, session) = fixture("", true, Instant::now() + AUTH_TIMEOUT);
    let (id, operation) = accepted_device_operation(&state);
    session.attach_operation(operation);
    assert!(session.cancel());
    session.finish_receipt_if_terminal();
    let (outcome, reason, metadata) = stored_receipt(&state, &id);
    assert_eq!(outcome, "rejected");
    assert_eq!(reason.as_deref(), Some("cancelled"));
    assert!(!metadata.contains("ocg-device"));
    drop(state);
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn device_completion_before_attachment_still_finishes_that_operation() {
    let (dir, state) = open_receipt_state("race");
    let (_host, session) = fixture(
        "Codex device authentication successful!\nAuthentication saved to memory\n",
        true,
        Instant::now() + AUTH_TIMEOUT,
    );
    session.refresh();
    session.finish_receipt_if_terminal();
    let (id, operation) = accepted_device_operation(&state);
    session.attach_operation(operation);
    let (outcome, reason, _) = stored_receipt(&state, &id);
    assert_eq!(outcome, "success");
    assert!(reason.is_none());
    drop(state);
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn replaced_terminal_session_finishes_the_original_pending_row_once() {
    let (dir, state) = open_receipt_state("barrier");
    let (_host_a, session_a) = fixture(
        "Codex device authentication successful!\nAuthentication saved to memory\n",
        true,
        Instant::now() + AUTH_TIMEOUT,
    );
    *state.cpa_runtime.device.lock() = Some(session_a.clone());
    session_a.refresh();
    assert_eq!(session_a.status().status, "ok");

    let host_b = Arc::new(FakeHost::default());
    host_b.running.store(true, Ordering::SeqCst);
    let session_b = Arc::new(DeviceSession {
        state: "ocg-device-replacement".into(),
        host: host_b,
        deadline: Instant::now() + AUTH_TIMEOUT,
        result: Mutex::new(DeviceResult::default()),
        operation: Mutex::new(None),
    });
    *state.cpa_runtime.device.lock() = Some(session_b.clone());

    let (id, operation) = accepted_device_operation(&state);
    CpaDeviceLoginSession {
        session: session_a.clone(),
    }
    .attach(operation);

    let (outcome, reason, metadata) = stored_receipt(&state, &id);
    assert_eq!(outcome, "success");
    assert!(reason.is_none());
    assert!(!metadata.contains("ocg-device"));
    let count: i64 = state
        .db
        .lock()
        .conn
        .query_row(
            "SELECT COUNT(*) FROM operation_logs WHERE operation_id = ?1",
            [&id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 1);
    assert!(session_b.operation.lock().is_none());

    session_a.finish_receipt_if_terminal();
    session_b.finish_receipt_if_terminal();
    let count_after: i64 = state
        .db
        .lock()
        .conn
        .query_row(
            "SELECT COUNT(*) FROM operation_logs WHERE operation_id = ?1",
            [&id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count_after, 1);
    drop(state);
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn device_wait_leaves_the_attached_operation_pending() {
    let (dir, state) = open_receipt_state("wait");
    let (_host, session) = fixture("", true, Instant::now() + AUTH_TIMEOUT);
    let (id, operation) = accepted_device_operation(&state);
    session.attach_operation(operation);
    session.refresh();
    session.finish_receipt_if_terminal();
    let (outcome, reason, _) = stored_receipt(&state, &id);
    assert_eq!(outcome, "pending");
    assert!(reason.is_none());
    drop(state);
    std::fs::remove_dir_all(dir).ok();
}

/// Opt-in real CPA prompt/cancel check, isolated from the user's auth directory.
#[test]
#[ignore = "requires OCG_CPA_DEVICE_SMOKE_EXECUTABLE and network; never completes account authorization"]
fn official_cpa_device_prompt_and_cancel() {
    let executable = PathBuf::from(
        std::env::var_os("OCG_CPA_DEVICE_SMOKE_EXECUTABLE").expect("official CPA executable"),
    );
    let dir = std::env::temp_dir().join(format!("ocg-cpa-device-smoke-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&dir).unwrap();
    let config_path = dir.join("config.yaml");
    let auth = dir.join("auth");
    fs::write(
        &config_path,
        format!(
            "host: 127.0.0.1\nport: 18317\nauth-dir: '{}'\ndebug: false\nlogging-to-file: false\n",
            auth.display()
        ),
    )
    .unwrap();
    let host = host::new_device_host().unwrap();
    host.start_owned(&CpaRuntimeProcessSpec {
        codex_device_login: true,
        executable,
        config_path,
        working_dir: dir.clone(),
        management_password: CpaRuntimeSecret::new("smoke-management-secret"),
        log_secrets: vec![CpaRuntimeSecret::new("smoke-management-secret")],
    })
    .unwrap();
    let session = Arc::new(DeviceSession {
        state: "ocg-device-smoke".into(),
        host: host.clone(),
        deadline: Instant::now() + Duration::from_secs(30),
        result: Mutex::new(DeviceResult::default()),
        operation: Mutex::new(None),
    });
    let prompt_deadline = Instant::now() + PROMPT_TIMEOUT;
    let received = loop {
        session.refresh();
        let result = session.result.lock();
        if result.code.is_some() {
            break true;
        }
        if result.terminal.is_some() || Instant::now() >= prompt_deadline {
            break false;
        }
        drop(result);
        std::thread::sleep(Duration::from_millis(100));
    };
    session.cancel();
    assert!(!host.owned_running());
    drop(session);
    drop(host);
    fs::remove_dir_all(&dir).unwrap();
    assert!(
        received,
        "CPA did not emit a device prompt within 15 seconds; inspect network/device support separately"
    );
}

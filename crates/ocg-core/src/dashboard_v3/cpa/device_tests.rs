use super::*;
use crate::cpa_runtime::{
    CpaRuntimeHost, CpaRuntimeLogTail, CpaRuntimeProcessHost, CpaRuntimeProcessSpec,
    CpaRuntimeSecret,
};
use crate::crypto::StaticKeyCipher;
use crate::db::Database;
use crate::state::CoreStateInner;
use serde_json::json;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

#[tokio::test]
async fn device_rejects_other_providers_and_stale_requests_without_side_effects() {
    let dir = std::env::temp_dir().join(format!("ocg-device-api-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let state = Arc::new(
        CoreStateInner::new(
            Database::open(dir.clone()).unwrap(),
            dir.clone(),
            Arc::new(StaticKeyCipher::new("device-test")),
        )
        .unwrap(),
    );
    let revision = state.settings_revision();
    let request = |provider: &str, revision: u64| {
        Bytes::from(
            serde_json::to_vec(&json!({
                "provider": provider, "method": "device", "expectedRevision": revision,
                "processGeneration": state.process_generation(),
            }))
            .unwrap(),
        )
    };
    assert!(
        start_oauth(
            State(state.clone()),
            Query(CpaTargetQuery::default()),
            request("anthropic", revision),
        )
        .await
        .is_err()
    );
    let stale = start_oauth(
        State(state.clone()),
        Query(CpaTargetQuery::default()),
        request("codex", revision + 1),
    )
    .await
    .unwrap_err();
    assert_eq!(stale.body.code, super::super::ERROR_REVISION_CONFLICT);
    // No managed Host/installation: do not dispatch an arbitrary external login.
    assert!(
        start_oauth(
            State(state.clone()),
            Query(CpaTargetQuery::default()),
            request("codex", revision),
        )
        .await
        .is_err()
    );
    assert_eq!(state.settings_revision(), revision);
    // Unknown local session must be rejected locally, without needing saved CPA secrets.
    assert!(
        oauth_status(
            State(state.clone()),
            Query(OAuthStatusQuery {
                state: "ocg-device-unknown".into(),
                target: None,
            })
        )
        .await
        .is_err()
    );
    drop(state);
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn device_armed_historical_row_prompts_without_remote_io() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let base_url = format!("http://127.0.0.1:{port}/");
    let plane = open_armed_plane("historical");
    save_historical_remote(&plane.state, &base_url);
    let before = snapshot_historical_remote(&plane.state);
    let revision = plane.state.settings_revision();
    plane.device.set_stdout(&prompt_stdout());
    let Json(started) = CoreStateInner::with_test_device_host(
        prompt_host(&plane.device),
        start_oauth(
            State(plane.state.clone()),
            Query(CpaTargetQuery::default()),
            oauth_body(&plane.state, revision),
        ),
    )
    .await
    .expect("historical row and obsolete env do not veto owned device login");
    assert_eq!(started.revision, revision + 1);
    assert_eq!(started.user_code.as_deref(), Some("ABCD-1234"));
    assert_eq!(plane.device.starts.load(Ordering::SeqCst), 1);
    let polled = oauth_status(
        State(plane.state.clone()),
        Query(OAuthStatusQuery {
            state: started.state.clone(),
            target: None,
        }),
    )
    .await
    .unwrap();
    assert_eq!(polled.status, "wait");
    let Json(cancelled) = cancel_oauth(
        State(plane.state.clone()),
        Bytes::from(
            serde_json::to_vec(&json!({
                "state": started.state,
                "expectedRevision": plane.state.settings_revision(),
                "processGeneration": plane.state.process_generation(),
            }))
            .unwrap(),
        ),
    )
    .await
    .unwrap();
    assert!(!plane.device.running.load(Ordering::SeqCst));
    assert!(cancelled.revision >= started.revision);
    let after = snapshot_historical_remote(&plane.state);
    assert!(after == before, "historical CPA bytes changed");
    assert_eq!(after.base_url, base_url);
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    listener.set_nonblocking(true).unwrap();
    assert!(
        listener.accept().is_err(),
        "device login opened a remote socket"
    );
    drop(plane);
}

#[tokio::test]
async fn device_explicit_integration_target_rejects_before_helper() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let plane = open_armed_plane("integration-target");
    save_historical_remote(&plane.state, &format!("http://127.0.0.1:{port}/"));
    let before = snapshot_historical_remote(&plane.state);
    let revision = plane.state.settings_revision();
    let error = CoreStateInner::with_test_device_host(
        prompt_host(&plane.device),
        start_oauth(
            State(plane.state.clone()),
            Query(CpaTargetQuery {
                target: Some(CpaControlTarget::Integration),
            }),
            oauth_body(&plane.state, revision),
        ),
    )
    .await
    .unwrap_err();
    assert_eq!(error.body.code, super::super::ERROR_INVALID_REQUEST);
    assert!(
        error.body.message.contains("explicit migration")
            || error.body.message.contains("not a target"),
        "{}",
        error.body.message
    );
    assert_eq!(plane.device.starts.load(Ordering::SeqCst), 0);
    assert_eq!(plane.state.settings_revision(), revision);
    let after = snapshot_historical_remote(&plane.state);
    assert!(after == before, "historical CPA bytes changed");
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    listener.set_nonblocking(true).unwrap();
    assert!(
        listener.accept().is_err(),
        "integration target opened a socket"
    );
    drop(plane);
}

struct PromptHost {
    starts: AtomicUsize,
    stops: AtomicUsize,
    running: AtomicBool,
    stdout: Mutex<String>,
}

impl PromptHost {
    fn running() -> Self {
        Self {
            starts: AtomicUsize::new(0),
            stops: AtomicUsize::new(0),
            running: AtomicBool::new(true),
            stdout: Mutex::new(String::new()),
        }
    }

    fn idle() -> Self {
        Self {
            running: AtomicBool::new(false),
            ..Self::running()
        }
    }

    fn set_stdout(&self, text: &str) {
        *self.stdout.lock().unwrap() = text.to_string();
    }
}

impl CpaRuntimeProcessHost for PromptHost {
    fn start_owned(
        &self,
        _: &CpaRuntimeProcessSpec,
    ) -> Result<(), crate::cpa_runtime::CpaRuntimeError> {
        self.starts.fetch_add(1, Ordering::SeqCst);
        self.running.store(true, Ordering::SeqCst);
        Ok(())
    }
    fn stop_owned(&self) -> Result<(), crate::cpa_runtime::CpaRuntimeError> {
        self.running.store(false, Ordering::SeqCst);
        self.stops.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    fn owned_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }
    fn logs(&self) -> CpaRuntimeLogTail {
        CpaRuntimeLogTail {
            stdout: self.stdout.lock().unwrap().clone(),
            stderr: String::new(),
        }
    }
    fn add_log_secret(&self, _: &CpaRuntimeSecret) {}
}

struct TempDir(std::path::PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct ArmedPlane {
    state: CoreState,
    device: Arc<PromptHost>,
    _dir: TempDir,
}

fn open_armed_plane(name: &str) -> ArmedPlane {
    let dir = std::env::temp_dir().join(format!("ocg-device-{name}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let state = Arc::new(
        CoreStateInner::new(
            Database::open(dir.clone()).unwrap(),
            dir.clone(),
            Arc::new(StaticKeyCipher::new("device-armed")),
        )
        .unwrap(),
    );
    let runtime = Arc::new(PromptHost::running());
    let registered: CpaRuntimeHost = runtime;
    state.set_cpa_runtime_host(registered);
    crate::cpa_execution::device::arm_synthetic_owned_plane(&state);
    ArmedPlane {
        state,
        device: Arc::new(PromptHost::idle()),
        _dir: TempDir(dir),
    }
}

fn prompt_host(host: &Arc<PromptHost>) -> CpaRuntimeHost {
    let host: CpaRuntimeHost = host.clone();
    host
}

fn oauth_body(state: &CoreState, revision: u64) -> Bytes {
    Bytes::from(
        serde_json::to_vec(&json!({
            "provider": "codex",
            "method": "device",
            "expectedRevision": revision,
            "processGeneration": state.process_generation(),
        }))
        .unwrap(),
    )
}

fn prompt_stdout() -> String {
    "Codex device URL: https://auth.openai.com/codex/device\nCodex device code: ABCD-1234\n".into()
}

fn success_stdout() -> String {
    "Authentication saved to private-path\nCodex device authentication successful!\n".into()
}

struct HistoricalRemoteBytes {
    base_url: String,
    management_key_cipher: String,
    account_key_cipher: String,
    destination_base_url: Option<String>,
    observer_key_cipher: String,
}

impl PartialEq for HistoricalRemoteBytes {
    fn eq(&self, other: &Self) -> bool {
        self.base_url == other.base_url
            && self.management_key_cipher == other.management_key_cipher
            && self.account_key_cipher == other.account_key_cipher
            && self.destination_base_url == other.destination_base_url
            && self.observer_key_cipher == other.observer_key_cipher
    }
}

fn save_historical_remote(state: &CoreStateInner, base_url: &str) {
    let inference = state
        .cipher
        .encrypt("synthetic-historical-inference")
        .unwrap();
    let management = state
        .cipher
        .encrypt("synthetic-historical-management")
        .unwrap();
    let now = chrono::Utc::now();
    let account = crate::models::Account {
        id: crate::provider::CPA_ACCOUNT_ID.to_string(),
        provider_id: crate::provider::CPA_PROVIDER_ID.to_string(),
        credential_kind: crate::provider::CredentialKind::ApiKey,
        quota_scope: crate::provider::QuotaScope::Key,
        name: "synthetic-historical-cpa".into(),
        username: None,
        password_cipher: None,
        key_cipher: inference,
        enabled: false,
        account_type: crate::models::AccountType::Key,
        setup_step: crate::models::AccountSetupStep::Ready,
        referral_code: None,
        purchase_date: String::new(),
        expires_on: String::new(),
        cooldown_until: None,
        cooldown_generic_until: None,
        cooldown_5h_until: None,
        cooldown_week_until: None,
        cooldown_month_until: None,
        cooldown_free_until: None,
        last_error: None,
        auth_error: None,
        notes: None,
        created_at: now,
        updated_at: now,
    };
    state
        .db
        .lock()
        .upsert_cpa_integration(&account, base_url, &management)
        .unwrap();
}

fn snapshot_historical_remote(state: &CoreStateInner) -> HistoricalRemoteBytes {
    let db = state.db.lock();
    let record = db.cpa_integration().unwrap().unwrap();
    let account_key_cipher = db
        .get_account(crate::provider::CPA_ACCOUNT_ID)
        .unwrap()
        .unwrap()
        .key_cipher;
    let destination_base_url = db
        .conn
        .query_row(
            "SELECT base_url FROM destinations
             WHERE adapter = 'cpa'
                OR (legacy_kind = 'builtin' AND legacy_id = 'cpa')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let observer_key_cipher = db
        .conn
        .query_row(
            "SELECT key_cipher FROM credentials WHERE id = ?1",
            [ocg_domain::credential::observer_credential_id_for_cpa().as_str()],
            |row| row.get(0),
        )
        .unwrap();
    HistoricalRemoteBytes {
        base_url: record.base_url,
        management_key_cipher: record.management_key_cipher,
        account_key_cipher,
        destination_base_url,
        observer_key_cipher,
    }
}

struct HandlerHooks {
    fail_ready: bool,
}

impl HandlerHooks {
    fn arm(fail_ready: bool) -> Self {
        crate::cpa_execution::set_skip_spawn(true);
        crate::cpa_execution::set_fail_ready(fail_ready);
        crate::cpa_execution::set_test_password(Some("synthetic-device-management".to_string()));
        crate::cpa_execution::set_before_apply_commit(None);
        Self { fail_ready }
    }
}

impl Drop for HandlerHooks {
    fn drop(&mut self) {
        let _ = self.fail_ready;
        crate::cpa_execution::set_skip_spawn(false);
        crate::cpa_execution::set_fail_ready(false);
        crate::cpa_execution::set_test_password(None);
        crate::cpa_execution::set_before_apply_commit(None);
    }
}

fn install_listener(state: &CoreState) {
    let (shutdown, _receiver) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async {});
    *state.gateway.lock() = Some(crate::state::GatewayHandle {
        port: 9,
        listen_addr: "127.0.0.1:9".parse().unwrap(),
        dashboard_is_local: true,
        shutdown,
        task,
    });
}

fn ready_document(credential_id: &str) -> String {
    json!({
        "authRefs": [{
            "relativePath": "codex.json",
            "rawProviderLabel": "codex",
            "effectiveSubtype": "codex",
            "effectiveMode": "",
            "effectiveGenerationBase": "",
            "authId": "codex-device",
            "credentialId": credential_id,
            "credentialVersion": "1",
            "materialRevision": "cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd",
            "registrationEpoch": "1",
            "models": ["gpt-5"],
            "disabled": false,
            "status": "active"
        }]
    })
    .to_string()
}

fn stale_anthropic_document(credential_id: &str) -> String {
    let material = "ab".repeat(32);
    json!({
        "authRefs": [{
            "relativePath": "anthropic.json",
            "rawProviderLabel": "anthropic",
            "effectiveSubtype": "anthropic",
            "effectiveMode": "",
            "effectiveGenerationBase": "",
            "authId": "anthropic-device",
            "credentialId": credential_id,
            "credentialVersion": "1",
            "materialRevision": material,
            "registrationEpoch": "1",
            "models": ["claude-3-5-sonnet"],
            "disabled": false,
            "status": "active"
        }]
    })
    .to_string()
}

fn serve_ready(port: u16, body: String, on_request: Option<Arc<dyn Fn() + Send + Sync>>) -> u16 {
    let listener = if port == 0 {
        std::net::TcpListener::bind("127.0.0.1:0").unwrap()
    } else {
        std::net::TcpListener::bind(("127.0.0.1", port)).unwrap()
    };
    let bound = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        let Ok((mut stream, _)) = listener.accept() else {
            return;
        };
        let mut buffer = [0u8; 4096];
        let _ = std::io::Read::read(&mut stream, &mut buffer);
        if let Some(hook) = on_request {
            hook();
        }
        let header = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        let _ = std::io::Write::write_all(&mut stream, header.as_bytes());
        let _ = std::io::Write::write_all(&mut stream, body.as_bytes());
    });
    bound
}

fn serve_ready_until(
    port: u16,
    body: String,
    entered: Arc<AtomicBool>,
    release: Arc<AtomicBool>,
) -> u16 {
    let listener = if port == 0 {
        std::net::TcpListener::bind("127.0.0.1:0").unwrap()
    } else {
        std::net::TcpListener::bind(("127.0.0.1", port)).unwrap()
    };
    let bound = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        let Ok((mut stream, _)) = listener.accept() else {
            return;
        };
        let mut buffer = [0u8; 4096];
        let _ = std::io::Read::read(&mut stream, &mut buffer);
        entered.store(true, Ordering::SeqCst);
        let started = std::time::Instant::now();
        while !release.load(Ordering::SeqCst) {
            if started.elapsed() > std::time::Duration::from_secs(5) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let header = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        let _ = std::io::Write::write_all(&mut stream, header.as_bytes());
        let _ = std::io::Write::write_all(&mut stream, body.as_bytes());
    });
    bound
}

struct ReleaseReady(Arc<AtomicBool>);

impl Drop for ReleaseReady {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

fn credential_count(state: &CoreState) -> i64 {
    state
        .db
        .lock()
        .conn
        .query_row("SELECT COUNT(*) FROM credentials", [], |row| row.get(0))
        .unwrap()
}

fn credential_id_count(state: &CoreState, id: &str) -> i64 {
    state
        .db
        .lock()
        .conn
        .query_row(
            "SELECT COUNT(*) FROM credentials WHERE id = ?1",
            [id],
            |row| row.get(0),
        )
        .unwrap()
}

fn credential_versions(state: &CoreState, id: &str) -> Option<(i64, i64)> {
    state
        .db
        .lock()
        .conn
        .query_row(
            "SELECT COALESCE(credential_version, 0), COALESCE(auth_state_version, 0)
             FROM credentials WHERE id = ?1",
            [id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .ok()
}

fn model_count(state: &CoreState) -> i64 {
    state
        .db
        .lock()
        .conn
        .query_row(
            "SELECT COUNT(*) FROM destination_models WHERE public_model = 'gpt-5'",
            [],
            |row| row.get(0),
        )
        .unwrap()
}

fn applied_route_count(state: &CoreState) -> usize {
    let json: String = state
        .db
        .lock()
        .conn
        .query_row(
            "SELECT value FROM settings WHERE key = 'cpa_execution_projection_v1'",
            [],
            |row| row.get(0),
        )
        .unwrap_or_default();
    serde_json::from_str::<serde_json::Value>(&json)
        .ok()
        .and_then(|value| {
            value
                .get("appliedRoutes")
                .and_then(|routes| routes.as_array())
                .map(|routes| routes.len())
        })
        .unwrap_or(0)
}

async fn started_device(plane: &ArmedPlane) -> CpaOAuthStart {
    let revision = plane.state.settings_revision();
    plane.device.set_stdout(&prompt_stdout());
    let Json(started) = CoreStateInner::with_test_device_host(
        prompt_host(&plane.device),
        start_oauth(
            State(plane.state.clone()),
            Query(CpaTargetQuery::default()),
            oauth_body(&plane.state, revision),
        ),
    )
    .await
    .expect("handler device start");
    assert_eq!(started.revision, revision + 1);
    assert_eq!(plane.state.settings_revision(), revision + 1);
    started
}

fn arm_ready(plane: &ArmedPlane, on_request: Option<Arc<dyn Fn() + Send + Sync>>) -> String {
    install_listener(&plane.state);
    crate::cpa_execution::device::write_synthetic_auth_placeholder(&plane.state, "codex.json");
    let credential_id = crate::cpa_execution::device::synthetic_codex_credential_id();
    let body = ready_document(&credential_id);
    let port = serve_ready(0, body.clone(), on_request);
    crate::cpa_execution::device::prepare_owned_ready_fetch(
        &plane.state,
        port,
        "http://127.0.0.1:9",
    );
    body
}

#[tokio::test(flavor = "current_thread")]
async fn device_handler_start_admits_ready_binding() {
    let _hooks = HandlerHooks::arm(false);
    let plane = open_armed_plane("admit");
    let before = credential_count(&plane.state);
    let body = arm_ready(&plane, None);
    let started = started_device(&plane).await;
    plane.device.set_stdout(&success_stdout());
    let Json(polled) = oauth_status(
        State(plane.state.clone()),
        Query(OAuthStatusQuery {
            state: started.state.clone(),
            target: None,
        }),
    )
    .await
    .unwrap();
    assert_eq!(polled.status, "ok", "{:?}", polled.error);
    assert_eq!(credential_count(&plane.state), before + 1);
    assert!(model_count(&plane.state) >= 1);
    assert!(applied_route_count(&plane.state) >= 1);
    let granted = credential_count(&plane.state);
    let routes = applied_route_count(&plane.state);
    let port = crate::cpa_execution::execution_report(&plane.state)
        .port
        .expect("applied listen port");
    serve_ready(port, body, None);
    let Json(again) = oauth_status(
        State(plane.state.clone()),
        Query(OAuthStatusQuery {
            state: started.state.clone(),
            target: None,
        }),
    )
    .await
    .unwrap();
    assert_eq!(again.status, "ok", "{:?}", again.error);
    assert_eq!(credential_count(&plane.state), granted);
    assert_eq!(applied_route_count(&plane.state), routes);
    plane.state.bump_settings_revision();
    let Json(stale) = oauth_status(
        State(plane.state.clone()),
        Query(OAuthStatusQuery {
            state: started.state,
            target: None,
        }),
    )
    .await
    .unwrap();
    assert_ne!(stale.status, "ok");
    assert_eq!(credential_count(&plane.state), granted);
    assert_eq!(applied_route_count(&plane.state), routes);
}

#[tokio::test(flavor = "current_thread")]
async fn device_handler_apply_failure_is_not_usable() {
    let _hooks = HandlerHooks::arm(true);
    let plane = open_armed_plane("apply-fail");
    let before = credential_count(&plane.state);
    let _body = arm_ready(&plane, None);
    let started = started_device(&plane).await;
    plane.device.set_stdout(&success_stdout());
    let Json(polled) = oauth_status(
        State(plane.state.clone()),
        Query(OAuthStatusQuery {
            state: started.state.clone(),
            target: None,
        }),
    )
    .await
    .unwrap();
    assert_ne!(polled.status, "ok");
    let count = credential_count(&plane.state);
    assert!(count <= before + 1);
    let Json(again) = oauth_status(
        State(plane.state.clone()),
        Query(OAuthStatusQuery {
            state: started.state,
            target: None,
        }),
    )
    .await
    .unwrap();
    assert_ne!(again.status, "ok");
    assert_eq!(credential_count(&plane.state), count);
}

#[tokio::test(flavor = "current_thread")]
async fn device_handler_ready_move_does_not_widen() {
    let _hooks = HandlerHooks::arm(false);
    let plane = open_armed_plane("ready-move");
    let before = credential_count(&plane.state);
    let state = plane.state.clone();
    let hook = Arc::new(move || {
        crate::cpa_execution::device::test_move_applied_child(&state);
    });
    let _body = arm_ready(&plane, Some(hook));
    let started = started_device(&plane).await;
    plane.device.set_stdout(&success_stdout());
    let Json(polled) = oauth_status(
        State(plane.state.clone()),
        Query(OAuthStatusQuery {
            state: started.state,
            target: None,
        }),
    )
    .await
    .unwrap();
    assert_ne!(polled.status, "ok");
    assert_eq!(credential_count(&plane.state), before);
    assert_eq!(applied_route_count(&plane.state), 0);
}

#[tokio::test(flavor = "current_thread")]
async fn device_handler_deleted_credential_during_ready_fetch_is_not_revived() {
    let _hooks = HandlerHooks::arm(false);
    let plane = open_armed_plane("ready-delete");
    let body = arm_ready(&plane, None);
    let started = started_device(&plane).await;
    plane.device.set_stdout(&success_stdout());
    let Json(polled) = oauth_status(
        State(plane.state.clone()),
        Query(OAuthStatusQuery {
            state: started.state.clone(),
            target: None,
        }),
    )
    .await
    .unwrap();
    assert_eq!(polled.status, "ok", "{:?}", polled.error);
    let granted = credential_count(&plane.state);
    assert!(granted >= 1);
    let port = crate::cpa_execution::execution_report(&plane.state)
        .port
        .expect("applied listen port");
    let state = plane.state.clone();
    let hook = Arc::new(move || {
        let db = state.db.lock();
        db.conn.execute("DELETE FROM credentials", []).unwrap();
    });
    serve_ready(port, body, Some(hook));
    let Json(stale) = oauth_status(
        State(plane.state.clone()),
        Query(OAuthStatusQuery {
            state: started.state,
            target: None,
        }),
    )
    .await
    .unwrap();
    assert_ne!(stale.status, "ok");
    assert!(credential_count(&plane.state) < granted);
}

async fn wait_for_device_ok(state: &CoreState, oauth_state: &str) {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        let ready = state
            .cpa_device_oauth_status(oauth_state)
            .and_then(Result::ok)
            .is_some_and(|status| status.status == "ok");
        if ready {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "device login did not reach ok"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

#[tokio::test(flavor = "current_thread")]
async fn device_handler_delayed_ready_does_not_adopt_a_newer_lease() {
    let _hooks = HandlerHooks::arm(false);
    let plane = open_armed_plane("stale-ready");
    install_listener(&plane.state);
    crate::cpa_execution::device::write_synthetic_auth_placeholder(&plane.state, "codex.json");
    crate::cpa_execution::device::write_synthetic_auth_placeholder(&plane.state, "anthropic.json");
    let codex_id = crate::cpa_execution::device::synthetic_codex_credential_id();
    let anthropic_account =
        crate::db::native_binding::account_id_for("anthropic", "anthropic.json")
            .expect("anthropic account id");
    let anthropic_id =
        ocg_domain::credential::credential_id_for_legacy_account(&anthropic_account).to_string();
    let codex_body = ready_document(&codex_id);
    let entered = Arc::new(AtomicBool::new(false));
    let release = Arc::new(AtomicBool::new(false));
    let stale_port = serve_ready_until(
        0,
        stale_anthropic_document(&anthropic_id),
        entered.clone(),
        release.clone(),
    );
    let _release = ReleaseReady(release.clone());
    crate::cpa_execution::device::prepare_owned_ready_fetch(
        &plane.state,
        stale_port,
        "http://127.0.0.1:9",
    );
    let started = started_device(&plane).await;
    plane.device.set_stdout(&success_stdout());
    wait_for_device_ok(&plane.state, &started.state).await;
    let before_poll = plane
        .state
        .owned_device_completion(&started.state)
        .expect("pre-fetch session");
    assert!(!before_poll.admitted);

    let state_a = plane.state.clone();
    let oauth_a = started.state.clone();
    let first = tokio::spawn(async move {
        oauth_status(
            State(state_a),
            Query(OAuthStatusQuery {
                state: oauth_a,
                target: None,
            }),
        )
        .await
    });
    let entered_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
    while !entered.load(Ordering::SeqCst) {
        assert!(
            tokio::time::Instant::now() < entered_deadline,
            "stale ready request was not received"
        );
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    let fresh_port = serve_ready(0, codex_body.clone(), None);
    crate::cpa_execution::device::prepare_owned_ready_fetch(
        &plane.state,
        fresh_port,
        "http://127.0.0.1:9",
    );
    let Json(winner) = oauth_status(
        State(plane.state.clone()),
        Query(OAuthStatusQuery {
            state: started.state.clone(),
            target: None,
        }),
    )
    .await
    .unwrap();
    assert_eq!(winner.status, "ok", "{:?}", winner.error);
    let granted = credential_count(&plane.state);
    let routes = applied_route_count(&plane.state);
    let applied = crate::cpa_execution::execution_report(&plane.state);
    let codex_versions = credential_versions(&plane.state, &codex_id);
    assert!(codex_versions.is_some());
    assert_eq!(credential_id_count(&plane.state, &anthropic_id), 0);
    let stops_after_winner = plane.device.stops.load(Ordering::SeqCst);
    let admitted = plane
        .state
        .owned_device_completion(&started.state)
        .expect("winner session");
    assert!(admitted.admitted);
    assert!(admitted.applied_revision > before_poll.applied_revision);
    assert_ne!(admitted.applied_digest, before_poll.applied_digest);
    assert_ne!(admitted.lease, before_poll.lease);
    assert!(crate::cpa_execution::device::completion_current(&plane.state, &admitted).is_ok());

    release.store(true, Ordering::SeqCst);
    let Json(stale) = first.await.expect("stale poll task").unwrap();
    assert_eq!(
        stale.error.as_deref(),
        Some("CPA device login completion is no longer current.")
    );
    assert_ne!(stale.status, "ok");
    assert_eq!(credential_count(&plane.state), granted);
    assert_eq!(credential_id_count(&plane.state, &anthropic_id), 0);
    assert_eq!(credential_versions(&plane.state, &codex_id), codex_versions);
    assert_eq!(applied_route_count(&plane.state), routes);
    let after = crate::cpa_execution::execution_report(&plane.state);
    assert_eq!(after.applied_revision, applied.applied_revision);
    assert_eq!(after.child_generation, applied.child_generation);
    assert_eq!(after.apply_status, applied.apply_status);
    assert_eq!(after.applied_digest, applied.applied_digest);
    assert_eq!(
        plane.device.stops.load(Ordering::SeqCst),
        stops_after_winner
    );
    let live = plane
        .state
        .owned_device_completion(&started.state)
        .expect("winner session");
    assert_eq!(live, admitted);
    assert!(crate::cpa_execution::device::completion_current(&plane.state, &live).is_ok());

    let port = after.port.expect("applied listen port");
    serve_ready(port, codex_body, None);
    let Json(again) = oauth_status(
        State(plane.state.clone()),
        Query(OAuthStatusQuery {
            state: started.state.clone(),
            target: None,
        }),
    )
    .await
    .unwrap();
    assert_eq!(again.status, "ok", "{:?}", again.error);
    assert_eq!(again.revision, winner.revision);
    assert_eq!(credential_count(&plane.state), granted);
    assert_eq!(applied_route_count(&plane.state), routes);
    assert_eq!(credential_id_count(&plane.state, &anthropic_id), 0);
    assert_eq!(credential_versions(&plane.state, &codex_id), codex_versions);
    assert_eq!(
        plane.device.stops.load(Ordering::SeqCst),
        stops_after_winner
    );
    let still_admitted = plane
        .state
        .owned_device_completion(&started.state)
        .expect("winner session");
    assert_eq!(still_admitted, admitted);
}

#[tokio::test(flavor = "current_thread")]
async fn device_handler_rejected_current_config_keeps_the_waiting_login() {
    let _hooks = HandlerHooks::arm(false);
    let plane = open_armed_plane("reject-current");
    install_listener(&plane.state);
    let started = started_device(&plane).await;
    let waiting = plane
        .state
        .cpa_device_oauth_status(&started.state)
        .unwrap()
        .unwrap();
    assert_eq!(waiting.status, "wait");
    assert!(plane.device.running.load(Ordering::SeqCst));
    assert_eq!(plane.device.stops.load(Ordering::SeqCst), 0);
    let revision = plane.state.settings_revision();
    let generation = plane.state.process_generation();
    let before = crate::cpa_execution::execution_report(&plane.state);
    let config = plane.state.data_dir.join("cpa").join("config.yaml");
    let previous = plane
        .state
        .data_dir
        .join("cpa")
        .join("config.yaml.previous");
    let yaml = "ocg:\n  process-generation: \"4\"\n  projection-revision: \"2\"\n";
    std::fs::write(&previous, yaml).unwrap();
    std::fs::remove_file(&config).unwrap();
    std::fs::create_dir(&config).unwrap();
    let error = crate::cpa_execution::rollback(&plane.state, revision, generation)
        .await
        .unwrap_err();
    let message = error.to_string();
    assert!(message.contains("regular file"), "{message}");
    assert!(plane.device.running.load(Ordering::SeqCst));
    assert_eq!(plane.device.stops.load(Ordering::SeqCst), 0);
    assert_eq!(plane.state.settings_revision(), revision);
    assert_eq!(plane.state.process_generation(), generation);
    let after = crate::cpa_execution::execution_report(&plane.state);
    assert_eq!(after.applied_revision, before.applied_revision);
    assert_eq!(after.apply_status, before.apply_status);
    assert_eq!(after.child_generation, before.child_generation);
    assert_eq!(after.applied_digest, before.applied_digest);
    assert_eq!(after.desired_revision, before.desired_revision);
    assert!(config.is_dir());
    assert_eq!(std::fs::read(&previous).unwrap(), yaml.as_bytes());
    let still = plane
        .state
        .cpa_device_oauth_status(&started.state)
        .unwrap()
        .unwrap();
    assert_eq!(still.status, "wait");
}

fn assert_device_poll_busy(error: super::super::V3ApiError) {
    assert_eq!(error.body.code, super::super::ERROR_CONFLICT);
    assert_eq!(error.body.message, super::DEVICE_POLL_BUSY);
}

async fn wait_for_poll_pause(pause: &crate::cpa_execution::device::DevicePollPause) {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
    while !pause.entered() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "device poll did not reach the operation pause"
        );
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
}

async fn wait_for_apply_handoff(pause: &crate::cpa_execution::DeviceApplyHandoffPause) {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
    while !pause.entered() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "device poll did not reach the apply handoff"
        );
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
}

fn anthropic_legacy_credential_id() -> String {
    let account = crate::db::native_binding::account_id_for("anthropic", "anthropic.json")
        .expect("anthropic account id");
    ocg_domain::credential::credential_id_for_legacy_account(&account).to_string()
}

#[tokio::test(flavor = "current_thread")]
async fn device_handler_in_progress_poll_is_not_poisoned() {
    let _hooks = HandlerHooks::arm(false);
    let plane = open_armed_plane("poll-in-progress");
    install_listener(&plane.state);
    crate::cpa_execution::device::write_synthetic_auth_placeholder(&plane.state, "codex.json");
    crate::cpa_execution::device::write_synthetic_auth_placeholder(&plane.state, "anthropic.json");
    let codex_id = crate::cpa_execution::device::synthetic_codex_credential_id();
    let anthropic_id = anthropic_legacy_credential_id();
    let body = arm_ready(&plane, None);
    let started = started_device(&plane).await;
    plane.device.set_stdout(&success_stdout());
    wait_for_device_ok(&plane.state, &started.state).await;
    let before = plane
        .state
        .owned_device_completion(&started.state)
        .expect("pre-poll session");
    let before_report = crate::cpa_execution::execution_report(&plane.state);
    let stops = plane.device.stops.load(Ordering::SeqCst);
    let reconcile_pause = crate::cpa_execution::device::arm_device_reconcile_pause();
    let apply_pause = crate::cpa_execution::device::arm_device_apply_pause();
    let state_for_winner = plane.state.clone();
    let oauth_for_winner = started.state.clone();
    let winner = tokio::spawn(async move {
        oauth_status(
            State(state_for_winner),
            Query(OAuthStatusQuery {
                state: oauth_for_winner,
                target: None,
            }),
        )
        .await
    });
    wait_for_poll_pause(&reconcile_pause).await;
    let codex_versions = credential_versions(&plane.state, &codex_id);
    assert!(codex_versions.is_some());
    let routes = applied_route_count(&plane.state);
    let stale_entered = Arc::new(AtomicBool::new(false));
    let stale_release = Arc::new(AtomicBool::new(false));
    let stale_port = serve_ready_until(
        0,
        stale_anthropic_document(&anthropic_id),
        stale_entered.clone(),
        stale_release.clone(),
    );
    let _stale_release = ReleaseReady(stale_release);
    crate::cpa_execution::device::prepare_owned_ready_fetch(
        &plane.state,
        stale_port,
        "http://127.0.0.1:9",
    );
    let busy = oauth_status(
        State(plane.state.clone()),
        Query(OAuthStatusQuery {
            state: started.state.clone(),
            target: None,
        }),
    )
    .await
    .expect_err("competing poll is busy");
    assert_device_poll_busy(busy);
    assert!(!stale_entered.load(Ordering::SeqCst));
    let direct = plane
        .state
        .cpa_device_oauth_status(&started.state)
        .expect("session")
        .expect("shared status");
    assert_eq!(direct.status, "ok");
    assert_eq!(credential_id_count(&plane.state, &anthropic_id), 0);
    assert_eq!(credential_versions(&plane.state, &codex_id), codex_versions);
    assert_eq!(applied_route_count(&plane.state), routes);
    let during = crate::cpa_execution::execution_report(&plane.state);
    assert_eq!(during.applied_revision, before_report.applied_revision);
    assert_eq!(during.child_generation, before_report.child_generation);
    assert_eq!(during.applied_digest, before_report.applied_digest);
    assert_eq!(plane.device.stops.load(Ordering::SeqCst), stops);
    reconcile_pause.release();
    wait_for_poll_pause(&apply_pause).await;
    let busy = oauth_status(
        State(plane.state.clone()),
        Query(OAuthStatusQuery {
            state: started.state.clone(),
            target: None,
        }),
    )
    .await
    .expect_err("competing poll stays busy during apply");
    assert_device_poll_busy(busy);
    assert!(!stale_entered.load(Ordering::SeqCst));
    let applying = crate::cpa_execution::execution_report(&plane.state);
    assert_eq!(applying.apply_status, "apply_pending");
    assert!(!applying.inference_ready);
    assert_eq!(
        applying.phase,
        crate::cpa_runtime::CpaRuntimePhase::Starting
    );
    assert_eq!(applying.applied_revision, before_report.applied_revision);
    assert_eq!(credential_id_count(&plane.state, &anthropic_id), 0);
    assert_eq!(credential_versions(&plane.state, &codex_id), codex_versions);
    assert_eq!(plane.device.stops.load(Ordering::SeqCst), stops);
    let direct = plane
        .state
        .cpa_device_oauth_status(&started.state)
        .expect("session")
        .expect("shared status during apply");
    assert_eq!(direct.status, "ok");
    apply_pause.release();
    let Json(polled) = winner.await.expect("winner task").expect("winner status");
    assert_eq!(polled.status, "ok", "{:?}", polled.error);
    let admitted = plane
        .state
        .owned_device_completion(&started.state)
        .expect("admitted session");
    assert!(admitted.admitted);
    assert_eq!(admitted.child_generation, before.child_generation);
    assert!(admitted.applied_revision > before.applied_revision);
    assert_ne!(admitted.applied_digest, before.applied_digest);
    assert_ne!(admitted.lease, before.lease);
    assert!(crate::cpa_execution::device::completion_current(&plane.state, &admitted).is_ok());
    assert_eq!(credential_id_count(&plane.state, &anthropic_id), 0);
    let admitted_versions = credential_versions(&plane.state, &codex_id);
    assert!(admitted_versions.is_some());
    assert_eq!(plane.device.stops.load(Ordering::SeqCst), stops);
    let granted = credential_count(&plane.state);
    let routes = applied_route_count(&plane.state);
    let applied = crate::cpa_execution::execution_report(&plane.state);
    assert!(applied.applied_revision > before_report.applied_revision);
    assert_eq!(applied.child_generation, before_report.child_generation);
    let port = applied.port.expect("applied listen port");
    serve_ready(port, body, None);
    let Json(again) = oauth_status(
        State(plane.state.clone()),
        Query(OAuthStatusQuery {
            state: started.state.clone(),
            target: None,
        }),
    )
    .await
    .expect("later poll");
    assert_eq!(again.status, "ok", "{:?}", again.error);
    assert_eq!(again.revision, polled.revision);
    assert_eq!(credential_count(&plane.state), granted);
    assert_eq!(applied_route_count(&plane.state), routes);
    assert_eq!(credential_id_count(&plane.state, &anthropic_id), 0);
    assert_eq!(
        credential_versions(&plane.state, &codex_id),
        admitted_versions
    );
    assert_eq!(plane.device.stops.load(Ordering::SeqCst), stops);
    let still = plane
        .state
        .owned_device_completion(&started.state)
        .expect("idempotent session");
    assert_eq!(still, admitted);
}

#[tokio::test(flavor = "current_thread")]
async fn device_handler_ready_error_releases_the_poll_guard() {
    let _hooks = HandlerHooks::arm(false);
    let plane = open_armed_plane("poll-error");
    install_listener(&plane.state);
    let started = started_device(&plane).await;
    plane.device.set_stdout(&success_stdout());
    wait_for_device_ok(&plane.state, &started.state).await;
    let before = crate::cpa_execution::execution_report(&plane.state);
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let closed = listener.local_addr().unwrap().port();
    drop(listener);
    crate::cpa_execution::device::prepare_owned_ready_fetch(
        &plane.state,
        closed,
        "http://127.0.0.1:9",
    );
    let Json(failed) = oauth_status(
        State(plane.state.clone()),
        Query(OAuthStatusQuery {
            state: started.state.clone(),
            target: None,
        }),
    )
    .await
    .expect("closed port is a status body");
    assert_ne!(failed.status, "ok");
    assert_ne!(failed.error.as_deref(), Some(super::DEVICE_POLL_BUSY));
    let after_error = crate::cpa_execution::execution_report(&plane.state);
    assert_eq!(after_error.applied_revision, before.applied_revision);
    assert_eq!(after_error.child_generation, before.child_generation);
    let body = arm_ready(&plane, None);
    let second = oauth_status(
        State(plane.state.clone()),
        Query(OAuthStatusQuery {
            state: started.state.clone(),
            target: None,
        }),
    )
    .await;
    let Json(polled) = second.expect("later poll acquires the released guard");
    assert_eq!(polled.status, "ok", "{:?}", polled.error);
    let port = crate::cpa_execution::execution_report(&plane.state)
        .port
        .expect("applied listen port");
    serve_ready(port, body, None);
    let Json(again) = oauth_status(
        State(plane.state.clone()),
        Query(OAuthStatusQuery {
            state: started.state,
            target: None,
        }),
    )
    .await
    .expect("idempotent poll");
    assert_eq!(again.status, "ok", "{:?}", again.error);
}

#[tokio::test(flavor = "current_thread")]
async fn device_handler_cancel_during_reconcile_writes_nothing() {
    // Post-commit window. The pause runs after reconcile_owned_discovery has
    // committed the codex ready body. The assertions below are about no later
    // note or apply. The pre-write case is
    // device_handler_cancel_before_ready_write_admits_nothing.
    let _hooks = HandlerHooks::arm(false);
    let plane = open_armed_plane("poll-cancel");
    install_listener(&plane.state);
    crate::cpa_execution::device::write_synthetic_auth_placeholder(&plane.state, "anthropic.json");
    let codex_id = crate::cpa_execution::device::synthetic_codex_credential_id();
    let anthropic_id = anthropic_legacy_credential_id();
    let _body = arm_ready(&plane, None);
    let started = started_device(&plane).await;
    plane.device.set_stdout(&success_stdout());
    wait_for_device_ok(&plane.state, &started.state).await;
    let before_report = crate::cpa_execution::execution_report(&plane.state);
    let settings = plane.state.settings_revision();
    let stops = plane.device.stops.load(Ordering::SeqCst);
    let reconcile_pause = crate::cpa_execution::device::arm_device_reconcile_pause();
    let state_for_winner = plane.state.clone();
    let oauth_for_winner = started.state.clone();
    let winner = tokio::spawn(async move {
        oauth_status(
            State(state_for_winner),
            Query(OAuthStatusQuery {
                state: oauth_for_winner,
                target: None,
            }),
        )
        .await
    });
    wait_for_poll_pause(&reconcile_pause).await;
    let codex_versions = credential_versions(&plane.state, &codex_id);
    assert!(codex_versions.is_some());
    let stale_entered = Arc::new(AtomicBool::new(false));
    let stale_release = Arc::new(AtomicBool::new(false));
    let stale_port = serve_ready_until(
        0,
        stale_anthropic_document(&anthropic_id),
        stale_entered.clone(),
        Arc::clone(&stale_release),
    );
    let _stale_release = ReleaseReady(stale_release);
    crate::cpa_execution::device::prepare_owned_ready_fetch(
        &plane.state,
        stale_port,
        "http://127.0.0.1:9",
    );
    let cancelled = plane
        .state
        .cancel_cpa_device_oauth(&started.state)
        .expect("session")
        .expect("cancel");
    // Direct session cancel claims the in-flight epoch and returns true.
    // It is not cancel_oauth, so the settings revision stays put.
    assert!(cancelled);
    let busy = oauth_status(
        State(plane.state.clone()),
        Query(OAuthStatusQuery {
            state: started.state.clone(),
            target: None,
        }),
    )
    .await
    .expect_err("in-flight cancel is still the owner's poll");
    assert_device_poll_busy(busy);
    assert!(!stale_entered.load(Ordering::SeqCst));
    let direct = plane
        .state
        .cpa_device_oauth_status(&started.state)
        .expect("session")
        .expect("shared status");
    assert_eq!(direct.status, "ok");
    assert_eq!(credential_id_count(&plane.state, &anthropic_id), 0);
    assert_eq!(plane.state.settings_revision(), settings);
    assert_eq!(plane.device.stops.load(Ordering::SeqCst), stops);
    let during = crate::cpa_execution::execution_report(&plane.state);
    assert_eq!(during.applied_revision, before_report.applied_revision);
    assert_eq!(during.apply_status, before_report.apply_status);
    reconcile_pause.release();
    let Json(polled) = winner.await.expect("winner task").expect("winner status");
    assert_ne!(polled.status, "ok");
    assert_eq!(
        polled.error.as_deref(),
        Some("CPA device login completion is no longer current.")
    );
    assert_eq!(credential_id_count(&plane.state, &anthropic_id), 0);
    assert_eq!(credential_versions(&plane.state, &codex_id), codex_versions);
    assert_eq!(plane.state.settings_revision(), settings);
    assert_eq!(plane.device.stops.load(Ordering::SeqCst), stops);
    let after = crate::cpa_execution::execution_report(&plane.state);
    assert_eq!(after.applied_revision, before_report.applied_revision);
    assert_eq!(after.applied_digest, before_report.applied_digest);
    assert_eq!(after.child_generation, before_report.child_generation);
    assert_eq!(after.apply_status, before_report.apply_status);
    let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let closed_port = closed.local_addr().unwrap().port();
    drop(closed);
    crate::cpa_execution::device::prepare_owned_ready_fetch(
        &plane.state,
        closed_port,
        "http://127.0.0.1:9",
    );
    let follow = oauth_status(
        State(plane.state.clone()),
        Query(OAuthStatusQuery {
            state: started.state,
            target: None,
        }),
    )
    .await;
    match follow {
        Ok(Json(body)) => {
            assert_ne!(body.status, "ok");
            assert_ne!(body.error.as_deref(), Some(super::DEVICE_POLL_BUSY));
        }
        Err(error) => assert_ne!(error.body.message, super::DEVICE_POLL_BUSY),
    }
}

fn ready_document_with_new_ref(codex_id: &str, anthropic_id: &str) -> String {
    let mut document: serde_json::Value =
        serde_json::from_str(&ready_document(codex_id)).expect("codex ready document");
    let extra: serde_json::Value = serde_json::from_str(&stale_anthropic_document(anthropic_id))
        .expect("anthropic ready document");
    document["authRefs"]
        .as_array_mut()
        .expect("auth refs")
        .push(extra["authRefs"][0].clone());
    document.to_string()
}

fn cancel_session_body(state: &CoreState, oauth_state: &str, revision: u64) -> Bytes {
    Bytes::from(
        serde_json::to_vec(&json!({
            "state": oauth_state,
            "expectedRevision": revision,
            "processGeneration": state.process_generation(),
        }))
        .unwrap(),
    )
}

struct PrewriteFacts {
    credentials: i64,
    grants: i64,
    models: i64,
    routes: usize,
    anthropic: i64,
    codex: Option<(i64, i64)>,
    catalog: Option<String>,
    projection: String,
    desired_revision: u64,
    applied_revision: u64,
    desired_digest: String,
    applied_digest: String,
    child_generation: u64,
    apply_status: String,
    completion: crate::cpa_execution::device::DeviceCompletion,
    starts: usize,
    stops: usize,
}

fn prewrite_facts(
    plane: &ArmedPlane,
    oauth_state: &str,
    codex_id: &str,
    anthropic_id: &str,
) -> PrewriteFacts {
    let report = crate::cpa_execution::execution_report(&plane.state);
    let db = plane.state.db.lock();
    let grants: i64 = db
        .conn
        .query_row("SELECT COUNT(*) FROM credential_grants", [], |row| {
            row.get(0)
        })
        .unwrap();
    let models: i64 = db
        .conn
        .query_row("SELECT COUNT(*) FROM destination_models", [], |row| {
            row.get(0)
        })
        .unwrap();
    let catalog: Option<String> = db
        .conn
        .query_row(
            "SELECT models_json FROM provider_model_catalogs WHERE provider_id = ?1",
            [crate::provider::CPA_PROVIDER_ID],
            |row| row.get(0),
        )
        .ok();
    let projection: String = db
        .conn
        .query_row(
            "SELECT value FROM settings WHERE key = 'cpa_execution_projection_v1'",
            [],
            |row| row.get(0),
        )
        .unwrap_or_default();
    drop(db);
    PrewriteFacts {
        credentials: credential_count(&plane.state),
        grants,
        models,
        routes: applied_route_count(&plane.state),
        anthropic: credential_id_count(&plane.state, anthropic_id),
        codex: credential_versions(&plane.state, codex_id),
        catalog,
        projection,
        desired_revision: report.desired_revision,
        applied_revision: report.applied_revision,
        desired_digest: report.desired_digest,
        applied_digest: report.applied_digest,
        child_generation: report.child_generation,
        apply_status: report.apply_status,
        completion: plane
            .state
            .owned_device_completion(oauth_state)
            .expect("device completion"),
        starts: plane.device.starts.load(Ordering::SeqCst),
        stops: plane.device.stops.load(Ordering::SeqCst),
    }
}

fn assert_same_prewrite_facts(live: &PrewriteFacts, before: &PrewriteFacts) {
    assert_eq!(live.credentials, before.credentials);
    assert_eq!(live.grants, before.grants);
    assert_eq!(live.models, before.models);
    assert_eq!(live.routes, before.routes);
    assert_eq!(live.anthropic, before.anthropic);
    assert_eq!(live.codex, before.codex);
    assert_eq!(live.catalog, before.catalog);
    assert_eq!(live.projection, before.projection);
    assert_eq!(live.desired_revision, before.desired_revision);
    assert_eq!(live.applied_revision, before.applied_revision);
    assert_eq!(live.desired_digest, before.desired_digest);
    assert_eq!(live.applied_digest, before.applied_digest);
    assert_eq!(live.child_generation, before.child_generation);
    assert_eq!(live.apply_status, before.apply_status);
    assert_eq!(live.completion, before.completion);
    assert_eq!(live.starts, before.starts);
    assert_eq!(live.stops, before.stops);
}

#[tokio::test(flavor = "current_thread")]
async fn device_handler_cancel_before_ready_write_admits_nothing() {
    // Pre-write window. The ready request is held until cancel_oauth's CAS
    // ack returns. device_handler_cancel_during_reconcile_writes_nothing is
    // the later pause, after the codex reconcile has already committed.
    let _hooks = HandlerHooks::arm(false);
    let plane = open_armed_plane("prewrite-cancel");
    install_listener(&plane.state);
    crate::cpa_execution::device::write_synthetic_auth_placeholder(&plane.state, "codex.json");
    crate::cpa_execution::device::write_synthetic_auth_placeholder(&plane.state, "anthropic.json");
    let codex_id = crate::cpa_execution::device::synthetic_codex_credential_id();
    let anthropic_id = anthropic_legacy_credential_id();
    let body = ready_document_with_new_ref(&codex_id, &anthropic_id);
    assert!(body.contains("anthropic.json"));
    let entered = Arc::new(AtomicBool::new(false));
    let release = Arc::new(AtomicBool::new(false));
    let port = serve_ready_until(0, body, entered.clone(), release.clone());
    let _held = ReleaseReady(release.clone());
    crate::cpa_execution::device::prepare_owned_ready_fetch(
        &plane.state,
        port,
        "http://127.0.0.1:9",
    );
    let started = started_device(&plane).await;
    plane.device.set_stdout(&success_stdout());
    wait_for_device_ok(&plane.state, &started.state).await;
    let before = prewrite_facts(&plane, &started.state, &codex_id, &anthropic_id);
    assert_eq!(before.anthropic, 0);
    assert!(before.codex.is_none());
    let settings = plane.state.settings_revision();
    let state_for_owner = plane.state.clone();
    let oauth_for_owner = started.state.clone();
    let owner = tokio::spawn(async move {
        oauth_status(
            State(state_for_owner),
            Query(OAuthStatusQuery {
                state: oauth_for_owner,
                target: None,
            }),
        )
        .await
    });
    let entered_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
    while !entered.load(Ordering::SeqCst) {
        assert!(
            tokio::time::Instant::now() < entered_deadline,
            "ready request was not received before cancel"
        );
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    let stale = cancel_oauth(
        State(plane.state.clone()),
        cancel_session_body(&plane.state, &started.state, settings.wrapping_sub(1)),
    )
    .await
    .expect_err("stale CAS");
    assert_eq!(stale.body.code, super::super::ERROR_REVISION_CONFLICT);
    assert_eq!(plane.state.settings_revision(), settings);
    assert_same_prewrite_facts(
        &prewrite_facts(&plane, &started.state, &codex_id, &anthropic_id),
        &before,
    );
    let Json(ack) = cancel_oauth(
        State(plane.state.clone()),
        cancel_session_body(&plane.state, &started.state, settings),
    )
    .await
    .expect("cancel ack");
    assert_eq!(ack.revision, settings + 1);
    assert_eq!(ack.process_generation, plane.state.process_generation());
    assert_eq!(plane.state.settings_revision(), ack.revision);
    assert_same_prewrite_facts(
        &prewrite_facts(&plane, &started.state, &codex_id, &anthropic_id),
        &before,
    );
    let Json(repeated) = cancel_oauth(
        State(plane.state.clone()),
        cancel_session_body(&plane.state, &started.state, ack.revision),
    )
    .await
    .expect("repeated cancel");
    assert_eq!(repeated.revision, ack.revision);
    assert_eq!(plane.state.settings_revision(), ack.revision);
    assert_same_prewrite_facts(
        &prewrite_facts(&plane, &started.state, &codex_id, &anthropic_id),
        &before,
    );
    release.store(true, Ordering::SeqCst);
    let Json(polled) = owner.await.expect("owner task").expect("owner status");
    assert_ne!(polled.status, "ok");
    assert_eq!(
        polled.error.as_deref(),
        Some("CPA device login completion is no longer current.")
    );
    assert_eq!(polled.revision, ack.revision);
    let after_owner = prewrite_facts(&plane, &started.state, &codex_id, &anthropic_id);
    assert_same_prewrite_facts(&after_owner, &before);
    assert_eq!(after_owner.starts, before.starts);
    assert_eq!(after_owner.stops, before.stops);
    let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let closed_port = closed.local_addr().unwrap().port();
    drop(closed);
    crate::cpa_execution::device::prepare_owned_ready_fetch(
        &plane.state,
        closed_port,
        "http://127.0.0.1:9",
    );
    let follow = oauth_status(
        State(plane.state.clone()),
        Query(OAuthStatusQuery {
            state: started.state.clone(),
            target: None,
        }),
    )
    .await;
    match follow {
        Ok(Json(body)) => {
            assert_ne!(body.status, "ok");
            assert_ne!(body.error.as_deref(), Some(super::DEVICE_POLL_BUSY));
        }
        Err(error) => assert_ne!(error.body.message, super::DEVICE_POLL_BUSY),
    }
    let after_follow = prewrite_facts(&plane, &started.state, &codex_id, &anthropic_id);
    assert_eq!(after_follow.credentials, before.credentials);
    assert_eq!(after_follow.grants, before.grants);
    assert_eq!(after_follow.models, before.models);
    assert_eq!(after_follow.routes, before.routes);
    assert_eq!(after_follow.anthropic, before.anthropic);
    assert_eq!(after_follow.codex, before.codex);
    assert_eq!(after_follow.catalog, before.catalog);
    assert_eq!(after_follow.projection, before.projection);
    assert_eq!(after_follow.desired_revision, before.desired_revision);
    assert_eq!(after_follow.applied_revision, before.applied_revision);
    assert_eq!(after_follow.desired_digest, before.desired_digest);
    assert_eq!(after_follow.applied_digest, before.applied_digest);
    assert_eq!(after_follow.child_generation, before.child_generation);
    assert_eq!(after_follow.apply_status, before.apply_status);
    assert_eq!(after_follow.completion, before.completion);
    assert_eq!(after_follow.starts, before.starts);
}

#[tokio::test(flavor = "current_thread")]
async fn device_handler_cancel_at_apply_handoff_writes_nothing() {
    // After the owner notes, and after the outside guard.cancelled() check.
    // The pause is the first step of schedule_owned_device_apply, before that
    // helper takes cpa_operations. cancel_oauth can win the mutex there.
    // device_handler_cancel_during_reconcile_writes_nothing is the earlier
    // pause and still calls cancel_cpa_device_oauth directly.
    let _hooks = HandlerHooks::arm(false);
    let plane = open_armed_plane("apply-handoff-cancel");
    crate::cpa_execution::set_artifact_dir(
        &plane.state,
        crate::cpa_execution::documented_runtime_dir(),
    );
    crate::cpa_execution::device::write_synthetic_auth_placeholder(&plane.state, "anthropic.json");
    let codex_id = crate::cpa_execution::device::synthetic_codex_credential_id();
    let anthropic_id = anthropic_legacy_credential_id();
    let _body = arm_ready(&plane, None);
    let started = started_device(&plane).await;
    plane.device.set_stdout(&success_stdout());
    wait_for_device_ok(&plane.state, &started.state).await;
    let handoff = crate::cpa_execution::arm_device_apply_handoff_pause();
    let state_for_owner = plane.state.clone();
    let oauth_for_owner = started.state.clone();
    let owner = tokio::spawn(async move {
        oauth_status(
            State(state_for_owner),
            Query(OAuthStatusQuery {
                state: oauth_for_owner,
                target: None,
            }),
        )
        .await
    });
    wait_for_apply_handoff(&handoff).await;
    let noted = prewrite_facts(&plane, &started.state, &codex_id, &anthropic_id);
    assert!(noted.codex.is_some(), "reconcile commit is already visible");
    assert_eq!(noted.anthropic, 0);
    assert_ne!(noted.apply_status, "apply_pending");
    let settings = plane.state.settings_revision();
    let stale = cancel_oauth(
        State(plane.state.clone()),
        cancel_session_body(&plane.state, &started.state, settings.wrapping_sub(1)),
    )
    .await
    .expect_err("stale CAS");
    assert_eq!(stale.body.code, super::super::ERROR_REVISION_CONFLICT);
    assert_eq!(plane.state.settings_revision(), settings);
    assert_same_prewrite_facts(
        &prewrite_facts(&plane, &started.state, &codex_id, &anthropic_id),
        &noted,
    );
    let Json(ack) = cancel_oauth(
        State(plane.state.clone()),
        cancel_session_body(&plane.state, &started.state, settings),
    )
    .await
    .expect("cancel ack");
    assert_eq!(ack.revision, settings + 1);
    assert_eq!(ack.process_generation, plane.state.process_generation());
    assert_eq!(plane.state.settings_revision(), ack.revision);
    assert_same_prewrite_facts(
        &prewrite_facts(&plane, &started.state, &codex_id, &anthropic_id),
        &noted,
    );
    let direct = plane
        .state
        .cpa_device_oauth_status(&started.state)
        .expect("session")
        .expect("in-flight status");
    assert_eq!(direct.status, "ok");
    let Json(repeated) = cancel_oauth(
        State(plane.state.clone()),
        cancel_session_body(&plane.state, &started.state, ack.revision),
    )
    .await
    .expect("repeated cancel");
    assert_eq!(repeated.revision, ack.revision);
    assert_eq!(plane.state.settings_revision(), ack.revision);
    assert_same_prewrite_facts(
        &prewrite_facts(&plane, &started.state, &codex_id, &anthropic_id),
        &noted,
    );
    handoff.release();
    let Json(polled) = owner.await.expect("owner task").expect("owner status");
    assert_ne!(polled.status, "ok");
    assert_eq!(
        polled.error.as_deref(),
        Some("CPA device login completion is no longer current.")
    );
    assert_eq!(polled.revision, ack.revision);
    let after_owner = prewrite_facts(&plane, &started.state, &codex_id, &anthropic_id);
    assert_same_prewrite_facts(&after_owner, &noted);
    assert_eq!(plane.state.settings_revision(), ack.revision);
    let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let closed_port = closed.local_addr().unwrap().port();
    drop(closed);
    crate::cpa_execution::device::prepare_owned_ready_fetch(
        &plane.state,
        closed_port,
        "http://127.0.0.1:9",
    );
    let follow = oauth_status(
        State(plane.state.clone()),
        Query(OAuthStatusQuery {
            state: started.state.clone(),
            target: None,
        }),
    )
    .await;
    match follow {
        Ok(Json(body)) => {
            assert_ne!(body.status, "ok");
            assert_ne!(body.error.as_deref(), Some(super::DEVICE_POLL_BUSY));
        }
        Err(error) => assert_ne!(error.body.message, super::DEVICE_POLL_BUSY),
    }
    let after_follow = prewrite_facts(&plane, &started.state, &codex_id, &anthropic_id);
    assert_eq!(after_follow.codex, noted.codex);
    assert_eq!(after_follow.anthropic, 0);
    assert_eq!(after_follow.routes, noted.routes);
    assert_eq!(after_follow.desired_revision, noted.desired_revision);
    assert_eq!(after_follow.applied_revision, noted.applied_revision);
    assert_eq!(after_follow.desired_digest, noted.desired_digest);
    assert_eq!(after_follow.applied_digest, noted.applied_digest);
    assert_eq!(after_follow.child_generation, noted.child_generation);
    assert_eq!(after_follow.apply_status, noted.apply_status);
    assert_eq!(plane.state.settings_revision(), ack.revision);
}

#[tokio::test(flavor = "current_thread")]
async fn device_handler_uncancelled_apply_handoff_applies() {
    let _hooks = HandlerHooks::arm(false);
    let plane = open_armed_plane("apply-handoff-ok");
    crate::cpa_execution::set_artifact_dir(
        &plane.state,
        crate::cpa_execution::documented_runtime_dir(),
    );
    let codex_id = crate::cpa_execution::device::synthetic_codex_credential_id();
    let body = arm_ready(&plane, None);
    let started = started_device(&plane).await;
    plane.device.set_stdout(&success_stdout());
    wait_for_device_ok(&plane.state, &started.state).await;
    let before = plane
        .state
        .owned_device_completion(&started.state)
        .expect("pre-poll session");
    let before_report = crate::cpa_execution::execution_report(&plane.state);
    let stops = plane.device.stops.load(Ordering::SeqCst);
    let handoff = crate::cpa_execution::arm_device_apply_handoff_pause();
    let state_for_owner = plane.state.clone();
    let oauth_for_owner = started.state.clone();
    let owner = tokio::spawn(async move {
        oauth_status(
            State(state_for_owner),
            Query(OAuthStatusQuery {
                state: oauth_for_owner,
                target: None,
            }),
        )
        .await
    });
    wait_for_apply_handoff(&handoff).await;
    let at_handoff = plane.state.settings_revision();
    let during = crate::cpa_execution::execution_report(&plane.state);
    assert_eq!(during.applied_revision, before_report.applied_revision);
    assert_eq!(during.applied_digest, before_report.applied_digest);
    assert_eq!(during.child_generation, before_report.child_generation);
    assert_ne!(during.apply_status, "apply_pending");
    assert_eq!(plane.device.stops.load(Ordering::SeqCst), stops);
    handoff.release();
    let Json(polled) = owner.await.expect("owner task").expect("owner status");
    assert_eq!(polled.status, "ok", "{:?}", polled.error);
    assert_eq!(polled.revision, at_handoff);
    assert_eq!(plane.state.settings_revision(), at_handoff);
    let admitted = plane
        .state
        .owned_device_completion(&started.state)
        .expect("admitted session");
    assert!(admitted.admitted);
    assert_eq!(admitted.child_generation, before.child_generation);
    assert!(admitted.applied_revision > before.applied_revision);
    assert_ne!(admitted.applied_digest, before.applied_digest);
    assert!(crate::cpa_execution::device::completion_current(&plane.state, &admitted).is_ok());
    let applied = crate::cpa_execution::execution_report(&plane.state);
    assert_eq!(applied.apply_status, "applied");
    assert!(applied.applied_revision > before_report.applied_revision);
    assert_eq!(applied.child_generation, before_report.child_generation);
    assert!(credential_id_count(&plane.state, &codex_id) > 0);
    assert_eq!(plane.device.stops.load(Ordering::SeqCst), stops);
    let port = applied.port.expect("applied listen port");
    serve_ready(port, body, None);
    let Json(again) = oauth_status(
        State(plane.state.clone()),
        Query(OAuthStatusQuery {
            state: started.state,
            target: None,
        }),
    )
    .await
    .expect("later poll");
    assert_eq!(again.status, "ok", "{:?}", again.error);
    assert_ne!(again.error.as_deref(), Some(super::DEVICE_POLL_BUSY));
    assert_eq!(again.revision, at_handoff);
    assert_eq!(plane.state.settings_revision(), at_handoff);
}

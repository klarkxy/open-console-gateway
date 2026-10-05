//! Synthetic device-helper evidence.
//!
//! These tests drive a stand-in executable and an injected host. They are not
//! an accepted CPA authenticator completion and not a provider proof.

use super::*;
use crate::cpa_runtime::{
    CpaRuntimeHost, CpaRuntimeLogTail, CpaRuntimeProcessHost, CpaRuntimeProcessSpec,
    CpaRuntimeSecret,
};
use crate::crypto::StaticKeyCipher;
use crate::db::Database;
use crate::state::{CoreState, CoreStateInner};
use parking_lot::Mutex;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use zeroize::Zeroize;

const DEVICE_URL: &str = "https://auth.openai.com/codex/device";

struct TempDir(PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct GatewayHost {
    running: AtomicBool,
    starts: AtomicUsize,
    stops: AtomicUsize,
}

impl GatewayHost {
    fn running() -> Arc<Self> {
        Arc::new(Self {
            running: AtomicBool::new(true),
            starts: AtomicUsize::new(0),
            stops: AtomicUsize::new(0),
        })
    }
}

impl CpaRuntimeProcessHost for GatewayHost {
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
            stdout: String::new(),
            stderr: String::new(),
        }
    }
    fn add_log_secret(&self, _: &CpaRuntimeSecret) {}
}

struct RecordingHost {
    running: AtomicBool,
    starts: AtomicUsize,
    stops: AtomicUsize,
    stdout: Mutex<String>,
    executable: Mutex<Option<PathBuf>>,
    config_path: Mutex<Option<PathBuf>>,
    working_dir: Mutex<Option<PathBuf>>,
    device_login: AtomicBool,
    password_covered: AtomicBool,
    bump_on_start: Mutex<Option<CoreState>>,
}

impl RecordingHost {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            running: AtomicBool::new(false),
            starts: AtomicUsize::new(0),
            stops: AtomicUsize::new(0),
            stdout: Mutex::new(String::new()),
            executable: Mutex::new(None),
            config_path: Mutex::new(None),
            working_dir: Mutex::new(None),
            device_login: AtomicBool::new(false),
            password_covered: AtomicBool::new(false),
            bump_on_start: Mutex::new(None),
        })
    }

    fn set_stdout(&self, text: &str) {
        *self.stdout.lock() = text.to_string();
    }
}

impl CpaRuntimeProcessHost for RecordingHost {
    fn start_owned(
        &self,
        spec: &CpaRuntimeProcessSpec,
    ) -> Result<(), crate::cpa_runtime::CpaRuntimeError> {
        let password = spec.management_password.expose_to_host();
        let covered = !password.is_empty()
            && spec
                .log_secrets
                .iter()
                .any(|secret| secret.expose_to_host() == password);
        self.password_covered.store(covered, Ordering::SeqCst);
        self.device_login
            .store(spec.codex_device_login, Ordering::SeqCst);
        *self.executable.lock() = Some(spec.executable.clone());
        *self.config_path.lock() = Some(spec.config_path.clone());
        *self.working_dir.lock() = Some(spec.working_dir.clone());
        self.running.store(true, Ordering::SeqCst);
        self.starts.fetch_add(1, Ordering::SeqCst);
        if let Some(state) = self.bump_on_start.lock().clone() {
            state.bump_settings_revision();
        }
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
            stdout: self.stdout.lock().clone(),
            stderr: String::new(),
        }
    }
    fn add_log_secret(&self, _: &CpaRuntimeSecret) {}
}

struct Harness {
    _dir: TempDir,
    state: CoreState,
    gateway: Arc<GatewayHost>,
    device: Arc<RecordingHost>,
}

fn harness(name: &str) -> Harness {
    let dir =
        std::env::temp_dir().join(format!("ocg-device-owned-{name}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let state = Arc::new(
        CoreStateInner::new(
            Database::open(dir.clone()).unwrap(),
            dir.clone(),
            Arc::new(StaticKeyCipher::new("device-owned-test")),
        )
        .unwrap(),
    );
    let gateway = GatewayHost::running();
    let registered: CpaRuntimeHost = gateway.clone();
    state.set_cpa_runtime_host(registered);
    arm_synthetic_owned_plane(&state);
    Harness {
        _dir: TempDir(dir),
        state,
        gateway,
        device: RecordingHost::new(),
    }
}

fn as_device_host(host: &Arc<RecordingHost>) -> CpaRuntimeHost {
    let host: CpaRuntimeHost = host.clone();
    host
}

fn assert_redacted(rendered: &str, secret: &str) {
    if secret.len() < 8 || secret == "[redacted]" {
        return;
    }
    assert!(
        !rendered.contains(secret),
        "management or log material leaked into a device result"
    );
}

fn credential_count(state: &CoreState) -> i64 {
    state
        .db
        .lock()
        .conn
        .query_row("SELECT COUNT(*) FROM credentials", [], |row| row.get(0))
        .unwrap()
}

fn credential_legacy_ids(state: &CoreState) -> Vec<String> {
    let db = state.db.lock();
    let mut statement = db
        .conn
        .prepare("SELECT legacy_account_id FROM credentials ORDER BY legacy_account_id")
        .unwrap();
    statement
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<std::result::Result<Vec<String>, rusqlite::Error>>()
        .unwrap()
}

fn prompt_stdout() -> String {
    format!("Codex device URL: {DEVICE_URL}\nCodex device code: ABCD-1234\n")
}

fn success_stdout() -> String {
    "Authentication saved to private-path\nCodex device authentication successful!\n".into()
}

#[tokio::test]
async fn simulated_helper_owned_plane_prompts_without_singleton() {
    let harness = harness("prompt");
    assert!(
        harness
            .state
            .db
            .lock()
            .get_account(crate::provider::CPA_ACCOUNT_ID)
            .unwrap()
            .is_none()
    );
    let bare_dir = std::env::temp_dir().join(format!("ocg-device-bare-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&bare_dir).unwrap();
    let _bare_cleanup = TempDir(bare_dir.clone());
    let bare = Arc::new(
        CoreStateInner::new(
            Database::open(bare_dir.clone()).unwrap(),
            bare_dir,
            Arc::new(StaticKeyCipher::new("device-owned-bare")),
        )
        .unwrap(),
    );
    let bare_gateway: CpaRuntimeHost = GatewayHost::running();
    bare.set_cpa_runtime_host(bare_gateway);
    let bare_error = owned_device_launch(&bare).unwrap_err().to_string();
    assert!(
        !bare_error.contains("CPA singleton account is missing"),
        "unarmed plane must not ask for the reserved singleton"
    );

    let launch = owned_device_launch(&harness.state).unwrap();
    let mut password = launch.management_password.clone();
    let executable = launch.executable.clone();
    let config_path = launch.config_path.clone();
    let working_dir = launch.working_dir.clone();
    let rendered = format!("{launch:?}");
    assert!(rendered.contains("[redacted]"));
    assert_redacted(&rendered, &password);
    assert!(launch.log_secrets.iter().any(|secret| secret == &password));
    for secret in &launch.log_secrets {
        assert_redacted(&rendered, secret);
    }
    drop(launch);

    harness.device.set_stdout(&prompt_stdout());
    let started = CoreStateInner::with_test_device_host(
        as_device_host(&harness.device),
        harness.state.start_cpa_device_oauth(),
    )
    .await
    .unwrap();
    assert_eq!(started.user_code.as_deref(), Some("ABCD-1234"));
    assert_eq!(started.url, DEVICE_URL);
    assert_redacted(&started.user_code.clone().unwrap_or_default(), &password);
    assert!(harness.device.device_login.load(Ordering::SeqCst));
    assert!(harness.device.password_covered.load(Ordering::SeqCst));
    assert_eq!(harness.device.executable.lock().as_ref(), Some(&executable));
    assert_eq!(
        harness.device.config_path.lock().as_ref(),
        Some(&config_path)
    );
    assert_eq!(
        harness.device.working_dir.lock().as_ref(),
        Some(&working_dir)
    );
    let stored = credential_legacy_ids(&harness.state);
    assert_eq!(
        stored,
        vec![crate::provider::ZEN_FREE_ACCOUNT_ID.to_string()],
        "device prompt stored a credential besides the database-owned Zen row: {stored:?}"
    );
    assert!(
        harness
            .state
            .db
            .lock()
            .get_account(crate::provider::CPA_ACCOUNT_ID)
            .unwrap()
            .is_none()
    );
    password.zeroize();
    drop(harness.state);
}

#[tokio::test]
async fn simulated_helper_stale_prompt_cancels_before_code() {
    let harness = harness("cas");
    *harness.device.bump_on_start.lock() = Some(harness.state.clone());
    let error = CoreStateInner::with_test_device_host(
        as_device_host(&harness.device),
        harness.state.start_cpa_device_oauth(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.to_string(), "revisionConflict");
    assert_eq!(harness.device.starts.load(Ordering::SeqCst), 1);
    assert!(harness.device.stops.load(Ordering::SeqCst) >= 1);
    assert!(!harness.device.running.load(Ordering::SeqCst));
    drop(harness.state);
}

#[tokio::test]
async fn simulated_helper_stale_completion_does_not_grant_authority() {
    let harness = harness("stale");
    let before = credential_count(&harness.state);
    harness.device.set_stdout(&prompt_stdout());
    let started = CoreStateInner::with_test_device_host(
        as_device_host(&harness.device),
        harness.state.start_cpa_device_oauth(),
    )
    .await
    .unwrap();
    harness.device.set_stdout(&success_stdout());
    let current = harness
        .state
        .cpa_device_oauth_status(&started.state)
        .unwrap()
        .unwrap();
    assert_eq!(current.status, "ok");
    assert_eq!(credential_count(&harness.state), before);
    test_move_applied_child(&harness.state);
    let stale = harness
        .state
        .cpa_device_oauth_status(&started.state)
        .unwrap()
        .unwrap();
    assert_ne!(stale.status, "ok");
    assert_eq!(credential_count(&harness.state), before);
    let rendered = format!("{stale:?}");
    assert!(!rendered.contains("private-path"));
    drop(harness.state);
}

#[tokio::test]
async fn simulated_helper_refuses_foreign_auth_and_missing_executable() {
    let foreign_harness = harness("foreign");
    let config = super::super::io::config_path(&foreign_harness.state.data_dir);
    super::super::io::atomic_write_private(&config, b"auth-dir: \"/not/the/owned/auth\"\n")
        .unwrap();
    let foreign = owned_device_launch(&foreign_harness.state)
        .unwrap_err()
        .to_string();
    assert!(foreign.contains("auth directory"));
    assert!(!foreign.contains("CPA singleton account is missing"));
    let started = CoreStateInner::with_test_device_host(
        as_device_host(&foreign_harness.device),
        foreign_harness.state.start_cpa_device_oauth(),
    )
    .await;
    assert!(started.is_err());
    assert_eq!(foreign_harness.device.starts.load(Ordering::SeqCst), 0);

    let missing = harness("missing-exe");
    let sha = super::super::artifact::selected_trusted_sha().expect("selected digest");
    assert_ne!(sha, super::super::artifact::PINNED_SHA256);
    let executable = super::super::io::version_dir(&missing.state.data_dir, &sha)
        .join(super::super::io::executable_name());
    assert!(
        executable.is_file(),
        "selected executable was not installed"
    );
    std::fs::remove_file(&executable).unwrap();
    let error = owned_device_launch(&missing.state).unwrap_err().to_string();
    assert!(error.contains("not installed"));
    assert!(!error.contains("CPA singleton account is missing"));
    let started = CoreStateInner::with_test_device_host(
        as_device_host(&missing.device),
        missing.state.start_cpa_device_oauth(),
    )
    .await;
    assert!(started.is_err());
    assert_eq!(missing.device.starts.load(Ordering::SeqCst), 0);
    drop(foreign_harness.state);
    drop(missing.state);
}

#[test]
fn simulated_helper_matching_owned_origin_keeps_saved_row_bytes() {
    let harness = harness("origin");
    let (inference, management) = save_historical(&harness.state, "http://127.0.0.1:9/");
    let before = snapshot_historical(&harness.state);
    let launch = owned_device_launch(&harness.state).unwrap();
    let sha = super::super::artifact::selected_trusted_sha().expect("selected digest");
    assert_ne!(sha, super::super::artifact::PINNED_SHA256);
    let executable = super::super::io::version_dir(&harness.state.data_dir, &sha)
        .join(super::super::io::executable_name());
    assert_eq!(launch.executable, executable);
    drop(launch);
    let after = snapshot_historical(&harness.state);
    assert!(after == before, "historical CPA bytes changed");
    assert_eq!(before.base_url, "http://127.0.0.1:9/");
    assert!(
        before.management_key_cipher == management,
        "management cipher changed"
    );
    assert!(
        before.account_key_cipher == inference,
        "inference cipher changed"
    );
    assert_eq!(harness.device.starts.load(Ordering::SeqCst), 0);
    drop(harness.state);
}

#[test]
fn simulated_helper_foreign_historical_row_does_not_veto() {
    let harness = harness("foreign-row");
    let (inference, management) = save_historical(&harness.state, "http://127.0.0.1:1/");
    let before = snapshot_historical(&harness.state);
    let launch = owned_device_launch(&harness.state).expect("historical row is not a device veto");
    drop(launch);
    let again = owned_device_launch(&harness.state).expect("obsolete env is not a device veto");
    drop(again);
    let after = snapshot_historical(&harness.state);
    assert!(after == before, "historical CPA bytes changed");
    assert_eq!(before.base_url, "http://127.0.0.1:1/");
    assert!(
        before.management_key_cipher == management,
        "management cipher changed"
    );
    assert!(
        before.account_key_cipher == inference,
        "inference cipher changed"
    );
    assert_eq!(harness.device.starts.load(Ordering::SeqCst), 0);
    drop(harness.state);
}

#[tokio::test]
async fn simulated_helper_lifecycle_cancels_on_stop_remove_and_rollback() {
    let stopped = prompted("stop").await;
    let revision = stopped.state.settings_revision();
    let generation = stopped.state.process_generation();
    crate::cpa_execution::stop(&stopped.state, revision, generation).unwrap();
    assert!(!stopped.device.running.load(Ordering::SeqCst));
    assert!(stopped.device.stops.load(Ordering::SeqCst) >= 1);

    let removed = prompted("remove").await;
    let revision = removed.state.settings_revision();
    let generation = removed.state.process_generation();
    crate::cpa_execution::remove(&removed.state, revision, generation).unwrap();
    assert!(!removed.device.running.load(Ordering::SeqCst));
    assert!(removed.device.stops.load(Ordering::SeqCst) >= 1);

    let rolled = prompted("rollback").await;
    let revision = rolled.state.settings_revision();
    let generation = rolled.state.process_generation();
    let error = crate::cpa_execution::rollback(&rolled.state, revision, generation)
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        crate::cpa_execution::ExecutionError::RollbackUnavailable
    ));
    assert!(rolled.device.running.load(Ordering::SeqCst));
    assert_eq!(rolled.device.stops.load(Ordering::SeqCst), 0);
    assert_eq!(rolled.gateway.starts.load(Ordering::SeqCst), 0);
    drop(stopped.state);
    drop(removed.state);
    drop(rolled.state);
}

#[tokio::test]
async fn simulated_helper_rejected_lifecycle_keeps_the_waiting_login() {
    let stopped = prompted("reject-stop").await;
    let revision = stopped.state.settings_revision();
    let generation = stopped.state.process_generation();
    let error = crate::cpa_execution::stop(&stopped.state, revision.wrapping_add(1), generation)
        .unwrap_err();
    assert_eq!(error.to_string(), "revisionConflict");
    assert!(stopped.device.running.load(Ordering::SeqCst));
    assert_eq!(stopped.device.stops.load(Ordering::SeqCst), 0);

    let removed = prompted("reject-remove").await;
    let revision = removed.state.settings_revision();
    let generation = removed.state.process_generation();
    let error = crate::cpa_execution::remove(&removed.state, revision, generation.wrapping_add(1))
        .unwrap_err();
    assert_eq!(error.to_string(), "revisionConflict");
    assert!(removed.device.running.load(Ordering::SeqCst));
    assert_eq!(removed.device.stops.load(Ordering::SeqCst), 0);

    let updated = prompted("reject-update").await;
    let revision = updated.state.settings_revision();
    let generation = updated.state.process_generation();
    let version = crate::cpa_execution::update(&updated.state, revision, generation, Some("0.0.0"))
        .await
        .unwrap_err();
    let version = version.to_string();
    assert!(version.contains("v8.0.10"), "{version}");
    let stale =
        crate::cpa_execution::update(&updated.state, revision.wrapping_add(4), generation, None)
            .await
            .unwrap_err();
    let stale = stale.to_string();
    assert_eq!(stale, "revisionConflict");
    assert!(updated.device.running.load(Ordering::SeqCst));
    assert_eq!(updated.device.stops.load(Ordering::SeqCst), 0);
    assert_eq!(updated.gateway.starts.load(Ordering::SeqCst), 0);
    drop(stopped.state);
    drop(removed.state);
    drop(updated.state);
}

#[tokio::test]
async fn simulated_helper_valid_rollback_cancels_the_waiting_helper() {
    let rolled = prompted("rollback-ok").await;
    let revision = rolled.state.settings_revision();
    let generation = rolled.state.process_generation();
    let (shutdown, _receiver) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async {});
    *rolled.state.gateway.lock() = Some(crate::state::GatewayHandle {
        port: 9,
        listen_addr: "127.0.0.1:9".parse().unwrap(),
        dashboard_is_local: true,
        shutdown,
        task,
    });
    let yaml = "ocg:\n  process-generation: \"4\"\n  projection-revision: \"2\"\n";
    super::super::io::atomic_write_private(
        &super::super::io::previous_config_path(&rolled.state.data_dir),
        yaml.as_bytes(),
    )
    .unwrap();
    let _hooks = SyntheticHooks::arm();
    let report = crate::cpa_execution::rollback(&rolled.state, revision, generation).await;
    drop(_hooks);
    let report = report.expect("synthetic rollback");
    assert_eq!(report.apply_status, "applied");
    assert!(!rolled.device.running.load(Ordering::SeqCst));
    assert!(rolled.device.stops.load(Ordering::SeqCst) >= 1);
    assert_eq!(rolled.gateway.starts.load(Ordering::SeqCst), 0);
    drop(rolled.state);
}

struct SyntheticHooks;

impl SyntheticHooks {
    fn arm() -> Self {
        crate::cpa_execution::set_skip_spawn(true);
        crate::cpa_execution::set_fail_ready(false);
        crate::cpa_execution::set_test_password(Some("synthetic-device-management".to_string()));
        crate::cpa_execution::set_before_apply_commit(None);
        Self
    }
}

impl Drop for SyntheticHooks {
    fn drop(&mut self) {
        crate::cpa_execution::set_skip_spawn(false);
        crate::cpa_execution::set_fail_ready(false);
        crate::cpa_execution::set_test_password(None);
        crate::cpa_execution::set_before_apply_commit(None);
    }
}

async fn prompted(name: &str) -> Harness {
    let harness = harness(name);
    harness.device.set_stdout(&prompt_stdout());
    let started = CoreStateInner::with_test_device_host(
        as_device_host(&harness.device),
        harness.state.start_cpa_device_oauth(),
    )
    .await
    .unwrap();
    assert_eq!(started.user_code.as_deref(), Some("ABCD-1234"));
    assert!(harness.device.running.load(Ordering::SeqCst));
    harness
}

struct HistoricalBytes {
    base_url: String,
    management_key_cipher: String,
    account_key_cipher: String,
    destination_base_url: Option<String>,
    observer_key_cipher: String,
}

impl PartialEq for HistoricalBytes {
    fn eq(&self, other: &Self) -> bool {
        self.base_url == other.base_url
            && self.management_key_cipher == other.management_key_cipher
            && self.account_key_cipher == other.account_key_cipher
            && self.destination_base_url == other.destination_base_url
            && self.observer_key_cipher == other.observer_key_cipher
    }
}

fn save_historical(state: &CoreState, base_url: &str) -> (String, String) {
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
        key_cipher: inference.clone(),
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
    (inference, management)
}

fn snapshot_historical(state: &CoreState) -> HistoricalBytes {
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
    HistoricalBytes {
        base_url: record.base_url,
        management_key_cipher: record.management_key_cipher,
        account_key_cipher,
        destination_base_url,
        observer_key_cipher,
    }
}

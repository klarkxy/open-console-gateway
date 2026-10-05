use super::super::destinations::DestinationsError;
use super::*;
use crate::crypto::StaticKeyCipher;
use crate::dashboard_v3::MutationExpectation;
use crate::db::Database;
use crate::models::{Account, AccountSetupStep, AccountType};
use crate::provider::{CredentialKind, OPENCODE_PROVIDER_ID, QuotaScope};
use crate::quota_recovery::PersistedQuotaRecovery;
use crate::quota_recovery::QuotaEpisode;
use crate::state::CoreStateInner;
use chrono::{TimeZone, Utc};
use ocg_domain::credential::credential_id_for_legacy_account;
use ocg_gateway::quota::{QuotaEvidence, QuotaReason, QuotaWindowKind};
use std::sync::Arc;

fn state(tag: &str) -> (std::path::PathBuf, crate::state::CoreState) {
    let dir = std::env::temp_dir().join(format!("ocg-quota-retry-{tag}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let db = Database::open(dir.clone()).unwrap();
    let state = Arc::new(
        CoreStateInner::new(
            db,
            dir.clone(),
            Arc::new(StaticKeyCipher::new("quota-retry")),
        )
        .unwrap(),
    );
    (dir, state)
}

fn insert_go(state: &crate::state::CoreState, id: &str) {
    let now = Utc::now();
    state
        .db
        .lock()
        .create_account(&Account {
            id: id.into(),
            provider_id: OPENCODE_PROVIDER_ID.into(),
            credential_kind: CredentialKind::ApiKey,
            quota_scope: QuotaScope::Key,
            name: id.into(),
            username: None,
            password_cipher: None,
            key_cipher: "cipher".into(),
            enabled: true,
            account_type: AccountType::Key,
            setup_step: AccountSetupStep::Ready,
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
        })
        .unwrap();
}

#[test]
fn legacy_exhaustion_does_not_retry_or_appear_on_the_credential() {
    let (dir, state) = state("legacy-only");
    insert_go(&state, "acct-a");
    let episode = persist_waiting(&state, "acct-a");
    state
        .quota_probes
        .lock()
        .insert(episode.credential_id.clone(), episode);
    let credential_id = credential_id_for_legacy_account("acct-a").to_string();
    let stored = recovery_column(&state, "acct-a");
    let revision = state.settings_revision();
    assert_no_policy_retry(&state, &credential_id);
    assert_eq!(state.settings_revision(), revision);
    assert_eq!(recovery_column(&state, "acct-a"), stored);
    assert!(listed_recovery(&state, &credential_id).is_none());
    drop(state);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn quota_retry_rejects_stale_cas() {
    let (dir, state) = state("cas");
    insert_go(&state, "acct-a");
    let credential_id = credential_id_for_legacy_account("acct-a").to_string();
    let err = quota_retry_locked(
        &state,
        &credential_id,
        MutationExpectation {
            expected_revision: state.settings_revision() + 1,
            process_generation: state.process_generation(),
        },
    )
    .unwrap_err();
    match err {
        DestinationsError::Api(_) => {}
        DestinationsError::Refused(_) => panic!("expected CAS conflict"),
    }
    drop(state);
    std::fs::remove_dir_all(dir).unwrap();
}

fn persist_waiting(state: &crate::state::CoreState, account_id: &str) -> QuotaEpisode {
    let now = Utc::now();
    persist_recovery_at(state, account_id, now + chrono::Duration::hours(1))
}

fn persist_recovery_at(
    state: &crate::state::CoreState,
    account_id: &str,
    next_retry_at: chrono::DateTime<Utc>,
) -> QuotaEpisode {
    let observed = next_retry_at - chrono::Duration::minutes(15);
    let mut recovery = PersistedQuotaRecovery::from_evidence(
        None,
        &QuotaEvidence {
            reason: QuotaReason::QuotaExhausted,
            window: QuotaWindowKind::Unknown,
            resets_at_rfc3339: None,
            resets_in_text: None,
        },
        observed,
        None,
    );
    recovery.next_retry_at = next_retry_at;
    let (id, version, key_cipher, _) =
        crate::db::quota_recovery::load_for_legacy_on(&state.db.lock().conn, account_id)
            .unwrap()
            .unwrap();
    let episode = QuotaEpisode {
        credential_id: id,
        account_id: account_id.into(),
        credential_version: version,
        epoch: recovery.epoch,
        key_cipher,
    };
    crate::db::quota_recovery::save_on(&state.db.lock().conn, &episode, &recovery).unwrap();
    episode
}

fn assert_no_policy_retry(state: &crate::state::CoreState, credential_id: &str) {
    let err = quota_retry_locked(
        state,
        credential_id,
        MutationExpectation {
            expected_revision: state.settings_revision(),
            process_generation: state.process_generation(),
        },
    )
    .unwrap_err();
    match err {
        DestinationsError::Api(error) => {
            let text = format!("{error:?}");
            assert!(text.contains("credential has no confirmed quota exhaustion"));
        }
        DestinationsError::Refused(_) => panic!("expected no applicable policy restriction"),
    }
}

fn listed_recovery(
    state: &crate::state::CoreState,
    credential_id: &str,
) -> Option<crate::dashboard_v4::types::QuotaRecoveryDto> {
    let projection = {
        let db = state.db.lock();
        read_v4_projection(&db).unwrap().unwrap()
    };
    super::super::destinations::overlay_with_policy(state, &projection.credentials)
        .unwrap()
        .into_iter()
        .find(|row| row.id == credential_id)
        .unwrap()
        .quota_recovery
}

fn apply_official(
    state: &crate::state::CoreState,
    credential_id: &str,
    fetched_at: chrono::DateTime<Utc>,
    rolling_status: &str,
    rolling_at: chrono::DateTime<Utc>,
) {
    let weekly = fetched_at + chrono::Duration::days(2);
    let monthly = fetched_at + chrono::Duration::days(40);
    let body = format!(
        r#"{{"usage":{{"rolling":{{"status":"{rolling_status}","percent":0,"resetsAt":"{rolling}"}},"weekly":{{"status":"ok","percent":0,"resetsAt":"{weekly}"}},"monthly":{{"status":"ok","percent":1,"resetsAt":"{monthly}"}}}},"note":"sk-planted-secret"}}"#,
        rolling = rolling_at.to_rfc3339(),
        weekly = weekly.to_rfc3339(),
        monthly = monthly.to_rfc3339(),
    );
    let mut db = state.db.lock();
    let fence = crate::cpa_quota::capture_live_fence(&db.conn, credential_id).unwrap();
    let commit = crate::cpa_quota::OfficialPlanCommit {
        provider_id: fence.provider_id.clone(),
        fence,
        observation_id: uuid::Uuid::new_v4().to_string(),
        fetched_at,
        body: body.into_bytes(),
    };
    let applied = crate::cpa_quota::apply_accepted(&db.conn, &commit).unwrap();
    assert_eq!(applied, crate::cpa_policy::QuotaApply::Applied);
}

#[test]
fn healthy_clear_expired_row_and_rotated_scope_leave_legacy_historical() {
    let (dir, state) = state("cleared-legacy");
    insert_go(&state, "acct-a");
    persist_waiting(&state, "acct-a");
    let credential_id = credential_id_for_legacy_account("acct-a").to_string();
    let stored = recovery_column(&state, "acct-a");
    let fetched_at = Utc::now();
    apply_official(
        &state,
        &credential_id,
        fetched_at,
        "rate-limited",
        fetched_at + chrono::Duration::seconds(90),
    );
    apply_official(
        &state,
        &credential_id,
        fetched_at + chrono::Duration::seconds(1),
        "ok",
        fetched_at + chrono::Duration::seconds(90),
    );
    let revision = state.settings_revision();
    assert!(listed_recovery(&state, &credential_id).is_none());
    assert_no_policy_retry(&state, &credential_id);
    assert_eq!(state.settings_revision(), revision);
    assert_eq!(recovery_column(&state, "acct-a"), stored);

    let expired_fetch = Utc::now() - chrono::Duration::seconds(120);
    let expired_at = expired_fetch + chrono::Duration::seconds(60);
    apply_official(
        &state,
        &credential_id,
        expired_fetch,
        "rate-limited",
        expired_at,
    );
    let card = listed_recovery(&state, &credential_id).unwrap();
    assert_eq!(
        card.status,
        crate::dashboard_v4::types::QuotaRecoveryStatus::Ready
    );
    assert_eq!(
        card.window,
        crate::dashboard_v4::types::QuotaRecoveryWindow::FiveHours
    );
    assert_eq!(
        card.resets_at.as_deref(),
        Some(expired_at.to_rfc3339().as_str())
    );
    assert_eq!(card.failure_count, 0);
    assert!(!stored.contains(&expired_at.to_rfc3339()));
    let before_retry = state.settings_revision();
    let unchanged = quota_retry_locked(
        &state,
        &credential_id,
        MutationExpectation {
            expected_revision: before_retry,
            process_generation: state.process_generation(),
        },
    )
    .unwrap();
    assert_eq!(unchanged.revision.revision, before_retry);
    assert_eq!(
        unchanged
            .credential
            .quota_recovery
            .as_ref()
            .unwrap()
            .resets_at
            .as_deref(),
        Some(expired_at.to_rfc3339().as_str())
    );
    assert_eq!(recovery_column(&state, "acct-a"), stored);

    state
        .db
        .lock()
        .conn
        .execute(
            "UPDATE credentials SET scope_json = '{\"kind\":\"only\",\"models\":[\"other-model\"]}'
             WHERE legacy_account_id = 'acct-a'",
            [],
        )
        .unwrap();
    assert!(listed_recovery(&state, &credential_id).is_none());
    assert_no_policy_retry(&state, &credential_id);
    assert_eq!(recovery_column(&state, "acct-a"), stored);
    drop(state);
    std::fs::remove_dir_all(dir).unwrap();
}

fn recovery_column(state: &crate::state::CoreState, account_id: &str) -> String {
    state
        .db
        .lock()
        .conn
        .query_row(
            "SELECT quota_recovery_json FROM credentials WHERE legacy_account_id = ?1",
            [account_id],
            |row| row.get(0),
        )
        .unwrap()
}

#[test]
fn official_quota_retry_keeps_the_exact_deadline_and_leaves_legacy_json() {
    let (dir, state) = state("official-plan");
    insert_go(&state, "acct-a");
    persist_waiting(&state, "acct-a");
    let before_json = recovery_column(&state, "acct-a");
    let credential_id = credential_id_for_legacy_account("acct-a").to_string();
    let fetched_at = Utc::now();
    let rolling = fetched_at + chrono::Duration::seconds(90);
    let weekly = fetched_at + chrono::Duration::days(2);
    let monthly = fetched_at + chrono::Duration::days(40);
    let body = format!(
        r#"{{"usage":{{"rolling":{{"status":"rate-limited","percent":100,"resetsAt":"{rolling}"}},"weekly":{{"status":"ok","percent":100,"resetsAt":"{weekly}"}},"monthly":{{"status":"ok","percent":1,"resetsAt":"{monthly}"}}}},"note":"sk-planted-secret"}}"#,
        rolling = rolling.to_rfc3339(),
        weekly = weekly.to_rfc3339(),
        monthly = monthly.to_rfc3339(),
    );
    let applied = {
        let mut db = state.db.lock();
        let fence = crate::cpa_quota::capture_live_fence(&db.conn, &credential_id).unwrap();
        let commit = crate::cpa_quota::OfficialPlanCommit {
            provider_id: fence.provider_id.clone(),
            fence,
            observation_id: uuid::Uuid::new_v4().to_string(),
            fetched_at,
            body: body.into_bytes(),
        };
        crate::cpa_quota::apply_accepted(&mut db.conn, &commit).unwrap()
    };
    assert_eq!(applied, crate::cpa_policy::QuotaApply::Applied);
    assert_eq!(recovery_column(&state, "acct-a"), before_json);

    let revision = state.settings_revision();
    let waiting = quota_retry_locked(
        &state,
        &credential_id,
        MutationExpectation {
            expected_revision: revision,
            process_generation: state.process_generation(),
        },
    )
    .unwrap();
    assert_eq!(waiting.revision.revision, revision);
    let card = waiting.credential.quota_recovery.as_ref().unwrap();
    assert_eq!(
        card.status,
        crate::dashboard_v4::types::QuotaRecoveryStatus::Waiting
    );
    assert_eq!(
        card.reason,
        crate::dashboard_v4::types::QuotaRecoveryReason::QuotaExhausted
    );
    assert_eq!(
        card.window,
        crate::dashboard_v4::types::QuotaRecoveryWindow::FiveHours
    );
    assert_eq!(
        card.resets_at.as_deref(),
        Some(rolling.to_rfc3339().as_str())
    );
    assert_eq!(card.next_retry_at, rolling.to_rfc3339());
    assert_eq!(card.failure_count, 0);
    let again = quota_retry_locked(
        &state,
        &credential_id,
        MutationExpectation {
            expected_revision: revision,
            process_generation: state.process_generation(),
        },
    )
    .unwrap();
    assert_eq!(again.revision.revision, revision);
    assert_eq!(
        again.credential.quota_recovery.as_ref().unwrap().resets_at,
        card.resets_at
    );
    assert_eq!(recovery_column(&state, "acct-a"), before_json);

    let projection = {
        let db = state.db.lock();
        read_v4_projection(&db).unwrap().unwrap()
    };
    let overlaid =
        super::super::destinations::overlay_with_policy(&state, &projection.credentials).unwrap();
    let listed = overlaid
        .iter()
        .find(|row| row.id == credential_id)
        .unwrap()
        .quota_recovery
        .as_ref()
        .unwrap();
    assert_eq!(listed.window, card.window);
    assert_eq!(listed.resets_at, card.resets_at);
    assert_eq!(listed.next_retry_at, card.next_retry_at);
    assert_eq!(listed.failure_count, 0);
    let policy = state
        .db
        .lock()
        .conn
        .query_row(
            "SELECT value FROM settings WHERE key = ?1",
            [crate::cpa_policy::SETTINGS_KEY],
            |row| row.get::<_, String>(0),
        )
        .unwrap();
    assert!(!policy.contains("sk-planted-secret"));
    assert!(!before_json.contains("sk-planted-secret"));
    drop(state);
    std::fs::remove_dir_all(dir).unwrap();
}

/// Skip-spawn owned plane. The empty task fills `GatewayHandle`; it is not a child process.
struct OwnedApplyGuard {
    _shutdown_rx: tokio::sync::oneshot::Receiver<()>,
}

impl OwnedApplyGuard {
    fn arm(state: &crate::state::CoreState) -> Self {
        crate::cpa_execution::set_skip_spawn(true);
        crate::cpa_execution::set_fail_ready(false);
        crate::cpa_execution::set_test_password(Some("synthetic-management".to_string()));
        crate::cpa_execution::set_before_apply_commit(None);
        crate::cpa_execution::set_artifact_dir(
            state,
            crate::cpa_execution::documented_runtime_dir(),
        );
        let (shutdown, shutdown_rx) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async {});
        *state.gateway.lock() = Some(crate::gateway_runtime::GatewayHandle {
            port: 9,
            listen_addr: "127.0.0.1:9".parse().unwrap(),
            dashboard_is_local: true,
            shutdown,
            task,
        });
        Self {
            _shutdown_rx: shutdown_rx,
        }
    }
}

impl Drop for OwnedApplyGuard {
    fn drop(&mut self) {
        crate::cpa_execution::set_skip_spawn(false);
        crate::cpa_execution::set_fail_ready(false);
        crate::cpa_execution::set_test_password(None);
        crate::cpa_execution::set_before_apply_commit(None);
    }
}

fn credential_version(state: &crate::state::CoreState, account_id: &str) -> u64 {
    state
        .db
        .lock()
        .list_identity_model()
        .unwrap()
        .accounts
        .into_iter()
        .find(|row| row.account.id == account_id)
        .unwrap()
        .credential_version
}

fn key_cipher(state: &crate::state::CoreState, account_id: &str) -> String {
    state
        .db
        .lock()
        .get_account(account_id)
        .unwrap()
        .unwrap()
        .key_cipher
}

fn rotate_body(state: &crate::state::CoreState, secret: &str) -> Bytes {
    Bytes::from(
        serde_json::to_vec(&CredentialRotateRequest {
            expectation: MutationExpectation {
                expected_revision: state.settings_revision(),
                process_generation: state.process_generation(),
            },
            secret_input: secret.into(),
        })
        .unwrap(),
    )
}

async fn started_plane(state: &crate::state::CoreState) -> crate::cpa_execution::ExecutionReport {
    let started =
        crate::cpa_execution::start(state, state.settings_revision(), state.process_generation())
            .await
            .expect("owned plane start");
    assert_eq!(started.apply_status, "applied");
    assert!(started.desired_running);
    started
}

#[tokio::test(flavor = "current_thread")]
async fn rotate_keeps_the_saved_receipt_and_applies_the_new_version() {
    let (dir, state) = state("projection-rotate");
    insert_go(&state, "acct-projection");
    let credential_id = credential_id_for_legacy_account("acct-projection").to_string();
    let before_version = credential_version(&state, "acct-projection");
    let before_cipher = key_cipher(&state, "acct-projection");
    let _plane = OwnedApplyGuard::arm(&state);
    let started = started_plane(&state).await;
    let saved = rotate(
        State(state.clone()),
        Path(credential_id.clone()),
        rotate_body(&state, "sk-rotated-projection"),
    )
    .await
    .expect("credential rotate")
    .0;
    assert_eq!(saved.credential_id, credential_id);
    assert!(!saved.replayed);
    assert!(saved.version > before_version);
    assert_eq!(saved.revision.revision, state.settings_revision());
    assert_eq!(
        saved.revision.process_generation,
        state.process_generation()
    );
    assert_eq!(credential_version(&state, "acct-projection"), saved.version);
    assert_ne!(key_cipher(&state, "acct-projection"), before_cipher);
    let applied = crate::cpa_execution::execution_report(&state);
    assert_eq!(applied.desired_revision, started.desired_revision + 1);
    assert_eq!(applied.applied_revision, applied.desired_revision);
    assert_eq!(applied.apply_status, "applied");
    assert!(state.settings_update.try_lock().is_some());
    drop(state);
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn rotate_apply_failure_keeps_the_saved_credential() {
    let (dir, state) = state("projection-rotate-fail");
    insert_go(&state, "acct-projection");
    let credential_id = credential_id_for_legacy_account("acct-projection").to_string();
    let before_version = credential_version(&state, "acct-projection");
    let before_cipher = key_cipher(&state, "acct-projection");
    let _plane = OwnedApplyGuard::arm(&state);
    let started = started_plane(&state).await;
    crate::cpa_execution::set_fail_ready(true);
    let saved = rotate(
        State(state.clone()),
        Path(credential_id.clone()),
        rotate_body(&state, "sk-rotated-projection"),
    )
    .await
    .expect("saved rotation survives apply failure");
    assert_eq!(saved.credential_id, credential_id);
    assert!(saved.version > before_version);
    assert_eq!(saved.revision.revision, state.settings_revision());
    assert_eq!(credential_version(&state, "acct-projection"), saved.version);
    assert_ne!(key_cipher(&state, "acct-projection"), before_cipher);
    let failed = crate::cpa_execution::execution_report(&state);
    assert_eq!(failed.desired_revision, started.desired_revision + 1);
    assert_eq!(failed.applied_revision, started.applied_revision);
    assert_eq!(failed.apply_status, "apply_failed");
    assert_eq!(failed.error.as_deref(), Some("cpa_apply_failed"));
    let logged: i64 = state
        .db
        .lock()
        .conn
        .query_row(
            "SELECT COUNT(*) FROM gateway_logs WHERE category = 'cpa' AND message LIKE '%event=cpa_apply_failed%'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(logged, 1);
    assert!(state.settings_update.try_lock().is_some());
    drop(state);
    std::fs::remove_dir_all(dir).unwrap();
}

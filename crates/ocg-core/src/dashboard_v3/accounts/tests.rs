use super::*;
use crate::crypto::StaticKeyCipher;
use crate::dashboard_v3::receipt_test_support::fail_after_cpa_revision_advance;
use crate::db::Database;
use crate::state::CoreStateInner;
use axum::extract::{Path, State};
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::Duration;

const LABEL: &str = "ledger-label-9f3c";
const SECRET: &str = "sk-test-ledger-9f3c";

struct OperationRow {
    action: String,
    source: String,
    subject_type: Option<String>,
    subject_id: Option<String>,
    outcome: String,
    reason_code: Option<String>,
    metadata_json: String,
}

fn open_state(tag: &str) -> (std::path::PathBuf, CoreState) {
    let dir = std::env::temp_dir().join(format!("ocg-account-op-{tag}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let db = Database::open(dir.clone()).unwrap();
    let state = Arc::new(
        CoreStateInner::new(db, dir.clone(), Arc::new(StaticKeyCipher::new(tag))).unwrap(),
    );
    (dir, state)
}

fn close(dir: std::path::PathBuf, state: CoreState) {
    drop(state);
    let _ = std::fs::remove_dir_all(dir);
}

fn operations(state: &CoreState) -> Vec<OperationRow> {
    let db = state.db.lock();
    let mut stmt = db
        .conn
        .prepare(
            "SELECT action, source, subject_type, subject_id, outcome, reason_code, metadata_json
             FROM operation_logs ORDER BY rowid",
        )
        .expect("operation_logs is required to read semantic receipts");
    let rows = stmt
        .query_map([], |row| {
            Ok(OperationRow {
                action: row.get(0)?,
                source: row.get(1)?,
                subject_type: row.get(2)?,
                subject_id: row.get(3)?,
                outcome: row.get(4)?,
                reason_code: row.get(5)?,
                metadata_json: row.get(6)?,
            })
        })
        .unwrap();
    rows.map(|row| row.unwrap()).collect()
}

fn body(state: &CoreState, revision: u64) -> Bytes {
    Bytes::from(
        serde_json::to_vec(&json!({
            "expectedRevision": revision,
            "processGeneration": state.process_generation(),
            "name": LABEL,
            "key": SECRET,
        }))
        .unwrap(),
    )
}

fn assert_no_secret(text: &str) {
    assert!(
        !text.contains(LABEL),
        "operation receipt contains an account label"
    );
    assert!(
        !text.contains(SECRET),
        "operation receipt contains key material"
    );
}

fn gateway_text(state: &CoreState) -> String {
    state
        .db
        .lock()
        .list_gateway_logs(100)
        .unwrap()
        .into_iter()
        .map(|log| format!("{} {}", log.category, log.message))
        .collect::<Vec<_>>()
        .join("\n")
}

#[tokio::test]
async fn create_records_one_success_with_committed_identity() {
    let (dir, state) = open_state("create");
    let created = create_account(
        State(state.clone()),
        body(&state, state.settings_revision()),
    )
    .await
    .expect("create")
    .0;
    let account = created.account.expect("created account");
    assert_eq!(account.name, LABEL);
    assert_eq!(created.revision, state.settings_revision());
    let rows = operations(&state);
    assert_eq!(rows.len(), 1, "one final receipt");
    assert_eq!(rows[0].action, "account.create");
    assert_eq!(rows[0].source, "dashboard");
    assert_eq!(rows[0].subject_type.as_deref(), Some("account"));
    assert_eq!(rows[0].subject_id.as_deref(), Some(account.id.as_str()));
    assert_eq!(rows[0].outcome, "success");
    assert!(rows[0].reason_code.is_none());
    let metadata: Value = serde_json::from_str(&rows[0].metadata_json).unwrap();
    assert_eq!(metadata["revision"].as_u64(), Some(created.revision));
    assert_no_secret(&rows[0].metadata_json);
    assert_no_secret(rows[0].subject_id.as_deref().unwrap_or(""));
    assert_no_secret(&gateway_text(&state));
    close(dir, state);
}

#[tokio::test]
async fn conflicting_revision_is_rejected_without_a_write() {
    let (dir, state) = open_state("cas");
    let before = state.settings_revision();
    let error = create_account(State(state.clone()), body(&state, before.saturating_add(4)))
        .await
        .expect_err("stale revision");
    assert_eq!(error.body.code, "revisionConflict");
    assert_eq!(state.settings_revision(), before);
    let rows = operations(&state);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].action, "account.create");
    assert_eq!(rows[0].outcome, "rejected");
    assert_eq!(rows[0].reason_code.as_deref(), Some("revisionConflict"));
    assert!(rows[0].subject_id.is_none());
    assert_no_secret(&rows[0].metadata_json);
    close(dir, state);
}

#[tokio::test]
async fn successful_delete_has_no_provisional_failure_count() {
    let (dir, state) = open_state("delete-success-counts");
    let created = create_account(
        State(state.clone()),
        body(&state, state.settings_revision()),
    )
    .await
    .unwrap()
    .0;
    let id = created.account.unwrap().id;
    let _ = delete_account(
        State(state.clone()),
        Path(id.clone()),
        Bytes::from(
            serde_json::to_vec(&json!({
                "expectedRevision": state.settings_revision(),
                "processGeneration": state.process_generation(),
            }))
            .unwrap(),
        ),
    )
    .await
    .unwrap();
    assert!(state.db.lock().get_account(&id).unwrap().is_none());
    let row = operations(&state)
        .into_iter()
        .find(|row| row.action == "account.delete")
        .unwrap();
    assert_eq!(row.outcome, "success");
    let metadata: Value = serde_json::from_str(&row.metadata_json).unwrap();
    assert_eq!(metadata["completedCount"].as_u64(), Some(1));
    assert!(metadata["failedCount"].is_null());
    close(dir, state);
}

#[tokio::test]
async fn invalid_payload_is_rejected() {
    let (dir, state) = open_state("payload");
    let before = state.settings_revision();
    let error = create_account(State(state.clone()), Bytes::from_static(b"{"))
        .await
        .expect_err("broken json");
    assert_eq!(error.body.code, "invalidJson");
    let missing = create_account(State(state.clone()), Bytes::from_static(b"{}"))
        .await
        .expect_err("missing revision");
    assert_eq!(missing.body.code, "missingExpectedRevision");
    assert_eq!(state.settings_revision(), before);
    let rows = operations(&state);
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|row| row.outcome == "rejected"));
    assert_eq!(rows[0].reason_code.as_deref(), Some("invalidJson"));
    assert_eq!(
        rows[1].reason_code.as_deref(),
        Some("missingExpectedRevision")
    );
    close(dir, state);
}

#[tokio::test]
async fn recording_failure_keeps_the_committed_ack() {
    let (dir, state) = open_state("record-fail");
    state
        .db
        .lock()
        .conn
        .execute_batch(
            "CREATE TRIGGER ocg_test_fail_operation_insert
             BEFORE INSERT ON operation_logs
             BEGIN
               SELECT RAISE(ABORT, 'operation record failed');
             END;",
        )
        .expect("operation_logs must exist to force a receipt insert failure");
    let before = state.settings_revision();
    let created = create_account(State(state.clone()), body(&state, before))
        .await
        .expect("a failed receipt must not fail the write")
        .0;
    let account = created.account.expect("created account");
    assert!(created.revision > before);
    assert_eq!(created.revision, state.settings_revision());
    assert!(state.db.lock().get_account(&account.id).unwrap().is_some());
    assert!(
        operations(&state).is_empty(),
        "the aborted receipt insert must not leave a second row"
    );
    assert_no_secret(&gateway_text(&state));
    close(dir, state);
}

const BLOCKED: &str = "own-commit-blocked";

fn completed(metadata_json: &str) -> Option<u64> {
    let metadata: Value = serde_json::from_str(metadata_json).unwrap();
    metadata
        .get("completedCount")
        .and_then(|value| value.as_u64())
}

fn credential_enabled(state: &CoreState, id: &str) -> i64 {
    state
        .db
        .lock()
        .conn
        .query_row(
            "SELECT enabled FROM credentials WHERE legacy_account_id = ?1",
            [id],
            |row| row.get(0),
        )
        .unwrap()
}

#[tokio::test]
async fn toggle_before_commit_ignores_an_outside_revision_bump() {
    let (dir, state) = open_state("toggle-before");
    let created = create_account(
        State(state.clone()),
        body(&state, state.settings_revision()),
    )
    .await
    .expect("create")
    .0;
    let id = created.account.expect("account").id;
    assert_eq!(credential_enabled(&state, &id), 1);
    state
        .db
        .lock()
        .conn
        .execute_batch(
            "CREATE TRIGGER ocg_test_abort_account_write
             BEFORE UPDATE ON credentials
             BEGIN
               SELECT ocg_test_before_commit();
             END;",
        )
        .unwrap();
    let before = state.settings_revision();
    let generation = state.process_generation();
    let state_for_call = state.clone();
    let id_for_call = id.clone();
    let error = fail_after_cpa_revision_advance(&state, BLOCKED, async move {
        toggle_account(
            State(state_for_call),
            Path(id_for_call),
            Bytes::from(
                serde_json::to_vec(&json!({
                    "expectedRevision": before,
                    "processGeneration": generation,
                }))
                .unwrap(),
            ),
        )
        .await
    })
    .expect_err("aborted write");
    assert_eq!(error.body.code, "internal");
    assert!(error.body.message.contains(BLOCKED));
    assert_eq!(credential_enabled(&state, &id), 1);
    assert_eq!(state.settings_revision(), before + 1);
    let row = operations(&state)
        .into_iter()
        .find(|row| row.action == "account.toggle")
        .expect("toggle receipt");
    assert_eq!(row.outcome, "failed");
    assert_eq!(row.reason_code.as_deref(), Some("internal"));
    assert_eq!(completed(&row.metadata_json), None);
    assert!(!row.metadata_json.contains(BLOCKED));
    close(dir, state);
}

#[tokio::test]
async fn toggle_commit_notice_precedes_fallible_account_reload() {
    let (dir, state) = open_state("toggle-after");
    let created = create_account(
        State(state.clone()),
        body(&state, state.settings_revision()),
    )
    .await
    .expect("create")
    .0;
    let id = created.account.expect("account").id;
    let before = state.settings_revision();
    let (committed_sender, committed_receiver) = std::sync::mpsc::channel();
    let (broken_sender, broken_receiver) = std::sync::mpsc::channel();
    let db_path = dir.join("data.sqlite");
    let id_for_breaker = id.clone();
    let breaker = std::thread::spawn(move || {
        committed_receiver
            .recv_timeout(Duration::from_secs(5))
            .unwrap();
        let conn = rusqlite::Connection::open(db_path).unwrap();
        conn.execute(
            "UPDATE credentials SET setup_step = 'not-a-step' WHERE legacy_account_id = ?1",
            [id_for_breaker],
        )
        .unwrap();
        broken_sender.send(()).unwrap();
    });
    let mut attempt = DashboardAttempt::open(&state, "account.toggle", "account", Some(id.clone()));
    let result = {
        let _settings_update = state.settings_update.lock();
        account_control::set_account_enabled_locked_recorded(&state, &id, false, |revision| {
            attempt.note_own_commit(revision);
            committed_sender.send(()).unwrap();
            broken_receiver
                .recv_timeout(Duration::from_secs(5))
                .unwrap();
        })
        .map_err(|error| map_account_control_error(&state, error))
        .and_then(|account| mutation_at(&state, account, state.settings_revision()))
    };
    breaker.join().unwrap();
    let error = attempt.finish(result).expect_err("follow-up read");
    assert_eq!(error.body.code, "internal");
    assert!(!error.body.message.is_empty());
    assert_eq!(credential_enabled(&state, &id), 0);
    assert!(state.settings_revision() > before);
    let row = operations(&state)
        .into_iter()
        .find(|row| row.action == "account.toggle")
        .expect("toggle receipt");
    assert_eq!(row.outcome, "partial");
    assert_eq!(row.reason_code.as_deref(), Some("internal"));
    let metadata: Value = serde_json::from_str(&row.metadata_json).unwrap();
    assert_eq!(metadata["completedCount"].as_u64(), Some(1));
    assert_eq!(metadata["failedCount"].as_u64(), Some(1));
    assert_eq!(
        metadata["revision"].as_u64(),
        Some(state.settings_revision())
    );
    assert!(!row.metadata_json.contains("not-a-step"));
    assert!(!row.metadata_json.contains(&error.body.message));
    close(dir, state);
}

mod goat_plan_preservation {
    use super::{AccountUpdate, DashboardAttempt, MutationExpectation, update_account_locked};
    use crate::crypto::StaticKeyCipher;
    use crate::db::Database;
    use crate::goat_plan_cooldowns::{self, GoatPlanCooldowns};
    use crate::models::{Account, AccountSetupStep, AccountType, UsageWindowKind};
    use crate::provider::{
        COMMAND_CODE_PROVIDER_ID, CredentialKind, OPENCODE_PROVIDER_ID, QuotaScope,
    };
    use crate::state::CoreStateInner;
    use chrono::{TimeZone, Utc};
    use std::sync::Arc;

    const SAME_KEY: &str = "goat-plan-same-key";
    const NEXT_KEY: &str = "goat-plan-next-key";

    fn state(tag: &str) -> (std::path::PathBuf, crate::state::CoreState) {
        let dir =
            std::env::temp_dir().join(format!("ocg-goat-key-patch-{tag}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = Database::open(dir.clone()).unwrap();
        let opened = Arc::new(
            CoreStateInner::new(
                db,
                dir.clone(),
                Arc::new(StaticKeyCipher::new("goat-key-patch")),
            )
            .unwrap(),
        );
        (dir, opened)
    }

    fn insert_keyed(state: &crate::state::CoreState, id: &str, provider_id: &str, secret: &str) {
        let now = Utc::now();
        state
            .db
            .lock()
            .create_account(&Account {
                id: id.into(),
                provider_id: provider_id.into(),
                credential_kind: CredentialKind::ApiKey,
                quota_scope: QuotaScope::Key,
                name: id.into(),
                username: None,
                password_cipher: None,
                key_cipher: state.encrypt_key(secret).unwrap(),
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

    fn instant(hour: u32) -> chrono::DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 3, hour, 0, 0).unwrap()
    }

    fn seeded_windows() -> GoatPlanCooldowns {
        GoatPlanCooldowns {
            five_hours: Some(instant(1)),
            week: Some(instant(2)),
            month: Some(instant(3)),
        }
    }

    struct StoredKey {
        credential_id: String,
        binding_id: String,
        version: u64,
        auth_state_version: u64,
        key_cipher: String,
    }

    fn stored_key(state: &crate::state::CoreState, account_id: &str) -> StoredKey {
        state
            .db
            .lock()
            .conn
            .query_row(
                "SELECT id, binding_id, COALESCE(credential_version, 1),
                    COALESCE(auth_state_version, 1), key_cipher
             FROM credentials WHERE legacy_account_id = ?1",
                [account_id],
                |row| {
                    Ok(StoredKey {
                        credential_id: row.get(0)?,
                        binding_id: row.get(1)?,
                        version: row.get::<_, i64>(2)? as u64,
                        auth_state_version: row.get::<_, i64>(3)? as u64,
                        key_cipher: row.get(4)?,
                    })
                },
            )
            .unwrap()
    }

    fn seed_windows(state: &crate::state::CoreState, account_id: &str) -> GoatPlanCooldowns {
        let stored = stored_key(state, account_id);
        let windows = seeded_windows();
        let db = state.db.lock();
        for (window, reset) in [
            (UsageWindowKind::FiveHours, windows.five_hours.unwrap()),
            (UsageWindowKind::Week, windows.week.unwrap()),
            (UsageWindowKind::Month, windows.month.unwrap()),
        ] {
            assert!(
                goat_plan_cooldowns::record_window_on(
                    &db.conn,
                    &stored.credential_id,
                    account_id,
                    &stored.binding_id,
                    stored.version,
                    &stored.key_cipher,
                    window,
                    reset,
                )
                .unwrap()
            );
        }
        windows
    }

    fn plan(state: &crate::state::CoreState, account_id: &str) -> Option<GoatPlanCooldowns> {
        goat_plan_cooldowns::load_for_legacy_on(&state.db.lock().conn, account_id).unwrap()
    }

    fn patch_key(state: &crate::state::CoreState, id: &str, key: &str) {
        let mut attempt =
            DashboardAttempt::open(state, "account.update", "account", Some(id.into()));
        let result = update_account_locked(
            state,
            id,
            AccountUpdate {
                expectation: MutationExpectation {
                    expected_revision: state.settings_revision(),
                    process_generation: state.process_generation(),
                },
                name: None,
                username: None,
                password: None,
                key: Some(key.into()),
                enabled: None,
                referral_code: None,
                purchase_date: None,
                notes: None,
                ollama_billing_tier: None,
            },
            &mut attempt,
        );
        attempt.finish(result).unwrap();
    }

    #[test]
    fn same_goat_key_patch_keeps_every_plan_window_and_ciphertext() {
        let (dir, state) = state("same");
        insert_keyed(&state, "goat-same", COMMAND_CODE_PROVIDER_ID, SAME_KEY);
        let windows = seed_windows(&state, "goat-same");
        let before = stored_key(&state, "goat-same");

        patch_key(&state, "goat-same", &format!("  {SAME_KEY}  "));

        let after = stored_key(&state, "goat-same");
        assert_eq!(after.key_cipher, before.key_cipher);
        assert_eq!(state.decrypt_key(&after.key_cipher).unwrap(), SAME_KEY);
        assert_eq!(plan(&state, "goat-same"), Some(windows));
        assert_eq!(after.version, before.version);
        assert_eq!(after.auth_state_version, before.auth_state_version);
        drop(state);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn changed_goat_key_patch_clears_every_plan_window() {
        let (dir, state) = state("changed");
        insert_keyed(&state, "goat-next", COMMAND_CODE_PROVIDER_ID, SAME_KEY);
        seed_windows(&state, "goat-next");
        let before = stored_key(&state, "goat-next");

        patch_key(&state, "goat-next", NEXT_KEY);

        let after = stored_key(&state, "goat-next");
        assert_ne!(after.key_cipher, before.key_cipher);
        assert_eq!(state.decrypt_key(&after.key_cipher).unwrap(), NEXT_KEY);
        assert_eq!(plan(&state, "goat-next"), None);
        assert_eq!(after.version, before.version);
        assert_eq!(after.auth_state_version, before.auth_state_version);
        drop(state);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn same_non_goat_key_patch_still_reencrypts_and_clears_a_plan_map() {
        let (dir, state) = state("other");
        insert_keyed(&state, "go-same", OPENCODE_PROVIDER_ID, SAME_KEY);
        seed_windows(&state, "go-same");
        let before = stored_key(&state, "go-same");

        patch_key(&state, "go-same", SAME_KEY);

        let after = stored_key(&state, "go-same");
        assert_ne!(after.key_cipher, before.key_cipher);
        assert_eq!(state.decrypt_key(&after.key_cipher).unwrap(), SAME_KEY);
        assert_eq!(plan(&state, "go-same"), None);
        drop(state);
        std::fs::remove_dir_all(dir).unwrap();
    }
}

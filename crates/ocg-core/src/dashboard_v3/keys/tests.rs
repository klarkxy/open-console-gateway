use super::*;
use crate::crypto::StaticKeyCipher;
use crate::dashboard_v3::receipt_test_support::fail_after_cpa_revision_advance;
use crate::db::Database;
use crate::gateway_keys::PRIMARY_KEY_ID;
use crate::state::CoreStateInner;
use axum::extract::State;
use serde_json::{Value, json};
use std::sync::Arc;

const LABEL: &str = "key-label-9f3c";

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
    let dir = std::env::temp_dir().join(format!("ocg-key-op-{tag}-{}", uuid::Uuid::new_v4()));
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

fn create_body(state: &CoreState, revision: u64) -> Bytes {
    Bytes::from(
        serde_json::to_vec(&json!({
            "expectedRevision": revision,
            "processGeneration": state.process_generation(),
            "name": LABEL,
        }))
        .unwrap(),
    )
}

fn stored_text(row: &OperationRow) -> String {
    format!(
        "{} {:?} {:?} {} {}",
        row.action, row.subject_id, row.reason_code, row.metadata_json, row.outcome
    )
}

#[tokio::test]
async fn create_records_the_key_id_without_its_label_or_secret() {
    let (dir, state) = open_state("create");
    let before_key = state.config().gateway_key.clone();
    let ack = create_key(
        State(state.clone()),
        create_body(&state, state.settings_revision()),
    )
    .await
    .expect("create")
    .0;
    assert_eq!(ack.revision, state.settings_revision());
    let stored = state
        .db
        .lock()
        .list_active_sub_gateway_keys()
        .unwrap()
        .into_iter()
        .find(|key| key.name == LABEL)
        .expect("created key");
    let rows = operations(&state);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].action, "key.create");
    assert_eq!(rows[0].source, "dashboard");
    assert_eq!(rows[0].subject_type.as_deref(), Some("key"));
    assert_eq!(rows[0].subject_id.as_deref(), Some(stored.id.as_str()));
    assert_eq!(rows[0].outcome, "success");
    let metadata: Value = serde_json::from_str(&rows[0].metadata_json).unwrap();
    assert_eq!(metadata["revision"].as_u64(), Some(ack.revision));
    let text = stored_text(&rows[0]);
    assert!(
        !text.contains(LABEL),
        "operation receipt contains a key label"
    );
    assert!(
        !text.contains(&stored.key),
        "operation receipt contains key material"
    );
    assert!(
        !text.contains(&before_key),
        "operation receipt contains the primary key"
    );
    let logs = state.db.lock().list_gateway_logs(100).unwrap();
    assert!(logs.iter().all(|log| log.category != "keys"));
    close(dir, state);
}

#[tokio::test]
async fn primary_regenerate_uses_the_stable_key_id() {
    let (dir, state) = open_state("primary");
    let before = state.settings_revision();
    let old_key = state.config().gateway_key.clone();
    let ack = regenerate_primary_key(
        State(state.clone()),
        Bytes::from(
            serde_json::to_vec(&json!({
                "expectedRevision": before,
                "processGeneration": state.process_generation(),
            }))
            .unwrap(),
        ),
    )
    .await
    .expect("regenerate")
    .0;
    let new_key = state.config().gateway_key.clone();
    assert_ne!(new_key, old_key);
    assert_eq!(ack.revision, before + 1);
    let rows = operations(&state);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].action, "key.regenerate");
    assert_eq!(rows[0].subject_id.as_deref(), Some(PRIMARY_KEY_ID));
    assert_eq!(rows[0].outcome, "success");
    let metadata: Value = serde_json::from_str(&rows[0].metadata_json).unwrap();
    assert_eq!(metadata["revision"].as_u64(), Some(ack.revision));
    let text = stored_text(&rows[0]);
    assert!(!text.contains(&old_key));
    assert!(!text.contains(&new_key));
    assert!(!text.contains("Primary"));
    close(dir, state);
}

#[tokio::test]
async fn stale_cas_and_broken_json_are_rejected() {
    let (dir, state) = open_state("reject");
    let before = state.settings_revision();
    let cas = create_key(
        State(state.clone()),
        create_body(&state, before.saturating_add(5)),
    )
    .await
    .expect_err("cas");
    assert_eq!(cas.body.code, "revisionConflict");
    let broken = create_key(State(state.clone()), Bytes::from_static(b"not-json"))
        .await
        .expect_err("json");
    assert_eq!(broken.body.code, "invalidJson");
    assert_eq!(state.settings_revision(), before);
    assert!(
        state
            .db
            .lock()
            .list_active_sub_gateway_keys()
            .unwrap()
            .is_empty()
    );
    let rows = operations(&state);
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].outcome, "rejected");
    assert_eq!(rows[0].reason_code.as_deref(), Some("revisionConflict"));
    assert_eq!(rows[1].reason_code.as_deref(), Some("invalidJson"));
    assert!(rows.iter().all(|row| !stored_text(row).contains(LABEL)));
    close(dir, state);
}

const BLOCKED: &str = "own-commit-blocked";

fn completed(metadata_json: &str) -> Option<u64> {
    let metadata: Value = serde_json::from_str(metadata_json).unwrap();
    metadata
        .get("completedCount")
        .and_then(|value| value.as_u64())
}

#[tokio::test]
async fn primary_regenerate_before_commit_ignores_an_outside_revision_bump() {
    let (dir, state) = open_state("primary-before");
    let old_key = state.config().gateway_key.clone();
    state
        .db
        .lock()
        .conn
        .execute_batch(
            "CREATE TRIGGER ocg_test_abort_config_update
             BEFORE UPDATE ON settings
             WHEN NEW.key = 'config'
             BEGIN
               SELECT ocg_test_before_commit();
             END;
             CREATE TRIGGER ocg_test_abort_config_insert
             BEFORE INSERT ON settings
             WHEN NEW.key = 'config'
             BEGIN
               SELECT ocg_test_before_commit();
             END;",
        )
        .unwrap();
    let before = state.settings_revision();
    let generation = state.process_generation();
    let state_for_call = state.clone();
    let error = fail_after_cpa_revision_advance(&state, BLOCKED, async move {
        regenerate_primary_key(
            State(state_for_call),
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
    .expect_err("aborted config write");
    assert_eq!(error.body.code, "internal");
    assert!(error.body.message.contains(BLOCKED));
    assert_eq!(state.config().gateway_key, old_key);
    assert_eq!(state.settings_revision(), before + 1);
    let rows = operations(&state);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].action, "key.regenerate");
    assert_eq!(rows[0].outcome, "failed");
    assert_eq!(rows[0].reason_code.as_deref(), Some("internal"));
    assert_eq!(completed(&rows[0].metadata_json), None);
    assert!(!stored_text(&rows[0]).contains(&old_key));
    assert!(!rows[0].metadata_json.contains(BLOCKED));
    close(dir, state);
}

#[tokio::test]
async fn primary_regenerate_publish_after_commit_stays_partial() {
    let (dir, state) = open_state("primary-after");
    let created = crate::dashboard_v3::accounts::create_account(
        State(state.clone()),
        Bytes::from(
            serde_json::to_vec(&json!({
                "expectedRevision": state.settings_revision(),
                "processGeneration": state.process_generation(),
                "name": LABEL,
                "key": "sk-test-ledger-9f3c",
            }))
            .unwrap(),
        ),
    )
    .await
    .expect("account")
    .0;
    let _account = created.account.expect("account");
    let changed = state
        .db
        .lock()
        .conn
        .execute("UPDATE credentials SET credential_version = 0", [])
        .unwrap();
    assert!(changed > 0, "a credential row is required");
    let before = state.settings_revision();
    let old_key = state.config().gateway_key.clone();
    let error = regenerate_primary_key(
        State(state.clone()),
        Bytes::from(
            serde_json::to_vec(&json!({
                "expectedRevision": before,
                "processGeneration": state.process_generation(),
            }))
            .unwrap(),
        ),
    )
    .await
    .expect_err("publication after the config commit");
    assert_eq!(error.body.code, "internal");
    assert!(!error.body.message.is_empty());
    let new_key = state.config().gateway_key.clone();
    assert_ne!(new_key, old_key);
    assert!(state.settings_revision() > before);
    let row = operations(&state)
        .into_iter()
        .find(|row| row.action == "key.regenerate")
        .expect("regenerate receipt");
    assert_eq!(row.outcome, "partial");
    assert_eq!(row.reason_code.as_deref(), Some("internal"));
    let metadata: Value = serde_json::from_str(&row.metadata_json).unwrap();
    assert_eq!(metadata["completedCount"].as_u64(), Some(1));
    assert_eq!(metadata["failedCount"].as_u64(), Some(1));
    assert_eq!(
        metadata["revision"].as_u64(),
        Some(state.settings_revision())
    );
    let text = stored_text(&row);
    assert!(!text.contains(&old_key));
    assert!(!text.contains(&new_key));
    assert!(!text.contains(&error.body.message));
    assert!(!text.contains(LABEL));
    close(dir, state);
}

use super::*;
use crate::crypto::StaticKeyCipher;
use crate::dashboard_v3::receipt_test_support::fail_after_cpa_revision_advance;
use crate::db::Database;
use crate::state::CoreStateInner;
use axum::extract::{Path, State};
use serde_json::{Value, json};
use std::sync::Arc;

const LABEL: &str = "provider-label-9f3c";
const ENDPOINT: &str = "http://127.0.0.1:9";

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
    let dir = std::env::temp_dir().join(format!("ocg-provider-op-{tag}-{}", uuid::Uuid::new_v4()));
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
            "endpointUrl": ENDPOINT,
            "upstreamProtocol": "chat_completions",
            "authKind": "none",
            "models": [{ "publicModel": "lab-model", "upstreamModel": "lab-model" }],
        }))
        .unwrap(),
    )
}

#[tokio::test]
async fn create_records_the_provider_id_without_its_label_or_endpoint() {
    let (dir, state) = open_state("create");
    let created = create_provider(
        State(state.clone()),
        create_body(&state, state.settings_revision()),
    )
    .await
    .expect("create")
    .0;
    assert_eq!(created.provider.name, LABEL);
    assert_eq!(created.revision, state.settings_revision());
    let rows = operations(&state);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].action, "provider.create");
    assert_eq!(rows[0].source, "dashboard");
    assert_eq!(rows[0].subject_type.as_deref(), Some("provider"));
    assert_eq!(
        rows[0].subject_id.as_deref(),
        Some(created.provider.id.as_str())
    );
    assert_eq!(rows[0].outcome, "success");
    let metadata: Value = serde_json::from_str(&rows[0].metadata_json).unwrap();
    assert_eq!(metadata["revision"].as_u64(), Some(created.revision));
    let text = format!(
        "{} {}",
        rows[0].subject_id.as_deref().unwrap_or(""),
        rows[0].metadata_json
    );
    assert!(!text.contains(LABEL));
    assert!(!text.contains(ENDPOINT));
    close(dir, state);
}

#[tokio::test]
async fn stale_cas_and_broken_json_are_rejected() {
    let (dir, state) = open_state("reject");
    let before = state.settings_revision();
    let cas = create_provider(
        State(state.clone()),
        create_body(&state, before.saturating_add(6)),
    )
    .await
    .expect_err("cas");
    assert_eq!(cas.body.code, "revisionConflict");
    let broken = create_provider(State(state.clone()), Bytes::from_static(b"{"))
        .await
        .expect_err("json");
    assert_eq!(broken.body.code, "invalidJson");
    assert_eq!(state.settings_revision(), before);
    let rows = operations(&state);
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].outcome, "rejected");
    assert_eq!(rows[0].reason_code.as_deref(), Some("revisionConflict"));
    assert_eq!(rows[1].reason_code.as_deref(), Some("invalidJson"));
    assert!(rows[0].subject_id.is_none());
    close(dir, state);
}

#[tokio::test]
async fn failed_probe_is_a_failed_operation_on_http_success() {
    let (dir, state) = open_state("probe");
    let response = test_provider(
        State(state.clone()),
        Bytes::from(
            serde_json::to_vec(&json!({
                "endpointUrl": "http://127.0.0.1:1",
                "upstreamProtocol": "chat_completions",
                "authKind": "none",
                "publicModel": "lab-model",
                "upstreamModel": "lab-model",
            }))
            .unwrap(),
        ),
    )
    .await
    .expect("probe returns the verdict")
    .0;
    assert!(!response.ok, "closed port is a failed test verdict");
    let rows = operations(&state);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].action, "provider.test");
    assert_eq!(rows[0].outcome, "failed");
    assert_eq!(rows[0].reason_code.as_deref(), Some("outboundFailed"));
    assert!(rows[0].subject_id.is_none());
    let metadata: Value = serde_json::from_str(&rows[0].metadata_json).unwrap();
    assert_eq!(metadata["revision"].as_u64(), Some(response.revision));
    let text = format!(
        "{} {}",
        rows[0].metadata_json,
        rows[0].reason_code.as_deref().unwrap_or("")
    );
    assert!(!text.contains("127.0.0.1"));
    if let Some(error) = &response.error {
        assert!(!rows[0].metadata_json.contains(error));
    }
    close(dir, state);
}

const BLOCKED: &str = "own-commit-blocked";

fn completed(metadata_json: &str) -> Option<u64> {
    let metadata: Value = serde_json::from_str(metadata_json).unwrap();
    metadata
        .get("completedCount")
        .and_then(|value| value.as_u64())
}

fn provider_name(state: &CoreState, id: &str) -> String {
    state
        .db
        .lock()
        .get_dynamic_provider(id)
        .unwrap()
        .expect("provider")
        .name
}

fn update_body(state: &CoreState, revision: u64, name: &str) -> Bytes {
    Bytes::from(
        serde_json::to_vec(&json!({
            "expectedRevision": revision,
            "processGeneration": state.process_generation(),
            "name": name,
            "endpointUrl": ENDPOINT,
            "upstreamProtocol": "chat_completions",
            "authKind": "none",
            "models": [{ "publicModel": "lab-model", "upstreamModel": "lab-model" }],
        }))
        .unwrap(),
    )
}

#[tokio::test]
async fn update_before_commit_ignores_an_outside_revision_bump() {
    let (dir, state) = open_state("update-before");
    let created = create_provider(
        State(state.clone()),
        create_body(&state, state.settings_revision()),
    )
    .await
    .expect("create")
    .0;
    let id = created.provider.id;
    state
        .db
        .lock()
        .conn
        .execute_batch(
            "CREATE TRIGGER ocg_test_abort_destination_write
             BEFORE UPDATE ON destinations
             BEGIN
               SELECT ocg_test_before_commit();
             END;",
        )
        .unwrap();
    let before = state.settings_revision();
    let state_for_call = state.clone();
    let id_for_call = id.clone();
    let body = update_body(&state, before, "Blocked");
    let error = fail_after_cpa_revision_advance(&state, BLOCKED, async move {
        update_provider(State(state_for_call), Path(id_for_call), body).await
    })
    .expect_err("aborted destination write");
    assert_eq!(error.body.code, "invalidRequest");
    assert!(error.body.message.contains(BLOCKED));
    assert_eq!(provider_name(&state, &id), LABEL);
    assert_eq!(state.settings_revision(), before + 1);
    let row = operations(&state)
        .into_iter()
        .find(|row| row.action == "provider.update")
        .expect("update receipt");
    assert_eq!(row.outcome, "rejected");
    assert_eq!(row.reason_code.as_deref(), Some("invalidRequest"));
    assert_eq!(completed(&row.metadata_json), None);
    assert!(!row.metadata_json.contains(BLOCKED));
    assert!(!row.metadata_json.contains(ENDPOINT));
    close(dir, state);
}

#[tokio::test]
async fn delete_before_commit_ignores_an_outside_revision_bump() {
    let (dir, state) = open_state("delete-before");
    let created = create_provider(
        State(state.clone()),
        Bytes::from(
            serde_json::to_vec(&json!({
                "expectedRevision": state.settings_revision(),
                "processGeneration": state.process_generation(),
                "name": LABEL,
                "endpointUrl": ENDPOINT,
                "upstreamProtocol": "chat_completions",
                "authKind": "api-key",
                "models": [{ "publicModel": "lab-model", "upstreamModel": "lab-model" }],
            }))
            .unwrap(),
        ),
    )
    .await
    .expect("definition without an account")
    .0;
    let id = created.provider.id;
    state
        .db
        .lock()
        .conn
        .execute_batch(
            "CREATE TRIGGER ocg_test_abort_destination_delete
             BEFORE DELETE ON destinations
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
        delete_provider(
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
    .expect_err("aborted delete");
    assert_eq!(error.body.code, "invalidRequest");
    assert!(error.body.message.contains(BLOCKED));
    assert_eq!(provider_name(&state, &id), LABEL);
    assert_eq!(state.settings_revision(), before + 1);
    let row = operations(&state)
        .into_iter()
        .find(|row| row.action == "provider.delete")
        .expect("delete receipt");
    assert_eq!(row.outcome, "rejected");
    assert_eq!(row.reason_code.as_deref(), Some("invalidRequest"));
    assert_eq!(completed(&row.metadata_json), None);
    assert!(!row.metadata_json.contains(BLOCKED));
    close(dir, state);
}

#[tokio::test]
async fn update_publish_after_commit_stays_partial() {
    let (dir, state) = open_state("update-after");
    let created = create_provider(
        State(state.clone()),
        create_body(&state, state.settings_revision()),
    )
    .await
    .expect("create")
    .0;
    let id = created.provider.id;
    state
        .db
        .lock()
        .set_setting(crate::gateway::policy::SETTING_KEY, "{not-json}")
        .unwrap();
    let before = state.settings_revision();
    let error = update_provider(
        State(state.clone()),
        Path(id.clone()),
        update_body(&state, before, "AfterPolicy"),
    )
    .await
    .expect_err("policy publication");
    assert_eq!(error.body.code, "invalidRequest");
    assert!(!error.body.message.is_empty());
    assert_eq!(provider_name(&state, &id), "AfterPolicy");
    assert!(state.settings_revision() > before);
    let row = operations(&state)
        .into_iter()
        .find(|row| row.action == "provider.update")
        .expect("update receipt");
    assert_eq!(row.outcome, "partial");
    assert_eq!(row.reason_code.as_deref(), Some("invalidRequest"));
    let metadata: Value = serde_json::from_str(&row.metadata_json).unwrap();
    assert_eq!(metadata["completedCount"].as_u64(), Some(1));
    assert_eq!(metadata["failedCount"].as_u64(), Some(1));
    assert_eq!(
        metadata["revision"].as_u64(),
        Some(state.settings_revision())
    );
    assert!(!row.metadata_json.contains("{not-json}"));
    assert!(!row.metadata_json.contains(&error.body.message));
    assert!(!row.metadata_json.contains(ENDPOINT));
    close(dir, state);
}

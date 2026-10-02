use super::*;
use crate::crypto::StaticKeyCipher;
use crate::db::Database;
use crate::desktop::DesktopUpdateStartError;
use crate::state::CoreStateInner;
use crate::user_operation::UserOperation;
use axum::body::Bytes;
use axum::extract::State;
use std::sync::Arc;

fn open_state(tag: &str) -> (std::path::PathBuf, CoreState) {
    let dir = std::env::temp_dir().join(format!("ocg-v3-receipt-{tag}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let state = Arc::new(
        CoreStateInner::new(
            Database::open(dir.clone()).unwrap(),
            dir.clone(),
            Arc::new(StaticKeyCipher::new(tag)),
        )
        .unwrap(),
    );
    (dir, state)
}

fn close(dir: std::path::PathBuf, state: CoreState) {
    drop(state);
    std::fs::remove_dir_all(dir).ok();
}

fn receipts(state: &CoreState, action: &str) -> Vec<(String, String, Option<String>, String)> {
    let db = state.db.lock();
    let mut statement = db
        .conn
        .prepare(
            "SELECT operation_id, outcome, reason_code, metadata_json
             FROM operation_logs WHERE action = ?1 ORDER BY rowid",
        )
        .unwrap();
    statement
        .query_map([action], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
}

fn fail_only_when_enabling(enabled: bool) -> crate::Result<()> {
    if enabled {
        anyhow::bail!("secret-should-not-leak");
    }
    Ok(())
}

fn fail_auto_start_both_ways(_: bool) -> crate::Result<()> {
    anyhow::bail!("secret-should-not-leak");
}

#[test]
fn unrelated_revision_keeps_an_atomic_rejection() {
    let (dir, state) = open_state("atomic-revision");
    state.bump_settings_revision();
    let op = open_dashboard(&state, "settings.update", "settings", None);
    let result: Result<(), super::super::V3ApiError> = Err(
        super::super::V3ApiError::invalid_request_at(&state, "rejected on purpose"),
    );
    assert!(record_after(op, &state, &[], (None, None, None), None, result).is_err());
    let rows = receipts(&state, "settings.update");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].1, "rejected");
    assert_eq!(rows[0].2.as_deref(), Some("invalidRequest"));
    assert!(!rows[0].3.contains("partial"));
    close(dir, state);
}

#[tokio::test]
async fn host_hook_compensation_uses_the_typed_rollback_result() {
    let (dir, state) = open_state("host-compensated");
    state.set_auto_start_sync(fail_only_when_enabling);
    let body = Bytes::from(
        serde_json::to_vec(&serde_json::json!({
            "autoStart": true,
            "expectedRevision": state.settings_revision(),
            "processGeneration": state.process_generation(),
        }))
        .unwrap(),
    );
    assert!(put_settings(State(state.clone()), body).await.is_err());
    assert!(!state.config().auto_start);
    let rows = receipts(&state, "settings.update");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].1, "compensated");
    assert_eq!(rows[0].2.as_deref(), Some("hostSyncFailed"));
    assert!(rows[0].3.contains("\"compensated\":true"));
    assert!(!rows[0].3.contains("secret-should-not-leak"));
    assert!(!rows[0].2.as_deref().unwrap().contains("secret"));
    close(dir, state);
}

#[tokio::test]
async fn host_hook_rollback_failure_stays_partial() {
    let (dir, state) = open_state("host-partial");
    state.set_auto_start_sync(fail_auto_start_both_ways);
    let body = Bytes::from(
        serde_json::to_vec(&serde_json::json!({
            "autoStart": true,
            "expectedRevision": state.settings_revision(),
            "processGeneration": state.process_generation(),
        }))
        .unwrap(),
    );
    assert!(put_settings(State(state.clone()), body).await.is_err());
    let rows = receipts(&state, "settings.update");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].1, "partial");
    assert_eq!(rows[0].2.as_deref(), Some("hostSyncFailed"));
    assert!(rows[0].3.contains("\"compensated\":false"));
    assert!(!rows[0].3.contains("secret-should-not-leak"));
    close(dir, state);
}

#[test]
fn cpa_proxy_failure_overrides_port_compensation() {
    let compensated = SettingsReceipt::Compensated {
        reason: "gatewayBindFailed",
    };
    assert!(matches!(
        prefer_proxy_partial(compensated, true),
        SettingsReceipt::Partial {
            reason: CPA_PROXY_SYNC_FAILED
        }
    ));
    let already_partial = SettingsReceipt::Partial {
        reason: "gatewayBindFailed",
    };
    assert!(matches!(
        prefer_proxy_partial(already_partial, true),
        SettingsReceipt::Partial {
            reason: "gatewayBindFailed"
        }
    ));
    assert!(matches!(
        prefer_proxy_partial(SettingsReceipt::Success, false),
        SettingsReceipt::Success
    ));
}

#[test]
fn desktop_update_finishes_the_accepted_operation() {
    let (dir, state) = open_state("desktop-immediate");
    let starter = state.clone();
    state.set_desktop_update_starter(Arc::new(move |_| {
        starter.set_desktop_update_completed();
        Ok(())
    }));
    let op = UserOperation::dashboard(&state, "desktop.update.install", "desktop", None);
    let id = op.operation_id().to_string();
    state
        .start_desktop_update_recorded("9.9.9".to_string(), op)
        .unwrap();
    let rows = receipts(&state, "desktop.update.install");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].0, id);
    assert_eq!(rows[0].1, "success");
    assert_eq!(rows[0].2.as_deref(), Some("installed"));
    close(dir, state);

    let (dir, state) = open_state("desktop-later");
    state.set_desktop_update_starter(Arc::new(|_| Ok(())));
    let op = UserOperation::dashboard(&state, "desktop.update.install", "desktop", None);
    let id = op.operation_id().to_string();
    state
        .start_desktop_update_recorded("9.9.9".to_string(), op)
        .unwrap();
    let pending = receipts(&state, "desktop.update.install");
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].0, id);
    assert_eq!(pending[0].1, "pending");
    state.set_desktop_update_completed();
    let done = receipts(&state, "desktop.update.install");
    assert_eq!(done.len(), 1);
    assert_eq!(done[0].0, id);
    assert_eq!(done[0].1, "success");
    assert_eq!(done[0].2.as_deref(), Some("installed"));
    close(dir, state);

    let (dir, state) = open_state("desktop-start-failed");
    state.set_desktop_update_starter(Arc::new(|_| anyhow::bail!("secret-should-not-leak")));
    let op = UserOperation::dashboard(&state, "desktop.update.install", "desktop", None);
    let id = op.operation_id().to_string();
    assert!(matches!(
        state.start_desktop_update_recorded("9.9.9".to_string(), op),
        Err(DesktopUpdateStartError::Starter(_))
    ));
    let rows = receipts(&state, "desktop.update.install");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].0, id);
    assert_eq!(rows[0].1, "failed");
    assert_eq!(rows[0].2.as_deref(), Some("updateStartFailed"));
    assert!(!rows[0].3.contains("secret-should-not-leak"));
    close(dir, state);

    let (dir, state) = open_state("desktop-unsupported");
    let op = UserOperation::dashboard(&state, "desktop.update.install", "desktop", None);
    let id = op.operation_id().to_string();
    assert!(matches!(
        state.start_desktop_update_recorded("9.9.9".to_string(), op),
        Err(DesktopUpdateStartError::Unsupported)
    ));
    let rows = receipts(&state, "desktop.update.install");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].0, id);
    assert_eq!(rows[0].1, "rejected");
    assert_eq!(rows[0].2.as_deref(), Some("updateUnsupported"));
    close(dir, state);
}

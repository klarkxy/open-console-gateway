use super::*;
use crate::{crypto::StaticKeyCipher, db::Database, state::CoreStateInner};
use std::sync::Arc;

#[test]
fn read_snapshot_does_not_block_mutations_and_keeps_secret_and_receipt_views_consistent() {
    let dir = std::env::temp_dir().join(format!("ocg-log-snapshot-{}", uuid::Uuid::new_v4()));
    let state = Arc::new(
        CoreStateInner::new(
            Database::open(dir.clone()).unwrap(),
            dir.clone(),
            Arc::new(StaticKeyCipher::new("log-snapshot")),
        )
        .unwrap(),
    );
    let conn = read_connection(&state).unwrap();
    let snapshot = conn.unchecked_transaction().unwrap();
    let _secrets = account_secrets(&state, &snapshot).unwrap();
    assert_eq!(
        crate::db::operation_logs::query_operation_logs_on(
            &snapshot,
            &OperationLogQuery::default()
        )
        .unwrap()
        .total,
        0
    );
    let mut config = state.config();
    config.gateway_port += 1;
    state.set_config(config.clone()).unwrap();
    crate::user_operation::UserOperation::dashboard(&state, "settings.update", "settings", None)
        .complete(
            crate::log_types::OperationOutcome::Success,
            None,
            Default::default(),
        );
    // The independent writer committed while this read transaction remained
    // open; repeated reads still see the same snapshot.
    assert_eq!(
        crate::db::operation_logs::query_operation_logs_on(
            &snapshot,
            &OperationLogQuery::default()
        )
        .unwrap()
        .total,
        0
    );
    assert!(snapshot.execute("DELETE FROM operation_logs", []).is_err());
    drop(snapshot);
    assert_eq!(
        read_operation_logs(&state, &OperationLogQuery::default())
            .unwrap()
            .total,
        1
    );
    assert_eq!(state.config().gateway_port, config.gateway_port);
    drop(conn);
    drop(state);
    std::fs::remove_dir_all(dir).unwrap();
}

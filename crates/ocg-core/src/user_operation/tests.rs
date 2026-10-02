use super::*;
use crate::{crypto::StaticKeyCipher, db::Database, state::CoreStateInner};
use std::{path::PathBuf, sync::Arc};

struct Fixture {
    state: Option<CoreState>,
    dir: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("ocg-operation-{}", uuid::Uuid::new_v4()));
        let db = Database::open(dir.clone()).unwrap();
        let state = Arc::new(
            CoreStateInner::new(db, dir.clone(), Arc::new(StaticKeyCipher::new("fixture")))
                .unwrap(),
        );
        Self {
            state: Some(state),
            dir,
        }
    }

    fn state(&self) -> &CoreState {
        self.state.as_ref().unwrap()
    }

    fn receipts(&self) -> Vec<OperationLog> {
        self.state()
            .db
            .lock()
            .query_operation_logs(&crate::log_types::OperationLogQuery::default())
            .unwrap()
            .items
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // Database handles must close before Windows removes their directory.
        drop(self.state.take());
        std::fs::remove_dir_all(&self.dir).unwrap();
    }
}

#[test]
fn committed_operation_records_generated_subject_and_truthful_receipt() {
    let fixture = Fixture::new();
    let mut operation =
        UserOperation::dashboard(fixture.state(), "settings.update", "settings", None);
    assert!(fixture.receipts().is_empty());
    let mut config = fixture.state().config();
    config.gateway_port += 1;
    fixture.state().set_config(config.clone()).unwrap();
    operation.subject("local-settings");
    operation.complete(
        OperationOutcome::Partial,
        Some("cpaProxySyncFailed"),
        OperationMetadata {
            changed_fields: vec!["gateway_port".into()],
            revision: Some(fixture.state().settings_revision()),
            ..Default::default()
        },
    );
    let rows = fixture.receipts();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].subject_id.as_deref(), Some("local-settings"));
    assert_eq!(rows[0].outcome, OperationOutcome::Partial);
    assert!(rows[0].completed_at.is_some());
    assert_eq!(fixture.state().config().gateway_port, config.gateway_port);
}

#[test]
fn queued_operation_finishes_the_same_row_once() {
    let fixture = Fixture::new();
    let mut operation = UserOperation::dashboard(
        fixture.state(),
        "account.verify",
        "account",
        Some("account-id".into()),
    );
    operation.accepted(Default::default());
    let pending = fixture.receipts();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].outcome, OperationOutcome::Pending);
    operation.complete(
        OperationOutcome::Failed,
        Some("verificationFailed"),
        Default::default(),
    );
    let done = fixture.receipts();
    assert_eq!(done.len(), 1);
    assert_eq!(done[0].operation_id, pending[0].operation_id);
    assert_eq!(done[0].outcome, OperationOutcome::Failed);
}

#[test]
fn receipt_failure_keeps_committed_effect_and_can_recover_at_job_completion() {
    let fixture = Fixture::new();
    let mut config = fixture.state().config();
    config.gateway_port += 1;
    fixture.state().set_config(config.clone()).unwrap();
    let revision = fixture.state().settings_revision();
    fixture
        .state()
        .db
        .lock()
        .conn
        .execute_batch("ALTER TABLE operation_logs RENAME TO unavailable_operation_logs")
        .unwrap();
    UserOperation::dashboard(fixture.state(), "settings.update", "settings", None).complete(
        OperationOutcome::Success,
        None,
        Default::default(),
    );
    let mut queued = UserOperation::dashboard(
        fixture.state(),
        "account.verify",
        "account",
        Some("account-id".into()),
    );
    queued.accepted(Default::default());
    fixture
        .state()
        .db
        .lock()
        .conn
        .execute_batch("ALTER TABLE unavailable_operation_logs RENAME TO operation_logs")
        .unwrap();
    queued.complete(OperationOutcome::Success, None, Default::default());
    assert_eq!(fixture.state().settings_revision(), revision);
    assert_eq!(fixture.state().config().gateway_port, config.gateway_port);
    assert_eq!(fixture.receipts().len(), 1);
    assert_eq!(fixture.receipts()[0].outcome, OperationOutcome::Success);
}

#[test]
fn atomic_result_classifies_business_codes() {
    let fixture = Fixture::new();
    let rejected: Result<(), _> = Err(crate::dashboard_v3::V3ApiError::invalid_request_at(
        fixture.state(),
        "invalid input",
    ));
    UserOperation::dashboard(fixture.state(), "key.update", "key", Some("key-id".into()))
        .result(&rejected);
    let failed: Result<(), _> = Err(crate::dashboard_v3::V3ApiError::internal(
        "persistence failed",
    ));
    UserOperation::dashboard(fixture.state(), "key.update", "key", Some("key-id".into()))
        .result(&failed);
    let rows = fixture.receipts();
    assert!(
        rows.iter()
            .any(|row| row.outcome == OperationOutcome::Rejected
                && row.reason_code.as_deref() == Some("invalidRequest"))
    );
    assert!(
        rows.iter()
            .any(|row| row.outcome == OperationOutcome::Failed
                && row.reason_code.as_deref() == Some("internal"))
    );
}

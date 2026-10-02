use super::*;
use serde_json::json;

#[test]
fn install_defaults_to_an_application_key_without_a_picker() {
    let request =
        json!({"expectedRevision": 1, "processGeneration": 1, "expectedFingerprint": "fp"});
    let parsed = parse_dsh_mutation::<DshInstallMutationCheck, DshApplicationInstallRequest>(
        &serde_json::to_vec(&request).unwrap(),
    )
    .unwrap();
    assert!(parsed.key_id.is_none());
}

#[tokio::test]
async fn dsh_failed_install_creates_one_named_key_and_retry_reuses_it() {
    use crate::{crypto::StaticKeyCipher, db::Database, state::CoreStateInner};
    use std::sync::{Arc, Mutex};
    let dir = std::env::temp_dir().join(format!("ocg-dsh-auto-key-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let state = Arc::new(
        CoreStateInner::new(
            Database::open(dir.clone()).unwrap(),
            dir.clone(),
            Arc::new(StaticKeyCipher::new("dsh-auto-key")),
        )
        .unwrap(),
    );
    let observed = Arc::new(Mutex::new(Vec::new()));
    let capture = observed.clone();
    state.set_dsh_application_host(Arc::new(move |request| {
        if let DshApplicationHostRequest::Install { secret, .. } = request {
            capture
                .lock()
                .unwrap()
                .push(secret.expose_to_host().to_owned());
        }
        Err(DshApplicationError::internal("fixture disk write failed"))
    }));
    let revision = state.settings_revision();
    for _ in 0..2 {
        let body = json!({"expectedRevision":state.settings_revision(),
            "processGeneration":state.process_generation(), "expectedFingerprint":"fp"});
        let error = install_dsh(
            State(state.clone()),
            Bytes::from(serde_json::to_vec(&body).unwrap()),
        )
        .await
        .unwrap_err();
        use axum::response::IntoResponse;
        let response = error.into_response();
        assert_eq!(
            response.status(),
            axum::http::StatusCode::INTERNAL_SERVER_ERROR
        );
        let bytes = axum::body::to_bytes(response.into_body(), 65536)
            .await
            .unwrap();
        let error: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(error["currentRevision"], revision + 1);
        assert_eq!(error["processGeneration"], state.process_generation());
    }
    let keys = state.db.lock().list_active_sub_gateway_keys().unwrap();
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0].name, "dsh");
    assert_eq!(state.settings_revision(), revision + 1);
    assert_eq!(
        *observed.lock().unwrap(),
        vec![keys[0].key.clone(), keys[0].key.clone()]
    );
    let rows = operation_receipts(&state);
    assert_eq!(rows.len(), 2);
    let partial = rows
        .iter()
        .find(|row| row.outcome == OperationOutcome::Partial)
        .unwrap();
    let failed = rows
        .iter()
        .find(|row| row.outcome == OperationOutcome::Failed)
        .unwrap();
    assert_eq!(partial.action, "application.install");
    assert_eq!(partial.reason_code.as_deref(), Some("internal"));
    assert_eq!(partial.metadata.related_ids, vec![keys[0].id.clone()]);
    assert_eq!(partial.metadata.revision, Some(revision + 1));
    assert_eq!(partial.metadata.completed_count, Some(1));
    assert_eq!(partial.metadata.failed_count, Some(1));
    assert_eq!(failed.action, "application.install");
    assert_eq!(failed.reason_code.as_deref(), Some("internal"));
    assert!(failed.metadata.related_ids.is_empty());
    let encoded = serde_json::to_string(&rows).unwrap();
    assert!(!encoded.contains(&keys[0].key));
    drop(state);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn install_and_uninstall_reject_unknown_fields() {
    let extra = json!({
        "expectedRevision": 1,
        "processGeneration": 1,
        "keyId": "primary",
        "expectedFingerprint": "abc",
        "extra": true
    });
    assert!(
        parse_dsh_mutation::<DshInstallMutationCheck, DshApplicationInstallRequest>(
            &serde_json::to_vec(&extra).unwrap()
        )
        .is_err()
    );
    let with_key = json!({
        "expectedRevision": 1,
        "processGeneration": 1,
        "expectedFingerprint": "abc",
        "keyId": "must-not-be-accepted"
    });
    assert!(
        parse_dsh_mutation::<DshUninstallMutationCheck, DshApplicationUninstallRequest>(
            &serde_json::to_vec(&with_key).unwrap()
        )
        .is_err()
    );
    let valid = json!({
        "expectedRevision": 1,
        "processGeneration": 1,
        "expectedFingerprint": "abc",
        "runtimeUrl": "http://127.0.0.1:3080"
    });
    assert!(
        parse_dsh_mutation::<DshUninstallMutationCheck, DshApplicationUninstallRequest>(
            &serde_json::to_vec(&valid).unwrap()
        )
        .is_ok()
    );
}

#[test]
fn persistable_reason_splits_camel_case_and_keeps_rejected_classes() {
    assert_eq!(persistable_reason("invalidJson"), "invalid.json");
    assert_eq!(persistable_reason("revisionConflict"), "revision.conflict");
    assert_eq!(
        persistable_reason("preconditionFailed"),
        "precondition.failed"
    );
    assert_eq!(persistable_reason("internal"), "internal");
    assert_eq!(persistable_reason("outboundFailed"), "outbound.failed");
    assert_eq!(persistable_reason("not a code"), "failed");
    assert_eq!(classify_reason("invalidJson"), OperationOutcome::Rejected);
    assert_eq!(classify_reason("throttled"), OperationOutcome::Rejected);
    assert_eq!(classify_reason("internal"), OperationOutcome::Failed);
    assert_eq!(classify_reason("outboundFailed"), OperationOutcome::Failed);
    assert_eq!(batch_outcome(0, 0), OperationOutcome::Success);
    assert_eq!(batch_outcome(2, 0), OperationOutcome::Success);
    assert_eq!(batch_outcome(0, 3), OperationOutcome::Failed);
    assert_eq!(batch_outcome(1, 1), OperationOutcome::Partial);
    assert!(opaque_subject("sk-live").is_none());
    assert!(opaque_subject("acct-1").is_some());
    assert!(!field_name_ok("api_key"));
    assert!(field_name_ok("published"));
}

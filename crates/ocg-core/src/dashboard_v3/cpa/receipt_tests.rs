use super::tests::{action_receipts, hold_readable_no_delete, mutation_bytes, test_state};
use super::*;
use crate::cpa_runtime::{CpaRuntimeLogTail, CpaRuntimeProcessHost, CpaRuntimeProcessSpec};
use axum::http::StatusCode;
use serde_json::json;
use std::sync::Arc;

struct StoppedHost;
impl CpaRuntimeProcessHost for StoppedHost {
    fn start_owned(&self, _: &CpaRuntimeProcessSpec) -> Result<(), CpaRuntimeError> {
        unreachable!()
    }
    fn stop_owned(&self) -> Result<(), CpaRuntimeError> {
        Ok(())
    }
    fn owned_running(&self) -> bool {
        false
    }
    fn logs(&self) -> CpaRuntimeLogTail {
        CpaRuntimeLogTail {
            stdout: String::new(),
            stderr: String::new(),
        }
    }
    fn add_log_secret(&self, _: &cpa_runtime::CpaRuntimeSecret) {}
}

#[tokio::test]
async fn partial_first_directory_delete_keeps_http_error_and_partial_receipt() {
    let (dir, state) = test_state("remove-partial-first-directory");
    let root = cpa_runtime::runtime_dir(&dir);
    let versions = root.join("versions");
    std::fs::create_dir_all(&versions).unwrap();
    let removed = versions.join("a-removable");
    let blocked = versions.join("z-locked");
    std::fs::write(&removed, b"owned").unwrap();
    std::fs::write(&blocked, b"owned").unwrap();
    cpa_runtime::save_managed(
        &dir,
        &cpa_runtime::ManagedCpa {
            current_version: "7.2.147".into(),
            previous_version: None,
            asset_sha256: "a".repeat(64),
            port: 8317,
            desired_running: false,
        },
    )
    .unwrap();
    state
        .persist_managed_connection(
            8317,
            "management-key",
            "inference-key",
            vec!["model".into()],
        )
        .unwrap();
    state.set_cpa_runtime_host(Arc::new(StoppedHost));
    let held = hold_readable_no_delete(&blocked);
    let error = remove_runtime(State(state.clone()), mutation_bytes(&state, json!({})))
        .await
        .expect_err("second child is locked after the first was deleted");
    assert_eq!(error.status, StatusCode::BAD_GATEWAY);
    assert_eq!(error.body.code, super::super::types::ERROR_OUTBOUND_FAILED);
    assert!(error.body.message.contains("CPA runtime file error"));
    assert!(!removed.exists());
    assert!(blocked.is_file());
    let rows = action_receipts(&state, "cpa.runtime.remove");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].0, "partial");
    assert_eq!(rows[0].1.as_deref(), Some("outboundFailed"));
    assert!(rows[0].2.contains("\"compensated\":false"));
    drop(held);
    drop(state);
    std::fs::remove_dir_all(dir).unwrap();
}

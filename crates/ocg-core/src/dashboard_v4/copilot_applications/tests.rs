use super::*;
use crate::{crypto::StaticKeyCipher, db::Database, state::CoreStateInner};
use serde_json::json;
use std::sync::{Arc, Mutex};
fn state() -> (std::path::PathBuf, CoreState) {
    let dir = std::env::temp_dir().join(format!("ocg-copilot-api-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&dir).unwrap();
    let state = Arc::new(
        CoreStateInner::new(
            Database::open(dir.clone()).unwrap(),
            dir.clone(),
            Arc::new(StaticKeyCipher::new("synthetic")),
        )
        .unwrap(),
    );
    (dir, state)
}
fn ready() -> CopilotInspection {
    let mut i = CopilotInspection::unsupported(CopilotTarget::default());
    i.status = CopilotStatus::Ready;
    i.install_supported = true;
    i.fingerprint = Some("fp".into());
    i
}
fn body(state: &CoreState) -> serde_json::Value {
    json!({"expectedRevision":state.settings_revision(),"processGeneration":state.process_generation(),"target":{},"expectedFingerprint":"fp"})
}
#[tokio::test]
async fn unsupported_and_stale_cas_never_create_keys() {
    let (dir, state) = state();
    assert!(
        install(
            State(state.clone()),
            Bytes::from(serde_json::to_vec(&body(&state)).unwrap())
        )
        .await
        .is_err()
    );
    assert!(
        state
            .db
            .lock()
            .list_active_sub_gateway_keys()
            .unwrap()
            .is_empty()
    );
    let calls = Arc::new(Mutex::new(0));
    let capture = calls.clone();
    state.set_copilot_application_host(Arc::new(move |_| {
        *capture.lock().unwrap() += 1;
        Ok(ready())
    }));
    let mut input = body(&state);
    input["processGeneration"] = json!(state.process_generation() + 1);
    assert!(
        install(
            State(state.clone()),
            Bytes::from(serde_json::to_vec(&input).unwrap())
        )
        .await
        .is_err()
    );
    assert_eq!(*calls.lock().unwrap(), 0);
    assert!(
        state
            .db
            .lock()
            .list_active_sub_gateway_keys()
            .unwrap()
            .is_empty()
    );
    drop(state);
    std::fs::remove_dir_all(dir).unwrap();
}
#[tokio::test]
async fn stale_target_preflight_never_creates_key() {
    let (dir, state) = state();
    state.set_copilot_application_host(Arc::new(|_| Ok(ready())));
    let mut input = body(&state);
    input["expectedFingerprint"] = json!("stale");
    assert!(
        install(
            State(state.clone()),
            Bytes::from(serde_json::to_vec(&input).unwrap())
        )
        .await
        .is_err()
    );
    assert!(
        state
            .db
            .lock()
            .list_active_sub_gateway_keys()
            .unwrap()
            .is_empty()
    );
    drop(state);
    std::fs::remove_dir_all(dir).unwrap();
}
#[tokio::test]
async fn default_key_is_reused_and_never_returned() {
    let (dir, state) = state();
    let seen = Arc::new(Mutex::new(vec![]));
    let capture = seen.clone();
    state.set_copilot_application_host(Arc::new(move |request| {
        if let CopilotApplicationHostRequest::Install { secret, .. } = request {
            capture
                .lock()
                .unwrap()
                .push(secret.expose_to_host().to_owned());
        }
        Ok(ready())
    }));
    for _ in 0..2 {
        let result = install(
            State(state.clone()),
            Bytes::from(serde_json::to_vec(&body(&state)).unwrap()),
        )
        .await
        .unwrap();
        let keys = state.db.lock().list_active_sub_gateway_keys().unwrap();
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].name, "copilot");
        assert!(
            !serde_json::to_string(&result.0)
                .unwrap()
                .contains(&keys[0].key)
        );
    }
    let values = seen.lock().unwrap();
    assert_eq!(values.len(), 2);
    assert_eq!(values[0], values[1]);
    drop(values);
    drop(state);
    std::fs::remove_dir_all(dir).unwrap();
}
#[tokio::test]
async fn unknown_fields_fail_before_host_or_key_effects() {
    let (dir, state) = state();
    let calls = Arc::new(Mutex::new(0));
    let capture = calls.clone();
    state.set_copilot_application_host(Arc::new(move |_| {
        *capture.lock().unwrap() += 1;
        Ok(ready())
    }));
    let mut input = body(&state);
    input["secret"] = json!("unexpected");
    assert!(
        install(
            State(state.clone()),
            Bytes::from(serde_json::to_vec(&input).unwrap())
        )
        .await
        .is_err()
    );
    assert_eq!(*calls.lock().unwrap(), 0);
    assert!(
        state
            .db
            .lock()
            .list_active_sub_gateway_keys()
            .unwrap()
            .is_empty()
    );
    drop(state);
    std::fs::remove_dir_all(dir).unwrap();
}

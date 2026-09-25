use super::*;
use crate::{crypto::StaticKeyCipher, db::Database, state::CoreStateInner};
use axum::response::IntoResponse;
use serde_json::json;
use std::{path::PathBuf, sync::Arc};

struct Fixture {
    state: CoreState,
    dir: PathBuf,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.dir).ok();
    }
}
fn fixture() -> Fixture {
    let dir = std::env::temp_dir().join(format!("ocg-temporary-policy-{}", uuid::Uuid::new_v4()));
    let db = Database::open(dir.clone()).unwrap();
    let state = Arc::new(
        CoreStateInner::new(
            db,
            dir.clone(),
            Arc::new(StaticKeyCipher::new("policy-test")),
        )
        .unwrap(),
    );
    Fixture { state, dir }
}
fn body(state: &CoreState, rules: serde_json::Value) -> Bytes {
    Bytes::from(
        serde_json::to_vec(&json!({"expectedRevision":state.settings_revision(),
        "processGeneration":state.process_generation(), "rules":rules}))
        .unwrap(),
    )
}
#[tokio::test]
async fn get_is_local_and_put_requires_cas_and_preserves_config_on_invalid_input() {
    let f = fixture();
    let state = &f.state;
    let revision = state.settings_revision();
    let initial = get(State(state.clone())).await.unwrap().0;
    assert_eq!(initial.rules.len(), 1);
    assert!(initial.waits.is_empty());
    assert_eq!(state.settings_revision(), revision);
    let mut rules = initial.rules;
    rules[0].initial_seconds = 60;
    let input = body(state, json!(rules));
    let updated = put(State(state.clone()), input.clone()).await.unwrap().0;
    assert_eq!(updated.rules, rules);
    assert_eq!(
        put(State(state.clone()), input)
            .await
            .unwrap_err()
            .into_response()
            .status(),
        axum::http::StatusCode::CONFLICT
    );
    let before = state.settings_revision();
    assert!(
        put(State(state.clone()), body(state, json!([])))
            .await
            .is_err()
    );
    assert_eq!(state.settings_revision(), before);
    assert_eq!(snapshot(state).unwrap().rules, rules);
    assert!(reset(State(state.clone()), Path("stale:1".into()),
        Bytes::from(serde_json::to_vec(&json!({"expectedRevision":before,"processGeneration":state.process_generation()})).unwrap())).await.is_err());
    assert_eq!(snapshot(state).unwrap().rules, rules);
}

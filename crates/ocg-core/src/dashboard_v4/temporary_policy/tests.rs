use super::*;
use crate::crypto::{KeyCipher, StaticKeyCipher};
use crate::dashboard_v3::MutationExpectation;
use crate::db::Database;
use crate::gateway::policy::{SETTING_KEY, persist_configured_rules};
use crate::state::CoreStateInner;
use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, State};
use serde_json::json;
use std::sync::Arc;

struct StateDir {
    state: Option<CoreState>,
    dir: Option<std::path::PathBuf>,
}

impl std::ops::Deref for StateDir {
    type Target = CoreState;
    fn deref(&self) -> &Self::Target {
        self.state.as_ref().unwrap()
    }
}

impl Drop for StateDir {
    fn drop(&mut self) {
        self.state.take();
        if let Some(dir) = self.dir.take() {
            std::fs::remove_dir_all(dir).ok();
        }
    }
}

fn fresh() -> StateDir {
    let dir = std::env::temp_dir().join(format!(
        "ocg-v4-temporary-policy-{}",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let db = Database::open(dir.clone()).unwrap();
    let cipher: Arc<dyn KeyCipher + Send + Sync> =
        Arc::new(StaticKeyCipher::new("temporary-policy"));
    StateDir {
        state: Some(Arc::new(
            CoreStateInner::new(db, dir.clone(), cipher).unwrap(),
        )),
        dir: Some(dir),
    }
}

fn expectation(state: &CoreState) -> MutationExpectation {
    MutationExpectation {
        expected_revision: state.settings_revision(),
        process_generation: state.process_generation(),
    }
}

fn update_bytes(state: &CoreState, rules: serde_json::Value) -> Bytes {
    let mut body = json!({
        "expectedRevision": expectation(state).expected_revision,
        "processGeneration": expectation(state).process_generation,
        "rules": rules,
    });
    if let Some(object) = body.as_object_mut() {
        object.insert("expectedRevision".into(), json!(state.settings_revision()));
        object.insert(
            "processGeneration".into(),
            json!(state.process_generation()),
        );
    }
    Bytes::from(serde_json::to_vec(&body).unwrap())
}

#[tokio::test]
async fn get_returns_builtin_catalog_and_empty_rules() {
    let state = fresh();
    let Json(config) = get_configuration(State(state.clone())).await.unwrap();
    assert!(config.rules.is_empty());
    assert_eq!(config.builtins.len(), 1);
    assert_eq!(config.builtins[0].id, GOAT_CREDITS_REJECTION_RULE);
    let Json(listed) = get_restrictions(State(state.clone())).await.unwrap();
    assert!(listed.restrictions.is_empty());
}

#[tokio::test]
async fn put_rejects_unknown_destination_without_writing() {
    let state = fresh();
    let before = state.db.lock().get_setting(SETTING_KEY).unwrap();
    let body = update_bytes(
        &state,
        json!([{
            "kind": "custom",
            "id": "status-400",
            "destinationId": "missing-dest",
            "enabled": true,
            "scope": "credential",
            "match": { "statusCodes": [400] },
            "backoff": { "initialSeconds": 30, "maxSeconds": 300 }
        }]),
    );
    let error = put_configuration(State(state.clone()), body).await;
    assert!(error.is_err());
    assert_eq!(state.db.lock().get_setting(SETTING_KEY).unwrap(), before);
}

#[tokio::test]
async fn put_cas_conflict_does_not_write() {
    let state = fresh();
    let mut body = json!({
        "expectedRevision": 0,
        "processGeneration": state.process_generation(),
        "rules": []
    });
    let bytes = Bytes::from(serde_json::to_vec(&body).unwrap());
    let error = put_configuration(State(state.clone()), bytes).await;
    assert!(error.is_err());
    body["expectedRevision"] = json!(state.settings_revision());
    body["processGeneration"] = json!(0);
    let bytes = Bytes::from(serde_json::to_vec(&body).unwrap());
    let error = put_configuration(State(state.clone()), bytes).await;
    assert!(error.is_err());
}

#[tokio::test]
async fn clear_absent_id_is_idempotent() {
    let state = fresh();
    let body = Bytes::from(
        serde_json::to_vec(&json!({
            "expectedRevision": state.settings_revision(),
            "processGeneration": state.process_generation(),
        }))
        .unwrap(),
    );
    let Json(listed) = clear_restriction(State(state.clone()), Path("tp-missing".into()), body)
        .await
        .unwrap();
    assert!(listed.restrictions.is_empty());
}

#[test]
fn invalid_saved_document_fails_construct() {
    let dir = std::env::temp_dir().join(format!(
        "ocg-v4-temporary-policy-bad-{}",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let db = Database::open(dir.clone()).unwrap();
    db.set_setting(SETTING_KEY, "{not-json}").unwrap();
    let cipher: Arc<dyn KeyCipher + Send + Sync> =
        Arc::new(StaticKeyCipher::new("temporary-policy"));
    let error = match CoreStateInner::new(db, dir.clone(), cipher) {
        Ok(_) => panic!("invalid temporary_unavailability_v1 must fail construct"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("temporary_unavailability_v1"));
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn persist_round_trip_keeps_custom_rule() {
    let state = fresh();
    let rules = vec![ConfiguredRule::Custom {
        id: "status-500".into(),
        destination_id: None,
        enabled: true,
        scope: crate::gateway::policy::RestrictionScope::Credential,
        matcher: CustomMatch {
            status_codes: vec![500],
            error_codes: Vec::new(),
            error_types: Vec::new(),
            message_contains: Vec::new(),
        },
        backoff: PolicyBackoff {
            initial_secs: 12,
            max_secs: 40,
        },
    }];
    persist_configured_rules(&state.db.lock(), &rules).unwrap();
    let loaded = load_configured_rules(&state.db.lock()).unwrap();
    assert_eq!(loaded, rules);
}

#[test]
fn strip_destination_rules_leaves_global_rules() {
    let state = fresh();
    let dest = destination_ids(&state.db.lock())
        .unwrap()
        .into_iter()
        .next()
        .expect("builtin destination");
    persist_configured_rules(
        &state.db.lock(),
        &[
            ConfiguredRule::Custom {
                id: "global-500".into(),
                destination_id: None,
                enabled: true,
                scope: crate::gateway::policy::RestrictionScope::Credential,
                matcher: CustomMatch {
                    status_codes: vec![500],
                    error_codes: Vec::new(),
                    error_types: Vec::new(),
                    message_contains: Vec::new(),
                },
                backoff: PolicyBackoff::default(),
            },
            ConfiguredRule::Custom {
                id: "dest-400".into(),
                destination_id: Some(dest.clone()),
                enabled: true,
                scope: crate::gateway::policy::RestrictionScope::Credential,
                matcher: CustomMatch {
                    status_codes: vec![400],
                    error_codes: Vec::new(),
                    error_types: Vec::new(),
                    message_contains: Vec::new(),
                },
                backoff: PolicyBackoff::default(),
            },
        ],
    )
    .unwrap();
    crate::gateway::policy::strip_destination_rules(&state.db.lock(), &dest).unwrap();
    let loaded = load_configured_rules(&state.db.lock()).unwrap();
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].id(), "global-500");
}

#[tokio::test]
async fn put_raw_destination_id_stays_destination_specific_and_rejects_unknown_fields() {
    let state = fresh();
    let dest_a = destination_ids(&state.db.lock())
        .unwrap()
        .into_iter()
        .next()
        .expect("builtin destination");
    let Json(committed) = put_configuration(
        State(state.clone()),
        update_bytes(
            &state,
            json!([
                {
                    "kind": "custom",
                    "id": "rule-a",
                    "destinationId": dest_a,
                    "enabled": true,
                    "scope": "credential",
                    "match": { "statusCodes": [400] },
                    "backoff": { "initialSeconds": 30, "maxSeconds": 300 }
                },
                {
                    "kind": "custom",
                    "id": "rule-b",
                    "destinationId": null,
                    "enabled": true,
                    "scope": "credential",
                    "match": { "statusCodes": [500] },
                    "backoff": { "initialSeconds": 30, "maxSeconds": 300 }
                }
            ]),
        ),
    )
    .await
    .unwrap();
    let raw = serde_json::to_value(&committed).unwrap();
    let rules = raw["rules"].as_array().unwrap();
    let a = rules
        .iter()
        .find(|rule| rule["id"] == "rule-a")
        .expect("rule-a");
    let b = rules
        .iter()
        .find(|rule| rule["id"] == "rule-b")
        .expect("rule-b");
    assert_eq!(a["destinationId"], dest_a);
    assert!(a.get("destination_id").is_none());
    assert_eq!(b["destinationId"], serde_json::Value::Null);
    let unknown = put_configuration(
        State(state.clone()),
        update_bytes(
            &state,
            json!([{
                "kind": "custom",
                "id": "rule-a",
                "destinationId": dest_a,
                "enabled": true,
                "scope": "credential",
                "match": { "statusCodes": [400] },
                "backoff": { "initialSeconds": 30, "maxSeconds": 300 },
                "extra": true
            }]),
        ),
    )
    .await;
    assert!(unknown.is_err());
}

#[tokio::test]
async fn put_rejects_each_supplied_empty_match_field_with_other_without_write() {
    let state = fresh();
    let Json(seeded) = put_configuration(
        State(state.clone()),
        update_bytes(
            &state,
            json!([{
                "kind": "custom",
                "id": "kept",
                "destinationId": null,
                "enabled": true,
                "scope": "credential_model",
                "match": { "statusCodes": [400] },
                "backoff": { "initialSeconds": 30, "maxSeconds": 300 }
            }]),
        ),
    )
    .await
    .unwrap();
    let stored = state.db.lock().get_setting(SETTING_KEY).unwrap();
    let matches = [
        json!({ "statusCodes": [], "errorCodes": ["X"] }),
        json!({ "errorCodes": [], "statusCodes": [400] }),
        json!({ "errorTypes": [], "statusCodes": [400] }),
        json!({ "messageContains": [], "statusCodes": [400] }),
    ];
    for matcher in matches {
        let rejected = put_configuration(
            State(state.clone()),
            update_bytes(
                &state,
                json!([{
                    "kind": "custom",
                    "id": "empty-with-other",
                    "destinationId": null,
                    "enabled": true,
                    "scope": "credential_model",
                    "match": matcher,
                    "backoff": { "initialSeconds": 30, "maxSeconds": 300 }
                }]),
            ),
        )
        .await;
        assert!(rejected.is_err(), "{matcher}");
        let Json(after) = get_configuration(State(state.clone())).await.unwrap();
        assert_eq!(after.revision, seeded.revision, "{matcher}");
        assert_eq!(after.rules, seeded.rules, "{matcher}");
        assert_eq!(
            state.db.lock().get_setting(SETTING_KEY).unwrap(),
            stored,
            "{matcher}"
        );
    }
}

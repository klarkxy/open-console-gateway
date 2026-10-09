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
    let global = config
        .effective_views
        .iter()
        .find(|view| view.destination_id.is_none())
        .unwrap();
    assert_eq!(global.rules.len(), 1);
    assert!(global.rules[0].applicable);
    assert!(!global.rules[0].overridden);
    {
        let db = state.db.lock();
        for view in config
            .effective_views
            .iter()
            .filter(|view| view.destination_id.is_some())
        {
            let adapter: String = db
                .conn
                .query_row(
                    "SELECT adapter FROM destinations WHERE id = ?1",
                    [view.destination_id.as_ref().unwrap()],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(view.rules[0].applicable, adapter == "goat");
        }
    }
    let Json(listed) = get_restrictions(State(state.clone())).await.unwrap();
    assert!(listed.restrictions.is_empty());
    assert!(super::super::applications::operation_receipts(&state).is_empty());
}

#[tokio::test]
async fn read_and_write_receipts_project_local_masks_and_restore_global_rules() {
    let state = fresh();
    let dest = destination_ids(&state.db.lock())
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    let global = json!({
        "kind": "custom", "id": "shared", "destinationId": null, "enabled": true,
        "scope": "credential", "match": { "statusCodes": [503] },
        "backoff": { "initialSeconds": 12, "maxSeconds": 40 }
    });
    let mask = json!({
        "kind": "custom", "id": "shared", "destinationId": dest, "enabled": false,
        "scope": "credential_model", "match": { "errorCodes": ["local"] },
        "backoff": { "initialSeconds": 20, "maxSeconds": 80 }
    });
    let Json(saved) = put_configuration(
        State(state.clone()),
        update_bytes(&state, json!([global, mask])),
    )
    .await
    .unwrap();
    let view = saved
        .effective_views
        .iter()
        .find(|view| view.destination_id.as_deref() == Some(&dest))
        .unwrap();
    let row = view
        .rules
        .iter()
        .find(|row| matches!(&row.rule, TemporaryPolicyRule::Custom { id, .. } if id == "shared"))
        .unwrap();
    assert_eq!(row.origin, TemporaryPolicyRuleOrigin::Local);
    assert_eq!(row.source, TemporaryPolicySource::Connection);
    assert_eq!(row.scope, TemporaryPolicyScope::CredentialModel);
    assert_eq!(row.backoff.initial_seconds, 20);
    assert!(row.overridden && row.applicable);
    assert!(matches!(
        row.rule,
        TemporaryPolicyRule::Custom { enabled: false, .. }
    ));
    assert!(
        !state
            .recovery
            .policy_snapshot()
            .effective_for(&dest)
            .iter()
            .any(|rule| rule.source.rule_id == "shared")
    );
    let Json(read) = get_configuration(State(state.clone())).await.unwrap();
    assert_eq!(read.effective_views, saved.effective_views);
    let Json(restored) =
        put_configuration(State(state.clone()), update_bytes(&state, json!([global])))
            .await
            .unwrap();
    for view in &restored.effective_views {
        let row = view
            .rules
            .iter()
            .find(
                |row| matches!(&row.rule, TemporaryPolicyRule::Custom { id, .. } if id == "shared"),
            )
            .unwrap();
        assert_eq!(row.source, TemporaryPolicySource::Global);
        assert_eq!(row.scope, TemporaryPolicyScope::Credential);
        assert_eq!(row.backoff.initial_seconds, 12);
        assert_eq!(
            row.origin,
            if view.destination_id.is_none() {
                TemporaryPolicyRuleOrigin::Local
            } else {
                TemporaryPolicyRuleOrigin::Inherited
            }
        );
        assert!(matches!(
            row.rule,
            TemporaryPolicyRule::Custom { enabled: true, .. }
        ));
    }
}

#[test]
fn builtin_projection_uses_compiled_override_defaults_and_destination_applicability() {
    let configured = vec![
        ConfiguredRule::BuiltinOverride {
            id: GOAT_CREDITS_REJECTION_RULE.into(),
            destination_id: None,
            enabled: false,
            backoff: Some(PolicyBackoff {
                initial_secs: 15,
                max_secs: 90,
            }),
        },
        ConfiguredRule::BuiltinOverride {
            id: GOAT_CREDITS_REJECTION_RULE.into(),
            destination_id: Some("goat".into()),
            enabled: true,
            backoff: None,
        },
    ];
    let snapshot = compile_from_previous(&configured, &EffectivePolicySnapshot::builtin());
    let global = effective_view(&snapshot, &configured, None, true);
    assert!(matches!(
        global.rules[0].rule,
        TemporaryPolicyRule::BuiltinOverride { enabled: false, .. }
    ));
    assert_eq!(global.rules[0].backoff.initial_seconds, 15);
    assert!(global.rules[0].overridden);
    let local = effective_view(&snapshot, &configured, Some("goat"), true);
    assert!(matches!(
        local.rules[0].rule,
        TemporaryPolicyRule::BuiltinOverride { enabled: true, .. }
    ));
    assert_eq!(local.rules[0].backoff.initial_seconds, DEFAULT_INITIAL_SECS);
    assert_eq!(local.rules[0].scope, TemporaryPolicyScope::CredentialModel);
    assert_eq!(local.rules[0].source, TemporaryPolicySource::Connection);
    let other = effective_view(&snapshot, &configured, Some("http"), false);
    assert!(!other.rules[0].applicable);
    assert!(!other.rules[0].overridden);
    assert_eq!(other.rules[0].origin, TemporaryPolicyRuleOrigin::Inherited);
    assert_eq!(other.rules[0].backoff.initial_seconds, 15);
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
    let rows = super::super::applications::operation_receipts(&state);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].action, "policy.update");
    assert_eq!(
        rows[0].outcome,
        crate::log_types::OperationOutcome::Rejected
    );
    assert_eq!(rows[0].reason_code.as_deref(), Some("invalid.request"));
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
    let rows = super::super::applications::operation_receipts(&state);
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|row| {
        row.action == "policy.update"
            && row.outcome == crate::log_types::OperationOutcome::Rejected
            && row.reason_code.as_deref() == Some("revision.conflict")
    }));
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
    let rows = super::super::applications::operation_receipts(&state);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].action, "policy.clear");
    assert_eq!(rows[0].outcome, crate::log_types::OperationOutcome::Success);
    assert_eq!(rows[0].subject_id.as_deref(), Some("tp-missing"));
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

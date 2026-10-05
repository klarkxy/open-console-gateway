use super::super::types::BindingPatchRequest;
use super::reject_meaningless_binding_mutation;
use crate::cpa_execution::{
    DiscoveredNativeRef, DiscoverySnapshot, capture_native_lease, fence_mapped_target,
    persisted_child_generation, reconcile_owned_discovery, specialized_native_binding_allowed,
};
use crate::crypto::StaticKeyCipher;
use crate::dashboard_v3::MutationExpectation;
use crate::db::Database;
use crate::models::{Account, AccountSetupStep, AccountType};
use crate::provider::CPA_PROVIDER_ID;
use crate::state::{CoreState, CoreStateInner};
use axum::body::Bytes;
use axum::extract::{Path, State};
use chrono::Utc;
use ocg_domain::catalog::CredentialKind;
use ocg_domain::credential::{ModelScope, credential_id_for_legacy_account};
use rusqlite::params;
use std::sync::Arc;

fn open_state(label: &str) -> (CoreState, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "ocg-native-v4-{label}-{}",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&dir).expect("test data dir");
    let state = Arc::new(
        CoreStateInner::new(
            Database::open(dir.clone()).expect("database"),
            dir.clone(),
            Arc::new(StaticKeyCipher::new("native-v4")),
        )
        .expect("state"),
    );
    (state, dir)
}

fn owned_record(
    state: &CoreState,
    credential_id: &str,
) -> crate::db::identity::IdentityAccountRecord {
    state
        .db
        .lock()
        .list_identity_model()
        .expect("identity")
        .accounts
        .into_iter()
        .find(|record| record.credential_id == credential_id)
        .expect("owned native account")
}

#[test]
fn persisted_native_none_binding_is_editable_only_while_present() {
    let (state, dir) = open_state("binding");
    let account_id = crate::db::native_binding::account_id_for("codex", "codex.json").unwrap();
    let credential_id = credential_id_for_legacy_account(&account_id).to_string();
    let snapshot = DiscoverySnapshot::Complete(vec![DiscoveredNativeRef {
        native_provider: "codex".into(),
        relative_path: "codex.json".into(),
        auth_id: "codex.json".into(),
        credential_id: String::new(),
        credential_version: 1,
        material_revision: "material-1".into(),
        registration_epoch: 1,
        models: vec!["gpt-5".into()],
        bound: true,
        disabled: Some(false),
        status: Some("active".into()),
        raw_provider_label: "codex".into(),
        native_mode: String::new(),
        reported_base: String::new(),
    }]);
    {
        let db = state.db.lock();
        let generation = persisted_child_generation(&db.conn).unwrap();
        let lease = capture_native_lease(&db.conn, generation, state.settings_revision()).unwrap();
        reconcile_owned_discovery(&db.conn, &snapshot, &lease).unwrap();
    }
    let record = owned_record(&state, &credential_id);
    let empty_scope = serde_json::to_string(&ModelScope::Only { models: Vec::new() }).unwrap();
    {
        let db = state.db.lock();
        db.conn
            .execute(
                "UPDATE credentials SET enabled = 0, routing_rank = 7, scope_json = ?2 WHERE id = ?1",
                params![record.credential_id, empty_scope],
            )
            .unwrap();
        assert!(specialized_native_binding_allowed(&db.conn, &record.credential_id).unwrap());
    }
    let loaded = owned_record(&state, &credential_id);
    assert_eq!(loaded.account.provider_id, CPA_PROVIDER_ID);
    assert_eq!(loaded.account.credential_kind, CredentialKind::None);
    assert_ne!(loaded.account.id, crate::provider::CPA_ACCOUNT_ID);
    assert!(!loaded.account.enabled);
    assert_eq!(
        loaded.binding_model_scope,
        ModelScope::Only { models: Vec::new() }
    );
    assert!(reject_meaningless_binding_mutation(&state, &loaded).is_ok());
    let rank: i64 = state
        .db
        .lock()
        .conn
        .query_row(
            "SELECT routing_rank FROM credentials WHERE id = ?1",
            [&loaded.credential_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(rank, 7);
    assert!(!owned_record(&state, &credential_id).account.enabled);

    {
        let db = state.db.lock();
        fence_mapped_target(&db.conn, "codex.json", "").unwrap();
        assert!(!specialized_native_binding_allowed(&db.conn, &loaded.credential_id).unwrap());
    }
    let fenced = owned_record(&state, &credential_id);
    let error = reject_meaningless_binding_mutation(&state, &fenced).expect_err("logged out");
    let rendered = format!("{error:?}");
    assert!(rendered.contains("anonymous and no-auth bindings cannot be edited"));
    assert_eq!(
        fenced.binding_model_scope,
        ModelScope::Only { models: Vec::new() }
    );
    assert!(!fenced.account.enabled);

    drop(state);
    let _ = std::fs::remove_dir_all(dir);
}

/// Skip-spawn owned plane. The empty task fills `GatewayHandle`; it is not a child process.
struct OwnedApplyGuard {
    _shutdown_rx: tokio::sync::oneshot::Receiver<()>,
}

impl OwnedApplyGuard {
    fn arm(state: &CoreState) -> Self {
        crate::cpa_execution::set_skip_spawn(true);
        crate::cpa_execution::set_fail_ready(false);
        crate::cpa_execution::set_test_password(Some("synthetic-management".to_string()));
        crate::cpa_execution::set_before_apply_commit(None);
        crate::cpa_execution::set_artifact_dir(
            state,
            crate::cpa_execution::documented_runtime_dir(),
        );
        let (shutdown, shutdown_rx) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async {});
        *state.gateway.lock() = Some(crate::gateway_runtime::GatewayHandle {
            port: 9,
            listen_addr: "127.0.0.1:9".parse().unwrap(),
            dashboard_is_local: true,
            shutdown,
            task,
        });
        Self {
            _shutdown_rx: shutdown_rx,
        }
    }
}

impl Drop for OwnedApplyGuard {
    fn drop(&mut self) {
        crate::cpa_execution::set_skip_spawn(false);
        crate::cpa_execution::set_fail_ready(false);
        crate::cpa_execution::set_test_password(None);
        crate::cpa_execution::set_before_apply_commit(None);
    }
}

fn seed_enabled_scope_model(state: &CoreState, account_id: &str) {
    use ocg_domain::destination::{CatalogModel, Protocol, destination_id_for_builtin};
    let encrypted = state
        .cipher
        .encrypt("synthetic-scope-key")
        .expect("scope key");
    let destination_id = destination_id_for_builtin(crate::provider::OPENCODE_PROVIDER_ID);
    let db = state.db.lock();
    db.conn
        .execute(
            "UPDATE credentials SET key_cipher = ?2 WHERE legacy_account_id = ?1",
            params![account_id, encrypted],
        )
        .expect("credential key");
    let mut catalog =
        crate::db::destination_store::load_destination_catalog(&db.conn, &destination_id)
            .expect("opencode catalog");
    if catalog
        .iter()
        .any(|model| model.enabled && model.public_model == "ocg-projection-scope")
    {
        return;
    }
    catalog.push(CatalogModel {
        public_model: "ocg-projection-scope".into(),
        upstream_model: "ocg-projection-scope".into(),
        protocols: vec![Protocol::ChatCompletions],
        preferred: Some(Protocol::ChatCompletions),
        enabled: true,
        upstream_override: None,
    });
    crate::db::destination_store::replace_destination_catalog(&db.conn, &destination_id, &catalog)
        .expect("scope model");
}

fn insert_opencode(state: &CoreState, id: &str) {
    let now = Utc::now();
    state
        .db
        .lock()
        .create_account(&Account {
            id: id.into(),
            provider_id: crate::provider::OPENCODE_PROVIDER_ID.into(),
            credential_kind: crate::provider::CredentialKind::ApiKey,
            quota_scope: crate::provider::QuotaScope::Key,
            name: id.into(),
            username: None,
            password_cipher: None,
            key_cipher: "cipher".into(),
            enabled: true,
            account_type: AccountType::Key,
            setup_step: AccountSetupStep::Ready,
            referral_code: None,
            purchase_date: String::new(),
            expires_on: String::new(),
            cooldown_until: None,
            cooldown_generic_until: None,
            cooldown_5h_until: None,
            cooldown_week_until: None,
            cooldown_month_until: None,
            cooldown_free_until: None,
            last_error: None,
            auth_error: None,
            notes: None,
            created_at: now,
            updated_at: now,
        })
        .unwrap();
}

fn binding_id_for(state: &CoreState, account_id: &str) -> String {
    state
        .db
        .lock()
        .conn
        .query_row(
            "SELECT binding_id FROM credentials WHERE legacy_account_id = ?1",
            [account_id],
            |row| row.get(0),
        )
        .unwrap()
}

fn stored_scope(state: &CoreState, account_id: &str) -> ModelScope {
    state
        .db
        .lock()
        .list_identity_model()
        .unwrap()
        .accounts
        .into_iter()
        .find(|row| row.account.id == account_id)
        .unwrap()
        .binding_model_scope
}

fn scope_body(state: &CoreState, scope: Option<ModelScope>) -> Bytes {
    Bytes::from(
        serde_json::to_vec(&BindingPatchRequest {
            expectation: MutationExpectation {
                expected_revision: state.settings_revision(),
                process_generation: state.process_generation(),
            },
            model_scope: scope,
            enabled: None,
            allowed_endpoint_ids: None,
            allowed_origins: None,
        })
        .unwrap(),
    )
}

async fn started_plane(state: &CoreState) -> crate::cpa_execution::ExecutionReport {
    let started =
        crate::cpa_execution::start(state, state.settings_revision(), state.process_generation())
            .await
            .expect("owned plane start");
    assert_eq!(started.apply_status, "applied");
    assert!(started.desired_running);
    started
}

#[tokio::test(flavor = "current_thread")]
async fn patch_binding_keeps_the_saved_receipt_and_applies_scope() {
    let (state, dir) = open_state("projection-patch");
    insert_opencode(&state, "acct-projection");
    seed_enabled_scope_model(&state, "acct-projection");
    let binding_id = binding_id_for(&state, "acct-projection");
    let _plane = OwnedApplyGuard::arm(&state);
    let started = started_plane(&state).await;
    let scope = ModelScope::Only {
        models: vec!["ocg-projection-scope".into()],
    };
    let saved = super::patch(
        State(state.clone()),
        Path(binding_id),
        scope_body(&state, Some(scope.clone())),
    )
    .await
    .expect("binding patch")
    .0;
    assert_eq!(saved.revision.revision, state.settings_revision());
    assert_eq!(
        saved.revision.process_generation,
        state.process_generation()
    );
    assert_eq!(saved.binding.model_scope, scope);
    assert_eq!(stored_scope(&state, "acct-projection"), scope);
    let applied = crate::cpa_execution::execution_report(&state);
    assert_eq!(applied.desired_revision, started.desired_revision + 1);
    assert_eq!(applied.applied_revision, applied.desired_revision);
    assert_eq!(applied.apply_status, "applied");
    assert!(state.settings_update.try_lock().is_some());
    drop(state);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test(flavor = "current_thread")]
async fn patch_binding_stale_cas_does_not_apply() {
    let (state, dir) = open_state("projection-patch-stale");
    insert_opencode(&state, "acct-projection");
    let binding_id = binding_id_for(&state, "acct-projection");
    let _plane = OwnedApplyGuard::arm(&state);
    let started = started_plane(&state).await;
    let revision = state.settings_revision();
    let body = Bytes::from(
        serde_json::to_vec(&BindingPatchRequest {
            expectation: MutationExpectation {
                expected_revision: revision + 1,
                process_generation: state.process_generation(),
            },
            model_scope: Some(ModelScope::Only {
                models: vec!["ocg-projection-scope".into()],
            }),
            enabled: None,
            allowed_endpoint_ids: None,
            allowed_origins: None,
        })
        .unwrap(),
    );
    let error = super::patch(State(state.clone()), Path(binding_id), body)
        .await
        .expect_err("stale binding patch");
    assert!(format!("{error:?}").contains("revisionConflict"));
    assert_eq!(stored_scope(&state, "acct-projection"), ModelScope::All);
    assert_eq!(state.settings_revision(), revision);
    let after = crate::cpa_execution::execution_report(&state);
    assert_eq!(after.desired_revision, started.desired_revision);
    assert_eq!(after.applied_revision, started.applied_revision);
    assert_eq!(after.apply_status, started.apply_status);
    assert!(state.settings_update.try_lock().is_some());
    drop(state);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test(flavor = "current_thread")]
async fn patch_binding_empty_input_does_not_apply() {
    let (state, dir) = open_state("projection-patch-empty");
    insert_opencode(&state, "acct-projection");
    let binding_id = binding_id_for(&state, "acct-projection");
    let _plane = OwnedApplyGuard::arm(&state);
    let started = started_plane(&state).await;
    let revision = state.settings_revision();
    let error = super::patch(
        State(state.clone()),
        Path(binding_id),
        scope_body(&state, None),
    )
    .await
    .expect_err("empty binding patch");
    assert!(format!("{error:?}").contains("modelScope, enabled, or grant fields are required"));
    assert_eq!(stored_scope(&state, "acct-projection"), ModelScope::All);
    assert_eq!(state.settings_revision(), revision);
    let after = crate::cpa_execution::execution_report(&state);
    assert_eq!(after.desired_revision, started.desired_revision);
    assert_eq!(after.applied_revision, started.applied_revision);
    assert_eq!(after.apply_status, started.apply_status);
    assert!(state.settings_update.try_lock().is_some());
    drop(state);
    let _ = std::fs::remove_dir_all(dir);
}

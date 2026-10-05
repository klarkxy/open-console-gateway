use super::*;
use crate::crypto::StaticKeyCipher;
use crate::db::Database;
use crate::provider_contracts::ProtocolOverrideState;
use crate::state::CoreStateInner;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use serde_json::json;
use std::{path::PathBuf, sync::Arc};

struct Fixture {
    state: Option<CoreState>,
    dir: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("ocg-catalog-add-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = Database::open(dir.clone()).unwrap();
        let state = Arc::new(
            CoreStateInner::new(
                db,
                dir.clone(),
                Arc::new(StaticKeyCipher::new("catalog-add-tests")),
            )
            .unwrap(),
        );
        Self {
            state: Some(state),
            dir,
        }
    }
    fn state(&self) -> CoreState {
        self.state.as_ref().unwrap().clone()
    }
    fn body(&self, ids: &[&str]) -> Bytes {
        let state = self.state();
        Bytes::from(serde_json::to_vec(&json!({ "expectedRevision": state.settings_revision(), "processGeneration": state.process_generation(), "modelIds": ids })).unwrap())
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.state.take();
        std::fs::remove_dir_all(&self.dir).ok();
    }
}
fn path(id: &str) -> Path<(String, String)> {
    Path(("provider".into(), id.into()))
}

#[test]
fn additions_validate_identity_batch_and_size_before_writing() {
    assert_eq!(
        validate_additions(&[], &[" MiniMax-New ".into()]).unwrap(),
        vec!["MiniMax-New"]
    );
    for input in [
        vec![],
        vec!["".into()],
        vec!["a b".into()],
        vec!["bad\u{7f}".into()],
        vec!["x".repeat(201)],
        vec!["a".into(), "A".into()],
    ] {
        assert!(validate_additions(&[], &input).is_err());
    }
    assert!(validate_additions(&["MiniMax-New".into()], &["minimax-new".into()]).is_err());
    assert!(
        validate_additions(&[], &(0..201).map(|i| format!("m{i}")).collect::<Vec<_>>()).is_err()
    );
}

#[tokio::test]
async fn add_first_builtin_model_is_local_disabled_cas_protected_and_survives_reopen() {
    let mut f = Fixture::new();
    let state = f.state();
    let before = state.settings_revision();
    let accounts_before = serde_json::to_value(state.db.lock().list_accounts().unwrap()).unwrap();
    let stale = f.body(&["second"]);
    let result = add_models(
        State(state.clone()),
        path("minimax"),
        f.body(&[" MiniMax-New "]),
    )
    .await
    .unwrap()
    .0;
    assert!(result.revision > before);
    let group = result
        .providers
        .iter()
        .find(|p| p.provider_id == "minimax")
        .unwrap();
    assert_eq!(group.catalog.models, vec!["MiniMax-New"]);
    assert_eq!(group.catalog.source, "manual");
    assert!(group.catalog.source_url.is_empty());
    assert!(group.catalog.refreshed_at.is_none());
    let scope = ContractScope::provider("minimax");
    let current = state.provider_contracts();
    let model = current.scope(&scope).unwrap().model("MiniMax-New").unwrap();
    assert!(model.protocols.values().all(|p| !p.enabled));
    let destination_id = ocg_domain::destination::destination_id_for_builtin("minimax");
    {
        let db = state.db.lock();
        let catalog =
            crate::db::destination_store::load_destination_catalog(&db.conn, &destination_id)
                .unwrap();
        assert_eq!(catalog.len(), 1);
        assert!(!catalog[0].enabled);
        assert_eq!(
            serde_json::to_value(db.list_accounts().unwrap()).unwrap(),
            accounts_before
        );
    }
    let error = add_models(State(state.clone()), path("minimax"), stale)
        .await
        .unwrap_err();
    assert_eq!(error.into_response().status(), StatusCode::CONFLICT);
    let error = add_models(
        State(state.clone()),
        path("minimax"),
        f.body(&["minimax-new"]),
    )
    .await
    .unwrap_err();
    assert_eq!(error.into_response().status(), StatusCode::BAD_REQUEST);
    assert_eq!(state.settings_revision(), result.revision);
    drop(state);
    f.state.take();
    let db = Database::open(f.dir.clone()).unwrap();
    let saved = db.load_persisted_scope(&scope).unwrap().unwrap();
    assert_eq!(saved.catalog_models, vec!["MiniMax-New"]);
    let catalog =
        crate::db::destination_store::load_destination_catalog(&db.conn, &destination_id).unwrap();
    assert!(!catalog[0].enabled);
}

#[tokio::test]
async fn addition_preserves_existing_switches_and_source_and_works_with_existing_delete() {
    let f = Fixture::new();
    let state = f.state();
    let scope = ContractScope::provider("minimax");
    let now = Utc::now();
    state
        .db
        .lock()
        .set_contract_catalog(
            &scope,
            &["MiniMax-M2".into()],
            Some(now),
            "minimax_cn_get_models",
            "https://api.minimax.cn/v1",
            now,
        )
        .unwrap();
    state
        .db
        .lock()
        .set_model_protocol_overrides(
            &scope,
            &[(
                "MiniMax-M2".into(),
                crate::provider::UpstreamProtocolKind::Messages,
                ProtocolOverrideState::ForceOn,
            )],
            now,
        )
        .unwrap();
    state.reload_provider_contracts().unwrap();
    let old = state
        .provider_contracts()
        .scope(&scope)
        .unwrap()
        .model("MiniMax-M2")
        .unwrap()
        .clone();
    let receipt = add_models(
        State(state.clone()),
        path("minimax"),
        f.body(&["MiniMax-Extra"]),
    )
    .await
    .unwrap()
    .0;
    let contracts = state.provider_contracts();
    assert_eq!(
        contracts
            .scope(&scope)
            .unwrap()
            .model("MiniMax-M2")
            .unwrap(),
        &old
    );
    let group = receipt
        .providers
        .iter()
        .find(|p| p.provider_id == "minimax")
        .unwrap();
    assert_eq!(group.catalog.source, "minimax_cn_get_models");
    assert_eq!(
        state
            .db
            .lock()
            .load_persisted_scope(&scope)
            .unwrap()
            .unwrap()
            .catalog_refreshed_at,
        Some(now)
    );
    let _ = remove_models(
        State(state.clone()),
        path("minimax"),
        f.body(&["MiniMax-Extra"]),
    )
    .await
    .unwrap();
    assert!(
        state
            .provider_contracts()
            .scope(&scope)
            .unwrap()
            .model("MiniMax-Extra")
            .is_none()
    );
    assert!(
        state
            .provider_contracts()
            .scope(&scope)
            .unwrap()
            .model("MiniMax-M2")
            .is_some()
    );
}

#[tokio::test]
async fn unknown_cpa_and_http_scopes_are_rejected_without_writes() {
    let f = Fixture::new();
    let state = f.state();
    let before = state.settings_revision();
    for scope in [
        path("cpa"),
        path("unknown"),
        Path(("custom_endpoint".into(), "custom".into())),
    ] {
        let error = add_models(State(state.clone()), scope, f.body(&["new-model"]))
            .await
            .unwrap_err();
        assert_eq!(error.into_response().status(), StatusCode::BAD_REQUEST);
    }
    assert_eq!(state.settings_revision(), before);
}

fn edit_body(
    f: &Fixture,
    original: Option<&str>,
    public: &str,
    upstream: &str,
    protocols: &[&str],
    preferred: Option<&str>,
    enabled: bool,
) -> Bytes {
    let state = f.state();
    Bytes::from(serde_json::to_vec(&json!({"expectedRevision":state.settings_revision(),"processGeneration":state.process_generation(),"originalModelId":original,"publicModel":public,"upstreamModel":upstream,"protocols":protocols,"preferred":preferred,"enabled":enabled})).unwrap())
}

#[tokio::test]
async fn builtin_alias_and_protocol_edit_survive_refresh_controls_and_restart() {
    use crate::provider::UpstreamProtocolKind as P;
    let mut f = Fixture::new();
    let state = f.state();
    let scope = ContractScope::provider("minimax");
    let response = edit_model(
        State(state.clone()),
        Path("minimax".into()),
        edit_body(
            &f,
            None,
            "my-minimax",
            "MiniMax-M2",
            &["messages", "chat_completions"],
            Some("messages"),
            true,
        ),
    )
    .await
    .unwrap()
    .0;
    let group = response
        .providers
        .iter()
        .find(|p| p.provider_id == "minimax")
        .unwrap();
    assert_eq!(group.catalog.models, vec!["MiniMax-M2"]);
    assert_eq!(group.models[0].alias, "my-minimax");
    let dest_id = ocg_domain::destination::destination_id_for_builtin("minimax");
    let read = || {
        crate::db::destination_store::load_destination_catalog(&state.db.lock().conn, &dest_id)
            .unwrap()
    };
    let before = read();
    assert_eq!(before[0].public_model, "my-minimax");
    assert_eq!(before[0].preferred, Some(P::Messages));
    assert!(before[0].enabled);
    let runtime = crate::gateway::handler::runtime_catalog_snapshot(&state).unwrap();
    let alias = runtime.resolve("my-minimax").unwrap();
    assert!(
        alias
            .routeable_mappings()
            .iter()
            .any(|m| m.provider_id == "minimax" && m.upstream_model == "MiniMax-M2")
    );
    state
        .db
        .lock()
        .refresh_contract_catalog_preserving_settings(
            &scope,
            &["MiniMax-M2".into(), "MiniMax-New".into()],
            Utc::now(),
            "minimax_cn_get_models",
            "https://api.minimax.cn/v1",
        )
        .unwrap();
    assert_eq!(read()[0], before[0]);
    state
        .db
        .lock()
        .set_model_protocol_overrides(
            &scope,
            &[
                (
                    "MiniMax-M2".into(),
                    P::Messages,
                    ProtocolOverrideState::ForceOff,
                ),
                (
                    "MiniMax-M2".into(),
                    P::ChatCompletions,
                    ProtocolOverrideState::ForceOff,
                ),
            ],
            Utc::now(),
        )
        .unwrap();
    assert_eq!(read()[0].public_model, "my-minimax");
    assert!(!read()[0].enabled);
    state.reload_provider_contracts().unwrap();
    let stale = edit_body(
        &f,
        Some("MiniMax-M2"),
        "stale-name",
        "MiniMax-M2",
        &["messages"],
        Some("messages"),
        true,
    );
    let _ = edit_model(
        State(state.clone()),
        Path("minimax".into()),
        edit_body(
            &f,
            Some("MiniMax-M2"),
            "renamed-minimax",
            "MiniMax-M2",
            &["messages"],
            Some("messages"),
            true,
        ),
    )
    .await
    .unwrap();
    assert_eq!(
        edit_model(State(state.clone()), Path("minimax".into()), stale)
            .await
            .unwrap_err()
            .into_response()
            .status(),
        StatusCode::CONFLICT
    );
    let runtime = crate::gateway::handler::runtime_catalog_snapshot(&state).unwrap();
    assert!(runtime.resolve("my-minimax").is_err());
    assert!(runtime.resolve("renamed-minimax").is_ok());
    assert_eq!(read()[0].upstream_model, "MiniMax-M2");
    drop(runtime);
    drop(state);
    f.state.take();
    let db = Database::open(f.dir.clone()).unwrap();
    let catalog =
        crate::db::destination_store::load_destination_catalog(&db.conn, &dest_id).unwrap();
    assert_eq!(catalog[0].public_model, "renamed-minimax");
    assert_eq!(catalog[0].protocols, vec![P::Messages]);
    db.remove_contract_catalog_models(&scope, &["MiniMax-M2".into()], Utc::now())
        .unwrap();
    assert!(
        !crate::db::destination_store::load_destination_catalog(&db.conn, &dest_id)
            .unwrap()
            .iter()
            .any(|m| m.public_model == "renamed-minimax")
    );
}

#[tokio::test]
async fn builtin_model_edits_reject_collisions_and_protocol_expansion_atomically() {
    let f = Fixture::new();
    let state = f.state();
    let _ = edit_model(
        State(state.clone()),
        Path("minimax".into()),
        edit_body(
            &f,
            None,
            "existing",
            "MiniMax-M2",
            &["messages"],
            Some("messages"),
            true,
        ),
    )
    .await
    .unwrap();
    for body in [
        edit_body(
            &f,
            None,
            "EXISTING",
            "MiniMax-New",
            &["messages"],
            Some("messages"),
            true,
        ),
        edit_body(
            &f,
            None,
            "different",
            "MiniMax-M2",
            &["messages"],
            Some("messages"),
            true,
        ),
        edit_body(
            &f,
            Some("missing"),
            "changed",
            "MiniMax-M2",
            &["messages"],
            Some("messages"),
            true,
        ),
        edit_body(
            &f,
            Some("MiniMax-M2"),
            "existing",
            "MiniMax-M2",
            &["messages"],
            Some("responses"),
            true,
        ),
    ] {
        let rev = state.settings_revision();
        assert_eq!(
            edit_model(State(state.clone()), Path("minimax".into()), body)
                .await
                .unwrap_err()
                .into_response()
                .status(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(state.settings_revision(), rev);
    }
    assert_eq!(
        edit_model(
            State(state.clone()),
            Path("kimi".into()),
            edit_body(
                &f,
                None,
                "my-kimi",
                "kimi-for-coding",
                &["responses"],
                Some("responses"),
                true
            )
        )
        .await
        .unwrap_err()
        .into_response()
        .status(),
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn builtin_alias_cannot_shadow_an_existing_generated_alias() {
    let f = Fixture::new();
    let state = f.state();
    let _ = add_models(
        State(state.clone()),
        path("opencode-zen-free"),
        f.body(&["foo-free"]),
    )
    .await
    .unwrap();
    let rev = state.settings_revision();
    let result = edit_model(
        State(state.clone()),
        Path("opencode-zen-free".into()),
        edit_body(&f, None, "foo", "bar-free", &[], None, false),
    )
    .await;
    assert_eq!(
        result.unwrap_err().into_response().status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(state.settings_revision(), rev);
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
        clear_contracts_receipt_failure();
    }
}

fn minimax_models(state: &CoreState) -> Vec<String> {
    let scope = ContractScope::provider("minimax");
    state
        .db
        .lock()
        .load_persisted_scope(&scope)
        .unwrap()
        .map(|row| row.catalog_models)
        .unwrap_or_default()
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

fn assert_plane_unchanged(state: &CoreState, started: &crate::cpa_execution::ExecutionReport) {
    let after = crate::cpa_execution::execution_report(state);
    assert_eq!(after.desired_revision, started.desired_revision);
    assert_eq!(after.applied_revision, started.applied_revision);
    assert_eq!(after.apply_status, started.apply_status);
    assert!(state.settings_update.try_lock().is_some());
}

#[tokio::test(flavor = "current_thread")]
async fn add_models_stale_cas_does_not_apply() {
    let f = Fixture::new();
    let state = f.state();
    let _plane = OwnedApplyGuard::arm(&state);
    let started = started_plane(&state).await;
    let revision = state.settings_revision();
    let before = minimax_models(&state);
    let error = add_models(
        State(state.clone()),
        path("minimax"),
        Bytes::from(
            serde_json::to_vec(&json!({
                "expectedRevision": revision + 1,
                "processGeneration": state.process_generation(),
                "modelIds": ["Boundary-Stale"],
            }))
            .unwrap(),
        ),
    )
    .await
    .expect_err("stale catalog add");
    assert!(format!("{error:?}").contains("revisionConflict"));
    assert_eq!(minimax_models(&state), before);
    assert_eq!(state.settings_revision(), revision);
    assert_plane_unchanged(&state, &started);
}

#[tokio::test(flavor = "current_thread")]
async fn add_models_unknown_scope_does_not_apply() {
    let f = Fixture::new();
    let state = f.state();
    let _plane = OwnedApplyGuard::arm(&state);
    let started = started_plane(&state).await;
    let revision = state.settings_revision();
    let error = add_models(
        State(state.clone()),
        Path(("provider".into(), "not-a-builtin".into())),
        f.body(&["Boundary-Model"]),
    )
    .await
    .expect_err("unknown builtin scope");
    assert!(format!("{error:?}").contains("only built-in provider catalogs can be added here"));
    assert_eq!(state.settings_revision(), revision);
    assert_plane_unchanged(&state, &started);
}

#[tokio::test(flavor = "current_thread")]
async fn add_models_receipt_failure_after_commit_still_applies() {
    let f = Fixture::new();
    let state = f.state();
    let _plane = OwnedApplyGuard::arm(&state);
    let started = started_plane(&state).await;
    let revision = state.settings_revision();
    assert!(
        !minimax_models(&state)
            .iter()
            .any(|id| id == "Boundary-Model")
    );
    fail_next_contracts_receipt();
    let error = add_models(
        State(state.clone()),
        path("minimax"),
        f.body(&["Boundary-Model"]),
    )
    .await
    .expect_err("contracts receipt");
    assert!(format!("{error:?}").contains("provider contracts could not be read"));
    assert!(
        minimax_models(&state)
            .iter()
            .any(|id| id == "Boundary-Model")
    );
    assert!(state.settings_revision() > revision);
    let applied = crate::cpa_execution::execution_report(&state);
    assert_eq!(applied.desired_revision, started.desired_revision + 1);
    assert_eq!(applied.applied_revision, applied.desired_revision);
    assert_eq!(applied.apply_status, "applied");
    assert!(state.settings_update.try_lock().is_some());
}

#[tokio::test(flavor = "current_thread")]
async fn edit_model_receipt_failure_after_commit_still_applies() {
    let f = Fixture::new();
    let state = f.state();
    let _plane = OwnedApplyGuard::arm(&state);
    let started = started_plane(&state).await;
    let revision = state.settings_revision();
    fail_next_contracts_receipt();
    let error = edit_model(
        State(state.clone()),
        Path("minimax".into()),
        edit_body(
            &f,
            None,
            "boundary-alias",
            "MiniMax-M2",
            &["messages"],
            Some("messages"),
            true,
        ),
    )
    .await
    .expect_err("contracts receipt");
    assert!(format!("{error:?}").contains("provider contracts could not be read"));
    let dest_id = ocg_domain::destination::destination_id_for_builtin("minimax");
    let catalog =
        crate::db::destination_store::load_destination_catalog(&state.db.lock().conn, &dest_id)
            .unwrap();
    assert_eq!(catalog[0].public_model, "boundary-alias");
    assert!(state.settings_revision() > revision);
    let applied = crate::cpa_execution::execution_report(&state);
    assert_eq!(applied.desired_revision, started.desired_revision + 1);
    assert_eq!(applied.applied_revision, applied.desired_revision);
    assert_eq!(applied.apply_status, "applied");
    assert!(state.settings_update.try_lock().is_some());
}

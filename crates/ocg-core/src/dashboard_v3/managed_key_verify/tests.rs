use super::{
    StagedManagedKey, VerifyOutcome, classify_managed_response, commit_managed_key_verify,
    prepare_managed_key_verify, require_waiting_managed, stage_managed_candidate,
    verify_managed_account_key,
};
use crate::crypto::{KeyCipher, StaticKeyCipher};
use crate::db::{Database, ManagedKeyVerificationCas};
use crate::models::{Account, AccountSetupStep, AccountType, AppConfig};
use crate::provider::{COMMAND_CODE_PROVIDER_ID, OPENCODE_PROVIDER_ID, UpstreamProtocolKind};
use crate::state::{CoreState, CoreStateInner};
use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use chrono::Utc;
use std::sync::Arc;

fn open_state(tag: &str) -> (TempDir, CoreState) {
    let path =
        std::env::temp_dir().join(format!("ocg-managed-verify-{tag}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&path).unwrap();
    let db = Database::open(path.clone()).unwrap();
    let cipher: Arc<dyn KeyCipher + Send + Sync> = Arc::new(StaticKeyCipher::new(tag));
    let state = Arc::new(CoreStateInner::new(db, path.clone(), cipher).unwrap());
    #[cfg(debug_assertions)]
    state
        .usage_sync
        .set_reactive_refresh_enabled_for_test(false);
    (TempDir(path), state)
}

struct TempDir(std::path::PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn insert_managed(
    state: &CoreState,
    id: &str,
    provider_id: &str,
    step: AccountSetupStep,
    enabled: bool,
) {
    let plan = crate::provider::builtin_provider(provider_id).expect(provider_id);
    let now = Utc::now();
    let account = Account {
        id: id.into(),
        provider_id: provider_id.into(),
        credential_kind: plan.credential_kind,
        quota_scope: plan.quota_scope,
        name: id.into(),
        username: None,
        password_cipher: None,
        key_cipher: "original-cipher".into(),
        enabled,
        account_type: AccountType::Managed,
        setup_step: step,
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
    };
    state.db.lock().create_account(&account).unwrap();
}

fn stored(state: &CoreState, id: &str) -> Account {
    state.db.lock().get_account(id).unwrap().unwrap()
}

fn credential_identity(state: &CoreState, id: &str) -> (u64, u64) {
    state
        .db
        .lock()
        .conn
        .query_row(
            "SELECT COALESCE(credential_version, 1), COALESCE(auth_state_version, 1)
             FROM credentials WHERE legacy_account_id = ?1",
            [id],
            |row| Ok((row.get::<_, i64>(0)? as u64, row.get::<_, i64>(1)? as u64)),
        )
        .unwrap()
}

fn stage_pending(state: &CoreState, id: &str) -> StagedManagedKey {
    let prepared = prepare_managed_key_verify(
        state,
        id,
        "sk-managed-candidate".into(),
        "staged-cipher-value".into(),
    )
    .expect("eligible pending row");
    stage_managed_candidate(state, id, prepared).expect("stage")
}

fn assert_still_pending(account: &Account) {
    assert!(!account.enabled);
    assert_eq!(account.setup_step, AccountSetupStep::KeyVerification);
    assert_eq!(account.key_cipher, "staged-cipher-value");
}

#[test]
fn initial_stage_of_new_material_bumps_version_and_product_revision() {
    let (_dir, state) = open_state("stage");
    insert_managed(
        &state,
        "stage-managed",
        OPENCODE_PROVIDER_ID,
        AccountSetupStep::KeyVerification,
        false,
    );
    let before = crate::protocol_probe::load_persisted_credential(&state, "stage-managed").unwrap();
    let (version, auth_state) = credential_identity(&state, "stage-managed");
    let revision = state.settings_revision();
    let staged = stage_pending(&state, "stage-managed");
    let account = stored(&state, "stage-managed");
    assert_still_pending(&account);
    assert!(!account.enabled);
    assert_eq!(staged.credential_id, before.credential_id);
    assert_eq!(staged.credential_version, version + 1);
    assert_eq!(staged.binding_id, before.binding_id);
    assert_eq!(staged.product_revision, revision + 1);
    assert_eq!(staged.process_generation, state.process_generation());
    assert_eq!(staged.account_cas.key_cipher, "staged-cipher-value");
    assert_eq!(
        staged.account_cas.setup_step,
        AccountSetupStep::KeyVerification
    );
    let after = crate::protocol_probe::load_persisted_credential(&state, "stage-managed").unwrap();
    assert_eq!(after.credential_version, version + 1);
    assert_eq!(after.credential_id, before.credential_id);
    assert_eq!(after.binding_id, before.binding_id);
    assert!(after.binding_enabled);
    assert_eq!(
        credential_identity(&state, "stage-managed").1,
        auth_state + 1
    );
    assert_eq!(state.settings_revision(), revision + 1);
}

#[test]
fn blocked_rows_do_not_stage_a_candidate() {
    let (_dir, state) = open_state("blocked");
    insert_managed(
        &state,
        "ready-managed",
        OPENCODE_PROVIDER_ID,
        AccountSetupStep::Ready,
        false,
    );
    let ready = stored(&state, "ready-managed");
    let error = require_waiting_managed(&state, &ready).unwrap_err();
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert!(
        error
            .body
            .message
            .contains("not waiting for key verification")
    );
    let error = prepare_managed_key_verify(
        &state,
        "ready-managed",
        "sk-managed-candidate".into(),
        "staged-cipher-value".into(),
    )
    .unwrap_err();
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert_eq!(
        stored(&state, "ready-managed").key_cipher,
        "original-cipher"
    );

    insert_managed(
        &state,
        "other-managed",
        COMMAND_CODE_PROVIDER_ID,
        AccountSetupStep::KeyVerification,
        false,
    );
    let error = prepare_managed_key_verify(
        &state,
        "other-managed",
        "sk-managed-candidate".into(),
        "staged-cipher-value".into(),
    )
    .unwrap_err();
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert!(error.body.message.contains("managed-registration"));
    assert_eq!(
        stored(&state, "other-managed").key_cipher,
        "original-cipher"
    );

    insert_managed(
        &state,
        "binding-managed",
        OPENCODE_PROVIDER_ID,
        AccountSetupStep::KeyVerification,
        false,
    );
    let changed = state
        .db
        .lock()
        .conn
        .execute(
            "UPDATE credentials SET binding_enabled = 0 WHERE legacy_account_id = ?1",
            ["binding-managed"],
        )
        .unwrap();
    assert_eq!(changed, 1);
    let error = prepare_managed_key_verify(
        &state,
        "binding-managed",
        "sk-managed-candidate".into(),
        "staged-cipher-value".into(),
    )
    .unwrap_err();
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert!(
        error
            .body
            .message
            .contains("credential binding is disabled")
    );
    assert_eq!(
        stored(&state, "binding-managed").key_cipher,
        "original-cipher"
    );
}

#[test]
fn late_completion_cannot_promote_a_changed_key() {
    let (_dir, state) = open_state("late");
    insert_managed(
        &state,
        "stale-managed",
        OPENCODE_PROVIDER_ID,
        AccountSetupStep::KeyVerification,
        false,
    );
    let original = ManagedKeyVerificationCas::from_account(&stored(&state, "stale-managed"));
    let mut staged = stage_pending(&state, "stale-managed");
    staged.account_cas = original;
    let error = commit_managed_key_verify(&state, "stale-managed", &staged, VerifyOutcome::Success)
        .unwrap_err();
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert_still_pending(&stored(&state, "stale-managed"));

    insert_managed(
        &state,
        "rotated-managed",
        OPENCODE_PROVIDER_ID,
        AccountSetupStep::KeyVerification,
        false,
    );
    let staged = stage_pending(&state, "rotated-managed");
    let changed = state
        .db
        .lock()
        .conn
        .execute(
            "UPDATE credentials SET key_cipher = 'rotated-after-stage' WHERE legacy_account_id = ?1",
            ["rotated-managed"],
        )
        .unwrap();
    assert_eq!(changed, 1);
    let error =
        commit_managed_key_verify(&state, "rotated-managed", &staged, VerifyOutcome::Success)
            .unwrap_err();
    assert_eq!(error.status, StatusCode::CONFLICT);
    let account = stored(&state, "rotated-managed");
    assert!(!account.enabled);
    assert_eq!(account.setup_step, AccountSetupStep::KeyVerification);
    assert_eq!(account.key_cipher, "rotated-after-stage");

    insert_managed(
        &state,
        "version-managed",
        OPENCODE_PROVIDER_ID,
        AccountSetupStep::KeyVerification,
        false,
    );
    let staged = stage_pending(&state, "version-managed");
    let changed = state
        .db
        .lock()
        .conn
        .execute(
            "UPDATE credentials SET credential_version = COALESCE(credential_version, 1) + 1 WHERE legacy_account_id = ?1",
            ["version-managed"],
        )
        .unwrap();
    assert_eq!(changed, 1);
    let error =
        commit_managed_key_verify(&state, "version-managed", &staged, VerifyOutcome::Success)
            .unwrap_err();
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert_still_pending(&stored(&state, "version-managed"));

    insert_managed(
        &state,
        "late-binding",
        OPENCODE_PROVIDER_ID,
        AccountSetupStep::KeyVerification,
        false,
    );
    let staged = stage_pending(&state, "late-binding");
    let changed = state
        .db
        .lock()
        .conn
        .execute(
            "UPDATE credentials SET binding_enabled = 0 WHERE legacy_account_id = ?1",
            ["late-binding"],
        )
        .unwrap();
    assert_eq!(changed, 1);
    let error = commit_managed_key_verify(&state, "late-binding", &staged, VerifyOutcome::Success)
        .unwrap_err();
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert_still_pending(&stored(&state, "late-binding"));

    insert_managed(
        &state,
        "revision-managed",
        OPENCODE_PROVIDER_ID,
        AccountSetupStep::KeyVerification,
        false,
    );
    let staged = stage_pending(&state, "revision-managed");
    let version = staged.credential_version;
    state.bump_settings_revision();
    let error =
        commit_managed_key_verify(&state, "revision-managed", &staged, VerifyOutcome::Success)
            .unwrap_err();
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert_still_pending(&stored(&state, "revision-managed"));
    assert_eq!(
        credential_identity(&state, "revision-managed").0,
        version,
        "a stale product revision must not bump the staged candidate again"
    );

    insert_managed(
        &state,
        "setup-managed",
        OPENCODE_PROVIDER_ID,
        AccountSetupStep::KeyVerification,
        false,
    );
    let staged = stage_pending(&state, "setup-managed");
    let changed = state
        .db
        .lock()
        .conn
        .execute(
            "UPDATE credentials SET setup_step = 'payment' WHERE legacy_account_id = ?1",
            ["setup-managed"],
        )
        .unwrap();
    assert_eq!(changed, 1);
    let error = commit_managed_key_verify(&state, "setup-managed", &staged, VerifyOutcome::Success)
        .unwrap_err();
    assert_eq!(error.status, StatusCode::CONFLICT);
    let account = stored(&state, "setup-managed");
    assert!(!account.enabled);
    assert_eq!(account.setup_step, AccountSetupStep::Payment);
    assert_eq!(account.key_cipher, "staged-cipher-value");
}

#[test]
fn success_enables_the_staged_candidate() {
    let (_dir, state) = open_state("success");
    insert_managed(
        &state,
        "success-managed",
        OPENCODE_PROVIDER_ID,
        AccountSetupStep::KeyVerification,
        false,
    );
    let staged = stage_pending(&state, "success-managed");
    let revision = state.settings_revision();
    let version = staged.credential_version;
    let auth_state = credential_identity(&state, "success-managed").1;
    let Json(_) =
        commit_managed_key_verify(&state, "success-managed", &staged, VerifyOutcome::Success)
            .expect("shaped success promotes");
    let account = stored(&state, "success-managed");
    assert!(account.enabled);
    assert_eq!(account.setup_step, AccountSetupStep::Ready);
    assert_eq!(account.key_cipher, "staged-cipher-value");
    assert_eq!(state.settings_revision(), revision + 1);
    let (after_version, after_auth) = credential_identity(&state, "success-managed");
    assert_eq!(after_version, version);
    assert_eq!(after_auth, auth_state);
}

#[test]
fn failure_keeps_the_saved_pending_candidate() {
    let (_dir, state) = open_state("failure");
    for (id, outcome, status) in [
        (
            "auth-managed",
            VerifyOutcome::AuthFailed {
                status: StatusCode::UNAUTHORIZED,
                body: "no".into(),
            },
            StatusCode::BAD_REQUEST,
        ),
        (
            "outbound-managed",
            VerifyOutcome::UpstreamFailed {
                message: "validated hop was not sent".into(),
            },
            StatusCode::BAD_GATEWAY,
        ),
        (
            "shape-managed",
            VerifyOutcome::ClientFailed {
                status: StatusCode::OK,
                body: "upstream response does not match ChatCompletions".into(),
            },
            StatusCode::BAD_REQUEST,
        ),
    ] {
        insert_managed(
            &state,
            id,
            OPENCODE_PROVIDER_ID,
            AccountSetupStep::KeyVerification,
            false,
        );
        let staged = stage_pending(&state, id);
        let version = staged.credential_version;
        let error = commit_managed_key_verify(&state, id, &staged, outcome).unwrap_err();
        assert_eq!(credential_identity(&state, id).0, version);
        assert_eq!(error.status, status);
        assert!(error.body.message.starts_with("saved pending candidate; "));
        assert_still_pending(&stored(&state, id));
    }
    assert!(stored(&state, "auth-managed").auth_error.is_some());
}

#[cfg(debug_assertions)]
#[test]
fn rate_limit_completion_enables_the_staged_candidate() {
    let (_dir, state) = open_state("rate");
    insert_managed(
        &state,
        "rate-managed",
        OPENCODE_PROVIDER_ID,
        AccountSetupStep::KeyVerification,
        false,
    );
    let staged = stage_pending(&state, "rate-managed");
    let version = staged.credential_version;
    let seeded = state
        .db
        .lock()
        .conn
        .execute(
            "UPDATE credentials SET quota_recovery_json = '{\"source\":\"plan\"}'
             WHERE legacy_account_id = ?1",
            ["rate-managed"],
        )
        .unwrap();
    assert_eq!(seeded, 1);
    let Json(_) = commit_managed_key_verify(
        &state,
        "rate-managed",
        &staged,
        VerifyOutcome::RateLimited {
            body: "weekly usage limit reached".into(),
            retry_after: None,
        },
    )
    .expect("temporary 429 promotes without a plan window");
    let account = stored(&state, "rate-managed");
    assert!(account.enabled);
    assert_eq!(account.setup_step, AccountSetupStep::Ready);
    assert!(account.cooldown_generic_until.is_some());
    assert!(account.cooldown_5h_until.is_none());
    assert!(account.cooldown_week_until.is_none());
    assert!(account.cooldown_month_until.is_none());
    assert!(account.cooldown_free_until.is_none());
    assert_eq!(credential_identity(&state, "rate-managed").0, version);
    let recovery: Option<String> = state
        .db
        .lock()
        .conn
        .query_row(
            "SELECT quota_recovery_json FROM credentials WHERE legacy_account_id = ?1",
            ["rate-managed"],
            |row| row.get(0),
        )
        .unwrap();
    assert!(recovery.is_none());
}

#[test]
fn classify_keeps_rate_limit_success_and_rejects_a_malformed_success() {
    let staged = StagedManagedKey {
        account_name: "managed".into(),
        account_cas: ManagedKeyVerificationCas {
            key_cipher: "staged-cipher-value".into(),
            updated_at: Utc::now(),
            provider_id: OPENCODE_PROVIDER_ID.into(),
            account_type: AccountType::Managed,
            setup_step: AccountSetupStep::KeyVerification,
        },
        existing_generic_cooldown_until: None,
        key: "sk-managed-candidate".into(),
        key_cipher: "staged-cipher-value".into(),
        config: AppConfig::default(),
        protocol: UpstreamProtocolKind::ChatCompletions,
        public_model: "public-alias".into(),
        credential_id: "cred".into(),
        credential_version: 1,
        binding_id: "bind".into(),
        product_revision: 1,
        process_generation: 1,
    };
    let shaped = crate::protocol_probe::ValidatedHttpResult {
        status: 200,
        body: br#"{"choices":[{"message":{"role":"assistant","content":"pong"}}]}"#.to_vec(),
        retry_after: None,
    };
    assert!(matches!(
        classify_managed_response(&staged, shaped),
        VerifyOutcome::Success
    ));
    let malformed = crate::protocol_probe::ValidatedHttpResult {
        status: 200,
        body: br#"{"id":"x"}"#.to_vec(),
        retry_after: None,
    };
    assert!(matches!(
        classify_managed_response(&staged, malformed),
        VerifyOutcome::ClientFailed { .. }
    ));
    let limited = crate::protocol_probe::ValidatedHttpResult {
        status: 429,
        body: b"slow".to_vec(),
        retry_after: Some("1".into()),
    };
    assert!(matches!(
        classify_managed_response(&staged, limited),
        VerifyOutcome::RateLimited { .. }
    ));
    let unavailable = crate::protocol_probe::ValidatedHttpResult {
        status: 503,
        body: Vec::new(),
        retry_after: None,
    };
    match classify_managed_response(&staged, unavailable) {
        VerifyOutcome::UpstreamFailed { message } => assert!(message.contains("remains pending")),
        VerifyOutcome::Success
        | VerifyOutcome::RateLimited { .. }
        | VerifyOutcome::AuthFailed { .. }
        | VerifyOutcome::ClientFailed { .. } => panic!("5xx stays pending"),
    }
}

#[test]
fn initial_disabled_key_verification_is_narrowly_eligible() {
    let (_dir, state) = open_state("narrow");
    insert_managed(
        &state,
        "pending-disabled",
        OPENCODE_PROVIDER_ID,
        AccountSetupStep::KeyVerification,
        false,
    );
    let (version, _) = credential_identity(&state, "pending-disabled");
    let staged = stage_pending(&state, "pending-disabled");
    let account = stored(&state, "pending-disabled");
    assert!(!account.enabled);
    assert_eq!(account.setup_step, AccountSetupStep::KeyVerification);
    assert_eq!(account.key_cipher, staged.key_cipher);
    assert_eq!(staged.credential_version, version + 1);

    insert_managed(
        &state,
        "ready-disabled",
        OPENCODE_PROVIDER_ID,
        AccountSetupStep::Ready,
        false,
    );
    let ready_version = credential_identity(&state, "ready-disabled").0;
    let error = prepare_managed_key_verify(
        &state,
        "ready-disabled",
        "sk-managed-candidate".into(),
        "staged-cipher-value".into(),
    )
    .unwrap_err();
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert!(
        error
            .body
            .message
            .contains("not waiting for key verification")
    );
    assert_eq!(
        stored(&state, "ready-disabled").key_cipher,
        "original-cipher"
    );
    assert_eq!(
        credential_identity(&state, "ready-disabled").0,
        ready_version
    );
}

fn verify_body(state: &CoreState, revision: u64, key: &str) -> Bytes {
    let body = serde_json::json!({
        "expectedRevision": revision,
        "processGeneration": state.process_generation(),
        "key": key,
    });
    Bytes::from(serde_json::to_vec(&body).unwrap())
}

#[tokio::test]
async fn stale_expectation_does_not_stage_or_send() {
    let (_dir, state) = open_state("stale-expectation");
    insert_managed(
        &state,
        "stale-expectation",
        OPENCODE_PROVIDER_ID,
        AccountSetupStep::KeyVerification,
        false,
    );
    let revision = state.settings_revision();
    let version = credential_identity(&state, "stale-expectation").0;
    let error = verify_managed_account_key(
        State(state.clone()),
        Path("stale-expectation".into()),
        verify_body(&state, revision + 4, "sk-managed-candidate"),
    )
    .await
    .unwrap_err();
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert_eq!(
        stored(&state, "stale-expectation").key_cipher,
        "original-cipher"
    );
    assert_eq!(credential_identity(&state, "stale-expectation").0, version);
    assert_eq!(state.settings_revision(), revision);
    let report = crate::cpa_execution::execution_report(&state);
    assert_eq!(report.apply_status, "not_prepared");
    assert!(!report.desired_running);
}

#[tokio::test]
async fn unavailable_pin_sends_nothing_and_leaves_the_saved_candidate_pending() {
    let (_dir, state) = open_state("pin-unavailable");
    insert_managed(
        &state,
        "pin-managed",
        OPENCODE_PROVIDER_ID,
        AccountSetupStep::KeyVerification,
        false,
    );
    let revision = state.settings_revision();
    let (version, auth_state) = credential_identity(&state, "pin-managed");
    let error = verify_managed_account_key(
        State(state.clone()),
        Path("pin-managed".into()),
        verify_body(&state, revision, "sk-managed-candidate"),
    )
    .await
    .unwrap_err();
    assert_eq!(error.status, StatusCode::BAD_GATEWAY);
    assert!(error.body.message.starts_with("saved pending candidate; "));
    assert!(
        error
            .body
            .message
            .contains("validated protocol pin is not enforced")
    );
    assert!(!error.body.message.contains("sk-managed-candidate"));
    assert_eq!(error.body.current_revision, Some(revision + 2));
    let account = stored(&state, "pin-managed");
    assert!(!account.enabled);
    assert_eq!(account.setup_step, AccountSetupStep::KeyVerification);
    assert_ne!(account.key_cipher, "original-cipher");
    assert_ne!(account.key_cipher, "sk-managed-candidate");
    assert!(!account.key_cipher.is_empty());
    let (after_version, after_auth) = credential_identity(&state, "pin-managed");
    assert_eq!(after_version, version + 1);
    assert_eq!(after_auth, auth_state + 1);
    assert_eq!(state.settings_revision(), revision + 2);
    let report = crate::cpa_execution::execution_report(&state);
    assert_eq!(report.apply_status, "not_prepared");
    assert!(!report.desired_running);
}

#[tokio::test]
async fn product_locks_stay_free_while_apply_is_awaited() {
    let (_dir, state) = open_state("await-lock");
    insert_managed(
        &state,
        "await-managed",
        OPENCODE_PROVIDER_ID,
        AccountSetupStep::KeyVerification,
        false,
    );
    let revision = state.settings_revision();
    let version = credential_identity(&state, "await-managed").0;
    let _gate = state.cpa_operations.lock().await;
    let worker = {
        let state = state.clone();
        let body = verify_body(&state, revision, "sk-managed-candidate");
        tokio::spawn(async move {
            verify_managed_account_key(State(state), Path("await-managed".into()), body).await
        })
    };
    let mut released = false;
    for _ in 0..200 {
        tokio::task::yield_now().await;
        if state.settings_revision() > revision
            && state.settings_update.try_lock().is_some()
            && state.db.try_lock().is_some()
        {
            released = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    assert!(
        released,
        "stage must release settings and database locks before the apply await"
    );
    state.bump_settings_revision();
    drop(_gate);
    let error = worker.await.unwrap().unwrap_err();
    assert_eq!(error.status, StatusCode::CONFLICT);
    let account = stored(&state, "await-managed");
    assert!(!account.enabled);
    assert_eq!(account.setup_step, AccountSetupStep::KeyVerification);
    assert_ne!(account.key_cipher, "original-cipher");
    assert_ne!(account.key_cipher, "sk-managed-candidate");
    assert_eq!(credential_identity(&state, "await-managed").0, version + 1);
}

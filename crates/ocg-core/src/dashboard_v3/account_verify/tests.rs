use super::{
    CustomProbeFinish, CustomVerificationContract, CustomVerificationJob, CustomVerifyFailure,
    MutationExpectation, capture_custom_verification_job, complete_custom_verification,
    persisted_verification_status, run_custom_probe,
};
use crate::crypto::{KeyCipher, StaticKeyCipher};
use crate::db::Database;
use crate::models::{
    Account, AccountCustomConfigInput, AccountModelCapabilityInput, AccountSetupStep, AccountType,
    AppConfig,
};
use crate::provider::{CUSTOM_PROVIDER_ID, ConnectionVerificationStatus, UpstreamProtocolKind};
use crate::state::{CoreState, CoreStateInner};
use chrono::Utc;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

fn open_state(tag: &str) -> (TempDir, CoreState) {
    let path =
        std::env::temp_dir().join(format!("ocg-custom-verify-{tag}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&path).unwrap();
    let db = Database::open(path.clone()).unwrap();
    let cipher: Arc<dyn KeyCipher + Send + Sync> = Arc::new(StaticKeyCipher::new(tag));
    let state = Arc::new(CoreStateInner::new(db, path.clone(), cipher).unwrap());
    (TempDir(path), state)
}

struct TempDir(std::path::PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn custom_account(state: &CoreState, id: &str, endpoint: &str) -> Account {
    let now = Utc::now();
    let account = Account {
        id: id.into(),
        provider_id: CUSTOM_PROVIDER_ID.into(),
        credential_kind: crate::provider::default_credential_kind(),
        quota_scope: crate::provider::default_quota_scope(),
        name: id.into(),
        username: None,
        password_cipher: None,
        key_cipher: state.encrypt_key("sk-custom-not-hop-bearer").unwrap(),
        enabled: false,
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
    };
    state
        .db
        .lock()
        .create_account_with_contract(
            &account,
            Some(&AccountCustomConfigInput {
                endpoint_url: endpoint.into(),
                upstream_protocol: UpstreamProtocolKind::ChatCompletions,
            }),
            &[AccountModelCapabilityInput {
                public_model: "public-alias".into(),
                upstream_model: "upstream-model".into(),
                protocol: UpstreamProtocolKind::ChatCompletions,
                source: None,
            }],
        )
        .unwrap();
    state.db.lock().get_account(id).unwrap().unwrap()
}

#[test]
fn not_sent_has_no_persisted_verification_status() {
    assert_eq!(
        persisted_verification_status(&CustomProbeFinish::Verified),
        Some(ConnectionVerificationStatus::Verified)
    );
    assert_eq!(
        persisted_verification_status(&CustomProbeFinish::Failed(CustomVerifyFailure {
            message: "upstream returned a protocol error".into(),
        })),
        Some(ConnectionVerificationStatus::Failed)
    );
    assert!(
        persisted_verification_status(&CustomProbeFinish::NotSent(
            "validated hop was not sent".into()
        ))
        .is_none()
    );
}

#[tokio::test]
async fn disabled_binding_does_not_send_or_commit_a_verification() {
    let (_dir, state) = open_state("binding");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("loopback listener");
    let port = listener.local_addr().unwrap().port();
    let hits = Arc::new(AtomicUsize::new(0));
    let hits_task = hits.clone();
    tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok(_) => {
                    hits_task.fetch_add(1, Ordering::SeqCst);
                }
                Err(_) => break,
            }
        }
    });
    let endpoint = format!("http://127.0.0.1:{port}/v1/chat/completions");
    let account = custom_account(&state, "custom-binding", &endpoint);
    let changed = state
        .db
        .lock()
        .conn
        .execute(
            "UPDATE credentials SET binding_enabled = 0 WHERE legacy_account_id = ?1",
            [account.id.as_str()],
        )
        .unwrap();
    assert_eq!(changed, 1);
    let before = state
        .db
        .lock()
        .account_verification_state(&account.id)
        .unwrap()
        .unwrap();
    let revision = state.settings_revision();
    let job = capture_custom_verification_job(
        &state,
        account,
        MutationExpectation {
            expected_revision: revision,
            process_generation: state.process_generation(),
        },
    )
    .unwrap();
    let error = complete_custom_verification(&state, job)
        .await
        .expect_err("disabled binding is not a provider failure");
    assert_eq!(error.status, axum::http::StatusCode::SERVICE_UNAVAILABLE);
    assert!(
        error
            .body
            .message
            .contains("credential binding is disabled")
    );
    let after = state
        .db
        .lock()
        .account_verification_state("custom-binding")
        .unwrap()
        .unwrap();
    assert_eq!(after.status, before.status);
    assert_eq!(state.settings_revision(), revision);
    tokio::task::yield_now().await;
    assert_eq!(hits.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn missing_credential_is_not_sent() {
    let (_dir, state) = open_state("missing");
    let now = Utc::now();
    let account = Account {
        id: "missing-custom".into(),
        provider_id: CUSTOM_PROVIDER_ID.into(),
        credential_kind: crate::provider::default_credential_kind(),
        quota_scope: crate::provider::default_quota_scope(),
        name: "missing-custom".into(),
        username: None,
        password_cipher: None,
        key_cipher: String::new(),
        enabled: false,
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
    };
    let custom_config = crate::models::AccountCustomConfig {
        account_id: account.id.clone(),
        endpoint_url: "https://custom.example/v1/chat/completions".into(),
        upstream_protocol: UpstreamProtocolKind::ChatCompletions,
        created_at: now,
        updated_at: now,
    };
    let capability = crate::models::AccountModelCapability {
        account_id: account.id.clone(),
        public_model: "public-alias".into(),
        upstream_model: "upstream-model".into(),
        protocol: UpstreamProtocolKind::ChatCompletions,
        verified_at: None,
        source: "declared".into(),
    };
    let finish = run_custom_probe(
        &state,
        &CustomVerificationJob {
            expectation: MutationExpectation {
                expected_revision: state.settings_revision(),
                process_generation: state.process_generation(),
            },
            #[cfg(debug_assertions)]
            process_generation: state.process_generation(),
            account,
            config: AppConfig::default(),
            contract: CustomVerificationContract {
                account_id: "missing-custom".into(),
                account_updated_at: now.to_rfc3339(),
                key_cipher: String::new(),
                endpoint_url: custom_config.endpoint_url.clone(),
                upstream_protocol: UpstreamProtocolKind::ChatCompletions,
                capabilities: Vec::new(),
            },
            custom_config,
            first_capability: capability,
            api_key: "sk-custom-not-hop-bearer".into(),
        },
    )
    .await;
    let CustomProbeFinish::NotSent(message) = finish else {
        panic!("missing credential must not finish as verified or failed");
    };
    assert!(message.contains("no persisted credential"));
    assert!(persisted_verification_status(&CustomProbeFinish::NotSent(message)).is_none());
}

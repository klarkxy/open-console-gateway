use super::*;
use crate::crypto::{KeyCipher, StaticKeyCipher};
use crate::db::Database;
use crate::models::{AccountSetupStep, AccountType};
use crate::provider::CUSTOM_PROVIDER_ID;
use crate::state::CoreStateInner;
use std::sync::Arc;

fn temp_state(label: &str) -> (Arc<CoreStateInner>, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "ocg-account-control-{label}-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let db = Database::open(dir.clone()).unwrap();
    let cipher: Arc<dyn KeyCipher + Send + Sync> =
        Arc::new(StaticKeyCipher::new("account-control"));
    (
        Arc::new(CoreStateInner::new(db, dir.clone(), cipher).unwrap()),
        dir,
    )
}

fn custom_pending(state: &CoreStateInner, id: &str) -> Account {
    let now = Utc::now();
    Account {
        id: id.to_string(),
        provider_id: CUSTOM_PROVIDER_ID.to_string(),

        credential_kind: crate::provider::CredentialKind::ApiKey,
        quota_scope: crate::provider::QuotaScope::Key,
        name: id.to_string(),
        username: None,
        password_cipher: None,
        key_cipher: state.encrypt_key("custom-key").unwrap(),
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
    }
}

#[test]
fn go_create_and_toggle_bump_revision_and_allow_pending_custom() {
    let (state, dir) = temp_state("go-toggle");
    let before = state.settings_revision();
    let created = create_go_api_key(
        &state,
        "go-main".into(),
        "sk-go".into(),
        Some("  alice  ".into()),
        Some("  secret  ".into()),
    )
    .unwrap();
    assert!(created.enabled);
    assert_eq!(created.provider_id, OPENCODE_PROVIDER_ID);
    assert_eq!(created.username.as_deref(), Some("alice"));
    assert_eq!(state.settings_revision(), before + 1);

    let disabled = set_account_enabled(&state, &created.id, false).unwrap();
    assert!(!disabled.enabled);
    assert_eq!(state.settings_revision(), before + 2);

    let enabled = set_account_enabled(&state, &created.id, true).unwrap();
    assert!(enabled.enabled);
    assert_eq!(state.settings_revision(), before + 3);

    // Custom verification is an optional tool: a pending Custom account
    // enables without verifying first.
    state
        .db
        .lock()
        .create_account(&custom_pending(&state, "cli-custom"))
        .unwrap();
    let enabled = set_account_enabled(&state, "cli-custom", true)
        .expect("pending Custom may enable; verification is optional");
    assert!(enabled.enabled);
    let verification = state
        .db
        .lock()
        .account_verification_state("cli-custom")
        .unwrap()
        .unwrap();
    assert_eq!(verification.status, ConnectionVerificationStatus::Pending);
    assert_eq!(state.settings_revision(), before + 4);

    let zen = set_account_enabled(&state, ZEN_FREE_ACCOUNT_ID, false).unwrap_err();
    assert!(
        matches!(zen, AccountControlError::Invalid(message) if message == ZEN_FREE_MUTATION_MESSAGE)
    );

    drop(state);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn model_metadata_declaration_persists_and_reset_clears_it() {
    let (state, dir) = temp_state("metadata");
    let created =
        create_go_api_key(&state, "metadata".into(), "sk-metadata".into(), None, None).unwrap();
    let runtime = state.load_destination_runtime().unwrap();
    let destination = runtime
        .destinations
        .iter()
        .find(|destination| destination.id.contains(&created.provider_id))
        .or_else(|| runtime.destinations.first())
        .cloned()
        .expect("created account publishes a destination");
    let model =
        destination
            .catalog
            .first()
            .cloned()
            .unwrap_or(ocg_domain::destination::CatalogModel {
                public_model: "declared-model".into(),
                upstream_model: "declared-model".into(),
                protocols: vec![ocg_domain::catalog::UpstreamProtocolKind::ChatCompletions],
                preferred: None,
                enabled: true,
                upstream_override: None,
            });
    let before = state.settings_revision();
    let metadata = crate::model_metadata::ModelMetadata {
        name: Some("Declared".into()),
        ..crate::model_metadata::ModelMetadata::default()
    };
    state
        .with_settings_update(|| {
            declare_model_metadata_locked(&state, &destination, &model, Some(metadata.clone()))
        })
        .unwrap();
    assert!(state.settings_revision() > before);
    state
        .with_settings_update(|| declare_model_metadata_locked(&state, &destination, &model, None))
        .unwrap();
    drop(state);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn publication_toggle_persists_and_advances_revision_without_changing_routing() {
    let (state, dir) = temp_state("publication");
    let before = state.settings_revision();
    let hidden = state.with_settings_update(|| {
        set_public_model_publication_locked(&state, "public-model", false)
    });
    let hidden = hidden.unwrap();
    assert_eq!(hidden, vec!["public-model".to_string()]);
    assert_eq!(state.settings_revision(), before + 1);
    let shown = state
        .with_settings_update(|| set_public_model_publication_locked(&state, "public-model", true));
    assert!(shown.unwrap().is_empty());
    assert_eq!(state.settings_revision(), before + 2);
    drop(state);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn rotation_keeps_identity_rejects_stale_cas_and_excludes_special_credentials() {
    let (state, dir) = temp_state("rotate");
    let created =
        create_go_api_key(&state, "rotate-me".into(), "sk-old".into(), None, None).unwrap();
    let before = state
        .list_identity_model()
        .unwrap()
        .accounts
        .into_iter()
        .find(|record| record.account.id == created.id)
        .unwrap();
    let revision = state.settings_revision();
    state
        .with_settings_update(|| {
            rotate_upstream_credential_locked(&state, &before.credential_id, "sk-new")
        })
        .unwrap();
    let after = state
        .list_identity_model()
        .unwrap()
        .accounts
        .into_iter()
        .find(|record| record.account.id == created.id)
        .unwrap();
    assert_eq!(after.credential_id, before.credential_id);
    assert!(after.credential_version > before.credential_version);
    assert!(after.auth_state_version > before.auth_state_version);
    assert_ne!(after.account.key_cipher, before.account.key_cipher);
    assert_eq!(state.settings_revision(), revision + 1);

    let stale = rotate_upstream_credential(
        &state,
        &before.credential_id,
        "sk-late",
        MutationCas {
            expected_revision: revision,
            process_generation: state.process_generation(),
        },
    );
    assert!(matches!(stale, Err(AccountControlError::RevisionConflict)));
    let unchanged = state
        .list_identity_model()
        .unwrap()
        .accounts
        .into_iter()
        .find(|record| record.account.id == created.id)
        .unwrap();
    assert_eq!(unchanged.credential_version, after.credential_version);
    assert_eq!(unchanged.account.key_cipher, after.account.key_cipher);

    let empty = state.with_settings_update(|| {
        rotate_upstream_credential_locked(&state, &before.credential_id, "  ")
    });
    assert!(matches!(empty, Err(AccountControlError::Invalid(_))));
    let missing = state.with_settings_update(|| {
        rotate_upstream_credential_locked(&state, "missing-credential", "sk-x")
    });
    assert!(matches!(missing, Err(AccountControlError::NotFound)));
    let zen = state.with_settings_update(|| {
        rotate_upstream_credential_locked(&state, ZEN_FREE_ACCOUNT_ID, "sk-x")
    });
    assert!(matches!(zen, Err(AccountControlError::NotFound)));

    drop(state);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn provider_enable_validation_accepts_dynamic_snapshot_and_rejects_unknown() {
    let (state, dir) = temp_state("dyn-enable");
    let unknown = state.ensure_provider_can_enable("no-such-provider");
    assert!(matches!(
        unknown,
        Err(crate::provider::ProviderBindingError::UnknownProvider { .. })
    ));

    let now = Utc::now();
    let provider_id = uuid::Uuid::new_v4().to_string();
    let runtime = crate::dynamic::DynamicProviderRuntime {
        preset_id: None,
        id: provider_id.clone(),
        name: "Lab".into(),
        endpoint_url: "http://127.0.0.1:9".into(),
        upstream_protocol: crate::provider::UpstreamProtocolKind::ChatCompletions,
        auth_kind: ocg_domain::dynamic::DynamicAuthKind::None,
        mappings: vec![ocg_domain::dynamic::DynamicModelMapping {
            upstream_override: None,
            public_model: "lab-opus".into(),
            upstream_model: "vendor/opus".into(),
        }],
        created_at: now,
        updated_at: now,
        origin: ocg_domain::provider::ProviderOrigin::Custom,
        offering: "api".to_string(),
    };
    let mut account = custom_pending(&state, "dyn-none");
    account.provider_id = provider_id.clone();
    account.credential_kind = crate::provider::CredentialKind::None;
    account.key_cipher = String::new();
    account.enabled = true;
    state
        .db
        .lock()
        .create_dynamic_provider(&runtime, &account)
        .unwrap();
    state.reload_dynamic_providers().unwrap();
    state
        .ensure_provider_can_enable(&provider_id)
        .expect("current dynamic snapshot members may be enabled");

    let disabled = set_account_enabled(&state, "dyn-none", false).unwrap();
    assert!(!disabled.enabled);
    let enabled = set_account_enabled(&state, "dyn-none", true).unwrap();
    assert!(enabled.enabled);

    drop(state);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn verification_enablement_gate_reads_the_composed_card_descriptor() {
    // Custom keeps required verification for status tracking while its
    // card does not gate enablement. Command Code's public model catalog
    // is not Key verification, so its Plan and card are both ungated.
    let custom_plan = crate::provider::builtin_provider(CUSTOM_PROVIDER_ID).unwrap();
    let goat_plan =
        crate::provider::builtin_provider(crate::provider::COMMAND_CODE_PROVIDER_ID).unwrap();
    assert_eq!(
        custom_plan.verification_policy,
        crate::provider::VerificationPolicy::Required
    );
    assert_eq!(
        goat_plan.verification_policy,
        crate::provider::VerificationPolicy::NotRequired
    );
    let custom_card = crate::provider::ProviderRegistry::get(CUSTOM_PROVIDER_ID).unwrap();
    assert!(!custom_card.card_actions.enable_requires_verification);
    let goat_card =
        crate::provider::ProviderRegistry::get(crate::provider::COMMAND_CODE_PROVIDER_ID).unwrap();
    assert!(!goat_card.card_actions.enable_requires_verification);
}

#[tokio::test]
async fn delete_account_bumps_revision_and_rejects_zen() {
    let (state, dir) = temp_state("delete");
    let created = create_go_api_key(&state, "gone".into(), "sk-gone".into(), None, None).unwrap();
    let before = state.settings_revision();
    delete_account(&state, &created.id, None).await.unwrap();
    assert!(state.db.lock().get_account(&created.id).unwrap().is_none());
    assert_eq!(state.settings_revision(), before + 1);

    let zen = delete_account(&state, ZEN_FREE_ACCOUNT_ID, None)
        .await
        .unwrap_err();
    assert!(
        matches!(zen, AccountControlError::Invalid(message) if message == ZEN_FREE_DELETE_MESSAGE)
    );
    assert!(
        state
            .db
            .lock()
            .get_account(ZEN_FREE_ACCOUNT_ID)
            .unwrap()
            .is_some()
    );

    drop(state);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn publication_stale_cas_writes_nothing() {
    let (state, dir) = temp_state("publication-stale");
    let revision = state.settings_revision();
    let generation = state.process_generation();
    let hidden = state
        .with_settings_update(|| set_public_model_publication_locked(&state, "kept-hidden", false))
        .unwrap();
    assert_eq!(hidden, vec!["kept-hidden".to_string()]);
    let stale = set_public_model_publication(
        &state,
        MutationCas {
            expected_revision: revision,
            process_generation: generation,
        },
        "kept-hidden",
        true,
    );
    assert!(matches!(stale, Err(AccountControlError::RevisionConflict)));
    assert_eq!(
        state.unpublished_public_model_list(),
        vec!["kept-hidden".to_string()]
    );
    assert_eq!(
        state.db.lock().list_unpublished_public_models().unwrap(),
        vec!["kept-hidden".to_string()]
    );
    assert_eq!(state.settings_revision(), revision + 1);
    drop(state);
    let _ = std::fs::remove_dir_all(dir);
}

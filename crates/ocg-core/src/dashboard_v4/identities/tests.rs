use super::*;
use crate::crypto::{KeyCipher, StaticKeyCipher};
use crate::dashboard_v3::MutationExpectation;
use crate::db::Database;
use crate::models::{
    Account, AccountCustomConfigInput, AccountModelCapabilityInput, AccountSetupStep, AccountType,
};
use crate::provider::{
    CPA_PROVIDER_ID, OPENCODE_PROVIDER_ID, OPENCODE_ZEN_FREE_PROVIDER_ID, QuotaScope,
    UpstreamProtocolKind, builtin_provider,
};
use crate::routing_snapshot::RoutingSnapshot;
use crate::state::CoreStateInner;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use chrono::Utc;
use ocg_domain::catalog::CredentialKind;
use ocg_domain::connection::{LegacyConnectionKind, connection_id_for_legacy};
use ocg_domain::credential::{MaterialKind, identity_id_for_legacy_account};
use ocg_domain::dynamic::{DynamicAuthKind, DynamicModelMapping};
use ocg_domain::ids::CUSTOM_PROVIDER_ID;
use ocg_domain::provider::ProviderOrigin;
use std::cell::Cell;
use std::collections::HashMap;
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

fn state_dir(tag: &str) -> StateDir {
    let dir =
        std::env::temp_dir().join(format!("ocg-v4-identities-{tag}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let db = Database::open(dir.clone()).unwrap();
    let cipher: Arc<dyn KeyCipher + Send + Sync> = Arc::new(StaticKeyCipher::new(tag));
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

fn sample_account(id: &str, provider_id: &str, key_cipher: String) -> Account {
    let now = Utc::now();
    Account {
        id: id.into(),
        provider_id: provider_id.into(),
        credential_kind: CredentialKind::ApiKey,
        quota_scope: QuotaScope::Key,
        name: id.into(),
        username: None,
        password_cipher: None,
        key_cipher,
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
    }
}

fn insert_go(state: &CoreState, id: &str) {
    state
        .db
        .lock()
        .create_account(&sample_account(id, OPENCODE_PROVIDER_ID, "cipher".into()))
        .unwrap();
}

fn dynamic_runtime(id: &str, auth_kind: DynamicAuthKind) -> DynamicProviderRuntime {
    let now = Utc::now();
    DynamicProviderRuntime {
        preset_id: None,
        id: id.into(),
        name: id.into(),
        endpoint_url: format!("https://{id}.example/v1"),
        upstream_protocol: UpstreamProtocolKind::ChatCompletions,
        auth_kind,
        mappings: vec![DynamicModelMapping {
            public_model: format!("{id}-public"),
            upstream_model: format!("{id}-upstream"),
            upstream_override: None,
        }],
        created_at: now,
        updated_at: now,
        origin: ProviderOrigin::Custom,
        offering: "api".into(),
    }
}

fn create_request(
    state: &CoreState,
    connection_id: &str,
    secret: &str,
) -> IdentityCredentialCreateRequest {
    IdentityCredentialCreateRequest {
        expectation: expectation(state),
        connection_id: connection_id.into(),
        secret_input: secret.into(),
        operation_id: None,
        quota_sharing: QuotaSharing::Independent,
        account_label: None,
    }
}

fn denied(facts: &CredentialCreateFacts<'_>) -> CredentialCreateUnavailableReasonDto {
    credential_create_capability(facts)
        .reason
        .expect("denied capability has a reason")
}

#[test]
fn capability_uses_existing_facts_without_a_second_blacklist() {
    let zen = builtin_provider(OPENCODE_ZEN_FREE_PROVIDER_ID).unwrap();
    let cpa = builtin_provider(CPA_PROVIDER_ID).unwrap();
    let go = builtin_provider(OPENCODE_PROVIDER_ID).unwrap();
    let custom = builtin_provider(CUSTOM_PROVIDER_ID).unwrap();
    assert_eq!(
        denied(&CredentialCreateFacts::Builtin(&zen)),
        CredentialCreateUnavailableReasonDto::Singleton
    );
    assert_eq!(
        denied(&CredentialCreateFacts::Builtin(&cpa)),
        CredentialCreateUnavailableReasonDto::ExternalIntegration
    );
    assert_eq!(
        denied(&CredentialCreateFacts::Builtin(&custom)),
        CredentialCreateUnavailableReasonDto::DedicatedAccountFlow
    );
    assert_eq!(
        denied(&CredentialCreateFacts::CustomAccount),
        CredentialCreateUnavailableReasonDto::DedicatedAccountFlow
    );
    let none = dynamic_runtime("none-auth", DynamicAuthKind::None);
    assert_eq!(
        denied(&CredentialCreateFacts::Dynamic {
            runtime: &none,
            onboarding_draft: false,
        }),
        CredentialCreateUnavailableReasonDto::NoAuthentication
    );
    let keyed = dynamic_runtime("keyed-draft", DynamicAuthKind::Bearer);
    assert_eq!(
        denied(&CredentialCreateFacts::Dynamic {
            runtime: &keyed,
            onboarding_draft: true,
        }),
        CredentialCreateUnavailableReasonDto::Draft
    );
    let allowed = credential_create_capability(&CredentialCreateFacts::Builtin(&go));
    assert!(allowed.allowed);
    assert_eq!(allowed.material_kinds, vec![MaterialKind::ApiKey]);
    assert_eq!(allowed.reason, None);
    let keyed_live = credential_create_capability(&CredentialCreateFacts::Dynamic {
        runtime: &keyed,
        onboarding_draft: false,
    });
    assert!(keyed_live.allowed);
    assert_eq!(keyed_live.material_kinds, vec![MaterialKind::ApiKey]);
    let mut builtin_def = dynamic_runtime("builtin-def", DynamicAuthKind::Bearer);
    builtin_def.origin = ProviderOrigin::Builtin;
    assert_eq!(
        denied(&CredentialCreateFacts::Dynamic {
            runtime: &builtin_def,
            onboarding_draft: false,
        }),
        CredentialCreateUnavailableReasonDto::BuiltinDefinition
    );
}

#[test]
fn routing_snapshot_loader_runs_once_as_credential_count_grows() {
    let state = state_dir("routing-once");
    insert_go(&state, "go-a");
    let go_identity = identity_id_for_legacy_account("go-a").to_string();
    let go_connection =
        connection_id_for_legacy(LegacyConnectionKind::BuiltinProvider, OPENCODE_PROVIDER_ID)
            .to_string();
    create_credential_locked(
        &state,
        &go_identity,
        create_request(&state, &go_connection, "sk-b"),
    )
    .unwrap();
    create_credential_locked(
        &state,
        &go_identity,
        create_request(&state, &go_connection, "sk-c"),
    )
    .unwrap();

    let custom = sample_account("custom-a", CUSTOM_PROVIDER_ID, "cipher".into());
    state
        .db
        .lock()
        .create_account_with_contract(
            &custom,
            Some(&AccountCustomConfigInput {
                endpoint_url: "https://grant.example/v1/chat/completions".into(),
                upstream_protocol: UpstreamProtocolKind::ChatCompletions,
            }),
            &[AccountModelCapabilityInput {
                public_model: "public-a".into(),
                upstream_model: "upstream-a".into(),
                protocol: UpstreamProtocolKind::ChatCompletions,
                source: None,
            }],
        )
        .unwrap();

    let loads = Cell::new(0);
    let (snapshot, custom_runtimes, dynamic_providers, routing) = {
        let db = state.db.lock();
        loads.set(loads.get() + 1);
        let routing =
            CurrentHttpRoutingFacts::from_snapshot(&RoutingSnapshot::load(&db).unwrap()).unwrap();
        (
            db.list_identity_model().unwrap(),
            db.list_custom_account_runtimes().unwrap(),
            db.list_control_plane_dynamic_providers().unwrap(),
            routing,
        )
    };
    assert_eq!(loads.get(), 1);
    assert!(routing.by_account.contains_key("custom-a"));
    assert_eq!(routing.by_account.len(), routing.by_credential.len());
    assert!(!routing.by_destination.is_empty());
    let credential_count = snapshot.accounts.len();
    assert!(credential_count >= 4, "{credential_count}");

    let listed = project_identities(
        &state,
        snapshot.clone(),
        &dynamic_providers,
        &custom_runtimes,
        &routing,
        Utc::now(),
    )
    .unwrap();
    assert_eq!(loads.get(), 1);

    let go = listed
        .iter()
        .find(|row| row.legacy.id == "go-a")
        .expect("go identity");
    assert_eq!(go.credentials.len(), 3);
    let go_record = snapshot
        .accounts
        .iter()
        .find(|record| record.account.id == "go-a")
        .unwrap();
    let go_summary = go
        .credentials
        .iter()
        .find(|credential| credential.legacy.id == "go-a")
        .unwrap();
    assert_eq!(
        go_summary.bindings[0].allowed_endpoint_ids,
        go_record.allowed_endpoint_ids
    );
    assert_eq!(
        go_summary.bindings[0].allowed_origins,
        go_record.allowed_origins
    );

    let custom_identity = listed
        .iter()
        .find(|row| row.legacy.id == "custom-a")
        .expect("custom identity");
    let custom_record = snapshot
        .accounts
        .iter()
        .find(|record| record.account.id == "custom-a")
        .unwrap();
    assert_eq!(
        custom_identity.credentials[0].bindings[0].allowed_endpoint_ids,
        custom_record.allowed_endpoint_ids
    );
    assert_eq!(
        custom_identity.credentials[0].bindings[0].allowed_origins,
        custom_record.allowed_origins
    );

    let custom_by_id: HashMap<&str, &crate::custom::CustomAccountRuntime> = custom_runtimes
        .iter()
        .map(|runtime| (runtime.account_id.as_str(), runtime))
        .collect();
    let dynamic_by_id: HashMap<&str, &DynamicProviderRuntime> = dynamic_providers
        .iter()
        .map(|runtime| (runtime.id.as_str(), runtime))
        .collect();
    let (single_id, single_endpoints) = assigned_endpoints_current(
        &state,
        &custom_record.account,
        &dynamic_by_id,
        &custom_by_id,
    )
    .unwrap();
    let (indexed_id, indexed_endpoints) =
        routing.assigned_endpoints(&custom_record.account, &dynamic_by_id, &custom_by_id);
    assert_eq!(single_id, indexed_id);
    assert_eq!(single_endpoints, indexed_endpoints);
    assert!(indexed_endpoints.iter().any(
        |endpoint| endpoint.url.as_deref() == Some("https://grant.example/v1/chat/completions")
    ));

    let go_record = snapshot
        .accounts
        .iter()
        .find(|record| record.account.id == "go-a")
        .unwrap();
    let (_, go_endpoints) =
        routing.assigned_endpoints(&go_record.account, &dynamic_by_id, &custom_by_id);
    assert!(go_endpoints.iter().all(|endpoint| endpoint.url.is_none()));
}

#[test]
fn write_rechecks_current_target_and_matches_capability() {
    let state = state_dir("write-parity");
    insert_go(&state, "go-primary");
    let identity_id = identity_id_for_legacy_account("go-primary").to_string();

    state
        .db
        .lock()
        .create_dynamic_provider_definition(&dynamic_runtime("no-auth", DynamicAuthKind::None))
        .unwrap();
    let none_id =
        connection_id_for_legacy(LegacyConnectionKind::DynamicProvider, "no-auth").to_string();
    let none_facts = CredentialCreateFacts::Dynamic {
        runtime: &dynamic_runtime("no-auth", DynamicAuthKind::None),
        onboarding_draft: false,
    };
    assert!(!credential_create_capability(&none_facts).allowed);
    let none_write = create_credential_locked(
        &state,
        &identity_id,
        create_request(&state, &none_id, "sk-x"),
    );
    assert_eq!(
        none_write.unwrap_err().into_response().status(),
        StatusCode::BAD_REQUEST
    );

    let custom = sample_account("custom-deny", CUSTOM_PROVIDER_ID, "cipher".into());
    state
        .db
        .lock()
        .create_account_with_contract(
            &custom,
            Some(&AccountCustomConfigInput {
                endpoint_url: "https://deny.example/v1/chat/completions".into(),
                upstream_protocol: UpstreamProtocolKind::ChatCompletions,
            }),
            &[AccountModelCapabilityInput {
                public_model: "deny-public".into(),
                upstream_model: "deny-upstream".into(),
                protocol: UpstreamProtocolKind::ChatCompletions,
                source: None,
            }],
        )
        .unwrap();
    let custom_id =
        connection_id_for_legacy(LegacyConnectionKind::CustomAccount, "custom-deny").to_string();
    assert!(!credential_create_capability(&CredentialCreateFacts::CustomAccount).allowed);
    let custom_write = create_credential_locked(
        &state,
        &identity_id,
        create_request(&state, &custom_id, "sk-y"),
    );
    assert_eq!(
        custom_write.unwrap_err().into_response().status(),
        StatusCode::BAD_REQUEST
    );

    state
        .db
        .lock()
        .commit_onboarding_new(
            &dynamic_runtime("draft-http", DynamicAuthKind::Bearer),
            None,
            true,
            &crate::db::NewDashboardOperation {
                operation_id: "11111111-1111-4111-8111-111111111111".into(),
                kind: "onboarding_commit".into(),
                payload_digest: "draft".into(),
                result_json: "{}".into(),
            },
        )
        .unwrap();
    let draft_id =
        connection_id_for_legacy(LegacyConnectionKind::DynamicProvider, "draft-http").to_string();
    let draft_facts = CredentialCreateFacts::Dynamic {
        runtime: &dynamic_runtime("draft-http", DynamicAuthKind::Bearer),
        onboarding_draft: true,
    };
    assert_eq!(
        denied(&draft_facts),
        CredentialCreateUnavailableReasonDto::Draft
    );
    let draft_write = create_credential_locked(
        &state,
        &identity_id,
        create_request(&state, &draft_id, "sk-z"),
    );
    assert_eq!(
        draft_write.unwrap_err().into_response().status(),
        StatusCode::BAD_REQUEST
    );

    let go_id =
        connection_id_for_legacy(LegacyConnectionKind::BuiltinProvider, OPENCODE_PROVIDER_ID)
            .to_string();
    create_credential_locked(
        &state,
        &identity_id,
        create_request(&state, &go_id, "sk-ok"),
    )
    .unwrap();
}

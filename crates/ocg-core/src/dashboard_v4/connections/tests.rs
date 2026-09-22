use super::*;
use crate::crypto::{KeyCipher, StaticKeyCipher};
use crate::db::Database;
use crate::dynamic::DynamicProviderRuntime;
use crate::models::{
    Account, AccountCustomConfigInput, AccountModelCapabilityInput, AccountSetupStep, AccountType,
};
use crate::provider::{
    OPENCODE_PROVIDER_ID, OPENCODE_ZEN_FREE_PROVIDER_ID, QuotaScope, UpstreamProtocolKind,
};
use crate::state::CoreStateInner;
use chrono::Utc;
use ocg_domain::catalog::CredentialKind;
use ocg_domain::connection::LegacyConnectionKind;
use ocg_domain::credential::MaterialKind;
use ocg_domain::dynamic::{DynamicAuthKind, DynamicModelMapping};
use ocg_domain::ids::CUSTOM_PROVIDER_ID;
use ocg_domain::provider::ProviderOrigin;
use std::sync::Arc;

use super::super::identities::{CredentialCreateFacts, credential_create_capability};
use super::super::types::CredentialCreateUnavailableReasonDto;

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
        std::env::temp_dir().join(format!("ocg-v4-connections-{tag}-{}", uuid::Uuid::new_v4()));
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

fn go_account(id: &str) -> Account {
    let now = Utc::now();
    Account {
        id: id.into(),
        provider_id: OPENCODE_PROVIDER_ID.into(),
        credential_kind: CredentialKind::ApiKey,
        quota_scope: QuotaScope::Key,
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
    }
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

fn find_legacy<'a>(
    list: &'a ConnectionList,
    kind: LegacyConnectionKind,
    id: &str,
) -> &'a ConnectionSummary {
    list.connections
        .iter()
        .find(|connection| connection.legacy.kind == kind && connection.legacy.id == id)
        .unwrap_or_else(|| panic!("missing {kind:?} {id}"))
}

#[test]
fn credential_create_capability_matches_shared_decision() {
    let state = state_dir("capability");
    state
        .db
        .lock()
        .create_account(&go_account("go-listed"))
        .unwrap();
    state
        .db
        .lock()
        .create_dynamic_provider_definition(&dynamic_runtime("keyed-http", DynamicAuthKind::Bearer))
        .unwrap();
    state
        .db
        .lock()
        .create_dynamic_provider_definition(&dynamic_runtime("none-http", DynamicAuthKind::None))
        .unwrap();
    let mut deepseek = dynamic_runtime("deepseek-http", DynamicAuthKind::Bearer);
    deepseek.endpoint_url = "https://api.deepseek.com/v1".into();
    state
        .db
        .lock()
        .create_dynamic_provider_definition(&deepseek)
        .unwrap();
    state
        .db
        .lock()
        .commit_onboarding_new(
            &dynamic_runtime("draft-http", DynamicAuthKind::Bearer),
            None,
            true,
            &crate::db::NewDashboardOperation {
                operation_id: "22222222-2222-4222-8222-222222222222".into(),
                kind: "onboarding_commit".into(),
                payload_digest: "draft".into(),
                result_json: "{}".into(),
            },
        )
        .unwrap();
    let custom = go_account("custom-listed");
    let mut custom = custom;
    custom.provider_id = CUSTOM_PROVIDER_ID.into();
    state
        .db
        .lock()
        .create_account_with_contract(
            &custom,
            Some(&AccountCustomConfigInput {
                endpoint_url: "https://listed.example/v1/chat/completions".into(),
                upstream_protocol: UpstreamProtocolKind::ChatCompletions,
            }),
            &[AccountModelCapabilityInput {
                public_model: "listed-public".into(),
                upstream_model: "listed-upstream".into(),
                protocol: UpstreamProtocolKind::ChatCompletions,
                source: None,
            }],
        )
        .unwrap();

    let list = list_connections_locked(&state).unwrap();
    let go = find_legacy(
        &list,
        LegacyConnectionKind::BuiltinProvider,
        OPENCODE_PROVIDER_ID,
    );
    assert!(go.credential_create.allowed);
    assert_eq!(
        go.credential_create.material_kinds,
        vec![MaterialKind::ApiKey]
    );
    assert_eq!(go.credential_create.reason, None);

    let zen = find_legacy(
        &list,
        LegacyConnectionKind::BuiltinProvider,
        OPENCODE_ZEN_FREE_PROVIDER_ID,
    );
    assert!(!zen.credential_create.allowed);
    assert!(zen.credential_create.material_kinds.is_empty());
    assert_eq!(
        zen.credential_create.reason,
        Some(CredentialCreateUnavailableReasonDto::Singleton)
    );

    let keyed = find_legacy(&list, LegacyConnectionKind::DynamicProvider, "keyed-http");
    assert_eq!(
        keyed.credential_create,
        credential_create_capability(&CredentialCreateFacts::Dynamic {
            runtime: &dynamic_runtime("keyed-http", DynamicAuthKind::Bearer),
            onboarding_draft: false,
        })
    );
    assert!(keyed.credential_create.allowed);

    let none = find_legacy(&list, LegacyConnectionKind::DynamicProvider, "none-http");
    assert!(!none.credential_create.allowed);
    assert_eq!(
        none.credential_create.reason,
        Some(CredentialCreateUnavailableReasonDto::NoAuthentication)
    );

    let draft = find_legacy(&list, LegacyConnectionKind::DynamicProvider, "draft-http");
    assert!(!draft.credential_create.allowed);
    assert_eq!(
        draft.credential_create.reason,
        Some(CredentialCreateUnavailableReasonDto::Draft)
    );
    assert!(draft.credential_create.material_kinds.is_empty());

    let custom = find_legacy(&list, LegacyConnectionKind::CustomAccount, "custom-listed");
    assert!(!custom.credential_create.allowed);
    assert_eq!(
        custom.credential_create.reason,
        Some(CredentialCreateUnavailableReasonDto::DedicatedAccountFlow)
    );

    assert!(go.endpoints.iter().all(|endpoint| endpoint.url.is_none()));
    assert!(
        go.endpoints
            .iter()
            .all(|endpoint| !endpoint.official_balance)
    );
    assert!(
        custom
            .endpoints
            .iter()
            .all(|endpoint| !endpoint.official_balance)
    );
    let deepseek = find_legacy(
        &list,
        LegacyConnectionKind::DynamicProvider,
        "deepseek-http",
    );
    assert!(
        deepseek.endpoints.iter().any(|endpoint| {
            endpoint.url.as_deref() == Some("https://api.deepseek.com/v1")
                && endpoint.official_balance
        }),
        "{:?}",
        deepseek.endpoints
    );
}

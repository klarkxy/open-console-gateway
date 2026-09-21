use super::*;
use crate::crypto::{KeyCipher, StaticKeyCipher};
use crate::dashboard_v3::ControlRevision;
use crate::db::Database;
use crate::db::identity::QuotaSharingJoin;
use crate::dynamic::DynamicProviderRuntime;
use crate::models::{
    Account, AccountCustomConfigInput, AccountModelCapabilityInput, AccountSetupStep, AccountType,
    local_today,
};
use crate::platform::PlatformGroup;
use crate::provider::{
    COMMAND_CODE_PROVIDER_ID, CUSTOM_PROVIDER_ID, KIMI_PROVIDER_ID, MINIMAX_PROVIDER_ID,
    OLLAMA_PROVIDER_ID, OPENCODE_PROVIDER_ID, builtin_provider, default_credential_kind,
    default_quota_scope,
};
use crate::provider_contracts::ContractScope;
use crate::state::CoreStateInner;
use ocg_domain::account::AccountSetupStep as DomainSetupStep;
use ocg_domain::credential::{
    AuthState, ModelScope, OnboardingTaskKind, credential_id_for_legacy_account,
};
use ocg_domain::destination::{
    AdapterKind, LegacyDestinationRef, destination_id_for_builtin,
    destination_id_for_custom_account, destination_id_for_dynamic,
    destination_id_for_platform_account,
};
use ocg_domain::dynamic::{DynamicAuthKind, DynamicModelMapping};
use ocg_domain::ids::{CPA_ACCOUNT_ID, OPENCODE_ZEN_FREE_PROVIDER_ID, ZEN_FREE_ACCOUNT_ID};
use ocg_domain::provider::ProviderOrigin;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

fn temp_data_dir(label: &str) -> PathBuf {
    let mut dir = std::env::temp_dir();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock should be after epoch")
        .as_nanos();
    dir.push(format!("ocg-dest-proj-{label}-{nanos}"));
    fs::create_dir_all(&dir).expect("test data dir should be created");
    dir
}

fn open_db(label: &str) -> (PathBuf, Database) {
    let dir = temp_data_dir(label);
    let db = Database::open(dir.clone()).expect("test database should open");
    (dir, db)
}

fn unwrap_projection(db: &Database) -> DestinationProjection {
    match project(db).expect("projection read should succeed") {
        Ok(projection) => projection,
        Err(refusals) => panic!("projection refused: {refusals:?}"),
    }
}

fn assert_shadow_matches_project(db: &Database) {
    replace_persisted(db)
        .expect("shadow persist should run")
        .expect("projection should be total for persist");
    assert_persisted_matches_live(db);
}

fn assert_persisted_matches_live(db: &Database) {
    let stored = load_persisted(db).expect("shadow load should succeed");
    assert_eq!(stored, unwrap_projection(db));
}

#[allow(dead_code)]
fn shadow_counts(db: &Database) -> [i64; 4] {
    let count = |table: &str| {
        db.conn
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap()
    };
    [
        count("destinations"),
        count("destination_models"),
        count("credentials"),
        count("credential_grants"),
    ]
}

fn account(id: &str, provider_id: &str) -> Account {
    let plan = builtin_provider(provider_id);
    Account {
        id: id.into(),
        provider_id: provider_id.into(),
        credential_kind: plan
            .map(|plan| plan.credential_kind)
            .unwrap_or_else(default_credential_kind),
        quota_scope: plan
            .map(|plan| plan.quota_scope)
            .unwrap_or_else(default_quota_scope),
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
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
    }
}

fn create_keyed_builtin(db: &Database, id: &str, provider_id: &str) {
    db.create_account(&account(id, provider_id))
        .expect("keyed builtin account should save");
}

fn custom_capabilities(
    public_model: &str,
    upstream_model: &str,
    protocol: UpstreamProtocolKind,
) -> AccountModelCapabilityInput {
    AccountModelCapabilityInput {
        public_model: public_model.into(),
        upstream_model: upstream_model.into(),
        protocol,
        source: None,
    }
}

fn create_custom(
    db: &Database,
    id: &str,
    endpoint: &str,
    protocol: UpstreamProtocolKind,
    capabilities: &[AccountModelCapabilityInput],
) {
    let mut draft = account(id, CUSTOM_PROVIDER_ID);
    draft.name = id.into();
    db.create_account_with_contract(
        &draft,
        Some(&AccountCustomConfigInput {
            endpoint_url: endpoint.into(),
            upstream_protocol: protocol,
        }),
        capabilities,
    )
    .expect("custom account should save");
}

fn assert_persisted_credential_identity(db: &Database, account_id: &str, destination_id: &str) {
    let stored = load_persisted(db).expect("persisted store should load");
    assert_eq!(
        cred(&stored, account_id).destination_id,
        destination_id,
        "persisted credential destination_id"
    );
    dest(&stored, destination_id);
}

fn dynamic_runtime(
    id: &str,
    name: &str,
    endpoint: &str,
    auth_kind: DynamicAuthKind,
) -> DynamicProviderRuntime {
    let now = chrono::Utc::now();
    DynamicProviderRuntime {
        preset_id: None,
        id: id.into(),
        name: name.into(),
        endpoint_url: endpoint.into(),
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

fn dest<'a>(projection: &'a DestinationProjection, id: &str) -> &'a Destination {
    projection
        .destinations
        .iter()
        .find(|destination| destination.id == id)
        .unwrap_or_else(|| panic!("missing destination {id}"))
}

fn cred<'a>(projection: &'a DestinationProjection, account_id: &str) -> &'a Credential {
    let id = credential_id_for_legacy_account(account_id).to_string();
    projection
        .credentials
        .iter()
        .find(|credential| credential.id == id)
        .unwrap_or_else(|| panic!("missing credential for {account_id}"))
}

fn keyed_builtin_ids() -> [&'static str; 5] {
    [
        OPENCODE_PROVIDER_ID,
        COMMAND_CODE_PROVIDER_ID,
        MINIMAX_PROVIDER_ID,
        KIMI_PROVIDER_ID,
        OLLAMA_PROVIDER_ID,
    ]
}

#[test]
fn fresh_database_projects_only_the_zen_singleton() {
    let (dir, db) = open_db("fresh");
    let stored = db.list_accounts().unwrap();
    assert!(
        stored
            .iter()
            .any(|account| account.id == ZEN_FREE_ACCOUNT_ID),
        "fresh schema owns the Zen singleton"
    );
    assert!(
        stored
            .iter()
            .all(|account| account.id != CPA_ACCOUNT_ID && account.provider_id != CPA_PROVIDER_ID),
        "fresh schema does not persist the CPA account until the integration writes it"
    );

    // Only the schema-owned Zen singleton exists; CPA is not invented until
    // the integration writes its reserved row.
    let projection = unwrap_projection(&db);
    assert_eq!(projection.destinations.len(), 1);
    assert_eq!(projection.credentials.len(), 1);

    let zen = dest(
        &projection,
        &destination_id_for_builtin(OPENCODE_ZEN_FREE_PROVIDER_ID),
    );
    assert_eq!(zen.adapter, AdapterKind::Zen);
    let zen_cred = cred(&projection, ZEN_FREE_ACCOUNT_ID);
    assert_eq!(zen_cred.destination_id, zen.id);
    assert!(!zen_cred.has_secret);
    assert!(
        projection
            .destinations
            .iter()
            .all(|destination| destination.id != destination_id_for_builtin(CPA_PROVIDER_ID))
    );
    assert_eq!(
        load_persisted(&db).expect("fresh open should persist the zen shadow"),
        projection
    );

    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn keyed_builtins_map_destination_ids_and_follow_reorder() {
    let (dir, db) = open_db("keyed");
    db.set_contract_catalog(
        &ContractScope::provider(OPENCODE_PROVIDER_ID),
        &["mimo-v2.5".into()],
        Some(chrono::Utc::now()),
        "test",
        "https://example.invalid/models",
        chrono::Utc::now(),
    )
    .unwrap();
    for (index, provider_id) in keyed_builtin_ids().into_iter().enumerate() {
        create_keyed_builtin(&db, &format!("key-{index}"), provider_id);
    }

    let projection = unwrap_projection(&db);
    let keyed_destinations: Vec<_> = projection
        .destinations
        .iter()
        .filter(|destination| {
            keyed_builtin_ids()
                .into_iter()
                .any(|provider_id| destination.id == destination_id_for_builtin(provider_id))
        })
        .collect();
    let keyed_credentials: Vec<_> = (0..5)
        .map(|index| cred(&projection, &format!("key-{index}")))
        .collect();
    assert_eq!(keyed_destinations.len(), 5);
    assert_eq!(keyed_credentials.len(), 5);
    for (index, provider_id) in keyed_builtin_ids().into_iter().enumerate() {
        let credential = cred(&projection, &format!("key-{index}"));
        assert_eq!(
            credential.destination_id,
            destination_id_for_builtin(provider_id)
        );
        assert!(credential.has_secret);
    }
    let go = dest(
        &projection,
        &destination_id_for_builtin(OPENCODE_PROVIDER_ID),
    );
    assert_eq!(go.catalog.len(), 1);
    assert_eq!(go.catalog[0].public_model, "mimo-v2.5");
    let goat = dest(
        &projection,
        &destination_id_for_builtin(COMMAND_CODE_PROVIDER_ID),
    );
    assert!(
        goat.catalog.is_empty(),
        "absent persisted catalogs must stay empty"
    );

    let mut ids: Vec<String> = db
        .list_accounts()
        .unwrap()
        .into_iter()
        .map(|account| account.id)
        .collect();
    ids.reverse();
    db.reorder_accounts(&ids).unwrap();
    let reordered = unwrap_projection(&db);
    for (index, account) in db.list_accounts().unwrap().iter().enumerate() {
        assert_eq!(
            cred(&reordered, &account.id).routing_rank,
            index as u32,
            "routing_rank follows persisted account order"
        );
    }
    assert_shadow_matches_project(&db);

    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn custom_root_and_complete_path_are_single_credential_http_destinations() {
    let (dir, db) = open_db("custom");
    create_custom(
        &db,
        "custom-root",
        "https://api.deepseek.com",
        UpstreamProtocolKind::ChatCompletions,
        &[custom_capabilities(
            "deepseek-chat",
            "deepseek-chat",
            UpstreamProtocolKind::ChatCompletions,
        )],
    );
    create_custom(
        &db,
        "custom-path",
        "https://api.example.com/v1/messages",
        UpstreamProtocolKind::Messages,
        &[custom_capabilities(
            "claude",
            "claude-sonnet",
            UpstreamProtocolKind::Messages,
        )],
    );

    let projection = unwrap_projection(&db);
    for account_id in ["custom-root", "custom-path"] {
        let destination = dest(&projection, &destination_id_for_custom_account(account_id));
        assert_eq!(destination.adapter, AdapterKind::Http);
        assert_eq!(destination.max_credentials, None);
        let credential = cred(&projection, account_id);
        assert_eq!(credential.destination_id, destination.id);
        let stored = db
            .list_account_model_capabilities_declared(account_id)
            .unwrap();
        assert_eq!(destination.catalog.len(), stored.len());
        assert_eq!(destination.catalog[0].public_model, stored[0].public_model);
        assert_eq!(
            destination.catalog[0].upstream_model,
            stored[0].upstream_model
        );
    }
    assert_eq!(
        dest(
            &projection,
            &destination_id_for_custom_account("custom-root")
        )
        .base_url
        .as_deref(),
        Some("https://api.deepseek.com")
    );
    assert_eq!(
        dest(
            &projection,
            &destination_id_for_custom_account("custom-path")
        )
        .base_url
        .as_deref(),
        Some("https://api.example.com/v1/messages")
    );
    assert_shadow_matches_project(&db);

    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn dynamic_provider_exists_with_and_without_a_key() {
    let (dir, db) = open_db("dynamic");
    let keyed = dynamic_runtime(
        "lab-keyed",
        "Keyed lab",
        "https://lab-keyed.example/v1",
        DynamicAuthKind::Bearer,
    );
    let mut keyed_account = account("dyn-key", "lab-keyed");
    keyed_account.credential_kind = DynamicAuthKind::Bearer.credential_kind();
    keyed_account.quota_scope = DynamicAuthKind::Bearer.quota_scope();
    db.create_dynamic_provider(&keyed, &keyed_account).unwrap();

    let keyless = dynamic_runtime(
        "lab-keyless",
        "Keyless lab",
        "https://lab-keyless.example/v1",
        DynamicAuthKind::None,
    );
    db.create_dynamic_provider_definition(&keyless).unwrap();

    let projection = unwrap_projection(&db);
    let keyed_dest = dest(&projection, &destination_id_for_dynamic("lab-keyed"));
    let keyless_dest = dest(&projection, &destination_id_for_dynamic("lab-keyless"));
    assert_eq!(keyed_dest.adapter, AdapterKind::Http);
    assert_eq!(keyless_dest.adapter, AdapterKind::Http);
    assert_eq!(
        projection
            .credentials
            .iter()
            .filter(|credential| credential.destination_id == keyed_dest.id)
            .count(),
        1
    );
    assert_eq!(
        projection
            .credentials
            .iter()
            .filter(|credential| credential.destination_id == keyless_dest.id)
            .count(),
        0
    );
    assert_shadow_matches_project(&db);

    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn platform_parent_unions_linked_keys_and_leaves_unlinked_custom() {
    let (dir, db) = open_db("platform");
    db.create_platform_account(
        "plat-1",
        crate::platform::PlatformKind::NewApi,
        "Site",
        "https://newapi.example",
        Some("observer-cipher"),
    )
    .unwrap();
    create_custom(
        &db,
        "linked-a",
        "https://newapi.example/v1/chat/completions",
        UpstreamProtocolKind::ChatCompletions,
        &[
            custom_capabilities("GPT-4", "gpt-4-key1", UpstreamProtocolKind::ChatCompletions),
            custom_capabilities("claude", "claude-a", UpstreamProtocolKind::ChatCompletions),
        ],
    );
    create_custom(
        &db,
        "linked-b",
        "https://newapi.example/v1/chat/completions",
        UpstreamProtocolKind::ChatCompletions,
        &[
            custom_capabilities("gpt-4", "gpt-4-key2", UpstreamProtocolKind::ChatCompletions),
            custom_capabilities("gemini", "gemini-b", UpstreamProtocolKind::ChatCompletions),
        ],
    );
    create_custom(
        &db,
        "unlinked",
        "https://loose.example/v1",
        UpstreamProtocolKind::Responses,
        &[custom_capabilities(
            "loose-model",
            "loose-upstream",
            UpstreamProtocolKind::Responses,
        )],
    );
    db.link_platform_account("linked-a", "plat-1", &PlatformGroup::default())
        .unwrap();
    db.link_platform_account("linked-b", "plat-1", &PlatformGroup::default())
        .unwrap();

    let projection = unwrap_projection(&db);
    let parent = dest(&projection, &destination_id_for_platform_account("plat-1"));
    assert!(parent.capabilities.observer);
    assert_eq!(cred(&projection, "linked-a").destination_id, parent.id);
    assert_eq!(cred(&projection, "linked-b").destination_id, parent.id);
    let unlinked = dest(&projection, &destination_id_for_custom_account("unlinked"));
    assert_eq!(unlinked.max_credentials, None);
    assert_eq!(cred(&projection, "unlinked").destination_id, unlinked.id);
    assert!(
        projection
            .destinations
            .iter()
            .all(
                |destination| destination.id != destination_id_for_custom_account("linked-a")
                    && destination.id != destination_id_for_custom_account("linked-b")
            )
    );
    assert_eq!(
        parent
            .catalog
            .iter()
            .map(|model| model.public_model.as_str())
            .collect::<Vec<_>>(),
        vec!["GPT-4", "claude", "gemini"]
    );
    assert_eq!(parent.catalog[0].upstream_model, "gpt-4-key1");
    assert_shadow_matches_project(&db);

    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn managed_draft_carries_onboarding_task_ready_does_not() {
    let (dir, db) = open_db("managed");
    let mut draft = account("managed-draft", OPENCODE_PROVIDER_ID);
    draft.account_type = AccountType::Managed;
    draft.setup_step = AccountSetupStep::Payment;
    draft.key_cipher.clear();
    draft.enabled = false;
    draft.cooldown_generic_until = Some(chrono::Utc::now());
    db.create_account(&draft).unwrap();

    let mut ready = account("managed-ready", OPENCODE_PROVIDER_ID);
    ready.account_type = AccountType::Managed;
    ready.setup_step = AccountSetupStep::Ready;
    db.create_account(&ready).unwrap();

    let projection = unwrap_projection(&db);
    let draft_cred = cred(&projection, "managed-draft");
    let task = draft_cred
        .onboarding_task
        .as_ref()
        .expect("non-ready managed draft carries a task");
    assert_eq!(task.kind, OnboardingTaskKind::ManagedRegistration);
    assert_eq!(task.step, DomainSetupStep::Payment.as_str());
    assert!(cred(&projection, "managed-ready").onboarding_task.is_none());
    assert!(draft_cred.cooldowns.generic_until.is_some());
    assert_shadow_matches_project(&db);

    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn managed_complete_persists_secret_ready_onboarding_and_auth_state() {
    let (dir, db) = open_db("managed-persist");
    let mut draft = account("managed-ready-key", OPENCODE_PROVIDER_ID);
    draft.account_type = AccountType::Managed;
    draft.setup_step = AccountSetupStep::GoogleAccount;
    draft.key_cipher.clear();
    draft.enabled = false;
    db.create_account(&draft).unwrap();
    for (from, to) in [
        (
            AccountSetupStep::GoogleAccount,
            AccountSetupStep::OpencodeRegistration,
        ),
        (
            AccountSetupStep::OpencodeRegistration,
            AccountSetupStep::Payment,
        ),
        (AccountSetupStep::Payment, AccountSetupStep::KeyVerification),
    ] {
        assert!(
            db.advance_managed_setup("managed-ready-key", from, to)
                .unwrap()
        );
    }
    assert!(
        db.save_managed_key_for_verification("managed-ready-key", "candidate-cipher")
            .unwrap()
    );
    let after_save = load_persisted(&db).expect("persisted store should load after key save");
    let saved = cred(&after_save, "managed-ready-key");
    assert!(saved.has_secret);
    assert_eq!(
        saved
            .onboarding_task
            .as_ref()
            .map(|task| task.step.as_str()),
        Some(DomainSetupStep::KeyVerification.as_str())
    );
    assert!(
        db.complete_managed_setup_if_key_matches("managed-ready-key", "candidate-cipher")
            .unwrap()
    );
    let stored = load_persisted(&db).expect("persisted store should load after complete");
    let credential = cred(&stored, "managed-ready-key");
    assert!(credential.has_secret);
    assert!(
        credential.onboarding_task.is_none(),
        "ready managed accounts must not keep an onboarding task"
    );
    assert_eq!(credential.auth_state, AuthState::Unknown);
    db.set_account_auth_error("managed-ready-key", Some("401 unauthorized"))
        .unwrap();
    let invalid = load_persisted(&db).expect("persisted store should load after auth error");
    assert_eq!(
        cred(&invalid, "managed-ready-key").auth_state,
        AuthState::Invalid
    );

    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn shared_quota_membership_updates_persisted_quota_pool_id() {
    let (dir, db) = open_db("quota-persist");
    create_keyed_builtin(&db, "go-primary", OPENCODE_PROVIDER_ID);
    let snapshot = db.list_identity_model().unwrap();
    let primary = snapshot
        .accounts
        .iter()
        .find(|record| record.account.id == "go-primary")
        .expect("primary identity row");
    let identity_id = primary.identity_id.clone();
    let primary_credential_id = primary.credential_id.clone();

    let independent = account("go-independent", OPENCODE_PROVIDER_ID);
    db.create_account_for_identity(
        &identity_id,
        &independent,
        &local_today(),
        ConnectionVerificationStatus::NotRequired,
        QuotaSharingJoin::Independent,
        None,
    )
    .unwrap();
    let shared = account("go-shared", OPENCODE_PROVIDER_ID);
    db.create_account_for_identity(
        &identity_id,
        &shared,
        &local_today(),
        ConnectionVerificationStatus::NotRequired,
        QuotaSharingJoin::Shared {
            source_credential_id: primary_credential_id,
        },
        None,
    )
    .unwrap();

    let stored = load_persisted(&db).expect("persisted store should load after quota join");
    let primary_cred = cred(&stored, "go-primary");
    let independent_cred = cred(&stored, "go-independent");
    let shared_cred = cred(&stored, "go-shared");
    assert!(primary_cred.quota_pool_id.is_some());
    assert_eq!(shared_cred.quota_pool_id, primary_cred.quota_pool_id);
    assert_ne!(independent_cred.quota_pool_id, primary_cred.quota_pool_id);

    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn identity_second_credential_shares_pool_only_when_joined_and_binding_is_projected() {
    let (dir, db) = open_db("identity");
    create_keyed_builtin(&db, "go-primary", OPENCODE_PROVIDER_ID);
    let snapshot = db.list_identity_model().unwrap();
    let primary = snapshot
        .accounts
        .iter()
        .find(|record| record.account.id == "go-primary")
        .expect("primary identity row");
    let identity_id = primary.identity_id.clone();
    let primary_credential_id = primary.credential_id.clone();
    let primary_binding_id = primary.binding_id.clone();

    let independent = account("go-independent", OPENCODE_PROVIDER_ID);
    db.create_account_for_identity(
        &identity_id,
        &independent,
        &local_today(),
        ConnectionVerificationStatus::NotRequired,
        QuotaSharingJoin::Independent,
        None,
    )
    .unwrap();

    let shared = account("go-shared", OPENCODE_PROVIDER_ID);
    db.create_account_for_identity(
        &identity_id,
        &shared,
        &local_today(),
        ConnectionVerificationStatus::NotRequired,
        QuotaSharingJoin::Shared {
            source_credential_id: primary_credential_id,
        },
        None,
    )
    .unwrap();

    db.update_credential_binding(
        &primary_binding_id,
        Some(&ModelScope::Only {
            models: vec!["mimo-v2.5".into()],
        }),
        Some(false),
        Some(&["ep-chat".into()]),
        Some(&["https://opencode.ai".into()]),
    )
    .unwrap();

    let projection = unwrap_projection(&db);
    let primary_cred = cred(&projection, "go-primary");
    let independent_cred = cred(&projection, "go-independent");
    let shared_cred = cred(&projection, "go-shared");
    assert_ne!(
        independent_cred.quota_pool_id, primary_cred.quota_pool_id,
        "independent second credential must not inherit the identity singleton pool"
    );
    assert_eq!(shared_cred.quota_pool_id, primary_cred.quota_pool_id);
    assert_eq!(
        primary_cred.scope,
        ModelScope::Only {
            models: vec!["mimo-v2.5".into()],
        }
    );
    assert!(!primary_cred.enabled);
    assert_eq!(primary_cred.grants.allowed_endpoint_ids, vec!["ep-chat"]);
    assert_shadow_matches_project(&db);

    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn corrupt_custom_endpoint_refuses_the_whole_projection() {
    let (dir, db) = open_db("refuse");
    create_keyed_builtin(&db, "go-ok", OPENCODE_PROVIDER_ID);
    create_custom(
        &db,
        "broken-custom",
        "https://ok.example/v1",
        UpstreamProtocolKind::ChatCompletions,
        &[custom_capabilities(
            "ok",
            "ok",
            UpstreamProtocolKind::ChatCompletions,
        )],
    );
    drop(db);

    let conn = rusqlite::Connection::open(dir.join("data.sqlite")).unwrap();
    conn.execute(
        "UPDATE destinations SET base_url = ''
         WHERE legacy_kind = 'custom_account' AND legacy_id = ?1",
        ["broken-custom"],
    )
    .unwrap();
    drop(conn);

    let db = Database::open(dir.clone()).unwrap();
    let result = project(&db).expect("corrupt rows are mapping refusals, not IO errors");
    let Err(refusals) = result else {
        panic!("corrupt custom endpoint must refuse");
    };
    assert!(
        refusals.iter().any(|refusal| {
            matches!(
                &refusal.row,
                RefusedRow::Account { id, provider_id }
                    if id == "broken-custom" && provider_id == CUSTOM_PROVIDER_ID
            ) && refusal.error
                == MappingError::CustomAccountMissingEndpoint {
                    account_id: "broken-custom".into(),
                }
        }),
        "refusals should name the corrupt account: {refusals:?}"
    );
    assert!(
        db.get_account("broken-custom").unwrap().is_some(),
        "persist-on-open must not delete credential-backed account rows"
    );
    let persist = replace_persisted(&db).expect("refusal persist must not fail open");
    assert!(persist.is_err());
    assert!(
        db.get_account("broken-custom").unwrap().is_some(),
        "an explicit refusal persist must not delete credential-backed account rows"
    );

    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn projection_is_read_only_and_has_no_http_client() {
    let dir = temp_data_dir("readonly");
    let cipher: Arc<dyn KeyCipher + Send + Sync> =
        Arc::new(StaticKeyCipher::new("dest-proj-readonly"));
    let state = Arc::new(
        CoreStateInner::new(Database::open(dir.clone()).unwrap(), dir.clone(), cipher).unwrap(),
    );
    let before_revision = state.settings_revision();
    let before_control = ControlRevision::from_state(&state);
    let projection = {
        let db = state.db.lock();
        unwrap_projection(&db)
    };
    assert!(!projection.destinations.is_empty());
    assert_eq!(state.settings_revision(), before_revision);
    let after_control = ControlRevision::from_state(&state);
    assert_eq!(after_control.revision, before_control.revision);
    assert_eq!(
        after_control.process_generation,
        before_control.process_generation
    );
    assert_eq!(
        after_control.pricing_revision,
        before_control.pricing_revision
    );

    drop(state);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn v51_credentials_store_secrets_without_exposing_them_on_projection() {
    let (dir, db) = open_db("v51-secrets");
    let names: Vec<String> = {
        let mut stmt = db.conn.prepare("PRAGMA table_info(credentials)").unwrap();
        stmt.query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    };
    assert!(
        names.iter().any(|name| name == "key_cipher"),
        "v51 stores Key ciphertext on credentials: {names:?}"
    );
    assert!(
        names.iter().any(|name| name == "password_cipher"),
        "v51 stores password ciphertext on credentials: {names:?}"
    );
    assert!(names.iter().any(|name| name == "has_secret"));
    assert!(names.iter().any(|name| name == "legacy_account_id"));

    create_keyed_builtin(&db, "go-secret", OPENCODE_PROVIDER_ID);
    assert_shadow_matches_project(&db);
    let stored: String = db
        .conn
        .query_row(
            "SELECT key_cipher FROM credentials WHERE legacy_account_id = ?1",
            ["go-secret"],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(stored, "cipher");
    assert_eq!(
        db.credential_key_cipher_for_legacy_account("go-secret")
            .unwrap()
            .as_deref(),
        Some("cipher")
    );
    let loaded = load_persisted(&db).expect("shadow load should succeed");
    let json = serde_json::to_value(&loaded.credentials).unwrap();
    let blob = json.to_string();
    assert!(
        !blob.contains("cipher"),
        "projection DTO must stay secret-free: {blob}"
    );
    assert!(
        !blob.contains("key_cipher"),
        "projection DTO must not name key_cipher: {blob}"
    );

    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn v53_leftover_custom_tables_are_gone_and_projection_stays_secret_free() {
    let (dir, db) = open_db("v53-no-leftover-custom");
    let schema_version: i32 = db
        .conn
        .query_row("SELECT MAX(version) FROM schema_version", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(schema_version, crate::db::CURRENT_SCHEMA_VERSION);
    let leftover: i64 = db
        .conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master
             WHERE type = 'table'
               AND name IN (
                    'account_custom_configs',
                    'account_model_capabilities',
                    'cpa_integration',
                    'providers',
                    'provider_models',
                    'upstream_identities',
                    'credential_state',
                    'credential_bindings',
                    'legacy_identity_map',
                    'onboarding_tasks',
                    'subscription_records'
               )",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(leftover, 0);
    let accounts_present: i64 = db
        .conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'accounts'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(accounts_present, 0);
    create_keyed_builtin(&db, "go-v52", OPENCODE_PROVIDER_ID);
    db.update_account(
        "go-v52",
        &crate::models::AccountUpdate {
            name: None,
            username: None,
            password: None,
            key: None,
            enabled: Some(false),
            referral_code: None,
            purchase_date: None,
            notes: None,
        },
        None,
        None,
    )
    .unwrap();
    let reconstructed = db.get_account("go-v52").unwrap().expect("credential row");
    assert!(!reconstructed.enabled);
    assert_eq!(reconstructed.key_cipher, "cipher");
    let live = unwrap_projection(&db);
    let stored = load_persisted(&db).expect("shadow should stay populated");
    assert!(shadow_is_populated(&stored));
    let blob = serde_json::to_value(&live.credentials).unwrap().to_string();
    assert!(!blob.contains("cipher"));
    assert!(!blob.contains("key_cipher"));
    let sql_cipher: String = db
        .conn
        .query_row(
            "SELECT key_cipher FROM credentials WHERE legacy_account_id = ?1",
            ["go-v52"],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(sql_cipher, "cipher");
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn create_custom_account_refreshes_shadow_without_reopen() {
    let (dir, db) = open_db("4d2-custom");
    create_custom(
        &db,
        "custom-new",
        "https://api.example.com/v1",
        UpstreamProtocolKind::ChatCompletions,
        &[custom_capabilities(
            "ex",
            "ex-up",
            UpstreamProtocolKind::ChatCompletions,
        )],
    );
    assert_persisted_credential_identity(
        &db,
        "custom-new",
        &destination_id_for_custom_account("custom-new"),
    );

    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn reorder_accounts_refreshes_shadow_routing_rank_without_reopen() {
    let (dir, db) = open_db("4d2-reorder");
    create_keyed_builtin(&db, "rank-a", OPENCODE_PROVIDER_ID);
    create_keyed_builtin(&db, "rank-b", MINIMAX_PROVIDER_ID);
    create_keyed_builtin(&db, "rank-c", KIMI_PROVIDER_ID);
    let mut ids: Vec<String> = db
        .list_accounts()
        .unwrap()
        .into_iter()
        .map(|account| account.id)
        .collect();
    ids.reverse();
    db.reorder_accounts(&ids).unwrap();
    let stored = load_persisted(&db).expect("reordered store should load");
    dest(&stored, &destination_id_for_builtin(OPENCODE_PROVIDER_ID));
    dest(&stored, &destination_id_for_builtin(MINIMAX_PROVIDER_ID));
    dest(&stored, &destination_id_for_builtin(KIMI_PROVIDER_ID));
    let stored_ids: Vec<&str> = stored
        .credentials
        .iter()
        .map(|credential| credential.legacy_account_id.as_str())
        .collect();
    assert_eq!(
        stored_ids,
        ids.iter().map(String::as_str).collect::<Vec<_>>(),
        "persisted credentials must follow the saved account order"
    );
    for (index, account) in db.list_accounts().unwrap().iter().enumerate() {
        assert_eq!(
            cred(&stored, &account.id).routing_rank,
            index as u32,
            "persisted routing_rank must follow the live account order"
        );
    }
    assert_eq!(
        account_ids(&list_accounts_for_v3(&db).expect("v3 list should follow persisted order")),
        ids
    );

    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn platform_link_and_unlink_refresh_shadow_without_reopen() {
    let (dir, db) = open_db("4d2-platform");
    db.create_platform_account(
        "plat-4d2",
        crate::platform::PlatformKind::NewApi,
        "Site",
        "https://newapi.example",
        Some("observer-cipher"),
    )
    .unwrap();
    create_custom(
        &db,
        "linked-4d2",
        "https://newapi.example/v1/chat/completions",
        UpstreamProtocolKind::ChatCompletions,
        &[custom_capabilities(
            "gpt",
            "gpt-up",
            UpstreamProtocolKind::ChatCompletions,
        )],
    );
    db.link_platform_account("linked-4d2", "plat-4d2", &PlatformGroup::default())
        .unwrap();
    assert_persisted_credential_identity(
        &db,
        "linked-4d2",
        &destination_id_for_platform_account("plat-4d2"),
    );
    let linked = load_persisted(&db).expect("linked store should load");
    let retained_id = destination_id_for_custom_account("linked-4d2");
    assert!(
        linked
            .destinations
            .iter()
            .any(|destination| destination.id == retained_id),
        "the now-empty Custom connection remains reusable"
    );
    assert!(
        linked
            .credentials
            .iter()
            .all(|credential| credential.destination_id != retained_id),
        "the linked Key moved to the platform destination"
    );

    db.unlink_platform_account("linked-4d2").unwrap();
    let unlinked = load_persisted(&db).expect("unlinked store should load");
    let unlinked_destination = cred(&unlinked, "linked-4d2").destination_id.clone();
    assert_ne!(unlinked_destination, retained_id);
    assert!(
        unlinked.destinations.iter().any(|destination| {
            destination.id == unlinked_destination
                && matches!(destination.legacy, LegacyDestinationRef::CustomAccount(_))
        }),
        "unlink creates a fresh standalone Custom connection without overwriting the retained one"
    );

    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn shadow_compare_is_projection_equality() {
    let (dir, db) = open_db("4d3a-eq");
    let live = unwrap_projection(&db);
    let stored = load_persisted(&db).expect("open persist should leave a readable shadow");
    assert!(shadow_compare(&live, &stored));
    assert_eq!(
        read_shadow_if_matches(&db, &live).expect("matching shadow should be returned"),
        stored
    );

    let mut changed = stored.clone();
    changed.destinations[0].name.push_str("-diverged");
    assert!(!shadow_compare(&live, &changed));

    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn read_shadow_if_matches_returns_none_when_stale_or_empty() {
    let (dir, db) = open_db("4d3a-stale");
    create_custom(
        &db,
        "custom-shadow",
        "https://api.example.com/v1",
        UpstreamProtocolKind::ChatCompletions,
        &[custom_capabilities(
            "ex",
            "ex-up",
            UpstreamProtocolKind::ChatCompletions,
        )],
    );
    let stored = load_persisted(&db).expect("custom destination must persist");
    dest(&stored, &destination_id_for_custom_account("custom-shadow"));
    cred(&stored, "custom-shadow");

    db.conn
        .execute(
            "UPDATE credentials SET name = ?1 WHERE legacy_account_id = ?2",
            ["diverged-custom", "custom-shadow"],
        )
        .unwrap();
    let diverged = unwrap_projection(&db);
    let stale = load_persisted(&db).expect("stale shadow should still load");
    assert_ne!(stale, diverged);
    assert!(read_shadow_if_matches(&db, &diverged).is_none());
    assert!(!shadow_compare(&diverged, &stale));

    db.conn
        .execute_batch(
            "DELETE FROM credential_grants;
             DELETE FROM credentials;
             DELETE FROM destination_models;
             DELETE FROM destinations;",
        )
        .unwrap();
    let after_empty = unwrap_projection(&db);
    let empty = load_persisted(&db).expect("emptied shadow should still load");
    assert!(empty.destinations.is_empty());
    assert!(empty.credentials.is_empty());
    assert!(read_shadow_if_matches(&db, &after_empty).is_none());

    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn v4_read_serves_persisted_configuration_including_empty() {
    let (dir, db) = open_db("4d3b-v4");
    create_custom(
        &db,
        "custom-shadow",
        "https://api.example.com/v1",
        UpstreamProtocolKind::ChatCompletions,
        &[custom_capabilities(
            "ex",
            "ex-up",
            UpstreamProtocolKind::ChatCompletions,
        )],
    );
    let stored = load_persisted(&db).expect("open persist should leave a readable store");
    dest(&stored, &destination_id_for_custom_account("custom-shadow"));
    cred(&stored, "custom-shadow");
    assert_eq!(
        read_v4_projection(&db)
            .expect("v4 read should succeed")
            .expect("populated store should be total"),
        stored
    );

    db.conn
        .execute(
            "UPDATE credentials SET name = ?1 WHERE legacy_account_id = ?2",
            ["diverged-custom", "custom-shadow"],
        )
        .unwrap();
    let diverged = unwrap_projection(&db);
    let stale = load_persisted(&db).expect("stale shadow should still load");
    assert_ne!(stale, diverged);
    assert!(shadow_is_populated(&stale));
    assert_eq!(
        read_v4_projection(&db)
            .expect("v4 read should succeed")
            .expect("populated shadow is the V4 read model"),
        stale
    );

    db.conn
        .execute_batch(
            "DELETE FROM credential_grants;
             DELETE FROM credentials;
             DELETE FROM destination_models;
             DELETE FROM destinations;",
        )
        .unwrap();
    let empty = load_persisted(&db).expect("emptied shadow should still load");
    assert!(!shadow_is_populated(&empty));
    assert_eq!(
        read_v4_projection(&db)
            .expect("v4 read should succeed")
            .expect("empty configuration is valid"),
        empty
    );
    assert!(load_runtime(&db).unwrap().destinations.is_empty());

    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn runtime_configuration_load_error_never_reconstructs_legacy_facts() {
    let (dir, db) = open_db("runtime-authority");
    db.conn
        .execute_batch("DROP TABLE credential_grants;")
        .unwrap();
    assert!(read_v4_projection(&db).is_err());
    assert!(load_runtime(&db).is_err());
    assert!(routing_projection(&db).is_err());
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn custom_protocol_control_roundtrip_uses_destination_authority() {
    use crate::provider_contracts::ProtocolOverrideState;
    let (dir, db) = open_db("custom-effective-controls");
    create_custom(
        &db,
        "effective-key",
        "https://example.com/v1",
        UpstreamProtocolKind::ChatCompletions,
        &[custom_capabilities(
            "public",
            "upstream",
            UpstreamProtocolKind::ChatCompletions,
        )],
    );
    let scope = ContractScope::custom_endpoint("effective-key");
    let changes = |state| {
        vec![(
            "public".to_string(),
            UpstreamProtocolKind::ChatCompletions,
            state,
        )]
    };
    db.set_model_protocol_overrides(
        &scope,
        &changes(ProtocolOverrideState::ForceOff),
        Utc::now(),
    )
    .unwrap();
    let id = destination_id_for_custom_account("effective-key");
    assert!(!dest(&load_runtime(&db).unwrap(), &id).catalog[0].enabled);
    db.create_account(&account("unrelated-key", OPENCODE_PROVIDER_ID))
        .unwrap();
    assert!(!dest(&load_runtime(&db).unwrap(), &id).catalog[0].enabled);
    db.set_model_protocol_overrides(&scope, &changes(ProtocolOverrideState::ForceOn), Utc::now())
        .unwrap();
    let restored = load_runtime(&db).unwrap();
    let model = &dest(&restored, &id).catalog[0];
    assert!(model.enabled);
    assert_eq!(model.protocols, vec![UpstreamProtocolKind::ChatCompletions]);
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn v59_upgrade_preserves_connection_grants_and_disabled_models() {
    let (dir, db) = open_db("v59-grant-identity");
    create_custom(
        &db,
        "upgrade-key",
        "https://example.com/v1",
        UpstreamProtocolKind::ChatCompletions,
        &[custom_capabilities(
            "public",
            "upstream",
            UpstreamProtocolKind::ChatCompletions,
        )],
    );
    let id = destination_id_for_custom_account("upgrade-key");
    db.conn
        .execute(
            "UPDATE destination_models SET enabled = 0 WHERE destination_id = ?1",
            [&id],
        )
        .unwrap();
    let previous = load_runtime(&db).unwrap();
    let connection: String = db.conn.query_row("SELECT authorization_connection_id FROM credentials WHERE legacy_account_id = 'upgrade-key'", [], |row| row.get(0)).unwrap();
    db.conn.execute_batch("ALTER TABLE credentials DROP COLUMN authorization_connection_id; DELETE FROM schema_version; INSERT INTO schema_version VALUES(58);").unwrap();
    drop(db);
    let db = Database::open(dir.clone()).unwrap();
    let after = load_runtime(&db).unwrap();
    assert_eq!(
        cred(&after, "upgrade-key").grants,
        cred(&previous, "upgrade-key").grants
    );
    assert!(!dest(&after, &id).catalog[0].enabled);
    let restored: String = db.conn.query_row("SELECT authorization_connection_id FROM credentials WHERE legacy_account_id = 'upgrade-key'", [], |row| row.get(0)).unwrap();
    assert_eq!(restored, connection);
    assert_eq!(
        db.schema_version().unwrap(),
        crate::db::CURRENT_SCHEMA_VERSION
    );
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn v4_read_refuses_a_corrupt_persisted_custom_destination() {
    let (dir, db) = open_db("4d3b-refuse");
    create_custom(
        &db,
        "custom-shadow",
        "https://api.example.com/v1",
        UpstreamProtocolKind::ChatCompletions,
        &[custom_capabilities(
            "ex",
            "ex-up",
            UpstreamProtocolKind::ChatCompletions,
        )],
    );
    let stored = load_persisted(&db).expect("shadow should stay populated");
    assert!(
        read_v4_projection(&db)
            .expect("v4 read should succeed")
            .is_ok()
    );
    assert!(shadow_is_populated(&stored));

    db.conn
        .execute(
            "UPDATE destinations SET base_url = ''
             WHERE legacy_kind = 'custom_account' AND legacy_id = ?1",
            ["custom-shadow"],
        )
        .unwrap();
    let refusals = read_v4_projection(&db)
        .expect("persisted validation should complete")
        .expect_err("a corrupt persisted endpoint must fail closed");
    assert_eq!(
        refusals,
        vec![ProjectionRefusal {
            row: RefusedRow::Account {
                id: "custom-shadow".to_string(),
                provider_id: CUSTOM_PROVIDER_ID.to_string(),
            },
            error: MappingError::CustomAccountMissingEndpoint {
                account_id: "custom-shadow".to_string(),
            },
        }]
    );

    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

fn account_ids(accounts: &[Account]) -> Vec<String> {
    accounts.iter().map(|account| account.id.clone()).collect()
}

#[allow(dead_code)]
fn reverse_account_sort_order(db: &Database) {
    let live = db.list_accounts().expect("live accounts should load");
    for (index, account) in live.iter().rev().enumerate() {
        db.conn
            .execute(
                "UPDATE credentials SET routing_rank = ?1 WHERE legacy_account_id = ?2",
                rusqlite::params![index as i64, account.id.as_str()],
            )
            .unwrap();
    }
}

#[test]
fn v3_account_list_follows_populated_shadow_then_live_leftovers() {
    let (dir, db) = open_db("4d3c-v3");
    create_custom(
        &db,
        "custom-a",
        "https://a.example/v1",
        UpstreamProtocolKind::ChatCompletions,
        &[custom_capabilities(
            "a",
            "a-up",
            UpstreamProtocolKind::ChatCompletions,
        )],
    );
    create_custom(
        &db,
        "custom-b",
        "https://b.example/v1",
        UpstreamProtocolKind::ChatCompletions,
        &[custom_capabilities(
            "b",
            "b-up",
            UpstreamProtocolKind::ChatCompletions,
        )],
    );
    let shadow_order = account_ids(&list_accounts_for_v3(&db).expect("shim should load"));
    assert_eq!(shadow_order, account_ids(&db.list_accounts().unwrap()));

    db.conn
        .execute_batch("DELETE FROM credential_grants; DELETE FROM destination_models;")
        .unwrap();
    assert_eq!(
        account_ids(&list_accounts_for_v3(&db).expect("credential rows still list")),
        account_ids(&db.list_accounts().unwrap())
    );
    assert_eq!(
        account_ids(&db.list_accounts().unwrap()).len(),
        shadow_order.len()
    );

    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn v3_platform_list_follows_populated_shadow_then_live_leftovers() {
    let (dir, db) = open_db("4d3c-plat");
    db.create_platform_account(
        "plat-a",
        crate::platform::PlatformKind::NewApi,
        "Site A",
        "https://a.example",
        None,
    )
    .unwrap();
    db.create_platform_account(
        "plat-b",
        crate::platform::PlatformKind::NewApi,
        "Site B",
        "https://b.example",
        None,
    )
    .unwrap();
    let shadow_ids: Vec<String> = list_platform_accounts_for_v3(&db)
        .expect("platform shim should load")
        .into_iter()
        .map(|account| account.id)
        .collect();
    assert_eq!(
        shadow_ids,
        db.list_platform_accounts()
            .unwrap()
            .into_iter()
            .map(|account| account.id)
            .collect::<Vec<_>>()
    );

    let dest_a = destination_id_for_platform_account("plat-a");
    db.conn
        .execute(
            "DELETE FROM destination_models WHERE destination_id = ?1",
            [dest_a.as_str()],
        )
        .unwrap();
    db.conn
        .execute("DELETE FROM destinations WHERE id = ?1", [dest_a.as_str()])
        .unwrap();
    let live_ids: Vec<String> = db
        .list_platform_accounts()
        .unwrap()
        .into_iter()
        .map(|account| account.id)
        .collect();
    assert_eq!(live_ids, vec!["plat-b".to_string()]);
    let shim_ids: Vec<String> = list_platform_accounts_for_v3(&db)
        .expect("deleted parent destination is gone after leftover drop")
        .into_iter()
        .map(|account| account.id)
        .collect();
    assert_eq!(shim_ids, live_ids);

    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn free_channel_exhausted_uses_zen_adapter_not_reserved_id() {
    let (dir, db) = open_db("5-free");
    let wall = chrono::Utc::now();
    let live = unwrap_projection(&db);
    assert!(
        !free_channel_exhausted(&live, wall),
        "fresh Zen has no free cooldown"
    );

    db.conn
        .execute(
            "UPDATE credentials SET cooldown_free_until = ?1 WHERE legacy_account_id = ?2",
            [
                (wall + chrono::Duration::hours(1)).to_rfc3339(),
                ZEN_FREE_ACCOUNT_ID.to_string(),
            ],
        )
        .unwrap();
    replace_persisted(&db)
        .expect("persist after cooldown")
        .expect("projection stays total");
    let stored = load_persisted(&db).expect("shadow should reload");
    assert!(
        free_channel_exhausted(&stored, wall),
        "Zen adapter + future free_until exhausts Free"
    );
    assert!(!free_channel_exhausted(
        &stored,
        wall + chrono::Duration::hours(1)
    ));

    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn catalog_refresh_survives_unreadable_protocol_evidence() {
    let (dir, db) = open_db("catalog-poison-refresh");
    db.set_contract_catalog(
        &ContractScope::provider(OPENCODE_PROVIDER_ID),
        &["keep-me".into(), "drop-me".into()],
        Some(chrono::Utc::now()),
        "test",
        "https://example.invalid/models",
        chrono::Utc::now(),
    )
    .unwrap();
    db.conn
        .execute(
            "INSERT INTO provider_contract_model_protocols
               (scope_kind, scope_id, model_id, protocol, source)
             VALUES ('provider', ?1, 'corrupt', 'chat_completions', 'invalid-after-commit')",
            [OPENCODE_PROVIDER_ID],
        )
        .unwrap();
    refresh_destination_shadow(&db).expect("poison evidence must not fail credential persist");
    assert!(
        db.get_account(ZEN_FREE_ACCOUNT_ID).unwrap().is_some(),
        "credential-backed zen row must survive a catalog refresh that cannot join evidence"
    );
    db.remove_contract_catalog_models(
        &ContractScope::provider(OPENCODE_PROVIDER_ID),
        &["drop-me".into()],
        chrono::Utc::now(),
    )
    .expect("catalog remove must commit even when leftover evidence is unreadable");
    let stored = db
        .load_persisted_scope(&ContractScope::provider(OPENCODE_PROVIDER_ID))
        .unwrap()
        .unwrap();
    assert_eq!(stored.catalog_models, vec!["keep-me".to_string()]);
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn create_account_ensures_builtin_destination_and_keeps_key_after_cooldown() {
    let (dir, db) = open_db("runtime-ensure-builtin");
    let dest_id = destination_id_for_builtin(OPENCODE_PROVIDER_ID);
    let before_dests: i64 = db
        .conn
        .query_row("SELECT COUNT(*) FROM destinations", [], |row| row.get(0))
        .unwrap();
    create_keyed_builtin(&db, "go-runtime", OPENCODE_PROVIDER_ID);
    let after_create: i64 = db
        .conn
        .query_row("SELECT COUNT(*) FROM destinations", [], |row| row.get(0))
        .unwrap();
    assert!(after_create >= before_dests);
    let exists: i64 = db
        .conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM destinations WHERE id = ?1)",
            [&dest_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(exists, 1);
    let key: String = db
        .conn
        .query_row(
            "SELECT key_cipher FROM credentials WHERE legacy_account_id = ?1",
            ["go-runtime"],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(key, "cipher");
    db.set_account_cooldown(
        "go-runtime",
        Some(chrono::Utc::now() + chrono::Duration::minutes(5)),
        Some("429"),
    )
    .unwrap();
    let key_after: String = db
        .conn
        .query_row(
            "SELECT key_cipher FROM credentials WHERE legacy_account_id = ?1",
            ["go-runtime"],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(key_after, "cipher");
    let dests_after_cooldown: i64 = db
        .conn
        .query_row("SELECT COUNT(*) FROM destinations", [], |row| row.get(0))
        .unwrap();
    assert_eq!(dests_after_cooldown, after_create);
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn refusing_write_empties_shadow_without_failing_the_mutation() {
    let (dir, db) = open_db("4d2-refuse");
    match db.create_account(&account("orphan-custom", CUSTOM_PROVIDER_ID)) {
        Ok(()) => {
            let result = project(&db).expect("refusals are mapping errors, not IO");
            assert!(
                result.is_err(),
                "custom account without custom_config must refuse"
            );
            assert!(
                db.get_account("orphan-custom").unwrap().is_some(),
                "a mapping refusal must not delete credential-backed account rows"
            );
        }
        Err(_) => {
            create_custom(
                &db,
                "broken-custom",
                "https://ok.example/v1",
                UpstreamProtocolKind::ChatCompletions,
                &[custom_capabilities(
                    "ok",
                    "ok",
                    UpstreamProtocolKind::ChatCompletions,
                )],
            );
            db.conn
                .execute(
                    "UPDATE destinations SET base_url = ''
                     WHERE legacy_kind = 'custom_account' AND legacy_id = ?1",
                    ["broken-custom"],
                )
                .unwrap();
            let persist = replace_persisted_on(&db).expect("refusal persist must not fail SQL");
            assert!(persist.is_err());
            assert!(
                db.get_account("broken-custom").unwrap().is_some(),
                "a mapping refusal must not delete credential-backed account rows"
            );
        }
    }

    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

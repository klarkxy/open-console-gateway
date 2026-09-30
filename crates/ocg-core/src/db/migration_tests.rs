//! Historical migration tests. Shared fixtures live in the parent tests module.

use super::tests::*;
use super::*;
use super::{migrations::V27MigrationFault, v27_test_hooks};
use crate::crypto::{
    KeyCipher, LOCAL_CIPHER_V2_PREFIX, StaticKeyCipher, is_legacy_local_ciphertext,
};
use ocg_domain::credential::ModelScope;
use ocg_domain::dynamic::DynamicAuthKind;
use std::fs;
use std::sync::Arc;

#[test]
fn v64_renames_only_safe_preset_names_and_preserves_routing_scope() {
    let dir = temp_data_dir("v64-preset-leaves");
    let db = open_with_host_cipher(dir.clone()).unwrap();
    let now = Utc::now();
    let provider_id = uuid::Uuid::new_v4().to_string();
    let account_id = "v64-preset-account";
    let runtime = crate::dynamic::DynamicProviderRuntime {
        preset_id: Some("stepfun-plan".into()),
        id: provider_id.clone(),
        name: "Step Plan".into(),
        endpoint_url: "http://127.0.0.1:9".into(),
        upstream_protocol: crate::provider::UpstreamProtocolKind::ChatCompletions,
        auth_kind: DynamicAuthKind::Bearer,
        mappings: [
            ("stepfun-plan/step-router-v1", "step-router-v1"),
            ("stepfun-plan/vendor/one", "vendor/one"),
            ("stepfun-plan/other/one", "other/one"),
            ("stepfun-plan/custom", "manually-changed"),
        ]
        .into_iter()
        .map(
            |(public_model, upstream_model)| ocg_domain::dynamic::DynamicModelMapping {
                public_model: public_model.into(),
                upstream_model: upstream_model.into(),
                upstream_override: None,
            },
        )
        .collect(),
        created_at: now,
        updated_at: now,
        origin: ocg_domain::provider::ProviderOrigin::Preset,
        offering: "plan".into(),
    };
    let mut first = account(account_id);
    first.provider_id = provider_id.clone();
    first.key_cipher = fixture_account_key_cipher();
    db.create_dynamic_provider(&runtime, &first).unwrap();
    let old = "stepfun-plan/step-router-v1";
    db.conn
        .execute(
            "UPDATE credentials SET scope_json=?2 WHERE legacy_account_id=?1",
            params![
                account_id,
                serde_json::to_string(&ModelScope::Only {
                    models: vec![old.into()]
                })
                .unwrap()
            ],
        )
        .unwrap();
    db.upsert_unpublished_public_model(old).unwrap();
    db.conn
        .execute_batch(
            "DELETE FROM schema_version; INSERT INTO schema_version(version) VALUES (63);",
        )
        .unwrap();
    migrate_to_v64(&db.conn).unwrap();
    assert_eq!(lifecycle::schema_version_on(&db.conn).unwrap(), 64);
    let updated = db.get_dynamic_provider(&provider_id).unwrap().unwrap();
    let names: Vec<_> = updated
        .mappings
        .iter()
        .map(|row| row.public_model.as_str())
        .collect();
    assert!(names.contains(&"step-router-v1"));
    assert!(!names.contains(&old));
    assert!(names.contains(&"stepfun-plan/vendor/one"));
    assert!(names.contains(&"stepfun-plan/other/one"));
    assert!(names.contains(&"stepfun-plan/custom"));
    let scope: String = db
        .conn
        .query_row(
            "SELECT scope_json FROM credentials WHERE legacy_account_id=?1",
            [account_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        serde_json::from_str::<ModelScope>(&scope).unwrap(),
        ModelScope::Only {
            models: vec!["step-router-v1".into()]
        }
    );
    assert!(
        db.list_unpublished_public_models()
            .unwrap()
            .contains(&"step-router-v1".into())
    );
    assert!(
        !db.list_unpublished_public_models()
            .unwrap()
            .contains(&old.into())
    );
    migrate_to_v64(&db.conn).unwrap();
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn v41_concurrent_migration_rechecks_version_under_the_writer_lock() {
    let dir = temp_data_dir("v41-concurrent");
    let path = dir.join("data.sqlite");
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch("CREATE TABLE schema_version(version INTEGER PRIMARY KEY); INSERT INTO schema_version VALUES(40);").unwrap();
    drop(conn);
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let mut workers = Vec::new();
    for _ in 0..2 {
        let path = path.clone();
        let barrier = barrier.clone();
        workers.push(std::thread::spawn(move || {
            let conn = Connection::open(path).unwrap();
            conn.busy_timeout(std::time::Duration::from_secs(5))
                .unwrap();
            barrier.wait();
            migrate_to_v41(&conn).unwrap();
            assert_eq!(lifecycle::schema_version_on(&conn).unwrap(), 41);
        }));
    }
    for worker in workers {
        worker.join().unwrap();
    }
    let conn = Connection::open(&path).unwrap();
    assert_eq!(
        conn.query_row(
            "SELECT COUNT(*) FROM provider_model_protocol_preferences",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    drop(conn);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn v41_model_preferences_migrate_and_survive_reopen_without_enabling_models() {
    let dir = temp_data_dir("model-preferences");
    let db = Database::open(dir.clone()).unwrap();
    let scope = ContractScope::provider(MINIMAX_PROVIDER_ID);
    let now = Utc::now();
    let rows = vec![(
        "MiniMax-M3".to_string(),
        UpstreamProtocolKind::ChatCompletions,
        ProtocolOverrideState::ForceOff,
    )];
    db.set_model_protocol_overrides(&scope, &rows, now).unwrap();
    db.conn.execute_batch("DROP TABLE provider_model_protocol_preferences; DELETE FROM schema_version; INSERT INTO schema_version (version) VALUES (38);").unwrap();
    drop_unified_provider_tables(&db.conn);
    drop(db);
    let db = Database::open(dir.clone()).unwrap();
    assert_eq!(
        lifecycle::schema_version_on(&db.conn).unwrap(),
        CURRENT_SCHEMA_VERSION
    );
    let before = db.load_persisted_contracts().unwrap();
    assert!(before.preferences.is_empty());
    assert_eq!(
        before.overrides[&scope][0].state,
        ProtocolOverrideState::ForceOff
    );
    assert!(
        db.set_model_protocol_settings(
            &scope,
            &[(
                "MiniMax-M3".into(),
                UpstreamProtocolKind::ChatCompletions,
                ProtocolOverrideState::ForceOn
            )],
            &[
                ("MiniMax-M3".into(), UpstreamProtocolKind::ChatCompletions),
                ("MiniMax-M3".into(), UpstreamProtocolKind::ChatCompletions),
            ],
            now
        )
        .is_err()
    );
    assert_eq!(db.load_persisted_contracts().unwrap(), before);
    db.set_model_protocol_settings(
        &scope,
        &rows,
        &[("MiniMax-M3".into(), UpstreamProtocolKind::ChatCompletions)],
        now,
    )
    .unwrap();
    drop(db);
    let reopened = Database::open(dir.clone()).unwrap();
    let saved = reopened.load_persisted_contracts().unwrap();
    assert_eq!(
        saved.preferences[&scope],
        vec![("minimax-m3".into(), UpstreamProtocolKind::ChatCompletions)]
    );
    assert_eq!(
        saved.overrides[&scope][0].state,
        ProtocolOverrideState::ForceOff
    );
    drop(reopened);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn v42_concurrent_migration_rechecks_version_under_the_writer_lock() {
    let dir = temp_data_dir("v42-concurrent");
    let path = dir.join("data.sqlite");
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch("CREATE TABLE schema_version(version INTEGER PRIMARY KEY); INSERT INTO schema_version VALUES(41);")
        .unwrap();
    drop(conn);
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let mut workers = Vec::new();
    for _ in 0..2 {
        let path = path.clone();
        let barrier = barrier.clone();
        workers.push(std::thread::spawn(move || {
            let path_for_migration = path.clone();
            let conn = Connection::open(path).unwrap();
            conn.busy_timeout(std::time::Duration::from_secs(5))
                .unwrap();
            barrier.wait();
            migrate_to_v42(&conn, &path_for_migration, true).unwrap();
            assert_eq!(lifecycle::schema_version_on(&conn).unwrap(), 42);
        }));
    }
    for worker in workers {
        worker.join().unwrap();
    }
    let conn = Connection::open(&path).unwrap();
    assert_eq!(lifecycle::schema_version_on(&conn).unwrap(), 42);
    assert!(table_exists(&conn, "providers").unwrap());
    assert!(!table_exists(&conn, "dynamic_providers").unwrap());
    assert!(!table_exists(&conn, "dynamic_provider_models").unwrap());
    drop(conn);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn v42_fresh_database_includes_seven_sealed_builtin_rows() {
    let dir = temp_data_dir("v42-fresh-seeds");
    let db = open_with_host_cipher(dir.clone()).unwrap();
    assert_eq!(
        lifecycle::schema_version_on(&db.conn).unwrap(),
        CURRENT_SCHEMA_VERSION
    );
    assert_leftover_dynamic_provider_storage_absent(&db.conn);
    for builtin_id in [
        OPENCODE_PROVIDER_ID,
        OPENCODE_ZEN_FREE_PROVIDER_ID,
        COMMAND_CODE_PROVIDER_ID,
        MINIMAX_PROVIDER_ID,
        KIMI_PROVIDER_ID,
        OLLAMA_PROVIDER_ID,
        CUSTOM_PROVIDER_ID,
    ] {
        let row = db
            .get_provider_definition(builtin_id)
            .unwrap()
            .unwrap_or_else(|| panic!("sealed builtin `{builtin_id}`"));
        assert_eq!(row.origin, ocg_domain::provider::ProviderOrigin::Builtin);
        assert_eq!(row.id, builtin_id);
    }
    let opencode = db
        .get_provider_definition(OPENCODE_PROVIDER_ID)
        .unwrap()
        .expect("opencode");
    assert_eq!(opencode.name, "OpenCode Go");
    assert_eq!(opencode.offering, "plan");
    assert_eq!(
        opencode.auth_kind,
        ocg_domain::dynamic::DynamicAuthKind::Bearer
    );
    let zen_free = db
        .get_provider_definition(OPENCODE_ZEN_FREE_PROVIDER_ID)
        .unwrap()
        .expect("zen");
    assert_eq!(
        zen_free.auth_kind,
        ocg_domain::dynamic::DynamicAuthKind::None
    );
    assert_eq!(zen_free.offering, "api");
    let custom = db
        .get_provider_definition(CUSTOM_PROVIDER_ID)
        .unwrap()
        .expect("custom");
    assert!(custom.endpoint_url.is_empty());
    let invented: i64 = db
        .conn
        .query_row(
            "SELECT COUNT(*) FROM destinations WHERE legacy_kind = 'dynamic'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        invented, 0,
        "fresh open must not invent user-defined destinations"
    );

    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn v42_unifies_existing_dynamic_providers_and_preserves_models_and_preferences() {
    let dir = temp_data_dir("v42-unify");
    // Create a fresh v42 DB so the v42 schema is in place, then reverse the
    // migration to a v41 source carrying two dynamic Providers, one with a
    // plan preset and one without. Reopening triggers v42.
    let db = open_with_host_cipher(dir.clone()).unwrap();
    let now = Utc::now().to_rfc3339();
    db.conn
        .execute_batch(&format!(
            "PRAGMA foreign_keys=OFF;
             DROP TABLE IF EXISTS providers;
             DROP TABLE IF EXISTS provider_models;
             CREATE TABLE dynamic_providers (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                endpoint_url TEXT NOT NULL,
                upstream_protocol TEXT NOT NULL,
                auth_kind TEXT NOT NULL,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                preset_id TEXT
             );
             CREATE TABLE dynamic_provider_models (
                provider_id TEXT NOT NULL,
                public_model TEXT NOT NULL,
                public_model_key TEXT NOT NULL,
                upstream_model TEXT NOT NULL,
                upstream_override TEXT,
                PRIMARY KEY (provider_id, public_model_key),
                FOREIGN KEY (provider_id) REFERENCES dynamic_providers(id)
             );
             INSERT INTO dynamic_providers
                 (id, name, endpoint_url, upstream_protocol, auth_kind, created_at, updated_at, preset_id)
             VALUES
                 ('plan-lab', 'Plan Lab', 'https://plan.example/v1', 'chat_completions', 'bearer',
                  '{now}', '{now}', 'zhipu-coding'),
                 ('free-lab', 'Free Lab', 'https://free.example/v1', 'chat_completions', 'bearer',
                  '{now}', '{now}', NULL);
             INSERT INTO dynamic_provider_models
                 (provider_id, public_model, public_model_key, upstream_model, upstream_override)
             VALUES
                 ('plan-lab', 'lab-plan', 'lab-plan', 'plan/model', NULL),
                 ('free-lab', 'lab-free', 'lab-free', 'free/model', NULL);
             INSERT INTO provider_model_protocol_preferences
                 (provider_id, model_id, protocol)
             VALUES ('minimax', 'MiniMax-M2.5', 'chat_completions');
             DELETE FROM schema_version;
             INSERT INTO schema_version (version) VALUES (41);
             PRAGMA foreign_keys=ON;"
        ))
        .unwrap();
    drop(db);

    let db = open_with_host_cipher(dir.clone()).unwrap();
    assert_eq!(
        lifecycle::schema_version_on(&db.conn).unwrap(),
        CURRENT_SCHEMA_VERSION
    );
    assert!(!table_exists(&db.conn, "dynamic_providers").unwrap());
    assert!(!table_exists(&db.conn, "dynamic_provider_models").unwrap());
    assert_leftover_dynamic_provider_storage_absent(&db.conn);

    let plan_lab = db
        .get_dynamic_provider("plan-lab")
        .unwrap()
        .expect("plan-lab survived v42 through v56");
    assert_eq!(
        plan_lab.origin,
        ocg_domain::provider::ProviderOrigin::Preset
    );
    assert_eq!(plan_lab.name, "Plan Lab");
    assert_eq!(plan_lab.preset_id.as_deref(), Some("zhipu-coding"));
    assert_eq!(plan_lab.offering, "plan");
    assert_eq!(plan_lab.mappings.len(), 1);

    let free_lab = db
        .get_dynamic_provider("free-lab")
        .unwrap()
        .expect("free-lab survived v42 through v56");
    assert_eq!(
        free_lab.origin,
        ocg_domain::provider::ProviderOrigin::Custom
    );
    assert!(free_lab.preset_id.is_none());
    assert_eq!(free_lab.offering, "api");
    assert_eq!(free_lab.mappings.len(), 1);

    let pref_protocol: String = db
        .conn
        .query_row(
            "SELECT protocol FROM provider_model_protocol_preferences
             WHERE provider_id = 'minimax' AND model_id = 'MiniMax-M2.5'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(pref_protocol, "chat_completions");

    // The provider_id CHECK on preferences was removed; a non-minimax/kimi
    // provider_id must be insertable.
    db.conn
        .execute(
            "INSERT INTO provider_model_protocol_preferences (provider_id, model_id, protocol)
             VALUES ('opencode', 'some-model', 'chat_completions')",
            [],
        )
        .unwrap();

    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn v43_restores_auto_on_exclusive_cn_available_siblings() {
    let dir = temp_data_dir("v43-cn-exclusive");
    let db = open_with_host_cipher(dir.clone()).unwrap();
    db.conn
        .execute_batch(
            "INSERT INTO provider_contract_model_protocol_overrides
                (scope_kind, scope_id, model_id, protocol, state, updated_at)
             VALUES
                ('provider', 'minimax', 'MiniMax-M3', 'chat_completions', 'force_on', '2026-09-10T00:00:00Z'),
                ('provider', 'minimax', 'MiniMax-M3', 'messages', 'force_off', '2026-09-10T00:00:00Z'),
                ('provider', 'opencode', 'glm-5.2', 'chat_completions', 'force_on', '2026-09-10T00:00:00Z'),
                ('provider', 'opencode', 'glm-5.2', 'responses', 'force_off', '2026-09-10T00:00:00Z');
             DELETE FROM schema_version;
             INSERT INTO schema_version(version) VALUES (42);",
        )
        .unwrap();
    drop(db);

    let db = open_with_host_cipher(dir.clone()).unwrap();
    assert_eq!(
        lifecycle::schema_version_on(&db.conn).unwrap(),
        CURRENT_SCHEMA_VERSION
    );
    let cn_messages_off: i64 = db
        .conn
        .query_row(
            "SELECT COUNT(*) FROM provider_contract_model_protocol_overrides
             WHERE scope_id = 'minimax' AND model_id = 'MiniMax-M3' AND protocol = 'messages'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(cn_messages_off, 0);
    let go_responses_off: i64 = db
        .conn
        .query_row(
            "SELECT COUNT(*) FROM provider_contract_model_protocol_overrides
             WHERE scope_id = 'opencode' AND model_id = 'glm-5.2' AND protocol = 'responses'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(go_responses_off, 1);
    db.conn
        .execute(
            "INSERT INTO provider_model_protocol_preferences (provider_id, model_id, protocol)
             VALUES ('opencode', 'grok-4.6', 'responses')",
            [],
        )
        .unwrap();
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn v44_adds_dashboard_operations_on_v43_reopen_and_fresh_databases() {
    let fresh = temp_data_dir("v44-fresh");
    let db = open_with_host_cipher(fresh.clone()).unwrap();
    assert_eq!(
        lifecycle::schema_version_on(&db.conn).unwrap(),
        CURRENT_SCHEMA_VERSION
    );
    assert!(table_exists(&db.conn, "dashboard_operations").unwrap());
    drop(db);
    fs::remove_dir_all(fresh).unwrap();

    let dir = temp_data_dir("v44-from-v43");
    let db = open_with_host_cipher(dir.clone()).unwrap();
    db.conn
        .execute_batch(
            "DROP TABLE dashboard_operations;
             DELETE FROM schema_version;
             INSERT INTO schema_version(version) VALUES (43);",
        )
        .unwrap();
    assert_eq!(lifecycle::schema_version_on(&db.conn).unwrap(), 43);
    assert!(!table_exists(&db.conn, "dashboard_operations").unwrap());
    drop(db);

    let db = open_with_host_cipher(dir.clone()).unwrap();
    assert_eq!(
        lifecycle::schema_version_on(&db.conn).unwrap(),
        CURRENT_SCHEMA_VERSION
    );
    assert!(table_exists(&db.conn, "dashboard_operations").unwrap());
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn v45_fresh_database_is_current_and_v44_reopen_migrates() {
    let fresh = temp_data_dir("v45-fresh");
    let db = open_with_host_cipher(fresh.clone()).unwrap();
    assert_eq!(
        lifecycle::schema_version_on(&db.conn).unwrap(),
        CURRENT_SCHEMA_VERSION
    );
    assert_leftover_identity_tables_absent(&db.conn);
    for table in ["quota_pools", "quota_pool_members"] {
        assert!(table_exists(&db.conn, table).unwrap(), "{table}");
    }
    assert!(!table_exists(&db.conn, "accounts").unwrap());
    assert!(table_has_column(&db.conn, "credentials", "identity_id").unwrap());
    assert!(table_has_column(&db.conn, "credentials", "binding_id").unwrap());
    drop(db);
    fs::remove_dir_all(fresh).unwrap();

    let dir = temp_data_dir("v45-from-v44");
    let db = open_with_host_cipher(dir.clone()).unwrap();
    rewind_identity_model_to_v44(&db.conn);
    assert_eq!(lifecycle::schema_version_on(&db.conn).unwrap(), 44);
    assert!(!table_exists(&db.conn, "upstream_identities").unwrap());
    drop(db);

    let db = open_with_host_cipher(dir.clone()).unwrap();
    assert_eq!(
        lifecycle::schema_version_on(&db.conn).unwrap(),
        CURRENT_SCHEMA_VERSION
    );
    assert_leftover_identity_tables_absent(&db.conn);
    assert!(table_exists(&db.conn, "quota_pools").unwrap());
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn identity_backfill_fails_closed_when_required_account_columns_are_missing() {
    let dir = temp_data_dir("identity-missing-provider-id");
    let db = open_with_host_cipher(dir.clone()).unwrap();
    rewind_identity_model_to_v44(&db.conn);
    db.conn
        .execute_batch("ALTER TABLE accounts DROP COLUMN provider_id;")
        .unwrap();
    assert_eq!(lifecycle::schema_version_on(&db.conn).unwrap(), 44);
    drop(db);

    let error = match open_with_host_cipher(dir.clone()) {
        Ok(_) => panic!("missing required account columns must fail closed"),
        Err(error) => error,
    };
    let message = format!("{error:#}");
    assert!(
        message.contains("provider_id") || message.contains("no such column"),
        "{message}"
    );
    let conn = Connection::open(dir.join("data.sqlite")).unwrap();
    assert_eq!(lifecycle::schema_version_on(&conn).unwrap(), 44);
    assert!(!table_exists(&conn, "upstream_identities").unwrap());
    drop(conn);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn v45_migrates_legacy_accounts_idempotently_without_changing_v3_rows() {
    use crate::platform::{PlatformGroup, PlatformKind};
    use ocg_domain::connection::{LegacyConnectionKind, connection_id_for_legacy};
    use ocg_domain::credential::{
        IdentityConfidence, anonymous_binding_id_for, credential_id_for_legacy_account,
        identity_id_for_legacy_account, identity_id_for_platform_account,
    };

    let dir = temp_data_dir("v45-legacy-fixture");
    let db = open_with_host_cipher(dir.clone()).unwrap();
    rewind_identity_model_to_v44(&db.conn);

    let now = Utc::now();
    let cooldown_5h = now + chrono::Duration::hours(5);
    let cooldown_week = now + chrono::Duration::days(7);
    let cooldown_5h_text = cooldown_5h.to_rfc3339();
    let cooldown_week_text = cooldown_week.to_rfc3339();

    let provider_id = uuid::Uuid::new_v4().to_string();
    let runtime = crate::dynamic::DynamicProviderRuntime {
        preset_id: None,
        id: provider_id.clone(),
        name: "Dynamic Lab".into(),
        endpoint_url: "https://dyn.example/v1/chat/completions".into(),
        upstream_protocol: crate::provider::UpstreamProtocolKind::ChatCompletions,
        auth_kind: ocg_domain::dynamic::DynamicAuthKind::Bearer,
        mappings: vec![ocg_domain::dynamic::DynamicModelMapping {
            public_model: "lab-opus".into(),
            upstream_model: "vendor/opus".into(),
            upstream_override: None,
        }],
        created_at: now,
        updated_at: now,
        origin: ocg_domain::provider::ProviderOrigin::Custom,
        offering: "api".to_string(),
    };
    let mut dynamic = account("dyn-keyed");
    dynamic.provider_id = provider_id.clone();
    dynamic.name = "Dynamic Key".into();
    dynamic.key_cipher = fixture_account_key_cipher();
    db.create_dynamic_provider(&runtime, &dynamic).unwrap();

    let mut custom = account("custom-keyed");
    custom.provider_id = CUSTOM_PROVIDER_ID.into();
    custom.name = "Custom Key".into();
    custom.key_cipher = fixture_account_key_cipher();
    db.create_account_with_contract(
        &custom,
        Some(&AccountCustomConfigInput {
            endpoint_url: "https://custom.example/v1/chat/completions".into(),
            upstream_protocol: UpstreamProtocolKind::ChatCompletions,
        }),
        &[AccountModelCapabilityInput {
            public_model: "custom-model".into(),
            upstream_model: "custom-model".into(),
            protocol: UpstreamProtocolKind::ChatCompletions,
            source: None,
        }],
    )
    .unwrap();

    let mut builtin = account("go-keyed");
    builtin.name = "Go Key".into();
    builtin.enabled = false;
    builtin.auth_error = Some("auth failed".into());
    builtin.key_cipher = fixture_account_key_cipher();
    builtin.cooldown_5h_until = Some(cooldown_5h);
    builtin.cooldown_week_until = Some(cooldown_week);
    db.create_account(&builtin).unwrap();

    let mut managed = account("managed-draft");
    managed.name = "Managed Draft".into();
    managed.account_type = AccountType::Managed;
    managed.setup_step = AccountSetupStep::Payment;
    managed.enabled = false;
    managed.key_cipher.clear();
    db.create_account(&managed).unwrap();

    db.create_platform_account(
        "parent-1",
        PlatformKind::NewApi,
        "Parent",
        "https://new.example/v1",
        Some("obfuscated-test-credential"),
    )
    .unwrap();
    db.link_platform_account("custom-keyed", "parent-1", &PlatformGroup::default())
        .unwrap();

    db.conn
        .execute(
            "UPDATE accounts SET sort_order = CASE id
                WHEN 'dyn-keyed' THEN 0
                WHEN 'custom-keyed' THEN 1
                WHEN 'go-keyed' THEN 2
                WHEN 'managed-draft' THEN 3
                ELSE sort_order END,
                cooldown_5h_until = CASE WHEN id = 'go-keyed' THEN ?1 ELSE cooldown_5h_until END,
                cooldown_week_until = CASE WHEN id = 'go-keyed' THEN ?2 ELSE cooldown_week_until END,
                auth_error = CASE WHEN id = 'go-keyed' THEN 'auth failed' ELSE auth_error END,
                enabled = CASE WHEN id = 'go-keyed' THEN 0 ELSE enabled END",
            params![cooldown_5h_text, cooldown_week_text],
        )
        .unwrap();

    let before = db.list_accounts().unwrap();
    assert_eq!(lifecycle::schema_version_on(&db.conn).unwrap(), 44);
    drop(db);

    let db = open_with_host_cipher(dir.clone()).unwrap();
    assert_eq!(
        lifecycle::schema_version_on(&db.conn).unwrap(),
        CURRENT_SCHEMA_VERSION
    );
    let after = db.list_accounts().unwrap();
    assert_eq!(
        serde_json::to_value(&before).unwrap(),
        serde_json::to_value(&after).unwrap()
    );

    let account_count: i64 = db
        .conn
        .query_row(
            "SELECT COUNT(*) FROM credentials
             WHERE COALESCE(credential_purpose, 'inference') = 'inference'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let identity_count: i64 = db
        .conn
        .query_row(
            "SELECT COUNT(DISTINCT identity_id) FROM credentials WHERE identity_id IS NOT NULL",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let credential_count: i64 = db
        .conn
        .query_row(
            "SELECT COUNT(*) FROM credentials
             WHERE COALESCE(credential_purpose, 'inference') = 'inference'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let binding_count: i64 = db
        .conn
        .query_row(
            "SELECT COUNT(*) FROM credentials
             WHERE COALESCE(credential_purpose, 'inference') = 'inference'
               AND binding_id IS NOT NULL AND binding_id <> ''",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(credential_count, account_count);
    assert_eq!(binding_count, account_count);
    assert_eq!(identity_count, account_count + 1);

    for id in [
        "go-keyed",
        "dyn-keyed",
        "custom-keyed",
        "managed-draft",
        ZEN_FREE_ACCOUNT_ID,
    ] {
        let identity = identity_id_for_legacy_account(id);
        let credential = credential_id_for_legacy_account(id);
        let stored_identity: String = db
            .conn
            .query_row(
                "SELECT identity_id FROM credentials WHERE legacy_account_id = ?1",
                [id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(stored_identity, identity.as_str(), "{id}");
        let stored_credential: String = db
            .conn
            .query_row(
                "SELECT id FROM credentials WHERE legacy_account_id = ?1",
                [id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(stored_credential, credential.as_str(), "{id}");
        let binding: Option<String> = db
            .conn
            .query_row(
                "SELECT binding_id FROM credentials WHERE legacy_account_id = ?1",
                [id],
                |row| row.get(0),
            )
            .unwrap();
        assert!(
            binding.as_deref().is_some_and(|value| !value.is_empty()),
            "{id}"
        );
    }

    let go_sort: i64 = db
        .conn
        .query_row(
            "SELECT routing_rank FROM credentials WHERE legacy_account_id = 'go-keyed'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(go_sort, 2);
    let stored_5h: String = db
        .conn
        .query_row(
            "SELECT cooldown_5h_until FROM credentials WHERE legacy_account_id = 'go-keyed'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let stored_week: String = db
        .conn
        .query_row(
            "SELECT cooldown_week_until FROM credentials WHERE legacy_account_id = 'go-keyed'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(stored_5h, cooldown_5h_text);
    assert_eq!(stored_week, cooldown_week_text);

    let snapshot = db.list_identity_model().unwrap();
    let task = snapshot
        .accounts
        .iter()
        .find(|row| row.account.id == "managed-draft")
        .and_then(|row| row.onboarding.as_ref())
        .expect("managed onboarding");
    assert_eq!(
        (task.step.as_str(), task.state.as_str()),
        ("payment", "in_progress")
    );
    let ready_tasks = snapshot
        .accounts
        .iter()
        .filter(|row| row.account.id != "managed-draft" && row.onboarding.is_some())
        .count();
    assert_eq!(ready_tasks, 0);

    let subscriptions: Vec<String> = snapshot
        .accounts
        .iter()
        .filter(|row| row.subscription.is_some())
        .map(|row| row.account.id.clone())
        .collect();
    assert_eq!(subscriptions, vec!["go-keyed".to_string()]);

    let custom_identity: (String, Option<String>) = db
        .conn
        .query_row(
            "SELECT identity_confidence, authority_site
             FROM credentials
             WHERE legacy_account_id = 'custom-keyed'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(custom_identity.0, IdentityConfidence::Declared.as_str());
    let stored_parent_base: String = db
        .conn
        .query_row(
            "SELECT base_url FROM destinations
             WHERE legacy_kind = 'platform_parent' AND legacy_id = 'parent-1'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        custom_identity.1.as_deref(),
        Some(stored_parent_base.as_str())
    );

    let platform_identity = identity_id_for_platform_account("parent-1");
    let platform_parent = snapshot
        .platform_parents
        .iter()
        .find(|row| row.platform_id == "parent-1")
        .expect("platform parent identity");
    assert_eq!(platform_parent.identity.id, platform_identity.as_str());

    let quota_pools: i64 = db
        .conn
        .query_row("SELECT COUNT(*) FROM quota_pools", [], |row| row.get(0))
        .unwrap();
    assert_eq!(quota_pools, account_count);
    let quota_members: i64 = db
        .conn
        .query_row("SELECT COUNT(*) FROM quota_pool_members", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(quota_members, account_count);
    for id in [
        "go-keyed",
        "dyn-keyed",
        "custom-keyed",
        "managed-draft",
        ZEN_FREE_ACCOUNT_ID,
    ] {
        let (subject_ref, confidence, mode, members): (String, String, String, i64) = db
            .conn
            .query_row(
                "SELECT p.subject_ref, p.relation_confidence, p.policy_mode, COUNT(m.account_id)
                 FROM quota_pools p
                 JOIN credentials a ON a.identity_id = p.subject_ref
                 JOIN quota_pool_members m ON m.pool_id = p.id
                 WHERE a.legacy_account_id = ?1
                 GROUP BY p.id",
                [id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        let identity = identity_id_for_legacy_account(id);
        assert_eq!(subject_ref, identity.as_str(), "{id}");
        assert_eq!(confidence, "unknown", "{id}");
        assert_eq!(mode, "authoritative_limit", "{id}");
        assert_eq!(members, 1, "{id}");
    }

    let zen_connection = connection_id_for_legacy(
        LegacyConnectionKind::BuiltinProvider,
        OPENCODE_ZEN_FREE_PROVIDER_ID,
    );
    let stored_zen_binding: String = db
        .conn
        .query_row(
            "SELECT binding_id FROM credentials WHERE legacy_account_id = ?1",
            [ZEN_FREE_ACCOUNT_ID],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        stored_zen_binding,
        anonymous_binding_id_for(&zen_connection).as_str()
    );

    let before_second = (
        identity_count,
        credential_count,
        binding_count,
        db.list_identity_model().unwrap().accounts.len(),
    );
    {
        let tx = db.conn.unchecked_transaction().unwrap();
        crate::db::identity::migrate_v45_body(&tx).unwrap();
        crate::db::identity::migrate_v45_body(&tx).unwrap();
        tx.commit().unwrap();
    }
    assert_leftover_identity_tables_absent(&db.conn);
    let after_second = (
        db.conn
            .query_row(
                "SELECT COUNT(DISTINCT identity_id) FROM credentials WHERE identity_id IS NOT NULL",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        db.conn
            .query_row(
                "SELECT COUNT(*) FROM credentials
                 WHERE COALESCE(credential_purpose, 'inference') = 'inference'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        db.conn
            .query_row(
                "SELECT COUNT(*) FROM credentials
                 WHERE COALESCE(credential_purpose, 'inference') = 'inference'
                   AND binding_id IS NOT NULL AND binding_id <> ''",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        db.list_identity_model().unwrap().accounts.len(),
    );
    assert_eq!(before_second, after_second);

    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn v45_delete_linked_key_keeps_platform_parent_identity() {
    use crate::platform::{PlatformGroup, PlatformKind};
    use ocg_domain::credential::identity_id_for_platform_account;

    let dir = temp_data_dir("v45-delete-keeps-parent");
    let mut db = open_with_host_cipher(dir.clone()).unwrap();
    let mut custom = account("linked-custom");
    custom.provider_id = CUSTOM_PROVIDER_ID.into();
    custom.key_cipher = fixture_account_key_cipher();
    db.create_account_with_contract(
        &custom,
        Some(&AccountCustomConfigInput {
            endpoint_url: "https://custom.example/v1/chat/completions".into(),
            upstream_protocol: UpstreamProtocolKind::ChatCompletions,
        }),
        &[AccountModelCapabilityInput {
            public_model: "keep-parent".into(),
            upstream_model: "keep-parent".into(),
            protocol: UpstreamProtocolKind::ChatCompletions,
            source: None,
        }],
    )
    .unwrap();
    db.create_platform_account(
        "keep-parent",
        PlatformKind::NewApi,
        "Keep Parent",
        "https://keep.example/v1",
        Some("obfuscated-test-credential"),
    )
    .unwrap();
    db.link_platform_account("linked-custom", "keep-parent", &PlatformGroup::default())
        .unwrap();
    let parent_identity = identity_id_for_platform_account("keep-parent");
    db.delete_account("linked-custom").unwrap();
    let parent = db
        .list_identity_model()
        .unwrap()
        .platform_parents
        .into_iter()
        .find(|row| row.platform_id == "keep-parent")
        .expect("platform parent identity survives");
    assert_eq!(parent.identity.id, parent_identity.as_str());
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn v45_unlink_returns_identity_to_opaque() {
    use crate::platform::{PlatformGroup, PlatformKind};
    use ocg_domain::credential::IdentityConfidence;

    let dir = temp_data_dir("v45-unlink-opaque");
    let db = open_with_host_cipher(dir.clone()).unwrap();
    let mut custom = account("unlink-custom");
    custom.provider_id = CUSTOM_PROVIDER_ID.into();
    custom.key_cipher = fixture_account_key_cipher();
    db.create_account_with_contract(
        &custom,
        Some(&AccountCustomConfigInput {
            endpoint_url: "https://custom.example/v1/chat/completions".into(),
            upstream_protocol: UpstreamProtocolKind::ChatCompletions,
        }),
        &[AccountModelCapabilityInput {
            public_model: "unlink-model".into(),
            upstream_model: "unlink-model".into(),
            protocol: UpstreamProtocolKind::ChatCompletions,
            source: None,
        }],
    )
    .unwrap();
    db.create_platform_account(
        "unlink-parent",
        PlatformKind::NewApi,
        "Unlink Parent",
        "https://unlink.example/v1",
        Some("obfuscated-test-credential"),
    )
    .unwrap();
    db.link_platform_account("unlink-custom", "unlink-parent", &PlatformGroup::default())
        .unwrap();
    db.unlink_platform_account("unlink-custom").unwrap();
    let state: (String, Option<String>) = db
        .conn
        .query_row(
            "SELECT identity_confidence, authority_site
             FROM credentials
             WHERE legacy_account_id = 'unlink-custom'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(state.0, IdentityConfidence::Opaque.as_str());
    assert!(state.1.is_none());
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn v45_open_repairs_missing_satellites_and_list_fails_closed() {
    let dir = temp_data_dir("v45-repair-fail-closed");
    let db = open_with_host_cipher(dir.clone()).unwrap();
    let mut keyed = account("repair-go");
    keyed.key_cipher = fixture_account_key_cipher();
    db.create_account(&keyed).unwrap();
    db.conn
        .execute(
            "UPDATE credentials SET identity_id = NULL WHERE legacy_account_id = 'repair-go'",
            [],
        )
        .unwrap();
    db.conn
        .execute(
            "UPDATE credentials SET binding_id = NULL WHERE legacy_account_id = 'repair-go'",
            [],
        )
        .unwrap();
    let listed = db.list_identity_model();
    assert!(
        listed.is_err(),
        "list must not synthesize missing v45 satellites"
    );
    drop(db);

    let db = open_with_host_cipher(dir.clone()).unwrap();
    let snapshot = db.list_identity_model().unwrap();
    assert!(
        snapshot
            .accounts
            .iter()
            .any(|record| record.account.id == "repair-go" && !record.identity_id.is_empty())
    );
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn v47_adds_onboarding_draft_to_existing_v46_rows() {
    let dir = temp_data_dir("v47-from-v46");
    let db = open_with_host_cipher(dir.clone()).unwrap();
    restore_v47_inert_columns(&db.conn);
    materialize_legacy_providers_for_rewind(&db.conn);
    db.conn
        .execute_batch(
            "ALTER TABLE providers DROP COLUMN onboarding_draft;
             DELETE FROM schema_version;
             INSERT INTO schema_version(version) VALUES (46);",
        )
        .unwrap();
    assert_eq!(lifecycle::schema_version_on(&db.conn).unwrap(), 46);
    assert!(!table_has_column(&db.conn, "providers", "onboarding_draft").unwrap());
    drop(db);

    let db = open_with_host_cipher(dir.clone()).unwrap();
    assert_eq!(
        lifecycle::schema_version_on(&db.conn).unwrap(),
        CURRENT_SCHEMA_VERSION
    );
    assert_leftover_dynamic_provider_storage_absent(&db.conn);
    let defaulted: i64 = db
        .conn
        .query_row(
            "SELECT COUNT(*) FROM destinations
             WHERE legacy_kind = 'dynamic' AND COALESCE(onboarding_draft, 0) != 0",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(defaulted, 0, "existing rows migrate to configured");
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn v49_adds_unpublished_public_models_on_v48_reopen_and_fresh_databases() {
    let fresh = temp_data_dir("v49-fresh");
    let db = open_with_host_cipher(fresh.clone()).unwrap();
    assert_eq!(
        lifecycle::schema_version_on(&db.conn).unwrap(),
        CURRENT_SCHEMA_VERSION
    );
    assert!(table_exists(&db.conn, "unpublished_public_models").unwrap());
    assert!(db.list_unpublished_public_models().unwrap().is_empty());
    drop(db);
    fs::remove_dir_all(fresh).unwrap();

    let dir = temp_data_dir("v49-from-v48");
    let db = open_with_host_cipher(dir.clone()).unwrap();
    db.conn
        .execute_batch(
            "DROP TABLE unpublished_public_models;
             DELETE FROM schema_version;
             INSERT INTO schema_version(version) VALUES (48);",
        )
        .unwrap();
    assert_eq!(lifecycle::schema_version_on(&db.conn).unwrap(), 48);
    assert!(!table_exists(&db.conn, "unpublished_public_models").unwrap());
    drop(db);

    let db = open_with_host_cipher(dir.clone()).unwrap();
    assert_eq!(
        lifecycle::schema_version_on(&db.conn).unwrap(),
        CURRENT_SCHEMA_VERSION
    );
    assert!(table_exists(&db.conn, "unpublished_public_models").unwrap());
    db.upsert_unpublished_public_model("deepseek-v4-flashnh")
        .unwrap();
    assert_eq!(
        db.list_unpublished_public_models().unwrap(),
        vec!["deepseek-v4-flashnh".to_string()]
    );
    db.remove_unpublished_public_model("deepseek-v4-flashnh")
        .unwrap();
    assert!(db.list_unpublished_public_models().unwrap().is_empty());
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn v50_adds_destination_shadow_tables_on_v49_reopen_and_fresh_databases() {
    let shadow_tables = [
        "destinations",
        "destination_models",
        "credentials",
        "credential_grants",
    ];

    let fresh = temp_data_dir("v50-fresh");
    let db = open_with_host_cipher(fresh.clone()).unwrap();
    assert_eq!(
        lifecycle::schema_version_on(&db.conn).unwrap(),
        CURRENT_SCHEMA_VERSION
    );
    for table in shadow_tables {
        assert!(table_exists(&db.conn, table).unwrap(), "{table}");
    }
    drop(db);
    fs::remove_dir_all(fresh).unwrap();

    let dir = temp_data_dir("v50-from-v49");
    let db = open_with_host_cipher(dir.clone()).unwrap();
    db.conn
        .execute_batch(
            "DROP TABLE credential_grants;
             DROP TABLE credentials;
             DROP TABLE destination_models;
             DROP TABLE destinations;
             DELETE FROM schema_version;
             INSERT INTO schema_version(version) VALUES (49);",
        )
        .unwrap();
    assert_eq!(lifecycle::schema_version_on(&db.conn).unwrap(), 49);
    for table in shadow_tables {
        assert!(!table_exists(&db.conn, table).unwrap(), "{table}");
    }
    drop(db);

    let db = open_with_host_cipher(dir.clone()).unwrap();
    assert_eq!(
        lifecycle::schema_version_on(&db.conn).unwrap(),
        CURRENT_SCHEMA_VERSION
    );
    for table in shadow_tables {
        assert!(table_exists(&db.conn, table).unwrap(), "{table}");
    }
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn v52_migrates_v51_fixture_and_drops_accounts() {
    let dir = temp_data_dir("v52-from-v51");
    let db = open_with_host_cipher(dir.clone()).unwrap();
    let mut keyed = account("v51-keyed");
    keyed.key_cipher = fixture_account_key_cipher();
    keyed.enabled = true;
    keyed.notes = Some("keep-notes".into());
    db.create_account(&keyed).unwrap();
    db.update_account(
        "v51-keyed",
        &AccountUpdate {
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
    let before = db.get_account("v51-keyed").unwrap().unwrap();
    account_store::materialize_legacy_accounts_for_rewind(&db.conn).unwrap();
    db.conn
        .execute_batch(
            "DELETE FROM schema_version;
             INSERT INTO schema_version(version) VALUES (51);",
        )
        .unwrap();
    assert_eq!(lifecycle::schema_version_on(&db.conn).unwrap(), 51);
    assert!(accounts_table_present(&db.conn));
    drop(db);

    let db = open_with_host_cipher(dir.clone()).unwrap();
    assert_eq!(
        lifecycle::schema_version_on(&db.conn).unwrap(),
        CURRENT_SCHEMA_VERSION
    );
    assert!(!accounts_table_present(&db.conn));
    let after = db.get_account("v51-keyed").unwrap().expect("reconstructed");
    assert_eq!(after.enabled, before.enabled);
    assert_eq!(after.notes, before.notes);
    assert_eq!(after.key_cipher, before.key_cipher);
    let listed = db.list_accounts().unwrap();
    assert!(listed.iter().any(|row| row.id == "v51-keyed"));
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn fresh_open_is_schema_v58_without_leftover_tables_and_keeps_zen() {
    let dir = temp_data_dir("v58-fresh");
    let db = open_with_host_cipher(dir.clone()).unwrap();
    assert_eq!(
        lifecycle::schema_version_on(&db.conn).unwrap(),
        crate::db::CURRENT_SCHEMA_VERSION
    );
    assert!(table_has_column(&db.conn, "destinations", "model_resolution").unwrap());
    assert!(!accounts_table_present(&db.conn));
    assert!(!table_exists(&db.conn, "account_custom_configs").unwrap());
    assert!(!table_exists(&db.conn, "account_model_capabilities").unwrap());
    assert!(!table_exists(&db.conn, "platform_accounts").unwrap());
    assert!(!table_exists(&db.conn, "platform_links").unwrap());
    assert!(!table_exists(&db.conn, "cpa_integration").unwrap());
    assert_leftover_dynamic_provider_storage_absent(&db.conn);
    assert_leftover_identity_tables_absent(&db.conn);
    assert!(table_exists(&db.conn, "quota_pools").unwrap());
    assert!(table_exists(&db.conn, "quota_pool_members").unwrap());
    let identity_snapshot = db.list_identity_model().unwrap();
    assert!(
        identity_snapshot
            .accounts
            .iter()
            .any(|row| row.account.id == ZEN_FREE_ACCOUNT_ID)
    );
    assert!(
        identity_snapshot
            .accounts
            .iter()
            .all(|row| row.account.id == ZEN_FREE_ACCOUNT_ID)
    );
    let zen = db
        .get_account(ZEN_FREE_ACCOUNT_ID)
        .unwrap()
        .expect("zen credential");
    assert_eq!(zen.id, ZEN_FREE_ACCOUNT_ID);
    let listed = db.list_accounts().unwrap();
    assert!(listed.iter().any(|row| row.id == ZEN_FREE_ACCOUNT_ID));
    assert!(listed.iter().all(|row| row.id != CPA_ACCOUNT_ID));
    assert!(db.cpa_integration().unwrap().is_none());
    let cpa_dest: i64 = db
        .conn
        .query_row(
            "SELECT COUNT(*) FROM destinations
             WHERE adapter = 'cpa'
                OR (legacy_kind = 'builtin' AND legacy_id = 'cpa')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(cpa_dest, 0, "fresh open must not invent a CPA destination");
    let invented: i64 = db
        .conn
        .query_row(
            "SELECT COUNT(*) FROM destinations WHERE legacy_kind = 'dynamic'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        invented, 0,
        "fresh open must not invent a user-defined destination"
    );
    for builtin_id in [
        OPENCODE_PROVIDER_ID,
        OPENCODE_ZEN_FREE_PROVIDER_ID,
        COMMAND_CODE_PROVIDER_ID,
        MINIMAX_PROVIDER_ID,
        KIMI_PROVIDER_ID,
        OLLAMA_PROVIDER_ID,
        CUSTOM_PROVIDER_ID,
    ] {
        assert!(
            db.get_provider_definition(builtin_id).unwrap().is_some(),
            "{builtin_id}"
        );
    }
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn v53_migrates_v52_custom_account_and_drops_leftover_tables() {
    let dir = temp_data_dir("v53-from-v52-custom");
    let db = Database::open(dir.clone()).unwrap();
    let mut custom = account("custom-v52");
    custom.provider_id = CUSTOM_PROVIDER_ID.to_string();
    custom.enabled = false;
    custom.credential_kind = CredentialKind::ApiKey;
    custom.quota_scope = QuotaScope::Key;
    db.create_account_with_contract(
        &custom,
        Some(&AccountCustomConfigInput {
            endpoint_url: "https://api.example.com/v1/chat/completions".into(),
            upstream_protocol: UpstreamProtocolKind::ChatCompletions,
        }),
        &[AccountModelCapabilityInput {
            public_model: "org/model".into(),
            upstream_model: "org/upstream".into(),
            protocol: UpstreamProtocolKind::ChatCompletions,
            source: Some("manual".into()),
        }],
    )
    .unwrap();
    let before = db.account_custom_config("custom-v52").unwrap().unwrap();
    let before_caps = db.list_account_model_capabilities("custom-v52").unwrap();
    db.conn
        .execute_batch(
            "CREATE TABLE account_custom_configs (
                account_id TEXT PRIMARY KEY,
                endpoint_url TEXT NOT NULL,
                upstream_protocol TEXT NOT NULL,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
             );
             CREATE TABLE account_model_capabilities (
                account_id TEXT NOT NULL,
                model_id TEXT NOT NULL,
                upstream_model TEXT NOT NULL,
                protocol TEXT NOT NULL,
                verified_at TEXT,
                source TEXT NOT NULL DEFAULT 'manual',
                PRIMARY KEY (account_id, model_id, protocol)
             );",
        )
        .unwrap();
    db.conn
        .execute(
            "INSERT INTO account_custom_configs (
                account_id, endpoint_url, upstream_protocol, created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?4)",
            rusqlite::params![
                "custom-v52",
                before.endpoint_url,
                before.upstream_protocol.as_str(),
                "2026-01-01T00:00:00Z",
            ],
        )
        .unwrap();
    db.conn
        .execute(
            "INSERT INTO account_model_capabilities (
                account_id, model_id, upstream_model, protocol, source
             ) VALUES (?1, ?2, ?3, ?4, 'manual')",
            rusqlite::params![
                "custom-v52",
                before_caps[0].public_model,
                before_caps[0].upstream_model,
                before_caps[0].protocol.as_str(),
            ],
        )
        .unwrap();
    db.conn
        .execute(
            "UPDATE destinations SET base_url = NULL, protocols_json = '[]'
             WHERE legacy_kind = 'custom_account' AND legacy_id = 'custom-v52'",
            [],
        )
        .unwrap();
    db.conn
        .execute(
            "DELETE FROM destination_models WHERE destination_id = (
                SELECT id FROM destinations
                 WHERE legacy_kind = 'custom_account' AND legacy_id = 'custom-v52'
             )",
            [],
        )
        .unwrap();
    db.conn
        .execute_batch(
            "DELETE FROM schema_version;
             INSERT INTO schema_version(version) VALUES (52);",
        )
        .unwrap();
    assert_eq!(lifecycle::schema_version_on(&db.conn).unwrap(), 52);
    assert!(table_exists(&db.conn, "account_custom_configs").unwrap());
    drop(db);

    let db = Database::open(dir.clone()).unwrap();
    assert_eq!(
        lifecycle::schema_version_on(&db.conn).unwrap(),
        CURRENT_SCHEMA_VERSION
    );
    let leftover: i64 = db
        .conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master
             WHERE type = 'table'
               AND name IN ('account_custom_configs', 'account_model_capabilities')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(leftover, 0);
    let after = db.account_custom_config("custom-v52").unwrap().unwrap();
    assert_eq!(after.endpoint_url, before.endpoint_url);
    assert_eq!(after.upstream_protocol, before.upstream_protocol);
    let after_caps = db.list_account_model_capabilities("custom-v52").unwrap();
    assert_eq!(after_caps.len(), 1);
    assert_eq!(after_caps[0].public_model, before_caps[0].public_model);
    assert_eq!(after_caps[0].upstream_model, before_caps[0].upstream_model);
    assert_eq!(after_caps[0].protocol, before_caps[0].protocol);
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn v53_keeps_both_keys_models_when_two_linked_keys_share_a_platform() {
    use ocg_domain::credential::ModelScope;

    let dir = temp_data_dir("v53-two-linked-keys");
    let db = Database::open(dir.clone()).unwrap();
    seed_linked_platform_keys(
        &db,
        "parent-shared",
        &[("key-a", "model-x", "up-x"), ("key-b", "model-y", "up-y")],
    );
    force_binding_scope_all(&db, "key-a");
    force_binding_scope_all(&db, "key-b");
    rewind_linked_keys_to_v52_leftover_capabilities(
        &db,
        "parent-shared",
        &[("key-a", "model-x", "up-x"), ("key-b", "model-y", "up-y")],
    );
    drop(db);

    let db = Database::open(dir.clone()).unwrap();
    assert_eq!(
        lifecycle::schema_version_on(&db.conn).unwrap(),
        CURRENT_SCHEMA_VERSION
    );
    assert_eq!(
        parent_catalog_pairs(&db, "parent-shared"),
        vec![
            ("model-x".into(), "up-x".into()),
            ("model-y".into(), "up-y".into()),
        ]
    );
    assert_eq!(
        capability_pairs(&db, "key-a"),
        vec![("model-x".into(), "up-x".into())]
    );
    assert_eq!(
        stored_model_scope(&db, "key-a"),
        ModelScope::Only {
            models: vec!["model-x".into()]
        }
    );
    assert_key_cannot_serve(&db, "key-a", "model-y");
    drop(db);

    let db = Database::open(dir.clone()).unwrap();
    assert_eq!(
        capability_pairs(&db, "key-b"),
        vec![("model-y".into(), "up-y".into())]
    );
    assert_eq!(
        stored_model_scope(&db, "key-b"),
        ModelScope::Only {
            models: vec!["model-y".into()]
        }
    );
    assert_key_cannot_serve(&db, "key-b", "model-x");
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn v53_empty_leftover_key_stays_ineligible_for_sibling_models() {
    use ocg_domain::credential::ModelScope;

    let dir = temp_data_dir("v53-empty-leftover-key");
    let db = Database::open(dir.clone()).unwrap();
    seed_linked_platform_keys(
        &db,
        "parent-empty",
        &[("key-a", "model-a", "up-a"), ("key-b", "model-b", "up-b")],
    );
    force_binding_scope_all(&db, "key-a");
    force_binding_scope_all(&db, "key-b");
    rewind_linked_keys_to_v52_leftover_capabilities(
        &db,
        "parent-empty",
        &[("key-b", "model-b", "up-b")],
    );
    drop(db);

    let db = Database::open(dir.clone()).unwrap();
    assert_eq!(
        lifecycle::schema_version_on(&db.conn).unwrap(),
        CURRENT_SCHEMA_VERSION
    );
    assert_eq!(
        parent_catalog_pairs(&db, "parent-empty"),
        vec![("model-b".into(), "up-b".into())]
    );
    assert_eq!(
        stored_model_scope(&db, "key-a"),
        ModelScope::Only { models: vec![] }
    );
    assert!(capability_pairs(&db, "key-a").is_empty());
    assert_key_cannot_serve(&db, "key-a", "model-b");
    assert_eq!(
        stored_model_scope(&db, "key-b"),
        ModelScope::Only {
            models: vec!["model-b".into()]
        }
    );
    assert_eq!(
        capability_pairs(&db, "key-b"),
        vec![("model-b".into(), "up-b".into())]
    );
    drop(db);

    let db = Database::open(dir.clone()).unwrap();
    assert_eq!(
        stored_model_scope(&db, "key-a"),
        ModelScope::Only { models: vec![] }
    );
    assert!(capability_pairs(&db, "key-a").is_empty());
    assert_key_cannot_serve(&db, "key-a", "model-b");
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn v53_intersects_existing_only_scope_instead_of_widening() {
    use ocg_domain::credential::ModelScope;

    let dir = temp_data_dir("v53-keep-manual-scope");
    let db = Database::open(dir.clone()).unwrap();
    seed_linked_platform_keys(
        &db,
        "parent-narrow",
        &[("key-a", "model-a", "up-a"), ("key-b", "model-b", "up-b")],
    );
    db.conn
        .execute(
            r#"UPDATE credentials SET scope_json = ?2 WHERE legacy_account_id = ?1"#,
            rusqlite::params![
                "key-a",
                serde_json::to_string(&ModelScope::Only {
                    models: vec!["model-a".into()],
                })
                .unwrap(),
            ],
        )
        .unwrap();
    rewind_linked_keys_to_v52_leftover_capabilities(
        &db,
        "parent-narrow",
        &[
            ("key-a", "model-a", "up-a"),
            ("key-a", "model-extra", "up-extra"),
            ("key-b", "model-b", "up-b"),
        ],
    );
    drop(db);

    let db = Database::open(dir.clone()).unwrap();
    assert_eq!(
        stored_model_scope(&db, "key-a"),
        ModelScope::Only {
            models: vec!["model-a".into()]
        }
    );
    assert_eq!(
        capability_pairs(&db, "key-a"),
        vec![("model-a".into(), "up-a".into())]
    );
    assert_key_cannot_serve(&db, "key-a", "model-extra");
    assert_key_cannot_serve(&db, "key-a", "model-b");
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn v53_refuses_conflicting_upstream_maps_for_the_same_platform_model() {
    use crate::platform::{PlatformGroup, PlatformKind};
    use ocg_domain::destination::destination_id_for_platform_account;

    let dir = temp_data_dir("v53-conflict-models");
    let db = Database::open(dir.clone()).unwrap();
    leftover_custom_account(&db, "key-a", "cipher-a", "shared", "up-a");
    leftover_custom_account(&db, "key-b", "cipher-b", "shared", "up-b");
    db.create_platform_account(
        "parent-conflict",
        PlatformKind::NewApi,
        "Conflict Parent",
        "https://platform.example/v1",
        Some("mgmt-cipher"),
    )
    .unwrap();
    db.link_platform_account("key-a", "parent-conflict", &PlatformGroup::default())
        .unwrap();
    db.link_platform_account("key-b", "parent-conflict", &PlatformGroup::default())
        .unwrap();
    let dest_id = destination_id_for_platform_account("parent-conflict");
    db.conn
        .execute_batch(
            "CREATE TABLE account_model_capabilities (
                account_id TEXT NOT NULL,
                model_id TEXT NOT NULL,
                upstream_model TEXT NOT NULL,
                protocol TEXT NOT NULL,
                verified_at TEXT,
                source TEXT NOT NULL DEFAULT 'manual',
                PRIMARY KEY (account_id, model_id, protocol)
             );",
        )
        .unwrap();
    insert_leftover_capability(&db, "key-a", "shared", "up-a");
    insert_leftover_capability(&db, "key-b", "shared", "up-b");
    db.conn
        .execute(
            "DELETE FROM destination_models WHERE destination_id = ?1",
            [&dest_id],
        )
        .unwrap();
    db.conn
        .execute_batch(
            "DELETE FROM schema_version;
             INSERT INTO schema_version(version) VALUES (52);",
        )
        .unwrap();
    drop(db);

    match Database::open(dir.clone()) {
        Ok(_) => panic!("v53 should refuse conflicting upstream maps"),
        Err(err) => assert!(
            err.to_string().contains("conflicting upstream")
                || err
                    .chain()
                    .any(|cause| cause.to_string().contains("conflicting upstream")),
            "{err:#}"
        ),
    }
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn v54_migrates_v53_platform_parent_and_linked_key_then_drops_leftover_tables() {
    use crate::platform::{PlatformGroup, PlatformKind, PlatformSnapshot};
    use ocg_domain::credential::observer_credential_id_for_platform_account;
    use ocg_domain::destination::destination_id_for_platform_account;

    let dir = temp_data_dir("v54-from-v53-platform");
    let db = Database::open(dir.clone()).unwrap();
    let mut custom = account("linked-v53");
    custom.provider_id = CUSTOM_PROVIDER_ID.to_string();
    custom.enabled = false;
    custom.credential_kind = CredentialKind::ApiKey;
    custom.quota_scope = QuotaScope::Key;
    custom.key_cipher = "linked-key-cipher".into();
    db.create_account_with_contract(
        &custom,
        Some(&AccountCustomConfigInput {
            endpoint_url: "https://old.example/v1/chat/completions".into(),
            upstream_protocol: UpstreamProtocolKind::ChatCompletions,
        }),
        &[AccountModelCapabilityInput {
            public_model: "org/model".into(),
            upstream_model: "org/upstream".into(),
            protocol: UpstreamProtocolKind::ChatCompletions,
            source: Some("manual".into()),
        }],
    )
    .unwrap();
    db.create_platform_account(
        "parent-v53",
        PlatformKind::NewApi,
        "Parent V53",
        "https://platform.example/v1",
        Some("mgmt-cipher"),
    )
    .unwrap();
    db.link_platform_account("linked-v53", "parent-v53", &PlatformGroup::default())
        .unwrap();
    let token = db
        .platform_refresh_token("parent-v53", Some("linked-v53"))
        .unwrap();
    assert!(
        db.save_platform_refresh(
            "parent-v53",
            Some("linked-v53"),
            &token,
            &PlatformSnapshot {
                observed_at: 1,
                ..PlatformSnapshot::default()
            },
        )
        .unwrap()
    );
    let before_parent = db.platform_account("parent-v53").unwrap().unwrap();
    let before_link = db
        .list_platform_links()
        .unwrap()
        .into_iter()
        .find(|link| link.account_id == "linked-v53")
        .unwrap();
    let before_cipher = db
        .platform_credential_cipher("parent-v53")
        .unwrap()
        .expect("management cipher");
    let dest_id = destination_id_for_platform_account("parent-v53");
    let observer_id = observer_credential_id_for_platform_account("parent-v53").to_string();
    let snapshot_json = serde_json::to_string(&before_parent.snapshot).unwrap();
    let group_json = serde_json::to_string(&before_link.group).unwrap();
    let link_version: i64 = db
        .conn
        .query_row(
            "SELECT COALESCE(link_version, 0) FROM credentials WHERE legacy_account_id = 'linked-v53'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let link_snapshot: Option<String> = db
        .conn
        .query_row(
            "SELECT link_snapshot FROM credentials WHERE legacy_account_id = 'linked-v53'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    db.conn
        .execute_batch(
            "CREATE TABLE platform_accounts (
                id TEXT PRIMARY KEY, kind TEXT NOT NULL,
                name TEXT NOT NULL, base_url TEXT NOT NULL, credential_cipher TEXT,
                version INTEGER NOT NULL DEFAULT 1, snapshot TEXT);
             CREATE TABLE platform_links (
                account_id TEXT PRIMARY KEY,
                platform_account_id TEXT NOT NULL,
                group_json TEXT NOT NULL, version INTEGER NOT NULL DEFAULT 1, snapshot TEXT);",
        )
        .unwrap();
    db.conn
        .execute(
            "INSERT INTO platform_accounts(id,kind,name,base_url,credential_cipher,version,snapshot)
             VALUES ('parent-v53','new_api','Parent V53','https://platform.example/v1',?1,?2,?3)",
            rusqlite::params![before_cipher, before_parent.version as i64, snapshot_json],
        )
        .unwrap();
    db.conn
        .execute(
            "INSERT INTO platform_links(account_id,platform_account_id,group_json,version,snapshot)
             VALUES ('linked-v53','parent-v53',?1,?2,?3)",
            rusqlite::params![group_json, link_version, link_snapshot],
        )
        .unwrap();
    db.conn
        .execute(
            "DELETE FROM credential_grants WHERE credential_id = ?1",
            [&observer_id],
        )
        .unwrap();
    db.conn
        .execute("DELETE FROM credentials WHERE id = ?1", [&observer_id])
        .unwrap();
    db.conn
        .execute(
            "DELETE FROM destination_models WHERE destination_id = ?1",
            [&dest_id],
        )
        .unwrap();
    db.conn
        .execute("DELETE FROM destinations WHERE id = ?1", [&dest_id])
        .unwrap();
    db.conn
        .execute(
            "UPDATE credentials
             SET destination_id = '', group_json = NULL, link_version = NULL, link_snapshot = NULL
             WHERE legacy_account_id = 'linked-v53'",
            [],
        )
        .unwrap();
    db.conn
        .execute_batch(
            "DELETE FROM schema_version;
             INSERT INTO schema_version(version) VALUES (53);",
        )
        .unwrap();
    assert_eq!(lifecycle::schema_version_on(&db.conn).unwrap(), 53);
    assert!(table_exists(&db.conn, "platform_accounts").unwrap());
    drop(db);

    let db = Database::open(dir.clone()).unwrap();
    assert_eq!(
        lifecycle::schema_version_on(&db.conn).unwrap(),
        CURRENT_SCHEMA_VERSION
    );
    let leftover: i64 = db
        .conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master
             WHERE type = 'table'
               AND name IN ('platform_accounts', 'platform_links')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(leftover, 0);
    let after_parent = db.platform_account("parent-v53").unwrap().unwrap();
    assert_eq!(after_parent.kind, PlatformKind::NewApi);
    assert_eq!(after_parent.base_url, "https://platform.example/v1");
    assert_eq!(after_parent.name, "Parent V53");
    assert!(after_parent.has_user_credential);
    assert_eq!(after_parent.version, before_parent.version);
    assert_eq!(
        after_parent.snapshot.is_some(),
        before_parent.snapshot.is_some()
    );
    assert_eq!(
        db.platform_credential_cipher("parent-v53")
            .unwrap()
            .as_deref(),
        Some(before_cipher.as_str())
    );
    let after_link = db
        .list_platform_links()
        .unwrap()
        .into_iter()
        .find(|link| link.account_id == "linked-v53")
        .expect("link survived");
    assert_eq!(after_link.platform_account_id, "parent-v53");
    assert_eq!(after_link.group, before_link.group);
    assert!(after_link.snapshot.is_some());
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn v54_refuses_unmappable_leftover_platform_parent() {
    let dir = temp_data_dir("v54-refuse-empty-url");
    let db = Database::open(dir.clone()).unwrap();
    db.conn
        .execute_batch(
            "CREATE TABLE platform_accounts (
                id TEXT PRIMARY KEY, kind TEXT NOT NULL,
                name TEXT NOT NULL, base_url TEXT NOT NULL, credential_cipher TEXT,
                version INTEGER NOT NULL DEFAULT 1, snapshot TEXT);
             INSERT INTO platform_accounts(id,kind,name,base_url,version)
             VALUES ('bad-parent','new_api','Bad','',1);
             DELETE FROM schema_version;
             INSERT INTO schema_version(version) VALUES (53);",
        )
        .unwrap();
    drop(db);
    match Database::open(dir.clone()) {
        Ok(_) => panic!("v54 should refuse leftover parent with empty base_url"),
        Err(err) => assert!(
            err.to_string().contains("empty base_url")
                || err
                    .chain()
                    .any(|cause| cause.to_string().contains("empty base_url")),
            "{err:#}"
        ),
    }
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn v55_migrates_v54_cpa_leftover_then_drops_table() {
    use ocg_domain::credential::observer_credential_id_for_cpa;
    use ocg_domain::destination::destination_id_for_builtin;

    let dir = temp_data_dir("v55-from-v54-cpa");
    let db = open_with_host_cipher(dir.clone()).unwrap();
    let now = Utc::now();
    let mut cpa_account = account(CPA_ACCOUNT_ID);
    cpa_account.provider_id = CPA_PROVIDER_ID.to_string();
    cpa_account.credential_kind = CredentialKind::ApiKey;
    cpa_account.quota_scope = QuotaScope::Key;
    cpa_account.name = CPA_ACCOUNT_NAME.to_string();
    cpa_account.account_type = AccountType::Key;
    cpa_account.setup_step = AccountSetupStep::Ready;
    cpa_account.created_at = now;
    cpa_account.updated_at = now;
    cpa_account.key_cipher = String::new();
    let management_cipher = test_host_cipher().encrypt("cpa-management-v54").unwrap();
    db.upsert_cpa_integration(&cpa_account, "http://127.0.0.1:8317", &management_cipher)
        .unwrap();
    let dest_id = destination_id_for_builtin(CPA_PROVIDER_ID);
    let observer_id = observer_credential_id_for_cpa().to_string();
    db.conn
        .execute_batch(
            "CREATE TABLE cpa_integration (
                id TEXT PRIMARY KEY CHECK (id = 'cpa'),
                account_id TEXT NOT NULL UNIQUE,
                base_url TEXT NOT NULL,
                management_key_cipher TEXT NOT NULL,
                updated_at TEXT NOT NULL
             );",
        )
        .unwrap();
    db.conn
        .execute(
            "INSERT INTO cpa_integration
                 (id, account_id, base_url, management_key_cipher, updated_at)
             VALUES ('cpa', ?1, ?2, ?3, ?4)",
            rusqlite::params![
                CPA_ACCOUNT_ID,
                "http://127.0.0.1:8317",
                management_cipher,
                now.to_rfc3339(),
            ],
        )
        .unwrap();
    db.conn
        .execute(
            "DELETE FROM credential_grants WHERE credential_id = ?1",
            [&observer_id],
        )
        .unwrap();
    db.conn
        .execute("DELETE FROM credentials WHERE id = ?1", [&observer_id])
        .unwrap();
    db.conn
        .execute(
            "DELETE FROM destination_models WHERE destination_id = ?1",
            [&dest_id],
        )
        .unwrap();
    db.conn
        .execute("DELETE FROM destinations WHERE id = ?1", [&dest_id])
        .unwrap();
    db.conn
        .execute_batch(
            "DELETE FROM schema_version;
             INSERT INTO schema_version(version) VALUES (54);",
        )
        .unwrap();
    assert_eq!(lifecycle::schema_version_on(&db.conn).unwrap(), 54);
    assert!(table_exists(&db.conn, "cpa_integration").unwrap());
    drop(db);

    let db = open_with_host_cipher(dir.clone()).unwrap();
    assert_eq!(
        lifecycle::schema_version_on(&db.conn).unwrap(),
        CURRENT_SCHEMA_VERSION
    );
    let leftover: i64 = db
        .conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master
             WHERE type = 'table' AND name = 'cpa_integration'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(leftover, 0);
    let record = db.cpa_integration().unwrap().expect("mapped leftover");
    assert_eq!(record.account_id, CPA_ACCOUNT_ID);
    assert_eq!(record.base_url, "http://127.0.0.1:8317");
    assert_eq!(record.management_key_cipher, management_cipher);
    let inference = db.get_account(CPA_ACCOUNT_ID).unwrap().expect("inference");
    assert_ne!(inference.key_cipher, management_cipher);
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn v55_refuses_unmappable_leftover_cpa() {
    let dir = temp_data_dir("v55-refuse-empty-cipher");
    let db = Database::open(dir.clone()).unwrap();
    db.conn
        .execute_batch(
            "CREATE TABLE cpa_integration (
                id TEXT PRIMARY KEY CHECK (id = 'cpa'),
                account_id TEXT NOT NULL UNIQUE,
                base_url TEXT NOT NULL,
                management_key_cipher TEXT NOT NULL,
                updated_at TEXT NOT NULL
             );
             INSERT INTO cpa_integration
                 (id, account_id, base_url, management_key_cipher, updated_at)
             VALUES ('cpa', '00000000-0000-0000-0000-000000000003',
                     'http://127.0.0.1:8317', '', '2026-01-01T00:00:00Z');
             DELETE FROM schema_version;
             INSERT INTO schema_version(version) VALUES (54);",
        )
        .unwrap();
    drop(db);
    match Database::open(dir.clone()) {
        Ok(_) => panic!("v55 should refuse leftover CPA with empty management cipher"),
        Err(err) => assert!(
            err.to_string().contains("empty management_key_cipher")
                || err
                    .chain()
                    .any(|cause| { cause.to_string().contains("empty management_key_cipher") }),
            "{err:#}"
        ),
    }
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn v56_migrates_v55_dynamic_leftover_then_drops_tables() {
    use ocg_domain::destination::destination_id_for_dynamic;

    let dir = temp_data_dir("v56-from-v55-dynamic");
    let db = open_with_host_cipher(dir.clone()).unwrap();
    let now = Utc::now().to_rfc3339();
    let dest_id = destination_id_for_dynamic("lab-http");
    db.conn
        .execute_batch(&format!(
            "{ddl}
             INSERT INTO providers
                (id, origin, adapter_kind, name, endpoint_url, upstream_protocol,
                 auth_kind, preset_id, offering, created_at, updated_at, onboarding_draft)
             VALUES
                ('lab-http', 'custom', 'configurable_http', 'Lab HTTP',
                 'https://lab.example/v1', 'chat_completions', 'bearer', NULL, 'api',
                 '{now}', '{now}', 0),
                ('draft-http', 'preset', 'configurable_http', 'Draft HTTP',
                 'https://draft.example/v1', 'chat_completions', 'bearer', 'zhipu-coding',
                 'plan', '{now}', '{now}', 1);
             INSERT INTO provider_models
                (provider_id, public_model, public_model_key, upstream_model, upstream_override)
             VALUES
                ('lab-http', 'lab-opus', 'lab-opus', 'vendor/opus',
                 '{{\"protocol\":\"messages\",\"endpoint_url\":\"https://lab.example/anthropic/v1/messages\"}}');
             DELETE FROM destination_models WHERE destination_id = '{dest_id}';
             DELETE FROM destinations WHERE id = '{dest_id}' OR legacy_id IN ('lab-http','draft-http');
             DELETE FROM schema_version;
             INSERT INTO schema_version(version) VALUES (55);",
            ddl = leftover_providers_ddl()
        ))
        .unwrap();
    assert_eq!(lifecycle::schema_version_on(&db.conn).unwrap(), 55);
    assert!(table_exists(&db.conn, "providers").unwrap());
    drop(db);

    let db = open_with_host_cipher(dir.clone()).unwrap();
    assert_eq!(
        lifecycle::schema_version_on(&db.conn).unwrap(),
        CURRENT_SCHEMA_VERSION
    );
    assert_leftover_dynamic_provider_storage_absent(&db.conn);
    let lab = db
        .get_dynamic_provider("lab-http")
        .unwrap()
        .expect("mapped leftover");
    assert_eq!(lab.endpoint_url, "https://lab.example/v1");
    assert_eq!(
        lab.upstream_protocol,
        crate::provider::UpstreamProtocolKind::ChatCompletions
    );
    assert_eq!(lab.mappings.len(), 1);
    assert_eq!(lab.mappings[0].public_model, "lab-opus");
    assert_eq!(lab.mappings[0].upstream_model, "vendor/opus");
    assert_eq!(
        lab.mappings[0]
            .upstream_override
            .as_ref()
            .map(|value| value.endpoint_url.as_str()),
        Some("https://lab.example/anthropic/v1/messages")
    );
    assert_eq!(
        db.provider_is_onboarding_draft("draft-http").unwrap(),
        Some(true)
    );
    let draft = db
        .list_control_plane_dynamic_providers()
        .unwrap()
        .into_iter()
        .find(|row| row.id == "draft-http")
        .expect("draft leftover");
    assert_eq!(draft.preset_id.as_deref(), Some("zhipu-coding"));
    assert_eq!(draft.offering, "plan");
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn v56_refuses_unmappable_leftover_dynamic_provider() {
    let dir = temp_data_dir("v56-refuse-empty-url");
    let db = Database::open(dir.clone()).unwrap();
    let now = Utc::now().to_rfc3339();
    db.conn
        .execute_batch(&format!(
            "{ddl}
             INSERT INTO providers
                (id, origin, adapter_kind, name, endpoint_url, upstream_protocol,
                 auth_kind, offering, created_at, updated_at)
             VALUES ('bad-http', 'custom', 'configurable_http', 'Bad HTTP', '',
                     'chat_completions', 'bearer', 'api', '{now}', '{now}');
             DELETE FROM schema_version;
             INSERT INTO schema_version(version) VALUES (55);",
            ddl = leftover_providers_ddl()
        ))
        .unwrap();
    drop(db);
    match Database::open(dir.clone()) {
        Ok(_) => panic!("v56 should refuse leftover keyed HTTP with empty URL"),
        Err(err) => assert!(
            err.to_string().contains("empty required URL")
                || err
                    .chain()
                    .any(|cause| cause.to_string().contains("empty required URL")),
            "{err:#}"
        ),
    }
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn v56_refuses_unknown_leftover_adapter() {
    let dir = temp_data_dir("v56-refuse-unknown-adapter");
    let db = Database::open(dir.clone()).unwrap();
    let now = Utc::now().to_rfc3339();
    db.conn
        .execute_batch(&format!(
            "{ddl}
             INSERT INTO providers
                (id, origin, adapter_kind, name, endpoint_url, upstream_protocol,
                 auth_kind, offering, created_at, updated_at)
             VALUES ('weird', 'custom', 'user_script', 'Weird', 'https://weird.example/v1',
                     'chat_completions', 'bearer', 'api', '{now}', '{now}');
             DELETE FROM schema_version;
             INSERT INTO schema_version(version) VALUES (55);",
            ddl = leftover_providers_ddl()
        ))
        .unwrap();
    drop(db);
    match Database::open(dir.clone()) {
        Ok(_) => panic!("v56 should refuse leftover with unknown adapter"),
        Err(err) => assert!(
            err.to_string().contains("unknown adapter")
                || err
                    .chain()
                    .any(|cause| cause.to_string().contains("unknown adapter")),
            "{err:#}"
        ),
    }
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn v57_migrates_v56_identity_satellites_then_drops_tables() {
    let dir = temp_data_dir("v57-from-v56-identity");
    let db = open_with_host_cipher(dir.clone()).unwrap();
    let mut keyed = account("v57-go");
    keyed.name = "Go Key".into();
    keyed.key_cipher = fixture_account_key_cipher();
    db.create_account(&keyed).unwrap();
    let mut managed = account("v57-managed");
    managed.name = "Managed Draft".into();
    managed.account_type = AccountType::Managed;
    managed.setup_step = AccountSetupStep::Payment;
    managed.enabled = false;
    managed.key_cipher.clear();
    db.create_account(&managed).unwrap();
    let before = db.list_identity_model().unwrap();
    let go = before
        .accounts
        .iter()
        .find(|row| row.account.id == "v57-go")
        .unwrap()
        .clone();
    let draft = before
        .accounts
        .iter()
        .find(|row| row.account.id == "v57-managed")
        .unwrap()
        .clone();
    rewind_identity_satellites_to_v56(&db.conn);
    assert_eq!(lifecycle::schema_version_on(&db.conn).unwrap(), 56);
    assert!(table_exists(&db.conn, "upstream_identities").unwrap());
    assert!(table_exists(&db.conn, "quota_pools").unwrap());
    drop(db);

    let db = open_with_host_cipher(dir.clone()).unwrap();
    assert_eq!(
        lifecycle::schema_version_on(&db.conn).unwrap(),
        CURRENT_SCHEMA_VERSION
    );
    assert_leftover_identity_tables_absent(&db.conn);
    assert!(table_exists(&db.conn, "quota_pools").unwrap());
    assert!(table_exists(&db.conn, "quota_pool_members").unwrap());
    let after = db.list_identity_model().unwrap();
    let go_after = after
        .accounts
        .iter()
        .find(|row| row.account.id == "v57-go")
        .expect("reconstructed go");
    assert_eq!(go_after.identity_id, go.identity_id);
    assert_eq!(go_after.credential_id, go.credential_id);
    assert_eq!(go_after.binding_id, go.binding_id);
    let mut go_ids = go.allowed_endpoint_ids.clone();
    let mut go_ids_after = go_after.allowed_endpoint_ids.clone();
    go_ids.sort();
    go_ids_after.sort();
    assert_eq!(go_ids_after, go_ids);
    assert!(go_after.subscription.is_some());
    let draft_after = after
        .accounts
        .iter()
        .find(|row| row.account.id == "v57-managed")
        .expect("reconstructed managed");
    assert_eq!(draft_after.identity_id, draft.identity_id);
    assert!(draft_after.onboarding.is_some());
    let pool_members: i64 = db
        .conn
        .query_row(
            "SELECT COUNT(*) FROM quota_pool_members WHERE account_id IN ('v57-go', 'v57-managed')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(pool_members, 2);
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn v57_refuses_unmappable_leftover_identity_satellite() {
    let dir = temp_data_dir("v57-refuse-orphan-binding");
    let db = Database::open(dir.clone()).unwrap();
    let now = Utc::now().to_rfc3339();
    db.conn
        .execute_batch(&format!(
            "{ddl}
             INSERT INTO credential_bindings (
                id, account_id, connection_legacy_kind, connection_legacy_id,
                model_scope, enabled, created_at, updated_at,
                allowed_endpoint_ids, allowed_origins
             ) VALUES (
                'orphan-binding', 'missing-account', 'account', 'missing-account',
                '{{\"kind\":\"all\"}}', 1, '{now}', '{now}', '[]', '[]'
             );
             DELETE FROM schema_version;
             INSERT INTO schema_version(version) VALUES (56);",
            ddl = crate::db::identity_v57::leftover_identity_tables_ddl()
        ))
        .unwrap();
    drop(db);
    match Database::open(dir.clone()) {
        Ok(_) => panic!("v57 should refuse leftover binding with no credential"),
        Err(err) => assert!(
            err.to_string().contains("no reconstructible credential")
                || err
                    .chain()
                    .any(|cause| cause.to_string().contains("no reconstructible credential")),
            "{err:#}"
        ),
    }
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn v57_refuses_unmappable_leftover_identity() {
    let dir = temp_data_dir("v57-refuse-orphan-identity");
    let db = Database::open(dir.clone()).unwrap();
    let now = Utc::now().to_rfc3339();
    db.conn
        .execute_batch(&format!(
            "{ddl}
             INSERT INTO upstream_identities (
                id, label, identity_confidence, authority_site, authority_subject,
                enabled, notes, created_at, updated_at
             ) VALUES (
                'orphan-identity', 'Orphan', 'opaque', NULL, NULL, 1, NULL, '{now}', '{now}'
             );
             DELETE FROM schema_version;
             INSERT INTO schema_version(version) VALUES (56);",
            ddl = crate::db::identity_v57::leftover_identity_tables_ddl()
        ))
        .unwrap();
    drop(db);
    match Database::open(dir.clone()) {
        Ok(_) => panic!("v57 should refuse leftover identity with no credential"),
        Err(err) => assert!(
            err.to_string().contains("no reconstructible credential")
                || err
                    .chain()
                    .any(|cause| cause.to_string().contains("no reconstructible credential")),
            "{err:#}"
        ),
    }
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn v57_create_credential_and_grants_survive_reopen_without_leftover_tables() {
    let dir = temp_data_dir("v57-crud-reopen");
    let db = open_with_host_cipher(dir.clone()).unwrap();
    let mut keyed = account("v57-persist");
    keyed.key_cipher = fixture_account_key_cipher();
    db.create_account(&keyed).unwrap();
    let stored = db
        .list_identity_model()
        .unwrap()
        .accounts
        .into_iter()
        .find(|row| row.account.id == "v57-persist")
        .unwrap();
    db.update_credential_binding(
        &stored.binding_id,
        None,
        None,
        Some(&[] as &[String]),
        Some(&[] as &[String]),
    )
    .unwrap();
    assert_leftover_identity_tables_absent(&db.conn);
    drop(db);

    let db = open_with_host_cipher(dir.clone()).unwrap();
    assert_leftover_identity_tables_absent(&db.conn);
    let reopened = db
        .list_identity_model()
        .unwrap()
        .accounts
        .into_iter()
        .find(|row| row.account.id == "v57-persist")
        .expect("reopened");
    assert_eq!(reopened.identity_id, stored.identity_id);
    assert_eq!(reopened.binding_id, stored.binding_id);
    assert!(reopened.allowed_endpoint_ids.is_empty());
    assert!(reopened.allowed_origins.is_empty());
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn v42_dynamic_read_paths_hide_builtin_rows() {
    let dir = temp_data_dir("v42-filter-builtins");
    let db = open_with_host_cipher(dir.clone()).unwrap();
    assert!(db.list_dynamic_providers().unwrap().is_empty());
    for builtin_id in [
        OPENCODE_PROVIDER_ID,
        OPENCODE_ZEN_FREE_PROVIDER_ID,
        COMMAND_CODE_PROVIDER_ID,
        MINIMAX_PROVIDER_ID,
        KIMI_PROVIDER_ID,
        OLLAMA_PROVIDER_ID,
        CUSTOM_PROVIDER_ID,
    ] {
        assert!(
            db.get_dynamic_provider(builtin_id).unwrap().is_none(),
            "get_dynamic_provider({builtin_id}) must return None"
        );
    }
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn v42_create_dynamic_provider_persists_origin_and_offering() {
    let dir = temp_data_dir("v42-create-origin");
    let db = open_with_host_cipher(dir.clone()).unwrap();
    let now = Utc::now();
    let preset_provider = uuid::Uuid::new_v4().to_string();
    let custom_provider = uuid::Uuid::new_v4().to_string();
    let mut preset_first = account("preset-acct");
    preset_first.provider_id = preset_provider.clone();
    preset_first.key_cipher = fixture_account_key_cipher();
    let mut custom_first = account("custom-acct");
    custom_first.provider_id = custom_provider.clone();
    custom_first.key_cipher = fixture_account_key_cipher();
    let preset_runtime = crate::dynamic::DynamicProviderRuntime {
        preset_id: Some("zhipu-coding".into()),
        id: preset_provider.clone(),
        name: "Preset Lab".into(),
        endpoint_url: "http://127.0.0.1:9".into(),
        upstream_protocol: crate::provider::UpstreamProtocolKind::ChatCompletions,
        auth_kind: ocg_domain::dynamic::DynamicAuthKind::Bearer,
        mappings: vec![ocg_domain::dynamic::DynamicModelMapping {
            public_model: "preset-model".into(),
            upstream_model: "preset/upstream".into(),
            upstream_override: None,
        }],
        created_at: now,
        updated_at: now,
        origin: ocg_domain::provider::ProviderOrigin::Preset,
        offering: ocg_domain::provider::preset_offering("zhipu-coding").to_string(),
    };
    let custom_runtime = crate::dynamic::DynamicProviderRuntime {
        preset_id: None,
        id: custom_provider.clone(),
        name: "Custom Lab".into(),
        endpoint_url: "http://127.0.0.1:10".into(),
        upstream_protocol: crate::provider::UpstreamProtocolKind::ChatCompletions,
        auth_kind: ocg_domain::dynamic::DynamicAuthKind::Bearer,
        mappings: vec![ocg_domain::dynamic::DynamicModelMapping {
            public_model: "custom-model".into(),
            upstream_model: "custom/upstream".into(),
            upstream_override: None,
        }],
        created_at: now,
        updated_at: now,
        origin: ocg_domain::provider::ProviderOrigin::Custom,
        offering: "api".to_string(),
    };
    db.create_dynamic_provider(&preset_runtime, &preset_first)
        .unwrap();
    db.create_dynamic_provider(&custom_runtime, &custom_first)
        .unwrap();

    let preset_row = db
        .get_dynamic_provider(&preset_provider)
        .unwrap()
        .expect("preset");
    assert_eq!(
        preset_row.origin,
        ocg_domain::provider::ProviderOrigin::Preset
    );
    assert_eq!(preset_row.preset_id.as_deref(), Some("zhipu-coding"));
    assert_eq!(preset_row.offering, "plan");

    let custom_row = db
        .get_dynamic_provider(&custom_provider)
        .unwrap()
        .expect("custom");
    assert_eq!(
        custom_row.origin,
        ocg_domain::provider::ProviderOrigin::Custom
    );
    assert!(custom_row.preset_id.is_none());
    assert_eq!(custom_row.offering, "api");

    // create_dynamic_provider on a builtin id must fail because get_dynamic_provider
    // already returns None for builtin rows.
    let mut builtin_first = account("builtin-attempt");
    builtin_first.provider_id = OPENCODE_PROVIDER_ID.into();
    builtin_first.key_cipher = fixture_account_key_cipher();
    let builtin_attempt = crate::dynamic::DynamicProviderRuntime {
        preset_id: None,
        id: OPENCODE_PROVIDER_ID.into(),
        name: "Builtin Collision".into(),
        endpoint_url: "http://127.0.0.1:11".into(),
        upstream_protocol: crate::provider::UpstreamProtocolKind::ChatCompletions,
        auth_kind: ocg_domain::dynamic::DynamicAuthKind::Bearer,
        mappings: vec![ocg_domain::dynamic::DynamicModelMapping {
            public_model: "model".into(),
            upstream_model: "model".into(),
            upstream_override: None,
        }],
        created_at: now,
        updated_at: now,
        origin: ocg_domain::provider::ProviderOrigin::Custom,
        offering: "api".to_string(),
    };
    let error = db
        .create_dynamic_provider(&builtin_attempt, &builtin_first)
        .expect_err("creating a dynamic provider with a builtin id must fail");
    let message = format!("{error:#}");
    assert!(
        message.contains("UNIQUE") || message.contains("collides") || message.contains("built-in"),
        "create on builtin id must surface uniqueness conflict, got: {message}"
    );

    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn v42_delete_dynamic_provider_rejects_builtin_id() {
    let dir = temp_data_dir("v42-delete-builtin");
    let db = open_with_host_cipher(dir.clone()).unwrap();
    for builtin_id in [
        OPENCODE_PROVIDER_ID,
        OPENCODE_ZEN_FREE_PROVIDER_ID,
        COMMAND_CODE_PROVIDER_ID,
        MINIMAX_PROVIDER_ID,
        KIMI_PROVIDER_ID,
        OLLAMA_PROVIDER_ID,
        CUSTOM_PROVIDER_ID,
    ] {
        let error = db
            .delete_dynamic_provider(builtin_id)
            .expect_err("delete must reject builtin id");
        let message = format!("{error:#}");
        assert!(
            message.contains("unknown provider"),
            "delete on builtin `{builtin_id}` must fail with unknown provider, got: {message}"
        );
    }
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn v42_writes_pre_v42_backup_for_non_fresh_v41_source() {
    let dir = temp_data_dir("v42-pre-v42-backup");
    let db = open_with_host_cipher(dir.clone()).unwrap();
    let now = Utc::now().to_rfc3339();
    db.conn
        .execute_batch(&format!(
            "PRAGMA foreign_keys=OFF;
             DROP TABLE IF EXISTS providers;
             DROP TABLE IF EXISTS provider_models;
             CREATE TABLE dynamic_providers (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                endpoint_url TEXT NOT NULL,
                upstream_protocol TEXT NOT NULL,
                auth_kind TEXT NOT NULL,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                preset_id TEXT
             );
             INSERT INTO dynamic_providers
                 (id, name, endpoint_url, upstream_protocol, auth_kind, created_at, updated_at, preset_id)
             VALUES ('legacy-lab', 'Legacy Lab', 'https://legacy.example/v1', 'chat_completions', 'bearer', '{now}', '{now}', NULL);
             DELETE FROM schema_version;
             INSERT INTO schema_version (version) VALUES (39);
             PRAGMA foreign_keys=ON;"
        ))
        .unwrap();
    drop(db);

    assert!(pre_v42_backup_paths(&dir).is_empty());
    let db = open_with_host_cipher(dir.clone()).unwrap();
    drop(db);

    let backups = pre_v42_backup_paths(&dir);
    assert_eq!(backups.len(), 1, "expected exactly one pre-v42 backup");
    let backup = &backups[0];
    let verified = Connection::open_with_flags(backup, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let backup_version = lifecycle::schema_version_on(&verified).unwrap();
    assert_eq!(backup_version, V41_SCHEMA_VERSION);
    let legacy_count: i64 = verified
        .query_row(
            "SELECT COUNT(*) FROM dynamic_providers WHERE id = 'legacy-lab'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(legacy_count, 1);
    drop(verified);
    let hash_path = backup.with_file_name(format!(
        "{}.sha256",
        backup.file_name().unwrap().to_str().unwrap()
    ));
    assert!(hash_path.exists(), "sha256 sidecar must be written");
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn v58_preserves_custom_identity_and_writes_verified_backup() {
    let dir = temp_data_dir("v58-custom-connection");
    let db = open_with_host_cipher(dir.clone()).unwrap();
    let mut custom = account("custom-v58");
    custom.provider_id = CUSTOM_PROVIDER_ID.into();
    custom.name = "Legacy Custom".into();
    custom.key_cipher = fixture_account_key_cipher();
    db.create_account_with_contract(
        &custom,
        Some(&AccountCustomConfigInput {
            endpoint_url: "https://custom.example/v1/chat/completions".into(),
            upstream_protocol: UpstreamProtocolKind::ChatCompletions,
        }),
        &[AccountModelCapabilityInput {
            public_model: "public-name".into(),
            upstream_model: "vendor/raw-id".into(),
            protocol: UpstreamProtocolKind::ChatCompletions,
            source: None,
        }],
    )
    .unwrap();
    let before: (String, String, String) = db
        .conn
        .query_row(
            "SELECT d.id, c.id, c.destination_id
             FROM destinations d
             JOIN credentials c ON c.destination_id = d.id
             WHERE c.legacy_account_id = 'custom-v58'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    db.conn
        .execute_batch(
            "ALTER TABLE destinations DROP COLUMN model_resolution;
             UPDATE destinations SET max_credentials = 1 WHERE legacy_kind = 'custom_account';
             DELETE FROM schema_version;
             INSERT INTO schema_version(version) VALUES (57);",
        )
        .unwrap();
    drop(db);

    assert!(pre_v58_backup_paths(&dir).is_empty());
    let db = open_with_host_cipher(dir.clone()).unwrap();
    assert_eq!(
        lifecycle::schema_version_on(&db.conn).unwrap(),
        crate::db::CURRENT_SCHEMA_VERSION
    );
    let after: (String, String, String, Option<i64>, String) = db
        .conn
        .query_row(
            "SELECT d.id, c.id, c.destination_id, d.max_credentials, d.model_resolution
             FROM destinations d
             JOIN credentials c ON c.destination_id = d.id
             WHERE c.legacy_account_id = 'custom-v58'",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(after.0, before.0);
    assert_eq!(after.1, before.1);
    assert_eq!(after.2, before.2);
    assert_eq!(after.3, None);
    assert_eq!(after.4, "public_only");
    let model: (String, String) = db
        .conn
        .query_row(
            "SELECT public_model, upstream_model FROM destination_models WHERE destination_id = ?1",
            [&after.0],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(model, ("public-name".into(), "vendor/raw-id".into()));
    drop(db);

    let backups = pre_v58_backup_paths(&dir);
    assert_eq!(backups.len(), 1);
    let backup =
        Connection::open_with_flags(&backups[0], OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    assert_eq!(lifecycle::schema_version_on(&backup).unwrap(), 57);
    drop(backup);
    let hash_path = backups[0].with_file_name(format!(
        "{}.sha256",
        backups[0].file_name().unwrap().to_str().unwrap()
    ));
    assert!(hash_path.exists());
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn current_schema_and_data_remain_stable_across_startup_replay() {
    let dir = temp_data_dir("current-schema-stable");
    let assert_current_shape = |db: &Database| {
        assert_eq!(
            lifecycle::schema_version_on(&db.conn).unwrap(),
            CURRENT_SCHEMA_VERSION
        );
        assert_retired_dynamic_provider_tables_absent(&db.conn);
        assert_v48_inert_columns_absent(&db.conn);
        assert_leftover_dynamic_provider_storage_absent(&db.conn);
        assert!(table_has_column(&db.conn, "destinations", "onboarding_draft").unwrap());
        assert!(table_has_column(&db.conn, "destinations", "model_resolution").unwrap());
        assert!(!table_has_column(&db.conn, "accounts", "offering_id").unwrap());
        for column in USAGE_SYNC_ACCOUNT_COLUMNS {
            assert!(
                !table_has_column(&db.conn, "accounts", column).unwrap(),
                "{column}"
            );
        }
        for table in [
            "access_keys",
            "provider_contract_scopes",
            "provider_contract_model_protocols",
            "destinations",
            "destination_models",
            "credentials",
            "credential_grants",
        ] {
            assert!(table_exists(&db.conn, table).unwrap(), "{table}");
        }
        assert!(!table_exists(&db.conn, "sub_gateway_keys").unwrap());
        for column in ["client_key_id", "client_key_name"] {
            assert!(
                table_has_column(&db.conn, "forward_logs", column).unwrap(),
                "{column}"
            );
        }
        for index in ["idx_forward_logs_client_key", "idx_access_keys_active_key"] {
            let count: i64 = db
                .conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' AND name = ?1",
                    [index],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(count, 1, "{index}");
        }
        assert!(!db.primary_access_key_value().unwrap().unwrap().is_empty());
        assert_eq!(db.count_active_sub_gateway_keys().unwrap(), 0);
        for (version, backups) in [
            ("v27", pre_v3_backup_paths(&dir)),
            ("v35", pre_v35_backup_paths(&dir)),
            ("v42", pre_v42_backup_paths(&dir)),
            ("v48", pre_v48_backup_paths(&dir)),
            ("v58", pre_v58_backup_paths(&dir)),
        ] {
            assert!(
                backups.is_empty(),
                "current schema must not create a {version} backup"
            );
        }
    };
    let db = open_with_host_cipher(dir.clone()).unwrap();
    assert_current_shape(&db);
    db.migrate().unwrap();
    assert_current_shape(&db);
    drop(db);

    let db = open_with_host_cipher(dir.clone()).unwrap();
    assert_current_shape(&db);

    let now = Utc::now();
    let provider_id = uuid::Uuid::new_v4().to_string();
    let runtime = crate::dynamic::DynamicProviderRuntime {
        preset_id: None,
        id: provider_id.clone(),
        name: "Survive Lab".into(),
        endpoint_url: "https://survive.example/v1/chat/completions".into(),
        upstream_protocol: crate::provider::UpstreamProtocolKind::ChatCompletions,
        auth_kind: ocg_domain::dynamic::DynamicAuthKind::Bearer,
        mappings: vec![ocg_domain::dynamic::DynamicModelMapping {
            public_model: "survive-model".into(),
            upstream_model: "vendor/survive".into(),
            upstream_override: None,
        }],
        created_at: now,
        updated_at: now,
        origin: ocg_domain::provider::ProviderOrigin::Custom,
        offering: "api".to_string(),
    };
    let mut keyed = account("survive-key");
    keyed.provider_id = provider_id.clone();
    keyed.key_cipher = fixture_account_key_cipher();
    db.create_dynamic_provider(&runtime, &keyed).unwrap();
    drop(db);

    let db = open_with_host_cipher(dir.clone()).unwrap();
    assert_current_shape(&db);
    let loaded = db.get_dynamic_provider(&provider_id).unwrap().unwrap();
    assert_eq!(loaded.name, "Survive Lab");
    assert_eq!(loaded.mappings.len(), 1);
    assert_eq!(loaded.mappings[0].public_model, "survive-model");
    let stored = db.get_account("survive-key").unwrap().unwrap();
    assert_eq!(stored.provider_id, provider_id);
    assert_fixture_account_cipher(&stored.key_cipher);
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn current_schema_reopen_preserves_nonempty_retired_dynamic_provider_residue() {
    let dir = temp_data_dir("current-retired-residue");
    let db = open_with_host_cipher(dir.clone()).unwrap();
    db.conn
        .execute_batch(
            "CREATE TABLE dynamic_providers (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL
             );
             CREATE TABLE dynamic_provider_models (
                provider_id TEXT NOT NULL,
                public_model TEXT NOT NULL
             );
             INSERT INTO dynamic_providers (id, name) VALUES ('residue', 'Leftover');
             INSERT INTO dynamic_provider_models (provider_id, public_model)
             VALUES ('residue', 'leftover-model');",
        )
        .unwrap();
    drop(db);

    let db = open_with_host_cipher(dir.clone()).unwrap();
    assert_eq!(
        lifecycle::schema_version_on(&db.conn).unwrap(),
        CURRENT_SCHEMA_VERSION
    );
    let leftover: (String, i64) = db
        .conn
        .query_row(
            "SELECT name,
                    (SELECT COUNT(*) FROM dynamic_provider_models WHERE provider_id = 'residue')
             FROM dynamic_providers WHERE id = 'residue'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(leftover.0, "Leftover");
    assert_eq!(leftover.1, 1);
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn v48_drops_inert_columns_and_empty_retired_tables_while_preserving_live_data() {
    let dir = temp_data_dir("v48-preserve-live");
    let db = open_with_host_cipher(dir.clone()).unwrap();
    let mut keyed = account("v48-go");
    keyed.key_cipher = fixture_account_key_cipher();
    keyed.enabled = false;
    db.create_account(&keyed).unwrap();
    let cipher_before = db.get_account("v48-go").unwrap().unwrap().key_cipher;
    let scope = ContractScope::provider(MINIMAX_PROVIDER_ID);
    let now = Utc::now();
    db.set_model_protocol_settings(
        &scope,
        &[(
            "MiniMax-M3".into(),
            UpstreamProtocolKind::ChatCompletions,
            ProtocolOverrideState::ForceOff,
        )],
        &[("MiniMax-M3".into(), UpstreamProtocolKind::ChatCompletions)],
        now,
    )
    .unwrap();
    let identity_before = db
        .list_identity_model()
        .unwrap()
        .accounts
        .into_iter()
        .find(|row| row.account.id == "v48-go")
        .unwrap();
    rewind_current_to_v47(&db.conn);
    ensure_dynamic_provider_tables(&db.conn).unwrap();
    assert!(table_exists(&db.conn, "dynamic_providers").unwrap());
    assert!(table_exists(&db.conn, "dynamic_provider_models").unwrap());
    drop(db);

    assert!(pre_v48_backup_paths(&dir).is_empty());
    let db = open_with_host_cipher(dir.clone()).unwrap();
    assert_eq!(
        lifecycle::schema_version_on(&db.conn).unwrap(),
        CURRENT_SCHEMA_VERSION
    );
    assert_v48_inert_columns_absent(&db.conn);
    assert_retired_dynamic_provider_tables_absent(&db.conn);
    let stored = db.get_account("v48-go").unwrap().unwrap();
    assert_eq!(stored.key_cipher, cipher_before);
    assert_fixture_account_cipher(&stored.key_cipher);
    assert!(!stored.enabled);
    let saved = db.load_persisted_contracts().unwrap();
    assert_eq!(
        saved.overrides[&scope][0].state,
        ProtocolOverrideState::ForceOff
    );
    assert_eq!(
        saved.preferences[&scope],
        vec![("minimax-m3".into(), UpstreamProtocolKind::ChatCompletions)]
    );
    let identity_after = db
        .list_identity_model()
        .unwrap()
        .accounts
        .into_iter()
        .find(|row| row.account.id == "v48-go")
        .unwrap();
    assert_eq!(identity_after.credential_id, identity_before.credential_id);
    assert_eq!(identity_after.binding_id, identity_before.binding_id);
    let backups = pre_v48_backup_paths(&dir);
    assert_eq!(backups.len(), 1, "expected exactly one pre-v48 backup");
    let backup = &backups[0];
    let verified = Connection::open_with_flags(backup, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    assert_eq!(
        lifecycle::schema_version_on(&verified).unwrap(),
        V47_SCHEMA_VERSION
    );
    assert!(table_has_column(&verified, "accounts", "free_alias_enabled").unwrap());
    drop(verified);
    let hash_path = backup.with_file_name(format!(
        "{}.sha256",
        backup.file_name().unwrap().to_str().unwrap()
    ));
    assert!(hash_path.exists(), "sha256 sidecar must be written");
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn v48_refuses_nonempty_retired_tables_without_claiming_upgrade() {
    for (label, insert_providers, insert_models) in [
        ("providers-only", true, false),
        ("models-only", false, true),
    ] {
        let dir = temp_data_dir(&format!("v48-nonempty-{label}"));
        let db = open_with_host_cipher(dir.clone()).unwrap();
        rewind_current_to_v47(&db.conn);
        ensure_dynamic_provider_tables(&db.conn).unwrap();
        if insert_providers {
            db.conn
                .execute(
                    "INSERT INTO dynamic_providers
                     (id, name, endpoint_url, upstream_protocol, auth_kind, created_at, updated_at)
                     VALUES ('residue', 'Leftover', 'https://legacy.example/v1',
                             'chat_completions', 'bearer', '2026-01-01T00:00:00Z',
                             '2026-01-01T00:00:00Z')",
                    [],
                )
                .unwrap();
        }
        if insert_models {
            db.conn
                .execute_batch(
                    "PRAGMA foreign_keys=OFF;
                     INSERT INTO dynamic_provider_models
                        (provider_id, public_model, public_model_key, upstream_model)
                     VALUES ('residue', 'leftover-model', 'leftover-model', 'upstream');
                     PRAGMA foreign_keys=ON;",
                )
                .unwrap();
        }
        drop(db);

        let error = match open_with_host_cipher(dir.clone()) {
            Ok(_) => panic!("{label}: nonempty leftover must fail closed"),
            Err(error) => error,
        };
        let message = format!("{error:#}");
        assert!(
            message.contains("nonempty leftover") && message.contains("refusing to drop"),
            "{label}: {message}"
        );
        if insert_providers {
            assert!(message.contains("dynamic_providers"), "{label}: {message}");
        }
        if insert_models {
            assert!(
                message.contains("dynamic_provider_models"),
                "{label}: {message}"
            );
        }
        let conn = Connection::open(dir.join("data.sqlite")).unwrap();
        assert_eq!(
            lifecycle::schema_version_on(&conn).unwrap(),
            V47_SCHEMA_VERSION
        );
        assert!(table_has_column(&conn, "accounts", "free_alias_enabled").unwrap());
        if insert_providers {
            let count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM dynamic_providers WHERE id = 'residue'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(count, 1, "{label}");
        } else {
            let count: i64 = conn
                .query_row("SELECT COUNT(*) FROM dynamic_providers", [], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(count, 0, "{label}");
        }
        if insert_models {
            let count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM dynamic_provider_models WHERE provider_id = 'residue'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(count, 1, "{label}");
        } else {
            let count: i64 = conn
                .query_row("SELECT COUNT(*) FROM dynamic_provider_models", [], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(count, 0, "{label}");
        }
        drop(conn);
        fs::remove_dir_all(dir).unwrap();
    }
}

#[test]
fn v48_transaction_failure_leaves_v47_source() {
    let dir = temp_data_dir("v48-tx-abort");
    let db = open_with_host_cipher(dir.clone()).unwrap();
    rewind_current_to_v47(&db.conn);
    ensure_dynamic_provider_tables(&db.conn).unwrap();
    db.conn
        .execute_batch(
            "CREATE TRIGGER fail_v48 BEFORE INSERT ON schema_version
             WHEN NEW.version = 48
             BEGIN
                 SELECT RAISE(ABORT, 'injected v48 failure');
             END;",
        )
        .unwrap();
    drop(db);

    let error = match open_with_host_cipher(dir.clone()) {
        Ok(_) => panic!("injected v48 failure must abort"),
        Err(error) => error,
    };
    assert!(
        format!("{error:#}").contains("injected v48 failure"),
        "{error:#}"
    );
    let conn = Connection::open(dir.join("data.sqlite")).unwrap();
    assert_eq!(
        lifecycle::schema_version_on(&conn).unwrap(),
        V47_SCHEMA_VERSION
    );
    assert!(table_has_column(&conn, "accounts", "free_alias_enabled").unwrap());
    assert!(
        table_has_column(
            &conn,
            "provider_contract_scopes",
            "chat_completions_enabled"
        )
        .unwrap()
    );
    assert!(table_exists(&conn, "dynamic_providers").unwrap());
    assert!(table_exists(&conn, "dynamic_provider_models").unwrap());
    drop(conn);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn v42_refuses_existing_providers_table_without_dropping_source_rows() {
    let dir = temp_data_dir("v42-collision");
    let path = dir.join("data.sqlite");
    let now = Utc::now().to_rfc3339();
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(&format!(
        "CREATE TABLE schema_version (version INTEGER PRIMARY KEY);
         INSERT INTO schema_version (version) VALUES (41);
         CREATE TABLE dynamic_providers (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            endpoint_url TEXT NOT NULL,
            upstream_protocol TEXT NOT NULL,
            auth_kind TEXT NOT NULL,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            preset_id TEXT
         );
         INSERT INTO dynamic_providers
            (id, name, endpoint_url, upstream_protocol, auth_kind, created_at, updated_at, preset_id)
         VALUES ('legacy-dyn', 'Legacy Dyn', 'https://legacy.example/v1', 'chat_completions',
                 'bearer', '{now}', '{now}', NULL);
         CREATE TABLE providers (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL
         );
         INSERT INTO providers (id, name) VALUES ('residue', 'Must Keep');"
    ))
    .unwrap();
    drop(conn);

    let error = migrate_to_v42(&Connection::open(&path).unwrap(), &path, true)
        .expect_err("noncanonical v41 providers table must fail closed");
    let message = format!("{error:#}");
    assert!(
        message.contains("canonical v41 source without unified provider tables"),
        "{message}"
    );

    let conn = Connection::open(&path).unwrap();
    assert_eq!(
        lifecycle::schema_version_on(&conn).unwrap(),
        V41_SCHEMA_VERSION
    );
    let residue: String = conn
        .query_row(
            "SELECT name FROM providers WHERE id = 'residue'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(residue, "Must Keep");
    let dynamic: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM dynamic_providers WHERE id = 'legacy-dyn'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(dynamic, 1);
    drop(conn);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn v39_preserves_existing_provider_configuration_and_adds_optional_provenance() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch("CREATE TABLE schema_version(version INTEGER PRIMARY KEY); INSERT INTO schema_version VALUES(38);").unwrap();
    ensure_dynamic_provider_tables(&conn).unwrap();
    conn.execute_batch("INSERT INTO dynamic_providers VALUES('old','Old provider','https://example.test/v1','responses','bearer','2026-09-08T00:00:00Z','2026-09-08T00:00:00Z');
        INSERT INTO dynamic_provider_models VALUES('old','public-name','public-name','exact/ID');").unwrap();
    migrate_to_v39(&conn).unwrap();
    migrate_to_v39(&conn).unwrap();
    assert_eq!(lifecycle::schema_version_on(&conn).unwrap(), 39);
    migrate_to_v40(&conn).unwrap();
    migrate_to_v40(&conn).unwrap();
    assert_eq!(lifecycle::schema_version_on(&conn).unwrap(), 40);
    let (preset_id, endpoint_url): (Option<String>, String) = conn
        .query_row(
            "SELECT preset_id, endpoint_url FROM dynamic_providers WHERE id = 'old'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(preset_id, None);
    assert_eq!(endpoint_url, "https://example.test/v1");
    let (upstream_model, upstream_override): (String, Option<String>) = conn
        .query_row(
            "SELECT upstream_model, upstream_override FROM dynamic_provider_models WHERE provider_id = 'old'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(upstream_model, "exact/ID");
    assert!(upstream_override.is_none());
    conn.execute(
        "UPDATE dynamic_providers SET preset_id = 'azure-openai' WHERE id = 'old'",
        [],
    )
    .unwrap();
    let updated_preset: Option<String> = conn
        .query_row(
            "SELECT preset_id FROM dynamic_providers WHERE id = 'old'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(updated_preset.as_deref(), Some("azure-openai"));
}

#[test]
fn v45_keeps_forward_logs_interpretable_against_migrated_account_ids() {
    let dir = temp_data_dir("v45-log-identity");
    let db = open_with_host_cipher(dir.clone()).unwrap();
    let mut go = account("go-log");
    go.key_cipher = fixture_account_key_cipher();
    db.create_account(&go).unwrap();
    let mut log = forward_log("go-log", "success", 1.25);
    log.model = "glm-5".into();
    db.log_forward(&log).unwrap();
    rewind_identity_model_to_v44(&db.conn);
    drop(db);

    let db = open_with_host_cipher(dir.clone()).unwrap();
    assert_eq!(
        lifecycle::schema_version_on(&db.conn).unwrap(),
        CURRENT_SCHEMA_VERSION
    );
    let account = db.get_account("go-log").unwrap().unwrap();
    assert_eq!(account.id, "go-log");
    let row = db
        .list_forward_logs(10)
        .unwrap()
        .into_iter()
        .find(|item| item.account_id == "go-log")
        .expect("migrated account id must still resolve the log");
    assert_eq!(row.account_id, account.id);
    assert_eq!(row.model, "glm-5");
    assert_eq!(row.status, "success");
    let mapped: Option<String> = db
        .conn
        .query_row(
            "SELECT identity_id FROM credentials WHERE legacy_account_id = 'go-log'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(mapped.as_deref().is_some_and(|value| !value.is_empty()));
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn peek_schema_version_reports_none_without_a_database_file() {
    let dir = temp_data_dir("peek-missing");
    assert_eq!(
        lifecycle::peek_schema_version(&dir).expect("peek should succeed without a database file"),
        None
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn peek_schema_version_reads_without_migrating() {
    let dir = temp_data_dir("peek-version");
    let db_path = dir.join("data.sqlite");
    {
        let conn = Connection::open(&db_path).expect("fixture database should open");
        conn.execute_batch(
            "CREATE TABLE schema_version (version INTEGER NOT NULL);
             INSERT INTO schema_version (version) VALUES (61), (63);",
        )
        .expect("fixture schema versions should insert");
    }
    let before = fs::read(&db_path).expect("fixture database should read");
    assert_eq!(
        lifecycle::peek_schema_version(&dir).expect("peek should read the fixture"),
        Some(63)
    );
    let after = fs::read(&db_path).expect("fixture database should read");
    assert_eq!(before, after, "peek must not modify the database file");
    {
        let conn = Connection::open(&db_path).expect("fixture database should reopen");
        assert!(
            !table_exists(&conn, "settings").expect("table probe should succeed"),
            "peek must not create tables"
        );
    }
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn peek_app_config_reads_the_settings_row_without_a_database_open() {
    let dir = temp_data_dir("peek-config");
    let db_path = dir.join("data.sqlite");
    {
        let conn = Connection::open(&db_path).expect("fixture database should open");
        conn.execute_batch("CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);")
            .expect("fixture settings table should be created");
        let config = AppConfig {
            proxy_mode: ProxyMode::Manual,
            proxy_url: "http://127.0.0.1:7890".to_string(),
            ..AppConfig::default()
        };
        conn.execute(
            "INSERT INTO settings (key, value) VALUES ('config', ?1)",
            [serde_json::to_string(&config).expect("config should serialize")],
        )
        .expect("fixture config row should insert");
    }
    let peeked = lifecycle::peek_app_config(&dir).expect("config should parse");
    assert_eq!(peeked.proxy_mode, ProxyMode::Manual);
    assert_eq!(peeked.proxy_url, "http://127.0.0.1:7890");

    let missing = temp_data_dir("peek-config-missing");
    assert!(lifecycle::peek_app_config(&missing).is_none());
    let _ = fs::remove_dir_all(&dir);
    let _ = fs::remove_dir_all(&missing);
}

#[test]
fn v16_migrates_existing_accounts_to_imported_ready_keys() {
    let dir = temp_data_dir("v16-account-lifecycle");
    let path = dir.join("data.sqlite");
    let now = Utc::now().to_rfc3339();
    let conn = Connection::open(&path).expect("fixture db should open");
    conn.execute_batch(
        "CREATE TABLE schema_version (version INTEGER PRIMARY KEY);
             INSERT INTO schema_version (version) VALUES (15);
             CREATE TABLE accounts (
                 id TEXT PRIMARY KEY, name TEXT NOT NULL, username TEXT,
                 password_cipher TEXT, key_cipher TEXT NOT NULL,
                 enabled INTEGER NOT NULL DEFAULT 1, referral_code TEXT,
                 recharge_date TEXT NOT NULL, sort_order INTEGER NOT NULL DEFAULT 0,
                 cooldown_until TEXT, cooldown_generic_until TEXT,
                 cooldown_5h_until TEXT, cooldown_week_until TEXT,
                 cooldown_month_until TEXT, last_error TEXT, auth_error TEXT,
                 created_at TEXT NOT NULL, updated_at TEXT NOT NULL
             );
             CREATE TABLE forward_logs (
                 id INTEGER PRIMARY KEY, timestamp TEXT NOT NULL,
                 cost_state TEXT NOT NULL DEFAULT 'not_applicable', diagnostic_json TEXT
             );
             CREATE TABLE gateway_logs (
                 id INTEGER PRIMARY KEY, created_at TEXT NOT NULL, diagnostic_json TEXT
             );",
    )
    .expect("v15 fixture should be created");
    conn.execute(
        "INSERT INTO accounts
             (id, name, key_cipher, enabled, recharge_date, created_at, updated_at)
             VALUES ('legacy', 'Legacy', ?2, 1, '2026-08-01', ?1, ?1)",
        params![now, fixture_account_key_cipher()],
    )
    .expect("legacy account should be inserted");
    drop(conn);

    let db = open_with_host_cipher(dir.clone()).expect("v16 migration should succeed");
    let legacy = db
        .get_account("legacy")
        .expect("legacy account should load")
        .expect("legacy account should remain");
    assert_eq!(legacy.account_type, AccountType::Key);
    assert_eq!(legacy.setup_step, AccountSetupStep::Ready);
    drop(db);
    fs::remove_dir_all(dir).expect("test data dir should be removed");
}

#[test]
fn v7_migration_repairs_pr11_pr12_and_combined_v6_databases() {
    let future = (Utc::now() + Duration::days(2)).to_rfc3339();
    for (label, extra_columns, extra_indexes, source_column, error) in [
        (
            "pr11-v6",
            "",
            "CREATE INDEX idx_forward_logs_model ON forward_logs(model);\nCREATE INDEX idx_forward_logs_status ON forward_logs(status);",
            "",
            "5 hour usage limit reached",
        ),
        (
            "pr12-v6",
            ", cooldown_5h_until TEXT, cooldown_week_until TEXT, cooldown_month_until TEXT",
            "",
            "cooldown_week_until",
            "weekly usage limit reached",
        ),
        (
            "combined-v6",
            ", cooldown_5h_until TEXT, cooldown_week_until TEXT, cooldown_month_until TEXT",
            "CREATE INDEX idx_forward_logs_model ON forward_logs(model);\nCREATE INDEX idx_forward_logs_status ON forward_logs(status);",
            "cooldown_month_until",
            "monthly usage limit reached",
        ),
        (
            "generic-dev-v6",
            ", cooldown_generic_until TEXT, cooldown_5h_until TEXT, cooldown_week_until TEXT, cooldown_month_until TEXT",
            "CREATE INDEX idx_forward_logs_model ON forward_logs(model);\nCREATE INDEX idx_forward_logs_status ON forward_logs(status);",
            "cooldown_generic_until",
            "unknown rate limit",
        ),
    ] {
        let dir = temp_data_dir(label);
        let conn = create_v6_database(&dir, extra_columns, extra_indexes);
        conn.execute(
                "INSERT INTO accounts
                 (id, name, key_cipher, recharge_date, created_at, updated_at, cooldown_until, last_error)
                 VALUES ('old', 'old', ?4, '2026-07-01', ?1, ?1, ?2, ?3)",
                params![Utc::now().to_rfc3339(), future, error, fixture_account_key_cipher()],
            )
            .expect("v6 account should be inserted");
        if !source_column.is_empty() {
            conn.execute(
                &format!("UPDATE accounts SET {source_column} = ?1 WHERE id = 'old'"),
                [&future],
            )
            .expect("existing cooldown source should be set");
        }
        drop(conn);

        let db = open_with_host_cipher(dir.clone()).expect("v6 database should migrate");
        let account = db
            .get_account("old")
            .expect("account query should work")
            .expect("account should exist");
        assert!(account.cooldown_until.is_some(), "{label}");
        assert!(account.is_cooling_at(Utc::now()), "{label}");
        let indexes: i64 = db
            .conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master
                     WHERE type = 'index' AND name IN (
                         'idx_forward_logs_model',
                         'idx_forward_logs_status',
                         'idx_forward_logs_time_instant'
                     )",
                [],
                |row| row.get(0),
            )
            .expect("indexes should be queryable");
        assert_eq!(indexes, 3, "{label}");

        drop(db);
        fs::remove_dir_all(dir).expect("test data dir should be removed");
    }
}

#[test]
fn v4_migration_preserves_uncalibrated_usage() {
    let dir = temp_data_dir("v4-migration");
    let conn = Connection::open(dir.join("data.sqlite")).expect("v3 db should open");
    conn.execute_batch(
        "CREATE TABLE schema_version (version INTEGER PRIMARY KEY);
             INSERT INTO schema_version (version) VALUES (3);
             CREATE TABLE accounts (
                 id TEXT PRIMARY KEY,
                 name TEXT NOT NULL,
                 key_cipher TEXT NOT NULL,
                 enabled INTEGER NOT NULL DEFAULT 1,
                 referral_code TEXT,
                 recharge_date TEXT,
                 created_at TEXT NOT NULL,
                 updated_at TEXT NOT NULL,
                 cooldown_until TEXT,
                 last_error TEXT,
                 username TEXT,
                 password_cipher TEXT
             );
             CREATE TABLE forward_logs (
                 timestamp TEXT NOT NULL,
                 model TEXT NOT NULL DEFAULT 'test',
                 account_id TEXT NOT NULL,
                 status TEXT NOT NULL,
                 cost REAL NOT NULL DEFAULT 0
             );",
    )
    .expect("v3 schema should be created");
    let now = Utc::now().to_rfc3339();
    conn.execute(
            "INSERT INTO accounts (id, name, key_cipher, created_at, updated_at) VALUES (?1, ?1, ?3, ?2, ?2)",
            params!["old", now, fixture_account_key_cipher()],
        )
        .expect("v3 account should be inserted");
    conn.execute(
            "INSERT INTO forward_logs (timestamp, account_id, status, cost) VALUES (?1, 'old', 'success', 2.5)",
            [Utc::now().to_rfc3339()],
        )
        .expect("v3 usage should be inserted");
    drop(conn);

    let db = open_with_host_cipher(dir.clone()).expect("v3 db should migrate");
    let usage = db
        .opencode_go_account_usage("old")
        .expect("usage should load");
    assert_eq!(
        db.get_account("old")
            .expect("account should load")
            .expect("account should exist")
            .purchase_date,
        now[..10]
    );
    assert_cost(usage.window_5h, 2.5);
    assert_cost(usage.window_week, 2.5);
    assert_cost(usage.window_month, 2.5);

    drop(db);
    fs::remove_dir_all(dir).expect("test data dir should be removed");
}

#[test]
fn v5_migration_backfills_dates_and_stable_dense_order() {
    let dir = temp_data_dir("v5-migration");
    let conn = Connection::open(dir.join("data.sqlite")).expect("v4 db should open");
    conn.execute_batch(
        "CREATE TABLE schema_version (version INTEGER PRIMARY KEY);
             INSERT INTO schema_version (version) VALUES (4);
             CREATE TABLE accounts (
                 id TEXT PRIMARY KEY,
                 name TEXT NOT NULL,
                 key_cipher TEXT NOT NULL,
                 enabled INTEGER NOT NULL DEFAULT 1,
                 referral_code TEXT,
                 recharge_date TEXT,
                 created_at TEXT NOT NULL,
                 updated_at TEXT NOT NULL,
                 cooldown_until TEXT,
                 last_error TEXT,
                 username TEXT,
                 password_cipher TEXT,
                 usage_5h_baseline_percent REAL,
                 usage_5h_anchor_success_cost REAL,
                 usage_week_baseline_percent REAL,
                 usage_week_anchor_success_cost REAL,
                 usage_month_baseline_percent REAL,
                 usage_month_anchor_success_cost REAL
             );
             CREATE TABLE forward_logs (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 timestamp TEXT NOT NULL,
                 model TEXT NOT NULL,
                 account_id TEXT NOT NULL,
                 account_name TEXT NOT NULL,
                 status TEXT NOT NULL,
                 http_status INTEGER,
                 prompt_tokens INTEGER NOT NULL DEFAULT 0,
                 completion_tokens INTEGER NOT NULL DEFAULT 0,
                 cached_tokens INTEGER NOT NULL DEFAULT 0,
                 cost REAL NOT NULL DEFAULT 0,
                 error_message TEXT
             );",
    )
    .expect("v4 schema should be created");
    let shared_created_at = "2026-01-02T01:30:00+02:00";
    for (id, recharge_date, created_at) in [
        ("a", Some("2025-12-31"), shared_created_at),
        ("b", None, shared_created_at),
        ("c", Some(""), shared_created_at),
        ("d", Some("2026-2-3"), "2026-02-04T04:00:00Z"),
    ] {
        conn.execute(
            "INSERT INTO accounts
                 (id, name, key_cipher, recharge_date, created_at, updated_at)
                 VALUES (?1, ?1, ?4, ?2, ?3, ?3)",
            params![id, recharge_date, created_at, fixture_account_key_cipher()],
        )
        .expect("v4 account should be inserted");
    }
    drop(conn);

    let db = open_with_host_cipher(dir.clone()).expect("v4 db should migrate");
    let accounts = db.list_accounts().expect("migrated accounts should load");
    assert_eq!(
        accounts
            .iter()
            .map(|account| account.id.as_str())
            .collect::<Vec<_>>(),
        ["a", "b", "c", "d", ZEN_FREE_ACCOUNT_ID]
    );
    assert_eq!(accounts[0].purchase_date, "2025-12-31");
    assert_eq!(accounts[1].purchase_date, "2026-01-01");
    assert_eq!(accounts[2].purchase_date, "2026-01-01");
    assert_eq!(accounts[3].purchase_date, "2026-02-04");
    let sort_orders = db
        .conn
        .prepare("SELECT routing_rank FROM credentials ORDER BY routing_rank")
        .expect("sort query should prepare")
        .query_map([], |row| row.get::<_, i64>(0))
        .expect("sort query should run")
        .collect::<rusqlite::Result<Vec<_>>>()
        .expect("sort orders should load");
    assert_eq!(sort_orders, [0, 1, 2, 3, 4]);
    drop(db);

    let reopened = open_with_host_cipher(dir.clone()).expect("migrated db should reopen");
    assert_eq!(
        reopened
            .list_accounts()
            .expect("reopened accounts should load")
            .iter()
            .map(|account| account.id.as_str())
            .collect::<Vec<_>>(),
        ["a", "b", "c", "d", ZEN_FREE_ACCOUNT_ID]
    );

    drop(reopened);
    fs::remove_dir_all(dir).expect("test data dir should be removed");
}

#[test]
fn v8_migration_repairs_purchase_dates_written_by_older_binaries() {
    let dir = temp_data_dir("v8-purchase-date-repair");
    let conn = create_v6_database(
        &dir,
        ", cooldown_generic_until TEXT, cooldown_5h_until TEXT, cooldown_week_until TEXT, cooldown_month_until TEXT",
        "",
    );
    conn.execute("INSERT INTO schema_version (version) VALUES (7)", [])
        .expect("v7 schema version should be recorded");

    let created_at = "2026-01-02T01:30:00+02:00";
    for (id, recharge_date) in [
        ("valid", Some("2025-12-31")),
        ("null", None),
        ("invalid", Some("2026-2-3")),
    ] {
        conn.execute(
            "INSERT INTO accounts
                 (id, name, key_cipher, recharge_date, created_at, updated_at)
                 VALUES (?1, ?1, ?4, ?2, ?3, ?3)",
            params![id, recharge_date, created_at, fixture_account_key_cipher()],
        )
        .expect("legacy account should be inserted");
    }
    drop(conn);

    let db = open_with_host_cipher(dir.clone()).expect("v7 database should migrate");
    assert_eq!(
        db.get_account("valid")
            .expect("valid account query should work")
            .expect("valid account should exist")
            .purchase_date,
        "2025-12-31"
    );
    for id in ["null", "invalid"] {
        assert_eq!(
            db.get_account(id)
                .expect("repaired account query should work")
                .expect("repaired account should exist")
                .purchase_date,
            "2026-01-01",
            "{id}"
        );
    }

    drop(db);
    fs::remove_dir_all(dir).expect("test data dir should be removed");
}

#[test]
fn v9_migration_preserves_charged_legacy_errors() {
    let dir = temp_data_dir("v9-charged-error-cost");
    let conn = create_v6_database(
        &dir,
        ", cooldown_generic_until TEXT, cooldown_5h_until TEXT, cooldown_week_until TEXT, cooldown_month_until TEXT",
        "",
    );
    conn.execute("INSERT INTO schema_version (version) VALUES (7)", [])
        .expect("v7 schema version should be recorded");
    let now = Utc::now().to_rfc3339();
    conn.execute(
        "INSERT INTO accounts
             (id, name, key_cipher, recharge_date, created_at, updated_at)
             VALUES ('legacy', 'legacy', ?2, '2026-07-01', ?1, ?1)",
        params![now, fixture_account_key_cipher()],
    )
    .expect("legacy account should be inserted");
    for (status, cost) in [("error", 1.25), ("error", 0.0), ("success", 2.0)] {
        conn.execute(
            "INSERT INTO forward_logs
                 (timestamp, model, account_id, account_name, status, http_status, cost)
                 VALUES (?1, 'glm-5.2', 'legacy', 'legacy', ?2, 200, ?3)",
            params![now, status, cost],
        )
        .expect("legacy forward log should be inserted");
    }
    drop(conn);

    let db = open_with_host_cipher(dir.clone()).expect("v7 database should migrate through v10");
    let states = db
        .conn
        .prepare("SELECT status, cost, cost_state FROM forward_logs ORDER BY id")
        .expect("migrated logs should prepare")
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, f64>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .expect("migrated logs should query")
        .collect::<rusqlite::Result<Vec<_>>>()
        .expect("migrated logs should load");
    assert_eq!(
        states,
        [
            ("error".to_string(), 1.25, "legacy_estimate".to_string()),
            ("error".to_string(), 0.0, "not_applicable".to_string()),
            ("success".to_string(), 2.0, "legacy_estimate".to_string()),
        ]
    );
    assert_cost(
        db.opencode_go_account_usage("legacy")
            .expect("legacy usage should load")
            .window_month,
        3.25,
    );

    drop(db);
    fs::remove_dir_all(dir).expect("test data dir should be removed");
}

#[test]
fn v10_migration_repairs_charged_errors_from_original_v9() {
    let dir = temp_data_dir("v10-repair-v9-charged-error-cost");
    let conn = create_v6_database(
        &dir,
        ", cooldown_generic_until TEXT, cooldown_5h_until TEXT, cooldown_week_until TEXT, cooldown_month_until TEXT",
        "",
    );
    conn.execute_batch(
        "CREATE TABLE pricing_snapshots (
                 revision TEXT PRIMARY KEY,
                 activated_at TEXT NOT NULL,
                 document_updated_at TEXT NOT NULL,
                 source_url TEXT NOT NULL,
                 content_hash TEXT NOT NULL,
                 snapshot_json TEXT NOT NULL
             );
             CREATE INDEX idx_pricing_snapshots_activated
                 ON pricing_snapshots(activated_at DESC);
             ALTER TABLE forward_logs ADD COLUMN pricing_revision_id TEXT;
             ALTER TABLE forward_logs ADD COLUMN quota_multiplier REAL;
             ALTER TABLE forward_logs ADD COLUMN local_adjustment_multiplier REAL;
             ALTER TABLE forward_logs ADD COLUMN cache_creation_tokens INTEGER NOT NULL DEFAULT 0;
             ALTER TABLE forward_logs ADD COLUMN service_tier TEXT;
             ALTER TABLE forward_logs ADD COLUMN cost_state TEXT NOT NULL DEFAULT 'not_applicable';
             INSERT INTO schema_version (version) VALUES (9);",
    )
    .expect("original v9 schema should be created");
    let now = Utc::now().to_rfc3339();
    conn.execute(
        "INSERT INTO accounts
             (id, name, key_cipher, recharge_date, created_at, updated_at)
             VALUES ('legacy', 'legacy', ?2, '2026-07-01', ?1, ?1)",
        params![now, fixture_account_key_cipher()],
    )
    .expect("legacy account should be inserted");
    for (cost, cost_state) in [
        (1.25, "not_applicable"),
        (0.0, "not_applicable"),
        (4.0, "unpriced"),
    ] {
        conn.execute(
            "INSERT INTO forward_logs
                 (timestamp, model, account_id, account_name, status, http_status, cost, cost_state)
                 VALUES (?1, 'glm-5.2', 'legacy', 'legacy', 'error', 200, ?2, ?3)",
            params![now, cost, cost_state],
        )
        .expect("original v9 forward log should be inserted");
    }
    drop(conn);

    let db = open_with_host_cipher(dir.clone()).expect("v9 database should migrate through v11");
    let states = db
        .conn
        .prepare("SELECT cost, cost_state FROM forward_logs ORDER BY id")
        .expect("migrated logs should prepare")
        .query_map([], |row| {
            Ok((row.get::<_, f64>(0)?, row.get::<_, String>(1)?))
        })
        .expect("migrated logs should query")
        .collect::<rusqlite::Result<Vec<_>>>()
        .expect("migrated logs should load");
    assert_eq!(
        states,
        [
            (1.25, "legacy_estimate".to_string()),
            (0.0, "not_applicable".to_string()),
            (4.0, "unpriced".to_string()),
        ]
    );
    assert_cost(
        db.opencode_go_account_usage("legacy")
            .expect("legacy usage should load")
            .window_month,
        1.25,
    );

    drop(db);
    fs::remove_dir_all(dir).expect("test data dir should be removed");
}

#[test]
fn v22_migration_failure_rolls_back_to_usable_v21_source() {
    let dir = temp_data_dir("v22-atomic-migration");
    create_v21_fixture(&dir, true);

    assert!(Database::open(dir.clone()).is_err());
    let conn = Connection::open(dir.join("data.sqlite")).expect("db should reopen");
    let columns = conn
        .prepare("PRAGMA table_info(accounts)")
        .expect("table info should prepare")
        .query_map([], |row| row.get::<_, String>(1))
        .expect("table info should query")
        .collect::<rusqlite::Result<Vec<_>>>()
        .expect("columns should load");
    let version: i32 = conn
        .query_row("SELECT MAX(version) FROM schema_version", [], |row| {
            row.get(0)
        })
        .expect("schema version should load");
    let preserved_account: (String, String, i64) = conn
        .query_row(
            "SELECT name, key_cipher, enabled FROM accounts WHERE id = 'rollback-account'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("source account should remain readable");
    let preserved_log: (String, String, String, f64) = conn
        .query_row(
            "SELECT account_id, model, status, cost FROM forward_logs LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .expect("source forward log should remain readable");
    assert!(!columns.iter().any(|name| name == "provider_id"));
    assert_eq!(version, 21);
    assert_eq!(preserved_account.0, "rollback-account");
    assert_eq!(preserved_account.2, 1);
    assert_fixture_account_cipher(&preserved_account.1);
    assert_eq!(
        preserved_log,
        (
            "rollback-account".into(),
            "test".into(),
            "success".into(),
            4.25
        )
    );

    drop(conn);
    let backups_before = pre_v22_backup_paths(&dir);
    assert_eq!(backups_before.len(), 1);
    let backup_bytes = fs::read(&backups_before[0]).expect("rollback backup should be readable");
    assert!(Database::open(dir.clone()).is_err());
    assert_eq!(pre_v22_backup_paths(&dir), backups_before);
    assert_eq!(
        fs::read(&backups_before[0]).expect("rollback backup should remain readable"),
        backup_bytes
    );
    fs::remove_dir_all(dir).expect("test data dir should be removed");
}

#[test]
fn v20_to_v22_failure_rolls_back_v21_and_v22_writes() {
    let dir = temp_data_dir("v20-v22-atomic-migration");
    create_v20_fixture(&dir, true);

    assert!(Database::open(dir.clone()).is_err());
    let conn = Connection::open(dir.join("data.sqlite")).expect("db should reopen");
    let columns = conn
        .prepare("PRAGMA table_info(accounts)")
        .expect("table info should prepare")
        .query_map([], |row| row.get::<_, String>(1))
        .expect("table info should query")
        .collect::<rusqlite::Result<Vec<_>>>()
        .expect("columns should load");
    assert!(
        !columns
            .iter()
            .any(|name| name == "usage_sync_last_success_at")
    );
    assert!(!columns.iter().any(|name| name == "provider_id"));
    assert_eq!(lifecycle::schema_version_on(&conn).unwrap(), 20);
    drop(conn);

    let backups_before = pre_v22_backup_paths(&dir);
    assert_eq!(backups_before.len(), 1);
    let backup_bytes = fs::read(&backups_before[0]).expect("backup should be readable");
    let backup = Connection::open_with_flags(&backups_before[0], OpenFlags::SQLITE_OPEN_READ_ONLY)
        .expect("backup should open read-only");
    assert_eq!(lifecycle::schema_version_on(&backup).unwrap(), 20);
    drop(backup);

    assert!(Database::open(dir.clone()).is_err());
    assert_eq!(pre_v22_backup_paths(&dir), backups_before);
    assert_eq!(
        fs::read(&backups_before[0]).expect("backup should remain readable"),
        backup_bytes
    );
    fs::remove_dir_all(dir).expect("test data dir should be removed");
}

#[test]
fn v13_migration_preserves_legacy_manual_usage_calibration() {
    let dir = temp_data_dir("v13-legacy-calibration");
    let db = Database::open(dir.clone()).expect("db should open");
    let mut acct = account("legacy-calibration");
    acct.key_cipher = fixture_account_key_cipher();
    acct.purchase_date = local_today();
    db.create_account(&acct).expect("account should be created");
    finalize_success(&db, "legacy-calibration", 2.0, Utc::now());
    finalize_success(&db, "legacy-calibration", 1.0, Utc::now());
    drop(db);
    reverse_current_to_v34(&dir);
    {
        let conn = Connection::open(dir.join("data.sqlite")).unwrap();
        for (column, definition) in [
            ("usage_5h_baseline_percent", "REAL"),
            ("usage_5h_anchor_success_cost", "REAL"),
            ("usage_week_baseline_percent", "REAL"),
            ("usage_week_anchor_success_cost", "REAL"),
            ("usage_month_baseline_percent", "REAL"),
            ("usage_month_anchor_success_cost", "REAL"),
        ] {
            if !table_has_column(&conn, "accounts", column).unwrap() {
                conn.execute(
                    &format!("ALTER TABLE accounts ADD COLUMN {column} {definition}"),
                    [],
                )
                .expect("legacy baseline columns should exist for rewind");
            }
        }
        conn.execute(
            "UPDATE accounts SET
                    usage_5h_baseline_percent = 50,
                    usage_5h_anchor_success_cost = 2,
                    usage_week_baseline_percent = 40,
                    usage_week_anchor_success_cost = 2,
                    usage_month_baseline_percent = 25,
                    usage_month_anchor_success_cost = 2
                 WHERE id = 'legacy-calibration'",
            [],
        )
        .expect("legacy baselines should save");
        conn.execute_batch(
            "DELETE FROM schema_version;
                 INSERT INTO schema_version (version) VALUES (10);",
        )
        .expect("legacy schema version should save");
    }

    let db = open_with_host_cipher(dir.clone()).expect("legacy database should migrate");
    let usage = db
        .opencode_go_account_usage("legacy-calibration")
        .expect("migrated usage should load");
    // Old effective values: 50% * 12 + 1, 40% * 30 + 1,
    // and 25% * 60 + 1. The migration must preserve all three.
    assert_cost(usage.window_5h, 7.0);
    assert_cost(usage.window_week, 13.0);
    assert_cost(usage.window_month, 16.0);

    let remaining_baselines: i64 = db
        .conn
        .query_row(
            "SELECT COUNT(*)
                 FROM sqlite_master
                 WHERE type = 'table' AND name = 'accounts'",
            [],
            |row| row.get(0),
        )
        .expect("migration state should load");
    assert_eq!(remaining_baselines, 0);

    finalize_success(&db, "legacy-calibration", 2.0, Utc::now());
    let usage = db
        .opencode_go_account_usage("legacy-calibration")
        .expect("new usage should accumulate after migration");
    assert_cost(usage.window_5h, 9.0);
    assert_cost(usage.window_week, 15.0);
    assert_cost(usage.window_month, 18.0);

    drop(db);
    fs::remove_dir_all(dir).expect("test data dir should be removed");
}

#[test]
fn v14_migrates_v13_logs_and_adds_request_id_indexes() {
    let dir = temp_data_dir("v14-log-diagnostics");
    let db = Database::open(dir.clone()).expect("db should open");
    db.conn
        .execute_batch(
            "DROP INDEX idx_forward_logs_request_id;
                 DROP INDEX idx_gateway_logs_request_id;
                 ALTER TABLE forward_logs DROP COLUMN request_id;
                 ALTER TABLE forward_logs DROP COLUMN attempt;
                 ALTER TABLE forward_logs DROP COLUMN error_source;
                 ALTER TABLE forward_logs DROP COLUMN error_stage;
                 ALTER TABLE forward_logs DROP COLUMN duration_ms;
                 ALTER TABLE forward_logs DROP COLUMN diagnostic_json;
                 ALTER TABLE gateway_logs DROP COLUMN request_id;
                 ALTER TABLE gateway_logs DROP COLUMN attempt;
                 ALTER TABLE gateway_logs DROP COLUMN error_source;
                 ALTER TABLE gateway_logs DROP COLUMN error_stage;
                 ALTER TABLE gateway_logs DROP COLUMN duration_ms;
                 ALTER TABLE gateway_logs DROP COLUMN diagnostic_json;
                 INSERT INTO forward_logs
                    (timestamp, model, account_id, account_name, status, error_message)
                 VALUES ('2026-07-01T00:00:00Z', 'legacy-model', 'legacy', 'Legacy',
                         'client_error', 'legacy error');
                 INSERT INTO gateway_logs (level, category, message, created_at)
                 VALUES ('warn', 'legacy', 'legacy gateway error', '2026-07-01T00:00:00Z');
                 DELETE FROM schema_version;
                 INSERT INTO schema_version (version) VALUES (13);",
        )
        .expect("v13 schema should be prepared");
    drop(db);
    reverse_current_to_v34(&dir);
    {
        let conn = Connection::open(dir.join("data.sqlite")).unwrap();
        conn.execute_batch(
            "DELETE FROM schema_version;
                 INSERT INTO schema_version (version) VALUES (13);",
        )
        .expect("v13 schema version should be restored after reverse");
    }

    let db = Database::open(dir.clone()).expect("v13 database should migrate");
    for index in ["idx_forward_logs_request_id", "idx_gateway_logs_request_id"] {
        let exists: bool = db
            .conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='index' AND name=?1)",
                [index],
                |row| row.get(0),
            )
            .expect("index state should load");
        assert!(exists, "{index} should exist");
    }
    let forward = db
        .query_forward_logs(ForwardLogQueryOptions {
            limit: 10,
            offset: 0,
            status: None,
            account_id: None,
            provider_id: None,
            route_account_id: None,
            credential_account_id: None,
            model: None,
            key_id: None,
            request_id: None,
            start_time: None,
            end_time: None,
            sort_by: None,
            sort_order: None,
        })
        .expect("legacy forward log should load")
        .items
        .pop()
        .expect("legacy forward log should remain");
    assert_eq!(forward.error_message.as_deref(), Some("legacy error"));
    assert!(forward.request_id.is_none());
    assert!(forward.diagnostic.is_none());
    let gateway = db
        .list_gateway_logs(10)
        .expect("legacy gateway log should load")
        .pop()
        .expect("legacy gateway log should remain");
    assert_eq!(gateway.message, "legacy gateway error");
    assert!(gateway.request_id.is_none());
    assert!(gateway.diagnostic.is_none());

    drop(db);
    fs::remove_dir_all(dir).expect("test data dir should be removed");
}

#[test]
fn v15_migration_adds_nullable_auth_error() {
    let dir = temp_data_dir("v15-auth-error");
    let conn = Connection::open(dir.join("data.sqlite")).expect("legacy db should open");
    let now = Utc::now().to_rfc3339();
    conn.execute_batch(
        "CREATE TABLE schema_version (version INTEGER PRIMARY KEY);
             INSERT INTO schema_version (version) VALUES (14);
             CREATE TABLE accounts (
                 id TEXT PRIMARY KEY, name TEXT NOT NULL, username TEXT,
                 password_cipher TEXT, key_cipher TEXT NOT NULL,
                 enabled INTEGER NOT NULL DEFAULT 1, referral_code TEXT,
                 recharge_date TEXT NOT NULL, sort_order INTEGER NOT NULL DEFAULT 0,
                 cooldown_until TEXT, cooldown_generic_until TEXT,
                 cooldown_5h_until TEXT, cooldown_week_until TEXT,
                 cooldown_month_until TEXT, last_error TEXT,
                 created_at TEXT NOT NULL, updated_at TEXT NOT NULL
             );
             CREATE TABLE forward_logs (
                 timestamp TEXT,
                 cost_state TEXT NOT NULL DEFAULT 'not_applicable',
                 diagnostic_json TEXT
             );
             CREATE TABLE gateway_logs (created_at TEXT, diagnostic_json TEXT);",
    )
    .expect("v14 fixture should be created");
    conn.execute(
        "INSERT INTO accounts
             (id, name, key_cipher, enabled, recharge_date, created_at, updated_at)
             VALUES ('legacy', 'Legacy', ?2, 1, '2026-08-01', ?1, ?1)",
        params![now, fixture_account_key_cipher()],
    )
    .expect("v14 account should be inserted");
    drop(conn);

    let db = open_with_host_cipher(dir.clone()).expect("v14 database should migrate");
    let auth_error: Option<String> = db
        .conn
        .query_row(
            "SELECT auth_error FROM credentials WHERE legacy_account_id = 'legacy'",
            [],
            |row| row.get(0),
        )
        .expect("v15 migration state should load");
    assert!(auth_error.is_none());

    drop(db);
    fs::remove_dir_all(dir).expect("test data dir should be removed");
}

#[test]
fn v21_to_v22_creates_one_usable_rollback_backup() {
    let dir = temp_data_dir("v21-v22-backup");
    create_v21_fixture(&dir, false);

    let db = open_with_host_cipher(dir.clone()).expect("v21 database should migrate");
    assert!(
        db.get_account("rollback-account")
            .expect("migrated account should load")
            .is_some()
    );
    let migrated = db.list_quota_windows("rollback-account").unwrap();
    let migrated_rolling = migrated
        .iter()
        .find(|window| window.window_kind == QUOTA_WINDOW_FIVE_HOURS)
        .unwrap()
        .used;
    db.log_forward(&forward_log("rollback-account", "success", 1.5))
        .unwrap();
    let limits = SEED_LIMITS;
    let live = db
        .live_opencode_go_quota_windows("rollback-account", &limits)
        .unwrap();
    let live_rolling = live
        .iter()
        .find(|window| window.window_kind == QUOTA_WINDOW_FIVE_HOURS)
        .unwrap();
    assert!((live_rolling.used - (migrated_rolling + 1.5)).abs() < 1e-9);
    assert_eq!(
        db.list_quota_windows("rollback-account")
            .unwrap()
            .iter()
            .find(|window| window.window_kind == QUOTA_WINDOW_FIVE_HOURS)
            .unwrap()
            .used,
        migrated_rolling,
        "frozen migration rows must not be the provider API authority"
    );
    drop(db);

    let backups_before = pre_v22_backup_paths(&dir);
    assert_eq!(backups_before.len(), 1);
    let pre_v23 = pre_v23_backup_paths(&dir);
    assert_eq!(pre_v23.len(), 1);
    let pre_v23_backup = Connection::open_with_flags(&pre_v23[0], OpenFlags::SQLITE_OPEN_READ_ONLY)
        .expect("pre-v23 backup should open");
    assert_eq!(lifecycle::schema_version_on(&pre_v23_backup).unwrap(), 21);
    drop(pre_v23_backup);
    let backup_path = &backups_before[0];
    let pre_v3 = pre_v3_backup_paths(&dir);
    assert_eq!(pre_v3.len(), 1);
    let pre_v3_backup =
        Connection::open_with_flags(&pre_v3[0], OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    assert_eq!(
        lifecycle::schema_version_on(&pre_v3_backup).unwrap(),
        V26_SCHEMA_VERSION
    );
    drop(pre_v3_backup);

    let backup = Connection::open_with_flags(backup_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .expect("backup should open read-only");
    assert_eq!(lifecycle::schema_version_on(&backup).unwrap(), 21);
    let backed_up_account: (String, String, i64) = backup
        .query_row(
            "SELECT name, key_cipher, enabled FROM accounts WHERE id = 'rollback-account'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("backup should retain the representative account");
    let backed_up_log: (String, String, String, f64) = backup
        .query_row(
            "SELECT account_id, model, status, cost FROM forward_logs LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .expect("backup should retain the representative forward log");
    assert_eq!(backed_up_account.0, "rollback-account");
    assert_eq!(backed_up_account.2, 1);
    assert_fixture_account_cipher(&backed_up_account.1);
    assert_eq!(
        backed_up_log,
        (
            "rollback-account".into(),
            "test".into(),
            "success".into(),
            4.25
        )
    );
    drop(backup);

    let backup_bytes = fs::read(backup_path).expect("backup should be readable");
    let reopened = open_with_host_cipher(dir.clone()).expect("v22 database should reopen");
    drop(reopened);
    assert_eq!(pre_v22_backup_paths(&dir), backups_before);
    assert_eq!(
        fs::read(backup_path).expect("backup should remain readable"),
        backup_bytes
    );

    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn v20_to_v22_creates_verified_source_backup_before_direct_upgrade() {
    let dir = temp_data_dir("v20-v22-backup");
    create_v20_fixture(&dir, false);

    let db = open_with_host_cipher(dir.clone()).expect("v20 database should migrate directly");
    assert!(!table_exists(&db.conn, "accounts").unwrap());
    assert!(table_has_column(&db.conn, "credentials", "provider_id").unwrap());
    assert!(!table_has_column(&db.conn, "credentials", "usage_sync_last_success_at").unwrap());
    drop(db);

    let backups_before = pre_v22_backup_paths(&dir);
    assert_eq!(backups_before.len(), 1);
    let backup_bytes = fs::read(&backups_before[0]).expect("backup should be readable");
    let backup = Connection::open_with_flags(&backups_before[0], OpenFlags::SQLITE_OPEN_READ_ONLY)
        .expect("backup should open read-only");
    assert_eq!(lifecycle::schema_version_on(&backup).unwrap(), 20);
    assert!(!table_has_column(&backup, "accounts", "provider_id").unwrap());
    assert!(!table_has_column(&backup, "accounts", "usage_sync_last_success_at").unwrap());
    drop(backup);

    let reopened = open_with_host_cipher(dir.clone()).expect("v22 database should reopen");
    drop(reopened);
    assert_eq!(pre_v22_backup_paths(&dir), backups_before);
    assert_eq!(
        fs::read(&backups_before[0]).expect("backup should remain readable"),
        backup_bytes
    );
    fs::remove_dir_all(dir).expect("test data dir should be removed");
}

#[test]
fn draft_v19_libraries_without_notes_gain_the_column_on_reopen() {
    let dir = temp_data_dir("draft-v19-notes-repair");
    let db = Database::open(dir.clone()).unwrap();
    let mut legacy = account("legacy");
    legacy.key_cipher = fixture_account_key_cipher();
    db.create_account(&legacy).unwrap();
    // Unreleased #43 drafts already sat at version 19 (client-key
    // columns + sub-key table) and never received upstream v18 notes.
    account_store::materialize_legacy_accounts_for_rewind(&db.conn).unwrap();
    db.conn
        .execute_batch(
            "ALTER TABLE accounts DROP COLUMN notes;
                 DELETE FROM schema_version;
                 INSERT INTO schema_version (version) VALUES (19);",
        )
        .expect("draft numbering should be reproducible");
    let notes_before: i64 = db
        .conn
        .query_row(
            "SELECT COUNT(*) FROM pragma_table_info('accounts') WHERE name = 'notes'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(notes_before, 0);
    drop(db);
    reverse_current_to_v34(&dir);
    {
        let conn = Connection::open(dir.join("data.sqlite")).unwrap();
        conn.execute_batch(
            "DELETE FROM schema_version;
                 INSERT INTO schema_version (version) VALUES (19);",
        )
        .expect("draft numbering should survive reverse-to-v34");
    }

    let db = open_with_host_cipher(dir.clone()).expect("draft database should reopen");
    assert!(!table_exists(&db.conn, "accounts").unwrap());
    let notes_after: i64 = db
        .conn
        .query_row(
            "SELECT COUNT(*) FROM pragma_table_info('credentials') WHERE name = 'notes'",
            [],
            |row| row.get(0),
        )
        .expect("repaired schema should load");
    assert_eq!(notes_after, 1);
    db.list_accounts()
        .expect("account reads must survive a missing notes column on the draft");

    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn v22_to_v23_creates_one_usable_rollback_backup_and_contract_tables() {
    let dir = temp_data_dir("v22-v23-backup");
    create_v22_fixture(&dir);
    assert!(pre_v23_backup_paths(&dir).is_empty());

    let db = open_with_host_cipher(dir.clone()).expect("v22 database should migrate");
    let go = db
        .account_verification_state("v22-account")
        .unwrap()
        .unwrap();
    assert_eq!(go.status, ConnectionVerificationStatus::NotRequired);
    let goat = db.get_account("v22-goat").unwrap().unwrap();
    assert!(!goat.enabled, "migrated GOAT rows must be fail-closed");
    let goat_state = db.account_verification_state("v22-goat").unwrap().unwrap();
    assert_eq!(goat_state.status, ConnectionVerificationStatus::NotRequired);
    assert!(db.get_account("v22-account").unwrap().unwrap().enabled);
    assert!(
        db.get_account(ZEN_FREE_ACCOUNT_ID)
            .unwrap()
            .unwrap()
            .enabled
    );
    db.update_account(
        "v22-goat",
        &AccountUpdate {
            name: Some("v22-goat-renamed".into()),
            ..AccountUpdate::default()
        },
        None,
        None,
    )
    .unwrap();
    let renamed = db.get_account("v22-goat").unwrap().unwrap();
    assert!(!renamed.enabled);
    assert_eq!(renamed.name, "v22-goat-renamed");
    let log_id: i64 = db
        .conn
        .query_row("SELECT id FROM forward_logs LIMIT 1", [], |row| row.get(0))
        .unwrap();
    let attribution = db.forward_log_native_attribution(log_id).unwrap().unwrap();
    assert_eq!(attribution.requested_model.as_deref(), Some("test"));
    assert_eq!(attribution.upstream_model.as_deref(), Some("test"));
    assert_eq!(attribution.native_cost_unit.as_deref(), Some("usd"));
    assert_eq!(attribution.native_cost_currency.as_deref(), Some("USD"));
    drop(db);

    let backups = pre_v23_backup_paths(&dir);
    assert_eq!(backups.len(), 1);
    let backup = Connection::open_with_flags(&backups[0], OpenFlags::SQLITE_OPEN_READ_ONLY)
        .expect("pre-v23 backup should open");
    assert_eq!(lifecycle::schema_version_on(&backup).unwrap(), 22);
    assert!(!table_has_column(&backup, "accounts", "verification_status").unwrap());
    drop(backup);

    let reopened = open_with_host_cipher(dir.clone()).expect("v23 database should reopen");
    drop(reopened);
    assert_eq!(pre_v23_backup_paths(&dir).len(), 1);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn newer_unsupported_schema_is_rejected_without_writes() {
    let dir = temp_data_dir("schema-too-new");
    let db = Database::open(dir.clone()).unwrap();
    let too_new = CURRENT_SCHEMA_VERSION + 1;
    db.conn
        .execute_batch(&format!(
            "DELETE FROM schema_version;
                     INSERT INTO schema_version (version) VALUES ({too_new});"
        ))
        .unwrap();
    drop(db);

    let error = match Database::open(dir.clone()) {
        Ok(_) => panic!("unsupported schema must fail closed"),
        Err(error) => error,
    };
    assert!(
        error.to_string().contains("newer than this build supports"),
        "{error}"
    );
    let conn = Connection::open(dir.join("data.sqlite")).unwrap();
    assert_eq!(lifecycle::schema_version_on(&conn).unwrap(), too_new);
    drop(conn);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn v25_to_v26_backfills_zen_catalog_into_provider_scope() {
    let dir = temp_data_dir("v25-v26-zen-backfill");
    let refreshed_at = Utc::now();
    {
        let db = Database::open(dir.clone()).unwrap();
        db.set_zen_free_model_catalog(&crate::kernel::zen::ZenFreeModelCatalog {
            models: vec!["backfill-coder-free".into()],
            refreshed_at: Some(refreshed_at),
            source_url: crate::kernel::zen::ZEN_MODELS_SOURCE_URL.into(),
        })
        .unwrap();
    }
    reverse_current_to_v34(&dir);
    {
        let conn = Connection::open(dir.join("data.sqlite")).unwrap();
        conn.execute_batch(
            "DROP TRIGGER IF EXISTS access_keys_protect_primary_delete;
                     DROP TABLE IF EXISTS access_keys;
                     DROP TABLE IF EXISTS provider_contract_model_protocols;
                     DROP TABLE IF EXISTS provider_contract_scopes;
                     DELETE FROM schema_version;
                     INSERT INTO schema_version (version) VALUES (25);",
        )
        .unwrap();
        assert_eq!(lifecycle::schema_version_on(&conn).unwrap(), 25);
    }
    let db = Database::open(dir.clone()).expect("v25 database should migrate to v26");
    let scope = db
        .load_persisted_scope(&ContractScope::provider(OPENCODE_ZEN_FREE_PROVIDER_ID))
        .unwrap()
        .expect("zen provider scope should be backfilled");
    assert_eq!(scope.catalog_models, ["backfill-coder-free"]);
    assert_eq!(scope.catalog_source, CATALOG_SOURCE_OFFICIAL_ZEN);
    assert!(scope.revision >= 1);
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn v23_persists_verification_custom_config_and_capabilities() {
    let dir = temp_data_dir("v23-contracts");
    let db = Database::open(dir.clone()).unwrap();
    let mut goat = account("goat-draft");
    goat.provider_id = COMMAND_CODE_PROVIDER_ID.to_string();
    goat.enabled = false;
    db.create_account(&goat).unwrap();
    let goat_state = db
        .account_verification_state("goat-draft")
        .unwrap()
        .unwrap();
    assert_eq!(goat_state.status, ConnectionVerificationStatus::NotRequired);

    let mut custom = account("custom-1");
    custom.provider_id = CUSTOM_PROVIDER_ID.to_string();
    custom.enabled = false;
    db.create_account(&custom).unwrap();
    db.upsert_account_custom_config(
        "custom-1",
        &AccountCustomConfigInput {
            endpoint_url: "https://api.example.com/v1/chat/completions".into(),
            upstream_protocol: UpstreamProtocolKind::ChatCompletions,
        },
    )
    .unwrap();
    let updated = db
        .upsert_account_custom_config(
            "custom-1",
            &AccountCustomConfigInput {
                endpoint_url: "https://api.example.com/v1/messages".into(),
                upstream_protocol: UpstreamProtocolKind::Messages,
            },
        )
        .unwrap();
    assert_eq!(
        updated.upstream_protocol,
        UpstreamProtocolKind::Messages,
        "Custom protocol stays editable after create"
    );
    db.replace_account_model_capabilities(
        "custom-1",
        &[AccountModelCapabilityInput {
            public_model: "deepseek/deepseek-v4-flash".into(),
            upstream_model: "deepseek/deepseek-v4-flash".into(),
            protocol: UpstreamProtocolKind::Messages,
            source: Some("manual".into()),
        }],
    )
    .unwrap();
    let capabilities = db.list_account_model_capabilities("custom-1").unwrap();
    assert_eq!(capabilities[0].public_model, "deepseek/deepseek-v4-flash");
    assert_eq!(capabilities[0].upstream_model, "deepseek/deepseek-v4-flash");

    db.set_account_verification(
        "custom-1",
        ConnectionVerificationStatus::Verified,
        Some(Utc::now()),
        None,
    )
    .unwrap();
    db.update_account(
        "custom-1",
        &AccountUpdate {
            key: Some("rotated".into()),
            ..AccountUpdate::default()
        },
        Some("new-cipher"),
        None,
    )
    .unwrap();
    let after_key = db.account_verification_state("custom-1").unwrap().unwrap();
    assert_eq!(after_key.status, ConnectionVerificationStatus::Pending);
    let caps_after_key = db.list_account_model_capabilities("custom-1").unwrap();
    assert_eq!(caps_after_key.len(), 1);
    assert_eq!(caps_after_key[0].public_model, "deepseek/deepseek-v4-flash");

    let unknown = account("unknown");
    let mut unknown = unknown;
    unknown.provider_id = "no-such-provider".into();
    assert!(db.create_account(&unknown).is_err());

    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn v23_migration_failure_rolls_back_to_usable_v22_source_and_backup() {
    let dir = temp_data_dir("v23-atomic-migration");
    create_v22_fixture(&dir);
    let conn = Connection::open(dir.join("data.sqlite")).expect("v22 fixture should reopen");
    conn.execute_batch(
        "CREATE TRIGGER fail_v23_migration
             BEFORE INSERT ON schema_version
             WHEN NEW.version = 23
             BEGIN
                 SELECT RAISE(ABORT, 'forced v23 migration failure');
             END;",
    )
    .expect("fault-injection trigger should install");
    drop(conn);

    assert!(Database::open(dir.clone()).is_err());
    let conn = Connection::open(dir.join("data.sqlite")).expect("db should reopen");
    let columns = conn
        .prepare("PRAGMA table_info(accounts)")
        .expect("table info should prepare")
        .query_map([], |row| row.get::<_, String>(1))
        .expect("table info should query")
        .collect::<rusqlite::Result<Vec<_>>>()
        .expect("columns should load");
    assert!(!columns.iter().any(|name| name == "verification_status"));
    assert!(!table_exists(&conn, "account_custom_configs").unwrap());
    assert_eq!(lifecycle::schema_version_on(&conn).unwrap(), 22);
    let preserved_account: (String, String, i64) = conn
        .query_row(
            "SELECT name, key_cipher, enabled FROM accounts WHERE id = 'v22-account'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("source account should remain readable");
    let preserved_goat: (String, i64) = conn
        .query_row(
            "SELECT name, enabled FROM accounts WHERE id = 'v22-goat'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("source GOAT account should remain readable");
    let preserved_log: (String, String, String, f64) = conn
        .query_row(
            "SELECT account_id, model, status, cost FROM forward_logs LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .expect("source forward log should remain readable");
    assert_eq!(preserved_account.0, "v22-account");
    assert_eq!(preserved_account.2, 1);
    assert_fixture_account_cipher(&preserved_account.1);
    assert_eq!(preserved_goat, ("v22-goat".into(), 1));
    assert_eq!(
        preserved_log,
        ("v22-account".into(), "test".into(), "success".into(), 3.5)
    );
    drop(conn);

    let backups_before = pre_v23_backup_paths(&dir);
    assert_eq!(backups_before.len(), 1);
    let backup_bytes = fs::read(&backups_before[0]).expect("rollback backup should be readable");
    let backup = Connection::open_with_flags(&backups_before[0], OpenFlags::SQLITE_OPEN_READ_ONLY)
        .expect("pre-v23 backup should open");
    assert_eq!(lifecycle::schema_version_on(&backup).unwrap(), 22);
    assert!(!table_has_column(&backup, "accounts", "verification_status").unwrap());
    drop(backup);

    assert!(Database::open(dir.clone()).is_err());
    assert_eq!(pre_v23_backup_paths(&dir), backups_before);
    assert_eq!(
        fs::read(&backups_before[0]).expect("rollback backup should remain readable"),
        backup_bytes
    );
    fs::remove_dir_all(dir).expect("test data dir should be removed");
}

#[test]
fn v27_to_v28_adds_goat_model_access_without_replaying_v27() {
    let dir = temp_data_dir("v27-v28-migrate");
    populate_v26_source(&dir);
    let db_path = dir.join("data.sqlite");
    let conn = Connection::open(&db_path).unwrap();
    let cipher = test_host_cipher();
    migrate_to_v27(&conn, &db_path, Some(cipher.as_ref()), false).unwrap();
    assert_eq!(
        lifecycle::schema_version_on(&conn).unwrap(),
        V27_SCHEMA_VERSION
    );
    assert!(!table_has_column(&conn, "accounts", "goat_model_access").unwrap());

    migrate_to_v28(&conn).unwrap();
    assert_eq!(lifecycle::schema_version_on(&conn).unwrap(), 28);
    assert!(table_has_column(&conn, "accounts", "goat_model_access").unwrap());
    let default_value: String = conn
        .query_row(
            "SELECT goat_model_access FROM accounts WHERE id = 'v26-account'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(default_value, "goat");
    for column in USAGE_SYNC_ACCOUNT_COLUMNS {
        assert!(!table_has_column(&conn, "accounts", column).unwrap());
    }

    drop(conn);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn v28_to_v29_purges_scnet_accounts_and_acknowledgements() {
    let dir = temp_data_dir("v28-v29-scnet-purge");
    let db = Database::open(dir.clone()).unwrap();
    let mut leftover = account("scnet-leftover");
    leftover.provider_id = OPENCODE_PROVIDER_ID.into();
    db.create_account(&leftover).unwrap();
    drop(db);
    reverse_current_to_v34(&dir);
    let conn = Connection::open(dir.join("data.sqlite")).unwrap();
    conn.execute(
        "UPDATE accounts
             SET provider_id = 'scnet', offering_id = 'scnet-token-plan-basic'
             WHERE id = 'scnet-leftover'",
        [],
    )
    .unwrap();
    conn.execute(
        "CREATE TABLE account_acknowledgements (
                account_id TEXT NOT NULL,
                acknowledgement_id TEXT NOT NULL,
                version TEXT NOT NULL,
                content_hash TEXT NOT NULL,
                accepted_at TEXT NOT NULL,
                PRIMARY KEY (account_id, acknowledgement_id)
            )",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO account_acknowledgements
             (account_id, acknowledgement_id, version, content_hash, accepted_at)
             VALUES ('scnet-leftover', 'ack-scnet', '1', 'hash', ?1)",
        [Utc::now().to_rfc3339()],
    )
    .unwrap();
    conn.execute_batch(
        "DELETE FROM schema_version;
             INSERT OR REPLACE INTO schema_version (version) VALUES (28);",
    )
    .unwrap();
    drop(conn);

    let db = Database::open(dir.clone()).unwrap();
    assert!(
        db.get_account("scnet-leftover").unwrap().is_none(),
        "v29 must delete SCNet account rows"
    );
    let ack_table_exists: bool = db
        .conn
        .query_row(
            "SELECT EXISTS(
                    SELECT 1 FROM sqlite_master
                    WHERE type = 'table' AND name = 'account_acknowledgements'
                )",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        !ack_table_exists,
        "v29 must drop the account_acknowledgements table"
    );
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn v31_to_v32_collapses_custom_protocols_and_disables_the_account() {
    let dir = temp_data_dir("v31-v32-single-protocol");
    let db = Database::open(dir.clone()).unwrap();
    let mut custom = account("custom-v31");
    custom.provider_id = CUSTOM_PROVIDER_ID.to_string();
    custom.enabled = false;
    db.create_account_with_contract(
        &custom,
        Some(&AccountCustomConfigInput {
            endpoint_url: "https://api.example.com/v1/messages".into(),
            upstream_protocol: UpstreamProtocolKind::Messages,
        }),
        &[AccountModelCapabilityInput {
            public_model: "org/model".into(),
            upstream_model: "org/model".into(),
            protocol: UpstreamProtocolKind::Messages,
            source: None,
        }],
    )
    .unwrap();
    account_store::materialize_legacy_accounts_for_rewind(&db.conn).unwrap();
    db.conn
        .execute_batch(
            "DROP TABLE IF EXISTS account_custom_configs;
                 CREATE TABLE account_custom_configs (
                    account_id TEXT PRIMARY KEY,
                    base_url TEXT NOT NULL,
                    upstream_protocols TEXT NOT NULL,
                    auth_scheme TEXT NOT NULL,
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL,
                    FOREIGN KEY (account_id) REFERENCES accounts(id) ON DELETE CASCADE
                 );
                 INSERT INTO account_custom_configs (
                    account_id, base_url, upstream_protocols, auth_scheme, created_at, updated_at
                 ) VALUES (
                    'custom-v31', 'https://api.example.com/v1',
                    '[\"messages\",\"responses\",\"chat_completions\"]', 'x_api_key',
                    '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z'
                 );
                 DROP TABLE IF EXISTS account_model_capabilities;
                 CREATE TABLE account_model_capabilities (
                    account_id TEXT NOT NULL,
                    model_id TEXT NOT NULL,
                    protocol TEXT NOT NULL,
                    verified_at TEXT,
                    source TEXT NOT NULL DEFAULT 'manual',
                    PRIMARY KEY (account_id, model_id, protocol)
                 );
                 INSERT INTO account_model_capabilities
                    (account_id, model_id, protocol, source)
                 VALUES ('custom-v31', 'org/model', 'chat_completions', 'manual');
                 UPDATE accounts
                    SET enabled = 1, verification_status = 'verified',
                        connection_verified_at = '2026-01-01T00:00:00Z'
                  WHERE id = 'custom-v31';
                 DELETE FROM schema_version;
                 INSERT OR REPLACE INTO schema_version (version) VALUES (31);",
        )
        .unwrap();
    drop_unified_provider_tables(&db.conn);
    drop(db);

    let db = Database::open(dir.clone()).unwrap();
    let config = db.account_custom_config("custom-v31").unwrap().unwrap();
    assert_eq!(
        config.upstream_protocol,
        UpstreamProtocolKind::ChatCompletions
    );
    assert_eq!(
        config.endpoint_url,
        "https://api.example.com/v1/chat/completions"
    );
    let migrated = db.get_account("custom-v31").unwrap().unwrap();
    assert!(!migrated.enabled);
    let migrated_state = db
        .account_verification_state("custom-v31")
        .unwrap()
        .unwrap();
    assert_eq!(migrated_state.status, ConnectionVerificationStatus::Pending);
    assert!(migrated_state.connection_verified_at.is_none());
    let capabilities = db.list_account_model_capabilities("custom-v31").unwrap();
    assert_eq!(capabilities.len(), 1);
    assert_eq!(
        capabilities[0].protocol,
        UpstreamProtocolKind::ChatCompletions
    );

    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn v32_to_v33_backfills_public_and_upstream_identities_for_custom_and_goat() {
    let dir = temp_data_dir("v32-v33-model-mapping");
    let db = Database::open(dir.clone()).unwrap();

    let mut custom = account("custom-v32");
    custom.provider_id = CUSTOM_PROVIDER_ID.to_string();
    custom.enabled = false;
    db.create_account_with_contract(
        &custom,
        Some(&AccountCustomConfigInput {
            endpoint_url: "https://api.example.com/v1/chat/completions".into(),
            upstream_protocol: UpstreamProtocolKind::ChatCompletions,
        }),
        &[AccountModelCapabilityInput {
            public_model: "custom-public".into(),
            upstream_model: "custom-public".into(),
            protocol: UpstreamProtocolKind::ChatCompletions,
            source: Some("manual".into()),
        }],
    )
    .unwrap();

    let mut goat = account("goat-v32");
    goat.provider_id = COMMAND_CODE_PROVIDER_ID.to_string();
    goat.enabled = false;
    db.create_account(&goat).unwrap();
    persist_goat_catalog_on(&db.conn, &goat.id, &["goat/model".into()], Some(Utc::now())).unwrap();
    drop(db);

    let conn = Connection::open(dir.join("data.sqlite")).unwrap();
    account_store::materialize_legacy_accounts_for_rewind(&conn).unwrap();
    conn.execute_batch(
        "PRAGMA foreign_keys = OFF;
             DROP INDEX IF EXISTS idx_account_model_capabilities_account;
             DROP TABLE IF EXISTS account_model_capabilities;
             CREATE TABLE account_model_capabilities (
                account_id TEXT NOT NULL,
                model_id TEXT NOT NULL,
                protocol TEXT NOT NULL,
                verified_at TEXT,
                source TEXT NOT NULL DEFAULT 'manual',
                PRIMARY KEY (account_id, model_id, protocol),
                FOREIGN KEY (account_id) REFERENCES accounts(id) ON DELETE CASCADE
             );
             INSERT INTO account_model_capabilities
                (account_id, model_id, protocol, source)
             VALUES
                ('custom-v32', 'custom-public', 'chat_completions', 'manual'),
                ('goat-v32', 'goat/model', 'chat_completions', 'manual');
             CREATE INDEX idx_account_model_capabilities_account
                ON account_model_capabilities(account_id);
             DELETE FROM schema_version;
             INSERT INTO schema_version (version) VALUES (32);
             PRAGMA foreign_keys = ON;",
    )
    .unwrap();
    drop_unified_provider_tables(&conn);
    drop(conn);

    let migrated = Database::open(dir.clone()).unwrap();
    let capabilities = migrated
        .list_account_model_capabilities("custom-v32")
        .unwrap();
    assert_eq!(capabilities.len(), 1);
    assert_eq!(capabilities[0].public_model, "custom-public");
    assert_eq!(capabilities[0].upstream_model, "custom-public");
    assert!(migrated.get_account("goat-v32").unwrap().is_some());
    let goat_catalog: String = migrated
        .conn
        .query_row(
            "SELECT models_json FROM provider_model_catalogs WHERE provider_id = ?1",
            [COMMAND_CODE_PROVIDER_ID],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        goat_catalog.contains("goat/model"),
        "GOAT catalog must survive leftover capability drop: {goat_catalog}"
    );

    drop(migrated);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn v33_reopen_reaches_current_without_cpa_leftover_table() {
    let dir = temp_data_dir("v33-v55-cpa");
    let db = Database::open(dir.clone()).unwrap();
    drop(db);
    let conn = Connection::open(dir.join("data.sqlite")).unwrap();
    conn.execute_batch(
        "DROP TABLE IF EXISTS cpa_integration;
             DELETE FROM schema_version;
             INSERT INTO schema_version (version) VALUES (33);",
    )
    .unwrap();
    drop_unified_provider_tables(&conn);
    drop(conn);

    let migrated = Database::open(dir.clone()).unwrap();
    assert_eq!(
        lifecycle::schema_version_on(&migrated.conn).unwrap(),
        CURRENT_SCHEMA_VERSION
    );
    assert!(!table_exists(&migrated.conn, "cpa_integration").unwrap());
    assert!(migrated.cpa_integration().unwrap().is_none());
    drop(migrated);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn v26_to_v27_copies_keys_drops_columns_and_writes_hashed_backup() {
    let dir = temp_data_dir("v26-v27-migrate");
    let (primary, laptop) = populate_v26_source(&dir);
    let (cipher_bytes, source_accounts, source_subs) = {
        let conn = Connection::open(dir.join("data.sqlite")).unwrap();
        let key: String = conn
            .query_row(
                "SELECT key_cipher FROM accounts WHERE id = 'v26-account'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let accounts: i64 = conn
            .query_row("SELECT COUNT(*) FROM accounts", [], |row| row.get(0))
            .unwrap();
        let subs: i64 = conn
            .query_row("SELECT COUNT(*) FROM sub_gateway_keys", [], |row| {
                row.get(0)
            })
            .unwrap();
        (key, accounts, subs)
    };

    let db = open_with_host_cipher(dir.clone()).expect("v26 database should migrate to v27");
    assert_eq!(
        db.primary_access_key_value().unwrap().as_deref(),
        Some(primary.as_str())
    );
    lifecycle::sqlite_foreign_key_check(&db.conn).unwrap();
    let accounts: i64 = db
        .conn
        .query_row("SELECT COUNT(*) FROM credentials", [], |row| row.get(0))
        .unwrap();
    let keys: i64 = db
        .conn
        .query_row("SELECT COUNT(*) FROM access_keys", [], |row| row.get(0))
        .unwrap();
    assert_eq!(accounts, source_accounts);
    assert_eq!(keys, source_subs + 1);
    let subs = db.list_active_sub_gateway_keys().unwrap();
    assert_eq!(subs.len(), 1);
    assert_eq!(subs[0].id, "sub-v26");
    assert_eq!(subs[0].key, laptop);
    assert!(!table_exists(&db.conn, "sub_gateway_keys").unwrap());
    for column in USAGE_SYNC_ACCOUNT_COLUMNS {
        assert!(!table_has_column(&db.conn, "accounts", column).unwrap());
    }
    let stored_cipher: String = db
        .conn
        .query_row(
            "SELECT key_cipher FROM credentials WHERE legacy_account_id = 'v26-account'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        stored_cipher, cipher_bytes,
        "ciphertext bytes must be preserved"
    );
    let config: serde_json::Value =
        serde_json::from_str(&db.get_setting("config").unwrap().unwrap()).unwrap();
    assert_eq!(config["gateway_key"], "");
    drop(db);

    let backups = pre_v3_backup_paths(&dir);
    assert_eq!(backups.len(), 1);
    let backup_name = backups[0]
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap();
    let backup =
        Connection::open_with_flags(&backups[0], OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    assert_eq!(
        lifecycle::schema_version_on(&backup).unwrap(),
        V26_SCHEMA_VERSION
    );
    lifecycle::sqlite_quick_check(&backup).unwrap();
    assert!(table_exists(&backup, "sub_gateway_keys").unwrap());
    assert!(table_has_column(&backup, "accounts", "usage_sync_last_success_at").unwrap());
    drop(backup);
    let digest = lifecycle::sha256_file(&backups[0]).unwrap();
    let evidence = fs::read_to_string(format!("{}.sha256", backups[0].display())).unwrap();
    assert!(evidence.starts_with(&digest));
    assert!(evidence.contains(backup_name));

    let reopened = open_with_host_cipher(dir.clone()).unwrap();
    drop(reopened);
    assert_eq!(pre_v3_backup_paths(&dir), backups);

    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn v27_fault_before_schema_version_leaves_usable_v26_source() {
    let dir = temp_data_dir("v27-interrupt");
    populate_v26_source(&dir);
    let _guard = arm_v27_fault(V27MigrationFault::BeforeSchemaVersion);
    assert!(open_with_host_cipher(dir.clone()).is_err());
    drop(_guard);
    let conn = Connection::open(dir.join("data.sqlite")).unwrap();
    assert_eq!(
        lifecycle::schema_version_on(&conn).unwrap(),
        V26_SCHEMA_VERSION
    );
    assert!(table_exists(&conn, "sub_gateway_keys").unwrap());
    assert!(!table_exists(&conn, "access_keys").unwrap());
    assert!(table_has_column(&conn, "accounts", "usage_sync_last_success_at").unwrap());
    drop(conn);
    assert_eq!(pre_v3_backup_paths(&dir).len(), 1);
    let db = open_with_host_cipher(dir.clone()).expect("v26 source should still migrate");
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn v27_duplicate_start_converges_on_one_primary() {
    let dir = temp_data_dir("v27-duplicate-start");
    populate_v26_source(&dir);
    let first = dir.clone();
    let second = dir.clone();
    let threads = [
        std::thread::spawn(move || open_with_host_cipher(first)),
        std::thread::spawn(move || open_with_host_cipher(second)),
    ];
    let results = threads
        .into_iter()
        .map(|thread| thread.join().expect("open thread should finish"))
        .collect::<Vec<_>>();
    assert!(
        results.iter().any(|result| result.is_ok()),
        "at least one opener must finish v27: {:?}",
        results
            .iter()
            .map(|result| result
                .as_ref()
                .map(|_| "ok")
                .map_err(|error| error.to_string()))
            .collect::<Vec<_>>()
    );
    for db in results.into_iter().flatten() {
        drop(db);
    }
    let db = open_with_host_cipher(dir.clone()).unwrap();
    let count: i64 = db
        .conn
        .query_row(
            "SELECT COUNT(*) FROM access_keys WHERE is_primary = 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 1);
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn v27_wrong_cipher_fails_closed_without_claiming_v27() {
    let dir = temp_data_dir("v27-wrong-cipher");
    let right: Arc<dyn KeyCipher + Send + Sync> = Arc::new(StaticKeyCipher::new("right-secret"));
    let db = Database::open_with_cipher(dir.clone(), right.clone()).unwrap();
    let mut enc = account("enc-account");
    enc.key_cipher = right.encrypt("sk-live").unwrap();
    enc.password_cipher = Some(right.encrypt("pw-live").unwrap());
    db.create_account(&enc).unwrap();
    drop(db);
    reverse_current_to_v26(&dir);

    struct FailingCipher;
    impl KeyCipher for FailingCipher {
        fn encrypt(&self, plaintext: &str) -> anyhow::Result<String> {
            Ok(plaintext.to_string())
        }
        fn decrypt(&self, _ciphertext: &str) -> anyhow::Result<String> {
            anyhow::bail!("wrong cipher")
        }
    }
    let failing: Arc<dyn KeyCipher + Send + Sync> = Arc::new(FailingCipher);
    assert!(Database::open_with_cipher(dir.clone(), failing).is_err());
    let conn = Connection::open(dir.join("data.sqlite")).unwrap();
    assert_eq!(
        lifecycle::schema_version_on(&conn).unwrap(),
        V26_SCHEMA_VERSION
    );
    drop(conn);

    let recovered = Database::open_with_cipher(dir.clone(), right).unwrap();
    drop(recovered);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn s05_wrong_host_cipher_fails_closed_without_rewriting_ciphertext() {
    let cipher_a: Arc<dyn KeyCipher + Send + Sync> =
        Arc::new(StaticKeyCipher::new("alpha-host-secret"));
    let cipher_b: Arc<dyn KeyCipher + Send + Sync> =
        Arc::new(StaticKeyCipher::new("omega-host-secret"));

    let empty = temp_data_dir("v37-empty-cipher-open");
    drop(Database::open_with_cipher(empty.clone(), cipher_b.clone()).unwrap());
    fs::remove_dir_all(&empty).unwrap();

    let no_auth = temp_data_dir("v37-no-auth-cipher-open");
    drop(Database::open_with_cipher(no_auth.clone(), cipher_a.clone()).unwrap());
    drop(Database::open_with_cipher(no_auth.clone(), cipher_b.clone()).unwrap());
    fs::remove_dir_all(&no_auth).unwrap();

    let dir = temp_data_dir("v37-wrong-host-cipher");
    let key_plain = "sk-preflight-live-key";
    let password_plain = "pw-preflight-live-secret";
    let db = Database::open_with_cipher(dir.clone(), cipher_a.clone()).unwrap();
    assert_eq!(
        lifecycle::schema_version_on(&db.conn).unwrap(),
        CURRENT_SCHEMA_VERSION
    );
    let mut enc = account("enc-current");
    enc.key_cipher = cipher_a.encrypt(key_plain).unwrap();
    enc.password_cipher = Some(cipher_a.encrypt(password_plain).unwrap());
    db.create_account(&enc).unwrap();
    let stored = db.get_account("enc-current").unwrap().unwrap();
    let key_before = stored.key_cipher.clone();
    let password_before = stored.password_cipher.clone();
    drop(db);

    let error = match Database::open_with_cipher(dir.clone(), cipher_b) {
        Ok(_) => panic!("wrong host cipher must fail closed on current schema"),
        Err(error) => error,
    };
    let message = format!("{error:#}");
    assert!(
        message.contains("host cipher rejected") && message.contains("key_cipher"),
        "{message}"
    );
    assert!(
        !message.contains(&key_before)
            && !message.contains(password_before.as_deref().unwrap_or_default())
            && !message.contains(key_plain)
            && !message.contains(password_plain)
            && !message.contains("alpha-host-secret")
            && !message.contains("omega-host-secret"),
        "probe error must not leak ciphertext, plaintext, or host secrets: {message}"
    );

    let conn = Connection::open(dir.join("data.sqlite")).unwrap();
    assert_eq!(
        lifecycle::schema_version_on(&conn).unwrap(),
        CURRENT_SCHEMA_VERSION
    );
    let (key_after, password_after): (String, Option<String>) = conn
        .query_row(
            "SELECT key_cipher, password_cipher FROM credentials WHERE legacy_account_id = ?1",
            ["enc-current"],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(key_after, key_before);
    assert_eq!(password_after, password_before);
    drop(conn);

    Database::open(dir.clone()).expect(
        "current schema still opens without a host cipher; v27 is the rewrite that requires one",
    );
    let recovered = Database::open_with_cipher(dir.clone(), cipher_a).unwrap();
    let loaded = recovered.get_account("enc-current").unwrap().unwrap();
    assert_eq!(loaded.key_cipher, key_before);
    assert_eq!(loaded.password_cipher, password_before);
    drop(recovered);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn s05_legacy_xor_repairs_to_v2_with_correct_cipher() {
    let cipher = StaticKeyCipher::new("legacy-repair-host");
    let cipher_arc: Arc<dyn KeyCipher + Send + Sync> = Arc::new(cipher.clone());
    let key_plain = "sk-legacy-repair-key";
    let password_plain = "pw-legacy-repair-secret";
    let dir = temp_data_dir("legacy-xor-repair");

    let db = Database::open_with_cipher(dir.clone(), cipher_arc.clone()).unwrap();
    let mut enc = account("legacy-repair");
    enc.key_cipher = cipher.encrypt_legacy(key_plain).unwrap();
    enc.password_cipher = Some(cipher.encrypt_legacy(password_plain).unwrap());
    assert!(is_legacy_local_ciphertext(&enc.key_cipher));
    assert!(is_legacy_local_ciphertext(
        enc.password_cipher.as_deref().unwrap()
    ));
    db.create_account(&enc).unwrap();
    let planted = db.get_account("legacy-repair").unwrap().unwrap();
    assert!(is_legacy_local_ciphertext(&planted.key_cipher));
    drop(db);

    let db = Database::open_with_cipher(dir.clone(), cipher_arc.clone()).unwrap();
    let loaded = db.get_account("legacy-repair").unwrap().unwrap();
    assert!(
        loaded.key_cipher.starts_with(LOCAL_CIPHER_V2_PREFIX),
        "open-time repair must rewrite key_cipher to v2"
    );
    assert!(
        loaded
            .password_cipher
            .as_deref()
            .is_some_and(|value| value.starts_with(LOCAL_CIPHER_V2_PREFIX)),
        "open-time repair must rewrite password_cipher to v2"
    );
    assert_eq!(cipher.decrypt(&loaded.key_cipher).unwrap(), key_plain);
    assert_eq!(
        cipher
            .decrypt(loaded.password_cipher.as_deref().unwrap())
            .unwrap(),
        password_plain
    );
    let repaired_key = loaded.key_cipher.clone();
    let repaired_password = loaded.password_cipher.clone();
    drop(db);

    let wrong: Arc<dyn KeyCipher + Send + Sync> =
        Arc::new(StaticKeyCipher::new("wrong-repair-host"));
    let error = match Database::open_with_cipher(dir.clone(), wrong) {
        Ok(_) => panic!("wrong host cipher must fail closed after v2 repair"),
        Err(error) => error,
    };
    let message = format!("{error:#}");
    assert!(
        message.contains("host cipher rejected") && message.contains("key_cipher"),
        "{message}"
    );
    assert!(
        !message.contains(&repaired_key)
            && !message.contains(repaired_password.as_deref().unwrap_or_default())
            && !message.contains(key_plain)
            && !message.contains(password_plain)
            && !message.contains("legacy-repair-host")
            && !message.contains("wrong-repair-host"),
        "repair/probe errors must not leak ciphertext, plaintext, or host secrets: {message}"
    );

    let recovered = Database::open_with_cipher(dir.clone(), cipher_arc).unwrap();
    let loaded = recovered.get_account("legacy-repair").unwrap().unwrap();
    assert_eq!(loaded.key_cipher, repaired_key);
    assert_eq!(loaded.password_cipher, repaired_password);
    drop(recovered);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn v27_open_without_cipher_cannot_bypass_ciphertext() {
    let dir = temp_data_dir("v27-open-bypass");
    let cipher: Arc<dyn KeyCipher + Send + Sync> = Arc::new(StaticKeyCipher::new("host-secret"));
    let db = Database::open_with_cipher(dir.clone(), cipher.clone()).unwrap();
    let mut enc = account("enc-bypass");
    enc.key_cipher = cipher.encrypt("sk-bypass").unwrap();
    db.create_account(&enc).unwrap();
    drop(db);
    reverse_current_to_v26(&dir);
    let error = match Database::open(dir.clone()) {
        Ok(_) => panic!("ciphertext must require the host cipher"),
        Err(error) => error,
    };
    assert!(
        error.to_string().contains("open_with_cipher")
            || error.to_string().contains("host encryption cipher"),
        "{error}"
    );
    let conn = Connection::open(dir.join("data.sqlite")).unwrap();
    assert_eq!(
        lifecycle::schema_version_on(&conn).unwrap(),
        V26_SCHEMA_VERSION
    );
    drop(conn);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn v27_corrupt_account_cipher_fails_before_backup() {
    let dir = temp_data_dir("v27-corrupt-account-cipher");
    populate_v26_source(&dir);
    let conn = Connection::open(dir.join("data.sqlite")).unwrap();
    conn.execute(
        "UPDATE accounts SET key_cipher = '!!!not-base64!!!' WHERE id = 'v26-account'",
        [],
    )
    .unwrap();
    drop(conn);
    let error = match open_with_host_cipher(dir.clone()) {
        Ok(_) => panic!("corrupt account cipher must fail closed"),
        Err(error) => error,
    };
    let message = format!("{error:#}");
    assert!(
        message.contains("key_cipher"),
        "corrupt account cipher must name the column: {message}"
    );
    assert!(
        pre_v3_backup_paths(&dir).is_empty(),
        "corrupt account cipher must fail before the pre-v3 backup"
    );
    let conn = Connection::open(dir.join("data.sqlite")).unwrap();
    assert_eq!(
        lifecycle::schema_version_on(&conn).unwrap(),
        V26_SCHEMA_VERSION
    );
    drop(conn);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn v27_base64_looking_plaintext_access_keys_migrate_without_cipher() {
    let dir = temp_data_dir("v27-b64-plaintext-keys");
    let primary = "ABCDEFGHIJKLMNOPQRSTUVWX";
    let sub = "ZYXWVUTSRQPONMLKJIHGFEDC";
    assert_eq!(primary.len(), 24);
    assert_eq!(sub.len(), 24);
    let db = Database::open(dir.to_path_buf()).unwrap();
    db.insert_sub_gateway_key(&SubGatewayKey {
        id: "sub-b64".into(),
        name: "Laptop".into(),
        key: sub.into(),
        enabled: true,
        deleted_at: None,
        created_at: Utc::now(),
    })
    .unwrap();
    let config = serde_json::json!({
        "gateway_port": 9042,
        "gateway_key": primary,
        "upstream_base_url": "https://opencode.ai/zen/go" });
    db.set_config(&config.to_string()).unwrap();
    drop(db);
    reverse_current_to_v26(&dir);
    let db = Database::open(dir.clone()).expect(
        "24-character base64-looking plaintext primary/sub keys must migrate without a host cipher",
    );
    assert_eq!(
        db.primary_access_key_value().unwrap().as_deref(),
        Some(primary)
    );
    let subs = db.list_active_sub_gateway_keys().unwrap();
    assert_eq!(subs.len(), 1);
    assert_eq!(subs[0].key, sub);
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn v27_corrupted_source_fails_quick_check_without_claiming_v27() {
    let dir = temp_data_dir("v27-corrupt");
    populate_v26_source(&dir);
    let path = dir.join("data.sqlite");
    fs::write(&path, b"not a sqlite database").unwrap();
    assert!(Database::open(dir.clone()).is_err());
    let raw = fs::read(&path).unwrap();
    assert_eq!(&raw, b"not a sqlite database");
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn v27_backup_includes_wal_committed_rows() {
    let dir = temp_data_dir("v27-wal-backup");
    populate_v26_source(&dir);
    let path = dir.join("data.sqlite");
    let writer = Connection::open(&path).unwrap();
    writer.pragma_update(None, "wal_autocheckpoint", 0).unwrap();
    let _mode: String = writer
        .query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))
        .unwrap();
    writer
        .execute(
            "INSERT INTO settings (key, value) VALUES ('wal-marker', 'visible')
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [],
        )
        .unwrap();
    let db = open_with_host_cipher(dir.clone()).expect("WAL source should migrate");
    drop(db);
    drop(writer);
    let backups = pre_v3_backup_paths(&dir);
    assert_eq!(backups.len(), 1);
    let backup =
        Connection::open_with_flags(&backups[0], OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let marker: String = backup
        .query_row(
            "SELECT value FROM settings WHERE key = 'wal-marker'",
            [],
            |row| row.get(0),
        )
        .expect("VACUUM INTO must include WAL-committed rows");
    assert_eq!(marker, "visible");
    drop(backup);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn v27_vacuum_into_writer_rejects_stale_backup_and_retries() {
    let dir = temp_data_dir("v27-vacuum-race");
    populate_v26_source(&dir);
    v27_test_hooks::reset();
    v27_test_hooks::set_race_during_vacuum(true);
    let _guard = V27HookGuard;
    let db = open_with_host_cipher(dir.clone()).expect("raced VACUUM INTO should retry and finish");
    drop(db);
    let backups = pre_v3_backup_paths(&dir);
    assert!(
        backups.len() >= 2,
        "the first backup must be rejected and a fresh backup taken, got {backups:?}"
    );
    let accepted = backups.last().expect("accepted backup should exist");
    let backup = Connection::open_with_flags(accepted, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let marker: String = backup
        .query_row(
            "SELECT value FROM settings WHERE key = 'v27-vacuum-race'",
            [],
            |row| row.get(0),
        )
        .expect("accepted backup must contain the row committed during VACUUM INTO");
    assert_eq!(marker, "committed");
    drop(backup);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn v27_set_config_is_atomic_with_the_primary_row() {
    let dir = temp_data_dir("v27-config-atomic");
    let db = Database::open(dir.clone()).unwrap();
    let original = db.primary_access_key_value().unwrap().unwrap();
    let initial = serde_json::json!({
        "gateway_port": 9042,
        "gateway_key": original,
        "upstream_base_url": "https://opencode.ai/zen/go",
        "connect_timeout_secs": 30,
        "non_stream_timeout_secs": 900,
        "stream_idle_timeout_secs": 300 });
    db.set_config(&initial.to_string()).unwrap();
    db.conn
        .execute_batch(
            "CREATE TRIGGER fail_primary_update
                 BEFORE UPDATE OF key ON access_keys
                 WHEN NEW.is_primary = 1
                 BEGIN
                     SELECT RAISE(ABORT, 'forced primary update failure');
                 END;",
        )
        .unwrap();
    let rotated = serde_json::json!({
        "gateway_port": 9042,
        "gateway_key": "ocg-rotated-primary",
        "upstream_base_url": "https://opencode.ai/zen/go",
        "connect_timeout_secs": 30,
        "non_stream_timeout_secs": 900,
        "stream_idle_timeout_secs": 300 });
    assert!(db.set_config(&rotated.to_string()).is_err());
    assert_eq!(
        db.primary_access_key_value().unwrap().as_deref(),
        Some(original.as_str())
    );
    let stored: serde_json::Value =
        serde_json::from_str(&db.get_setting("config").unwrap().unwrap()).unwrap();
    assert_eq!(stored["gateway_key"], "");
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn v27_primary_row_cannot_be_disabled_or_deleted() {
    let dir = temp_data_dir("v27-primary-protect");
    let db = Database::open(dir.clone()).unwrap();
    assert!(
        !db.set_sub_gateway_key_enabled(PRIMARY_KEY_ID, false)
            .unwrap()
    );
    assert!(
        !db.soft_delete_sub_gateway_key(PRIMARY_KEY_ID, Utc::now())
            .unwrap()
    );
    assert!(
        db.conn
            .execute(
                "UPDATE access_keys SET enabled = 0 WHERE id = ?1",
                [PRIMARY_KEY_ID],
            )
            .is_err()
    );
    assert!(
        db.conn
            .execute("DELETE FROM access_keys WHERE id = ?1", [PRIMARY_KEY_ID])
            .is_err()
    );
    let primary = db.primary_access_key_value().unwrap().unwrap();
    assert!(!primary.is_empty());
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn v31_migration_creates_override_table() {
    let dir = temp_data_dir("v31-migration");
    let db = Database::open(dir.clone()).unwrap();
    db.conn
        .execute_batch(
            "DROP TABLE IF EXISTS provider_contract_model_protocol_overrides;
                 DELETE FROM schema_version;
                 INSERT OR REPLACE INTO schema_version (version) VALUES (30);",
        )
        .unwrap();
    drop_unified_provider_tables(&db.conn);
    drop(db);

    let db = Database::open(dir.clone()).unwrap();
    let table_exists: i64 = db
        .conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master
                 WHERE type = 'table' AND name = 'provider_contract_model_protocol_overrides'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(table_exists, 1);
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn v35_maps_known_pairs_conserves_rows_and_writes_pre_v35_snapshot() {
    let dir = temp_data_dir("v35-known-pairs");
    let db = open_with_host_cipher(dir.clone()).unwrap();
    let mut go = account("v35-go");
    go.key_cipher = fixture_account_key_cipher();
    db.create_account(&go).unwrap();
    let cipher_before = db.get_account("v35-go").unwrap().unwrap().key_cipher;
    db.log_forward(&forward_log("v35-go", "success", 1.25))
        .unwrap();
    let account_count: i64 = db
        .conn
        .query_row("SELECT COUNT(*) FROM credentials", [], |row| row.get(0))
        .unwrap();
    let log_count: i64 = db
        .conn
        .query_row("SELECT COUNT(*) FROM forward_logs", [], |row| row.get(0))
        .unwrap();
    drop(db);

    reverse_current_to_v34(&dir);
    let conn = Connection::open(dir.join("data.sqlite")).unwrap();
    let offering: String = conn
        .query_row(
            "SELECT offering_id FROM accounts WHERE id = ?1",
            ["v35-go"],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(offering, "go");
    drop(conn);

    let db = open_with_host_cipher(dir.clone()).unwrap();
    lifecycle::sqlite_quick_check(&db.conn).unwrap();
    lifecycle::sqlite_foreign_key_check(&db.conn).unwrap();
    let account_count_after: i64 = db
        .conn
        .query_row("SELECT COUNT(*) FROM credentials", [], |row| row.get(0))
        .unwrap();
    let log_count_after: i64 = db
        .conn
        .query_row("SELECT COUNT(*) FROM forward_logs", [], |row| row.get(0))
        .unwrap();
    assert_eq!(account_count_after, account_count);
    assert_eq!(log_count_after, log_count);
    let stored = db.get_account("v35-go").unwrap().unwrap();
    assert_eq!(stored.provider_id, OPENCODE_PROVIDER_ID);
    assert_eq!(stored.key_cipher, cipher_before);
    assert_fixture_account_cipher(&stored.key_cipher);
    let account_columns = v35_column_names(&db.conn, "accounts");
    let log_columns = v35_column_names(&db.conn, "forward_logs");
    let catalog_columns = v35_column_names(&db.conn, "provider_model_catalogs");
    let pricing_columns = v35_column_names(&db.conn, "provider_pricing_snapshots");
    assert!(!account_columns.iter().any(|name| name == "offering_id"));
    assert!(!log_columns.iter().any(|name| name == "offering_id"));
    assert!(!catalog_columns.iter().any(|name| name == "offering_id"));
    assert!(!pricing_columns.iter().any(|name| name == "offering_id"));
    assert!(v35_index_sql(&db.conn, "idx_forward_logs_provider_offering").is_none());
    let backups = pre_v35_backup_paths(&dir);
    assert_eq!(backups.len(), 1);
    let hash_path = backups[0].with_file_name(format!(
        "{}.sha256",
        backups[0].file_name().unwrap().to_str().unwrap()
    ));
    assert!(hash_path.exists());
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn v35_unknown_pair_rolls_back_without_mutation() {
    let dir = temp_data_dir("v35-unknown-pair");
    let db = open_with_host_cipher(dir.clone()).unwrap();
    let mut leftover = account("v35-unknown");
    leftover.key_cipher = fixture_account_key_cipher();
    db.create_account(&leftover).unwrap();
    drop(db);
    reverse_current_to_v34(&dir);
    let conn = Connection::open(dir.join("data.sqlite")).unwrap();
    conn.execute(
        "UPDATE accounts SET provider_id = 'unknown-provider', offering_id = 'unknown-offering'
         WHERE id = ?1",
        ["v35-unknown"],
    )
    .unwrap();
    drop(conn);
    let error = match open_with_host_cipher(dir.clone()) {
        Ok(_) => panic!("unknown pair must fail closed"),
        Err(error) => error,
    };
    let message = format!("{error:#}");
    assert!(
        message.contains("unknown provider/offering pair"),
        "{message}"
    );
    let conn = Connection::open(dir.join("data.sqlite")).unwrap();
    assert_eq!(
        lifecycle::schema_version_on(&conn).unwrap(),
        V34_SCHEMA_VERSION
    );
    assert!(table_has_column(&conn, "accounts", "offering_id").unwrap());
    drop(conn);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn v35_unknown_catalog_pair_rolls_back_without_mutation() {
    let dir = temp_data_dir("v35-catalog-collision");
    let db = open_with_host_cipher(dir.clone()).unwrap();
    drop(db);
    reverse_current_to_v34(&dir);
    let conn = Connection::open(dir.join("data.sqlite")).unwrap();
    conn.execute(
        "INSERT INTO provider_model_catalogs
         (provider_id, offering_id, models_json, refreshed_at, source_url)
         VALUES ('opencode', 'extra', '[]', NULL, 'https://example.test/b')",
        [],
    )
    .unwrap();
    drop(conn);
    let error = match open_with_host_cipher(dir.clone()) {
        Ok(_) => panic!("unknown catalog pair must fail closed"),
        Err(error) => error,
    };
    let message = format!("{error:#}");
    assert!(
        message.contains("unknown provider/offering pair"),
        "{message}"
    );
    let conn = Connection::open(dir.join("data.sqlite")).unwrap();
    assert_eq!(
        lifecycle::schema_version_on(&conn).unwrap(),
        V34_SCHEMA_VERSION
    );
    assert!(table_has_column(&conn, "provider_model_catalogs", "offering_id").unwrap());
    drop(conn);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn v36_to_v37_discards_cookie_usage_state_and_keeps_account_keys() {
    let dir = temp_data_dir("v36-v37-ollama-billing");
    let db = open_with_host_cipher(dir.clone()).unwrap();
    let mut ollama = account("ollama-v36");
    ollama.provider_id = OLLAMA_PROVIDER_ID.to_string();
    ollama.key_cipher = fixture_account_key_cipher();
    db.create_account(&ollama).unwrap();
    let key_before = db.get_account("ollama-v36").unwrap().unwrap().key_cipher;
    drop(db);

    let conn = Connection::open(dir.join("data.sqlite")).unwrap();
    account_store::materialize_legacy_accounts_for_rewind(&conn).unwrap();
    conn.execute_batch(
        "DROP TABLE IF EXISTS ollama_cloud_billing;
         CREATE TABLE IF NOT EXISTS ollama_cloud_usage_state (
            account_id TEXT PRIMARY KEY,
            cookie_cipher TEXT,
            status TEXT NOT NULL DEFAULT 'unconfigured',
            snapshot TEXT,
            last_error TEXT,
            last_success_at TEXT,
            last_attempt_at TEXT,
            next_eligible_at TEXT,
            failure_streak INTEGER NOT NULL DEFAULT 0,
            FOREIGN KEY (account_id) REFERENCES accounts(id) ON DELETE CASCADE
         );
         INSERT INTO ollama_cloud_usage_state (account_id, cookie_cipher, status, snapshot)
         VALUES ('ollama-v36', 'obsolete-cookie', 'ok', '{\"windows\":[]}');
         DELETE FROM schema_version;
         INSERT INTO schema_version (version) VALUES (36);",
    )
    .unwrap();
    drop_unified_provider_tables(&conn);
    drop(conn);

    let migrated = open_with_host_cipher(dir.clone()).unwrap();
    assert!(!table_exists(&migrated.conn, "ollama_cloud_usage_state").unwrap());
    assert!(table_exists(&migrated.conn, "ollama_cloud_billing").unwrap());
    let loaded = migrated.get_account("ollama-v36").unwrap().unwrap();
    assert_eq!(loaded.key_cipher, key_before);
    assert_eq!(
        migrated.ollama_cloud_billing_tier("ollama-v36").unwrap(),
        None
    );
    drop(migrated);
    fs::remove_dir_all(dir).unwrap();
}

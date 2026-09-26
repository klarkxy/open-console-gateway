use super::*;
use super::{V27MigrationFault, v27_test_hooks};
use crate::crypto::{
    KeyCipher, LOCAL_CIPHER_V2_PREFIX, StaticKeyCipher, is_legacy_local_ciphertext,
};
use ocg_domain::dynamic::DynamicAuthKind;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::sync::Arc;

const TEST_HOST_SECRET: &str = "ocg-db-v27-test-host";

fn billing_open_fixture(dir: &Path) -> (Database, i64, crate::billing::CreditAttempt) {
    use crate::billing_types::{CreditBucket, CreditBucketKind, CreditConfiguration, CreditRate};
    let db = open_with_host_cipher(dir.to_path_buf()).unwrap();
    let mut draft = account("billing-open");
    draft.provider_id = CUSTOM_PROVIDER_ID.into();
    draft.key_cipher = fixture_account_key_cipher();
    let endpoint = "https://billing-open.example/v1/chat/completions";
    db.create_account_with_contract(
        &draft,
        Some(&AccountCustomConfigInput {
            endpoint_url: endpoint.into(),
            upstream_protocol: UpstreamProtocolKind::ChatCompletions,
        }),
        &[AccountModelCapabilityInput {
            public_model: "model".into(),
            upstream_model: "model".into(),
            protocol: UpstreamProtocolKind::ChatCompletions,
            source: None,
        }],
    )
    .unwrap();
    let now = Utc::now();
    billing::configure_on(
        &db.conn,
        &draft.id,
        CreditConfiguration {
            name: "Personal".into(),
            currency: "CNY".into(),
            credits_per_currency: 1.0,
            rates: vec![CreditRate {
                model: "model".into(),
                input_per_million: 10.0,
                output_per_million: 20.0,
                cache_read_per_million: None,
                cache_write_per_million: None,
            }],
            monthly: None,
            source_url: None,
        },
        Some(vec![CreditBucket {
            id: "initial".into(),
            kind: CreditBucketKind::Manual,
            label: "Current".into(),
            granted: 100.0,
            remaining: 75.0,
            starts_at: now,
            expires_at: None,
        }]),
        now,
    )
    .unwrap();
    let attempt = billing::capture_on(&db.conn, &draft.id, endpoint, "model", now)
        .unwrap()
        .unwrap();
    let log_id = db
        .log_forward(&forward_log(&draft.id, "streaming", 0.0))
        .unwrap();
    billing::attach_attempt_on(&db.conn, log_id, &attempt).unwrap();
    (db, log_id, attempt)
}

fn assert_billing_open_state(db: &Database, remaining: f64, pending: u64, unpriced: u64) {
    let view = billing::read_view_on(&db.conn, "billing-open", Utc::now())
        .unwrap()
        .unwrap();
    assert_eq!(view.remaining, remaining);
    assert_eq!(view.pending_requests, pending);
    assert_eq!(view.unpriced_requests, unpriced);
}

fn finish_billing_open_attempt(
    db: &Database,
    log_id: i64,
    attempt: &crate::billing::CreditAttempt,
) {
    for _ in 0..2 {
        let tx = db.conn.unchecked_transaction().unwrap();
        billing::settle_on(
            &tx,
            log_id,
            attempt,
            ocg_domain::billing::BillingTokens::new(1_000_000, 0, 0, 0),
            "success",
            Utc::now(),
        )
        .unwrap();
        tx.commit().unwrap();
        assert_billing_open_state(db, 65.0, 0, 0);
    }
}

#[test]
fn billing_open_live_receipt_survives_concurrent_open_and_settles_once() {
    let dir = temp_data_dir("billing-live-open");
    let (db, log_id, attempt) = billing_open_fixture(&dir);
    let second = open_with_host_cipher(dir.clone()).unwrap();
    assert_billing_open_state(&second, 75.0, 1, 0);
    finish_billing_open_attempt(&db, log_id, &attempt);
    assert_billing_open_state(&second, 65.0, 0, 0);
    drop(db);
    drop(second);
    let reopened = open_with_host_cipher(dir.clone()).unwrap();
    assert_billing_open_state(&reopened, 65.0, 0, 0);
    drop(reopened);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn billing_open_recovers_uncertainty_only_after_last_handle_closes() {
    let dir = temp_data_dir("billing-cold-open");
    let (db, _, _) = billing_open_fixture(&dir);
    let second = open_with_host_cipher(dir.clone()).unwrap();
    drop(db);
    let third = open_with_host_cipher(dir.clone()).unwrap();
    assert_billing_open_state(&third, 75.0, 1, 0);
    drop(second);
    drop(third);
    for _ in 0..2 {
        let reopened = open_with_host_cipher(dir.clone()).unwrap();
        assert_billing_open_state(&reopened, 75.0, 0, 1);
    }
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn billing_open_failed_recovery_rolls_back_and_releases_lifetime_lock() {
    let dir = temp_data_dir("billing-failed-open");
    let (db, _, _) = billing_open_fixture(&dir);
    db.conn.execute_batch("CREATE TRIGGER block_receipt BEFORE UPDATE OF credit_receipt_json ON forward_logs BEGIN SELECT RAISE(ABORT,'fixture recovery failure'); END;").unwrap();
    drop(db);
    assert!(open_with_host_cipher(dir.clone()).is_err());
    let guard = open_guard::DatabaseOpenGuard::acquire(&dir).unwrap();
    assert!(
        guard.can_recover_pending(),
        "failed open must release its lock"
    );
    let conn = Connection::open(dir.join("data.sqlite")).unwrap();
    let state = billing::load_on(&conn, "billing-open").unwrap().unwrap();
    assert_eq!(state.unpriced_requests, 0, "failed recovery must roll back");
    assert_eq!(
        billing::pending_count_on(&conn, &state.credential_id, &state.meter_id).unwrap(),
        1
    );
    conn.execute_batch("DROP TRIGGER block_receipt;").unwrap();
    drop(conn);
    drop(guard);
    let recovered = open_with_host_cipher(dir.clone()).unwrap();
    assert_billing_open_state(&recovered, 75.0, 0, 1);
    drop(recovered);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn billing_open_waiter_recovers_when_cold_initializer_fails() {
    use std::sync::{Mutex, mpsc};
    use std::time::Duration as StdDuration;

    struct BlockedFailingCipher {
        entered: mpsc::Sender<()>,
        fail: Mutex<mpsc::Receiver<()>>,
    }

    impl KeyCipher for BlockedFailingCipher {
        fn encrypt(&self, _plaintext: &str) -> Result<String> {
            anyhow::bail!("fixture does not encrypt")
        }

        fn decrypt(&self, _ciphertext: &str) -> Result<String> {
            self.entered.send(()).unwrap();
            self.fail
                .lock()
                .unwrap()
                .recv_timeout(StdDuration::from_secs(5))
                .unwrap();
            anyhow::bail!("fixture cold initializer failure")
        }
    }

    let dir = temp_data_dir("billing-failed-initializer-waiter");
    let (db, _, _) = billing_open_fixture(&dir);
    drop(db);
    let (entered_tx, entered_rx) = mpsc::channel();
    let (fail_tx, fail_rx) = mpsc::channel();
    let cipher = Arc::new(BlockedFailingCipher {
        entered: entered_tx,
        fail: Mutex::new(fail_rx),
    });
    let first_dir = dir.clone();
    let first = std::thread::spawn(move || Database::open_with_cipher(first_dir, cipher).is_err());
    entered_rx.recv_timeout(StdDuration::from_secs(5)).unwrap();
    let (started_tx, started_rx) = mpsc::channel();
    let (opened_tx, opened_rx) = mpsc::channel();
    let second_dir = dir.clone();
    let second = std::thread::spawn(move || {
        started_tx.send(()).unwrap();
        opened_tx.send(open_with_host_cipher(second_dir)).unwrap();
    });
    started_rx.recv().unwrap();
    assert!(matches!(
        opened_rx.recv_timeout(StdDuration::from_millis(100)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));
    fail_tx.send(()).unwrap();
    assert!(first.join().unwrap());
    let recovered = opened_rx
        .recv_timeout(StdDuration::from_secs(5))
        .unwrap()
        .unwrap();
    second.join().unwrap();
    assert_billing_open_state(&recovered, 75.0, 0, 1);
    drop(recovered);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn billing_open_early_failure_releases_lifetime_lock() {
    let dir = temp_data_dir("billing-early-failed-open");
    fs::create_dir(dir.join("data.sqlite")).unwrap();
    assert!(Database::open(dir.clone()).is_err());
    let guard = open_guard::DatabaseOpenGuard::acquire(&dir).unwrap();
    assert!(guard.can_recover_pending());
    drop(guard);
    fs::remove_dir_all(dir).unwrap();
}

// Run by the parent test in another process, including on Windows. The child
// remains open while the parent finalizes the original pending receipt.
#[test]
fn billing_open_subprocess() {
    let Some(dir) = std::env::var_os("OCG_TEST_BILLING_OPEN_DIR") else {
        return;
    };
    let dir = PathBuf::from(dir);
    let db = open_with_host_cipher(dir.clone()).unwrap();
    assert_billing_open_state(&db, 75.0, 1, 0);
    fs::write(dir.join("child-opened"), b"ready").unwrap();
    let mut signal = [0];
    std::io::stdin().read_exact(&mut signal).unwrap();
    assert_billing_open_state(&db, 65.0, 0, 0);
}

#[test]
fn billing_open_cross_process_receipt_survives_and_settles_once() {
    use std::process::{Command, Stdio};
    use std::time::{Duration as StdDuration, Instant};
    let dir = temp_data_dir("billing-process-open");
    let (db, log_id, attempt) = billing_open_fixture(&dir);
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "db::tests::billing_open_subprocess",
            "--nocapture",
        ])
        .env("OCG_TEST_BILLING_OPEN_DIR", &dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + StdDuration::from_secs(30);
    while !dir.join("child-opened").exists() {
        if child.try_wait().unwrap().is_some() || Instant::now() >= deadline {
            let _ = child.kill();
            let output = child.wait_with_output().unwrap();
            panic!("concurrent child open failed: {output:?}");
        }
        std::thread::sleep(StdDuration::from_millis(10));
    }
    finish_billing_open_attempt(&db, log_id, &attempt);
    child.stdin.take().unwrap().write_all(&[1]).unwrap();
    while child.try_wait().unwrap().is_none() {
        if Instant::now() >= deadline {
            let _ = child.kill();
            let output = child.wait_with_output().unwrap();
            panic!("child settlement check timed out: {output:?}");
        }
        std::thread::sleep(StdDuration::from_millis(10));
    }
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "{output:?}");
    drop(db);
    let reopened = open_with_host_cipher(dir.clone()).unwrap();
    assert_billing_open_state(&reopened, 65.0, 0, 0);
    drop(reopened);
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
            assert_eq!(schema_version_on(&conn).unwrap(), 41);
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
    assert_eq!(schema_version_on(&db.conn).unwrap(), CURRENT_SCHEMA_VERSION);
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
fn authorized_builtin_protocol_edit_grants_only_selected_credentials_atomically() {
    use ocg_domain::connection::{
        EndpointOperation, LegacyConnectionKind, connection_id_for_legacy, endpoint_id_for,
    };
    use ocg_domain::credential::credential_id_for_legacy_account;

    let dir = temp_data_dir("authorized-protocol-grants");
    let db = open_with_host_cipher(dir.clone()).unwrap();
    let mut goat_a = account("authorized-goat-a");
    goat_a.provider_id = COMMAND_CODE_PROVIDER_ID.into();
    goat_a.key_cipher = fixture_account_key_cipher();
    let mut goat_b = account("authorized-goat-b");
    goat_b.provider_id = COMMAND_CODE_PROVIDER_ID.into();
    goat_b.key_cipher = fixture_account_key_cipher();
    let mut foreign = account("authorized-foreign");
    foreign.provider_id = OPENCODE_PROVIDER_ID.into();
    foreign.key_cipher = fixture_account_key_cipher();
    db.create_account(&goat_a).unwrap();
    db.create_account(&goat_b).unwrap();
    db.create_account(&foreign).unwrap();

    let goat_a_id = credential_id_for_legacy_account(&goat_a.id).to_string();
    let goat_b_id = credential_id_for_legacy_account(&goat_b.id).to_string();
    let foreign_id = credential_id_for_legacy_account(&foreign.id).to_string();
    let goat_connection = connection_id_for_legacy(
        LegacyConnectionKind::BuiltinProvider,
        COMMAND_CODE_PROVIDER_ID,
    );
    let responses_endpoint =
        endpoint_id_for(&goat_connection, EndpointOperation::ResponseCreate).to_string();
    for credential_id in [&goat_a_id, &goat_b_id] {
        db.conn
            .execute(
                "DELETE FROM credential_grants WHERE credential_id = ?1 AND kind = 'endpoint_id' AND value = ?2",
                params![credential_id, responses_endpoint],
            )
            .unwrap();
    }
    fn grants_for(db: &Database, credential_id: &str) -> Vec<(String, String)> {
        db.conn
            .prepare(
                "SELECT kind, value FROM credential_grants WHERE credential_id = ?1 ORDER BY kind, value",
            )
            .unwrap()
            .query_map([credential_id], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
    }
    let before_a = grants_for(&db, &goat_a_id);
    let before_b = grants_for(&db, &goat_b_id);
    let before_foreign = grants_for(&db, &foreign_id);
    assert!(
        !before_a
            .iter()
            .any(|(_, value)| value == &responses_endpoint)
    );
    assert!(
        !before_b
            .iter()
            .any(|(_, value)| value == &responses_endpoint)
    );

    let scope = ContractScope::provider(COMMAND_CODE_PROVIDER_ID);
    let rows = [(
        COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM.into(),
        UpstreamProtocolKind::Responses,
        ProtocolOverrideState::ForceOn,
    )];
    let now = Utc::now();
    db.set_model_protocol_settings(&scope, &rows, &[], now)
        .unwrap();
    assert_eq!(grants_for(&db, &goat_a_id), before_a);
    assert_eq!(grants_for(&db, &goat_b_id), before_b);

    db.set_model_protocol_settings_authorized(
        &scope,
        &rows,
        &[],
        now,
        std::slice::from_ref(&goat_a_id),
    )
    .unwrap();
    let after_a = grants_for(&db, &goat_a_id);
    assert!(
        after_a
            .iter()
            .any(|(kind, value)| kind == "endpoint_id" && value == &responses_endpoint)
    );
    assert_eq!(
        after_a
            .iter()
            .filter(|(_, value)| value != &responses_endpoint)
            .cloned()
            .collect::<Vec<_>>(),
        before_a
    );
    assert_eq!(grants_for(&db, &goat_b_id), before_b);
    assert_eq!(grants_for(&db, &foreign_id), before_foreign);

    let contract_before_foreign = db.load_persisted_contracts().unwrap();
    let grants_before_foreign = [
        grants_for(&db, &goat_a_id),
        grants_for(&db, &goat_b_id),
        grants_for(&db, &foreign_id),
    ];
    assert!(
        db.set_model_protocol_settings_authorized(
            &scope,
            &rows,
            &[],
            now,
            &[goat_a_id.clone(), foreign_id.clone()],
        )
        .is_err()
    );
    assert_eq!(
        db.load_persisted_contracts().unwrap(),
        contract_before_foreign
    );
    assert_eq!(grants_for(&db, &goat_a_id), grants_before_foreign[0]);
    assert_eq!(grants_for(&db, &goat_b_id), grants_before_foreign[1]);
    assert_eq!(grants_for(&db, &foreign_id), grants_before_foreign[2]);

    let contract_before_duplicate = db.load_persisted_contracts().unwrap();
    let grants_before_duplicate = [
        grants_for(&db, &goat_a_id),
        grants_for(&db, &goat_b_id),
        grants_for(&db, &foreign_id),
    ];
    assert!(
        db.set_model_protocol_settings_authorized(
            &scope,
            &rows,
            &[],
            now,
            &[goat_a_id.clone(), goat_a_id.clone()],
        )
        .is_err()
    );
    assert_eq!(
        db.load_persisted_contracts().unwrap(),
        contract_before_duplicate
    );
    assert_eq!(grants_for(&db, &goat_a_id), grants_before_duplicate[0]);
    assert_eq!(grants_for(&db, &goat_b_id), grants_before_duplicate[1]);
    assert_eq!(grants_for(&db, &foreign_id), grants_before_duplicate[2]);

    drop(db);
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
            assert_eq!(schema_version_on(&conn).unwrap(), 42);
        }));
    }
    for worker in workers {
        worker.join().unwrap();
    }
    let conn = Connection::open(&path).unwrap();
    assert_eq!(schema_version_on(&conn).unwrap(), 42);
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
    assert_eq!(schema_version_on(&db.conn).unwrap(), CURRENT_SCHEMA_VERSION);
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
    assert_eq!(schema_version_on(&db.conn).unwrap(), CURRENT_SCHEMA_VERSION);
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
    assert_eq!(schema_version_on(&db.conn).unwrap(), CURRENT_SCHEMA_VERSION);
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
    assert_eq!(schema_version_on(&db.conn).unwrap(), CURRENT_SCHEMA_VERSION);
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
    assert_eq!(schema_version_on(&db.conn).unwrap(), 43);
    assert!(!table_exists(&db.conn, "dashboard_operations").unwrap());
    drop(db);

    let db = open_with_host_cipher(dir.clone()).unwrap();
    assert_eq!(schema_version_on(&db.conn).unwrap(), CURRENT_SCHEMA_VERSION);
    assert!(table_exists(&db.conn, "dashboard_operations").unwrap());
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

fn rewind_identity_model_to_v44(conn: &Connection) {
    account_store::materialize_legacy_accounts_for_rewind(conn).unwrap();
    conn.execute_batch(
        "DROP TABLE IF EXISTS quota_pool_members;
         DROP TABLE IF EXISTS quota_pools;
         DROP TABLE IF EXISTS subscription_records;
         DROP TABLE IF EXISTS onboarding_tasks;
         DROP TABLE IF EXISTS legacy_identity_map;
         DROP TABLE IF EXISTS credential_bindings;
         DROP TABLE IF EXISTS credential_state;
         DROP TABLE IF EXISTS upstream_identities;
         UPDATE accounts SET identity_id = NULL;
         DELETE FROM schema_version;
         INSERT INTO schema_version(version) VALUES (44);",
    )
    .unwrap();
}

#[test]
fn v45_fresh_database_is_current_and_v44_reopen_migrates() {
    let fresh = temp_data_dir("v45-fresh");
    let db = open_with_host_cipher(fresh.clone()).unwrap();
    assert_eq!(schema_version_on(&db.conn).unwrap(), CURRENT_SCHEMA_VERSION);
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
    assert_eq!(schema_version_on(&db.conn).unwrap(), 44);
    assert!(!table_exists(&db.conn, "upstream_identities").unwrap());
    drop(db);

    let db = open_with_host_cipher(dir.clone()).unwrap();
    assert_eq!(schema_version_on(&db.conn).unwrap(), CURRENT_SCHEMA_VERSION);
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
    assert_eq!(schema_version_on(&db.conn).unwrap(), 44);
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
    assert_eq!(schema_version_on(&conn).unwrap(), 44);
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
    assert_eq!(schema_version_on(&db.conn).unwrap(), 44);
    drop(db);

    let db = open_with_host_cipher(dir.clone()).unwrap();
    assert_eq!(schema_version_on(&db.conn).unwrap(), CURRENT_SCHEMA_VERSION);
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
fn unlink_to_unchanged_empty_custom_source_does_not_duplicate_connection() {
    use crate::platform::{PlatformGroup, PlatformKind};

    let dir = temp_data_dir("platform-unlink-reuses-empty-source");
    let mut db = open_with_host_cipher(dir.clone()).unwrap();
    let mut key = account("site-key");
    key.provider_id = CUSTOM_PROVIDER_ID.into();
    key.key_cipher = fixture_account_key_cipher();
    db.create_account_with_contract(
        &key,
        Some(&AccountCustomConfigInput {
            endpoint_url: "https://site.example/chat".into(),
            upstream_protocol: UpstreamProtocolKind::ChatCompletions,
        }),
        &[AccountModelCapabilityInput {
            public_model: "site-model".into(),
            upstream_model: "site-model".into(),
            protocol: UpstreamProtocolKind::ChatCompletions,
            source: None,
        }],
    )
    .unwrap();
    let source_id = ocg_domain::destination::destination_id_for_custom_account(&key.id);
    db.create_platform_account(
        "site-parent",
        PlatformKind::NewApi,
        "Site Parent",
        "https://site.example/chat/v1",
        None,
    )
    .unwrap();
    db.link_platform_account(&key.id, "site-parent", &PlatformGroup::default())
        .unwrap();
    db.unlink_platform_account(&key.id).unwrap();
    let destination_id: String = db
        .conn
        .query_row(
            "SELECT destination_id FROM credentials WHERE legacy_account_id = ?1",
            [&key.id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(destination_id, source_id);
    db.delete_account(&key.id).unwrap();
    db.delete_platform_account("site-parent").unwrap();
    let remaining: i64 = db
        .conn
        .query_row(
            "SELECT COUNT(*) FROM destinations WHERE legacy_kind = 'custom_account'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(remaining, 1);
    drop(db);
    let reopened = open_with_host_cipher(dir.clone()).unwrap();
    let projected = crate::destination_projection::load_persisted(&reopened).unwrap();
    let custom = projected
        .destinations
        .iter()
        .filter(|row| {
            matches!(
                &row.legacy,
                ocg_domain::destination::LegacyDestinationRef::CustomAccount(_)
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(custom.len(), 1);
    assert_eq!(custom[0].id, source_id);
    drop(reopened);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn shared_custom_link_and_unlink_preserve_sibling_connection() {
    use crate::platform::{PlatformGroup, PlatformKind};

    let dir = temp_data_dir("shared-custom-link-unlink");
    let db = open_with_host_cipher(dir.clone()).unwrap();
    let mut owner = account("shared-owner");
    owner.provider_id = CUSTOM_PROVIDER_ID.into();
    owner.key_cipher = fixture_account_key_cipher();
    db.create_account_with_contract(
        &owner,
        Some(&AccountCustomConfigInput {
            endpoint_url: "https://shared-source.example/v1/chat/completions".into(),
            upstream_protocol: UpstreamProtocolKind::ChatCompletions,
        }),
        &[AccountModelCapabilityInput {
            public_model: "shared-model".into(),
            upstream_model: "vendor/shared-model".into(),
            protocol: UpstreamProtocolKind::ChatCompletions,
            source: None,
        }],
    )
    .unwrap();
    db.replace_credential_id("shared-owner", "00000000-0000-4000-8000-00000000c0de")
        .unwrap();
    let source_destination = ocg_domain::destination::destination_id_for_custom_account(&owner.id);
    let mut sibling = account("shared-sibling");
    sibling.provider_id = CUSTOM_PROVIDER_ID.into();
    sibling.key_cipher = fixture_account_key_cipher();
    db.commit_onboarding_existing_account(
        &sibling,
        Some(&source_destination),
        &NewDashboardOperation {
            operation_id: uuid::Uuid::new_v4().to_string(),
            kind: "onboarding_commit".into(),
            payload_digest: "2".repeat(64),
            result_json: "{}".into(),
        },
    )
    .unwrap();
    db.create_platform_account(
        "shared-parent",
        PlatformKind::NewApi,
        "Shared Parent",
        "https://shared-source.example/v1",
        Some("obfuscated-test-credential"),
    )
    .unwrap();

    db.link_platform_account("shared-owner", "shared-parent", &PlatformGroup::default())
        .unwrap();
    let sibling_after_link = db.account_custom_config("shared-sibling").unwrap().unwrap();
    assert_eq!(
        sibling_after_link.endpoint_url,
        "https://shared-source.example/v1/chat/completions"
    );
    let sibling_destination: String = db
        .conn
        .query_row(
            "SELECT destination_id FROM credentials WHERE legacy_account_id = 'shared-sibling'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(sibling_destination, source_destination);

    db.unlink_platform_account("shared-owner").unwrap();
    let owner_destination: String = db
        .conn
        .query_row(
            "SELECT destination_id FROM credentials WHERE legacy_account_id = 'shared-owner'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_ne!(owner_destination, source_destination);
    let owner_legacy_id: String = db
        .conn
        .query_row(
            "SELECT legacy_id FROM destinations WHERE id = ?1",
            [&owner_destination],
            |row| row.get(0),
        )
        .unwrap();
    let owner_connection = ocg_domain::connection::connection_id_for_legacy(
        ocg_domain::connection::LegacyConnectionKind::CustomAccount,
        &owner_legacy_id,
    );
    let expected_endpoint = ocg_domain::connection::endpoint_id_for(
        &owner_connection,
        ocg_domain::connection::EndpointOperation::ChatCreate,
    );
    let owner_binding = db
        .list_inference_bindings()
        .unwrap()
        .into_iter()
        .find(|binding| binding.account_id == "shared-owner")
        .unwrap();
    assert_eq!(
        owner_binding.allowed_endpoint_ids,
        vec![expected_endpoint.to_string()]
    );
    let sibling_destination: String = db
        .conn
        .query_row(
            "SELECT destination_id FROM credentials WHERE legacy_account_id = 'shared-sibling'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(sibling_destination, source_destination);
    assert_eq!(
        db.account_custom_config("shared-sibling")
            .unwrap()
            .unwrap()
            .endpoint_url,
        "https://shared-source.example/v1/chat/completions"
    );
    assert_eq!(
        db.account_custom_config("shared-owner")
            .unwrap()
            .unwrap()
            .endpoint_url,
        "https://shared-source.example"
    );

    let binding_id = owner_binding.binding_id;
    db.update_credential_binding(&binding_id, None, None, Some(&[]), Some(&[]))
        .unwrap();
    db.link_platform_account("shared-owner", "shared-parent", &PlatformGroup::default())
        .unwrap();
    db.unlink_platform_account("shared-owner").unwrap();
    let revoked = db
        .list_inference_bindings()
        .unwrap()
        .into_iter()
        .find(|binding| binding.account_id == "shared-owner")
        .unwrap();
    assert!(revoked.allowed_endpoint_ids.is_empty());
    assert!(revoked.allowed_origins.is_empty());

    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn substantive_custom_destination_edit_invalidates_verification_and_auth_projection() {
    let dir = temp_data_dir("custom-edit-invalidates-verification");
    let db = open_with_host_cipher(dir.clone()).unwrap();
    let mut custom = account("custom-verified");
    custom.provider_id = CUSTOM_PROVIDER_ID.into();
    custom.key_cipher = fixture_account_key_cipher();
    db.create_account_with_contract(
        &custom,
        Some(&AccountCustomConfigInput {
            endpoint_url: "https://before.example/v1/chat/completions".into(),
            upstream_protocol: UpstreamProtocolKind::ChatCompletions,
        }),
        &[AccountModelCapabilityInput {
            public_model: "verified-model".into(),
            upstream_model: "vendor/verified-model".into(),
            protocol: UpstreamProtocolKind::ChatCompletions,
            source: None,
        }],
    )
    .unwrap();
    db.conn
        .execute(
            "UPDATE credentials SET verification_status = 'verified',
                 connection_verified_at = '2026-09-20T00:00:00Z',
                 cooldown_generic_until = '2026-09-21T00:00:00Z'
             WHERE legacy_account_id = 'custom-verified'",
            [],
        )
        .unwrap();
    account_store::sync_inference_credential_projection_on(&db.conn, "custom-verified").unwrap();
    let destination_id =
        ocg_domain::destination::destination_id_for_custom_account("custom-verified");
    db.replace_custom_destination(
        &destination_id,
        &ocg_domain::dynamic::DynamicProviderDefinition {
            preset_id: None,
            id: "custom-verified".into(),
            name: "After".into(),
            endpoint_url: "https://after.example/v1/chat/completions".into(),
            upstream_protocol: UpstreamProtocolKind::ChatCompletions,
            auth_kind: ocg_domain::dynamic::DynamicAuthKind::Bearer,
            mappings: vec![ocg_domain::dynamic::DynamicModelMapping {
                public_model: "verified-model".into(),
                upstream_model: "vendor/verified-model".into(),
                upstream_override: None,
            }],
        },
        &[],
    )
    .unwrap();
    let state: (String, Option<String>, String, Option<String>) = db
        .conn
        .query_row(
            "SELECT verification_status, connection_verified_at, auth_state,
                    cooldown_generic_until
             FROM credentials WHERE legacy_account_id = 'custom-verified'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(state.0, "pending");
    assert!(state.1.is_none());
    assert_eq!(state.2, "unknown");
    assert_eq!(state.3.as_deref(), Some("2026-09-21T00:00:00Z"));

    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn d02_shared_identity_pool_fans_out_cooldown_to_sibling_key() {
    use crate::models::{UpstreamChannel, local_today};
    use crate::provider::ConnectionVerificationStatus;
    use ocg_domain::credential::{identity_id_for_legacy_account, quota_pool_id_for_identity};

    let dir = temp_data_dir("d02-shared-pool");
    let db = open_with_host_cipher(dir.clone()).unwrap();
    let mut first = account("pool-a");
    first.name = "Pool A".into();
    first.key_cipher = fixture_account_key_cipher();
    db.create_account(&first).unwrap();
    let identity_id: String = db
        .conn
        .query_row(
            "SELECT identity_id FROM credentials WHERE legacy_account_id = 'pool-a'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        identity_id,
        identity_id_for_legacy_account("pool-a").as_str()
    );

    let mut second = account("pool-b");
    second.name = "Pool B".into();
    second.key_cipher = fixture_account_key_cipher();
    let created = db
        .create_account_for_identity(
            &identity_id,
            &second,
            &local_today(),
            ConnectionVerificationStatus::NotRequired,
            crate::db::identity::QuotaSharingJoin::Shared {
                source_credential_id: ocg_domain::credential::credential_id_for_legacy_account(
                    "pool-a",
                )
                .to_string(),
            },
            None,
        )
        .unwrap();
    assert_eq!(created.identity_id, identity_id);
    assert_ne!(created.account_id, "pool-a");

    let pool_id = quota_pool_id_for_identity(&identity_id);
    let members: Vec<String> = db
        .conn
        .prepare("SELECT account_id FROM quota_pool_members WHERE pool_id = ?1 ORDER BY account_id")
        .unwrap()
        .query_map([pool_id.as_str()], |row| row.get(0))
        .unwrap()
        .map(|row| row.unwrap())
        .collect();
    assert_eq!(members, vec!["pool-a".to_string(), "pool-b".to_string()]);
    let confidence: String = db
        .conn
        .query_row(
            "SELECT relation_confidence FROM quota_pools WHERE id = ?1",
            [pool_id.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(confidence, "declared");

    let until = Utc::now() + chrono::Duration::hours(2);
    db.set_account_rate_limit(
        "pool-a",
        until,
        "429 exhausted",
        Some(UsageWindowKind::FiveHours),
    )
    .unwrap();
    let sibling = db.get_account("pool-b").unwrap().expect("sibling");
    assert_eq!(sibling.cooldown_5h_until, Some(until));
    assert!(sibling.is_cooling_for(UpstreamChannel::Go, Utc::now()));
    assert_eq!(sibling.last_error.as_deref(), Some("429 exhausted"));
    assert!(sibling.auth_error.is_none());

    db.set_account_auth_error("pool-a", Some("401 only A"))
        .unwrap();
    let sibling = db.get_account("pool-b").unwrap().expect("sibling");
    assert!(sibling.auth_error.is_none());
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
fn identity_repair_preserves_existing_shared_graph_and_ids() {
    use ocg_domain::credential::ModelScope;
    let dir = temp_data_dir("repair-preserves-shared-graph");
    let db = open_with_host_cipher(dir.clone()).unwrap();
    let mut first = account("repair-shared-a");
    first.key_cipher = fixture_account_key_cipher();
    db.create_account(&first).unwrap();
    let identity_id = db
        .list_identity_model()
        .unwrap()
        .accounts
        .into_iter()
        .find(|row| row.account.id == first.id)
        .unwrap()
        .identity_id;
    let mut second = account("repair-shared-b");
    second.key_cipher = fixture_account_key_cipher();
    db.create_account_for_identity(
        &identity_id,
        &second,
        &local_today(),
        ConnectionVerificationStatus::NotRequired,
        crate::db::identity::QuotaSharingJoin::Shared {
            source_credential_id: ocg_domain::credential::credential_id_for_legacy_account(
                "repair-shared-a",
            )
            .to_string(),
        },
        None,
    )
    .unwrap();
    let mut other = account("repair-unrelated");
    other.key_cipher = fixture_account_key_cipher();
    db.create_account(&other).unwrap();
    let credential_id = "00000000-0000-4000-8000-000000000091";
    let binding_id = "00000000-0000-4000-8000-000000000092";
    let pool_id = "00000000-0000-4000-8000-000000000093";
    let scope = ModelScope::Only {
        models: vec!["glm-5.1".into()],
    };
    db.conn
        .execute(
            "UPDATE credentials SET id=?2, credential_version=7, auth_state_version=7
             WHERE legacy_account_id=?1",
            params![second.id, credential_id],
        )
        .unwrap();
    db.conn
        .execute(
            "UPDATE credentials SET binding_id=?2, scope_json=?3, binding_enabled=0
             WHERE legacy_account_id=?1",
            params![
                second.id,
                binding_id,
                serde_json::to_string(&scope).unwrap()
            ],
        )
        .unwrap();
    let old_pool: String = db
        .conn
        .query_row(
            "SELECT pool_id FROM quota_pool_members WHERE account_id=?1",
            [&first.id],
            |row| row.get(0),
        )
        .unwrap();
    db.conn
        .execute(
            "INSERT INTO quota_pools SELECT ?2, subject_kind, subject_ref,
        relation_confidence, policy_mode, created_at FROM quota_pools WHERE id=?1",
            params![old_pool, pool_id],
        )
        .unwrap();
    db.conn
        .execute(
            "UPDATE quota_pool_members SET pool_id=?2 WHERE pool_id=?1",
            params![old_pool, pool_id],
        )
        .unwrap();
    db.conn
        .execute("DELETE FROM quota_pools WHERE id=?1", [&old_pool])
        .unwrap();
    db.conn
        .execute(
            "UPDATE credentials SET binding_id = NULL WHERE legacy_account_id=?1",
            [&other.id],
        )
        .unwrap();
    drop(db);

    let db = open_with_host_cipher(dir.clone()).unwrap();
    let snapshot = db.list_identity_model().unwrap();
    let repaired = snapshot
        .accounts
        .iter()
        .find(|row| row.account.id == second.id)
        .unwrap();
    assert_eq!(repaired.identity_id, identity_id);
    assert_eq!(repaired.credential_id, credential_id);
    assert_eq!(repaired.credential_version, 7);
    assert_eq!(repaired.binding_id, binding_id);
    assert!(!repaired.binding_enabled);
    assert_eq!(repaired.binding_model_scope, scope);
    assert_eq!(
        db.shared_pool_account_ids(&first.id).unwrap(),
        vec![first.id.clone(), second.id.clone()]
    );
    let pools: Vec<String> = db
        .conn
        .prepare("SELECT pool_id FROM quota_pool_members WHERE account_id=?1")
        .unwrap()
        .query_map([&second.id], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(pools, vec![pool_id]);
    let bindings: i64 = db
        .conn
        .query_row(
            "SELECT COUNT(*) FROM credentials
             WHERE legacy_account_id=?1 AND binding_id IS NOT NULL AND binding_id <> ''",
            [&second.id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(bindings, 1);
    assert!(
        snapshot
            .accounts
            .iter()
            .any(|row| row.account.id == other.id)
    );
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn saved_grants_remain_revoked_across_reopen_and_rotation() {
    use ocg_domain::credential::credential_id_for_legacy_account;

    let dir = temp_data_dir("v46-grants-once");
    let db = open_with_host_cipher(dir.clone()).unwrap();
    let mut keyed = account("grant-go");
    keyed.key_cipher = fixture_account_key_cipher();
    db.create_account(&keyed).unwrap();
    let stored = db
        .list_identity_model()
        .unwrap()
        .accounts
        .into_iter()
        .find(|row| row.account.id == "grant-go")
        .unwrap();
    assert!(!stored.allowed_endpoint_ids.is_empty());
    assert!(stored.allowed_origins.is_empty());
    db.conn
        .execute(
            "DELETE FROM credential_grants WHERE credential_id = ?1",
            [credential_id_for_legacy_account("grant-go").as_str()],
        )
        .unwrap();
    db.conn
        .execute(
            "UPDATE credentials SET grants_initialized = 1 WHERE legacy_account_id='grant-go'",
            [],
        )
        .unwrap();
    drop(db);

    let db = open_with_host_cipher(dir.clone()).unwrap();
    let reopened = db
        .list_identity_model()
        .unwrap()
        .accounts
        .into_iter()
        .find(|row| row.account.id == "grant-go")
        .unwrap();
    assert!(reopened.allowed_endpoint_ids.is_empty());
    assert!(reopened.allowed_origins.is_empty());
    db.rotate_account_credential("grant-go", &fixture_account_key_cipher())
        .unwrap();
    let rotated = db
        .list_identity_model()
        .unwrap()
        .accounts
        .into_iter()
        .find(|row| row.account.id == "grant-go")
        .unwrap();
    assert!(rotated.allowed_endpoint_ids.is_empty());
    assert_eq!(
        rotated.credential_id,
        credential_id_for_legacy_account("grant-go").as_str()
    );
    assert!(rotated.credential_version >= 2);
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
    assert_eq!(schema_version_on(&db.conn).unwrap(), 46);
    assert!(!table_has_column(&db.conn, "providers", "onboarding_draft").unwrap());
    drop(db);

    let db = open_with_host_cipher(dir.clone()).unwrap();
    assert_eq!(schema_version_on(&db.conn).unwrap(), CURRENT_SCHEMA_VERSION);
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
    assert_eq!(schema_version_on(&db.conn).unwrap(), CURRENT_SCHEMA_VERSION);
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
    assert_eq!(schema_version_on(&db.conn).unwrap(), 48);
    assert!(!table_exists(&db.conn, "unpublished_public_models").unwrap());
    drop(db);

    let db = open_with_host_cipher(dir.clone()).unwrap();
    assert_eq!(schema_version_on(&db.conn).unwrap(), CURRENT_SCHEMA_VERSION);
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
    assert_eq!(schema_version_on(&db.conn).unwrap(), CURRENT_SCHEMA_VERSION);
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
    assert_eq!(schema_version_on(&db.conn).unwrap(), 49);
    for table in shadow_tables {
        assert!(!table_exists(&db.conn, table).unwrap(), "{table}");
    }
    drop(db);

    let db = open_with_host_cipher(dir.clone()).unwrap();
    assert_eq!(schema_version_on(&db.conn).unwrap(), CURRENT_SCHEMA_VERSION);
    for table in shadow_tables {
        assert!(table_exists(&db.conn, table).unwrap(), "{table}");
    }
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

fn accounts_table_present(conn: &Connection) -> bool {
    table_exists(conn, "accounts").unwrap()
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
    assert_eq!(schema_version_on(&db.conn).unwrap(), 51);
    assert!(accounts_table_present(&db.conn));
    drop(db);

    let db = open_with_host_cipher(dir.clone()).unwrap();
    assert_eq!(schema_version_on(&db.conn).unwrap(), CURRENT_SCHEMA_VERSION);
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
        schema_version_on(&db.conn).unwrap(),
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
    assert_eq!(schema_version_on(&db.conn).unwrap(), 52);
    assert!(table_exists(&db.conn, "account_custom_configs").unwrap());
    drop(db);

    let db = Database::open(dir.clone()).unwrap();
    assert_eq!(schema_version_on(&db.conn).unwrap(), CURRENT_SCHEMA_VERSION);
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

fn leftover_custom_account(
    db: &Database,
    id: &str,
    key_cipher: &str,
    public_model: &str,
    upstream_model: &str,
) {
    let mut custom = account(id);
    custom.provider_id = CUSTOM_PROVIDER_ID.to_string();
    custom.enabled = false;
    custom.credential_kind = CredentialKind::ApiKey;
    custom.quota_scope = QuotaScope::Key;
    custom.key_cipher = key_cipher.into();
    db.create_account_with_contract(
        &custom,
        Some(&AccountCustomConfigInput {
            endpoint_url: "https://old.example/v1/chat/completions".into(),
            upstream_protocol: UpstreamProtocolKind::ChatCompletions,
        }),
        &[AccountModelCapabilityInput {
            public_model: public_model.into(),
            upstream_model: upstream_model.into(),
            protocol: UpstreamProtocolKind::ChatCompletions,
            source: Some("manual".into()),
        }],
    )
    .unwrap();
}

fn insert_leftover_capability(
    db: &Database,
    account_id: &str,
    public_model: &str,
    upstream_model: &str,
) {
    db.conn
        .execute(
            "INSERT INTO account_model_capabilities (
                account_id, model_id, upstream_model, protocol, source
             ) VALUES (?1, ?2, ?3, 'chat_completions', 'manual')",
            rusqlite::params![account_id, public_model, upstream_model],
        )
        .unwrap();
}

fn capability_pairs(db: &Database, account_id: &str) -> Vec<(String, String)> {
    // These scope tests compare model identities; v59 materializes the
    // platform passthrough protocols as several rows for each same identity.
    let mut pairs: Vec<_> = db
        .list_account_model_capabilities(account_id)
        .unwrap()
        .into_iter()
        .map(|row| (row.public_model, row.upstream_model))
        .collect();
    pairs.dedup();
    pairs
}

fn parent_catalog_pairs(db: &Database, parent_id: &str) -> Vec<(String, String)> {
    use ocg_domain::destination::destination_id_for_platform_account;
    let dest_id = destination_id_for_platform_account(parent_id);
    let mut stmt = db
        .conn
        .prepare(
            "SELECT public_model, upstream_model FROM destination_models
             WHERE destination_id = ?1 ORDER BY rowid ASC",
        )
        .unwrap();
    stmt.query_map([dest_id], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap()
}

fn stored_model_scope(db: &Database, account_id: &str) -> ocg_domain::credential::ModelScope {
    db.list_identity_model()
        .unwrap()
        .accounts
        .into_iter()
        .find(|row| row.account.id == account_id)
        .unwrap()
        .binding_model_scope
}

fn assert_key_cannot_serve(db: &Database, account_id: &str, model: &str) {
    use ocg_domain::credential::model_scope_allows;
    let runtime = db
        .list_custom_account_runtimes()
        .unwrap()
        .into_iter()
        .find(|runtime| runtime.account_id == account_id)
        .expect("custom runtime");
    assert!(
        runtime.capability_matching_public(model).is_none(),
        "{account_id} still declares {model}"
    );
    assert!(
        !model_scope_allows(&stored_model_scope(db, account_id), model),
        "{account_id} scope still admits {model}"
    );
}

fn custom_capability(public_model: &str, upstream_model: &str) -> AccountModelCapabilityInput {
    AccountModelCapabilityInput {
        public_model: public_model.into(),
        upstream_model: upstream_model.into(),
        protocol: UpstreamProtocolKind::ChatCompletions,
        source: Some("manual".into()),
    }
}

fn seed_linked_platform_keys(db: &Database, parent_id: &str, keys: &[(&str, &str, &str)]) {
    use crate::platform::{PlatformGroup, PlatformKind};
    for (account_id, public_model, upstream_model) in keys {
        leftover_custom_account(
            db,
            account_id,
            &format!("cipher-{account_id}"),
            public_model,
            upstream_model,
        );
    }
    db.create_platform_account(
        parent_id,
        PlatformKind::NewApi,
        "Shared Parent",
        "https://platform.example/v1",
        Some("mgmt-cipher"),
    )
    .unwrap();
    for (account_id, _, _) in keys {
        db.link_platform_account(account_id, parent_id, &PlatformGroup::default())
            .unwrap();
    }
}

fn rewind_linked_keys_to_v52_leftover_capabilities(
    db: &Database,
    parent_id: &str,
    leftovers: &[(&str, &str, &str)],
) {
    use ocg_domain::destination::destination_id_for_platform_account;
    let dest_id = destination_id_for_platform_account(parent_id);
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
    for (account_id, public_model, upstream_model) in leftovers {
        insert_leftover_capability(db, account_id, public_model, upstream_model);
    }
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
}

fn force_binding_scope_all(db: &Database, account_id: &str) {
    db.conn
        .execute(
            r#"UPDATE credentials SET scope_json = '{"kind":"all"}' WHERE legacy_account_id = ?1"#,
            [account_id],
        )
        .unwrap();
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
    assert_eq!(schema_version_on(&db.conn).unwrap(), CURRENT_SCHEMA_VERSION);
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
    assert_eq!(schema_version_on(&db.conn).unwrap(), CURRENT_SCHEMA_VERSION);
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
fn refresh_one_linked_key_does_not_replace_sibling_catalog() {
    let dir = temp_data_dir("refresh-one-key");
    let db = Database::open(dir.clone()).unwrap();
    seed_linked_platform_keys(
        &db,
        "parent-refresh",
        &[("key-a", "model-a", "up-a"), ("key-b", "model-b", "up-b")],
    );
    db.replace_account_model_capabilities("key-a", &[custom_capability("model-a", "up-a")])
        .unwrap();
    assert_eq!(
        parent_catalog_pairs(&db, "parent-refresh"),
        vec![
            ("model-a".into(), "up-a".into()),
            ("model-b".into(), "up-b".into()),
        ]
    );
    assert_eq!(
        capability_pairs(&db, "key-b"),
        vec![("model-b".into(), "up-b".into())]
    );
    assert_key_cannot_serve(&db, "key-a", "model-b");
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn fetch_all_linked_keys_either_order_keeps_shared_catalog_and_scopes() {
    use ocg_domain::credential::ModelScope;

    fn apply_fetch_all(db: &Database, first: &str, second: &str) {
        let first_cap = if first == "key-a" {
            custom_capability("model-a", "up-a")
        } else {
            custom_capability("model-b", "up-b")
        };
        let second_cap = if second == "key-a" {
            custom_capability("model-a", "up-a")
        } else {
            custom_capability("model-b", "up-b")
        };
        db.replace_account_model_capabilities(first, &[first_cap])
            .unwrap();
        db.replace_account_model_capabilities(second, &[second_cap])
            .unwrap();
    }

    fn assert_shared(db: &Database) {
        let mut catalog = parent_catalog_pairs(db, "parent-fetch-all");
        catalog.sort();
        assert_eq!(
            catalog,
            vec![
                ("model-a".into(), "up-a".into()),
                ("model-b".into(), "up-b".into()),
            ]
        );
        assert_eq!(
            stored_model_scope(db, "key-a"),
            ModelScope::Only {
                models: vec!["model-a".into()]
            }
        );
        assert_eq!(
            stored_model_scope(db, "key-b"),
            ModelScope::Only {
                models: vec!["model-b".into()]
            }
        );
        assert_eq!(
            capability_pairs(db, "key-a"),
            vec![("model-a".into(), "up-a".into())]
        );
        assert_eq!(
            capability_pairs(db, "key-b"),
            vec![("model-b".into(), "up-b".into())]
        );
    }

    for (label, first, second) in [
        ("a-then-b", "key-a", "key-b"),
        ("b-then-a", "key-b", "key-a"),
    ] {
        let dir = temp_data_dir(&format!("fetch-all-{label}"));
        let db = Database::open(dir.clone()).unwrap();
        seed_linked_platform_keys(
            &db,
            "parent-fetch-all",
            &[("key-a", "model-a", "up-a"), ("key-b", "model-b", "up-b")],
        );
        apply_fetch_all(&db, first, second);
        assert_shared(&db);
        drop(db);
        fs::remove_dir_all(dir).unwrap();
    }
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
    assert_eq!(schema_version_on(&db.conn).unwrap(), 53);
    assert!(table_exists(&db.conn, "platform_accounts").unwrap());
    drop(db);

    let db = Database::open(dir.clone()).unwrap();
    assert_eq!(schema_version_on(&db.conn).unwrap(), CURRENT_SCHEMA_VERSION);
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
    assert_eq!(schema_version_on(&db.conn).unwrap(), 54);
    assert!(table_exists(&db.conn, "cpa_integration").unwrap());
    drop(db);

    let db = open_with_host_cipher(dir.clone()).unwrap();
    assert_eq!(schema_version_on(&db.conn).unwrap(), CURRENT_SCHEMA_VERSION);
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
    assert_eq!(schema_version_on(&db.conn).unwrap(), 55);
    assert!(table_exists(&db.conn, "providers").unwrap());
    drop(db);

    let db = open_with_host_cipher(dir.clone()).unwrap();
    assert_eq!(schema_version_on(&db.conn).unwrap(), CURRENT_SCHEMA_VERSION);
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

fn rewind_identity_satellites_to_v56(conn: &Connection) {
    crate::db::identity_v57::rewind_identity_satellites_to_v56(conn).unwrap();
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
    assert_eq!(schema_version_on(&db.conn).unwrap(), 56);
    assert!(table_exists(&db.conn, "upstream_identities").unwrap());
    assert!(table_exists(&db.conn, "quota_pools").unwrap());
    drop(db);

    let db = open_with_host_cipher(dir.clone()).unwrap();
    assert_eq!(schema_version_on(&db.conn).unwrap(), CURRENT_SCHEMA_VERSION);
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
fn dynamic_provider_crud_and_onboarding_survive_reopen_without_leftover_tables() {
    let dir = temp_data_dir("v56-crud-reopen");
    let db = open_with_host_cipher(dir.clone()).unwrap();
    let provider_id = uuid::Uuid::new_v4().to_string();
    let draft_id = uuid::Uuid::new_v4().to_string();
    let runtime = onboarding_runtime(&provider_id, "Persist Lab");
    let mut first = account("persist-lab");
    first.provider_id = provider_id.clone();
    first.key_cipher = fixture_account_key_cipher();
    db.create_dynamic_provider(&runtime, &first).unwrap();
    let mut changed = runtime.clone();
    changed.name = "Persist Lab Updated".into();
    changed.endpoint_url = "https://persist.example/v1".into();
    db.replace_dynamic_provider(&changed, false, false, None)
        .unwrap();
    db.commit_onboarding_new(
        &onboarding_runtime(&draft_id, "Draft Lab"),
        None,
        true,
        &onboarding_operation(
            &uuid::Uuid::new_v4().to_string(),
            "draft-reopen",
            r#"{"connectionId":"c","credentialId":null,"targetIds":[]}"#,
        ),
    )
    .unwrap();
    db.commit_onboarding_resume(
        &onboarding_runtime(&draft_id, "Draft Lab Done"),
        false,
        None,
        None,
        None,
        None,
        None,
        &onboarding_operation(
            &uuid::Uuid::new_v4().to_string(),
            "draft-complete",
            r#"{"connectionId":"c","credentialId":null,"targetIds":[]}"#,
        ),
    )
    .unwrap();
    drop(db);

    let db = open_with_host_cipher(dir.clone()).unwrap();
    assert_leftover_dynamic_provider_storage_absent(&db.conn);
    let loaded = db
        .get_dynamic_provider(&provider_id)
        .unwrap()
        .expect("reopened");
    assert_eq!(loaded.name, "Persist Lab Updated");
    assert_eq!(loaded.endpoint_url, "https://persist.example/v1");
    assert_eq!(db.count_accounts_for_provider(&provider_id).unwrap(), 1);
    let completed = db.get_dynamic_provider(&draft_id).unwrap().expect("draft");
    assert_eq!(completed.name, "Draft Lab Done");
    assert_eq!(
        db.provider_is_onboarding_draft(&draft_id).unwrap(),
        Some(false)
    );
    db.delete_dynamic_provider(&draft_id).unwrap();
    drop(db);

    let db = open_with_host_cipher(dir.clone()).unwrap();
    assert!(db.get_dynamic_provider(&draft_id).unwrap().is_none());
    assert!(db.get_dynamic_provider(&provider_id).unwrap().is_some());
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn platform_parent_link_refresh_survives_reopen_without_leftover_tables() {
    use crate::platform::{PlatformGroup, PlatformKind, PlatformSnapshot};

    let dir = temp_data_dir("v54-platform-reopen");
    let db = Database::open(dir.clone()).unwrap();
    assert!(!table_exists(&db.conn, "platform_accounts").unwrap());
    let mut custom = account("reopen-linked");
    custom.provider_id = CUSTOM_PROVIDER_ID.to_string();
    custom.credential_kind = CredentialKind::ApiKey;
    custom.quota_scope = QuotaScope::Key;
    custom.key_cipher = "reopen-key".into();
    db.create_account_with_contract(
        &custom,
        Some(&AccountCustomConfigInput {
            endpoint_url: "https://old.example/v1/chat/completions".into(),
            upstream_protocol: UpstreamProtocolKind::ChatCompletions,
        }),
        &[AccountModelCapabilityInput {
            public_model: "reopen-model".into(),
            upstream_model: "reopen-model".into(),
            protocol: UpstreamProtocolKind::ChatCompletions,
            source: None,
        }],
    )
    .unwrap();
    db.create_platform_account(
        "reopen-parent",
        PlatformKind::Sub2api,
        "Reopen Parent",
        "https://sub.example",
        Some("reopen-mgmt"),
    )
    .unwrap();
    db.link_platform_account("reopen-linked", "reopen-parent", &PlatformGroup::default())
        .unwrap();
    let token = db.platform_refresh_token("reopen-parent", None).unwrap();
    assert!(
        db.save_platform_refresh(
            "reopen-parent",
            None,
            &token,
            &PlatformSnapshot {
                observed_at: 9,
                ..PlatformSnapshot::default()
            },
        )
        .unwrap()
    );
    drop(db);

    let db = Database::open(dir.clone()).unwrap();
    assert!(!table_exists(&db.conn, "platform_accounts").unwrap());
    assert!(!table_exists(&db.conn, "platform_links").unwrap());
    let parent = db.platform_account("reopen-parent").unwrap().unwrap();
    assert_eq!(parent.kind, PlatformKind::Sub2api);
    assert_eq!(parent.base_url, "https://sub.example");
    assert_eq!(parent.name, "Reopen Parent");
    assert!(parent.has_user_credential);
    assert!(parent.snapshot.is_some());
    assert_eq!(
        db.platform_credential_cipher("reopen-parent")
            .unwrap()
            .as_deref(),
        Some("reopen-mgmt")
    );
    let links = db.list_platform_links().unwrap();
    assert_eq!(links.len(), 1);
    assert_eq!(links[0].account_id, "reopen-linked");
    db.unlink_platform_account("reopen-linked").unwrap();
    assert!(db.list_platform_links().unwrap().is_empty());
    db.delete_platform_account("reopen-parent").unwrap();
    assert!(db.platform_account("reopen-parent").unwrap().is_none());
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn custom_config_and_capabilities_round_trip_without_leftover_tables() {
    let dir = temp_data_dir("v53-custom-roundtrip");
    let db = Database::open(dir.clone()).unwrap();
    assert!(!table_exists(&db.conn, "account_custom_configs").unwrap());
    let mut custom = account("custom-roundtrip");
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
            public_model: "org/one".into(),
            upstream_model: "org/one-up".into(),
            protocol: UpstreamProtocolKind::ChatCompletions,
            source: Some("manual".into()),
        }],
    )
    .unwrap();
    db.upsert_account_custom_config(
        "custom-roundtrip",
        &AccountCustomConfigInput {
            endpoint_url: "https://api.example.net/v1/messages".into(),
            upstream_protocol: UpstreamProtocolKind::Messages,
        },
    )
    .unwrap();
    db.replace_account_model_capabilities(
        "custom-roundtrip",
        &[AccountModelCapabilityInput {
            public_model: "org/two".into(),
            upstream_model: "org/two-up".into(),
            protocol: UpstreamProtocolKind::Messages,
            source: Some("manual".into()),
        }],
    )
    .unwrap();
    let live_config = db
        .account_custom_config("custom-roundtrip")
        .unwrap()
        .unwrap();
    let live_caps = db
        .list_account_model_capabilities("custom-roundtrip")
        .unwrap();
    assert_eq!(
        live_config.endpoint_url,
        "https://api.example.net/v1/messages"
    );
    assert_eq!(
        live_config.upstream_protocol,
        UpstreamProtocolKind::Messages
    );
    assert_eq!(live_caps.len(), 1);
    assert_eq!(live_caps[0].public_model, "org/two");
    assert_eq!(live_caps[0].upstream_model, "org/two-up");
    drop(db);

    let db = Database::open(dir.clone()).unwrap();
    assert!(!table_exists(&db.conn, "account_custom_configs").unwrap());
    assert!(!table_exists(&db.conn, "account_model_capabilities").unwrap());
    let reopened = db
        .account_custom_config("custom-roundtrip")
        .unwrap()
        .unwrap();
    let reopened_caps = db
        .list_account_model_capabilities("custom-roundtrip")
        .unwrap();
    assert_eq!(reopened.endpoint_url, live_config.endpoint_url);
    assert_eq!(reopened.upstream_protocol, live_config.upstream_protocol);
    assert_eq!(reopened_caps.len(), 1);
    assert_eq!(reopened_caps[0].public_model, "org/two");
    assert_eq!(reopened_caps[0].upstream_model, "org/two-up");
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn mutation_then_reopen_keeps_reconstructed_account_and_credential_secrets() {
    let dir = temp_data_dir("v52-mutate-reopen");
    let db = open_with_host_cipher(dir.clone()).unwrap();
    let mut keyed = account("mutate-go");
    keyed.key_cipher = fixture_account_key_cipher();
    keyed.notes = Some("before".into());
    db.create_account(&keyed).unwrap();
    db.update_account(
        "mutate-go",
        &AccountUpdate {
            name: Some("Mutated".into()),
            username: None,
            password: None,
            key: None,
            enabled: Some(false),
            referral_code: None,
            purchase_date: None,
            notes: Some("after".into()),
        },
        Some(&fixture_account_key_cipher()),
        None,
    )
    .unwrap();
    let live = db.get_account("mutate-go").unwrap().unwrap();
    drop(db);

    let db = open_with_host_cipher(dir.clone()).unwrap();
    assert!(!accounts_table_present(&db.conn));
    let reopened = db.get_account("mutate-go").unwrap().unwrap();
    assert_eq!(reopened.enabled, live.enabled);
    assert_eq!(reopened.name, live.name);
    assert_eq!(reopened.notes, live.notes);
    assert_eq!(reopened.key_cipher, live.key_cipher);
    let sql_cipher: String = db
        .conn
        .query_row(
            "SELECT key_cipher FROM credentials WHERE legacy_account_id = ?1",
            ["mutate-go"],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(sql_cipher, reopened.key_cipher);
    assert_fixture_account_cipher(&sql_cipher);
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn list_dynamic_providers_excludes_drafts_and_control_plane_includes_them() {
    let dir = temp_data_dir("draft-list-boundary");
    let db = open_with_host_cipher(dir.clone()).unwrap();
    let provider_id = uuid::Uuid::new_v4().to_string();
    let runtime = onboarding_runtime(&provider_id, "DraftBoundary");
    db.commit_onboarding_new(
        &runtime,
        None,
        true,
        &onboarding_operation(
            &uuid::Uuid::new_v4().to_string(),
            "draft-digest",
            r#"{"connectionId":"c","credentialId":null,"targetIds":[]}"#,
        ),
    )
    .unwrap();
    assert!(
        db.list_dynamic_providers()
            .unwrap()
            .iter()
            .all(|row| row.id != provider_id)
    );
    assert!(
        db.list_control_plane_dynamic_providers()
            .unwrap()
            .iter()
            .any(|row| row.id == provider_id)
    );
    assert_eq!(
        db.provider_is_onboarding_draft(&provider_id).unwrap(),
        Some(true)
    );
    assert!(
        db.onboarding_draft_provider_ids()
            .unwrap()
            .contains(&provider_id)
    );
    let loaded = db.get_dynamic_provider(&provider_id).unwrap().unwrap();
    assert_eq!(loaded.id, provider_id);
    db.replace_dynamic_provider(&runtime, false, false, None)
        .unwrap();
    assert_eq!(
        db.provider_is_onboarding_draft(&provider_id).unwrap(),
        Some(true),
        "ordinary replace must preserve the draft flag"
    );
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn second_identity_credential_is_independent_until_explicit_share() {
    use crate::models::{UpstreamChannel, UsageWindowKind, local_today};
    use crate::provider::ConnectionVerificationStatus;

    let dir = temp_data_dir("independent-second-key");
    let db = open_with_host_cipher(dir.clone()).unwrap();
    let mut first = account("indep-a");
    first.key_cipher = fixture_account_key_cipher();
    db.create_account(&first).unwrap();
    let identity_id: String = db
        .conn
        .query_row(
            "SELECT identity_id FROM credentials WHERE legacy_account_id = 'indep-a'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let mut second = account("indep-b");
    second.key_cipher = fixture_account_key_cipher();
    db.create_account_for_identity(
        &identity_id,
        &second,
        &local_today(),
        ConnectionVerificationStatus::NotRequired,
        crate::db::identity::QuotaSharingJoin::Independent,
        None,
    )
    .unwrap();
    let members = db.shared_pool_account_ids("indep-a").unwrap();
    assert_eq!(members, vec!["indep-a".to_string()]);
    let until = Utc::now() + chrono::Duration::hours(2);
    db.set_account_rate_limit(
        "indep-a",
        until,
        "429 exhausted",
        Some(UsageWindowKind::FiveHours),
    )
    .unwrap();
    let sibling = db.get_account("indep-b").unwrap().expect("sibling");
    assert!(sibling.cooldown_5h_until.is_none());
    assert!(!sibling.is_cooling_for(UpstreamChannel::Go, Utc::now()));
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn shared_pool_fanout_preserves_maxima_and_clear_still_propagates() {
    use crate::models::{UsageWindowKind, local_today};
    use crate::provider::ConnectionVerificationStatus;

    let dir = temp_data_dir("shared-pool-max-fanout");
    let db = open_with_host_cipher(dir.clone()).unwrap();
    let mut first = account("max-a");
    first.key_cipher = fixture_account_key_cipher();
    db.create_account(&first).unwrap();
    let identity_id: String = db
        .conn
        .query_row(
            "SELECT identity_id FROM credentials WHERE legacy_account_id = 'max-a'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let source_credential: String = db
        .conn
        .query_row(
            "SELECT id FROM credentials WHERE legacy_account_id = 'max-a'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let mut second = account("max-b");
    second.key_cipher = fixture_account_key_cipher();
    db.create_account_for_identity(
        &identity_id,
        &second,
        &local_today(),
        ConnectionVerificationStatus::NotRequired,
        crate::db::identity::QuotaSharingJoin::Shared {
            source_credential_id: source_credential,
        },
        None,
    )
    .unwrap();

    let two_hours = Utc::now() + chrono::Duration::hours(2);
    db.set_account_rate_limit(
        "max-a",
        two_hours,
        "429 two hours",
        Some(UsageWindowKind::FiveHours),
    )
    .unwrap();
    let one_hour = Utc::now() + chrono::Duration::hours(1);
    db.set_account_rate_limit(
        "max-b",
        one_hour,
        "429 one hour",
        Some(UsageWindowKind::FiveHours),
    )
    .unwrap();
    let stored_a = db.get_account("max-a").unwrap().unwrap();
    let stored_b = db.get_account("max-b").unwrap().unwrap();
    assert_eq!(stored_a.cooldown_5h_until, Some(two_hours));
    assert_eq!(stored_b.cooldown_5h_until, Some(two_hours));

    db.clear_account_cooldown("max-a").unwrap();
    let stored_a = db.get_account("max-a").unwrap().unwrap();
    let stored_b = db.get_account("max-b").unwrap().unwrap();
    assert!(stored_a.cooldown_5h_until.is_none());
    assert!(stored_b.cooldown_5h_until.is_none());
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn rotate_increments_version_and_auth_state_version_together() {
    let dir = temp_data_dir("rotate-versions");
    let db = open_with_host_cipher(dir.clone()).unwrap();
    let mut keyed = account("rotate-go");
    keyed.key_cipher = fixture_account_key_cipher();
    keyed.auth_error = Some("stale-auth".into());
    keyed.last_error = Some("stale-limit".into());
    db.create_account(&keyed).unwrap();
    let before: (i64, i64) = db
        .conn
        .query_row(
            "SELECT COALESCE(credential_version, 1), COALESCE(auth_state_version, 1)
             FROM credentials WHERE legacy_account_id = 'rotate-go'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(before, (1, 1));

    let rotated = db
        .rotate_account_credential("rotate-go", "replacement-cipher")
        .unwrap();
    assert_eq!(rotated.version, 2);
    assert_eq!(rotated.auth_state_version, 2);
    let after: (i64, i64, Option<String>, Option<String>, String) = db
        .conn
        .query_row(
            "SELECT COALESCE(credential_version, 1), COALESCE(auth_state_version, 1),
                    auth_error, last_error, key_cipher
             FROM credentials
             WHERE legacy_account_id = 'rotate-go'",
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
    assert_eq!((after.0, after.1), (2, 2));
    assert!(after.2.is_none());
    assert!(after.3.is_none());
    assert_eq!(after.4, "replacement-cipher");

    db.conn
        .execute(
            "UPDATE credentials SET credential_version = NULL, auth_state_version = NULL
             WHERE legacy_account_id = 'rotate-go'",
            [],
        )
        .unwrap();
    let repaired = db
        .rotate_account_credential("rotate-go", "repaired-cipher")
        .unwrap();
    assert_eq!(repaired.version, 2);
    assert_eq!(repaired.auth_state_version, 2);
    assert_eq!(repaired.credential_id, rotated.credential_id);
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

fn probe_row(scope: ContractScope, model_id: &str, now: DateTime<Utc>) -> PersistedModelProtocol {
    PersistedModelProtocol {
        scope,
        model_id: model_id.into(),
        protocol: UpstreamProtocolKind::ChatCompletions,
        source: ContractEvidenceSource::ProbeConfirmed,
        verified_at: Some(now),
        observed_at: Some(now),
        last_probe_result: Some(ProbeResultKind::Success),
        last_probe_at: Some(now),
        last_probe_error: None,
    }
}

#[test]
fn o02_rotate_invalidates_custom_probe_evidence_and_keeps_builtin_catalog() {
    let dir = temp_data_dir("o02-rotate-probe");
    let db = open_with_host_cipher(dir.clone()).unwrap();
    let mut custom = account("o02-custom");
    custom.provider_id = CUSTOM_PROVIDER_ID.into();
    custom.key_cipher = fixture_account_key_cipher();
    db.create_account_with_contract(
        &custom,
        Some(&AccountCustomConfigInput {
            endpoint_url: "https://o02.example/v1/chat/completions".into(),
            upstream_protocol: UpstreamProtocolKind::ChatCompletions,
        }),
        &[AccountModelCapabilityInput {
            public_model: "lab-model".into(),
            upstream_model: "lab-model".into(),
            protocol: UpstreamProtocolKind::ChatCompletions,
            source: None,
        }],
    )
    .unwrap();
    let mut go = account("o02-go");
    go.key_cipher = fixture_account_key_cipher();
    db.create_account(&go).unwrap();

    let now = Utc::now();
    let custom_scope = ContractScope::custom_endpoint("o02-custom");
    let go_scope = ContractScope::provider(OPENCODE_PROVIDER_ID);
    db.upsert_model_protocol(&probe_row(custom_scope.clone(), "lab-model", now))
        .unwrap();
    db.upsert_model_protocol(&probe_row(go_scope.clone(), "glm-5.2", now))
        .unwrap();

    db.rotate_account_credential("o02-custom", "replacement-custom")
        .unwrap();
    db.rotate_account_credential("o02-go", "replacement-go")
        .unwrap();

    assert!(
        db.load_model_protocol(
            &custom_scope,
            "lab-model",
            UpstreamProtocolKind::ChatCompletions
        )
        .unwrap()
        .is_none(),
        "rotated Custom Key must drop probe evidence"
    );
    assert!(
        db.load_model_protocol(&go_scope, "glm-5.2", UpstreamProtocolKind::ChatCompletions)
            .unwrap()
            .is_some(),
        "builtin catalog probe rows must survive a Go Key rotate"
    );
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn o02_endpoint_change_invalidates_custom_and_dynamic_probe_evidence() {
    let dir = temp_data_dir("o02-endpoint-probe");
    let db = open_with_host_cipher(dir.clone()).unwrap();
    let mut custom = account("o02-endpoint-custom");
    custom.provider_id = CUSTOM_PROVIDER_ID.into();
    custom.key_cipher = fixture_account_key_cipher();
    db.create_account_with_contract(
        &custom,
        Some(&AccountCustomConfigInput {
            endpoint_url: "https://old-o02.example/v1/chat/completions".into(),
            upstream_protocol: UpstreamProtocolKind::ChatCompletions,
        }),
        &[AccountModelCapabilityInput {
            public_model: "lab-model".into(),
            upstream_model: "lab-model".into(),
            protocol: UpstreamProtocolKind::ChatCompletions,
            source: None,
        }],
    )
    .unwrap();
    let now = Utc::now();
    let provider_id = "o02-dyn";
    let runtime = crate::dynamic::DynamicProviderRuntime {
        preset_id: None,
        id: provider_id.into(),
        name: "O02 Dyn".into(),
        endpoint_url: "https://dyn-old.example/v1/chat/completions".into(),
        upstream_protocol: UpstreamProtocolKind::ChatCompletions,
        auth_kind: ocg_domain::dynamic::DynamicAuthKind::Bearer,
        mappings: vec![ocg_domain::dynamic::DynamicModelMapping {
            public_model: "lab".into(),
            upstream_model: "vendor/lab".into(),
            upstream_override: None,
        }],
        created_at: now,
        updated_at: now,
        origin: ocg_domain::provider::ProviderOrigin::Custom,
        offering: "api".into(),
    };
    let mut dynamic = account("o02-dyn-key");
    dynamic.provider_id = provider_id.into();
    dynamic.key_cipher = fixture_account_key_cipher();
    db.create_dynamic_provider(&runtime, &dynamic).unwrap();

    let custom_scope = ContractScope::custom_endpoint("o02-endpoint-custom");
    let dyn_scope = ContractScope::custom_endpoint("o02-dyn-key");
    db.upsert_model_protocol(&probe_row(custom_scope.clone(), "lab-model", now))
        .unwrap();
    db.upsert_model_protocol(&probe_row(dyn_scope.clone(), "lab", now))
        .unwrap();

    db.upsert_account_custom_config(
        "o02-endpoint-custom",
        &AccountCustomConfigInput {
            endpoint_url: "https://new-o02.example/v1/chat/completions".into(),
            upstream_protocol: UpstreamProtocolKind::ChatCompletions,
        },
    )
    .unwrap();
    let mut moved = runtime.clone();
    moved.endpoint_url = "https://dyn-new.example/v1/chat/completions".into();
    moved.updated_at = Utc::now();
    db.replace_dynamic_provider(&moved, true, false, None)
        .unwrap();

    assert!(
        db.load_model_protocol(
            &custom_scope,
            "lab-model",
            UpstreamProtocolKind::ChatCompletions
        )
        .unwrap()
        .is_none()
    );
    assert!(
        db.load_model_protocol(&dyn_scope, "lab", UpstreamProtocolKind::ChatCompletions)
            .unwrap()
            .is_none()
    );
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
    let backup_version = schema_version_on(&verified).unwrap();
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

fn pre_v42_backup_paths(dir: &Path) -> Vec<PathBuf> {
    backup_paths_with_prefix(dir, PRE_V42_BACKUP_FILE_PREFIX)
}

fn pre_v48_backup_paths(dir: &Path) -> Vec<PathBuf> {
    backup_paths_with_prefix(dir, PRE_V48_BACKUP_FILE_PREFIX)
}

fn pre_v58_backup_paths(dir: &Path) -> Vec<PathBuf> {
    backup_paths_with_prefix(dir, PRE_V58_BACKUP_FILE_PREFIX)
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
        schema_version_on(&db.conn).unwrap(),
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
    assert_eq!(schema_version_on(&backup).unwrap(), 57);
    drop(backup);
    let hash_path = backups[0].with_file_name(format!(
        "{}.sha256",
        backups[0].file_name().unwrap().to_str().unwrap()
    ));
    assert!(hash_path.exists());
    fs::remove_dir_all(dir).unwrap();
}

fn assert_leftover_dynamic_provider_storage_absent(conn: &Connection) {
    assert!(!table_exists(conn, "providers").unwrap());
    assert!(!table_exists(conn, "provider_models").unwrap());
}

fn assert_leftover_identity_tables_absent(conn: &Connection) {
    for table in crate::db::identity_v57::IDENTITY_LEFTOVER_TABLES {
        assert!(!table_exists(conn, table).unwrap(), "{table}");
    }
}

fn leftover_providers_ddl() -> &'static str {
    "CREATE TABLE providers (
            id TEXT PRIMARY KEY,
            origin TEXT NOT NULL CHECK(origin IN ('builtin','preset','custom')),
            adapter_kind TEXT NOT NULL,
            name TEXT NOT NULL,
            endpoint_url TEXT,
            upstream_protocol TEXT,
            auth_kind TEXT,
            preset_id TEXT,
            offering TEXT NOT NULL DEFAULT 'api' CHECK(offering IN ('plan','api')),
            display_family TEXT,
            endpoint_per_account INTEGER NOT NULL DEFAULT 0,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            onboarding_draft INTEGER NOT NULL DEFAULT 0
         );
         CREATE TABLE provider_models (
            provider_id TEXT NOT NULL,
            public_model TEXT NOT NULL,
            public_model_key TEXT NOT NULL,
            upstream_model TEXT NOT NULL,
            upstream_override TEXT,
            PRIMARY KEY (provider_id, public_model_key)
         );"
}

fn materialize_legacy_providers_for_rewind(conn: &Connection) {
    if table_exists(conn, "providers").unwrap() {
        return;
    }
    conn.execute_batch(leftover_providers_ddl())
        .expect("rewind leftover providers should recreate");
}

fn assert_retired_dynamic_provider_tables_absent(conn: &Connection) {
    assert!(!table_exists(conn, "dynamic_providers").unwrap());
    assert!(!table_exists(conn, "dynamic_provider_models").unwrap());
    let leftover_index: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master
             WHERE type = 'index' AND name = 'idx_dynamic_provider_models_provider'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(leftover_index, 0);
}

#[test]
fn current_schema_and_data_remain_stable_across_startup_replay() {
    let dir = temp_data_dir("current-schema-stable");
    let assert_current_shape = |db: &Database| {
        assert_eq!(schema_version_on(&db.conn).unwrap(), CURRENT_SCHEMA_VERSION);
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
    assert_eq!(schema_version_on(&db.conn).unwrap(), CURRENT_SCHEMA_VERSION);
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

fn assert_v48_inert_columns_absent(conn: &Connection) {
    assert!(!table_has_column(conn, "accounts", "free_alias_enabled").unwrap());
    for column in [
        "chat_completions_enabled",
        "responses_enabled",
        "messages_enabled",
    ] {
        assert!(
            !table_has_column(conn, "provider_contract_scopes", column).unwrap(),
            "{column}"
        );
    }
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
    assert_eq!(schema_version_on(&db.conn).unwrap(), CURRENT_SCHEMA_VERSION);
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
    assert_eq!(schema_version_on(&verified).unwrap(), V47_SCHEMA_VERSION);
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
        assert_eq!(schema_version_on(&conn).unwrap(), V47_SCHEMA_VERSION);
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
    assert_eq!(schema_version_on(&conn).unwrap(), V47_SCHEMA_VERSION);
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
    assert_eq!(schema_version_on(&conn).unwrap(), V41_SCHEMA_VERSION);
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
    assert_eq!(schema_version_on(&conn).unwrap(), 39);
    migrate_to_v40(&conn).unwrap();
    migrate_to_v40(&conn).unwrap();
    assert_eq!(schema_version_on(&conn).unwrap(), 40);
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
fn platform_link_lifecycle_and_refresh_races() {
    use crate::platform::{PlatformGroup, PlatformKind, PlatformSnapshot};
    let dir = temp_data_dir("platform-link");
    let mut db = Database::open(dir.clone()).unwrap();
    let mut key = account("platform-key");
    key.provider_id = CUSTOM_PROVIDER_ID.into();
    db.create_account_with_contract(
        &key,
        Some(&AccountCustomConfigInput {
            endpoint_url: "https://old.example/v1/chat/completions".into(),
            upstream_protocol: UpstreamProtocolKind::ChatCompletions,
        }),
        &[AccountModelCapabilityInput {
            public_model: "model-a".into(),
            upstream_model: "model-a".into(),
            protocol: UpstreamProtocolKind::ChatCompletions,
            source: None,
        }],
    )
    .unwrap();
    db.create_platform_account(
        "parent",
        PlatformKind::NewApi,
        "Parent",
        "https://new.example/v1",
        Some("obfuscated-test-credential"),
    )
    .unwrap();
    db.link_platform_account(&key.id, "parent", &PlatformGroup::default())
        .unwrap();
    assert_eq!(
        db.account_custom_config(&key.id)
            .unwrap()
            .unwrap()
            .endpoint_url,
        "https://new.example"
    );
    let linked_runtime = db
        .list_custom_account_runtimes()
        .unwrap()
        .into_iter()
        .find(|runtime| runtime.account_id == key.id)
        .expect("linked custom runtime");
    assert!(linked_runtime.protocol_passthrough);
    assert_eq!(linked_runtime.config.endpoint_url, "https://new.example");
    assert!(db.delete_platform_account("parent").is_err());
    let old = db.platform_refresh_token("parent", Some(&key.id)).unwrap();
    db.unlink_platform_account(&key.id).unwrap();
    db.link_platform_account(&key.id, "parent", &PlatformGroup::default())
        .unwrap();
    assert!(
        !db.save_platform_refresh("parent", Some(&key.id), &old, &PlatformSnapshot::default())
            .unwrap()
    );
    let old = db.platform_refresh_token("parent", Some(&key.id)).unwrap();
    db.update_platform_account("parent", "Parent", Some(None))
        .unwrap();
    assert!(
        !db.save_platform_refresh("parent", Some(&key.id), &old, &PlatformSnapshot::default())
            .unwrap()
    );
    assert!(
        !db.platform_account("parent")
            .unwrap()
            .unwrap()
            .has_user_credential
    );
    let current = db.platform_refresh_token("parent", Some(&key.id)).unwrap();
    assert!(
        db.save_platform_refresh(
            "parent",
            Some(&key.id),
            &current,
            &PlatformSnapshot::default()
        )
        .unwrap()
    );
    assert!(
        !db.save_platform_refresh(
            "parent",
            Some(&key.id),
            &current,
            &PlatformSnapshot::default()
        )
        .unwrap()
    );
    let untouched = db.platform_refresh_token("parent", Some(&key.id)).unwrap();
    platform::merge_platforms_on(&db.conn, &[], &[], &HashSet::new()).unwrap();
    assert_eq!(
        db.platform_refresh_token("parent", Some(&key.id)).unwrap(),
        untouched
    );
    assert!(db.list_platform_links().unwrap()[0].snapshot.is_some());
    db.unlink_platform_account(&key.id).unwrap();
    assert_eq!(
        db.account_custom_config(&key.id)
            .unwrap()
            .unwrap()
            .endpoint_url,
        "https://new.example"
    );
    db.link_platform_account(&key.id, "parent", &PlatformGroup::default())
        .unwrap();
    db.delete_account(&key.id).unwrap();
    assert!(db.list_platform_links().unwrap().is_empty());
    db.delete_platform_account("parent").unwrap();
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn platform_link_failure_rolls_back_endpoint() {
    use crate::platform::{PlatformGroup, PlatformKind};
    let dir = temp_data_dir("platform-atomic");
    let db = Database::open(dir.clone()).unwrap();
    assert!(
        db.create_platform_account(
            "invalid",
            PlatformKind::Sub2api,
            "Invalid",
            "https://new.example/v1/messages",
            None
        )
        .is_err()
    );
    let mut key = account("platform-key");
    key.provider_id = CUSTOM_PROVIDER_ID.into();
    db.create_account_with_contract(
        &key,
        Some(&AccountCustomConfigInput {
            endpoint_url: "https://old.example/v1/chat/completions".into(),
            upstream_protocol: UpstreamProtocolKind::ChatCompletions,
        }),
        &[AccountModelCapabilityInput {
            public_model: "model-a".into(),
            upstream_model: "model-a".into(),
            protocol: UpstreamProtocolKind::ChatCompletions,
            source: None,
        }],
    )
    .unwrap();
    db.create_platform_account(
        "parent",
        PlatformKind::Sub2api,
        "Parent",
        "https://new.example",
        None,
    )
    .unwrap();
    db.conn.execute_batch("CREATE TRIGGER reject_platform BEFORE UPDATE ON credentials WHEN NEW.group_json IS NOT NULL AND OLD.group_json IS NULL BEGIN SELECT RAISE(ABORT,'injected failure'); END;").unwrap();
    assert!(
        db.link_platform_account(&key.id, "parent", &PlatformGroup::default())
            .is_err()
    );
    assert_eq!(
        db.account_custom_config(&key.id)
            .unwrap()
            .unwrap()
            .endpoint_url,
        "https://old.example/v1/chat/completions"
    );
    assert!(db.list_platform_links().unwrap().is_empty());
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn platform_key_survives_failed_link_and_retry_links_without_a_second_key() {
    use crate::platform::{PlatformGroup, PlatformKind};
    let dir = temp_data_dir("platform-key-link-retry");
    let db = Database::open(dir.clone()).unwrap();
    let mut key = account("platform-key");
    key.provider_id = CUSTOM_PROVIDER_ID.into();
    db.create_account_with_contract(
        &key,
        Some(&AccountCustomConfigInput {
            endpoint_url: "https://old.example/v1/chat/completions".into(),
            upstream_protocol: UpstreamProtocolKind::ChatCompletions,
        }),
        &[AccountModelCapabilityInput {
            public_model: "model-a".into(),
            upstream_model: "model-a".into(),
            protocol: UpstreamProtocolKind::ChatCompletions,
            source: None,
        }],
    )
    .unwrap();
    db.create_platform_account(
        "parent",
        PlatformKind::NewApi,
        "Parent",
        "https://new.example",
        None,
    )
    .unwrap();
    db.conn
        .execute_batch(
            "CREATE TRIGGER reject_platform BEFORE UPDATE ON credentials WHEN NEW.group_json IS NOT NULL AND OLD.group_json IS NULL BEGIN SELECT RAISE(ABORT,'injected failure'); END;",
        )
        .unwrap();
    assert!(
        db.link_platform_account(&key.id, "parent", &PlatformGroup::default())
            .is_err()
    );
    let custom_ids: Vec<_> = db
        .list_accounts()
        .unwrap()
        .into_iter()
        .filter(|account| account.provider_id == CUSTOM_PROVIDER_ID)
        .map(|account| account.id)
        .collect();
    assert_eq!(custom_ids, ["platform-key".to_string()]);
    assert!(db.get_account("platform-key").unwrap().is_some());
    assert!(db.list_platform_links().unwrap().is_empty());

    db.conn
        .execute_batch("DROP TRIGGER reject_platform;")
        .unwrap();
    db.link_platform_account(&key.id, "parent", &PlatformGroup::default())
        .unwrap();
    let custom_ids: Vec<_> = db
        .list_accounts()
        .unwrap()
        .into_iter()
        .filter(|account| account.provider_id == CUSTOM_PROVIDER_ID)
        .map(|account| account.id)
        .collect();
    assert_eq!(custom_ids, ["platform-key".to_string()]);
    let links = db.list_platform_links().unwrap();
    assert_eq!(links.len(), 1);
    assert_eq!(links[0].account_id, "platform-key");
    assert_eq!(links[0].platform_account_id, "parent");
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

fn custom_platform_import_record(
    id: &str,
    public_model: &str,
    upstream_model: &str,
) -> AccountImportRecord {
    let mut custom = account(id);
    custom.provider_id = CUSTOM_PROVIDER_ID.to_string();
    custom.credential_kind = CredentialKind::ApiKey;
    custom.quota_scope = QuotaScope::Key;
    custom.key_cipher = format!("cipher-{id}");
    AccountImportRecord {
        account: custom,
        custom_config: Some(AccountCustomConfigInput {
            endpoint_url: "https://old.example/v1/chat/completions".into(),
            upstream_protocol: UpstreamProtocolKind::ChatCompletions,
        }),
        capabilities: vec![custom_capability(public_model, upstream_model)],
        verification_status: ConnectionVerificationStatus::NotRequired,
        connection_verified_at: None,
        ollama_billing_tier: None,
    }
}

fn identity_snapshot_forcing_all(
    db: &Database,
    account_ids: &[&str],
) -> crate::db::identity::IdentityImportSnapshot {
    use crate::db::identity::{IdentityImportSnapshot, ImportedAccountIdentity, ImportedIdentity};
    use ocg_domain::credential::ModelScope;
    let model = db.list_identity_model().unwrap();
    let accounts = model
        .accounts
        .iter()
        .filter(|row| account_ids.contains(&row.account.id.as_str()))
        .map(|row| ImportedAccountIdentity {
            account_id: row.account.id.clone(),
            identity_id: row.identity_id.clone(),
            credential_id: row.credential_id.clone(),
            credential_version: row.credential_version,
            auth_state_version: row.auth_state_version,
            binding_id: row.binding_id.clone(),
            binding_enabled: row.binding_enabled,
            binding_model_scope: ModelScope::All,
            allowed_endpoint_ids: row.allowed_endpoint_ids.clone(),
            allowed_origins: row.allowed_origins.clone(),
        })
        .collect::<Vec<_>>();
    let identity_ids = accounts
        .iter()
        .map(|row| row.identity_id.clone())
        .collect::<HashSet<_>>();
    let identities = model
        .identities
        .into_iter()
        .filter(|identity| identity_ids.contains(&identity.id))
        .map(|identity| ImportedIdentity {
            id: identity.id,
            label: identity.label,
            identity_confidence: identity.identity_confidence,
            authority_site: identity.authority_site,
            authority_subject: identity.authority_subject,
            enabled: identity.enabled,
            notes: identity.notes,
        })
        .collect();
    IdentityImportSnapshot {
        identities,
        accounts,
        quota_pools: Vec::new(),
    }
}

fn assert_restored_platform_catalog_and_scopes(db: &Database, parent_id: &str) {
    use crate::custom::eligible_custom_public_models;
    use ocg_domain::credential::ModelScope;
    let mut catalog = parent_catalog_pairs(db, parent_id);
    catalog.sort();
    assert_eq!(
        catalog,
        vec![
            ("model-a".into(), "up-a".into()),
            ("model-b".into(), "up-b".into()),
        ]
    );
    assert_eq!(
        stored_model_scope(db, "key-a"),
        ModelScope::Only {
            models: vec!["model-a".into()]
        }
    );
    assert_eq!(
        stored_model_scope(db, "key-b"),
        ModelScope::Only {
            models: vec!["model-b".into()]
        }
    );
    assert_eq!(
        capability_pairs(db, "key-a"),
        vec![("model-a".into(), "up-a".into())]
    );
    assert_eq!(
        capability_pairs(db, "key-b"),
        vec![("model-b".into(), "up-b".into())]
    );
    let mut public = eligible_custom_public_models(&db.list_custom_account_runtimes().unwrap());
    public.sort();
    assert_eq!(public, vec!["model-a".to_string(), "model-b".to_string()]);
    assert_key_cannot_serve(db, "key-a", "model-b");
    assert_key_cannot_serve(db, "key-b", "model-a");
}

#[test]
fn import_node_state_moves_linked_models_onto_parent_and_keeps_scopes() {
    use crate::platform::{PortablePlatformAccount, PortablePlatformLink};
    use ocg_domain::credential::ModelScope;

    let source_dir = temp_data_dir("import-models-source");
    let source = Database::open(source_dir.clone()).unwrap();
    seed_linked_platform_keys(
        &source,
        "parent-import",
        &[("key-a", "model-a", "up-a"), ("key-b", "model-b", "up-b")],
    );
    assert_eq!(
        stored_model_scope(&source, "key-a"),
        ModelScope::Only {
            models: vec!["model-a".into()]
        }
    );
    let parents = source
        .list_platform_accounts()
        .unwrap()
        .into_iter()
        .map(|parent| PortablePlatformAccount {
            id: parent.id,
            kind: parent.kind,
            name: parent.name,
            base_url: parent.base_url,
        })
        .collect::<Vec<_>>();
    let links = source
        .list_platform_links()
        .unwrap()
        .into_iter()
        .map(|link| PortablePlatformLink {
            account_id: link.account_id,
            platform_account_id: link.platform_account_id,
            group: {
                let mut group = link.group;
                group.verified = false;
                group.subscription_type = None;
                group
            },
        })
        .collect::<Vec<_>>();
    let mut record = node_import_record(
        &source,
        vec![
            custom_platform_import_record("key-a", "model-a", "up-a"),
            custom_platform_import_record("key-b", "model-b", "up-b"),
        ],
        parents,
        links,
    );
    record.identity_snapshot = Some(identity_snapshot_forcing_all(&source, &["key-a", "key-b"]));
    drop(source);
    fs::remove_dir_all(source_dir).unwrap();

    let dest_dir = temp_data_dir("import-models-dest");
    let dest = Database::open(dest_dir.clone()).unwrap();
    dest.import_node_state(&record, |_| -> Result<()> { Ok(()) })
        .unwrap();
    drop(dest);

    let dest = Database::open(dest_dir.clone()).unwrap();
    assert_restored_platform_catalog_and_scopes(&dest, "parent-import");
    dest.import_node_state(&record, |_| -> Result<()> { Ok(()) })
        .unwrap();
    assert_restored_platform_catalog_and_scopes(&dest, "parent-import");
    drop(dest);

    let dest = Database::open(dest_dir.clone()).unwrap();
    assert_restored_platform_catalog_and_scopes(&dest, "parent-import");
    drop(dest);
    fs::remove_dir_all(dest_dir).unwrap();
}

#[test]
fn import_v7_platform_catalog_merges_target_models_and_preserves_exact_scopes() {
    use crate::platform::{PortablePlatformAccount, PortablePlatformLink};
    use ocg_domain::credential::ModelScope;
    use ocg_domain::destination::CatalogModel;

    let source_dir = temp_data_dir("import-v7-platform-source");
    let source = Database::open(source_dir.clone()).unwrap();
    seed_linked_platform_keys(
        &source,
        "parent-import",
        &[("key-a", "model-a", "up-a"), ("key-b", "model-b", "up-b")],
    );
    let parents = source
        .list_platform_accounts()
        .unwrap()
        .into_iter()
        .map(|parent| PortablePlatformAccount {
            id: parent.id,
            kind: parent.kind,
            name: parent.name,
            base_url: parent.base_url,
        })
        .collect::<Vec<_>>();
    let links = source
        .list_platform_links()
        .unwrap()
        .into_iter()
        .map(|link| PortablePlatformLink {
            account_id: link.account_id,
            platform_account_id: link.platform_account_id,
            group: link.group,
        })
        .collect::<Vec<_>>();
    let mut accounts = vec![
        custom_platform_import_record("key-a", "model-a", "up-a"),
        custom_platform_import_record("key-b", "model-b", "up-b"),
    ];
    for account in &mut accounts {
        account.custom_config = None;
        account.capabilities.clear();
    }
    let mut record = node_import_record(&source, accounts, parents, links);
    let mut identity = identity_snapshot_forcing_all(&source, &["key-a", "key-b"]);
    for row in &mut identity.accounts {
        row.binding_model_scope = if row.account_id == "key-a" {
            ModelScope::Only {
                models: vec!["model-a".into()],
            }
        } else {
            ModelScope::Only { models: Vec::new() }
        };
    }
    record.identity_snapshot = Some(identity);
    record.platform_catalogs.insert(
        "parent-import".into(),
        [("model-a", "up-a"), ("model-b", "up-b")]
            .into_iter()
            .map(|(public_model, upstream_model)| CatalogModel {
                public_model: public_model.into(),
                upstream_model: upstream_model.into(),
                protocols: vec![UpstreamProtocolKind::ChatCompletions],
                preferred: Some(UpstreamProtocolKind::ChatCompletions),
                enabled: true,
                upstream_override: None,
            })
            .collect(),
    );
    drop(source);
    fs::remove_dir_all(source_dir).unwrap();

    let dest_dir = temp_data_dir("import-v7-platform-dest");
    let dest = Database::open(dest_dir.clone()).unwrap();
    seed_linked_platform_keys(
        &dest,
        "parent-import",
        &[("key-target", "model-target", "up-target")],
    );
    dest.import_node_state(&record, |_| -> Result<()> { Ok(()) })
        .unwrap();

    let mut catalog = parent_catalog_pairs(&dest, "parent-import");
    catalog.sort();
    assert_eq!(
        catalog,
        vec![
            ("model-a".into(), "up-a".into()),
            ("model-b".into(), "up-b".into()),
            ("model-target".into(), "up-target".into()),
        ]
    );
    assert_eq!(
        stored_model_scope(&dest, "key-a"),
        ModelScope::Only {
            models: vec!["model-a".into()]
        }
    );
    assert_eq!(
        stored_model_scope(&dest, "key-b"),
        ModelScope::Only { models: Vec::new() }
    );
    assert_eq!(
        stored_model_scope(&dest, "key-target"),
        ModelScope::Only {
            models: vec!["model-target".into()]
        }
    );
    assert_eq!(
        capability_pairs(&dest, "key-a"),
        vec![("model-a".into(), "up-a".into())]
    );
    assert!(capability_pairs(&dest, "key-b").is_empty());
    assert_eq!(
        capability_pairs(&dest, "key-target"),
        vec![("model-target".into(), "up-target".into())]
    );

    let mut conflicting = record.clone();
    conflicting
        .platform_catalogs
        .get_mut("parent-import")
        .unwrap()[0]
        .upstream_model = "different-upstream".into();
    let error = dest
        .import_node_state(&conflicting, |_| -> Result<()> { Ok(()) })
        .expect_err("a conflicting public-to-upstream map must roll back");
    assert!(error.to_string().contains("conflicting upstream mappings"));
    let mut after_conflict = parent_catalog_pairs(&dest, "parent-import");
    after_conflict.sort();
    assert_eq!(after_conflict, catalog);

    drop(dest);
    fs::remove_dir_all(dest_dir).unwrap();
}

fn node_import_record(
    db: &Database,
    accounts: Vec<AccountImportRecord>,
    platform_accounts: Vec<crate::platform::PortablePlatformAccount>,
    platform_links: Vec<crate::platform::PortablePlatformLink>,
) -> NodeImportRecord {
    let mut account_order: Vec<String> = db
        .list_accounts()
        .unwrap()
        .into_iter()
        .map(|account| account.id)
        .collect();
    for record in &accounts {
        if !account_order.contains(&record.account.id) {
            account_order.push(record.account.id.clone());
        }
    }
    let config = crate::models::AppConfig {
        gateway_key: "ocg-import-primary-key".into(),
        ..crate::models::AppConfig::default()
    };
    NodeImportRecord {
        platform_links_authoritative: true,
        platform_accounts,
        platform_links,
        platform_catalogs: HashMap::new(),
        destination_controls: Vec::new(),
        accounts,
        account_order,
        config_json: serde_json::to_string(&config).unwrap(),
        sub_keys: Vec::new(),
        zen_free_enabled: false,
        zen_catalog: crate::kernel::zen::ZenFreeModelCatalog::default(),
        provider_contracts: crate::provider_contracts::PersistedContracts::default(),
        dynamic_providers: Vec::new(),
        custom_destinations: Vec::new(),
        custom_credential_destinations: HashMap::new(),
        identity_snapshot: None,
        draft_provider_ids: HashSet::new(),
        platform_observer_ciphers: HashMap::new(),
        platform_snapshots: HashMap::new(),
        platform_versions: HashMap::new(),
        cpa_base_url: None,
        cpa_management_key_cipher: None,
    }
}

fn go_import_record(id: &str) -> AccountImportRecord {
    AccountImportRecord {
        account: account(id),
        custom_config: None,
        capabilities: Vec::new(),
        verification_status: ConnectionVerificationStatus::NotRequired,
        connection_verified_at: None,
        ollama_billing_tier: None,
    }
}

#[test]
fn import_v8_custom_stable_id_collision_rolls_back_whole_node() {
    let dir = temp_data_dir("import-custom-id-collision");
    let db = open_with_host_cipher(dir.clone()).unwrap();
    let now = Utc::now();
    db.create_dynamic_provider_definition(&DynamicProviderRuntime {
        preset_id: None,
        id: "collision-dynamic".into(),
        name: "Collision".into(),
        endpoint_url: "https://dynamic.example/v1".into(),
        upstream_protocol: UpstreamProtocolKind::ChatCompletions,
        auth_kind: DynamicAuthKind::Bearer,
        mappings: Vec::new(),
        created_at: now,
        updated_at: now,
        origin: ProviderOrigin::Custom,
        offering: "api".into(),
    })
    .unwrap();
    let custom_legacy_id = "00000000-0000-4000-8000-00000000c011";
    let custom_id = ocg_domain::destination::destination_id_for_custom_account(custom_legacy_id);
    let dynamic_id = ocg_domain::destination::destination_id_for_dynamic("collision-dynamic");
    db.conn
        .execute(
            "UPDATE destinations SET id = ?2 WHERE id = ?1",
            params![dynamic_id, custom_id],
        )
        .unwrap();
    let before_primary = db.primary_access_key_value().unwrap();
    let mut record = node_import_record(&db, Vec::new(), Vec::new(), Vec::new());
    record.custom_destinations.push(ImportedCustomDestination {
        id: custom_id.clone(),
        legacy_id: custom_legacy_id.into(),
        name: "Imported Custom".into(),
        endpoint_url: "https://custom.example/v1/chat/completions".into(),
        protocol: UpstreamProtocolKind::ChatCompletions,
        auth_scheme: AuthScheme::Bearer,
        models: vec![ocg_domain::dynamic::DynamicModelMapping {
            public_model: "custom-model".into(),
            upstream_model: "vendor/custom-model".into(),
            upstream_override: None,
        }],
        enabled: true,
    });
    let error = db.import_node_state(&record, |_| Ok(())).unwrap_err();
    assert!(error.to_string().contains("collides"), "{error:#}");
    assert_eq!(db.primary_access_key_value().unwrap(), before_primary);
    let row: (String, String) = db
        .conn
        .query_row(
            "SELECT legacy_kind, legacy_id FROM destinations WHERE id = ?1",
            [&custom_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(row, ("dynamic".into(), "collision-dynamic".into()));

    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn import_same_platform_id_different_site_writes_nothing() {
    use crate::platform::{PlatformKind, PortablePlatformAccount};
    let dir = temp_data_dir("import-platform-site-conflict");
    let db = Database::open(dir.clone()).unwrap();
    db.create_platform_account(
        "00000000-0000-4000-8000-0000000000aa",
        PlatformKind::NewApi,
        "Destination",
        "https://dest.example",
        None,
    )
    .unwrap();
    let before_accounts = db
        .list_accounts()
        .unwrap()
        .into_iter()
        .map(|account| account.id)
        .collect::<Vec<_>>();
    let before_primary = db.primary_access_key_value().unwrap();
    let record = node_import_record(
        &db,
        vec![go_import_record("imported-go")],
        vec![PortablePlatformAccount {
            id: "00000000-0000-4000-8000-0000000000aa".into(),
            kind: PlatformKind::NewApi,
            name: "Source".into(),
            base_url: "https://other.example".into(),
        }],
        Vec::new(),
    );
    let error = db
        .import_node_state(&record, |_| -> Result<()> {
            Err(anyhow::anyhow!("should not build a snapshot"))
        })
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("imported platform identity conflicts with immutable origin"),
        "{error}"
    );
    assert_eq!(
        db.list_accounts()
            .unwrap()
            .into_iter()
            .map(|account| account.id)
            .collect::<Vec<_>>(),
        before_accounts
    );
    assert!(db.get_account("imported-go").unwrap().is_none());
    assert_eq!(
        db.platform_account("00000000-0000-4000-8000-0000000000aa")
            .unwrap()
            .unwrap()
            .base_url,
        "https://dest.example"
    );
    assert_eq!(db.primary_access_key_value().unwrap(), before_primary);
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn import_node_state_does_not_commit_when_runtime_snapshot_fails() {
    let dir = temp_data_dir("import-snapshot-fail");
    let db = Database::open(dir.clone()).unwrap();
    let before_accounts = db
        .list_accounts()
        .unwrap()
        .into_iter()
        .map(|account| account.id)
        .collect::<Vec<_>>();
    let before_primary = db.primary_access_key_value().unwrap();
    let record = node_import_record(
        &db,
        vec![go_import_record("snapshot-go")],
        Vec::new(),
        Vec::new(),
    );
    let error = db
        .import_node_state(&record, |_| -> Result<()> {
            Err(anyhow::anyhow!("forced snapshot failure"))
        })
        .unwrap_err();
    assert!(
        error.to_string().contains("forced snapshot failure"),
        "{error}"
    );
    assert_eq!(
        db.list_accounts()
            .unwrap()
            .into_iter()
            .map(|account| account.id)
            .collect::<Vec<_>>(),
        before_accounts
    );
    assert!(db.get_account("snapshot-go").unwrap().is_none());
    assert_eq!(db.primary_access_key_value().unwrap(), before_primary);
    let satellites: i64 = db
        .conn
        .query_row(
            "SELECT COUNT(*) FROM credentials WHERE legacy_account_id = 'snapshot-go'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(satellites, 0);
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn import_v6_identity_conflict_writes_nothing() {
    use crate::db::identity::{
        IdentityImportSnapshot, ImportedAccountIdentity, ImportedIdentity, ImportedQuotaPool,
    };
    use ocg_domain::credential::{
        ModelScope, credential_id_for_legacy_account, identity_id_for_legacy_account,
        quota_pool_id_for_identity,
    };

    let dir = temp_data_dir("import-v6-identity-conflict");
    let db = Database::open(dir.clone()).unwrap();
    db.import_accounts_with_contracts(&[go_import_record("dest-go")])
        .unwrap();
    let dest_identity = db
        .list_identity_model()
        .unwrap()
        .accounts
        .into_iter()
        .find(|row| row.account.id == "dest-go")
        .unwrap()
        .identity_id;
    let before_accounts = db
        .list_accounts()
        .unwrap()
        .into_iter()
        .map(|account| account.id)
        .collect::<Vec<_>>();
    let imported_id = "00000000-0000-4000-8000-0000000000b1";
    let credential_id = credential_id_for_legacy_account(imported_id).to_string();
    let binding_id = "00000000-0000-4000-8000-0000000000b2".to_string();
    let mut record = node_import_record(
        &db,
        vec![go_import_record(imported_id)],
        Vec::new(),
        Vec::new(),
    );
    record.identity_snapshot = Some(IdentityImportSnapshot {
        identities: vec![ImportedIdentity {
            id: dest_identity.clone(),
            label: "Shared".into(),
            identity_confidence: "opaque".into(),
            authority_site: None,
            authority_subject: None,
            enabled: true,
            notes: None,
        }],
        accounts: vec![ImportedAccountIdentity {
            account_id: imported_id.into(),
            identity_id: dest_identity.clone(),
            credential_id: credential_id.clone(),
            credential_version: 1,
            auth_state_version: 1,
            binding_id: binding_id.clone(),
            binding_enabled: true,
            binding_model_scope: ModelScope::All,
            allowed_endpoint_ids: Vec::new(),
            allowed_origins: Vec::new(),
        }],
        quota_pools: vec![ImportedQuotaPool {
            id: quota_pool_id_for_identity(&dest_identity).to_string(),
            subject_kind: "credential".into(),
            subject_ref: dest_identity.clone(),
            relation_confidence: "unknown".into(),
            policy_mode: "authoritative_limit".into(),
            member_account_ids: vec![imported_id.into()],
        }],
    });
    let error = db
        .import_node_state(&record, |_| -> Result<()> { Ok(()) })
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("already attached to a destination-only account"),
        "{error}"
    );
    assert_eq!(
        db.list_accounts()
            .unwrap()
            .into_iter()
            .map(|account| account.id)
            .collect::<Vec<_>>(),
        before_accounts
    );
    assert!(db.get_account(imported_id).unwrap().is_none());
    assert_eq!(
        identity_id_for_legacy_account("dest-go").to_string(),
        dest_identity
    );
    drop(db);
    fs::remove_dir_all(dir).unwrap();
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
    assert_eq!(schema_version_on(&db.conn).unwrap(), CURRENT_SCHEMA_VERSION);
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
const FIXTURE_ACCOUNT_PLAINTEXT: &str = "sk-fixture";

fn test_host_cipher() -> Arc<dyn KeyCipher + Send + Sync> {
    Arc::new(StaticKeyCipher::new(TEST_HOST_SECRET))
}

fn fixture_account_key_cipher() -> String {
    test_host_cipher()
        .encrypt(FIXTURE_ACCOUNT_PLAINTEXT)
        .expect("test host cipher should encrypt fixture account keys")
}

fn open_with_host_cipher(dir: PathBuf) -> Result<Database> {
    Database::open_with_cipher(dir, test_host_cipher())
}

fn assert_fixture_account_cipher(value: &str) {
    assert_eq!(
        test_host_cipher()
            .decrypt(value)
            .expect("fixture account cipher should decrypt with the test host"),
        FIXTURE_ACCOUNT_PLAINTEXT
    );
}

fn temp_data_dir(label: &str) -> PathBuf {
    let mut dir = std::env::temp_dir();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock should be after epoch")
        .as_nanos();
    dir.push(format!("ocg-db-test-{label}-{nanos}"));
    fs::create_dir_all(&dir).expect("test data dir should be created");
    dir
}

fn create_v21_fixture(dir: &Path, include_reserved_account_conflict: bool) {
    let db = Database::open(dir.to_path_buf()).expect("fixture database should open");
    let mut rollback = account("rollback-account");
    rollback.key_cipher = fixture_account_key_cipher();
    db.create_account(&rollback)
        .expect("representative account should save");
    db.log_forward(&forward_log("rollback-account", "success", 4.25))
        .expect("representative forward log should save");
    if !include_reserved_account_conflict {
        db.conn
            .execute(
                "DELETE FROM credentials WHERE legacy_account_id = ?1",
                [ZEN_FREE_ACCOUNT_ID],
            )
            .expect("reserved v22 account should be removed from a normal v21 fixture");
    }
    drop(db);
    reverse_current_to_v34(dir);

    let conn = Connection::open(dir.join("data.sqlite")).expect("fixture db should reopen");
    conn.execute_batch(
        "PRAGMA foreign_keys=OFF;
             DROP TRIGGER IF EXISTS access_keys_protect_primary_delete;
             DROP TABLE IF EXISTS access_keys;
             CREATE TABLE IF NOT EXISTS sub_gateway_keys (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                key TEXT NOT NULL,
                enabled INTEGER NOT NULL DEFAULT 1,
                deleted_at TEXT,
                created_at TEXT NOT NULL
             );
             CREATE UNIQUE INDEX IF NOT EXISTS idx_sub_gateway_keys_key
                ON sub_gateway_keys(key) WHERE deleted_at IS NULL AND key <> '';
             DROP INDEX IF EXISTS idx_forward_logs_route_account;
             DROP INDEX IF EXISTS idx_forward_logs_provider_offering;
             DROP INDEX IF EXISTS idx_account_model_capabilities_account;
             DROP TABLE IF EXISTS provider_usage_sync_state;
             DROP TABLE IF EXISTS provider_pricing_snapshots;
             DROP TABLE IF EXISTS credit_balances;
             DROP TABLE IF EXISTS quota_windows;
             DROP TABLE IF EXISTS account_custom_configs;
             DROP TABLE IF EXISTS account_model_capabilities;
             ALTER TABLE accounts DROP COLUMN verification_error;
             ALTER TABLE accounts DROP COLUMN connection_verified_at;
             ALTER TABLE accounts DROP COLUMN verification_status;
             ALTER TABLE forward_logs DROP COLUMN native_cost_currency;
             ALTER TABLE forward_logs DROP COLUMN native_cost_unit;
             ALTER TABLE forward_logs DROP COLUMN native_cost_value;
             ALTER TABLE forward_logs DROP COLUMN upstream_model;
             ALTER TABLE forward_logs DROP COLUMN resolved_alias;
             ALTER TABLE forward_logs DROP COLUMN requested_model;
             ALTER TABLE accounts DROP COLUMN free_alias_enabled;
             ALTER TABLE accounts DROP COLUMN quota_scope;
             ALTER TABLE accounts DROP COLUMN credential_kind;
             ALTER TABLE accounts DROP COLUMN offering_id;
             ALTER TABLE accounts DROP COLUMN provider_id;
             ALTER TABLE forward_logs DROP COLUMN effective_paid_cost_usd;
             ALTER TABLE forward_logs DROP COLUMN quota_debit;
             ALTER TABLE forward_logs DROP COLUMN raw_cost_usd;
             ALTER TABLE forward_logs DROP COLUMN credential_account_id;
             ALTER TABLE forward_logs DROP COLUMN offering_id;
             ALTER TABLE forward_logs DROP COLUMN provider_id;
             ALTER TABLE forward_logs DROP COLUMN route_account_id;
             DELETE FROM schema_version;
             INSERT INTO schema_version (version) VALUES (21);
             PRAGMA foreign_keys=ON;",
    )
    .expect("v21 fixture should be created");
    restore_usage_sync_account_columns(&conn);
}

fn restore_usage_sync_account_columns(conn: &Connection) {
    for (column, definition) in [
        ("usage_sync_last_success_at", "TEXT"),
        ("usage_sync_last_attempt_at", "TEXT"),
        ("usage_sync_next_eligible_at", "TEXT"),
        ("usage_sync_failure_streak", "INTEGER NOT NULL DEFAULT 0"),
        ("usage_sync_last_expedited_at", "TEXT"),
    ] {
        if !table_has_column(conn, "accounts", column).unwrap() {
            conn.execute(
                &format!("ALTER TABLE accounts ADD COLUMN {column} {definition}"),
                [],
            )
            .unwrap();
        }
    }
}

fn create_v20_fixture(dir: &Path, include_reserved_account_conflict: bool) {
    create_v21_fixture(dir, include_reserved_account_conflict);
    let conn = Connection::open(dir.join("data.sqlite")).expect("v21 fixture should reopen");
    for column in USAGE_SYNC_ACCOUNT_COLUMNS {
        if table_has_column(&conn, "accounts", column).unwrap() {
            conn.execute(&format!("ALTER TABLE accounts DROP COLUMN {column}"), [])
                .unwrap();
        }
    }
    conn.execute_batch(
        "DELETE FROM schema_version;
             INSERT INTO schema_version (version) VALUES (20);",
    )
    .expect("v20 fixture should be created");
}

fn pre_v22_backup_paths(dir: &Path) -> Vec<PathBuf> {
    let mut paths = fs::read_dir(dir)
        .expect("fixture directory should be readable")
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| {
                    name.starts_with(PRE_V22_BACKUP_FILE_PREFIX) && name.ends_with(".bak")
                })
        })
        .collect::<Vec<_>>();
    paths.sort();
    paths
}

fn backup_paths_with_prefix(dir: &Path, prefix: &str) -> Vec<PathBuf> {
    let mut paths = fs::read_dir(dir)
        .expect("fixture directory should be readable")
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(prefix) && name.ends_with(".bak"))
        })
        .collect::<Vec<_>>();
    paths.sort();
    paths
}

fn pre_v23_backup_paths(dir: &Path) -> Vec<PathBuf> {
    backup_paths_with_prefix(dir, PRE_V23_BACKUP_FILE_PREFIX)
}

fn pre_v3_backup_paths(dir: &Path) -> Vec<PathBuf> {
    backup_paths_with_prefix(dir, PRE_V3_BACKUP_FILE_PREFIX)
}

fn pre_v35_backup_paths(dir: &Path) -> Vec<PathBuf> {
    backup_paths_with_prefix(dir, PRE_V35_BACKUP_FILE_PREFIX)
}

fn drop_unified_provider_tables(conn: &Connection) {
    conn.execute_batch(
        "DROP TABLE IF EXISTS provider_models;
         DROP TABLE IF EXISTS providers;
         DROP TABLE IF EXISTS provider_model_protocol_preferences_v42;",
    )
    .expect("pre-v42 fixtures must not carry unified provider tables");
}

fn restore_v47_inert_columns(conn: &Connection) {
    account_store::materialize_legacy_accounts_for_rewind(conn).unwrap();
    conn.execute_batch(
        "ALTER TABLE accounts ADD COLUMN free_alias_enabled INTEGER NOT NULL DEFAULT 0;
         ALTER TABLE provider_contract_scopes ADD COLUMN chat_completions_enabled INTEGER NOT NULL DEFAULT 1;
         ALTER TABLE provider_contract_scopes ADD COLUMN responses_enabled INTEGER NOT NULL DEFAULT 1;
         ALTER TABLE provider_contract_scopes ADD COLUMN messages_enabled INTEGER NOT NULL DEFAULT 1;",
    )
    .expect("v47 inert columns should restore");
}

fn rewind_current_to_v47(conn: &Connection) {
    restore_v47_inert_columns(conn);
    conn.execute_batch(
        "DELETE FROM schema_version;
         INSERT INTO schema_version (version) VALUES (47);",
    )
    .expect("schema should rewind to v47");
    assert_eq!(schema_version_on(conn).unwrap(), V47_SCHEMA_VERSION);
}

fn reverse_current_to_v34(dir: &Path) {
    let path = dir.join("data.sqlite");
    let conn = Connection::open(&path).expect("migrated database should reopen for reverse");
    account_store::materialize_legacy_accounts_for_rewind(&conn).unwrap();
    drop_unified_provider_tables(&conn);
    restore_v47_inert_columns(&conn);
    conn.execute_batch(
        "
        PRAGMA foreign_keys=OFF;
        ALTER TABLE accounts ADD COLUMN offering_id TEXT NOT NULL DEFAULT 'go';
        UPDATE accounts SET offering_id = CASE provider_id
            WHEN 'opencode' THEN 'go'
            WHEN 'opencode-zen-free' THEN 'anonymous-free'
            WHEN 'command-code' THEN 'goat'
            WHEN 'minimax' THEN 'cn'
            WHEN 'kimi' THEN 'cn'
            WHEN 'custom' THEN 'api'
            WHEN 'cpa' THEN 'local'
            ELSE offering_id
        END;
        ALTER TABLE forward_logs ADD COLUMN offering_id TEXT;
        UPDATE forward_logs SET offering_id = CASE provider_id
            WHEN 'opencode' THEN 'go'
            WHEN 'opencode-zen-free' THEN 'anonymous-free'
            WHEN 'command-code' THEN 'goat'
            WHEN 'minimax' THEN 'cn'
            WHEN 'kimi' THEN 'cn'
            WHEN 'custom' THEN 'api'
            WHEN 'cpa' THEN 'local'
            ELSE offering_id
        END;
        DROP INDEX IF EXISTS idx_forward_logs_provider;
        CREATE INDEX IF NOT EXISTS idx_forward_logs_provider_offering
            ON forward_logs(provider_id, offering_id);
        CREATE TABLE provider_model_catalogs_v34 (
            provider_id TEXT NOT NULL,
            offering_id TEXT NOT NULL,
            models_json TEXT NOT NULL,
            refreshed_at TEXT,
            source_url TEXT NOT NULL,
            PRIMARY KEY (provider_id, offering_id)
        );
        INSERT INTO provider_model_catalogs_v34
            (provider_id, offering_id, models_json, refreshed_at, source_url)
        SELECT provider_id,
               CASE provider_id
                   WHEN 'opencode' THEN 'go'
                   WHEN 'opencode-zen-free' THEN 'anonymous-free'
                   WHEN 'command-code' THEN 'goat'
                   WHEN 'minimax' THEN 'cn'
                   WHEN 'kimi' THEN 'cn'
                   WHEN 'custom' THEN 'api'
                   WHEN 'cpa' THEN 'local'
                   ELSE 'unknown'
               END,
               models_json, refreshed_at, source_url
          FROM provider_model_catalogs;
        DROP TABLE provider_model_catalogs;
        ALTER TABLE provider_model_catalogs_v34 RENAME TO provider_model_catalogs;
        DROP INDEX IF EXISTS idx_provider_pricing_active;
        CREATE TABLE provider_pricing_snapshots_v34 (
            provider_id TEXT NOT NULL,
            offering_id TEXT NOT NULL,
            revision TEXT NOT NULL,
            activated_at TEXT NOT NULL,
            document_updated_at TEXT,
            source_url TEXT NOT NULL,
            content_hash TEXT NOT NULL,
            snapshot_json TEXT NOT NULL,
            PRIMARY KEY (provider_id, offering_id, revision)
        );
        INSERT INTO provider_pricing_snapshots_v34
            (provider_id, offering_id, revision, activated_at, document_updated_at,
             source_url, content_hash, snapshot_json)
        SELECT provider_id,
               CASE provider_id
                   WHEN 'opencode' THEN 'go'
                   WHEN 'opencode-zen-free' THEN 'anonymous-free'
                   WHEN 'command-code' THEN 'goat'
                   WHEN 'minimax' THEN 'cn'
                   WHEN 'kimi' THEN 'cn'
                   WHEN 'custom' THEN 'api'
                   WHEN 'cpa' THEN 'local'
                   ELSE 'unknown'
               END,
               revision, activated_at, document_updated_at, source_url, content_hash, snapshot_json
          FROM provider_pricing_snapshots;
        DROP TABLE provider_pricing_snapshots;
        ALTER TABLE provider_pricing_snapshots_v34 RENAME TO provider_pricing_snapshots;
        CREATE INDEX IF NOT EXISTS idx_provider_pricing_active
            ON provider_pricing_snapshots(provider_id, offering_id, activated_at DESC);
        DELETE FROM schema_version;
        INSERT INTO schema_version (version) VALUES (34);
        PRAGMA foreign_keys=ON;
        ",
    )
    .expect("schema should reverse to v34");
    assert_eq!(schema_version_on(&conn).unwrap(), V34_SCHEMA_VERSION);
}

fn reverse_current_to_v26(dir: &Path) {
    reverse_current_to_v34(dir);
    let path = dir.join("data.sqlite");
    let conn = Connection::open(&path).expect("migrated database should reopen for reverse");
    let primary = conn
        .query_row(
            "SELECT key FROM access_keys WHERE is_primary = 1 LIMIT 1",
            [],
            |row| row.get::<_, String>(0),
        )
        .unwrap_or_default();
    if let Some(json) = conn
        .query_row(
            "SELECT value FROM settings WHERE key = 'config'",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .unwrap()
    {
        let mut value: serde_json::Value =
            serde_json::from_str(&json).unwrap_or_else(|_| serde_json::json!({}));
        if let Some(object) = value.as_object_mut() {
            object.insert(
                "gateway_key".to_string(),
                serde_json::Value::String(primary.clone()),
            );
        }
        conn.execute(
            "INSERT INTO settings (key, value) VALUES ('config', ?1)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [serde_json::to_string(&value).unwrap()],
        )
        .unwrap();
    } else if !primary.is_empty() {
        conn.execute(
            "INSERT INTO settings (key, value) VALUES ('config', ?1)",
            [serde_json::json!({ "gateway_key": primary }).to_string()],
        )
        .unwrap();
    }
    conn.execute_batch(
        "
            PRAGMA foreign_keys=OFF;
            CREATE TABLE IF NOT EXISTS sub_gateway_keys (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                key TEXT NOT NULL,
                enabled INTEGER NOT NULL DEFAULT 1,
                deleted_at TEXT,
                created_at TEXT NOT NULL
            );
            CREATE UNIQUE INDEX IF NOT EXISTS idx_sub_gateway_keys_key
                ON sub_gateway_keys(key) WHERE deleted_at IS NULL AND key <> '';
            INSERT OR IGNORE INTO sub_gateway_keys (id, name, key, enabled, deleted_at, created_at)
                SELECT id, name, key, enabled, deleted_at, created_at
                FROM access_keys WHERE is_primary = 0;
            DROP TRIGGER IF EXISTS access_keys_protect_primary_delete;
            DROP TABLE IF EXISTS access_keys;
            ",
    )
    .expect("access_keys should reverse into sub_gateway_keys");
    restore_usage_sync_account_columns(&conn);
    if table_has_column(&conn, "accounts", "goat_model_access").unwrap() {
        conn.execute_batch("ALTER TABLE accounts DROP COLUMN goat_model_access;")
            .expect("v28 GOAT model access should reverse out of the v26 fixture");
    }
    conn.execute_batch(
        "DELETE FROM schema_version;
             INSERT INTO schema_version (version) VALUES (26);
             PRAGMA foreign_keys=ON;",
    )
    .expect("schema should reverse to v26");
    assert_eq!(schema_version_on(&conn).unwrap(), V26_SCHEMA_VERSION);
}

fn account(id: &str) -> Account {
    Account {
        id: id.into(),
        provider_id: default_provider_id(),
        credential_kind: default_credential_kind(),
        quota_scope: default_quota_scope(),
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
        created_at: Utc::now(),
        updated_at: Utc::now(),
    }
}

fn persist_sanitation_account(db: &Database, plan: BuiltinProvider, id: &str, notes: &str) {
    let mut draft = account(id);
    draft.provider_id = plan.provider_id.to_string();
    draft.credential_kind = plan.credential_kind;
    draft.quota_scope = plan.quota_scope;
    draft.enabled = false;
    draft.notes = Some(notes.to_string());
    if plan_requires_custom_config(plan) {
        db.create_account_with_contract(
            &draft,
            Some(&AccountCustomConfigInput {
                endpoint_url: "https://api.example.com/v1/chat/completions".into(),
                upstream_protocol: UpstreamProtocolKind::ChatCompletions,
            }),
            &[AccountModelCapabilityInput {
                public_model: "org/model".into(),
                upstream_model: "org/model".into(),
                protocol: UpstreamProtocolKind::ChatCompletions,
                source: None,
            }],
        )
        .unwrap();
    } else {
        db.create_account_with_contract(&draft, None, &[]).unwrap();
    }
}

fn leftover_enable(db: &Database, id: &str) {
    let changed = db
        .conn
        .execute(
            "UPDATE credentials SET enabled = 1 WHERE legacy_account_id = ?1",
            [id],
        )
        .unwrap();
    assert_eq!(changed, 1, "{id}");
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SanitationSnapshot {
    enabled: bool,
    name: String,
    notes: Option<String>,
    updated_at: DateTime<Utc>,
    verification: ConnectionVerificationStatus,
    verification_error: Option<String>,
}

fn sanitation_snapshot(db: &Database, id: &str) -> SanitationSnapshot {
    let account = db.get_account(id).unwrap().expect(id);
    let verification = db.account_verification_state(id).unwrap().expect(id);
    SanitationSnapshot {
        enabled: account.enabled,
        name: account.name,
        notes: account.notes,
        updated_at: account.updated_at,
        verification: verification.status,
        verification_error: verification.verification_error,
    }
}

fn forward_log(account_id: &str, status: &str, cost: f64) -> ForwardLog {
    ForwardLog {
        id: 0,
        timestamp: Utc::now(),
        model: "test".into(),
        account_id: account_id.into(),
        account_name: account_id.into(),
        route_account_id: None,
        provider_id: None,
        credential_account_id: None,
        client_key_id: None,
        client_key_name: None,
        status: status.into(),
        http_status: Some(200),
        route: String::new(),
        prompt_tokens: 0,
        completion_tokens: 0,
        cached_tokens: 0,
        cache_creation_tokens: 0,
        cost: Some(cost),
        raw_cost_usd: None,
        quota_debit: None,
        effective_paid_cost_usd: None,
        pricing_revision_id: None,
        quota_multiplier: None,
        local_adjustment_multiplier: None,
        service_tier: None,
        cost_state: "legacy_estimate".into(),
        error_message: None,
        request_id: None,
        attempt: None,
        error_source: None,
        error_stage: None,
        duration_ms: None,
        diagnostic: None,
    }
}

#[test]
fn forward_log_route_defaults_empty_and_round_trips_explicit_labels() {
    let dir = temp_data_dir("v24-route-column");
    let db = Database::open(dir.clone()).unwrap();

    // Omitting route on the current schema keeps its empty default.
    db.conn
        .execute(
            "INSERT INTO forward_logs
                 (timestamp, model, account_id, account_name, status, cost_state)
                 VALUES ('2026-01-01T00:00:00Z', 'glm-5.3', 'a1', 'a1', 'success',
                         'legacy_estimate')",
            [],
        )
        .unwrap();

    let mut modern = forward_log("a1", "success", 0.5);
    modern.route = "proxy".to_string();
    modern.model = "gpt-5.6-luna".to_string();
    db.log_forward(&modern).unwrap();

    let logs = db.list_forward_logs(10).unwrap();
    assert_eq!(logs.len(), 2);
    let historical = logs.iter().find(|log| log.model == "glm-5.3").unwrap();
    assert_eq!(historical.route, "");
    let labeled = logs.iter().find(|log| log.model == "gpt-5.6-luna").unwrap();
    assert_eq!(labeled.route, "proxy");

    // The paginated query surface exposes the same column.
    let page = db
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
        .unwrap();
    assert_eq!(page.items.len(), 2);
    assert!(page.items.iter().all(|log| {
        (log.model == "glm-5.3" && log.route.is_empty())
            || (log.model == "gpt-5.6-luna" && log.route == "proxy")
    }));

    drop(db);
    fs::remove_dir_all(dir).unwrap();
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
fn managed_setup_requires_order_and_matching_verified_key() {
    let dir = temp_data_dir("managed-setup-state");
    let db = Database::open(dir.clone()).expect("db should open");
    let mut managed = account("managed");
    managed.account_type = AccountType::Managed;
    managed.setup_step = AccountSetupStep::GoogleAccount;
    managed.key_cipher.clear();
    managed.enabled = false;
    db.create_account(&managed).expect("draft should save");

    assert!(
        !db.advance_managed_setup(
            "managed",
            AccountSetupStep::OpencodeRegistration,
            AccountSetupStep::Payment,
        )
        .unwrap()
    );
    for (from, to) in [
        (
            AccountSetupStep::GoogleAccount,
            AccountSetupStep::OpencodeRegistration,
        ),
        (
            AccountSetupStep::OpencodeRegistration,
            AccountSetupStep::Payment,
        ),
    ] {
        assert!(db.advance_managed_setup("managed", from, to).unwrap());
    }
    db.conn
            .execute(
                "UPDATE credentials SET purchase_date = '2000-01-01', usage_month_window_cost_offset = 1 WHERE legacy_account_id = 'managed'",
                [],
            )
            .unwrap();
    assert!(
        db.advance_managed_setup(
            "managed",
            AccountSetupStep::Payment,
            AccountSetupStep::KeyVerification,
        )
        .unwrap()
    );
    let paid = db.get_account("managed").unwrap().unwrap();
    assert_eq!(paid.purchase_date, local_today());
    let month_offset: f64 = db
        .conn
        .query_row(
            "SELECT usage_month_window_cost_offset FROM credentials WHERE legacy_account_id = 'managed'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(month_offset, 0.0);
    assert!(
        db.save_managed_key_for_verification("managed", "candidate")
            .unwrap()
    );
    assert!(
        !db.complete_managed_setup_if_key_matches("managed", "stale")
            .unwrap()
    );
    assert!(
        db.complete_managed_setup_if_key_matches("managed", "candidate")
            .unwrap()
    );
    let ready = db.get_account("managed").unwrap().unwrap();
    assert_eq!(ready.setup_step, AccountSetupStep::Ready);
    assert!(ready.enabled);

    drop(db);
    fs::remove_dir_all(dir).expect("test data dir should be removed");
}

#[test]
fn delete_account_removes_credential_grants() {
    use ocg_domain::credential::credential_id_for_legacy_account;

    let dir = temp_data_dir("delete-grants");
    let mut db = Database::open(dir.clone()).expect("db should open");
    db.create_account(&account("gone"))
        .expect("account should save");
    let credential_id = credential_id_for_legacy_account("gone").to_string();
    let before: i64 = db
        .conn
        .query_row(
            "SELECT COUNT(*) FROM credential_grants WHERE credential_id = ?1",
            [&credential_id],
            |row| row.get(0),
        )
        .unwrap();
    assert!(before > 0, "create should persist credential grants");
    db.delete_account("gone").expect("account should delete");
    let after: i64 = db
        .conn
        .query_row(
            "SELECT COUNT(*) FROM credential_grants WHERE credential_id = ?1",
            [&credential_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(after, 0);
    drop(db);
    fs::remove_dir_all(dir).expect("test data dir should be removed");
}

#[test]
fn managed_key_verification_transaction_rolls_back_after_candidate_write_failure() {
    let dir = temp_data_dir("managed-key-atomic-rollback");
    let db = Database::open(dir.clone()).expect("db should open");
    let mut managed = account("managed-atomic");
    managed.account_type = AccountType::Managed;
    managed.setup_step = AccountSetupStep::KeyVerification;
    managed.key_cipher = "original-cipher".into();
    managed.enabled = false;
    db.create_account(&managed).expect("draft should save");
    let cooldown = (Utc::now() + Duration::hours(2)).to_rfc3339();
    db.conn
        .execute(
            "UPDATE credentials
                 SET auth_error = 'original-auth', last_error = 'original-limit',
                     cooldown_until = ?2, cooldown_generic_until = ?2,
                     verification_error = 'original-verification'
                 WHERE legacy_account_id = ?1",
            params![managed.id, cooldown],
        )
        .expect("rollback sentinel state should save");
    let before = db.get_account(&managed.id).unwrap().unwrap();
    let before_verification = db.account_verification_state(&managed.id).unwrap().unwrap();

    // The first transaction update writes the candidate while leaving the
    // setup step unchanged. This trigger deterministically aborts the
    // following completion update, after that first intended write.
    db.conn
        .execute_batch(
            "CREATE TRIGGER fail_managed_verification_completion
                 BEFORE UPDATE ON credentials
                 WHEN OLD.legacy_account_id = 'managed-atomic'
                      AND OLD.setup_step = 'key_verification'
                      AND NEW.setup_step = 'ready'
                 BEGIN
                     SELECT RAISE(ABORT, 'injected managed verification completion failure');
                 END;",
        )
        .expect("fault trigger should install");

    let error = db
        .commit_managed_key_verification(
            &managed.id,
            &ManagedKeyVerificationCas::from_account(&before),
            "candidate-cipher",
            &ManagedKeyVerificationWrite::Verified {
                rate_limit: None,
                account_name: managed.name.clone(),
            },
        )
        .expect_err("second intended write should fail");
    assert!(
        error
            .to_string()
            .contains("injected managed verification completion failure"),
        "{error:#}"
    );

    let after = db.get_account(&managed.id).unwrap().unwrap();
    let after_verification = db.account_verification_state(&managed.id).unwrap().unwrap();
    assert_eq!(after.key_cipher, before.key_cipher);
    assert_eq!(after.enabled, before.enabled);
    assert_eq!(after.setup_step, before.setup_step);
    assert_eq!(after.auth_error, before.auth_error);
    assert_eq!(after.last_error, before.last_error);
    assert_eq!(after.cooldown_until, before.cooldown_until);
    assert_eq!(after.cooldown_generic_until, before.cooldown_generic_until);
    assert_eq!(after.updated_at, before.updated_at);
    assert_eq!(after_verification.status, before_verification.status);
    assert_eq!(
        after_verification.connection_verified_at,
        before_verification.connection_verified_at
    );
    assert_eq!(
        after_verification.verification_error,
        before_verification.verification_error
    );
    assert!(
        db.list_gateway_logs(10)
            .unwrap()
            .iter()
            .all(|log| !log.message.contains("managed-atomic")),
        "success log must roll back with the account writes"
    );

    drop(db);
    fs::remove_dir_all(dir).expect("test data dir should be removed");
}

#[test]
fn managed_key_verification_transaction_rolls_back_if_gateway_audit_insert_aborts() {
    let dir = temp_data_dir("managed-key-audit-rollback");
    let db = Database::open(dir.clone()).expect("db should open");
    let mut managed = account("managed-audit-atomic");
    managed.account_type = AccountType::Managed;
    managed.setup_step = AccountSetupStep::KeyVerification;
    managed.key_cipher = "original-cipher".into();
    managed.enabled = false;
    db.create_account(&managed).expect("draft should save");
    let before = db.get_account(&managed.id).unwrap().unwrap();
    let before_verification = db.account_verification_state(&managed.id).unwrap().unwrap();

    db.conn
        .execute_batch(
            "CREATE TRIGGER fail_managed_verification_audit
                 BEFORE INSERT ON gateway_logs
                 BEGIN
                     SELECT RAISE(ABORT, 'injected managed verification audit failure');
                 END;",
        )
        .expect("fault trigger should install");

    let error = db
        .commit_managed_key_verification(
            &managed.id,
            &ManagedKeyVerificationCas::from_account(&before),
            "candidate-cipher",
            &ManagedKeyVerificationWrite::Verified {
                rate_limit: None,
                account_name: managed.name.clone(),
            },
        )
        .expect_err("gateway audit insert should fail");
    assert!(
        error
            .to_string()
            .contains("injected managed verification audit failure"),
        "{error:#}"
    );

    let after = db.get_account(&managed.id).unwrap().unwrap();
    let after_verification = db.account_verification_state(&managed.id).unwrap().unwrap();
    assert_eq!(after.key_cipher, before.key_cipher);
    assert_eq!(after.enabled, before.enabled);
    assert_eq!(after.setup_step, before.setup_step);
    assert_eq!(after.updated_at, before.updated_at);
    assert_eq!(after_verification.status, before_verification.status);
    assert_eq!(
        after_verification.connection_verified_at,
        before_verification.connection_verified_at
    );
    assert!(
        db.list_gateway_logs(10)
            .unwrap()
            .iter()
            .all(|log| !log.message.contains("managed-audit-atomic")),
        "aborted audit insert must not persist a success log"
    );

    drop(db);
    fs::remove_dir_all(dir).expect("test data dir should be removed");
}

fn forward_log_at(
    account_id: &str,
    status: &str,
    cost: f64,
    timestamp: DateTime<Utc>,
) -> ForwardLog {
    let mut log = forward_log(account_id, status, cost);
    log.timestamp = timestamp;
    log
}

fn finalize_success(db: &Database, account_id: &str, cost: f64, timestamp: DateTime<Utc>) {
    let id = db
        .log_forward(&forward_log_at(account_id, "streaming", 0.0, timestamp))
        .expect("log should insert");
    db.update_forward_log(
        id,
        "success",
        None,
        ForwardMetrics {
            cost,
            cost_state: "priced",
            ..ForwardMetrics::default()
        },
        None,
        None,
    )
    .expect("stream should finalize");
}

fn assert_cost(actual: f64, expected: f64) {
    assert!(
        (actual - expected).abs() < 1e-9,
        "expected {expected}, got {actual}"
    );
}

fn create_v6_database(
    dir: &std::path::Path,
    extra_cooldown_columns: &str,
    extra_indexes: &str,
) -> Connection {
    let conn = Connection::open(dir.join("data.sqlite")).expect("v6 db should open");
    conn.execute_batch(&format!(
        "CREATE TABLE schema_version (version INTEGER PRIMARY KEY);
             INSERT INTO schema_version (version) VALUES (6);
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
                 usage_month_anchor_success_cost REAL,
                 sort_order INTEGER NOT NULL DEFAULT 0
                 {extra_cooldown_columns}
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
             );
             {extra_indexes}"
    ))
    .expect("v6 schema should be created");
    conn
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
fn account_reads_fallback_after_v8_data_is_corrupted() {
    let dir = temp_data_dir("post-v8-purchase-date-corruption");
    let conn = create_v6_database(
        &dir,
        ", cooldown_generic_until TEXT, cooldown_5h_until TEXT, cooldown_week_until TEXT, cooldown_month_until TEXT",
        "",
    );
    conn.execute("INSERT INTO schema_version (version) VALUES (7)", [])
        .expect("v7 schema version should be recorded");
    drop(conn);

    let db = Database::open(dir.clone()).expect("database should open");
    let created_at = DateTime::parse_from_rfc3339("2026-01-02T01:30:00+02:00")
        .expect("fixed timestamp should parse")
        .with_timezone(&Utc);
    for id in ["null", "invalid"] {
        let mut legacy = account(id);
        legacy.purchase_date = "2025-12-31".to_string();
        legacy.created_at = created_at;
        legacy.updated_at = created_at;
        db.create_account(&legacy)
            .expect("account should be created before corruption");
    }
    // v12 重建 accounts 表后 recharge_date 是 NOT NULL（恢复 v1 原始约束），
    // 无法再被 UPDATE 成 NULL；只测试 invalid-text 这一支。
    db.conn
        .execute(
            "UPDATE credentials SET purchase_date = 'not-a-date' WHERE legacy_account_id = 'invalid'",
            [],
        )
        .expect("purchase date should be corrupted to invalid text");

    let accounts = db
        .list_accounts()
        .expect("one corrupt row must not break the account list");
    assert_eq!(accounts.len(), 3);
    // 仅 invalid 被破坏；null 仍持有原始 2025-12-31。
    let invalid_account = accounts
        .iter()
        .find(|a| a.id == "invalid")
        .expect("invalid account should be present");
    assert_eq!(
        invalid_account.purchase_date, "2026-01-01",
        "list_accounts should fall back to default date for corrupted rows"
    );
    let invalid = db
        .get_account("invalid")
        .expect("corrupt account query should work")
        .expect("corrupt account should exist");
    assert_eq!(invalid.purchase_date, "2026-01-01");
    assert_eq!(invalid.expires_on, "2026-02-01");
    let remains_invalid: bool = db
        .conn
        .query_row(
            "SELECT purchase_date = 'not-a-date' FROM credentials WHERE legacy_account_id = 'invalid'",
            [],
            |row| row.get(0),
        )
        .expect("raw purchase date should remain queryable");
    assert!(
        remains_invalid,
        "read fallback must not hide a migration rerun"
    );

    drop(db);
    fs::remove_dir_all(dir).expect("test data dir should be removed");
}

#[test]
fn account_creation_defaults_dates_and_appends_to_saved_order() {
    let dir = temp_data_dir("create-order");
    let db = Database::open(dir.clone()).expect("db should open");
    let purchase_date_column: i64 = db
        .conn
        .query_row(
            "SELECT COUNT(*)
                 FROM pragma_table_info('credentials')
                 WHERE name = 'purchase_date'",
            [],
            |row| row.get(0),
        )
        .expect("fresh credential schema should expose purchase date");
    assert_eq!(purchase_date_column, 1);
    let mut first = account("first");
    first.created_at = Utc::now() + Duration::days(1);
    db.create_account(&first)
        .expect("first account should save");
    let mut second = account("second");
    second.created_at = Utc::now() - Duration::days(1);
    second.purchase_date = "2024-01-31".to_string();
    db.create_account(&second)
        .expect("second account should save");

    let accounts = db.list_accounts().expect("accounts should load");
    assert_eq!(
        accounts
            .iter()
            .map(|account| account.id.as_str())
            .collect::<Vec<_>>(),
        [ZEN_FREE_ACCOUNT_ID, "first", "second"]
    );
    assert_eq!(accounts[1].purchase_date, local_today());
    assert_eq!(
        accounts[1].expires_on,
        purchase_expires_on(&accounts[1].purchase_date)
            .expect("default date should have an expiry")
    );
    assert_eq!(accounts[2].expires_on, "2024-02-29");

    let mut invalid = account("invalid");
    invalid.purchase_date = "2026-2-03".to_string();
    assert!(db.create_account(&invalid).is_err());
    assert!(
        db.get_account("invalid")
            .expect("invalid account lookup should work")
            .is_none()
    );

    drop(db);
    fs::remove_dir_all(dir).expect("test data dir should be removed");
}

#[test]
fn zen_enabled_has_a_dedicated_writer_and_generic_update_is_rejected() {
    let dir = temp_data_dir("zen-enabled-writer");
    let db = Database::open(dir.clone()).expect("db should open");
    db.set_setting("config", r#"{"marker":"before"}"#)
        .expect("initial config should save");
    let zen_before = db
        .get_account(ZEN_FREE_ACCOUNT_ID)
        .expect("Zen lookup should work")
        .expect("Zen singleton should exist");

    let generic = AccountUpdate {
        name: None,
        username: None,
        password: None,
        key: None,
        enabled: Some(!zen_before.enabled),
        referral_code: None,
        purchase_date: None,
        notes: None,
    };
    assert!(
        db.update_account(ZEN_FREE_ACCOUNT_ID, &generic, None, None)
            .is_err(),
        "generic account writers must not bypass the Zen facade"
    );

    db.conn
        .execute_batch(&format!(
            "CREATE TRIGGER reject_zen_provider_settings
                 BEFORE UPDATE OF enabled ON credentials
                 WHEN OLD.legacy_account_id = '{ZEN_FREE_ACCOUNT_ID}'
                 BEGIN
                     SELECT RAISE(ABORT, 'forced Zen settings failure');
                 END;"
        ))
        .expect("failure trigger should install");
    db.set_config(r#"{"marker":"after"}"#)
        .expect("ordinary config should save independently");
    let error = db
        .set_zen_free_enabled(!zen_before.enabled)
        .expect_err("Zen row failure must abort the config write");
    assert!(error.to_string().contains("forced Zen settings failure"));
    assert_eq!(
        db.get_setting("config").unwrap().as_deref(),
        Some(r#"{"marker":"after"}"#)
    );
    let zen_after_failure = db.get_account(ZEN_FREE_ACCOUNT_ID).unwrap().unwrap();
    assert_eq!(zen_after_failure.enabled, zen_before.enabled);

    db.conn
        .execute("DROP TRIGGER reject_zen_provider_settings", [])
        .expect("failure trigger should drop");
    db.set_zen_free_enabled(true)
        .expect("Zen enabled setting should save");
    let zen_after = db.get_account(ZEN_FREE_ACCOUNT_ID).unwrap().unwrap();
    assert!(zen_after.enabled);

    drop(db);
    fs::remove_dir_all(dir).expect("test data dir should be removed");
}

#[test]
fn reorder_accounts_validates_atomically_and_persists_dense_order() {
    let dir = temp_data_dir("reorder");
    let db = Database::open(dir.clone()).expect("db should open");
    for id in ["a", "b", "c"] {
        db.create_account(&account(id))
            .expect("account should be created");
    }

    db.reorder_accounts(&[
        "c".into(),
        "a".into(),
        "b".into(),
        ZEN_FREE_ACCOUNT_ID.into(),
    ])
    .expect("valid reorder should save");
    assert_eq!(account_ids(&db), ["c", "a", "b", ZEN_FREE_ACCOUNT_ID]);

    let duplicate = db
        .reorder_accounts(&[
            "c".into(),
            "c".into(),
            "b".into(),
            ZEN_FREE_ACCOUNT_ID.into(),
        ])
        .expect_err("duplicates should fail");
    assert!(matches!(
        duplicate,
        ReorderAccountsError::DuplicateAccountId
    ));
    assert_eq!(account_ids(&db), ["c", "a", "b", ZEN_FREE_ACCOUNT_ID]);

    for stale in [
        vec!["c".into(), "a".into()],
        vec!["c".into(), "a".into(), "missing".into()],
        Vec::<String>::new(),
    ] {
        let error = db
            .reorder_accounts(&stale)
            .expect_err("stale account set should fail");
        assert!(matches!(error, ReorderAccountsError::AccountSetMismatch));
        assert_eq!(account_ids(&db), ["c", "a", "b", ZEN_FREE_ACCOUNT_ID]);
    }

    let sort_orders = db
        .conn
        .prepare("SELECT routing_rank FROM credentials ORDER BY routing_rank")
        .expect("sort query should prepare")
        .query_map([], |row| row.get::<_, i64>(0))
        .expect("sort query should run")
        .collect::<rusqlite::Result<Vec<_>>>()
        .expect("sort orders should load");
    assert_eq!(sort_orders, [0, 1, 2, 3]);
    drop(db);

    let reopened = Database::open(dir.clone()).expect("db should reopen");
    assert_eq!(account_ids(&reopened), ["c", "a", "b", ZEN_FREE_ACCOUNT_ID]);
    drop(reopened);

    let empty_dir = temp_data_dir("reorder-empty");
    let empty = Database::open(empty_dir.clone()).expect("empty db should open");
    empty
        .reorder_accounts(&[ZEN_FREE_ACCOUNT_ID.into()])
        .expect("the built-in Zen row is the complete empty-user order");
    drop(empty);

    fs::remove_dir_all(dir).expect("test data dir should be removed");
    fs::remove_dir_all(empty_dir).expect("empty test data dir should be removed");
}

#[test]
fn reorder_accounts_rolls_back_when_an_update_fails_mid_transaction() {
    let dir = temp_data_dir("reorder-write-failure");
    let db = Database::open(dir.clone()).expect("db should open");
    for id in ["a", "b", "c"] {
        db.create_account(&account(id))
            .expect("account should be created");
    }
    db.conn
        .execute_batch(
            "CREATE TRIGGER reject_b_sort_update
                 BEFORE UPDATE OF routing_rank ON credentials
                 WHEN NEW.legacy_account_id = 'b'
                 BEGIN
                     SELECT RAISE(ABORT, 'forced reorder failure');
                 END;",
        )
        .expect("failure trigger should be installed");

    let error = db
        .reorder_accounts(&[
            "c".into(),
            "a".into(),
            "b".into(),
            ZEN_FREE_ACCOUNT_ID.into(),
        ])
        .expect_err("the trigger should interrupt the reorder");
    assert!(matches!(error, ReorderAccountsError::Database(_)));
    assert_eq!(account_ids(&db), [ZEN_FREE_ACCOUNT_ID, "a", "b", "c"]);

    drop(db);
    fs::remove_dir_all(dir).expect("test data dir should be removed");
}

fn account_ids(db: &Database) -> Vec<String> {
    db.list_accounts()
        .expect("accounts should load")
        .into_iter()
        .map(|account| account.id)
        .collect()
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
    assert_eq!(schema_version_on(&conn).unwrap(), 20);
    drop(conn);

    let backups_before = pre_v22_backup_paths(&dir);
    assert_eq!(backups_before.len(), 1);
    let backup_bytes = fs::read(&backups_before[0]).expect("backup should be readable");
    let backup = Connection::open_with_flags(&backups_before[0], OpenFlags::SQLITE_OPEN_READ_ONLY)
        .expect("backup should open read-only");
    assert_eq!(schema_version_on(&backup).unwrap(), 20);
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
fn clear_account_cooldown_clears_free_window() {
    let dir = temp_data_dir("clear-free-cooldown");
    let db = Database::open(dir.clone()).expect("db should open");
    db.create_account(&account("free-cd"))
        .expect("account should be created");

    let until = Utc::now() + Duration::minutes(30);
    db.set_account_rate_limit(
        "free-cd",
        until,
        r#"{"type":"FreeUsageLimitError","message":"Free usage exceeded"}"#,
        Some(UsageWindowKind::Free),
    )
    .expect("free rate limit should save");

    let cooled = db
        .get_account("free-cd")
        .expect("account should load")
        .expect("account should exist");
    assert!(cooled.cooldown_free_until.is_some());
    assert!(cooled.cooldown_until.is_some());

    db.clear_account_cooldown("free-cd")
        .expect("clear should succeed");
    let cleared = db
        .get_account("free-cd")
        .expect("account should load")
        .expect("account should exist");
    assert!(cleared.cooldown_free_until.is_none());
    assert!(cleared.cooldown_until.is_none());
    assert!(cleared.cooldown_generic_until.is_none());
    assert!(cleared.last_error.is_none());

    drop(db);
    fs::remove_dir_all(dir).expect("test data dir should be removed");
}

#[test]
fn free_channel_cooldown_survives_account_deletion_restart_and_expires() {
    let dir = temp_data_dir("global-free-cooldown");
    let until = Utc::now() + Duration::minutes(30);
    {
        let mut db = Database::open(dir.clone()).expect("db should open");
        db.create_account(&account("free-source"))
            .expect("source account should be created");
        db.set_account_rate_limit(
            "free-source",
            until,
            "free quota exhausted",
            Some(UsageWindowKind::Free),
        )
        .expect("free rate limit should save");
        db.delete_account("free-source")
            .expect("source account should be deleted");
        db.create_account(&account("replacement"))
            .expect("replacement account should be created");

        assert!(db.free_channel_cooldown_until().unwrap().is_some());
    }

    let db = Database::open(dir.clone()).expect("db should reopen");
    assert!(
        db.free_channel_cooldown_until()
            .expect("global cooldown should load")
            .is_some(),
        "deleting every source row and reopening must not clear the IP-wide cooldown"
    );
    assert!(
        db.free_channel_cooldown_until_at(until + Duration::seconds(1))
            .expect("expiry should be evaluated")
            .is_none(),
        "the global gate must reopen after its deadline"
    );

    drop(db);
    fs::remove_dir_all(dir).expect("test data dir should be removed");
}

#[test]
fn durable_free_cooldown_expires_at_exact_deadline() {
    let dir = temp_data_dir("free-cooldown-exact-boundary");
    let until = DateTime::from_naive_utc_and_offset(
        NaiveDate::from_ymd_opt(2024, 1, 2)
            .unwrap()
            .and_hms_opt(3, 4, 5)
            .unwrap(),
        Utc,
    );
    let db = Database::open(dir.clone()).expect("db should open");
    db.create_account(&account("free-source"))
        .expect("source account should be created");
    db.set_account_rate_limit(
        "free-source",
        until,
        "free quota exhausted",
        Some(UsageWindowKind::Free),
    )
    .expect("free rate limit should save");

    let stored = db
        .free_channel_cooldown_until_at(until - Duration::days(1))
        .expect("durable cooldown should load")
        .expect("durable cooldown should be active far before the deadline");
    assert!(
        db.free_channel_cooldown_until_at(stored - Duration::seconds(1))
            .expect("pre-deadline evaluation")
            .is_some(),
        "until > now must keep the durable Free gate closed"
    );
    assert_eq!(
        db.free_channel_cooldown_until_at(stored)
            .expect("exact-deadline evaluation"),
        None,
        "until == now must expire the durable Free gate"
    );

    drop(db);
    fs::remove_dir_all(dir).expect("test data dir should be removed");
}

#[test]
fn account_stays_cooling_until_all_windows_expire() {
    let dir = temp_data_dir("multi-window-cooldown");
    let db = Database::open(dir.clone()).expect("db should open");
    db.create_account(&account("multi"))
        .expect("account should be created");

    let now = Utc::now();
    let past_5h = now - Duration::minutes(1);
    let future_week = now + Duration::days(2);
    db.set_account_rate_limit(
        "multi",
        past_5h,
        "5-hour usage limit reached. Resets in 13min.",
        Some(UsageWindowKind::FiveHours),
    )
    .expect("5h rate limit should save");
    db.set_account_rate_limit(
        "multi",
        future_week,
        "weekly usage limit reached. Resets in 4 days.",
        Some(UsageWindowKind::Week),
    )
    .expect("weekly rate limit should save");

    let account = db
        .get_account("multi")
        .expect("account should load")
        .expect("account should exist");
    assert!(account.cooldown_5h_until.is_some_and(|until| until <= now));
    assert!(account.cooldown_week_until.is_some_and(|until| until > now));
    assert!(
        account
            .cooldown_until
            .is_some_and(|until| (until - future_week).num_seconds().abs() < 2)
    );

    drop(db);
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
fn diagnostic_retention_removes_only_old_json() {
    let dir = temp_data_dir("diagnostic-retention");
    let db = Database::open(dir.clone()).expect("db should open");
    db.conn
        .execute_batch(
            "INSERT INTO forward_logs
                    (timestamp, model, account_id, account_name, status, error_message,
                     request_id, attempt, error_source, error_stage, duration_ms, diagnostic_json)
                 VALUES
                    (datetime('now', '-31 days'), 'old', 'a', 'A', 'client_error', 'keep me',
                     'ocg-old', 1, 'upstream', 'upstream_http', 12, '{\"old\":true}'),
                    (datetime('now', '-29 days'), 'new', 'a', 'A', 'client_error', 'keep new',
                     'ocg-new', 1, 'upstream', 'upstream_http', 13, '{\"new\":true}');
                 INSERT INTO gateway_logs
                    (level, category, message, created_at, request_id, error_source,
                     error_stage, duration_ms, diagnostic_json)
                 VALUES
                    ('warn', 'gateway', 'old gateway', datetime('now', '-31 days'),
                     'ocg-gateway-old', 'client', 'parse', 5, '{\"old\":true}'),
                    ('warn', 'gateway', 'new gateway', datetime('now', '-29 days'),
                     'ocg-gateway-new', 'client', 'parse', 6, '{\"new\":true}');",
        )
        .expect("diagnostic rows should insert");
    drop(db);

    let db = Database::open(dir.clone()).expect("db reopen should apply retention");
    let (old_detail, old_id, old_error, old_source): (Option<String>, String, String, String) = db
        .conn
        .query_row(
            "SELECT diagnostic_json, request_id, error_message, error_source
                 FROM forward_logs WHERE request_id='ocg-old'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .expect("old row should remain");
    assert!(old_detail.is_none());
    assert_eq!(old_id, "ocg-old");
    assert_eq!(old_error, "keep me");
    assert_eq!(old_source, "upstream");
    let new_detail: Option<String> = db
        .conn
        .query_row(
            "SELECT diagnostic_json FROM forward_logs WHERE request_id='ocg-new'",
            [],
            |row| row.get(0),
        )
        .expect("new detail should load");
    assert!(new_detail.is_some());
    let gateway_details: (Option<String>, Option<String>) = db
        .conn
        .query_row(
            "SELECT
                    (SELECT diagnostic_json FROM gateway_logs WHERE request_id='ocg-gateway-old'),
                    (SELECT diagnostic_json FROM gateway_logs WHERE request_id='ocg-gateway-new')",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("gateway details should load");
    assert!(gateway_details.0.is_none());
    assert!(gateway_details.1.is_some());

    drop(db);
    fs::remove_dir_all(dir).expect("test data dir should be removed");
}

#[test]
fn fixed_window_5h_anchors_at_the_first_unexpired_success() {
    let dir = temp_data_dir("fixed-5h-windows");
    let db = Database::open(dir.clone()).unwrap();
    for (id, history, expected_cost, expected_minutes) in [
        ("active", &[(4, 1.0), (3, 2.0)][..], 3.0, 60),
        ("after-expiry", &[(6, 10.0), (1, 5.0)][..], 5.0, 240),
        (
            "after-multiple",
            &[(19, 10.0), (13, 5.0), (7, 3.0), (1, 2.0)][..],
            2.0,
            240,
        ),
    ] {
        db.create_account(&account(id)).unwrap();
        let now = Utc::now();
        for &(hours_ago, cost) in history {
            finalize_success(&db, id, cost, now - Duration::hours(hours_ago));
        }
        let usage = db.opencode_go_account_usage(id).unwrap();
        assert_cost(usage.window_5h, expected_cost);
        let reset = usage.resets_in_5h.expect("active window must have a reset");
        let remaining = (reset - Utc::now()).num_minutes();
        assert!(
            (expected_minutes - 5..=expected_minutes + 5).contains(&remaining),
            "{id}: expected ~{expected_minutes}min remaining, got {remaining}"
        );
    }
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn fixed_window_treats_exact_end_as_the_next_window_start() {
    let dir = temp_data_dir("fixed-boundary");
    let db = Database::open(dir.clone()).expect("db should open");
    db.create_account(&account("boundary"))
        .expect("account should be created");

    let first = Utc::now() - Duration::hours(5) - Duration::minutes(1);
    let exact_end = first + Duration::hours(5);
    finalize_success(&db, "boundary", 10.0, first);
    finalize_success(&db, "boundary", 2.0, exact_end);

    let usage = db
        .opencode_go_account_usage("boundary")
        .expect("usage should load");
    assert_cost(usage.window_5h, 2.0);
    assert!(usage.resets_in_5h.is_some());

    drop(db);
    fs::remove_dir_all(dir).expect("test data dir should be removed");
}

#[test]
fn fixed_window_5h_advances_through_multiple_expired_windows_in_one_call() {
    // 复现用户报告的"刷新递减"循环 bug：
    //   4 条间隔 6h 的计费日志（全部已过期）。
    // 旧实现每次刷新只前进一个窗口，前端可见 60→30→13→5.8→0→60+ 循环；
    // 修复后一次调用内连过 4 个过期窗口，next=None 时清空并返回 0，
    // 第二次刷新仍为 0，不再回到最旧日志。
    let dir = temp_data_dir("fixed-5h-multi-expired");
    let db = Database::open(dir.clone()).expect("db should open");
    db.create_account(&account("cycle"))
        .expect("account should be created");

    // ts1 = -24h, ts2 = -18h, ts3 = -12h, ts4 = -6h：每条间隔 6h（> 5h 窗口长度）。
    let ts1 = Utc::now() - Duration::hours(24);
    let ts2 = ts1 + Duration::hours(6);
    let ts3 = ts2 + Duration::hours(6);
    let ts4 = ts3 + Duration::hours(6);
    finalize_success(&db, "cycle", 10.0, ts1);
    finalize_success(&db, "cycle", 5.0, ts2);
    finalize_success(&db, "cycle", 3.0, ts3);
    finalize_success(&db, "cycle", 2.0, ts4);

    // 第一次刷新：应直接走完所有过期窗口，返回 0（无新请求）。
    let usage = db
        .opencode_go_account_usage("cycle")
        .expect("usage should load");
    assert_cost(usage.window_5h, 0.0);
    assert!(
        usage.resets_in_5h.is_none(),
        "no active window after all expired; resets_in_5h should be None"
    );

    // 第二次刷新：不应回到最旧日志循环重放，仍稳定为 0。
    let usage2 = db
        .opencode_go_account_usage("cycle")
        .expect("usage should load again");
    assert_cost(usage2.window_5h, 0.0);
    assert!(usage2.resets_in_5h.is_none());

    drop(db);
    fs::remove_dir_all(dir).expect("test data dir should be removed");
}

#[test]
fn fixed_window_5h_with_no_usage_returns_zero_and_full_window_remaining() {
    let dir = temp_data_dir("fixed-5h-empty");
    let db = Database::open(dir.clone()).expect("db should open");
    db.create_account(&account("empty"))
        .expect("account should be created");

    let usage = db
        .opencode_go_account_usage("empty")
        .expect("usage should load");
    assert_cost(usage.window_5h, 0.0);
    // 没用过：倒计时为 None（前端显示"5h0min"由默认值决定）
    assert!(usage.resets_in_5h.is_none());

    drop(db);
    fs::remove_dir_all(dir).expect("test data dir should be removed");
}

#[test]
fn month_window_accumulates_from_purchase_date_to_expires_on() {
    let dir = temp_data_dir("month-window");
    let db = Database::open(dir.clone()).expect("db should open");
    let mut acct = account("monthly");
    acct.purchase_date = "2026-07-01".into();
    db.create_account(&acct).expect("account should be created");

    // 模拟一条历史成功请求（任何时间都算，月窗口从 purchase_date 累计）
    finalize_success(&db, "monthly", 5.0, Utc::now());

    let usage = db
        .opencode_go_account_usage("monthly")
        .expect("usage should load");
    assert_cost(usage.window_month, 5.0);
    let reset = usage
        .resets_in_month
        .expect("month window reset should be purchase_date + 1 month");
    // 2026-07-01 + 1 自然月 = 2026-08-01 00:00
    let expected = DateTime::parse_from_rfc3339("2026-08-01T00:00:00+00:00")
        .unwrap()
        .with_timezone(&Utc);
    assert!(
        (reset - expected).num_seconds().abs() < 86400,
        "expected ~2026-08-01, got {reset}"
    );

    drop(db);
    fs::remove_dir_all(dir).expect("test data dir should be removed");
}

#[test]
fn manual_calibrate_5h_window_sets_started_at_and_cost_offset() {
    let dir = temp_data_dir("calibrate-5h");
    let db = Database::open(dir.clone()).expect("db should open");
    db.create_account(&account("calib"))
        .expect("account should be created");

    // 用户在别处已用 50%，距上游重置还剩 3 小时
    db.calibrate_account_usage("calib", UsageWindowKind::FiveHours, 50.0, Some(180), 12.0)
        .expect("calibrate should save");

    let usage = db
        .opencode_go_account_usage("calib")
        .expect("usage should load");
    // 5h 限额 12.0，50% = 6.0
    assert_cost(usage.window_5h, 6.0);
    let reset = usage
        .resets_in_5h
        .expect("5h window reset should be set after manual calibrate");
    let remaining_min = (reset - Utc::now()).num_minutes();
    assert!(
        (175..=185).contains(&remaining_min),
        "expected ~180min remaining, got {remaining_min}"
    );

    // 后续网关内的请求累加到偏移之上
    finalize_success(&db, "calib", 1.0, Utc::now());
    let usage = db
        .opencode_go_account_usage("calib")
        .expect("usage should reload");
    assert_cost(usage.window_5h, 7.0);

    drop(db);
    fs::remove_dir_all(dir).expect("test data dir should be removed");
}

#[test]
fn calibrate_subtracts_existing_window_usage_from_offset() {
    // 回归测试：活跃账号（窗口内已有 forward_logs）校准时，
    // offset 必须 = target_cost - actual_cost，否则 compute_fixed_window
    // 返回 offset + actual_cost，显示百分比会高于用户输入。
    let dir = temp_data_dir("calibrate-with-usage");
    let db = Database::open(dir.clone()).expect("db should open");
    db.create_account(&account("active"))
        .expect("account should be created");

    // 1 小时前已用 $3（落在 5h 窗口内）
    let ts = Utc::now() - Duration::hours(1);
    finalize_success(&db, "active", 3.0, ts);

    // 用户说"我在别处用到了 50%"（5h 限额 12.0 → target_cost = 6.0）
    // 期望：offset = 6.0 - 3.0 = 3.0，compute_fixed_window 返回 3.0 + 3.0 = 6.0 = 50%
    // 修复前 bug：offset = 6.0，compute_fixed_window 返回 6.0 + 3.0 = 9.0 = 75%
    // 用 resets_in_minutes=180 让新窗口的 started_at = now + 3h - 5h = now - 2h，
    // 把 1 小时前的 log 稳稳包含进窗口（避开 finalize 与 calibrate 之间的微秒级时序差）。
    db.calibrate_account_usage("active", UsageWindowKind::FiveHours, 50.0, Some(180), 12.0)
        .expect("calibrate should save with existing usage");
    let usage = db
        .opencode_go_account_usage("active")
        .expect("usage should load");
    assert_cost(usage.window_5h, 6.0);

    // 后续请求继续累加：offset=3.0 + actual=3.0 + new=2.0 = 8.0
    finalize_success(&db, "active", 2.0, Utc::now());
    let usage = db
        .opencode_go_account_usage("active")
        .expect("usage should reload");
    assert_cost(usage.window_5h, 8.0);

    drop(db);
    fs::remove_dir_all(dir).expect("test data dir should be removed");
}

#[test]
fn calibrate_below_actual_usage_allows_negative_offset() {
    // 回归测试（Bug 1.5）：用户校准的百分比低于窗口内实际 cost 时，offset 允许为负数，
    // 让 compute_fixed_window 返回 offset + actual = target_cost，与用户输入一致。
    // 之前 max(0, target - actual) 钳制 + schema CHECK (offset >= 0) 约束让向左拉
    // 滑块时被锁死在实际 cost 对应的百分比（9.0 / 12.0 * 100 = 75%，对应用户看到的 40.2%）。
    let dir = temp_data_dir("calibrate-below-usage");
    let db = Database::open(dir.clone()).expect("db should open");
    db.create_account(&account("clamp"))
        .expect("account should be created");

    // 已用 $9
    let ts = Utc::now() - Duration::hours(1);
    finalize_success(&db, "clamp", 9.0, ts);

    // 用户校准到 20%（target_cost = 2.4，但实际已用 9.0）
    // offset = 2.4 - 9.0 = -6.6；compute_fixed_window 返回 -6.6 + 9.0 = 2.4 = 20%。
    // 用 resets_in_minutes=180 让新窗口的 started_at = now - 2h，把 1 小时前的
    // $9 log 稳稳包含进窗口（避开 finalize 与 calibrate 之间的微秒级时序差）。
    db.calibrate_account_usage("clamp", UsageWindowKind::FiveHours, 20.0, Some(180), 12.0)
        .expect("calibrate below actual usage should allow negative offset");
    let usage = db
        .opencode_go_account_usage("clamp")
        .expect("usage should load");
    // 显示的 cost = offset(-6.6) + actual(9.0) = 2.4（用户输入的 20%）
    assert_cost(usage.window_5h, 2.4);

    drop(db);
    fs::remove_dir_all(dir).expect("test data dir should be removed");
}

#[test]
fn calibrate_month_window_writes_offset_without_started_at() {
    // 回归测试（Bug 2）：月窗口必须支持手动校准。
    // 月窗口不写 started_at 列（起点固定为 purchase_date），只更新 cost_offset。
    // resets_in_minutes 被忽略——窗口由 purchase_date/expires_on 决定。
    let dir = temp_data_dir("calibrate-month");
    let db = Database::open(dir.clone()).expect("db should open");
    let mut acct = account("monthly-calib");
    acct.purchase_date = "2026-07-01".into();
    db.create_account(&acct).expect("account should be created");

    // 已用 $5（落在月窗口内：purchase_date 00:00 起）
    finalize_success(&db, "monthly-calib", 5.0, Utc::now());

    // 用户校准到 50%（月限额 100.0 → target_cost = 50.0）
    // 期望：offset = 50.0 - 5.0 = 45.0；compute_month_window 返回 45.0 + 5.0 = 50.0 = 50%。
    db.calibrate_account_usage("monthly-calib", UsageWindowKind::Month, 50.0, None, 100.0)
        .expect("month window calibrate should save");
    let usage = db
        .opencode_go_account_usage("monthly-calib")
        .expect("usage should load");
    assert_cost(usage.window_month, 50.0);
    // resets_in_month 仍是 purchase_date + 1 自然月（不受 resets_in_minutes 影响）
    let reset = usage
        .resets_in_month
        .expect("month window reset should be purchase_date + 1 month");
    let expected = DateTime::parse_from_rfc3339("2026-08-01T00:00:00+00:00")
        .unwrap()
        .with_timezone(&Utc);
    assert!(
        (reset - expected).num_seconds().abs() < 86400,
        "expected ~2026-08-01, got {reset}"
    );

    drop(db);
    fs::remove_dir_all(dir).expect("test data dir should be removed");
}

#[test]
fn changing_purchase_date_resets_month_calibration_offset() {
    let dir = temp_data_dir("month-renewal-reset");
    let db = Database::open(dir.clone()).expect("db should open");
    let new_purchase_date = local_today();
    let old_purchase_date = (Local::now().date_naive() - Duration::days(10))
        .format("%Y-%m-%d")
        .to_string();
    let mut acct = account("monthly-renewal");
    acct.purchase_date = old_purchase_date;
    db.create_account(&acct).expect("account should be created");

    finalize_success(&db, "monthly-renewal", 5.0, Utc::now() - Duration::days(2));
    db.calibrate_account_usage("monthly-renewal", UsageWindowKind::Month, 0.0, None, 100.0)
        .expect("month calibration should save a negative offset");
    assert_cost(
        db.opencode_go_account_usage("monthly-renewal")
            .expect("usage should load")
            .window_month,
        0.0,
    );

    db.update_account(
        "monthly-renewal",
        &AccountUpdate {
            name: None,
            username: None,
            password: None,
            key: None,
            enabled: None,
            referral_code: None,
            purchase_date: Some(new_purchase_date),
            notes: None,
        },
        None,
        None,
    )
    .expect("purchase date should update");
    let offset: f64 = db
        .conn
        .query_row(
            "SELECT usage_month_window_cost_offset FROM credentials WHERE legacy_account_id = ?1",
            ["monthly-renewal"],
            |row| row.get(0),
        )
        .expect("month offset should load");
    assert_cost(offset, 0.0);
    assert_cost(
        db.opencode_go_account_usage("monthly-renewal")
            .expect("renewed usage should load")
            .window_month,
        0.0,
    );

    finalize_success(&db, "monthly-renewal", 2.0, Utc::now());
    assert_cost(
        db.opencode_go_account_usage("monthly-renewal")
            .expect("new cycle usage should load")
            .window_month,
        2.0,
    );

    drop(db);
    fs::remove_dir_all(dir).expect("test data dir should be removed");
}

#[test]
fn replacing_key_clears_auth_error_but_other_updates_preserve_it() {
    let dir = temp_data_dir("auth-error-key-replacement");
    let db = Database::open(dir.clone()).expect("db should open");
    db.create_account(&account("auth-failed"))
        .expect("account should be created");
    let old_key_cipher = db
        .get_account("auth-failed")
        .expect("account should load")
        .expect("account should exist")
        .key_cipher;
    db.set_account_auth_error("auth-failed", Some("upstream auth error 401"))
        .expect("auth error should save");

    let rename = AccountUpdate {
        name: Some("renamed".into()),
        username: None,
        password: None,
        key: None,
        enabled: None,
        referral_code: None,
        purchase_date: None,
        notes: None,
    };
    db.update_account("auth-failed", &rename, None, None)
        .expect("non-key update should save");
    assert!(
        db.get_account("auth-failed")
            .expect("account should load")
            .expect("account should exist")
            .auth_error
            .is_some()
    );

    let no_fields = AccountUpdate {
        name: None,
        username: None,
        password: None,
        key: None,
        enabled: None,
        referral_code: None,
        purchase_date: None,
        notes: None,
    };
    db.update_account("auth-failed", &no_fields, Some("replacement-cipher"), None)
        .expect("key replacement should save");
    assert!(
        db.get_account("auth-failed")
            .expect("account should load")
            .expect("account should exist")
            .auth_error
            .is_none()
    );

    assert!(
        !db.set_account_auth_error_if_key_matches(
            "auth-failed",
            &old_key_cipher,
            Some("late old-key 401"),
        )
        .expect("stale auth response should be ignored")
    );
    assert!(
        db.get_account("auth-failed")
            .expect("account should load")
            .expect("account should exist")
            .auth_error
            .is_none(),
        "a delayed 401 from the old key must not break its replacement"
    );

    assert!(
        db.set_account_auth_error_if_key_matches(
            "auth-failed",
            "replacement-cipher",
            Some("new-key auth error"),
        )
        .expect("current-key auth response should save")
    );
    assert!(
        !db.set_account_auth_error_if_key_matches("auth-failed", &old_key_cipher, None)
            .expect("stale success response should be ignored")
    );
    assert_eq!(
        db.get_account("auth-failed")
            .expect("account should load")
            .expect("account should exist")
            .auth_error
            .as_deref(),
        Some("new-key auth error"),
        "a delayed success from the old key must not recover its replacement"
    );
    assert!(
        db.set_account_auth_error_if_key_matches("auth-failed", "replacement-cipher", None)
            .expect("current-key success should clear auth state")
    );

    let stale_cooldown = Utc::now() + Duration::days(3);
    assert!(
        !db.set_account_rate_limit_if_key_matches(
            "auth-failed",
            &old_key_cipher,
            stale_cooldown,
            "late old-key 429",
            None,
        )
        .expect("stale rate limit should be ignored")
    );
    let stored = db
        .get_account("auth-failed")
        .expect("account should load")
        .expect("account should exist");
    assert!(stored.cooldown_until.is_none());
    assert!(stored.last_error.is_none());

    drop(db);
    fs::remove_dir_all(dir).expect("test data dir should be removed");
}

#[test]
fn calibrate_rejects_reset_outside_fixed_window_without_panicking() {
    let dir = temp_data_dir("calibrate-reset-bounds");
    let db = Database::open(dir.clone()).expect("db should open");
    db.create_account(&account("reset-bounds"))
        .expect("account should be created");

    for (window, minutes) in [
        (UsageWindowKind::FiveHours, -1),
        (UsageWindowKind::FiveHours, 301),
        (UsageWindowKind::Week, 10_081),
        (UsageWindowKind::FiveHours, i64::MAX),
    ] {
        assert!(
            db.calibrate_account_usage("reset-bounds", window, 50.0, Some(minutes), 100.0,)
                .is_err(),
            "{window:?} should reject {minutes} minutes"
        );
    }

    drop(db);
    fs::remove_dir_all(dir).expect("test data dir should be removed");
}

fn snapshot_limits() -> PricingLimits {
    PricingLimits {
        window_5h: 12.0,
        window_week: 30.0,
        window_month: 100.0,
    }
}

fn usage_calibration(
    rolling_percent: f64,
    weekly_percent: f64,
    monthly_percent: f64,
    rolling_resets_in_minutes: i64,
    weekly_resets_in_minutes: i64,
) -> AccountUsageCalibrationSnapshot {
    AccountUsageCalibrationSnapshot {
        rolling_percent,
        weekly_percent,
        monthly_percent,
        rolling_resets_in_minutes,
        weekly_resets_in_minutes,
    }
}

fn usage_offset_row(db: &Database, id: &str) -> (Option<String>, f64, Option<String>, f64, f64) {
    db.conn
        .query_row(
            "SELECT usage_5h_window_started_at, usage_5h_window_cost_offset,
                        usage_week_window_started_at, usage_week_window_cost_offset,
                        usage_month_window_cost_offset
                 FROM credentials WHERE legacy_account_id = ?1",
            [id],
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
        .expect("usage offset row should load")
}

#[test]
fn calibrate_account_usage_snapshot_updates_all_three_windows() {
    let dir = temp_data_dir("calibrate-snapshot-ok");
    let db = Database::open(dir.clone()).expect("db should open");
    let mut acct = account("snap-ok");
    acct.purchase_date = "2026-07-01".into();
    db.create_account(&acct).expect("account should be created");
    finalize_success(&db, "snap-ok", 3.0, Utc::now() - Duration::hours(1));

    let limits = snapshot_limits();
    let usage = db
        .calibrate_account_usage_snapshot(
            "snap-ok",
            &usage_calibration(50.0, 20.0, 10.0, 180, 1_440),
            &limits,
        )
        .expect("snapshot calibrate should save");
    assert_cost(usage.window_5h, 6.0);
    assert_cost(usage.window_week, 6.0);
    assert_cost(usage.window_month, 10.0);
    let remaining_5h =
        (usage.resets_in_5h.expect("5h reset should be set") - Utc::now()).num_minutes();
    assert!(
        (175..=185).contains(&remaining_5h),
        "expected ~180min remaining, got {remaining_5h}"
    );
    let remaining_week =
        (usage.resets_in_week.expect("week reset should be set") - Utc::now()).num_minutes();
    assert!(
        (1_435..=1_445).contains(&remaining_week),
        "expected ~1440min remaining, got {remaining_week}"
    );

    drop(db);
    fs::remove_dir_all(dir).expect("test data dir should be removed");
}

#[test]
fn official_usage_sync_rolls_back_baseline_when_success_metadata_fails() {
    let dir = temp_data_dir("official-sync-atomic-failure");
    let db = Database::open(dir.clone()).expect("db should open");
    let mut acct = account("atomic-sync");
    acct.purchase_date = "2026-07-01".into();
    db.create_account(&acct).expect("account should be created");
    let limits = snapshot_limits();
    db.calibrate_account_usage_snapshot(
        "atomic-sync",
        &usage_calibration(10.0, 20.0, 30.0, 120, 1_200),
        &limits,
    )
    .expect("initial baseline should save");
    let previous_success = Utc::now() - Duration::hours(2);
    db.record_account_usage_sync_success(
        "atomic-sync",
        previous_success,
        previous_success + Duration::hours(24),
        false,
    )
    .expect("initial sync metadata should save");
    let before = db
        .account_usage_with_limits("atomic-sync", &limits)
        .expect("initial usage should load");
    let sync_before = db
        .account_usage_sync_state("atomic-sync")
        .expect("initial sync state should load")
        .expect("sync state should exist");

    db.conn
        .execute_batch(
            "CREATE TRIGGER fail_official_sync_metadata
                 BEFORE UPDATE OF last_success_at ON provider_usage_sync_state
                 WHEN NEW.account_id = 'atomic-sync'
                 BEGIN
                    SELECT RAISE(ABORT, 'forced usage sync metadata failure');
                 END;",
        )
        .expect("failure trigger should install");

    let now = Utc::now();
    let result = db.commit_official_usage_sync_success(
        "atomic-sync",
        "cipher",
        &usage_calibration(80.0, 70.0, 60.0, 180, 1_440),
        &limits,
        AccountUsageSyncSuccessMetadata {
            now,
            next_eligible_at: now + Duration::hours(1),
            mark_expedited: true,
        },
    );
    assert!(
        result.is_err(),
        "forced metadata failure must abort the sync"
    );

    let after = db
        .account_usage_with_limits("atomic-sync", &limits)
        .expect("usage should remain readable");
    assert_cost(after.window_5h, before.window_5h);
    assert_cost(after.window_week, before.window_week);
    assert_cost(after.window_month, before.window_month);
    let sync_after = db
        .account_usage_sync_state("atomic-sync")
        .expect("sync state should load")
        .expect("sync state should exist");
    assert_eq!(sync_after.last_success_at, sync_before.last_success_at);
    assert_eq!(sync_after.next_eligible_at, sync_before.next_eligible_at);
    assert_eq!(sync_after.failure_streak, sync_before.failure_streak);
    assert_eq!(sync_after.last_expedited_at, sync_before.last_expedited_at);

    drop(db);
    fs::remove_dir_all(dir).expect("test data dir should be removed");
}

#[test]
fn calibrate_account_usage_snapshot_rolls_back_when_second_window_fails() {
    let dir = temp_data_dir("calibrate-snapshot-week-fail");
    let db = Database::open(dir.clone()).expect("db should open");
    let mut acct = account("snap-week");
    acct.purchase_date = "2026-07-01".into();
    db.create_account(&acct).expect("account should be created");
    let limits = snapshot_limits();
    db.calibrate_account_usage_snapshot(
        "snap-week",
        &usage_calibration(10.0, 20.0, 30.0, 100, 200),
        &limits,
    )
    .expect("initial snapshot should save");
    let before = usage_offset_row(&db, "snap-week");
    let before_usage = db
        .account_usage_with_limits("snap-week", &limits)
        .expect("usage should load");

    assert!(
        db.calibrate_account_usage_snapshot(
            "snap-week",
            &usage_calibration(80.0, 90.0, 40.0, 180, 10_081),
            &limits
        )
        .is_err(),
        "weekly minutes outside the 7-day window should fail"
    );

    assert_eq!(usage_offset_row(&db, "snap-week"), before);
    let after = db
        .account_usage_with_limits("snap-week", &limits)
        .expect("usage should reload");
    assert_cost(after.window_5h, before_usage.window_5h);
    assert_cost(after.window_week, before_usage.window_week);
    assert_cost(after.window_month, before_usage.window_month);

    drop(db);
    fs::remove_dir_all(dir).expect("test data dir should be removed");
}

#[test]
fn calibrate_account_usage_snapshot_rolls_back_when_third_window_fails() {
    let dir = temp_data_dir("calibrate-snapshot-month-fail");
    let db = Database::open(dir.clone()).expect("db should open");
    let mut acct = account("snap-month");
    acct.purchase_date = "2026-07-01".into();
    db.create_account(&acct).expect("account should be created");
    let limits = snapshot_limits();
    db.calibrate_account_usage_snapshot(
        "snap-month",
        &usage_calibration(10.0, 20.0, 30.0, 100, 200),
        &limits,
    )
    .expect("initial snapshot should save");
    let before = usage_offset_row(&db, "snap-month");
    let before_usage = db
        .account_usage_with_limits("snap-month", &limits)
        .expect("usage should load");

    db.conn
        .execute_batch(
            "CREATE TRIGGER reject_month_calibrate
                 BEFORE UPDATE OF usage_month_window_cost_offset ON credentials
                 WHEN NEW.legacy_account_id = 'snap-month'
                 BEGIN
                     SELECT RAISE(ABORT, 'forced month calibrate failure');
                 END;",
        )
        .expect("failure trigger should be installed");

    assert!(
        db.calibrate_account_usage_snapshot(
            "snap-month",
            &usage_calibration(80.0, 90.0, 40.0, 180, 1_440),
            &limits
        )
        .is_err(),
        "month window trigger should fail the transaction"
    );

    assert_eq!(usage_offset_row(&db, "snap-month"), before);
    let after = db
        .account_usage_with_limits("snap-month", &limits)
        .expect("usage should reload");
    assert_cost(after.window_5h, before_usage.window_5h);
    assert_cost(after.window_week, before_usage.window_week);
    assert_cost(after.window_month, before_usage.window_month);

    drop(db);
    fs::remove_dir_all(dir).expect("test data dir should be removed");
}

#[test]
fn soonest_reset_is_minimum_of_each_accounts_latest_active_cooldown() {
    let dir = temp_data_dir("soonest-account-reset");
    let db = Database::open(dir.clone()).expect("db should open");
    for id in ["first", "second"] {
        db.create_account(&account(id))
            .expect("account should be created");
    }

    let now = Utc::now();
    let first_early = now + Duration::hours(1);
    let first_latest = now + Duration::hours(4);
    let second_latest = now + Duration::hours(2);
    db.set_account_rate_limit(
        "first",
        first_early,
        "5-hour usage limit reached",
        Some(UsageWindowKind::FiveHours),
    )
    .expect("first short cooldown should save");
    db.set_account_rate_limit(
        "first",
        first_latest,
        "weekly usage limit reached",
        Some(UsageWindowKind::Week),
    )
    .expect("first long cooldown should save");
    db.set_account_rate_limit("second", second_latest, "unknown rate limit", None)
        .expect("second cooldown should save");

    let reset = db
        .soonest_cooldown_reset()
        .expect("reset query should work")
        .expect("a reset should exist");
    assert!((reset - second_latest).num_seconds().abs() < 2);

    db.set_account_auth_error("second", Some("upstream auth error 401"))
        .expect("auth breaker should save");
    let reset = db
        .soonest_cooldown_reset()
        .expect("reset query should work")
        .expect("an eligible reset should exist");
    assert!((reset - first_latest).num_seconds().abs() < 2);

    drop(db);
    fs::remove_dir_all(dir).expect("test data dir should be removed");
}

#[test]
fn forward_log_time_filter_compares_rfc3339_offsets_by_instant() {
    let dir = temp_data_dir("forward-log-offset-filter");
    let db = Database::open(dir.clone()).expect("db should open");
    db.conn
        .execute(
            "INSERT INTO forward_logs
                 (timestamp, model, account_id, account_name, status, cost)
                 VALUES (?1, 'inside', 'a', 'a', 'success', 1)",
            ["2026-07-17T04:15:00Z"],
        )
        .expect("inside log should save");
    db.conn
        .execute(
            "INSERT INTO forward_logs
                 (timestamp, model, account_id, account_name, status, cost)
                 VALUES (?1, 'outside', 'a', 'a', 'success', 2)",
            ["2026-07-17T03:30:00Z"],
        )
        .expect("outside log should save");

    let page = db
        .query_forward_logs(ForwardLogQueryOptions {
            limit: 20,
            offset: 0,
            status: None,
            account_id: None,
            provider_id: None,
            route_account_id: None,
            credential_account_id: None,
            model: None,
            key_id: None,
            request_id: None,
            start_time: Some("2026-07-17T12:00:00+08:00"),
            end_time: Some("2026-07-17T12:30:00+08:00"),
            sort_by: Some("cost"),
            sort_order: Some("asc"),
        })
        .expect("offset filter should query");
    assert_eq!(page.summary.total_requests, 1);
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].model, "inside");

    drop(db);
    fs::remove_dir_all(dir).expect("test data dir should be removed");
}

#[test]
fn forward_logs_can_sort_by_attempt() {
    let dir = temp_data_dir("forward-log-attempt-sort");
    let db = Database::open(dir.clone()).expect("db should open");
    for attempt in [2, 1] {
        db.conn
            .execute(
                "INSERT INTO forward_logs
                     (timestamp, model, account_id, account_name, status, cost, attempt)
                     VALUES ('2026-07-23T00:00:00Z', ?1, 'a', 'a', 'client_error', 0, ?2)",
                params![format!("attempt-{attempt}"), attempt],
            )
            .expect("forward log should save");
    }

    let page = db
        .query_forward_logs(ForwardLogQueryOptions {
            limit: 20,
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
            sort_by: Some("attempt"),
            sort_order: Some("asc"),
        })
        .expect("attempt sort should query");
    assert_eq!(
        page.items
            .iter()
            .filter_map(|log| log.attempt)
            .collect::<Vec<_>>(),
        [1, 2]
    );

    drop(db);
    fs::remove_dir_all(dir).expect("test data dir should be removed");
}

fn attributed_log(account_id: &str, key_id: Option<&str>, cost: f64) -> ForwardLog {
    let mut log = forward_log(account_id, "success", cost);
    log.client_key_id = key_id.map(str::to_string);
    log.client_key_name = key_id.map(|id| format!("Key-{id}"));
    log
}

#[test]
fn forward_logs_success_filter_includes_unpriced_rows() {
    let dir = temp_data_dir("forward-success-includes-unpriced");
    let db = Database::open(dir.clone()).unwrap();
    db.log_forward(&forward_log("acct", "success", 1.0))
        .unwrap();
    db.log_forward(&forward_log("acct", "success_unpriced", 2.0))
        .unwrap();
    db.log_forward(&forward_log("acct", "error", 4.0)).unwrap();

    let query = |status: Option<&str>| {
        db.query_forward_logs(ForwardLogQueryOptions {
            status,
            ..empty_forward_query()
        })
        .unwrap()
    };

    let success = query(Some("success"));
    assert_eq!(success.summary.total_requests, 2);
    let mut statuses = success
        .items
        .iter()
        .map(|log| log.status.as_str())
        .collect::<Vec<_>>();
    statuses.sort_unstable();
    assert_eq!(statuses, ["success", "success_unpriced"]);

    let unpriced = query(Some("success_unpriced"));
    assert_eq!(unpriced.summary.total_requests, 1);
    assert_eq!(unpriced.items[0].status, "success_unpriced");

    let errors = query(Some("error"));
    assert_eq!(errors.summary.total_requests, 1);
    assert_eq!(errors.items[0].status, "error");

    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn forward_logs_filter_by_key_and_unattributed_sentinel() {
    let dir = temp_data_dir("forward-key-filter");
    let db = Database::open(dir.clone()).unwrap();
    db.log_forward(&attributed_log("acct", Some("key-a"), 1.0))
        .unwrap();
    db.log_forward(&attributed_log("acct", Some("key-b"), 2.0))
        .unwrap();
    db.log_forward(&attributed_log("acct", None, 4.0)).unwrap();

    let query = |key_id: Option<&str>| {
        db.query_forward_logs(ForwardLogQueryOptions {
            limit: 50,
            offset: 0,
            status: None,
            account_id: None,
            provider_id: None,
            route_account_id: None,
            credential_account_id: None,
            model: None,
            key_id,
            request_id: None,
            start_time: None,
            end_time: None,
            sort_by: Some("cost"),
            sort_order: Some("asc"),
        })
        .unwrap()
    };

    let all = query(None);
    assert_eq!(all.summary.total_requests, 3);
    assert_eq!(all.items.len(), 3);

    let key_a = query(Some("key-a"));
    assert_eq!(key_a.summary.total_requests, 1);
    assert_eq!(key_a.summary.cost, 1.0);
    assert_eq!(key_a.items[0].client_key_id.as_deref(), Some("key-a"));
    assert_eq!(key_a.items[0].client_key_name.as_deref(), Some("Key-key-a"));

    let unattributed = query(Some(UNATTRIBUTED_KEY_FILTER));
    assert_eq!(unattributed.summary.total_requests, 1);
    assert_eq!(unattributed.summary.cost, 4.0);
    assert!(unattributed.items[0].client_key_id.is_none());

    let keys = db.list_forward_log_keys().unwrap();
    assert_eq!(keys.len(), 2);
    assert!(
        keys.iter()
            .any(|key| key.id == "key-a" && key.name == "Key-key-a")
    );
    assert!(keys.iter().any(|key| key.id == "key-b"));

    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn forward_logs_filter_by_provider_attribution_before_pagination() {
    let dir = temp_data_dir("forward-provider-filter");
    let db = Database::open(dir.clone()).unwrap();
    let insert =
        |model: &str, provider_id: &str, route_account_id: &str, credential_account_id: &str| {
            let mut log = forward_log(credential_account_id, "success", 1.0);
            log.model = model.into();
            log.provider_id = Some(provider_id.into());
            log.route_account_id = Some(route_account_id.into());
            log.credential_account_id = Some(credential_account_id.into());
            db.log_forward(&log).unwrap();
        };

    insert("go-a", "opencode", "go-a", "go-a");
    insert("go-b", "opencode", "go-b", "go-b");
    // A Zen route may deliberately debit an OpenCode credential account.
    insert("zen", "opencode-zen-free", "zen-free", "go-a");
    // These newer rows would hide OpenCode Go rows if filtering happened
    // after LIMIT/OFFSET.
    insert("goat-a", "command-code", "goat-a", "goat-a");
    insert("goat-b", "command-code", "goat-b", "goat-b");

    let query = |limit: i64,
                 offset: i64,
                 provider_id: Option<&str>,
                 route_account_id: Option<&str>,
                 credential_account_id: Option<&str>| {
        db.query_forward_logs(ForwardLogQueryOptions {
            limit,
            offset,
            status: None,
            account_id: None,
            provider_id,
            route_account_id,
            credential_account_id,
            model: None,
            key_id: None,
            request_id: None,
            start_time: None,
            end_time: None,
            sort_by: None,
            sort_order: None,
        })
        .unwrap()
    };

    let first_go = query(1, 0, Some("opencode"), None, None);
    assert_eq!(first_go.summary.total_requests, 2);
    assert_eq!(first_go.items[0].model, "go-b");
    let second_go = query(1, 1, Some("opencode"), None, None);
    assert_eq!(second_go.summary.total_requests, 2);
    assert_eq!(second_go.items[0].model, "go-a");

    let routed_zen = query(10, 0, Some("opencode-zen-free"), Some("zen-free"), None);
    assert_eq!(routed_zen.summary.total_requests, 1);
    assert_eq!(routed_zen.items[0].model, "zen");
    assert_eq!(
        routed_zen.items[0].credential_account_id.as_deref(),
        Some("go-a")
    );

    let credential_go_a = query(10, 0, None, None, Some("go-a"));
    assert_eq!(credential_go_a.summary.total_requests, 2);
    assert_eq!(
        credential_go_a
            .items
            .iter()
            .map(|log| log.model.as_str())
            .collect::<Vec<_>>(),
        ["zen", "go-a"]
    );

    let goat = query(10, 0, Some("command-code"), None, None);
    assert_eq!(goat.summary.total_requests, 2);
    assert_eq!(
        goat.items
            .iter()
            .map(|log| log.route_account_id.as_deref())
            .collect::<Vec<_>>(),
        [Some("goat-b"), Some("goat-a")]
    );

    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

fn empty_forward_query<'a>() -> ForwardLogQueryOptions<'a> {
    ForwardLogQueryOptions {
        limit: 50,
        offset: 0,
        status: None,
        account_id: None,
        provider_id: None,
        route_account_id: None,
        credential_account_id: None,
        model: None,
        request_id: None,
        start_time: None,
        end_time: None,
        sort_by: None,
        sort_order: None,
        key_id: None,
    }
}

fn insert_identity_log(
    db: &Database,
    log: ForwardLog,
    requested: Option<&str>,
    alias: Option<&str>,
    upstream: Option<&str>,
) -> i64 {
    let id = db.log_forward(&log).unwrap();
    db.set_forward_log_native_attribution(
        id,
        &ForwardLogNativeAttribution {
            requested_model: requested.map(str::to_string),
            resolved_alias: alias.map(str::to_string),
            upstream_model: upstream.map(str::to_string),
            native_cost_value: None,
            native_cost_unit: None,
            native_cost_currency: None,
        },
    )
    .unwrap();
    id
}

fn clear_v23_identity(db: &Database, id: i64) {
    db.conn
        .execute(
            "UPDATE forward_logs
                 SET requested_model = NULL, resolved_alias = NULL, upstream_model = NULL
                 WHERE id = ?1",
            [id],
        )
        .unwrap();
}

#[test]
fn forward_logs_model_filter_matches_each_identity_and_legacy_fallback() {
    let dir = temp_data_dir("forward-model-identity-filter");
    let db = Database::open(dir.clone()).unwrap();

    let mut legacy = forward_log("acct", "success", 1.0);
    legacy.model = "needle".into();
    legacy.prompt_tokens = 1;
    let legacy_id = db.log_forward(&legacy).unwrap();
    clear_v23_identity(&db, legacy_id);

    let mut requested_only = forward_log("acct", "success", 2.0);
    requested_only.model = "legacy-req".into();
    requested_only.prompt_tokens = 2;
    let requested_id = insert_identity_log(
        &db,
        requested_only,
        Some("needle"),
        Some("alias-req"),
        Some("up-req"),
    );

    let mut alias_only = forward_log("acct", "success", 3.0);
    alias_only.model = "legacy-alias".into();
    alias_only.prompt_tokens = 3;
    let alias_id = insert_identity_log(
        &db,
        alias_only,
        Some("req-alias"),
        Some("needle"),
        Some("up-alias"),
    );

    let mut upstream_only = forward_log("acct", "success", 4.0);
    upstream_only.model = "legacy-up".into();
    upstream_only.prompt_tokens = 4;
    let upstream_id = insert_identity_log(
        &db,
        upstream_only,
        Some("req-up"),
        Some("alias-up"),
        Some("needle"),
    );

    let mut empty_v23 = forward_log("acct", "success", 5.0);
    empty_v23.model = "kept-empty".into();
    empty_v23.prompt_tokens = 5;
    insert_identity_log(&db, empty_v23, Some(""), Some(""), Some(""));

    let mut other = forward_log("acct", "success", 100.0);
    other.model = "other-legacy".into();
    other.prompt_tokens = 100;
    insert_identity_log(
        &db,
        other,
        Some("other-req"),
        Some("other-alias"),
        Some("other-up"),
    );

    let mut overlap = forward_log("acct", "success", 6.0);
    overlap.model = "needle".into();
    overlap.prompt_tokens = 6;
    let overlap_id =
        insert_identity_log(&db, overlap, Some("needle"), Some("needle"), Some("needle"));

    let page = db
        .query_forward_logs(ForwardLogQueryOptions {
            model: Some("needle"),
            sort_by: Some("cost"),
            sort_order: Some("asc"),
            ..empty_forward_query()
        })
        .unwrap();
    let ids = page.items.iter().map(|log| log.id).collect::<Vec<_>>();
    assert_eq!(
        ids,
        [legacy_id, requested_id, alias_id, upstream_id, overlap_id]
    );
    assert_eq!(page.summary.total_requests, 5);
    assert_eq!(page.summary.prompt_tokens, 16);
    assert!((page.summary.cost - 16.0).abs() < f64::EPSILON);

    let requested = db
        .query_forward_logs(ForwardLogQueryOptions {
            model: Some("legacy-req"),
            ..empty_forward_query()
        })
        .unwrap();
    assert_eq!(
        requested.items.iter().map(|log| log.id).collect::<Vec<_>>(),
        [requested_id]
    );

    let alias = db
        .query_forward_logs(ForwardLogQueryOptions {
            model: Some("alias-req"),
            ..empty_forward_query()
        })
        .unwrap();
    assert_eq!(
        alias.items.iter().map(|log| log.id).collect::<Vec<_>>(),
        [requested_id]
    );

    let empty_identity = db
        .query_forward_logs(ForwardLogQueryOptions {
            model: Some("kept-empty"),
            ..empty_forward_query()
        })
        .unwrap();
    assert_eq!(empty_identity.summary.total_requests, 1);
    assert_eq!(empty_identity.items[0].model, "kept-empty");

    let missing = db
        .query_forward_logs(ForwardLogQueryOptions {
            model: Some("missing"),
            ..empty_forward_query()
        })
        .unwrap();
    assert!(missing.items.is_empty());
    assert_eq!(missing.summary.total_requests, 0);

    let substring = db
        .query_forward_logs(ForwardLogQueryOptions {
            model: Some("need"),
            ..empty_forward_query()
        })
        .unwrap();
    assert!(substring.items.is_empty());
    assert_eq!(substring.summary.total_requests, 0);

    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn forward_logs_model_filter_ands_other_filters_before_pagination() {
    let dir = temp_data_dir("forward-model-combo-filter");
    let db = Database::open(dir.clone()).unwrap();
    let inside = DateTime::parse_from_rfc3339("2026-07-17T04:15:00Z")
        .unwrap()
        .with_timezone(&Utc);
    let outside = DateTime::parse_from_rfc3339("2026-07-17T03:30:00Z")
        .unwrap()
        .with_timezone(&Utc);

    let matching = |suffix: &str, cost: f64| {
        let mut log = forward_log("acct", "success", cost);
        log.model = format!("legacy-{suffix}");
        log.provider_id = Some("opencode".into());
        log.client_key_id = Some("key-a".into());
        log.client_key_name = Some("Key-a".into());
        log.timestamp = inside;
        log.prompt_tokens = cost as i64;
        insert_identity_log(
            &db,
            log,
            Some("req-other"),
            Some("needle"),
            Some("up-other"),
        )
    };
    let first = matching("a", 1.0);
    let second = matching("b", 2.0);
    let third = matching("c", 3.0);

    let mut wrong_provider = forward_log("acct", "success", 9.0);
    wrong_provider.model = "legacy-provider".into();
    wrong_provider.provider_id = Some("goat".into());
    wrong_provider.client_key_id = Some("key-a".into());
    wrong_provider.timestamp = inside;
    insert_identity_log(
        &db,
        wrong_provider,
        Some("needle"),
        Some("alias-other"),
        Some("up-other"),
    );

    let mut wrong_key = forward_log("acct", "success", 8.0);
    wrong_key.model = "legacy-key".into();
    wrong_key.provider_id = Some("opencode".into());
    wrong_key.client_key_id = Some("key-b".into());
    wrong_key.timestamp = inside;
    insert_identity_log(&db, wrong_key, None, None, Some("needle"));

    let mut wrong_status = forward_log("acct", "error", 7.0);
    wrong_status.model = "needle".into();
    wrong_status.provider_id = Some("opencode".into());
    wrong_status.client_key_id = Some("key-a".into());
    wrong_status.timestamp = inside;
    let wrong_status_id = db.log_forward(&wrong_status).unwrap();
    clear_v23_identity(&db, wrong_status_id);

    let mut wrong_time = forward_log("acct", "success", 6.0);
    wrong_time.model = "legacy-time".into();
    wrong_time.provider_id = Some("opencode".into());
    wrong_time.client_key_id = Some("key-a".into());
    wrong_time.timestamp = outside;
    insert_identity_log(
        &db,
        wrong_time,
        Some("needle"),
        Some("needle"),
        Some("needle"),
    );

    for index in 0..5 {
        let mut decoy = forward_log("busy", "success", 100.0);
        decoy.model = format!("decoy-{index}");
        decoy.provider_id = Some("opencode".into());
        decoy.client_key_id = Some("key-a".into());
        decoy.timestamp = inside;
        db.log_forward(&decoy).unwrap();
    }

    let filtered = |limit, offset| ForwardLogQueryOptions {
        limit,
        offset,
        status: Some("success"),
        provider_id: Some("opencode"),
        model: Some("needle"),
        key_id: Some("key-a"),
        start_time: Some("2026-07-17T12:00:00+08:00"),
        end_time: Some("2026-07-17T12:30:00+08:00"),
        sort_by: Some("cost"),
        sort_order: Some("asc"),
        ..empty_forward_query()
    };
    let first_page = db.query_forward_logs(filtered(1, 0)).unwrap();
    assert_eq!(first_page.items.len(), 1);
    assert_eq!(first_page.items[0].id, first);
    assert_eq!(first_page.summary.total_requests, 3);
    assert_eq!(first_page.summary.prompt_tokens, 6);
    assert!((first_page.summary.cost - 6.0).abs() < f64::EPSILON);

    let second_page = db.query_forward_logs(filtered(1, 1)).unwrap();
    assert_eq!(second_page.items.len(), 1);
    assert_eq!(second_page.items[0].id, second);
    assert_eq!(second_page.summary.total_requests, 3);

    let rest = db.query_forward_logs(filtered(50, 2)).unwrap();
    assert_eq!(
        rest.items.iter().map(|log| log.id).collect::<Vec<_>>(),
        [third]
    );

    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn backfill_attributes_null_rows_in_chunks_with_resume_and_completion() {
    let dir = temp_data_dir("backfill-chunks");
    let db = Database::open(dir.clone()).unwrap();
    for index in 0..7 {
        let mut log = forward_log("acct", "success", index as f64);
        log.client_key_id = (index % 2 == 0).then(|| "already-set".to_string());
        db.log_forward(&log).unwrap();
    }

    // Chunk size 3 covers rowids 1..=7 in three steps; already-attributed
    // rows must never be overwritten by the range update.
    assert!(
        db.backfill_forward_logs_client_key_step("primary", "Primary", 3)
            .unwrap()
    );
    assert!(
        db.backfill_forward_logs_client_key_step("primary", "Primary", 3)
            .unwrap()
    );
    // The final chunk exactly reaches max rowid and records completion
    // in the same call; a further step is a no-op.
    assert!(
        !db.backfill_forward_logs_client_key_step("primary", "Primary", 3)
            .unwrap()
    );
    assert!(
        !db.backfill_forward_logs_client_key_step("primary", "Primary", 3)
            .unwrap()
    );
    assert_eq!(
        db.forward_log_backfill_marker().unwrap().as_deref(),
        Some(BACKFILL_DONE)
    );

    let rows = db.list_forward_logs(100).unwrap();
    assert_eq!(rows.len(), 7);
    for (index, row) in rows.iter().rev().enumerate() {
        if index % 2 == 0 {
            assert_eq!(row.client_key_id.as_deref(), Some("already-set"));
        } else {
            assert_eq!(row.client_key_id.as_deref(), Some("primary"));
            assert_eq!(row.client_key_name.as_deref(), Some("Primary"));
        }
    }

    // New NULL rows written by an older binary (a downgrade window)
    // restart the scan instead of staying "unattributed" forever.
    for cost in [9.0, 11.0] {
        db.log_forward(&forward_log("acct", "success", cost))
            .unwrap();
    }
    assert!(
        db.backfill_forward_logs_client_key_step("primary", "Primary", 3)
            .unwrap()
    );
    while db
        .backfill_forward_logs_client_key_step("primary", "Primary", 3)
        .unwrap()
    {}
    assert_eq!(
        db.forward_log_backfill_marker().unwrap().as_deref(),
        Some(BACKFILL_DONE)
    );
    let late_rows: Vec<_> = db
        .list_forward_logs(100)
        .unwrap()
        .into_iter()
        .filter(|row| row.client_key_name.as_deref() == Some("Primary"))
        .collect();
    assert_eq!(late_rows.len(), 5);
    for cost in [9.0, 11.0] {
        assert!(late_rows.iter().any(|row| row.cost == Some(cost)));
    }

    // A late row already carrying an attribution must not restart the scan.
    let mut attributed = forward_log("acct", "success", 13.0);
    attributed.client_key_id = Some("primary".into());
    attributed.client_key_name = Some("Primary".into());
    db.log_forward(&attributed).unwrap();
    assert!(
        !db.backfill_forward_logs_client_key_step("primary", "Primary", 50)
            .unwrap()
    );

    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn backfill_resumes_from_persisted_watermark_after_interruption() {
    let dir = temp_data_dir("backfill-resume");
    let db = Database::open(dir.clone()).unwrap();
    for index in 0..5 {
        db.log_forward(&forward_log("acct", "success", index as f64))
            .unwrap();
    }

    // Simulate a crash after the first chunk: the watermark persists but
    // the remaining rows are still NULL.
    assert!(
        db.backfill_forward_logs_client_key_step("primary", "Primary", 2)
            .unwrap()
    );
    assert_eq!(
        db.forward_log_backfill_marker().unwrap().as_deref(),
        Some("2")
    );
    let partial = db.list_forward_logs(100).unwrap();
    assert_eq!(
        partial
            .iter()
            .filter(|row| row.client_key_id.is_some())
            .count(),
        2
    );

    drop(db);
    let db = Database::open(dir.clone()).unwrap();
    assert_eq!(
        db.forward_log_backfill_marker().unwrap().as_deref(),
        Some("2")
    );

    // A restarted run continues from the watermark instead of
    // rescanning; the last chunk completes the table and records done.
    assert!(
        db.backfill_forward_logs_client_key_step("primary", "Primary", 2)
            .unwrap()
    );
    assert!(
        !db.backfill_forward_logs_client_key_step("primary", "Primary", 2)
            .unwrap()
    );
    assert_eq!(
        db.forward_log_backfill_marker().unwrap().as_deref(),
        Some(BACKFILL_DONE)
    );
    let rows = db.list_forward_logs(100).unwrap();
    assert!(
        rows.iter()
            .all(|row| row.client_key_id.as_deref() == Some("primary"))
    );
    // No row was attributed twice: costs and row counts are unchanged.
    assert_eq!(
        rows.iter().map(|row| row.cost.unwrap_or(0.0)).sum::<f64>() as i64,
        10
    );

    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn backfill_completes_inline_for_empty_tables() {
    let dir = temp_data_dir("backfill-empty");
    let db = Database::open(dir.clone()).unwrap();
    assert!(
        !db.backfill_forward_logs_client_key_step("primary", "Primary", 50_000)
            .unwrap()
    );
    assert_eq!(
        db.forward_log_backfill_marker().unwrap().as_deref(),
        Some(BACKFILL_DONE)
    );

    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn new_accounts_have_safe_usage_sync_defaults() {
    let dir = temp_data_dir("v21-usage-sync");
    let db = Database::open(dir.clone()).unwrap();
    let account = account("sync-defaults");
    db.create_account(&account).unwrap();
    let sync = db
        .account_usage_sync_state("sync-defaults")
        .unwrap()
        .unwrap();
    assert!(sync.last_success_at.is_none());
    assert!(sync.last_attempt_at.is_none());
    assert!(sync.next_eligible_at.is_none());
    assert_eq!(sync.failure_streak, 0);
    assert!(sync.last_expedited_at.is_none());

    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn fresh_go_accounts_project_live_provider_quota_windows() {
    let dir = temp_data_dir("fresh-provider-quota");
    let db = Database::open(dir.clone()).unwrap();
    db.create_account(&account("fresh-go")).unwrap();
    assert!(db.list_quota_windows("fresh-go").unwrap().is_empty());

    let limits = SEED_LIMITS;
    db.calibrate_account_usage(
        "fresh-go",
        UsageWindowKind::FiveHours,
        50.0,
        Some(180),
        limits.window_5h,
    )
    .unwrap();
    let windows = db
        .live_opencode_go_quota_windows("fresh-go", &limits)
        .unwrap();
    assert_eq!(windows.len(), 3);
    let rolling = windows
        .iter()
        .find(|window| window.window_kind == QUOTA_WINDOW_FIVE_HOURS)
        .unwrap();
    assert!((rolling.used - limits.window_5h * 0.5).abs() < 1e-9);
    assert_eq!(rolling.limit_value, Some(limits.window_5h));
    assert_eq!(rolling.source, "opencode-go-live");

    drop(db);
    fs::remove_dir_all(dir).unwrap();
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
    assert_eq!(schema_version_on(&pre_v23_backup).unwrap(), 21);
    drop(pre_v23_backup);
    let backup_path = &backups_before[0];
    let pre_v3 = pre_v3_backup_paths(&dir);
    assert_eq!(pre_v3.len(), 1);
    let pre_v3_backup =
        Connection::open_with_flags(&pre_v3[0], OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    assert_eq!(
        schema_version_on(&pre_v3_backup).unwrap(),
        V26_SCHEMA_VERSION
    );
    drop(pre_v3_backup);

    let backup = Connection::open_with_flags(backup_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .expect("backup should open read-only");
    assert_eq!(schema_version_on(&backup).unwrap(), 21);
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
    assert_eq!(schema_version_on(&backup).unwrap(), 20);
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
fn sub_gateway_key_crud_and_unique_index_backstop() {
    let dir = temp_data_dir("sub-keys-crud");
    let db = Database::open(dir.clone()).unwrap();
    let now = Utc::now();
    let key = SubGatewayKey {
        id: "sub-1".into(),
        name: "Laptop".into(),
        key: "ocg-laptop".into(),
        enabled: true,
        deleted_at: None,
        created_at: now,
    };
    db.insert_sub_gateway_key(&key).unwrap();
    assert_eq!(db.count_active_sub_gateway_keys().unwrap(), 1);
    let primary_value = db.primary_access_key_value().unwrap().unwrap();
    let collide_primary = SubGatewayKey {
        id: "sub-primary-collide".into(),
        name: "Collide".into(),
        key: primary_value,
        enabled: true,
        deleted_at: None,
        created_at: now,
    };
    assert!(db.insert_sub_gateway_key(&collide_primary).is_err());
    assert_eq!(
        db.list_active_sub_gateway_keys().unwrap(),
        vec![key.clone()]
    );
    assert_eq!(db.list_sub_gateway_keys().unwrap().len(), 1);

    // Duplicate active values are rejected by the partial unique index.
    let duplicate = SubGatewayKey {
        id: "sub-2".into(),
        name: "Twin".into(),
        key: "ocg-laptop".into(),
        enabled: true,
        deleted_at: None,
        created_at: now,
    };
    assert!(db.insert_sub_gateway_key(&duplicate).is_err());

    // Disabled keys keep their plaintext, so they still block duplicates.
    assert!(db.set_sub_gateway_key_enabled("sub-1", false).unwrap());
    assert!(db.insert_sub_gateway_key(&duplicate).is_err());
    assert!(db.sub_gateway_key_value_exists("ocg-laptop").unwrap());
    assert_eq!(
        db.active_sub_gateway_key_values().unwrap(),
        vec!["ocg-laptop".to_string()]
    );

    // Renaming and regenerating address only non-deleted rows.
    assert!(db.rename_sub_gateway_key("sub-1", "Deck").unwrap());
    assert!(
        db.update_sub_gateway_key_value("sub-1", "ocg-deck")
            .unwrap()
    );
    assert!(db.sub_gateway_key_value_exists("ocg-deck").unwrap());

    // Soft delete clears the plaintext; tombstones free the value and do
    // not count as active.
    assert!(db.soft_delete_sub_gateway_key("sub-1", now).unwrap());
    let tombstone = db.get_sub_gateway_key("sub-1").unwrap().unwrap();
    assert!(tombstone.deleted_at.is_some());
    assert!(tombstone.key.is_empty());
    assert!(!tombstone.enabled);
    assert_eq!(db.count_active_sub_gateway_keys().unwrap(), 0);
    assert!(!db.sub_gateway_key_value_exists("ocg-deck").unwrap());
    assert!(!db.rename_sub_gateway_key("sub-1", "Gone").unwrap());
    assert!(!db.set_sub_gateway_key_enabled("sub-1", true).unwrap());
    assert!(!db.soft_delete_sub_gateway_key("sub-1", now).unwrap());

    // The freed value is insertable again, and missing ids report false.
    let recycled = SubGatewayKey {
        id: "sub-3".into(),
        name: "Recycled".into(),
        key: "ocg-deck".into(),
        enabled: true,
        deleted_at: None,
        created_at: now,
    };
    db.insert_sub_gateway_key(&recycled).unwrap();
    assert!(!db.rename_sub_gateway_key("missing", "X").unwrap());
    assert!(db.get_sub_gateway_key("missing").unwrap().is_none());

    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn forward_log_keys_resolve_the_latest_name_per_id() {
    let dir = temp_data_dir("log-keys-latest-name");
    let db = Database::open(dir.clone()).unwrap();
    let base = ForwardLog {
        model: "m".into(),
        client_key_id: Some("sub-1".into()),
        client_key_name: Some("Laptop".into()),
        cost: None,
        raw_cost_usd: None,
        quota_debit: None,
        effective_paid_cost_usd: None,
        cost_state: "not_applicable".into(),
        ..forward_log("a", "success", 0.0)
    };
    // A lexicographically "larger" historical name must not win: it was
    // written first, the current name last.
    let mut zzz = base.clone();
    zzz.client_key_name = Some("zzz-old".into());
    db.log_forward(&zzz).unwrap();
    db.log_forward(&base).unwrap();
    let mut renamed = base.clone();
    renamed.client_key_name = Some("Deck".into());
    db.log_forward(&renamed).unwrap();

    let keys = db.list_forward_log_keys().unwrap();
    assert_eq!(keys.len(), 1, "one entry per distinct key id");
    assert_eq!(keys[0].id, "sub-1");
    assert_eq!(keys[0].name, "Deck", "the latest snapshot wins");

    // NULL-name rows fall back to the id label.
    let mut unnamed = base.clone();
    unnamed.client_key_id = Some("ghost".into());
    unnamed.client_key_name = None;
    db.log_forward(&unnamed).unwrap();
    let keys = db.list_forward_log_keys().unwrap();
    assert_eq!(keys.len(), 2);
    assert_eq!(
        keys.iter().find(|key| key.id == "ghost").unwrap().name,
        "ghost"
    );

    // The list stays purely log-driven: an id with no rows (e.g. the
    // primary key before its first attributed request) never appears.
    assert!(
        !keys
            .iter()
            .any(|key| key.id == "00000000-0000-0000-0000-000000000001")
    );

    // A primary-attributed row with a NULL name resolves to the fixed
    // display name, never the raw id constant.
    let mut unnamed_primary = base.clone();
    unnamed_primary.client_key_id = Some("00000000-0000-0000-0000-000000000001".into());
    unnamed_primary.client_key_name = None;
    db.log_forward(&unnamed_primary).unwrap();
    let keys = db.list_forward_log_keys().unwrap();
    let primary = keys
        .iter()
        .find(|key| key.id == "00000000-0000-0000-0000-000000000001")
        .unwrap();
    assert_eq!(primary.name, "Primary");

    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

fn create_v22_fixture(dir: &Path) {
    let db = Database::open(dir.to_path_buf()).expect("fixture database should open");
    let mut v22_account = account("v22-account");
    v22_account.key_cipher = fixture_account_key_cipher();
    db.create_account(&v22_account)
        .expect("representative account should save");
    let mut goat = account("v22-goat");
    goat.key_cipher = fixture_account_key_cipher();
    goat.provider_id = COMMAND_CODE_PROVIDER_ID.to_string();
    goat.enabled = false;
    db.create_account(&goat)
        .expect("representative GOAT account should save");
    db.log_forward(&forward_log("v22-account", "success", 3.5))
        .expect("representative forward log should save");
    drop(db);
    reverse_current_to_v34(dir);

    let conn = Connection::open(dir.join("data.sqlite")).expect("v23 fixture should reopen");
    conn.execute_batch(
        "PRAGMA foreign_keys=OFF;
             DROP TRIGGER IF EXISTS access_keys_protect_primary_delete;
             DROP TABLE IF EXISTS access_keys;
             CREATE TABLE IF NOT EXISTS sub_gateway_keys (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                key TEXT NOT NULL,
                enabled INTEGER NOT NULL DEFAULT 1,
                deleted_at TEXT,
                created_at TEXT NOT NULL
             );
             CREATE UNIQUE INDEX IF NOT EXISTS idx_sub_gateway_keys_key
                ON sub_gateway_keys(key) WHERE deleted_at IS NULL AND key <> '';
             DROP INDEX IF EXISTS idx_account_model_capabilities_account;
             DROP TABLE IF EXISTS account_custom_configs;
             DROP TABLE IF EXISTS account_model_capabilities;
             ALTER TABLE accounts DROP COLUMN verification_error;
             ALTER TABLE accounts DROP COLUMN connection_verified_at;
             ALTER TABLE accounts DROP COLUMN verification_status;
             ALTER TABLE forward_logs DROP COLUMN native_cost_currency;
             ALTER TABLE forward_logs DROP COLUMN native_cost_unit;
             ALTER TABLE forward_logs DROP COLUMN native_cost_value;
             ALTER TABLE forward_logs DROP COLUMN upstream_model;
             ALTER TABLE forward_logs DROP COLUMN resolved_alias;
             ALTER TABLE forward_logs DROP COLUMN requested_model;
             DELETE FROM schema_version;
             INSERT INTO schema_version (version) VALUES (22);
             UPDATE accounts SET enabled = 1 WHERE id = 'v22-goat';
             PRAGMA foreign_keys=ON;",
    )
    .expect("v22 fixture should be created");
    restore_usage_sync_account_columns(&conn);
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
    assert_eq!(schema_version_on(&backup).unwrap(), 22);
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
    assert_eq!(schema_version_on(&conn).unwrap(), too_new);
    drop(conn);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn zen_free_model_catalog_survives_reopen() {
    let dir = temp_data_dir("zen-free-model-catalog");
    let refreshed_at = Utc::now();
    {
        let db = Database::open(dir.clone()).unwrap();
        db.set_zen_free_model_catalog(&crate::kernel::zen::ZenFreeModelCatalog {
            models: vec!["persisted-coder-free".into()],
            refreshed_at: Some(refreshed_at),
            source_url: crate::kernel::zen::ZEN_MODELS_SOURCE_URL.into(),
        })
        .unwrap();
    }
    {
        let db = Database::open(dir.clone()).unwrap();
        let catalog = db.zen_free_model_catalog().unwrap().unwrap();
        assert_eq!(catalog.models, ["persisted-coder-free"]);
        assert_eq!(catalog.refreshed_at, Some(refreshed_at));
    }
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
        assert_eq!(schema_version_on(&conn).unwrap(), 25);
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
fn provider_and_custom_contract_scopes_are_isolated() {
    let dir = temp_data_dir("v26-scope-isolation");
    let db = Database::open(dir.clone()).unwrap();
    let now = Utc::now();
    let go = ContractScope::provider(OPENCODE_PROVIDER_ID);
    let custom = ContractScope::custom_endpoint("custom-a");
    db.upsert_model_protocol(&PersistedModelProtocol {
        scope: go.clone(),
        model_id: "glm-5.2".into(),
        protocol: UpstreamProtocolKind::ChatCompletions,
        source: ContractEvidenceSource::ProbeConfirmed,
        verified_at: Some(now),
        observed_at: Some(now),
        last_probe_result: Some(ProbeResultKind::Success),
        last_probe_at: Some(now),
        last_probe_error: None,
    })
    .unwrap();
    db.upsert_model_protocol(&PersistedModelProtocol {
        scope: custom.clone(),
        model_id: "local-model".into(),
        protocol: UpstreamProtocolKind::ChatCompletions,
        source: ContractEvidenceSource::Preset,
        verified_at: Some(now),
        observed_at: Some(now),
        last_probe_result: None,
        last_probe_at: None,
        last_probe_error: None,
    })
    .unwrap();
    db.set_model_protocol_overrides(
        &go,
        &[(
            "glm-5.2".into(),
            UpstreamProtocolKind::Messages,
            ProtocolOverrideState::ForceOff,
        )],
        now,
    )
    .unwrap();
    let persisted = db.load_persisted_contracts().unwrap();
    assert!(
        persisted
            .evidence
            .get(&go)
            .unwrap()
            .iter()
            .any(|row| row.model_id == "glm-5.2")
    );
    assert!(
        persisted
            .evidence
            .get(&custom)
            .unwrap()
            .iter()
            .all(|row| row.model_id != "glm-5.2")
    );
    assert!(
        persisted
            .overrides
            .get(&go)
            .unwrap()
            .iter()
            .any(|row| row.model_id == "glm-5.2" && row.state == ProtocolOverrideState::ForceOff)
    );
    assert!(
        persisted
            .overrides
            .get(&custom)
            .map(|rows| rows.iter().all(|row| row.model_id != "glm-5.2"))
            .unwrap_or(true)
    );
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn probe_evidence_and_catalog_mutations_advance_scope_revision_atomically() {
    let dir = temp_data_dir("v26-revision-bump");
    let db = Database::open(dir.clone()).unwrap();
    let now = Utc::now();
    let scope = ContractScope::provider(OPENCODE_PROVIDER_ID);
    assert!(db.load_persisted_scope(&scope).unwrap().is_none());

    let success = db
        .upsert_model_protocol(&PersistedModelProtocol {
            scope: scope.clone(),
            model_id: "grok-4.5".into(),
            protocol: UpstreamProtocolKind::ChatCompletions,
            source: ContractEvidenceSource::ProbeConfirmed,
            verified_at: Some(now),
            observed_at: Some(now),
            last_probe_result: Some(ProbeResultKind::Success),
            last_probe_at: Some(now),
            last_probe_error: None,
        })
        .unwrap();
    assert_eq!(success.revision, 2);
    let after_success = db.load_persisted_scope(&scope).unwrap().unwrap();
    assert_eq!(after_success.revision, 2);

    let failure = db
        .upsert_model_protocol(&PersistedModelProtocol {
            scope: scope.clone(),
            model_id: "grok-4.5".into(),
            protocol: UpstreamProtocolKind::Messages,
            source: ContractEvidenceSource::ProbeObserved,
            verified_at: None,
            observed_at: Some(now),
            last_probe_result: Some(ProbeResultKind::Failure),
            last_probe_at: Some(now),
            last_probe_error: Some("upstream 500".into()),
        })
        .unwrap();
    assert_eq!(failure.revision, 3);

    let catalog = db
        .set_contract_catalog(
            &scope,
            &["grok-4.5".into()],
            Some(now),
            crate::provider_contracts::CATALOG_SOURCE_STATIC,
            "",
            now,
        )
        .unwrap();
    assert_eq!(catalog.revision, 4);

    db.conn
        .execute_batch(
            "CREATE TRIGGER fail_probe_write BEFORE INSERT ON provider_contract_model_protocols
                 BEGIN SELECT RAISE(ABORT, 'injected write failure'); END;",
        )
        .unwrap();
    let before_failed = db.load_persisted_scope(&scope).unwrap().unwrap().revision;
    let failed = db.upsert_model_protocol(&PersistedModelProtocol {
        scope: scope.clone(),
        model_id: "glm-5.3".into(),
        protocol: UpstreamProtocolKind::Responses,
        source: ContractEvidenceSource::ProbeObserved,
        verified_at: None,
        observed_at: Some(now),
        last_probe_result: Some(ProbeResultKind::Failure),
        last_probe_at: Some(now),
        last_probe_error: Some("should roll back".into()),
    });
    assert!(failed.is_err());
    let after_failed = db.load_persisted_scope(&scope).unwrap().unwrap();
    assert_eq!(after_failed.revision, before_failed);
    assert!(
        db.load_model_protocol(&scope, "glm-5.3", UpstreamProtocolKind::Responses)
            .unwrap()
            .is_none()
    );

    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

fn probe_observation(
    scope: ContractScope,
    model_id: &str,
    protocol: UpstreamProtocolKind,
    now: DateTime<Utc>,
) -> PersistedModelProtocol {
    PersistedModelProtocol {
        scope,
        model_id: model_id.into(),
        protocol,
        source: ContractEvidenceSource::ProbeConfirmed,
        verified_at: Some(now),
        observed_at: Some(now),
        last_probe_result: Some(ProbeResultKind::Success),
        last_probe_at: Some(now),
        last_probe_error: None,
    }
}

#[test]
fn probe_observation_batch_upserts_atomically_and_bumps_scope_once() {
    let dir = temp_data_dir("v26-probe-batch");
    let db = Database::open(dir.clone()).unwrap();
    let now = Utc::now();
    let go = ContractScope::provider(OPENCODE_PROVIDER_ID);
    let custom = ContractScope::custom_endpoint("custom-a");

    let empty = db.upsert_model_protocols(&[]);
    assert!(empty.is_err(), "{empty:?}");
    assert!(db.load_persisted_scope(&go).unwrap().is_none());

    let mixed = db.upsert_model_protocols(&[
        probe_observation(
            go.clone(),
            "grok-4.5",
            UpstreamProtocolKind::ChatCompletions,
            now,
        ),
        probe_observation(
            custom.clone(),
            "local-model",
            UpstreamProtocolKind::ChatCompletions,
            now,
        ),
    ]);
    assert!(mixed.is_err(), "{mixed:?}");
    assert!(db.load_persisted_scope(&go).unwrap().is_none());
    assert!(db.load_persisted_scope(&custom).unwrap().is_none());
    assert!(
        db.load_model_protocol(&go, "grok-4.5", UpstreamProtocolKind::ChatCompletions)
            .unwrap()
            .is_none()
    );

    let persisted = db
        .upsert_model_protocols(&[
            probe_observation(
                go.clone(),
                "grok-4.5",
                UpstreamProtocolKind::ChatCompletions,
                now,
            ),
            probe_observation(go.clone(), "grok-4.5", UpstreamProtocolKind::Responses, now),
        ])
        .unwrap();
    assert_eq!(persisted.revision, 2);
    let after = db.load_persisted_scope(&go).unwrap().unwrap();
    assert_eq!(after.revision, 2);
    assert!(
        db.load_model_protocol(&go, "grok-4.5", UpstreamProtocolKind::ChatCompletions)
            .unwrap()
            .is_some()
    );
    assert!(
        db.load_model_protocol(&go, "grok-4.5", UpstreamProtocolKind::Responses)
            .unwrap()
            .is_some()
    );

    db.conn
        .execute_batch(
            "CREATE TRIGGER fail_second_probe_observation_write
                 BEFORE INSERT ON provider_contract_model_protocols
                 WHEN NEW.protocol = 'messages'
                 BEGIN SELECT RAISE(ABORT, 'injected second observation write failure'); END;",
        )
        .unwrap();
    let before_failed = db.load_persisted_scope(&go).unwrap().unwrap().revision;
    let failed = db.upsert_model_protocols(&[
        probe_observation(
            go.clone(),
            "glm-5.3",
            UpstreamProtocolKind::ChatCompletions,
            now,
        ),
        probe_observation(go.clone(), "glm-5.3", UpstreamProtocolKind::Messages, now),
    ]);
    assert!(failed.is_err(), "{failed:?}");
    assert_eq!(
        db.load_persisted_scope(&go).unwrap().unwrap().revision,
        before_failed
    );
    assert!(
        db.load_model_protocol(&go, "glm-5.3", UpstreamProtocolKind::ChatCompletions)
            .unwrap()
            .is_none()
    );
    assert!(
        db.load_model_protocol(&go, "glm-5.3", UpstreamProtocolKind::Messages)
            .unwrap()
            .is_none()
    );

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
fn forward_logs_dual_write_native_usd_attribution() {
    let dir = temp_data_dir("v23-native-logs");
    let db = Database::open(dir.clone()).unwrap();
    db.create_account(&account("priced")).unwrap();
    let mut log = forward_log("priced", "success", 1.25);
    log.raw_cost_usd = Some(1.25);
    log.cost_state = "priced".into();
    let id = db.log_forward(&log).unwrap();
    let attribution = db.forward_log_native_attribution(id).unwrap().unwrap();
    assert_eq!(attribution.native_cost_value, Some(1.25));
    assert_eq!(attribution.native_cost_unit.as_deref(), Some("usd"));
    assert_eq!(attribution.native_cost_currency.as_deref(), Some("USD"));
    assert_eq!(attribution.upstream_model.as_deref(), Some("test"));

    db.set_forward_log_native_attribution(
        id,
        &ForwardLogNativeAttribution {
            requested_model: Some("deepseek-v4-flash".into()),
            resolved_alias: Some("deepseek-v4-flash".into()),
            upstream_model: Some("deepseek/deepseek-v4-flash".into()),
            native_cost_value: Some(12.0),
            native_cost_unit: Some("credits".into()),
            native_cost_currency: None,
        },
    )
    .unwrap();
    let updated = db.forward_log_native_attribution(id).unwrap().unwrap();
    assert_eq!(
        updated.upstream_model.as_deref(),
        Some("deepseek/deepseek-v4-flash")
    );
    assert_eq!(updated.native_cost_unit.as_deref(), Some("credits"));

    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn update_forward_log_finalizes_native_usd_with_cost_fields() {
    let dir = temp_data_dir("v23-native-finalize");
    let db = Database::open(dir.clone()).unwrap();
    db.create_account(&account("stream")).unwrap();

    let mut streaming = forward_log("stream", "streaming", 0.0);
    streaming.cost = None;
    streaming.raw_cost_usd = None;
    streaming.cost_state = "not_applicable".into();
    streaming.provider_id = Some(OPENCODE_PROVIDER_ID.to_string());
    let streaming_id = db.log_forward(&streaming).unwrap();
    let preliminary = db
        .forward_log_native_attribution(streaming_id)
        .unwrap()
        .unwrap();
    assert_eq!(preliminary.native_cost_value, None);
    assert_eq!(preliminary.native_cost_unit, None);

    db.update_forward_log(
        streaming_id,
        "success",
        Some(200),
        ForwardMetrics {
            cost: 1.25,
            raw_cost_usd: Some(1.25),
            pricing_provider_id: Some(OPENCODE_PROVIDER_ID.to_string()),
            cost_state: "priced",
            ..ForwardMetrics::default()
        },
        None,
        None,
    )
    .unwrap();
    let finalized = db
        .forward_log_native_attribution(streaming_id)
        .unwrap()
        .unwrap();
    assert_eq!(finalized.native_cost_value, Some(1.25));
    assert_eq!(finalized.native_cost_unit.as_deref(), Some("usd"));
    assert_eq!(finalized.native_cost_currency.as_deref(), Some("USD"));
    let stored_cost: f64 = db
        .conn
        .query_row(
            "SELECT cost FROM forward_logs WHERE id = ?1",
            [streaming_id],
            |row| row.get(0),
        )
        .unwrap();
    assert!((stored_cost - 1.25).abs() < 1e-9);

    let zero_id = db
        .log_forward(&forward_log("stream", "streaming", 0.0))
        .unwrap();
    assert_eq!(
        db.forward_log_native_attribution(zero_id)
            .unwrap()
            .unwrap()
            .native_cost_value,
        Some(0.0)
    );
    db.update_forward_log(
        zero_id,
        "success",
        None,
        ForwardMetrics {
            cost: 2.5,
            raw_cost_usd: Some(2.5),
            cost_state: "priced",
            ..ForwardMetrics::default()
        },
        None,
        None,
    )
    .unwrap();
    assert_eq!(
        db.forward_log_native_attribution(zero_id)
            .unwrap()
            .unwrap()
            .native_cost_value,
        Some(2.5)
    );

    let mut zen = forward_log("stream", "streaming", 0.0);
    zen.cost = None;
    zen.raw_cost_usd = None;
    zen.cost_state = "not_applicable".into();
    zen.provider_id = Some(OPENCODE_ZEN_FREE_PROVIDER_ID.to_string());
    let zen_id = db.log_forward(&zen).unwrap();
    db.update_forward_log(
        zen_id,
        "success",
        Some(200),
        ForwardMetrics {
            cost: 1.0,
            raw_cost_usd: Some(1.0),
            cost_state: "priced",
            ..ForwardMetrics::default()
        },
        None,
        None,
    )
    .unwrap();
    let zen_native = db.forward_log_native_attribution(zen_id).unwrap().unwrap();
    assert_eq!(zen_native.native_cost_value, Some(0.0));
    assert_eq!(zen_native.native_cost_unit.as_deref(), Some("usd"));
    let zen_cost: (f64, String) = db
        .conn
        .query_row(
            "SELECT cost, cost_state FROM forward_logs WHERE id = ?1",
            [zen_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(zen_cost.0, 0.0);
    assert_eq!(zen_cost.1, "free");

    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn create_account_with_contract_is_atomic_on_custom_config_failure() {
    let dir = temp_data_dir("v23-atomic-create");
    let db = Database::open(dir.clone()).unwrap();
    let mut custom = account("custom-atomic");
    custom.provider_id = CUSTOM_PROVIDER_ID.to_string();
    custom.enabled = false;
    db.conn
        .execute_batch(
            "CREATE TRIGGER fail_custom_config
                 BEFORE INSERT ON destinations
                 WHEN NEW.legacy_kind = 'custom_account'
                 BEGIN
                     SELECT RAISE(ABORT, 'forced custom config failure');
                 END;",
        )
        .unwrap();

    let error = db
        .create_account_with_contract(
            &custom,
            Some(&AccountCustomConfigInput {
                endpoint_url: "https://api.example.com/v1/chat/completions".into(),
                upstream_protocol: UpstreamProtocolKind::ChatCompletions,
            }),
            &[AccountModelCapabilityInput {
                public_model: "org/model".into(),
                upstream_model: "org/model".into(),
                protocol: UpstreamProtocolKind::ChatCompletions,
                source: None,
            }],
        )
        .expect_err("forced custom config failure should abort the create");
    assert!(
        error.to_string().contains("forced custom config failure"),
        "{error}"
    );
    assert!(db.get_account("custom-atomic").unwrap().is_none());
    assert!(db.account_custom_config("custom-atomic").unwrap().is_none());
    assert!(
        db.list_account_model_capabilities("custom-atomic")
            .unwrap()
            .is_empty()
    );

    db.conn
        .execute_batch("DROP TRIGGER fail_custom_config;")
        .unwrap();
    db.create_account_with_contract(
        &custom,
        Some(&AccountCustomConfigInput {
            endpoint_url: "https://api.example.com/v1/chat/completions".into(),
            upstream_protocol: UpstreamProtocolKind::ChatCompletions,
        }),
        &[AccountModelCapabilityInput {
            public_model: "org/model".into(),
            upstream_model: "org/model".into(),
            protocol: UpstreamProtocolKind::ChatCompletions,
            source: None,
        }],
    )
    .unwrap();
    assert!(db.get_account("custom-atomic").unwrap().is_some());
    assert!(db.account_custom_config("custom-atomic").unwrap().is_some());

    let mut go = account("go-rejects-custom");
    let rejected = db.create_account_with_contract(
        &go,
        Some(&AccountCustomConfigInput {
            endpoint_url: "https://api.example.com/v1/chat/completions".into(),
            upstream_protocol: UpstreamProtocolKind::ChatCompletions,
        }),
        &[],
    );
    assert!(rejected.is_err(), "non-Custom accounts must reject config");
    assert!(db.get_account("go-rejects-custom").unwrap().is_none());

    go.id = "go-rejects-caps".into();
    go.name = "go-rejects-caps".into();
    let rejected_caps = db.create_account_with_contract(
        &go,
        None,
        &[AccountModelCapabilityInput {
            public_model: "org/model".into(),
            upstream_model: "org/model".into(),
            protocol: UpstreamProtocolKind::ChatCompletions,
            source: None,
        }],
    );
    assert!(
        rejected_caps.is_err(),
        "non-Custom accounts must reject capabilities"
    );
    assert!(db.get_account("go-rejects-caps").unwrap().is_none());

    let mut custom_empty = account("custom-empty-caps");
    custom_empty.provider_id = CUSTOM_PROVIDER_ID.to_string();
    custom_empty.enabled = false;
    let empty_caps = db.create_account_with_contract(
        &custom_empty,
        Some(&AccountCustomConfigInput {
            endpoint_url: "https://api.example.com/v1/chat/completions".into(),
            upstream_protocol: UpstreamProtocolKind::ChatCompletions,
        }),
        &[],
    );
    assert!(
        empty_caps.is_err(),
        "Custom create must require at least one model capability"
    );
    assert!(db.get_account("custom-empty-caps").unwrap().is_none());

    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn create_account_linked_to_platform_is_atomic_on_link_failure() {
    use crate::platform::{PlatformGroup, PlatformKind};

    let dir = temp_data_dir("atomic-create-link");
    let db = Database::open(dir.clone()).unwrap();
    db.create_platform_account(
        "parent-atomic",
        PlatformKind::NewApi,
        "Atomic Parent",
        "https://platform.example/v1",
        Some("mgmt-cipher"),
    )
    .unwrap();
    db.conn
        .execute_batch(
            "CREATE TRIGGER fail_platform_link
                 BEFORE UPDATE ON credentials
                 WHEN NEW.group_json IS NOT NULL AND OLD.group_json IS NULL
                 BEGIN
                     SELECT RAISE(ABORT, 'forced link failure');
                 END;",
        )
        .unwrap();

    let mut custom = account("linked-atomic");
    custom.provider_id = CUSTOM_PROVIDER_ID.to_string();
    custom.enabled = false;
    custom.credential_kind = CredentialKind::ApiKey;
    custom.quota_scope = QuotaScope::Key;
    custom.key_cipher = "linked-atomic-cipher".into();
    let error = db
        .create_account_with_contract_linked_to_platform(
            &custom,
            &AccountCustomConfigInput {
                endpoint_url: "https://platform.example/v1/chat/completions".into(),
                upstream_protocol: UpstreamProtocolKind::ChatCompletions,
            },
            &[AccountModelCapabilityInput {
                public_model: "org/model".into(),
                upstream_model: "org/model".into(),
                protocol: UpstreamProtocolKind::ChatCompletions,
                source: Some("discovery".into()),
            }],
            "parent-atomic",
            &PlatformGroup::default(),
        )
        .expect_err("forced link failure should abort the create");
    assert!(error.to_string().contains("forced link failure"), "{error}");
    assert!(db.get_account("linked-atomic").unwrap().is_none());
    assert!(
        db.list_platform_links()
            .unwrap()
            .into_iter()
            .all(|link| link.account_id != "linked-atomic")
    );

    db.conn
        .execute_batch("DROP TRIGGER fail_platform_link;")
        .unwrap();
    db.create_account_with_contract_linked_to_platform(
        &custom,
        &AccountCustomConfigInput {
            endpoint_url: "https://platform.example/v1/chat/completions".into(),
            upstream_protocol: UpstreamProtocolKind::ChatCompletions,
        },
        &[AccountModelCapabilityInput {
            public_model: "org/model".into(),
            upstream_model: "org/model".into(),
            protocol: UpstreamProtocolKind::ChatCompletions,
            source: Some("discovery".into()),
        }],
        "parent-atomic",
        &PlatformGroup::default(),
    )
    .unwrap();
    assert!(db.get_account("linked-atomic").unwrap().is_some());
    assert!(
        db.list_platform_links()
            .unwrap()
            .into_iter()
            .any(|link| link.account_id == "linked-atomic"
                && link.platform_account_id == "parent-atomic")
    );
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn account_migration_batch_is_atomic_and_preserves_order() {
    let dir = temp_data_dir("account-migration-batch");
    let db = Database::open(dir.clone()).unwrap();
    let go = account("migration-go");
    let mut custom = account("migration-custom");
    custom.provider_id = CUSTOM_PROVIDER_ID.to_string();
    custom.credential_kind = CredentialKind::ApiKey;
    custom.quota_scope = QuotaScope::Key;
    custom.enabled = false;
    let records = vec![
        AccountImportRecord {
            account: go,
            custom_config: None,
            capabilities: Vec::new(),
            verification_status: ConnectionVerificationStatus::NotRequired,
            connection_verified_at: None,
            ollama_billing_tier: None,
        },
        AccountImportRecord {
            account: custom,
            custom_config: Some(AccountCustomConfigInput {
                endpoint_url: "https://api.example.com/v1/chat/completions".into(),
                upstream_protocol: UpstreamProtocolKind::ChatCompletions,
            }),
            capabilities: vec![AccountModelCapabilityInput {
                public_model: "org/model".into(),
                upstream_model: "org/model".into(),
                protocol: UpstreamProtocolKind::ChatCompletions,
                source: Some("import".into()),
            }],
            verification_status: ConnectionVerificationStatus::Pending,
            connection_verified_at: None,
            ollama_billing_tier: None,
        },
    ];
    db.conn
        .execute_batch(
            "CREATE TRIGGER fail_migration_custom_config
                 BEFORE INSERT ON destinations
                 WHEN NEW.legacy_kind = 'custom_account'
                 BEGIN
                     SELECT RAISE(ABORT, 'forced migration failure');
                 END;",
        )
        .unwrap();
    assert!(db.import_accounts_with_contracts(&records).is_err());
    assert!(db.get_account("migration-go").unwrap().is_none());
    assert!(db.get_account("migration-custom").unwrap().is_none());

    db.conn
        .execute_batch("DROP TRIGGER fail_migration_custom_config;")
        .unwrap();
    db.import_accounts_with_contracts(&records).unwrap();
    let imported = db
        .list_accounts()
        .unwrap()
        .into_iter()
        .filter(|account| account.id.starts_with("migration-"))
        .map(|account| account.id)
        .collect::<Vec<_>>();
    assert_eq!(imported, ["migration-go", "migration-custom"]);
    assert!(
        db.account_custom_config("migration-custom")
            .unwrap()
            .is_some()
    );
    let imported_satellites: i64 = db
        .conn
        .query_row(
            "SELECT COUNT(*) FROM credentials
             WHERE legacy_account_id IN ('migration-go', 'migration-custom')
               AND binding_id IS NOT NULL",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(imported_satellites, 2);
    let imported_identities: i64 = db
        .conn
        .query_row(
            "SELECT COUNT(*) FROM credentials
             WHERE legacy_account_id IN ('migration-go', 'migration-custom') AND identity_id IS NOT NULL",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(imported_identities, 2);

    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn custom_capability_protocol_must_equal_the_config_protocol() {
    let dir = temp_data_dir("custom-protocol-mismatch");
    let db = Database::open(dir.clone()).unwrap();
    let mut custom = account("custom-protocol");
    custom.provider_id = CUSTOM_PROVIDER_ID.to_string();
    custom.enabled = false;
    let mismatch = db.create_account_with_contract(
        &custom,
        Some(&AccountCustomConfigInput {
            endpoint_url: "https://api.example.com/v1/messages".into(),
            upstream_protocol: UpstreamProtocolKind::Messages,
        }),
        &[AccountModelCapabilityInput {
            public_model: "org/model".into(),
            upstream_model: "org/model".into(),
            protocol: UpstreamProtocolKind::ChatCompletions,
            source: None,
        }],
    );
    assert!(
        mismatch
            .unwrap_err()
            .to_string()
            .contains("must equal account custom_config.upstream_protocol")
    );
    assert!(db.get_account("custom-protocol").unwrap().is_none());

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
    let stored = db
        .list_account_model_capabilities("custom-protocol")
        .unwrap();
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].protocol, UpstreamProtocolKind::Messages);

    let rejected = db.replace_account_model_capabilities(
        "custom-protocol",
        &[AccountModelCapabilityInput {
            public_model: "org/other".into(),
            upstream_model: "org/other".into(),
            protocol: UpstreamProtocolKind::Responses,
            source: None,
        }],
    );
    assert!(
        rejected
            .unwrap_err()
            .to_string()
            .contains("must equal account custom_config.upstream_protocol")
    );
    let kept = db
        .list_account_model_capabilities("custom-protocol")
        .unwrap();
    assert_eq!(kept.len(), 1);
    assert!(kept.iter().all(|row| row.public_model == "org/model"));

    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn custom_capabilities_allow_shared_upstream_but_reject_duplicate_public_names() {
    let dir = temp_data_dir("custom-model-mapping-uniqueness");
    let db = Database::open(dir.clone()).unwrap();
    let mut custom = account("custom-mapping");
    custom.provider_id = CUSTOM_PROVIDER_ID.to_string();
    custom.enabled = false;
    db.create_account_with_contract(
        &custom,
        Some(&AccountCustomConfigInput {
            endpoint_url: "https://api.example.com/v1/chat/completions".into(),
            upstream_protocol: UpstreamProtocolKind::ChatCompletions,
        }),
        &[
            AccountModelCapabilityInput {
                public_model: "public-one".into(),
                upstream_model: "shared-upstream:0731".into(),
                protocol: UpstreamProtocolKind::ChatCompletions,
                source: None,
            },
            AccountModelCapabilityInput {
                public_model: "public-two".into(),
                upstream_model: "shared-upstream:0731".into(),
                protocol: UpstreamProtocolKind::ChatCompletions,
                source: None,
            },
        ],
    )
    .unwrap();
    let saved = db
        .list_account_model_capabilities("custom-mapping")
        .unwrap();
    assert_eq!(saved.len(), 2);
    assert!(
        saved
            .iter()
            .all(|row| row.upstream_model == "shared-upstream:0731")
    );

    let duplicate = db.replace_account_model_capabilities(
        "custom-mapping",
        &[
            AccountModelCapabilityInput {
                public_model: "Public-One".into(),
                upstream_model: "upstream-a".into(),
                protocol: UpstreamProtocolKind::ChatCompletions,
                source: None,
            },
            AccountModelCapabilityInput {
                public_model: "public-one".into(),
                upstream_model: "upstream-b".into(),
                protocol: UpstreamProtocolKind::ChatCompletions,
                source: None,
            },
        ],
    );
    assert!(
        duplicate
            .unwrap_err()
            .to_string()
            .contains("duplicate model capability")
    );
    assert_eq!(
        db.list_account_model_capabilities("custom-mapping")
            .unwrap(),
        saved
    );

    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn custom_mutations_repend_but_keep_verified_accounts_enabled() {
    let dir = temp_data_dir("custom-lifecycle-stale");
    let db = Database::open(dir.clone()).unwrap();
    let mut custom = account("custom-stale");
    custom.provider_id = CUSTOM_PROVIDER_ID.to_string();
    custom.enabled = false;
    db.create_account_with_contract(
        &custom,
        Some(&AccountCustomConfigInput {
            endpoint_url: "https://api.example.com/v1/chat/completions".into(),
            upstream_protocol: UpstreamProtocolKind::ChatCompletions,
        }),
        &[AccountModelCapabilityInput {
            public_model: "org/model".into(),
            upstream_model: "org/model".into(),
            protocol: UpstreamProtocolKind::ChatCompletions,
            source: None,
        }],
    )
    .unwrap();
    db.set_account_verification(
        "custom-stale",
        ConnectionVerificationStatus::Verified,
        Some(Utc::now()),
        Some("previous"),
    )
    .unwrap();
    db.conn
        .execute(
            "UPDATE credentials SET enabled = 1 WHERE legacy_account_id = 'custom-stale'",
            [],
        )
        .unwrap();

    db.upsert_account_custom_config(
        "custom-stale",
        &AccountCustomConfigInput {
            endpoint_url: "https://api.example.net/v2/chat/completions".into(),
            upstream_protocol: UpstreamProtocolKind::ChatCompletions,
        },
    )
    .unwrap();
    let after_url = db.get_account("custom-stale").unwrap().unwrap();
    let after_url_state = db
        .account_verification_state("custom-stale")
        .unwrap()
        .unwrap();
    assert!(after_url.enabled);
    assert_eq!(
        after_url_state.status,
        ConnectionVerificationStatus::Pending
    );
    assert!(after_url_state.connection_verified_at.is_none());
    assert!(after_url_state.verification_error.is_none());

    db.set_account_verification(
        "custom-stale",
        ConnectionVerificationStatus::Verified,
        Some(Utc::now()),
        None,
    )
    .unwrap();
    db.conn
        .execute(
            "UPDATE credentials SET enabled = 1 WHERE legacy_account_id = 'custom-stale'",
            [],
        )
        .unwrap();
    db.replace_account_model_capabilities(
        "custom-stale",
        &[AccountModelCapabilityInput {
            public_model: "org/other".into(),
            upstream_model: "org/other".into(),
            protocol: UpstreamProtocolKind::ChatCompletions,
            source: None,
        }],
    )
    .unwrap();
    let after_caps = db.get_account("custom-stale").unwrap().unwrap();
    let after_caps_state = db
        .account_verification_state("custom-stale")
        .unwrap()
        .unwrap();
    assert!(after_caps.enabled);
    assert_eq!(
        after_caps_state.status,
        ConnectionVerificationStatus::Pending
    );
    assert!(after_caps_state.connection_verified_at.is_none());

    db.set_account_verification(
        "custom-stale",
        ConnectionVerificationStatus::Verified,
        Some(Utc::now()),
        Some("stale"),
    )
    .unwrap();
    db.conn
        .execute(
            "UPDATE credentials SET enabled = 1 WHERE legacy_account_id = 'custom-stale'",
            [],
        )
        .unwrap();
    db.update_account(
        "custom-stale",
        &AccountUpdate {
            key: Some("rotated".into()),
            enabled: Some(true),
            ..AccountUpdate::default()
        },
        Some("new-cipher"),
        None,
    )
    .unwrap();
    let after_key = db.get_account("custom-stale").unwrap().unwrap();
    let after_key_state = db
        .account_verification_state("custom-stale")
        .unwrap()
        .unwrap();
    assert!(after_key.enabled);
    assert_eq!(
        after_key_state.status,
        ConnectionVerificationStatus::Pending
    );
    assert!(after_key_state.connection_verified_at.is_none());
    assert!(after_key_state.verification_error.is_none());
    let caps_after_key = db.list_account_model_capabilities("custom-stale").unwrap();
    assert_eq!(caps_after_key.len(), 1);
    assert_eq!(caps_after_key[0].public_model, "org/other");

    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn custom_verification_cas_rejects_stale_key_config_caps_and_delete() {
    let dir = temp_data_dir("custom-verify-cas");
    let mut db = Database::open(dir.clone()).unwrap();
    let mut custom = account("custom-cas");
    custom.provider_id = CUSTOM_PROVIDER_ID.to_string();
    custom.enabled = false;
    custom.key_cipher = "cipher-a".into();
    db.create_account_with_contract(
        &custom,
        Some(&AccountCustomConfigInput {
            endpoint_url: "https://api.example.com/v1/chat/completions".into(),
            upstream_protocol: UpstreamProtocolKind::ChatCompletions,
        }),
        &[AccountModelCapabilityInput {
            public_model: "one".into(),
            upstream_model: "one".into(),
            protocol: UpstreamProtocolKind::ChatCompletions,
            source: None,
        }],
    )
    .unwrap();

    let contract = db
        .capture_custom_verification_contract("custom-cas")
        .unwrap()
        .unwrap();
    assert_eq!(contract.key_cipher, "cipher-a");
    assert_eq!(contract.capabilities[0].0, "one");

    db.update_account(
        "custom-cas",
        &AccountUpdate {
            key: Some("rotated".into()),
            ..AccountUpdate::default()
        },
        Some("cipher-b"),
        None,
    )
    .unwrap();
    assert!(
        !db.commit_custom_verification_if_contract_matches(
            &contract,
            ConnectionVerificationStatus::Verified,
            Some(Utc::now()),
            None,
        )
        .unwrap()
    );
    assert_eq!(
        db.account_verification_state("custom-cas")
            .unwrap()
            .unwrap()
            .status,
        ConnectionVerificationStatus::Pending
    );

    let after_key = db
        .capture_custom_verification_contract("custom-cas")
        .unwrap()
        .unwrap();
    db.upsert_account_custom_config(
        "custom-cas",
        &AccountCustomConfigInput {
            endpoint_url: "https://api.example.net/v2/chat/completions".into(),
            upstream_protocol: UpstreamProtocolKind::ChatCompletions,
        },
    )
    .unwrap();
    assert!(
        !db.commit_custom_verification_if_contract_matches(
            &after_key,
            ConnectionVerificationStatus::Verified,
            Some(Utc::now()),
            None,
        )
        .unwrap()
    );

    let after_config = db
        .capture_custom_verification_contract("custom-cas")
        .unwrap()
        .unwrap();
    db.replace_account_model_capabilities(
        "custom-cas",
        &[AccountModelCapabilityInput {
            public_model: "two".into(),
            upstream_model: "two".into(),
            protocol: UpstreamProtocolKind::ChatCompletions,
            source: None,
        }],
    )
    .unwrap();
    assert!(
        !db.commit_custom_verification_if_contract_matches(
            &after_config,
            ConnectionVerificationStatus::Verified,
            Some(Utc::now()),
            None,
        )
        .unwrap()
    );

    let matching = db
        .capture_custom_verification_contract("custom-cas")
        .unwrap()
        .unwrap();
    assert!(
        db.commit_custom_verification_if_contract_matches(
            &matching,
            ConnectionVerificationStatus::Verified,
            Some(Utc::now()),
            None,
        )
        .unwrap()
    );
    assert_eq!(
        db.account_verification_state("custom-cas")
            .unwrap()
            .unwrap()
            .status,
        ConnectionVerificationStatus::Verified
    );
    assert!(
        !db.commit_custom_verification_if_contract_matches(
            &matching,
            ConnectionVerificationStatus::Failed,
            None,
            Some("stale"),
        )
        .unwrap()
    );
    assert_eq!(
        db.account_verification_state("custom-cas")
            .unwrap()
            .unwrap()
            .status,
        ConnectionVerificationStatus::Verified
    );

    let mut leftover = account("custom-delete");
    leftover.provider_id = CUSTOM_PROVIDER_ID.to_string();
    leftover.enabled = false;
    db.create_account_with_contract(
        &leftover,
        Some(&AccountCustomConfigInput {
            endpoint_url: "https://api.example.com/v1/chat/completions".into(),
            upstream_protocol: UpstreamProtocolKind::ChatCompletions,
        }),
        &[AccountModelCapabilityInput {
            public_model: "one".into(),
            upstream_model: "one".into(),
            protocol: UpstreamProtocolKind::ChatCompletions,
            source: None,
        }],
    )
    .unwrap();
    let deleted_contract = db
        .capture_custom_verification_contract("custom-delete")
        .unwrap()
        .unwrap();
    db.delete_account("custom-delete").unwrap();
    assert!(
        !db.commit_custom_verification_if_contract_matches(
            &deleted_contract,
            ConnectionVerificationStatus::Verified,
            Some(Utc::now()),
            None,
        )
        .unwrap()
    );

    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn go_and_ollama_accounts_persist_enablement_changes() {
    let dir = temp_data_dir("enablement-gate");
    let db = Database::open(dir.clone()).unwrap();
    for (id, provider_id) in [
        ("go-enabled", OPENCODE_PROVIDER_ID),
        ("ollama-enabled", OLLAMA_PROVIDER_ID),
    ] {
        let mut candidate = account(id);
        candidate.provider_id = provider_id.to_string();
        candidate.enabled = true;
        db.create_account(&candidate).unwrap();
        assert!(db.get_account(id).unwrap().unwrap().enabled, "{id}");
        for enabled in [false, true] {
            db.update_account(
                id,
                &AccountUpdate {
                    enabled: Some(enabled),
                    ..AccountUpdate::default()
                },
                None,
                None,
            )
            .unwrap();
            assert_eq!(
                db.get_account(id).unwrap().unwrap().enabled,
                enabled,
                "{id}"
            );
        }
    }
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn reopen_repairs_legacy_goat_verification_without_changing_other_accounts() {
    let dir = temp_data_dir("unroutable-sanitation");
    let db = Database::open(dir.clone()).unwrap();

    let mut go = account("go-keep");
    go.notes = Some("go-notes".into());
    db.create_account(&go).unwrap();
    leftover_enable(&db, "go-keep");

    let mut unknown = account("unknown-keep");
    unknown.notes = Some("unknown-notes".into());
    db.create_account(&unknown).unwrap();
    leftover_enable(&db, "unknown-keep");
    db.conn
        .execute(
            "UPDATE credentials
                 SET provider_id = 'unknown-provider'
                 WHERE legacy_account_id = 'unknown-keep'",
            [],
        )
        .unwrap();

    persist_sanitation_account(
        &db,
        builtin_provider(COMMAND_CODE_PROVIDER_ID).unwrap(),
        "goat-pending",
        "goat-pending-notes",
    );
    leftover_enable(&db, "goat-pending");

    persist_sanitation_account(
        &db,
        builtin_provider(COMMAND_CODE_PROVIDER_ID).unwrap(),
        "goat-verified",
        "goat-verified-notes",
    );
    db.conn
        .execute(
            "UPDATE credentials
                 SET enabled = 1, verification_status = 'verified', verification_error = NULL
                 WHERE legacy_account_id = 'goat-verified'",
            [],
        )
        .unwrap();

    persist_sanitation_account(
        &db,
        builtin_provider(COMMAND_CODE_PROVIDER_ID).unwrap(),
        "goat-failed",
        "goat-failed-notes",
    );
    db.conn
        .execute(
            "UPDATE credentials
                 SET enabled = 1, verification_status = 'failed', verification_error = 'boom'
                 WHERE legacy_account_id = 'goat-failed'",
            [],
        )
        .unwrap();

    persist_sanitation_account(
        &db,
        builtin_provider(CUSTOM_PROVIDER_ID).unwrap(),
        "draft-api",
        "draft-api-notes",
    );
    leftover_enable(&db, "draft-api");

    // An enabled Ollama Cloud row is now legitimate (routable offering),
    // so open must leave it untouched.
    persist_sanitation_account(
        &db,
        builtin_provider(OLLAMA_PROVIDER_ID).unwrap(),
        "ollama-leftover",
        "ollama-leftover-notes",
    );
    leftover_enable(&db, "ollama-leftover");

    let zen_before = sanitation_snapshot(&db, ZEN_FREE_ACCOUNT_ID);
    let go_before = sanitation_snapshot(&db, "go-keep");
    let unknown_before = sanitation_snapshot(&db, "unknown-keep");
    let goat_pending_before = sanitation_snapshot(&db, "goat-pending");
    let goat_verified_before = sanitation_snapshot(&db, "goat-verified");
    let goat_failed_before = sanitation_snapshot(&db, "goat-failed");
    let custom_before = sanitation_snapshot(&db, "draft-api");
    let ollama_before = sanitation_snapshot(&db, "ollama-leftover");
    assert!(go_before.enabled);
    assert!(custom_before.enabled);
    assert!(ollama_before.enabled);
    assert!(unknown_before.enabled);
    assert!(goat_pending_before.enabled);
    assert!(goat_verified_before.enabled);
    assert!(goat_failed_before.enabled);
    assert_eq!(
        goat_pending_before.verification,
        ConnectionVerificationStatus::NotRequired
    );
    assert_eq!(
        goat_verified_before.verification,
        ConnectionVerificationStatus::Verified
    );
    assert_eq!(
        goat_failed_before.verification,
        ConnectionVerificationStatus::Failed
    );

    drop(db);
    let db = Database::open(dir.clone()).unwrap();

    let zen_after = sanitation_snapshot(&db, ZEN_FREE_ACCOUNT_ID);
    let go_after = sanitation_snapshot(&db, "go-keep");
    let unknown_after = sanitation_snapshot(&db, "unknown-keep");
    assert_eq!(zen_after, zen_before);
    assert_eq!(go_after, go_before);
    assert_eq!(unknown_after, unknown_before);

    let goat_pending_after = sanitation_snapshot(&db, "goat-pending");
    assert_eq!(goat_pending_after, goat_pending_before);

    let goat_verified_after = sanitation_snapshot(&db, "goat-verified");
    assert_eq!(goat_verified_after.name, goat_verified_before.name);
    assert_eq!(goat_verified_after.notes, goat_verified_before.notes);
    assert_eq!(
        goat_verified_after.updated_at,
        goat_verified_before.updated_at
    );
    assert!(goat_verified_after.enabled);
    assert_eq!(
        goat_verified_after.verification,
        ConnectionVerificationStatus::NotRequired
    );

    let goat_failed_after = sanitation_snapshot(&db, "goat-failed");
    assert_eq!(goat_failed_after.name, goat_failed_before.name);
    assert_eq!(goat_failed_after.notes, goat_failed_before.notes);
    assert_eq!(goat_failed_after.updated_at, goat_failed_before.updated_at);
    assert!(goat_failed_after.enabled);
    assert_eq!(
        goat_failed_after.verification,
        ConnectionVerificationStatus::NotRequired
    );
    assert!(goat_failed_after.verification_error.is_none());

    let custom_after = sanitation_snapshot(&db, "draft-api");
    assert_eq!(custom_after, custom_before);

    let ollama_after = sanitation_snapshot(&db, "ollama-leftover");
    assert_eq!(ollama_after, ollama_before);

    let first_pass: Vec<_> = [
        ZEN_FREE_ACCOUNT_ID,
        "go-keep",
        "unknown-keep",
        "goat-pending",
        "goat-verified",
        "goat-failed",
        "draft-api",
        "ollama-leftover",
    ]
    .into_iter()
    .map(|id| (id.to_string(), sanitation_snapshot(&db, id)))
    .collect();

    drop(db);
    let db = Database::open(dir.clone()).unwrap();
    for (id, expected) in &first_pass {
        assert_eq!(
            sanitation_snapshot(&db, id),
            *expected,
            "second open must be idempotent for {id}"
        );
    }

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
    assert_eq!(schema_version_on(&conn).unwrap(), 22);
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
    assert_eq!(schema_version_on(&backup).unwrap(), 22);
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

struct V27HookGuard;
impl Drop for V27HookGuard {
    fn drop(&mut self) {
        v27_test_hooks::reset();
    }
}

fn arm_v27_fault(point: V27MigrationFault) -> V27HookGuard {
    v27_test_hooks::reset();
    v27_test_hooks::set_fault(Some(point));
    V27HookGuard
}

fn populate_v26_source(dir: &Path) -> (String, String) {
    let db = Database::open(dir.to_path_buf()).expect("fixture database should open");
    let mut v26_account = account("v26-account");
    v26_account.key_cipher = fixture_account_key_cipher();
    db.create_account(&v26_account)
        .expect("representative account should save");
    let now = Utc::now();
    db.insert_sub_gateway_key(&SubGatewayKey {
        id: "sub-v26".into(),
        name: "Laptop".into(),
        key: "ocg-v26-laptop".into(),
        enabled: true,
        deleted_at: None,
        created_at: now,
    })
    .expect("sub key should save");
    let config = serde_json::json!({
        "gateway_port": 9042,
        "gateway_key": "ocg-v26-primary",
        "upstream_base_url": "https://opencode.ai/zen/go" });
    db.set_config(&config.to_string())
        .expect("v26 config should persist");
    drop(db);
    reverse_current_to_v26(dir);
    ("ocg-v26-primary".into(), "ocg-v26-laptop".into())
}

#[test]
fn v27_to_v28_adds_goat_model_access_without_replaying_v27() {
    let dir = temp_data_dir("v27-v28-migrate");
    populate_v26_source(&dir);
    let db_path = dir.join("data.sqlite");
    let conn = Connection::open(&db_path).unwrap();
    let cipher = test_host_cipher();
    migrate_to_v27(&conn, &db_path, Some(cipher.as_ref()), false).unwrap();
    assert_eq!(schema_version_on(&conn).unwrap(), V27_SCHEMA_VERSION);
    assert!(!table_has_column(&conn, "accounts", "goat_model_access").unwrap());

    migrate_to_v28(&conn).unwrap();
    assert_eq!(schema_version_on(&conn).unwrap(), 28);
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
        schema_version_on(&migrated.conn).unwrap(),
        CURRENT_SCHEMA_VERSION
    );
    assert!(!table_exists(&migrated.conn, "cpa_integration").unwrap());
    assert!(migrated.cpa_integration().unwrap().is_none());
    drop(migrated);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn cpa_singleton_upsert_catalog_and_disconnect_are_idempotent_and_atomic() {
    let dir = temp_data_dir("cpa-singleton-lifecycle");
    let db = open_with_host_cipher(dir.clone()).unwrap();
    let now = Utc::now();
    let mut cpa_account = account(CPA_ACCOUNT_ID);
    cpa_account.provider_id = CPA_PROVIDER_ID.to_string();
    cpa_account.credential_kind = CredentialKind::ApiKey;
    cpa_account.quota_scope = QuotaScope::Key;
    cpa_account.name = CPA_ACCOUNT_NAME.to_string();
    cpa_account.key_cipher = test_host_cipher().encrypt("cpa-inference").unwrap();
    cpa_account.enabled = false;
    cpa_account.account_type = AccountType::Key;
    cpa_account.setup_step = AccountSetupStep::Ready;
    cpa_account.created_at = now;
    cpa_account.updated_at = now;
    let management_cipher = test_host_cipher().encrypt("cpa-management").unwrap();

    db.upsert_cpa_integration(&cpa_account, "http://127.0.0.1:8317", &management_cipher)
        .unwrap();
    cpa_account.enabled = true;
    db.upsert_cpa_integration(&cpa_account, "http://127.0.0.1:9317", &management_cipher)
        .unwrap();
    let record = db.cpa_integration().unwrap().unwrap();
    assert_eq!(record.account_id, CPA_ACCOUNT_ID);
    assert_eq!(record.base_url, "http://127.0.0.1:9317");
    assert_eq!(record.management_key_cipher, management_cipher);
    assert!(db.get_account(CPA_ACCOUNT_ID).unwrap().unwrap().enabled);
    assert!(!table_exists(&db.conn, "cpa_integration").unwrap());
    let dest_url: Option<String> = db
        .conn
        .query_row(
            "SELECT base_url FROM destinations
             WHERE adapter = 'cpa'
                OR (legacy_kind = 'builtin' AND legacy_id = 'cpa')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(dest_url.as_deref(), Some("http://127.0.0.1:9317"));
    let observer_cipher: String = db
        .conn
        .query_row(
            "SELECT key_cipher FROM credentials WHERE id = ?1",
            [ocg_domain::credential::observer_credential_id_for_cpa().as_str()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(observer_cipher, management_cipher);
    assert_ne!(
        db.get_account(CPA_ACCOUNT_ID).unwrap().unwrap().key_cipher,
        management_cipher
    );

    db.conn
        .execute(
            "UPDATE credentials SET auth_error = '401' WHERE legacy_account_id = ?1",
            [CPA_ACCOUNT_ID],
        )
        .unwrap();
    db.upsert_cpa_integration(&cpa_account, "http://127.0.0.1:9317", &management_cipher)
        .unwrap();
    assert_eq!(
        db.get_account(CPA_ACCOUNT_ID)
            .unwrap()
            .unwrap()
            .auth_error
            .as_deref(),
        Some("401"),
        "saving the same inference cipher must preserve an existing breaker"
    );
    cpa_account.key_cipher = test_host_cipher().encrypt("cpa-inference-fixed").unwrap();
    db.upsert_cpa_integration(&cpa_account, "http://127.0.0.1:9317", &management_cipher)
        .unwrap();
    assert!(
        db.get_account(CPA_ACCOUNT_ID)
            .unwrap()
            .unwrap()
            .auth_error
            .is_none(),
        "replacing the inference cipher must clear the stale 401 breaker"
    );

    db.replace_cpa_model_catalog(
        &[
            CpaCatalogModel {
                id: "gpt-5.6-sol".into(),
                owned_by: Some("openai".into()),
                enabled: true,
            },
            "unknown-cpa-model".into(),
        ],
        "http://127.0.0.1:9317",
        now,
    )
    .unwrap();
    let catalog = db.cpa_model_catalog().unwrap().unwrap();
    assert_eq!(catalog.models.len(), 2);
    assert_eq!(catalog.models[0].id, "gpt-5.6-sol");
    assert_eq!(catalog.models[0].owned_by.as_deref(), Some("openai"));
    assert!(catalog.models[1].owned_by.is_none());

    drop(db);
    let db = open_with_host_cipher(dir.clone()).unwrap();
    let reopened = db.cpa_integration().unwrap().unwrap();
    assert_eq!(reopened.account_id, CPA_ACCOUNT_ID);
    assert_eq!(reopened.base_url, "http://127.0.0.1:9317");
    assert_eq!(reopened.management_key_cipher, management_cipher);
    assert!(!table_exists(&db.conn, "cpa_integration").unwrap());

    db.delete_cpa_integration().unwrap();
    db.delete_cpa_integration().unwrap();
    assert!(db.cpa_integration().unwrap().is_none());
    assert!(db.cpa_model_catalog().unwrap().is_none());
    assert!(db.get_account(CPA_ACCOUNT_ID).unwrap().is_none());
    let leftover_dest: i64 = db
        .conn
        .query_row(
            "SELECT COUNT(*) FROM destinations
             WHERE adapter = 'cpa'
                OR (legacy_kind = 'builtin' AND legacy_id = 'cpa')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(leftover_dest, 0);
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn cpa_model_catalog_reads_legacy_id_arrays() {
    let dir = temp_data_dir("cpa-catalog-legacy-ids");
    let db = open_with_host_cipher(dir.clone()).unwrap();
    db.conn
        .execute(
            "INSERT INTO provider_model_catalogs
                 (provider_id, models_json, refreshed_at, source_url)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                CPA_PROVIDER_ID,
                r#"["gpt-5","claude"]"#,
                Utc::now().to_rfc3339(),
                "http://127.0.0.1:8317",
            ],
        )
        .unwrap();
    let catalog = db.cpa_model_catalog().unwrap().unwrap();
    assert_eq!(
        catalog.models,
        [
            CpaCatalogModel {
                id: "gpt-5".into(),
                owned_by: None,
                enabled: true,
            },
            CpaCatalogModel {
                id: "claude".into(),
                owned_by: None,
                enabled: true,
            },
        ]
    );
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn cpa_model_catalog_merge_refresh_keeps_selection_and_defaults_new_ids_off() {
    let previous = vec![
        CpaCatalogModel {
            id: "kept".into(),
            owned_by: None,
            enabled: true,
        },
        CpaCatalogModel {
            id: "off".into(),
            owned_by: None,
            enabled: false,
        },
        CpaCatalogModel {
            id: "gone".into(),
            owned_by: None,
            enabled: true,
        },
    ];
    let incoming = vec![
        CpaCatalogModel {
            id: "kept".into(),
            owned_by: Some("openai".into()),
            enabled: true,
        },
        CpaCatalogModel {
            id: "off".into(),
            owned_by: None,
            enabled: true,
        },
        CpaCatalogModel {
            id: "fresh".into(),
            owned_by: None,
            enabled: true,
        },
    ];
    assert_eq!(
        CpaCatalogModel::merge_refresh(incoming, &previous),
        [
            CpaCatalogModel {
                id: "kept".into(),
                owned_by: Some("openai".into()),
                enabled: true,
            },
            CpaCatalogModel {
                id: "off".into(),
                owned_by: None,
                enabled: false,
            },
            CpaCatalogModel {
                id: "fresh".into(),
                owned_by: None,
                enabled: false,
            },
        ]
    );
    assert!(
        CpaCatalogModel::enabled_ids(&CpaCatalogModel::merge_refresh(vec!["only".into()], &[]))
            .is_empty()
    );
}

#[test]
fn cpa_model_catalog_reads_enabled_flag_and_defaults_missing_on() {
    let dir = temp_data_dir("cpa-catalog-enabled");
    let db = open_with_host_cipher(dir.clone()).unwrap();
    db.conn
        .execute(
            "INSERT INTO provider_model_catalogs
                 (provider_id, models_json, refreshed_at, source_url)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                CPA_PROVIDER_ID,
                r#"[{"id":"legacy"},{"id":"off","enabled":false}]"#,
                Utc::now().to_rfc3339(),
                "http://127.0.0.1:8317",
            ],
        )
        .unwrap();
    let catalog = db.cpa_model_catalog().unwrap().unwrap();
    assert_eq!(
        catalog.models,
        [
            CpaCatalogModel {
                id: "legacy".into(),
                owned_by: None,
                enabled: true,
            },
            CpaCatalogModel {
                id: "off".into(),
                owned_by: None,
                enabled: false,
            },
        ]
    );
    drop(db);
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
    sqlite_foreign_key_check(&db.conn).unwrap();
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
    assert_eq!(schema_version_on(&backup).unwrap(), V26_SCHEMA_VERSION);
    sqlite_quick_check(&backup).unwrap();
    assert!(table_exists(&backup, "sub_gateway_keys").unwrap());
    assert!(table_has_column(&backup, "accounts", "usage_sync_last_success_at").unwrap());
    drop(backup);
    let digest = sha256_file(&backups[0]).unwrap();
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
    assert_eq!(schema_version_on(&conn).unwrap(), V26_SCHEMA_VERSION);
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
    assert_eq!(schema_version_on(&conn).unwrap(), V26_SCHEMA_VERSION);
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
    assert_eq!(schema_version_on(&db.conn).unwrap(), CURRENT_SCHEMA_VERSION);
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
    assert_eq!(schema_version_on(&conn).unwrap(), CURRENT_SCHEMA_VERSION);
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
    assert_eq!(schema_version_on(&conn).unwrap(), V26_SCHEMA_VERSION);
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
    assert_eq!(schema_version_on(&conn).unwrap(), V26_SCHEMA_VERSION);
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
fn model_protocol_override_upsert_and_auto_delete_round_trip() {
    let dir = temp_data_dir("override-roundtrip");
    let db = Database::open(dir.clone()).unwrap();
    let now = Utc::now();
    let scope = ContractScope::provider(OPENCODE_PROVIDER_ID);

    db.set_model_protocol_overrides(
        &scope,
        &[
            (
                "glm-5.2".into(),
                UpstreamProtocolKind::ChatCompletions,
                ProtocolOverrideState::ForceOn,
            ),
            (
                "glm-5.2".into(),
                UpstreamProtocolKind::Messages,
                ProtocolOverrideState::ForceOff,
            ),
        ],
        now,
    )
    .unwrap();

    let persisted = db.load_persisted_contracts().unwrap();
    let overrides = persisted.overrides.get(&scope).unwrap();
    assert_eq!(overrides.len(), 2);
    assert!(overrides.iter().any(|row| row.model_id == "glm-5.2"
        && row.protocol == UpstreamProtocolKind::ChatCompletions
        && row.state == ProtocolOverrideState::ForceOn));
    assert!(overrides.iter().any(|row| row.model_id == "glm-5.2"
        && row.protocol == UpstreamProtocolKind::Messages
        && row.state == ProtocolOverrideState::ForceOff));

    db.set_model_protocol_overrides(
        &scope,
        &[(
            "glm-5.2".into(),
            UpstreamProtocolKind::ChatCompletions,
            ProtocolOverrideState::Auto,
        )],
        now,
    )
    .unwrap();

    let persisted = db.load_persisted_contracts().unwrap();
    let overrides = persisted.overrides.get(&scope).unwrap();
    assert_eq!(overrides.len(), 1);
    assert_eq!(overrides[0].protocol, UpstreamProtocolKind::Messages);
    assert_eq!(overrides[0].state, ProtocolOverrideState::ForceOff);

    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

fn effective_from_db(db: &Database) -> crate::provider_contracts::EffectiveContractSet {
    crate::provider_contracts::build_effective_contracts(
        &db.zen_free_model_catalog().unwrap().unwrap_or_default(),
        &[],
        db.load_persisted_contracts().unwrap(),
    )
}

#[test]
fn catalog_refresh_preserves_settings_and_does_not_force_off_new_models() {
    let dir = temp_data_dir("catalog-refresh-preserving-settings");
    let db = Database::open(dir.clone()).unwrap();
    let now = Utc::now();
    let scope = ContractScope::provider(OPENCODE_PROVIDER_ID);

    db.set_contract_catalog(
        &scope,
        &["grok-4.5".into()],
        None,
        crate::provider_contracts::CATALOG_SOURCE_STATIC,
        "",
        now,
    )
    .unwrap();
    db.set_model_protocol_overrides(
        &scope,
        &[
            (
                "grok-4.5".into(),
                UpstreamProtocolKind::Responses,
                ProtocolOverrideState::ForceOn,
            ),
            (
                "glm-5.2".into(),
                UpstreamProtocolKind::ChatCompletions,
                ProtocolOverrideState::ForceOff,
            ),
            (
                "future-go-model".into(),
                UpstreamProtocolKind::Messages,
                ProtocolOverrideState::ForceOn,
            ),
        ],
        now,
    )
    .unwrap();
    let revision_before = db.load_persisted_scope(&scope).unwrap().unwrap().revision;

    let refreshed = db
        .refresh_contract_catalog_preserving_settings(
            &scope,
            &[
                "grok-4.5".into(),
                "glm-5.2".into(),
                "future-go-model".into(),
                "omen-alpha".into(),
            ],
            now,
            crate::provider_contracts::CATALOG_SOURCE_OPENCODE_MODELS,
            "https://opencode.ai/zen/go/v1/models",
        )
        .unwrap();

    assert_eq!(refreshed.revision, revision_before + 1);
    assert_eq!(
        refreshed.catalog_models,
        vec!["grok-4.5", "glm-5.2", "future-go-model", "omen-alpha"]
    );
    let persisted = db.load_persisted_contracts().unwrap();
    let overrides = persisted.overrides.get(&scope).unwrap();
    assert!(overrides.iter().any(|row| row.model_id == "grok-4.5"
        && row.protocol == UpstreamProtocolKind::Responses
        && row.state == ProtocolOverrideState::ForceOn));
    assert!(overrides.iter().any(|row| row.model_id == "glm-5.2"
        && row.protocol == UpstreamProtocolKind::ChatCompletions
        && row.state == ProtocolOverrideState::ForceOff));
    assert!(overrides.iter().any(|row| row.model_id == "future-go-model"
        && row.protocol == UpstreamProtocolKind::Messages
        && row.state == ProtocolOverrideState::ForceOn));
    assert_eq!(overrides.len(), 3, "refresh must not invent force_off rows");
    assert!(
        overrides.iter().all(|row| row.model_id != "omen-alpha"),
        "unknown new models must not receive guessed overrides: {overrides:?}"
    );

    let go = effective_from_db(&db)
        .providers
        .remove(OPENCODE_PROVIDER_ID)
        .unwrap();
    assert!(
        go.model("glm-5.2")
            .is_some_and(|model| !model.protocols["chat_completions"].enabled)
    );
    assert!(
        go.model("omen-alpha")
            .is_some_and(|model| model.enabled_protocols().is_empty() && !model.routable)
    );
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn catalog_refresh_enables_new_models_with_official_or_known_baseline() {
    let dir = temp_data_dir("catalog-refresh-official-on");
    let db = Database::open(dir.clone()).unwrap();
    let now = Utc::now();
    let go_scope = ContractScope::provider(OPENCODE_PROVIDER_ID);
    let goat_scope = ContractScope::provider(COMMAND_CODE_PROVIDER_ID);
    let extra = "vendor/future-command-model";

    db.refresh_contract_catalog_preserving_settings(
        &go_scope,
        &["glm-5.2".into(), "future-go-model".into()],
        now,
        crate::provider_contracts::CATALOG_SOURCE_OPENCODE_MODELS,
        "https://opencode.ai/zen/go/v1/models",
    )
    .unwrap();
    db.apply_official_protocol_baseline(
        &go_scope,
        &["glm-5.2".into(), "future-go-model".into()],
        &crate::official_protocols::OfficialProtocolBaseline::mapped([(
            "future-go-model",
            UpstreamProtocolKind::Responses,
        )]),
        now,
    )
    .unwrap();

    db.refresh_contract_catalog_preserving_settings(
        &goat_scope,
        &["gpt-6-luna".into()],
        now,
        CATALOG_SOURCE_COMMAND_CODE_MODELS,
        COMMAND_CODE_GOAT_BASE_URL,
    )
    .unwrap();
    db.refresh_contract_catalog_preserving_settings(
        &goat_scope,
        &["gpt-6-luna".into(), extra.into()],
        now,
        CATALOG_SOURCE_COMMAND_CODE_MODELS,
        COMMAND_CODE_GOAT_BASE_URL,
    )
    .unwrap();
    db.apply_official_protocol_baseline(
        &goat_scope,
        &[extra.into()],
        &crate::official_protocols::OfficialProtocolBaseline::mapped([(
            extra,
            UpstreamProtocolKind::ChatCompletions,
        )]),
        now,
    )
    .unwrap();

    let set = effective_from_db(&db);
    let go = set.providers.get(OPENCODE_PROVIDER_ID).unwrap();
    let glm = go.model("glm-5.2").unwrap();
    assert!(glm.protocols["chat_completions"].enabled);
    assert_eq!(
        glm.protocols["chat_completions"].r#override,
        ProtocolOverrideState::Auto
    );
    let future = go.model("future-go-model").unwrap();
    assert!(future.protocols["responses"].enabled);
    assert_eq!(
        future.protocols["responses"].r#override,
        ProtocolOverrideState::Auto
    );
    assert!(
        future
            .protocols
            .get("chat_completions")
            .is_none_or(|row| !row.enabled && !row.available)
    );
    let goat = set.providers.get(COMMAND_CODE_PROVIDER_ID).unwrap();
    let extra_model = goat.model(extra).unwrap();
    assert!(extra_model.protocols["chat_completions"].enabled);
    assert_eq!(
        extra_model.protocols["chat_completions"].r#override,
        ProtocolOverrideState::Auto
    );

    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn unavailable_official_baseline_does_not_drop_catalog_or_overrides() {
    let dir = temp_data_dir("catalog-refresh-unavailable-baseline");
    let db = Database::open(dir.clone()).unwrap();
    let now = Utc::now();
    let scope = ContractScope::provider(OPENCODE_PROVIDER_ID);
    db.set_contract_catalog(
        &scope,
        &["grok-4.5".into()],
        Some(now),
        crate::provider_contracts::CATALOG_SOURCE_OPENCODE_MODELS,
        "https://opencode.ai/zen/go/v1/models",
        now,
    )
    .unwrap();
    db.set_model_protocol_overrides(
        &scope,
        &[(
            "grok-4.5".into(),
            UpstreamProtocolKind::Responses,
            ProtocolOverrideState::ForceOff,
        )],
        now,
    )
    .unwrap();
    let before = db.load_persisted_contracts().unwrap();

    db.apply_official_protocol_baseline(
        &scope,
        &["grok-4.5".into()],
        &crate::official_protocols::OfficialProtocolBaseline::Unavailable,
        now,
    )
    .unwrap();

    let after = db.load_persisted_contracts().unwrap();
    assert_eq!(
        after.scopes.get(&scope).unwrap().catalog_models,
        before.scopes.get(&scope).unwrap().catalog_models
    );
    assert_eq!(after.overrides.get(&scope), before.overrides.get(&scope));
    assert_eq!(after.evidence.get(&scope), before.evidence.get(&scope));
    let grok = effective_from_db(&db)
        .providers
        .remove(OPENCODE_PROVIDER_ID)
        .unwrap()
        .model("grok-4.5")
        .unwrap()
        .clone();
    assert!(!grok.protocols["responses"].enabled);
    assert_eq!(
        grok.protocols["responses"].r#override,
        ProtocolOverrideState::ForceOff
    );

    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn catalog_remove_drops_models_and_satellite_rows_without_rewriting_source() {
    let dir = temp_data_dir("catalog-remove-models");
    let db = Database::open(dir.clone()).unwrap();
    let now = Utc::now();
    let scope = ContractScope::provider(OPENCODE_PROVIDER_ID);
    let refreshed_at = now;

    db.set_contract_catalog(
        &scope,
        &["keep-me".into(), "drop-me".into()],
        Some(refreshed_at),
        crate::provider_contracts::CATALOG_SOURCE_OPENCODE_MODELS,
        "https://opencode.ai/zen/go/v1/models",
        now,
    )
    .unwrap();
    db.set_model_protocol_overrides(
        &scope,
        &[
            (
                "keep-me".into(),
                UpstreamProtocolKind::ChatCompletions,
                ProtocolOverrideState::ForceOn,
            ),
            (
                "drop-me".into(),
                UpstreamProtocolKind::ChatCompletions,
                ProtocolOverrideState::ForceOn,
            ),
        ],
        now,
    )
    .unwrap();
    let revision_before = db.load_persisted_scope(&scope).unwrap().unwrap().revision;

    let removed = db
        .remove_contract_catalog_models(&scope, &["drop-me".into()], now)
        .unwrap();

    assert_eq!(removed.revision, revision_before + 1);
    assert_eq!(removed.catalog_models, vec!["keep-me"]);
    assert_eq!(removed.catalog_refreshed_at, Some(refreshed_at));
    assert_eq!(
        removed.catalog_source,
        crate::provider_contracts::CATALOG_SOURCE_OPENCODE_MODELS
    );
    assert_eq!(
        removed.catalog_source_url,
        "https://opencode.ai/zen/go/v1/models"
    );
    let persisted = db.load_persisted_contracts().unwrap();
    let overrides = persisted.overrides.get(&scope).unwrap();
    assert!(overrides.iter().all(|row| row.model_id != "drop-me"));
    assert!(overrides.iter().any(|row| row.model_id == "keep-me"
        && row.protocol == UpstreamProtocolKind::ChatCompletions
        && row.state == ProtocolOverrideState::ForceOn));

    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn zen_catalog_remove_last_and_all_stay_empty_after_reopen() {
    let dir = temp_data_dir("zen-catalog-remove-empty");
    let now = Utc::now();
    let scope = ContractScope::provider(OPENCODE_ZEN_FREE_PROVIDER_ID);
    let snapshot = crate::kernel::zen::ZenFreeModelCatalog {
        models: vec!["review-model-free".into(), "second-free".into()],
        refreshed_at: Some(now),
        source_url: crate::kernel::zen::ZEN_MODELS_SOURCE_URL.into(),
    };

    {
        let db = Database::open(dir.clone()).unwrap();
        db.set_zen_free_model_catalog_preserving_settings(&snapshot)
            .unwrap();
        db.remove_contract_catalog_models(&scope, &["second-free".into()], now)
            .unwrap();
        let after_one = db.load_persisted_scope(&scope).unwrap().unwrap();
        assert_eq!(after_one.catalog_models, vec!["review-model-free"]);
        db.remove_contract_catalog_models(&scope, &["review-model-free".into()], now)
            .unwrap();
        let after_last = db.load_persisted_scope(&scope).unwrap().unwrap();
        assert!(after_last.catalog_models.is_empty());
        let live = crate::provider_contracts::build_effective_contracts(
            &snapshot,
            &[],
            db.load_persisted_contracts().unwrap(),
        );
        let zen = live.scope(&scope).unwrap();
        assert!(zen.catalog.models.is_empty());
        assert!(zen.model("review-model-free").is_none());
        assert!(zen.model("second-free").is_none());
    }

    let reopened = Database::open(dir.clone()).unwrap();
    let stored_snapshot = reopened.zen_free_model_catalog().unwrap().unwrap();
    assert_eq!(
        stored_snapshot.models,
        vec!["review-model-free", "second-free"]
    );
    let persisted = reopened.load_persisted_contracts().unwrap();
    assert!(
        persisted
            .scopes
            .get(&scope)
            .is_some_and(|row| row.catalog_models.is_empty())
    );
    let restored =
        crate::provider_contracts::build_effective_contracts(&stored_snapshot, &[], persisted);
    let zen = restored.scope(&scope).unwrap();
    assert!(zen.catalog.models.is_empty());
    assert!(zen.model("review-model-free").is_none());
    assert!(!zen.model_has_enabled_protocol("review-model-free"));
    drop(reopened);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn zen_catalog_remove_all_at_once_stays_empty() {
    let dir = temp_data_dir("zen-catalog-remove-all");
    let now = Utc::now();
    let scope = ContractScope::provider(OPENCODE_ZEN_FREE_PROVIDER_ID);
    let snapshot = crate::kernel::zen::ZenFreeModelCatalog {
        models: vec!["review-model-free".into(), "second-free".into()],
        refreshed_at: Some(now),
        source_url: crate::kernel::zen::ZEN_MODELS_SOURCE_URL.into(),
    };
    let db = Database::open(dir.clone()).unwrap();
    db.set_zen_free_model_catalog_preserving_settings(&snapshot)
        .unwrap();
    db.remove_contract_catalog_models(
        &scope,
        &["review-model-free".into(), "second-free".into()],
        now,
    )
    .unwrap();
    let stored = db.load_persisted_scope(&scope).unwrap().unwrap();
    assert!(stored.catalog_models.is_empty());
    let live = crate::provider_contracts::build_effective_contracts(
        &snapshot,
        &[],
        db.load_persisted_contracts().unwrap(),
    );
    assert!(live.scope(&scope).unwrap().catalog.models.is_empty());
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn zen_official_static_preference_saves_responses_and_messages_not_probe_rows() {
    let dir = temp_data_dir("zen-official-protocol-preference");
    let now = Utc::now();
    let scope = ContractScope::provider(OPENCODE_ZEN_FREE_PROVIDER_ID);
    let db = Database::open(dir.clone()).unwrap();
    db.set_zen_free_model_catalog_preserving_settings(&crate::kernel::zen::ZenFreeModelCatalog {
        models: vec!["review-model-free".into(), "messages-model-free".into()],
        refreshed_at: Some(now),
        source_url: crate::kernel::zen::ZEN_MODELS_SOURCE_URL.into(),
    })
    .unwrap();
    db.apply_official_protocol_baseline(
        &scope,
        &["review-model-free".into(), "messages-model-free".into()],
        &crate::official_protocols::OfficialProtocolBaseline::mapped([
            ("review-model", UpstreamProtocolKind::Responses),
            ("messages-model", UpstreamProtocolKind::Messages),
        ]),
        now,
    )
    .unwrap();
    let saved = db.load_persisted_contracts().unwrap();
    let preferences = saved.preferences.get(&scope).cloned().unwrap_or_default();
    assert!(preferences.iter().any(|(model, protocol)| {
        model == "review-model-free" && *protocol == UpstreamProtocolKind::Responses
    }));
    assert!(preferences.iter().any(|(model, protocol)| {
        model == "messages-model-free" && *protocol == UpstreamProtocolKind::Messages
    }));
    db.set_model_protocol_settings(
        &scope,
        &[(
            "review-model-free".into(),
            UpstreamProtocolKind::Responses,
            ProtocolOverrideState::ForceOn,
        )],
        &[("review-model-free".into(), UpstreamProtocolKind::Responses)],
        now,
    )
    .unwrap();
    db.set_model_protocol_settings(
        &scope,
        &[(
            "messages-model-free".into(),
            UpstreamProtocolKind::Messages,
            ProtocolOverrideState::ForceOn,
        )],
        &[("messages-model-free".into(), UpstreamProtocolKind::Messages)],
        now,
    )
    .unwrap();

    db.upsert_model_protocol(&PersistedModelProtocol {
        scope: scope.clone(),
        model_id: "review-model-free".into(),
        protocol: UpstreamProtocolKind::Messages,
        source: ContractEvidenceSource::ProbeObserved,
        verified_at: Some(now),
        observed_at: Some(now),
        last_probe_result: Some(ProbeResultKind::Success),
        last_probe_at: Some(now),
        last_probe_error: None,
    })
    .unwrap();
    let rejected = db.set_model_protocol_settings(
        &scope,
        &[(
            "review-model-free".into(),
            UpstreamProtocolKind::Messages,
            ProtocolOverrideState::ForceOn,
        )],
        &[("review-model-free".into(), UpstreamProtocolKind::Messages)],
        now,
    );
    assert!(
        rejected.is_err(),
        "probe-manufactured evidence must not expand Zen preference admission: {rejected:?}"
    );
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn command_first_refresh_enables_goat_cohort_then_new_discoveries() {
    let dir = temp_data_dir("command-first-refresh-goat-cohort");
    let db = Database::open(dir.clone()).unwrap();
    let now = Utc::now();
    let scope = ContractScope::provider(COMMAND_CODE_PROVIDER_ID);
    let included = "gpt-6-luna".to_string();
    let premium = "vendor/premium-model".to_string();
    let preenabled = "vendor/saved-on".to_string();
    let later = "vendor/new-model".to_string();
    let baseline = crate::official_protocols::OfficialProtocolBaseline::mapped([
        (included.as_str(), UpstreamProtocolKind::ChatCompletions),
        (premium.as_str(), UpstreamProtocolKind::ChatCompletions),
        (preenabled.as_str(), UpstreamProtocolKind::ChatCompletions),
        (later.as_str(), UpstreamProtocolKind::ChatCompletions),
    ]);
    set_model_protocol_override_on(
        &db.conn,
        &scope,
        &preenabled,
        UpstreamProtocolKind::ChatCompletions,
        ProtocolOverrideState::ForceOn,
        now,
    )
    .unwrap();

    db.refresh_contract_catalog_preserving_settings(
        &scope,
        &[included.clone(), premium.clone(), preenabled.clone()],
        now,
        CATALOG_SOURCE_COMMAND_CODE_MODELS,
        COMMAND_CODE_GOAT_BASE_URL,
    )
    .unwrap();
    db.apply_official_protocol_baseline(
        &scope,
        &[included.clone(), premium.clone(), preenabled.clone()],
        &baseline,
        now,
    )
    .unwrap();
    let initial = effective_from_db(&db)
        .providers
        .remove(COMMAND_CODE_PROVIDER_ID)
        .unwrap();
    assert!(initial.model(&included).unwrap().has_enabled_protocol());
    assert!(!initial.model(&premium).unwrap().has_enabled_protocol());
    assert!(initial.model(&preenabled).unwrap().has_enabled_protocol());

    db.refresh_contract_catalog_preserving_settings(
        &scope,
        &[
            included.clone(),
            premium.clone(),
            preenabled.clone(),
            later.clone(),
        ],
        now,
        CATALOG_SOURCE_COMMAND_CODE_MODELS,
        COMMAND_CODE_GOAT_BASE_URL,
    )
    .unwrap();
    db.apply_official_protocol_baseline(
        &scope,
        &[included, premium.clone(), preenabled.clone(), later.clone()],
        &baseline,
        now,
    )
    .unwrap();
    let refreshed = effective_from_db(&db)
        .providers
        .remove(COMMAND_CODE_PROVIDER_ID)
        .unwrap();
    assert!(!refreshed.model(&premium).unwrap().has_enabled_protocol());
    assert!(refreshed.model(&preenabled).unwrap().has_enabled_protocol());
    assert!(refreshed.model(&later).unwrap().has_enabled_protocol());

    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn command_catalog_reappearing_preset_returns_to_auto_enabled() {
    let dir = temp_data_dir("command-catalog-reappearing-preset");
    let db = Database::open(dir.clone()).unwrap();
    let now = Utc::now();
    let scope = ContractScope::provider(COMMAND_CODE_PROVIDER_ID);
    let preset = COMMAND_CODE_GOAT_INCLUDED_MODEL_IDS[0].to_string();
    let extra = "vendor/future-command-model".to_string();

    db.set_contract_catalog(
        &scope,
        std::slice::from_ref(&preset),
        Some(now),
        CATALOG_SOURCE_COMMAND_CODE_MODELS,
        COMMAND_CODE_GOAT_BASE_URL,
        now,
    )
    .unwrap();
    db.refresh_contract_catalog_preserving_settings(
        &scope,
        std::slice::from_ref(&extra),
        now,
        CATALOG_SOURCE_COMMAND_CODE_MODELS,
        COMMAND_CODE_GOAT_BASE_URL,
    )
    .unwrap();
    db.refresh_contract_catalog_preserving_settings(
        &scope,
        &[extra.clone(), preset.clone()],
        now,
        CATALOG_SOURCE_COMMAND_CODE_MODELS,
        COMMAND_CODE_GOAT_BASE_URL,
    )
    .unwrap();

    let persisted = db.load_persisted_contracts().unwrap();
    let overrides = persisted.overrides.get(&scope);
    assert!(
        overrides.is_none_or(|rows| rows.is_empty()),
        "catalog refresh must not invent GOAT overrides: {overrides:?}"
    );
    let goat = effective_from_db(&db)
        .providers
        .remove(COMMAND_CODE_PROVIDER_ID)
        .unwrap();
    assert!(
        goat.model(&preset)
            .is_some_and(crate::provider_contracts::EffectiveModelContract::has_enabled_protocol)
    );
    assert!(
        goat.model(&extra)
            .is_some_and(|model| !model.has_enabled_protocol()),
        "GOAT extras stay off until official-docs Static evidence exists"
    );

    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

fn v35_column_names(conn: &Connection, table: &str) -> Vec<String> {
    let mut stmt = conn
        .prepare(&format!("PRAGMA table_info({table})"))
        .unwrap();
    stmt.query_map([], |row| row.get::<_, String>(1))
        .unwrap()
        .map(|column| column.unwrap())
        .collect()
}

fn v35_index_sql(conn: &Connection, name: &str) -> Option<String> {
    conn.query_row(
        "SELECT sql FROM sqlite_master WHERE type = 'index' AND name = ?1",
        [name],
        |row| row.get::<_, Option<String>>(0),
    )
    .optional()
    .unwrap()
    .flatten()
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
    sqlite_quick_check(&db.conn).unwrap();
    sqlite_foreign_key_check(&db.conn).unwrap();
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
    assert_eq!(schema_version_on(&conn).unwrap(), V34_SCHEMA_VERSION);
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
    assert_eq!(schema_version_on(&conn).unwrap(), V34_SCHEMA_VERSION);
    assert!(table_has_column(&conn, "provider_model_catalogs", "offering_id").unwrap());
    drop(conn);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn dynamic_provider_round_trip_and_duplicate_public_model_rejection() {
    let dir = temp_data_dir("v35-dynamic-providers");
    let db = open_with_host_cipher(dir.clone()).unwrap();
    assert_leftover_dynamic_provider_storage_absent(&db.conn);
    let dest_columns = v35_column_names(&db.conn, "destinations");
    for required in [
        "id",
        "name",
        "base_url",
        "legacy_kind",
        "legacy_id",
        "origin",
    ] {
        assert!(
            dest_columns.iter().any(|name| name == required),
            "{required}"
        );
    }
    let model_columns = v35_column_names(&db.conn, "destination_models");
    assert!(model_columns.iter().any(|name| name == "public_model_key"));
    assert!(model_columns.iter().any(|name| name == "upstream_override"));

    let now = Utc::now();
    let provider_id = uuid::Uuid::new_v4().to_string();
    let runtime = crate::dynamic::DynamicProviderRuntime {
        preset_id: None,
        id: provider_id.clone(),
        name: "Lab".into(),
        endpoint_url: "http://127.0.0.1:9".into(),
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
    let mut first = account("dyn-acct");
    first.provider_id = provider_id.clone();
    first.key_cipher = fixture_account_key_cipher();
    db.create_dynamic_provider(&runtime, &first).unwrap();
    let loaded = db.get_dynamic_provider(&provider_id).unwrap().unwrap();
    assert_eq!(loaded.name, "Lab");
    assert_eq!(loaded.mappings[0].public_model, "lab-opus");
    assert_eq!(db.count_accounts_for_provider(&provider_id).unwrap(), 1);

    let mut changed = loaded.clone();
    changed.mappings[0].upstream_override =
        Some(ocg_domain::dynamic::DynamicModelUpstreamOverride {
            protocol: crate::provider::UpstreamProtocolKind::Messages,
            endpoint_url: "https://example.test/anthropic/v1/messages".into(),
        });
    db.replace_dynamic_provider(&changed, true, false, None)
        .unwrap();
    let saved = db.get_dynamic_provider(&provider_id).unwrap().unwrap();
    assert_eq!(saved.mappings, changed.mappings);
    assert_eq!(saved.upstream_protocol, loaded.upstream_protocol);
    assert_eq!(
        db.get_account(&first.id).unwrap().unwrap().key_cipher,
        first.key_cipher
    );
    db.replace_dynamic_provider(&loaded, true, false, None)
        .unwrap();
    assert!(
        db.get_dynamic_provider(&provider_id)
            .unwrap()
            .unwrap()
            .mappings[0]
            .upstream_override
            .is_none()
    );

    let dest_id = ocg_domain::destination::destination_id_for_dynamic(&provider_id);
    let duplicate = db.conn.execute(
        "INSERT INTO destination_models
         (destination_id, public_model, public_model_key, upstream_model,
          protocols_json, preferred, enabled)
         VALUES (?1, 'LAB-OPUS', 'lab-opus', 'other', '[]', NULL, 1)",
        [&dest_id],
    );
    assert!(duplicate.is_err());
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn replace_dynamic_provider_keeps_persisted_credential_projection_in_sync() {
    let dir = temp_data_dir("dynamic-provider-projection-sync");
    let db = open_with_host_cipher(dir.clone()).unwrap();
    let now = Utc::now();
    let provider_id = uuid::Uuid::new_v4().to_string();
    let runtime = crate::dynamic::DynamicProviderRuntime {
        preset_id: None,
        id: provider_id.clone(),
        name: "Projection sync".into(),
        endpoint_url: "http://127.0.0.1:9".into(),
        upstream_protocol: crate::provider::UpstreamProtocolKind::ChatCompletions,
        auth_kind: ocg_domain::dynamic::DynamicAuthKind::Bearer,
        mappings: vec![ocg_domain::dynamic::DynamicModelMapping {
            public_model: "lab-model".into(),
            upstream_model: "vendor/model".into(),
            upstream_override: None,
        }],
        created_at: now,
        updated_at: now,
        origin: ocg_domain::provider::ProviderOrigin::Custom,
        offering: "api".to_string(),
    };
    let mut account = account("dynamic-projection-account");
    account.provider_id = provider_id.clone();
    account.key_cipher = fixture_account_key_cipher();
    db.create_dynamic_provider(&runtime, &account).unwrap();

    db.conn
        .execute(
            "UPDATE credentials
             SET auth_error = 'stale auth failure', auth_state = 'invalid'
             WHERE legacy_account_id = ?1",
            [&account.id],
        )
        .unwrap();
    let mut edited = runtime.clone();
    edited.endpoint_url = "http://127.0.0.1:10".into();
    db.replace_dynamic_provider(&edited, true, false, None)
        .unwrap();
    let auth_state: String = db
        .conn
        .query_row(
            "SELECT auth_state FROM credentials WHERE legacy_account_id = ?1",
            [&account.id],
            |row| row.get(0),
        )
        .unwrap();
    assert_ne!(
        auth_state, "invalid",
        "cleared auth_error must update the projection"
    );

    let mut none = edited.clone();
    none.auth_kind = ocg_domain::dynamic::DynamicAuthKind::None;
    db.replace_dynamic_provider(&none, true, true, None)
        .unwrap();
    let has_secret: i64 = db
        .conn
        .query_row(
            "SELECT has_secret FROM credentials WHERE legacy_account_id = ?1",
            [&account.id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        has_secret, 0,
        "bearer to none must clear projected secret state"
    );

    let mut bearer = none;
    bearer.auth_kind = ocg_domain::dynamic::DynamicAuthKind::Bearer;
    let replacement = test_host_cipher().encrypt("sk-replacement").unwrap();
    db.replace_dynamic_provider(&bearer, true, false, Some(&replacement))
        .unwrap();
    let has_secret: i64 = db
        .conn
        .query_row(
            "SELECT has_secret FROM credentials WHERE legacy_account_id = ?1",
            [&account.id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        has_secret, 1,
        "none to bearer must set projected secret state"
    );

    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn create_dynamic_provider_definition_persists_without_an_account() {
    let dir = temp_data_dir("dyn-definition-only");
    let db = open_with_host_cipher(dir.clone()).unwrap();
    let now = Utc::now();
    let provider_id = uuid::Uuid::new_v4().to_string();
    let runtime = crate::dynamic::DynamicProviderRuntime {
        preset_id: Some("tencent-token-global".into()),
        id: provider_id.clone(),
        name: "Tencent Token Plan".into(),
        endpoint_url: "http://127.0.0.1:9".into(),
        upstream_protocol: crate::provider::UpstreamProtocolKind::ChatCompletions,
        auth_kind: ocg_domain::dynamic::DynamicAuthKind::Bearer,
        mappings: vec![ocg_domain::dynamic::DynamicModelMapping {
            public_model: "tencent-model".into(),
            upstream_model: "tencent/upstream".into(),
            upstream_override: None,
        }],
        created_at: now,
        updated_at: now,
        origin: ocg_domain::provider::ProviderOrigin::Preset,
        offering: "plan".to_string(),
    };
    db.create_dynamic_provider_definition(&runtime).unwrap();
    let loaded = db.get_dynamic_provider(&provider_id).unwrap().unwrap();
    assert_eq!(loaded.name, "Tencent Token Plan");
    assert_eq!(loaded.preset_id.as_deref(), Some("tencent-token-global"));
    assert_eq!(db.count_accounts_for_provider(&provider_id).unwrap(), 0);
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn dynamic_provider_create_fault_rolls_back_provider_and_account() {
    let dir = temp_data_dir("dyn-create-fault");
    let db = open_with_host_cipher(dir.clone()).unwrap();
    let now = Utc::now();
    let provider_id = uuid::Uuid::new_v4().to_string();
    let runtime = crate::dynamic::DynamicProviderRuntime {
        preset_id: None,
        id: provider_id.clone(),
        name: "Faulty".into(),
        endpoint_url: "http://127.0.0.1:9".into(),
        upstream_protocol: crate::provider::UpstreamProtocolKind::Responses,
        auth_kind: ocg_domain::dynamic::DynamicAuthKind::None,
        mappings: vec![ocg_domain::dynamic::DynamicModelMapping {
            public_model: "free-model".into(),
            upstream_model: "free-model".into(),
            upstream_override: None,
        }],
        created_at: now,
        updated_at: now,
        origin: ocg_domain::provider::ProviderOrigin::Custom,
        offering: "api".to_string(),
    };
    let mut first = account("dyn-none");
    first.provider_id = provider_id.clone();
    first.credential_kind = crate::provider::CredentialKind::None;
    first.key_cipher = String::new();
    crate::db::dynamic_provider_fault::install("after_account_insert");
    let error = db.create_dynamic_provider(&runtime, &first).unwrap_err();
    crate::db::dynamic_provider_fault::clear();
    assert!(
        error
            .to_string()
            .contains("injected dynamic provider fault")
    );
    assert!(db.get_dynamic_provider(&provider_id).unwrap().is_none());
    assert_eq!(db.count_accounts_for_provider(&provider_id).unwrap(), 0);
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

fn onboarding_runtime(provider_id: &str, name: &str) -> crate::dynamic::DynamicProviderRuntime {
    let now = Utc::now();
    crate::dynamic::DynamicProviderRuntime {
        preset_id: None,
        id: provider_id.to_string(),
        name: name.into(),
        endpoint_url: "http://127.0.0.1:9".into(),
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
    }
}

fn onboarding_operation(
    operation_id: &str,
    digest: &str,
    result_json: &str,
) -> NewDashboardOperation {
    NewDashboardOperation {
        operation_id: operation_id.to_string(),
        kind: "onboarding_commit".into(),
        payload_digest: digest.to_string(),
        result_json: result_json.to_string(),
    }
}

#[test]
fn onboarding_resume_route_changes_remap_existing_key_grants_without_authorizing_new_routes() {
    use ocg_domain::connection::{
        EndpointOperation, LegacyConnectionKind, connection_id_for_legacy,
    };
    use ocg_domain::credential::{RouteSpec, assigned_endpoints_for_routes, remap_route_grant_ids};
    use ocg_domain::destination::{HttpProtocolRoute, Protocol};

    let dir = temp_data_dir("onboard-resume-route-remap");
    let db = open_with_host_cipher(dir.clone()).unwrap();
    let provider_id = uuid::Uuid::new_v4().to_string();
    let old_chat = "https://resume-old.example/v1/chat/completions";
    let old_messages = "https://resume-old.example/v1/messages";
    let old_responses = "https://resume-other.example/v1/responses";
    let new_responses = "https://resume-new.example/v1/responses";
    let mut initial = onboarding_runtime(&provider_id, "Resume Routes");
    initial.endpoint_url = old_chat.into();
    initial.upstream_protocol = UpstreamProtocolKind::ChatCompletions;
    initial.auth_kind = DynamicAuthKind::Bearer;
    let old_routes = vec![
        HttpProtocolRoute {
            protocol: Protocol::ChatCompletions,
            endpoint_url: old_chat.into(),
            auth_scheme: AuthScheme::Bearer,
        },
        HttpProtocolRoute {
            protocol: Protocol::Messages,
            endpoint_url: old_messages.into(),
            auth_scheme: AuthScheme::Bearer,
        },
        HttpProtocolRoute {
            protocol: Protocol::Responses,
            endpoint_url: old_responses.into(),
            auth_scheme: AuthScheme::Bearer,
        },
    ];
    let mut first = account("resume-route-key");
    first.provider_id = provider_id.clone();
    first.key_cipher = fixture_account_key_cipher();
    db.commit_onboarding_new_with_routes(
        &initial,
        Some(&first),
        true,
        &onboarding_operation(
            &uuid::Uuid::new_v4().to_string(),
            "resume-route-draft",
            "{}",
        ),
        Some(&old_routes),
    )
    .unwrap();
    let destination_id = ocg_domain::destination::destination_id_for_dynamic(&provider_id);
    let mut draft_catalog =
        destination_store::load_destination_catalog(&db.conn, &destination_id).unwrap();
    draft_catalog[0].protocols = vec![Protocol::Messages];
    draft_catalog[0].preferred = Some(Protocol::Messages);
    draft_catalog[0].enabled = false;
    destination_store::replace_destination_catalog(&db.conn, &destination_id, &draft_catalog)
        .unwrap();
    let connection = connection_id_for_legacy(LegacyConnectionKind::DynamicProvider, &provider_id);
    let to_spec = |route: &HttpProtocolRoute| RouteSpec {
        operation: EndpointOperation::from(route.protocol),
        url: Some(route.endpoint_url.clone()),
    };
    let old_specs = old_routes.iter().map(to_spec).collect::<Vec<_>>();
    let before = db
        .list_identity_model()
        .unwrap()
        .accounts
        .into_iter()
        .find(|row| row.account.id == first.id)
        .unwrap();
    let expected_old_grants = before.allowed_endpoint_ids.clone();
    let mut updated = initial.clone();
    updated.endpoint_url = new_responses.into();
    updated.upstream_protocol = UpstreamProtocolKind::Responses;
    updated.auth_kind = DynamicAuthKind::ApiKey;
    let new_routes = vec![
        HttpProtocolRoute {
            protocol: Protocol::Responses,
            endpoint_url: new_responses.into(),
            auth_scheme: AuthScheme::ApiKey,
        },
        HttpProtocolRoute {
            protocol: Protocol::Messages,
            endpoint_url: old_messages.into(),
            auth_scheme: AuthScheme::XApiKey,
        },
        HttpProtocolRoute {
            protocol: Protocol::ChatCompletions,
            endpoint_url: old_chat.into(),
            auth_scheme: AuthScheme::XApiKey,
        },
    ];
    let new_specs = new_routes.iter().map(to_spec).collect::<Vec<_>>();
    let expected = remap_route_grant_ids(&connection, &old_specs, &new_specs, &expected_old_grants);
    db.commit_onboarding_resume_with_routes(
        &updated,
        false,
        None,
        None,
        None,
        None,
        None,
        &onboarding_operation(
            &uuid::Uuid::new_v4().to_string(),
            "resume-route-complete",
            "{}",
        ),
        Some(&new_routes),
    )
    .unwrap();
    let persisted = crate::destination_projection::load_persisted(&db).unwrap();
    let destination = persisted
        .destinations
        .iter()
        .find(|destination| destination.id == destination_id)
        .unwrap();
    assert_eq!(destination.protocol_routes, new_routes);
    assert_eq!(destination.catalog[0].protocols, vec![Protocol::Messages]);
    assert_eq!(destination.catalog[0].preferred, Some(Protocol::Messages));
    assert!(!destination.catalog[0].enabled);
    let after = persisted
        .credentials
        .iter()
        .find(|credential| credential.legacy_account_id == first.id)
        .unwrap();
    assert_eq!(after.grants.allowed_endpoint_ids, expected);
    assert!(
        !after
            .grants
            .allowed_endpoint_ids
            .iter()
            .any(|id| id == &assigned_endpoints_for_routes(&connection, &new_specs)[0].id),
        "the new first Responses route requires explicit authorization"
    );
    assert_eq!(
        after.grants.allowed_origins,
        vec!["https://resume-old.example"]
    );
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn catalog_refresh_preserves_disabled_models_when_baseline_adds_responses() {
    let dir = temp_data_dir("catalog-refresh-disabled-baseline");
    let db = open_with_host_cipher(dir.clone()).unwrap();
    let now = Utc::now();
    for (provider_id, old_model, new_model, source, source_url) in [
        (
            OPENCODE_PROVIDER_ID,
            "grok-4.5",
            "new-go-model",
            crate::provider_contracts::CATALOG_SOURCE_OPENCODE_MODELS,
            "https://opencode.ai/zen/go/v1/models",
        ),
        (
            COMMAND_CODE_PROVIDER_ID,
            COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM,
            "vendor/new-command-model",
            CATALOG_SOURCE_COMMAND_CODE_MODELS,
            COMMAND_CODE_GOAT_BASE_URL,
        ),
    ] {
        let scope = ContractScope::provider(provider_id);
        db.set_contract_catalog(
            &scope,
            &[old_model.into()],
            Some(now),
            source,
            source_url,
            now,
        )
        .unwrap();
        db.upsert_model_protocol(&PersistedModelProtocol {
            scope: scope.clone(),
            model_id: old_model.into(),
            protocol: UpstreamProtocolKind::ChatCompletions,
            source: ContractEvidenceSource::Static,
            verified_at: Some(now),
            observed_at: Some(now),
            last_probe_result: Some(ProbeResultKind::Success),
            last_probe_at: Some(now),
            last_probe_error: None,
        })
        .unwrap();
        db.set_model_protocol_settings(
            &scope,
            &[(
                old_model.into(),
                UpstreamProtocolKind::ChatCompletions,
                ProtocolOverrideState::ForceOff,
            )],
            &[(old_model.into(), UpstreamProtocolKind::ChatCompletions)],
            now,
        )
        .unwrap();
        let before_destination = crate::destination_projection::load_persisted(&db)
            .unwrap()
            .destinations
            .into_iter()
            .find(|destination| {
                destination.id == ocg_domain::destination::destination_id_for_builtin(provider_id)
            })
            .unwrap();
        assert!(
            !before_destination
                .catalog
                .iter()
                .find(|model| model.public_model.eq_ignore_ascii_case(old_model))
                .unwrap()
                .enabled
        );
        assert!(
            !db.load_persisted_contracts().unwrap().overrides[&scope]
                .iter()
                .any(|row| row.protocol == UpstreamProtocolKind::Responses),
            "new Responses protocol must start Auto so this test detects accidental reopening"
        );
        let before_evidence = db
            .load_persisted_contracts()
            .unwrap()
            .evidence
            .get(&scope)
            .cloned()
            .unwrap();
        db.refresh_contract_catalog_preserving_settings(
            &scope,
            &[old_model.into(), new_model.into()],
            now,
            source,
            source_url,
        )
        .unwrap();
        db.apply_official_protocol_baseline(
            &scope,
            &[old_model.into(), new_model.into()],
            &crate::official_protocols::OfficialProtocolBaseline::mapped_protocols([
                (
                    old_model,
                    vec![
                        UpstreamProtocolKind::ChatCompletions,
                        UpstreamProtocolKind::Responses,
                    ],
                ),
                (new_model, vec![UpstreamProtocolKind::Responses]),
            ]),
            now,
        )
        .unwrap();
        let persisted = db.load_persisted_contracts().unwrap();
        assert!(
            persisted.preferences[&scope]
                .iter()
                .any(|(model, protocol)| {
                    model == old_model && *protocol == UpstreamProtocolKind::ChatCompletions
                })
        );
        assert!(before_evidence.iter().all(|before| {
            persisted.evidence[&scope]
                .iter()
                .any(|after| after == before)
        }));
        let effective = effective_from_db(&db);
        let contract = effective.providers.get(provider_id).unwrap();
        let old = contract.model(old_model).unwrap();
        let new = contract.model(new_model).unwrap();
        assert!(
            !old.has_enabled_protocol(),
            "old disabled model was resurrected"
        );
        assert!(new.protocols["responses"].enabled);
        let destination_id = ocg_domain::destination::destination_id_for_builtin(provider_id);
        let destination = crate::destination_projection::load_persisted(&db)
            .unwrap()
            .destinations
            .into_iter()
            .find(|destination| destination.id == destination_id)
            .unwrap();
        assert!(
            !destination
                .catalog
                .iter()
                .find(|model| model.public_model.eq_ignore_ascii_case(old_model))
                .unwrap()
                .enabled
        );
        assert!(
            destination
                .catalog
                .iter()
                .find(|model| model.public_model.eq_ignore_ascii_case(new_model))
                .unwrap()
                .enabled
        );
    }
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn raw_disabled_unknown_model_stays_off_when_first_official_protocol_arrives() {
    let dir = temp_data_dir("raw-disabled-unknown-model");
    let db = open_with_host_cipher(dir.clone()).unwrap();
    let scope = ContractScope::provider(COMMAND_CODE_PROVIDER_ID);
    let model = "vendor/awaiting-official-evidence";
    let now = Utc::now();
    db.set_contract_catalog(
        &scope,
        &[model.into()],
        Some(now),
        CATALOG_SOURCE_COMMAND_CODE_MODELS,
        COMMAND_CODE_GOAT_BASE_URL,
        now,
    )
    .unwrap();
    db.set_model_protocol_overrides(
        &scope,
        &[(
            model.into(),
            UpstreamProtocolKind::ChatCompletions,
            ProtocolOverrideState::ForceOff,
        )],
        now,
    )
    .unwrap();
    assert!(
        !effective_from_db(&db)
            .scope(&scope)
            .unwrap()
            .model(model)
            .unwrap()
            .has_enabled_protocol()
    );
    db.refresh_contract_catalog_preserving_settings(
        &scope,
        &[model.into()],
        now,
        CATALOG_SOURCE_COMMAND_CODE_MODELS,
        COMMAND_CODE_GOAT_BASE_URL,
    )
    .unwrap();
    db.apply_official_protocol_baseline(
        &scope,
        &[model.into()],
        &crate::official_protocols::OfficialProtocolBaseline::mapped([(
            model,
            UpstreamProtocolKind::Responses,
        )]),
        now,
    )
    .unwrap();
    assert!(
        !effective_from_db(&db)
            .scope(&scope)
            .unwrap()
            .model(model)
            .unwrap()
            .has_enabled_protocol()
    );
    let id = ocg_domain::destination::destination_id_for_builtin(COMMAND_CODE_PROVIDER_ID);
    assert!(!destination_store::load_destination_catalog(&db.conn, &id).unwrap()[0].enabled);
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn commit_transaction_fault_after_provider_insert_leaves_no_partial_rows() {
    let dir = temp_data_dir("onboard-provider-fault");
    let db = open_with_host_cipher(dir.clone()).unwrap();
    let provider_id = uuid::Uuid::new_v4().to_string();
    let runtime = onboarding_runtime(&provider_id, "FaultyOnboard");
    let mut first = account("onboard-fault");
    first.provider_id = provider_id.clone();
    first.key_cipher = fixture_account_key_cipher();
    let operation = onboarding_operation(
        &uuid::Uuid::new_v4().to_string(),
        "digest-not-a-secret",
        r#"{"connectionId":"c","credentialId":"a","targetIds":[]}"#,
    );
    crate::db::dynamic_provider_fault::install("after_provider_insert");
    let error = db
        .commit_onboarding_new(&runtime, Some(&first), false, &operation)
        .unwrap_err();
    crate::db::dynamic_provider_fault::clear();
    assert!(
        error
            .to_string()
            .contains("injected dynamic provider fault")
    );
    assert!(db.get_dynamic_provider(&provider_id).unwrap().is_none());
    assert_eq!(db.count_accounts_for_provider(&provider_id).unwrap(), 0);
    assert!(
        db.find_dashboard_operation(&operation.operation_id)
            .unwrap()
            .is_none()
    );
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn dashboard_operations_prune_rows_older_than_30_days_on_insert() {
    let dir = temp_data_dir("onboard-prune");
    let db = open_with_host_cipher(dir.clone()).unwrap();
    let old_id = uuid::Uuid::new_v4().to_string();
    let old_time = (Utc::now() - Duration::days(31)).to_rfc3339();
    db.conn
        .execute(
            "INSERT INTO dashboard_operations
             (operation_id, kind, payload_digest, result_json, created_at)
             VALUES (?1, 'onboarding_commit', 'old-digest', '{}', ?2)",
            rusqlite::params![old_id, old_time],
        )
        .unwrap();
    let provider_id = uuid::Uuid::new_v4().to_string();
    let runtime = onboarding_runtime(&provider_id, "PruneOnboard");
    let mut first = account("onboard-prune");
    first.provider_id = provider_id.clone();
    first.key_cipher = fixture_account_key_cipher();
    let new_id = uuid::Uuid::new_v4().to_string();
    db.commit_onboarding_new(
        &runtime,
        Some(&first),
        false,
        &onboarding_operation(&new_id, "new-digest", "{}"),
    )
    .unwrap();
    assert!(db.find_dashboard_operation(&old_id).unwrap().is_none());
    assert!(db.find_dashboard_operation(&new_id).unwrap().is_some());
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn onboarding_operation_ledger_preserves_result_without_account_cipher() {
    let dir = temp_data_dir("onboard-secret-free");
    let db = open_with_host_cipher(dir.clone()).unwrap();
    let provider_id = uuid::Uuid::new_v4().to_string();
    let runtime = onboarding_runtime(&provider_id, "SecretOnboard");
    let mut first = account("onboard-secret");
    first.provider_id = provider_id.clone();
    first.key_cipher = fixture_account_key_cipher();
    let operation_id = uuid::Uuid::new_v4().to_string();
    let result_json = r#"{"connectionId":"conn-1","credentialId":"acct-1","targetIds":["t1"]}"#;
    db.commit_onboarding_new(
        &runtime,
        Some(&first),
        false,
        &onboarding_operation(&operation_id, "hmac-digest-without-secret", result_json),
    )
    .unwrap();
    let row = db
        .find_dashboard_operation(&operation_id)
        .unwrap()
        .expect("operation row");
    assert_eq!(row.result_json, result_json);
    assert_eq!(row.payload_digest, "hmac-digest-without-secret");
    for haystack in [&row.result_json, &row.payload_digest] {
        assert!(
            !haystack.contains(&first.key_cipher),
            "key cipher leaked in {haystack}"
        );
    }
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn dynamic_provider_patch_fault_rolls_back_mappings_and_runtime_state() {
    let dir = temp_data_dir("dyn-patch-fault");
    let db = open_with_host_cipher(dir.clone()).unwrap();
    let now = Utc::now();
    let provider_id = uuid::Uuid::new_v4().to_string();
    let runtime = crate::dynamic::DynamicProviderRuntime {
        preset_id: None,
        id: provider_id.clone(),
        name: "PatchFault".into(),
        endpoint_url: "http://127.0.0.1:9".into(),
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
    let mut first = account("dyn-patch");
    first.provider_id = provider_id.clone();
    first.key_cipher = fixture_account_key_cipher();
    db.create_dynamic_provider(&runtime, &first).unwrap();
    db.conn
        .execute(
            "UPDATE credentials SET auth_error = 'stale' WHERE legacy_account_id = ?1",
            [&first.id],
        )
        .unwrap();

    let mut updated = runtime.clone();
    updated.endpoint_url = "http://127.0.0.1:10".into();
    updated.mappings = vec![ocg_domain::dynamic::DynamicModelMapping {
        public_model: "lab-opus".into(),
        upstream_model: "vendor/opus-2".into(),
        upstream_override: None,
    }];
    crate::db::dynamic_provider_fault::install("after_mapping_replace");
    let error = db
        .replace_dynamic_provider(&updated, true, false, None)
        .unwrap_err();
    crate::db::dynamic_provider_fault::clear();
    assert!(
        error
            .to_string()
            .contains("injected dynamic provider fault")
    );
    let loaded = db.get_dynamic_provider(&provider_id).unwrap().unwrap();
    assert_eq!(loaded.endpoint_url, "http://127.0.0.1:9");
    assert_eq!(loaded.mappings[0].upstream_model, "vendor/opus");
    let account = db.get_account(&first.id).unwrap().unwrap();
    assert_eq!(account.auth_error.as_deref(), Some("stale"));
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn replace_dynamic_provider_refuses_to_fan_out_a_replacement_key() {
    let dir = temp_data_dir("dyn-no-fanout");
    let db = open_with_host_cipher(dir.clone()).unwrap();
    let now = Utc::now();
    let provider_id = uuid::Uuid::new_v4().to_string();
    let runtime = crate::dynamic::DynamicProviderRuntime {
        preset_id: None,
        id: provider_id.clone(),
        name: "Fanout".into(),
        endpoint_url: "http://127.0.0.1:9".into(),
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
    let mut first = account("dyn-fanout-1");
    first.provider_id = provider_id.clone();
    first.key_cipher = fixture_account_key_cipher();
    db.create_dynamic_provider(&runtime, &first).unwrap();
    let mut second = account("dyn-fanout-2");
    second.provider_id = provider_id.clone();
    second.key_cipher = test_host_cipher().encrypt("sk-second").unwrap();
    db.create_account(&second).unwrap();

    let error = db
        .replace_dynamic_provider(&runtime, false, false, Some("cipher-new"))
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("replacement Key requires exactly one credential"),
        "{error}"
    );
    let first_loaded = db.get_account(&first.id).unwrap().unwrap();
    let second_loaded = db.get_account(&second.id).unwrap().unwrap();
    assert_eq!(first_loaded.key_cipher, first.key_cipher);
    assert_eq!(second_loaded.key_cipher, second.key_cipher);
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn imported_dynamic_auth_change_rejects_destination_only_accounts() {
    let dir = temp_data_dir("dyn-import-auth-conflict");
    let db = open_with_host_cipher(dir.clone()).unwrap();
    let now = Utc::now();
    let provider_id = uuid::Uuid::new_v4().to_string();
    let mut runtime = crate::dynamic::DynamicProviderRuntime {
        preset_id: None,
        id: provider_id.clone(),
        name: "Auth conflict".into(),
        endpoint_url: "http://127.0.0.1:9".into(),
        upstream_protocol: crate::provider::UpstreamProtocolKind::ChatCompletions,
        auth_kind: ocg_domain::dynamic::DynamicAuthKind::None,
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
    let mut destination_only = account("dyn-destination-only");
    destination_only.provider_id = provider_id.clone();
    destination_only.credential_kind = CredentialKind::None;
    destination_only.key_cipher.clear();
    db.create_dynamic_provider(&runtime, &destination_only)
        .unwrap();

    runtime.auth_kind = ocg_domain::dynamic::DynamicAuthKind::Bearer;
    let error = upsert_imported_dynamic_provider_on(&db.conn, &runtime, &HashSet::new(), false)
        .expect_err("destination-only account must block an auth-boundary change");
    assert!(
        error.to_string().contains("destination-only accounts"),
        "{error}"
    );
    assert_eq!(
        db.get_dynamic_provider(&provider_id)
            .unwrap()
            .unwrap()
            .auth_kind,
        ocg_domain::dynamic::DynamicAuthKind::None
    );

    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn ollama_billing_create_failure_rolls_back_the_account_row() {
    let dir = temp_data_dir("ollama-billing-atomic");
    let db = open_with_host_cipher(dir.clone()).unwrap();
    let mut ollama = account("ollama-atomic");
    ollama.provider_id = OLLAMA_PROVIDER_ID.to_string();
    ollama.key_cipher = fixture_account_key_cipher();
    ollama.purchase_date = "2026-08-01".into();
    db.conn
        .execute_batch(
            "CREATE TRIGGER fail_ollama_billing
                 BEFORE INSERT ON ollama_cloud_billing
                 BEGIN
                     SELECT RAISE(ABORT, 'forced ollama billing failure');
                 END;",
        )
        .unwrap();
    let error = db
        .create_account_with_contract_and_billing(&ollama, None, &[], Some(OllamaBillingTier::Pro))
        .expect_err("billing failure should abort the create");
    assert!(
        error.to_string().contains("forced ollama billing failure"),
        "{error}"
    );
    assert!(db.get_account("ollama-atomic").unwrap().is_none());
    assert_eq!(db.ollama_cloud_billing_tier("ollama-atomic").unwrap(), None);
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn ollama_billing_update_failure_preserves_account_fields_and_key() {
    let dir = temp_data_dir("ollama-billing-update-atomic");
    let db = open_with_host_cipher(dir.clone()).unwrap();
    let original_key = fixture_account_key_cipher();
    let replacement_key = test_host_cipher()
        .encrypt("sk-replacement")
        .expect("replacement key should encrypt");
    let mut ollama = account("ollama-update-atomic");
    ollama.provider_id = OLLAMA_PROVIDER_ID.to_string();
    ollama.name = "original-name".into();
    ollama.key_cipher = original_key.clone();
    ollama.purchase_date = "2026-08-01".into();
    db.create_account_with_contract_and_billing(&ollama, None, &[], Some(OllamaBillingTier::Pro))
        .unwrap();

    db.conn
        .execute_batch(
            "CREATE TRIGGER fail_ollama_billing_update
                 BEFORE INSERT ON ollama_cloud_billing
                 BEGIN
                     SELECT RAISE(ABORT, 'forced ollama billing update failure');
                 END;",
        )
        .unwrap();
    let rename = AccountUpdate {
        name: Some("renamed".into()),
        ..AccountUpdate::default()
    };
    let error = db
        .update_account_with_billing(
            "ollama-update-atomic",
            &rename,
            Some(&replacement_key),
            None,
            Some(Some(OllamaBillingTier::Max)),
        )
        .expect_err("billing failure should abort the account update");
    assert!(
        error
            .to_string()
            .contains("forced ollama billing update failure"),
        "{error}"
    );
    let rolled_back = db.get_account("ollama-update-atomic").unwrap().unwrap();
    assert_eq!(rolled_back.name, "original-name");
    assert_eq!(rolled_back.key_cipher, original_key);
    assert_eq!(
        db.ollama_cloud_billing_tier("ollama-update-atomic")
            .unwrap(),
        Some(OllamaBillingTier::Pro)
    );

    db.conn
        .execute_batch("DROP TRIGGER fail_ollama_billing_update;")
        .unwrap();
    db.update_account_with_billing(
        "ollama-update-atomic",
        &rename,
        Some(&replacement_key),
        None,
        Some(Some(OllamaBillingTier::Max)),
    )
    .unwrap();
    let updated = db.get_account("ollama-update-atomic").unwrap().unwrap();
    assert_eq!(updated.name, "renamed");
    assert_eq!(updated.key_cipher, replacement_key);
    assert_eq!(
        db.ollama_cloud_billing_tier("ollama-update-atomic")
            .unwrap(),
        Some(OllamaBillingTier::Max)
    );

    db.update_account_with_billing(
        "ollama-update-atomic",
        &AccountUpdate {
            name: Some("name-only".into()),
            ..AccountUpdate::default()
        },
        None,
        None,
        None,
    )
    .unwrap();
    let name_only = db.get_account("ollama-update-atomic").unwrap().unwrap();
    assert_eq!(name_only.name, "name-only");
    assert_eq!(name_only.key_cipher, replacement_key);
    assert_eq!(
        db.ollama_cloud_billing_tier("ollama-update-atomic")
            .unwrap(),
        Some(OllamaBillingTier::Max)
    );

    db.update_account_with_billing(
        "ollama-update-atomic",
        &AccountUpdate {
            name: Some("cleared".into()),
            ..AccountUpdate::default()
        },
        None,
        None,
        Some(None),
    )
    .unwrap();
    let cleared = db.get_account("ollama-update-atomic").unwrap().unwrap();
    assert_eq!(cleared.name, "cleared");
    assert_eq!(cleared.key_cipher, replacement_key);
    assert_eq!(
        db.ollama_cloud_billing_tier("ollama-update-atomic")
            .unwrap(),
        None
    );

    drop(db);
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

#[test]
fn ollama_billing_tier_round_trip_and_cascade() {
    let dir = temp_data_dir("ollama-billing-roundtrip");
    let mut db = Database::open(dir.clone()).unwrap();
    let mut ollama = account("ollama-bill");
    ollama.provider_id = OLLAMA_PROVIDER_ID.to_string();
    ollama.purchase_date = "2026-08-01".into();
    db.create_account(&ollama).unwrap();
    db.set_ollama_cloud_billing_tier("ollama-bill", Some(OllamaBillingTier::Pro))
        .unwrap();
    assert_eq!(
        db.ollama_cloud_billing_tier("ollama-bill").unwrap(),
        Some(OllamaBillingTier::Pro)
    );
    db.set_ollama_cloud_billing_tier("ollama-bill", None)
        .unwrap();
    assert_eq!(db.ollama_cloud_billing_tier("ollama-bill").unwrap(), None);
    db.set_ollama_cloud_billing_tier("ollama-bill", Some(OllamaBillingTier::Team))
        .unwrap();
    db.delete_account("ollama-bill").unwrap();
    assert_eq!(db.ollama_cloud_billing_tier("ollama-bill").unwrap(), None);
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn ollama_tier_change_clears_month_offset_same_tier_preserves() {
    let dir = temp_data_dir("ollama-tier-offset");
    let db = Database::open(dir.clone()).unwrap();
    let mut ollama = account("ollama-offset");
    ollama.provider_id = OLLAMA_PROVIDER_ID.to_string();
    ollama.purchase_date = "2026-08-01".into();
    db.create_account(&ollama).unwrap();
    db.set_ollama_cloud_billing_tier("ollama-offset", Some(OllamaBillingTier::Pro))
        .unwrap();
    db.conn
        .execute(
            "UPDATE credentials SET usage_month_window_cost_offset = 12.5 WHERE legacy_account_id = ?1",
            ["ollama-offset"],
        )
        .unwrap();
    db.set_ollama_cloud_billing_tier("ollama-offset", Some(OllamaBillingTier::Pro))
        .unwrap();
    let offset: f64 = db
        .conn
        .query_row(
            "SELECT usage_month_window_cost_offset FROM credentials WHERE legacy_account_id = ?1",
            ["ollama-offset"],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(offset, 12.5);
    db.set_ollama_cloud_billing_tier("ollama-offset", Some(OllamaBillingTier::Max))
        .unwrap();
    let offset: f64 = db
        .conn
        .query_row(
            "SELECT usage_month_window_cost_offset FROM credentials WHERE legacy_account_id = ?1",
            ["ollama-offset"],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(offset, 0.0);
    assert_eq!(db.list_forward_logs(10).unwrap().len(), 0);
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn ollama_month_window_is_half_open_and_exposes_overage() {
    use chrono::TimeZone;
    let dir = temp_data_dir("ollama-month-bounds");
    let db = Database::open(dir.clone()).unwrap();
    let mut ollama = account("ollama-month");
    ollama.provider_id = OLLAMA_PROVIDER_ID.to_string();
    ollama.purchase_date = "2026-08-01".into();
    db.create_account(&ollama).unwrap();
    db.set_ollama_cloud_billing_tier("ollama-month", Some(OllamaBillingTier::Pro))
        .unwrap();

    let start_naive = chrono::NaiveDate::from_ymd_opt(2026, 8, 1)
        .unwrap()
        .and_hms_opt(0, 0, 0)
        .unwrap();
    let start = chrono::Local
        .from_local_datetime(&start_naive)
        .single()
        .unwrap()
        .with_timezone(&Utc);
    let expires = purchase_expires_on("2026-08-01").unwrap();
    let end_naive = chrono::NaiveDate::parse_from_str(&expires, "%Y-%m-%d")
        .unwrap()
        .and_hms_opt(0, 0, 0)
        .unwrap();
    let end = chrono::Local
        .from_local_datetime(&end_naive)
        .single()
        .unwrap()
        .with_timezone(&Utc);

    let mut before = forward_log("ollama-month", "success", 5.0);
    before.cost_state = "priced".into();
    before.timestamp = start - chrono::Duration::hours(1);
    db.log_forward(&before).unwrap();

    let mut inside = forward_log("ollama-month", "success", 80.0);
    inside.cost_state = "priced".into();
    inside.timestamp = start + chrono::Duration::days(1);
    db.log_forward(&inside).unwrap();

    let mut at_end = forward_log("ollama-month", "success", 9.0);
    at_end.cost_state = "priced".into();
    at_end.timestamp = end;
    db.log_forward(&at_end).unwrap();

    let mut after = forward_log("ollama-month", "success", 11.0);
    after.cost_state = "priced".into();
    after.timestamp = end + chrono::Duration::hours(1);
    db.log_forward(&after).unwrap();

    let windows = db
        .live_ollama_month_quota_window("ollama-month", 60.0)
        .unwrap();
    assert_eq!(windows.len(), 1);
    assert_eq!(windows[0].used, 80.0);
    assert_eq!(windows[0].limit_value, Some(60.0));
    let (used, reset) = db.ollama_month_usage("ollama-month").unwrap();
    assert_eq!(used, 80.0);
    assert_eq!(reset, Some(end));
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn imported_http_controls_survive_fresh_merge_and_failed_preflight() {
    use ocg_domain::destination::{CatalogModel, HttpProtocolRoute, ModelResolution, Protocol};
    let dir = temp_data_dir("http-control-import");
    let db = open_with_host_cipher(dir.clone()).unwrap();
    let legacy_id = "00000000-0000-4000-8000-00000000c099";
    let id = ocg_domain::destination::destination_id_for_custom_account(legacy_id);
    let model = CatalogModel {
        public_model: "public-model".into(),
        upstream_model: "upstream-model".into(),
        protocols: vec![Protocol::Messages],
        preferred: Some(Protocol::Messages),
        enabled: false,
        upstream_override: None,
    };
    let mut record = node_import_record(&db, Vec::new(), Vec::new(), Vec::new());
    record.custom_destinations.push(ImportedCustomDestination {
        id: id.clone(),
        legacy_id: legacy_id.into(),
        name: "No Key".into(),
        endpoint_url: "https://custom.example/v1/chat/completions".into(),
        protocol: UpstreamProtocolKind::ChatCompletions,
        auth_scheme: AuthScheme::None,
        models: vec![ocg_domain::dynamic::DynamicModelMapping {
            public_model: model.public_model.clone(),
            upstream_model: model.upstream_model.clone(),
            upstream_override: None,
        }],
        enabled: false,
    });
    db.import_node_state(&record, |_| Ok(())).unwrap();
    let mut destination = crate::destination_projection::load_persisted(&db)
        .unwrap()
        .destinations
        .into_iter()
        .find(|d| d.id == id)
        .unwrap();
    destination.enabled = false;
    destination.protocols = vec![Protocol::ChatCompletions, Protocol::Messages];
    destination.protocol_routes = vec![
        HttpProtocolRoute {
            protocol: Protocol::ChatCompletions,
            endpoint_url: "https://custom.example/v1/chat/completions".into(),
            auth_scheme: AuthScheme::None,
        },
        HttpProtocolRoute {
            protocol: Protocol::Messages,
            endpoint_url: "https://custom.example/v1/messages".into(),
            auth_scheme: AuthScheme::None,
        },
    ];
    destination.catalog = vec![model.clone()];
    destination.model_resolution = ModelResolution::PublicOnly;
    let expected_routes = destination.protocol_routes.clone();
    record.destination_controls = vec![destination];
    db.conn
        .execute(
            "DELETE FROM destination_models WHERE destination_id = ?1",
            [&id],
        )
        .unwrap();
    db.conn
        .execute("DELETE FROM destinations WHERE id = ?1", [&id])
        .unwrap();
    db.import_node_state(&record, |db| {
        let d = crate::destination_projection::load_persisted(db)?
            .destinations
            .into_iter()
            .find(|d| d.id == id)
            .unwrap();
        assert!(!d.enabled);
        assert_eq!(d.auth_scheme, AuthScheme::None);
        assert_eq!(
            d.protocols,
            vec![Protocol::ChatCompletions, Protocol::Messages]
        );
        assert_eq!(
            d.protocol_routes,
            vec![
                HttpProtocolRoute {
                    protocol: Protocol::ChatCompletions,
                    endpoint_url: "https://custom.example/v1/chat/completions".into(),
                    auth_scheme: AuthScheme::None,
                },
                HttpProtocolRoute {
                    protocol: Protocol::Messages,
                    endpoint_url: "https://custom.example/v1/messages".into(),
                    auth_scheme: AuthScheme::None,
                },
            ]
        );
        assert_eq!(d.catalog, vec![model.clone()]);
        Ok(())
    })
    .unwrap();
    let mut target_only = model.clone();
    target_only.public_model = "target-only".into();
    target_only.upstream_model = "target-upstream".into();
    target_only.enabled = true;
    target_only.protocols = vec![UpstreamProtocolKind::ChatCompletions];
    let mut target_model = model.clone();
    target_model.enabled = true;
    target_model.protocols = vec![UpstreamProtocolKind::ChatCompletions];
    target_model.preferred = Some(UpstreamProtocolKind::ChatCompletions);
    destination_store::replace_destination_catalog(
        &db.conn,
        &id,
        &[target_model, target_only.clone()],
    )
    .unwrap();
    db.conn
        .execute("UPDATE destinations SET enabled = 1 WHERE id = ?1", [&id])
        .unwrap();
    let before = destination_store::load_destination_catalog(&db.conn, &id).unwrap();
    let before_destination = crate::destination_projection::load_persisted(&db)
        .unwrap()
        .destinations
        .into_iter()
        .find(|d| d.id == id)
        .unwrap();
    assert!(
        db.import_node_state(&record, |_| -> Result<()> {
            anyhow::bail!("preflight refused")
        })
        .is_err()
    );
    assert_eq!(
        destination_store::load_destination_catalog(&db.conn, &id).unwrap(),
        before
    );
    assert_eq!(
        crate::destination_projection::load_persisted(&db)
            .unwrap()
            .destinations
            .into_iter()
            .find(|d| d.id == id)
            .unwrap(),
        before_destination
    );
    db.import_node_state(&record, |_| Ok(())).unwrap();
    assert_eq!(
        destination_store::load_destination_catalog(&db.conn, &id).unwrap(),
        vec![model.clone(), target_only.clone()]
    );
    let restored = crate::destination_projection::load_persisted(&db)
        .unwrap()
        .destinations
        .into_iter()
        .find(|d| d.id == id)
        .unwrap();
    assert_eq!(
        restored.protocols,
        vec![Protocol::ChatCompletions, Protocol::Messages]
    );
    assert_eq!(restored.protocol_routes, expected_routes);
    assert_eq!(restored.catalog, vec![model, target_only]);
    assert_eq!(
        db.conn
            .query_row(
                "SELECT enabled FROM destinations WHERE id = ?1",
                [&id],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn imported_http_routes_remap_old_grants_by_operation_and_url_without_new_routes() {
    use ocg_domain::connection::{
        EndpointOperation, LegacyConnectionKind, connection_id_for_legacy,
    };
    use ocg_domain::credential::{RouteSpec, remap_route_grant_ids};
    use ocg_domain::destination::{HttpProtocolRoute, Protocol};

    let dir = temp_data_dir("import-http-grant-remap");
    let db = open_with_host_cipher(dir.clone()).unwrap();
    let provider_id = uuid::Uuid::new_v4().to_string();
    let target_chat = "https://target.example/v1/chat/completions";
    let target_messages = "https://target.example/v1/messages";
    let target_responses = "https://target-other.example/v1/responses";
    let source_responses = "https://source.example/v1/responses";
    let mut runtime = onboarding_runtime(&provider_id, "Import Grant Remap");
    runtime.endpoint_url = target_chat.into();
    let target_routes = vec![
        HttpProtocolRoute {
            protocol: Protocol::ChatCompletions,
            endpoint_url: target_chat.into(),
            auth_scheme: AuthScheme::Bearer,
        },
        HttpProtocolRoute {
            protocol: Protocol::Messages,
            endpoint_url: target_messages.into(),
            auth_scheme: AuthScheme::Bearer,
        },
        HttpProtocolRoute {
            protocol: Protocol::Responses,
            endpoint_url: target_responses.into(),
            auth_scheme: AuthScheme::Bearer,
        },
    ];
    let mut key = account("import-grant-key");
    key.provider_id = provider_id.clone();
    key.key_cipher = fixture_account_key_cipher();
    db.commit_onboarding_new_with_routes(
        &runtime,
        Some(&key),
        false,
        &onboarding_operation(
            &uuid::Uuid::new_v4().to_string(),
            "import-grant-target",
            "{}",
        ),
        Some(&target_routes),
    )
    .unwrap();
    let mut source_key = account("import-source-key");
    source_key.provider_id = provider_id.clone();
    source_key.key_cipher = fixture_account_key_cipher();
    db.create_account(&source_key).unwrap();
    let destination_id = ocg_domain::destination::destination_id_for_dynamic(&provider_id);
    let before = crate::destination_projection::load_persisted(&db).unwrap();
    let before_destination = before
        .destinations
        .iter()
        .find(|destination| destination.id == destination_id)
        .unwrap()
        .clone();
    let before_key = before
        .credentials
        .iter()
        .find(|credential| credential.legacy_account_id == key.id)
        .unwrap()
        .clone();
    assert_eq!(before_key.grants.allowed_endpoint_ids.len(), 2);

    let source_routes = vec![
        HttpProtocolRoute {
            protocol: Protocol::Responses,
            endpoint_url: source_responses.into(),
            auth_scheme: AuthScheme::Bearer,
        },
        HttpProtocolRoute {
            protocol: Protocol::Messages,
            endpoint_url: target_messages.into(),
            auth_scheme: AuthScheme::Bearer,
        },
        HttpProtocolRoute {
            protocol: Protocol::ChatCompletions,
            endpoint_url: target_chat.into(),
            auth_scheme: AuthScheme::Bearer,
        },
    ];
    let mut source_destination = before_destination.clone();
    source_destination.protocol_routes = source_routes.clone();
    source_destination.protocols = source_routes.iter().map(|route| route.protocol).collect();
    source_destination.base_url = Some(source_responses.into());
    source_destination.catalog[0].protocols = vec![Protocol::Messages];
    source_destination.catalog[0].preferred = Some(Protocol::Messages);
    source_destination.catalog[0].enabled = false;
    let mut source_import = source_key.clone();
    source_import.key_cipher = fixture_account_key_cipher();
    let mut record = node_import_record(
        &db,
        vec![AccountImportRecord {
            account: source_import,
            custom_config: None,
            capabilities: Vec::new(),
            verification_status: ConnectionVerificationStatus::NotRequired,
            connection_verified_at: None,
            ollama_billing_tier: None,
        }],
        Vec::new(),
        Vec::new(),
    );
    record.destination_controls = vec![source_destination];
    let mut source_snapshot = identity_snapshot_forcing_all(&db, &[source_key.id.as_str()]);
    source_snapshot.accounts[0].allowed_endpoint_ids.clear();
    source_snapshot.accounts[0].allowed_origins.clear();
    record.identity_snapshot = Some(source_snapshot);

    let connection = connection_id_for_legacy(LegacyConnectionKind::DynamicProvider, &provider_id);
    let route_specs = |routes: &[HttpProtocolRoute]| {
        routes
            .iter()
            .map(|route| RouteSpec {
                operation: EndpointOperation::from(route.protocol),
                url: Some(route.endpoint_url.clone()),
            })
            .collect::<Vec<_>>()
    };
    let expected_target = remap_route_grant_ids(
        &connection,
        &route_specs(&target_routes),
        &route_specs(&source_routes),
        &before_key.grants.allowed_endpoint_ids,
    );
    db.import_node_state(&record, |_| Ok(())).unwrap();
    let after = crate::destination_projection::load_persisted(&db).unwrap();
    let after_destination = after
        .destinations
        .iter()
        .find(|destination| destination.id == destination_id)
        .unwrap();
    assert_eq!(after_destination.protocol_routes, source_routes);
    let after_key = after
        .credentials
        .iter()
        .find(|credential| credential.legacy_account_id == key.id)
        .unwrap();
    assert_eq!(after_key.grants.allowed_endpoint_ids, expected_target);
    assert_eq!(
        after_key.grants.allowed_origins,
        before_key.grants.allowed_origins
    );
    assert!(!after_key.grants.allowed_endpoint_ids.iter().any(|id| {
        id == &ocg_domain::connection::endpoint_id_for(
            &connection,
            EndpointOperation::ResponseCreate,
        )
        .to_string()
    }));
    assert!(
        !after_key
            .grants
            .allowed_origins
            .contains(&"https://source.example".into())
    );
    let source_after = after
        .credentials
        .iter()
        .find(|credential| credential.legacy_account_id == source_key.id)
        .unwrap();
    assert!(source_after.grants.allowed_endpoint_ids.is_empty());
    assert!(source_after.grants.allowed_origins.is_empty());
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn imported_builtin_controls_replace_target_choices_for_old_and_new_payloads() {
    let dir = temp_data_dir("builtin-control-import");
    let db = open_with_host_cipher(dir.clone()).unwrap();
    let scope = ContractScope::provider(OPENCODE_PROVIDER_ID);
    let now = Utc::now();
    db.set_contract_catalog(
        &scope,
        &["gpt-5.6-luna".into()],
        Some(now),
        "test",
        "https://example.test/models",
        now,
    )
    .unwrap();
    let destination_id = ocg_domain::destination::destination_id_for_builtin(OPENCODE_PROVIDER_ID);
    let changes = [
        UpstreamProtocolKind::ChatCompletions,
        UpstreamProtocolKind::Responses,
        UpstreamProtocolKind::Messages,
    ]
    .map(|p| ("gpt-5.6-luna".into(), p, ProtocolOverrideState::ForceOff));
    db.set_model_protocol_overrides(&scope, &changes, now)
        .unwrap();
    let source = crate::destination_projection::load_persisted(&db)
        .unwrap()
        .destinations
        .into_iter()
        .find(|d| d.id == destination_id)
        .unwrap();
    assert!(!source.catalog[0].enabled);
    let mut record = node_import_record(&db, Vec::new(), Vec::new(), Vec::new());
    record.provider_contracts = db.load_persisted_contracts().unwrap();
    for canonical in [false, true] {
        db.set_model_protocol_overrides(
            &scope,
            &[(
                "gpt-5.6-luna".into(),
                UpstreamProtocolKind::ChatCompletions,
                ProtocolOverrideState::ForceOn,
            )],
            now,
        )
        .unwrap();
        assert!(
            destination_store::load_destination_catalog(&db.conn, &destination_id).unwrap()[0]
                .enabled
        );
        record.destination_controls = if canonical {
            vec![source.clone()]
        } else {
            vec![]
        };
        db.import_node_state(&record, |db| {
            let catalog = destination_store::load_destination_catalog(&db.conn, &destination_id)?;
            assert!(!catalog[0].enabled);
            assert!(catalog[0].protocols.is_empty());
            Ok(())
        })
        .unwrap();
    }
    let fresh_dir = temp_data_dir("builtin-control-import-fresh");
    let fresh = open_with_host_cipher(fresh_dir.clone()).unwrap();
    fresh.import_node_state(&record, |_| Ok(())).unwrap();
    assert_eq!(
        destination_store::load_destination_catalog(&fresh.conn, &destination_id).unwrap(),
        source.catalog
    );
    let empty_dir = temp_data_dir("builtin-control-import-empty");
    let empty = open_with_host_cipher(empty_dir.clone()).unwrap();
    record.provider_contracts = PersistedContracts::default();
    record.destination_controls[0].catalog.clear();
    empty.import_node_state(&record, |_| Ok(())).unwrap();
    assert!(destination_store::destination_exists(&empty.conn, &destination_id).unwrap());
    assert!(
        destination_store::load_destination_catalog(&empty.conn, &destination_id)
            .unwrap()
            .is_empty()
    );
    drop(fresh);
    drop(empty);
    fs::remove_dir_all(fresh_dir).unwrap();
    fs::remove_dir_all(empty_dir).unwrap();
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn platform_discovery_preserves_empty_protocol_controls_and_initializes_new_models() {
    let dir = temp_data_dir("platform-protocol-controls");
    let db = Database::open(dir.clone()).unwrap();
    seed_linked_platform_keys(&db, "parent-controls", &[("key-a", "model-a", "up-a")]);
    let id = ocg_domain::destination::destination_id_for_platform_account("parent-controls");
    let before = destination_store::load_destination_catalog(&db.conn, &id).unwrap();
    assert_eq!(before[0].protocols.len(), 3);
    db.conn.execute("UPDATE destination_models SET enabled = 0, protocols_json = '[]', preferred = NULL WHERE destination_id = ?1 AND public_model = 'model-a'", [&id]).unwrap();
    db.replace_account_model_capabilities(
        "key-a",
        &[
            custom_capability("model-a", "up-a"),
            custom_capability("model-b", "up-b"),
        ],
    )
    .unwrap();
    let catalog = destination_store::load_destination_catalog(&db.conn, &id).unwrap();
    let saved = catalog
        .iter()
        .find(|m| m.public_model == "model-a")
        .unwrap();
    assert!(!saved.enabled);
    assert!(saved.protocols.is_empty());
    assert!(saved.preferred.is_none());
    let new = catalog
        .iter()
        .find(|m| m.public_model == "model-b")
        .unwrap();
    assert_eq!(new.protocols.len(), 3);
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn utc_token_day_bounds_include_the_whole_earliest_calendar_day() {
    let now = chrono::DateTime::parse_from_rfc3339("2026-09-22T14:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    let (start, end) = utc_token_day_bounds(now, 1).unwrap();
    assert_eq!(
        start,
        chrono::DateTime::parse_from_rfc3339("2026-09-22T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc)
    );
    assert_eq!(
        end,
        chrono::DateTime::parse_from_rfc3339("2026-09-23T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc)
    );
    let morning = chrono::DateTime::parse_from_rfc3339("2026-09-22T10:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    assert!(morning >= start && morning < end);
    let (week_start, week_end) = utc_token_day_bounds(now, 7).unwrap();
    let early_morning = chrono::DateTime::parse_from_rfc3339("2026-09-16T10:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    let early_evening = chrono::DateTime::parse_from_rfc3339("2026-09-16T20:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    assert!(early_morning >= week_start && early_morning < week_end);
    assert!(early_evening >= week_start && early_evening < week_end);
    assert!(utc_token_day_bounds(now, 0).is_err());
}

#[test]
fn daily_tokens_by_model_includes_midnight_of_the_earliest_utc_day() {
    let dir = temp_data_dir("daily-token-calendar");
    let db = Database::open(dir.clone()).unwrap();
    let now = chrono::Utc::now();
    let mut today = forward_log("acct", "success", 0.0);
    today.timestamp = now.date_naive().and_hms_opt(0, 0, 0).unwrap().and_utc();
    today.model = "today-model".into();
    today.prompt_tokens = 100;
    db.log_forward(&today).unwrap();

    let earliest = (now.date_naive() - chrono::Duration::days(6))
        .and_hms_opt(0, 0, 0)
        .unwrap()
        .and_utc();
    let mut early = forward_log("acct", "success", 0.0);
    early.timestamp = earliest;
    early.model = "early-model".into();
    early.prompt_tokens = 40;
    early.completion_tokens = 10;
    db.log_forward(&early).unwrap();
    let mut early_evening = forward_log("acct", "success", 0.0);
    early_evening.timestamp = earliest + chrono::Duration::hours(20);
    early_evening.model = "early-model".into();
    early_evening.prompt_tokens = 5;
    early_evening.completion_tokens = 5;
    db.log_forward(&early_evening).unwrap();

    let one_day = db.daily_tokens_by_model(1).unwrap();
    assert_eq!(one_day.iter().map(|row| row.tokens).sum::<i64>(), 100);
    let week = db.daily_tokens_by_model(7).unwrap();
    let early_tokens: i64 = week
        .iter()
        .filter(|row| row.model == "early-model")
        .map(|row| row.tokens)
        .sum();
    assert_eq!(early_tokens, 60);
    assert!(db.daily_tokens_by_model(0).is_err());
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

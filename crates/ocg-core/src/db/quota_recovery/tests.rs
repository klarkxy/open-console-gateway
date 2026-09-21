use super::*;
use crate::db::Database;
use crate::models::{Account, AccountSetupStep, AccountType};
use crate::provider::{CredentialKind, OPENCODE_PROVIDER_ID, QuotaScope};
use crate::quota_recovery::{PersistedQuotaRecovery, PersistedQuotaWindow, QuotaEpisode};
use chrono::{Duration, TimeZone, Utc};
use ocg_domain::credential::credential_id_for_legacy_account;
use ocg_gateway::quota::{QuotaEvidence, QuotaReason, QuotaWindowKind};

fn open_db(tag: &str) -> (std::path::PathBuf, Database) {
    let dir = std::env::temp_dir().join(format!("ocg-quota-db-{tag}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let db = Database::open(dir.clone()).unwrap();
    (dir, db)
}

fn insert_go(db: &Database, id: &str, cipher: &str) {
    let now = Utc::now();
    db.create_account(&Account {
        id: id.into(),
        provider_id: OPENCODE_PROVIDER_ID.into(),
        credential_kind: CredentialKind::ApiKey,
        quota_scope: QuotaScope::Key,
        name: id.into(),
        username: None,
        password_cipher: None,
        key_cipher: cipher.into(),
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

fn episode(db: &Database, account_id: &str) -> QuotaEpisode {
    let (id, version, key_cipher, _) = load_for_legacy_on(&db.conn, account_id).unwrap().unwrap();
    QuotaEpisode {
        credential_id: id,
        account_id: account_id.into(),
        credential_version: version,
        epoch: 1,
        key_cipher,
    }
}

#[test]
fn v60_adds_quota_recovery_column_and_roundtrips() {
    let (dir, db) = open_db("roundtrip");
    assert!(table_has_column(&db.conn, "credentials", "quota_recovery_json").unwrap());
    insert_go(&db, "acct-a", "cipher-a");
    let now = Utc.with_ymd_and_hms(2026, 9, 20, 0, 0, 0).unwrap();
    let recovery = PersistedQuotaRecovery::from_evidence(
        None,
        &QuotaEvidence {
            reason: QuotaReason::QuotaExhausted,
            window: QuotaWindowKind::Week,
            resets_at_rfc3339: Some("2026-09-27T00:00:00Z".into()),
            resets_in_text: None,
        },
        now,
        None,
    );
    let mut ep = episode(&db, "acct-a");
    ep.epoch = recovery.epoch;
    assert!(save_on(&db.conn, &ep, &recovery).unwrap());
    let loaded = load_on(&db.conn, &ep.credential_id).unwrap().unwrap();
    assert_eq!(loaded.window_count_for_test(), 1);
    assert_eq!(
        loaded.windows.get(&PersistedQuotaWindow::Week).copied(),
        Some(Some(now + Duration::days(7)))
    );
    drop(db);
    let db = Database::open(dir.clone()).unwrap();
    let reloaded = load_on(
        &db.conn,
        credential_id_for_legacy_account("acct-a").as_str(),
    )
    .unwrap()
    .unwrap();
    assert_eq!(reloaded.epoch, 1);
    drop(db);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn rotate_and_key_mismatch_refuse_to_mutate_replacement() {
    let (dir, db) = open_db("stale");
    insert_go(&db, "acct-a", "cipher-a");
    insert_go(&db, "acct-b", "cipher-b");
    let now = Utc::now();
    let recovery = PersistedQuotaRecovery::from_evidence(
        None,
        &QuotaEvidence {
            reason: QuotaReason::QuotaExhausted,
            window: QuotaWindowKind::Unknown,
            resets_at_rfc3339: None,
            resets_in_text: None,
        },
        now,
        None,
    );
    let mut ep = episode(&db, "acct-a");
    ep.epoch = 1;
    assert!(save_on(&db.conn, &ep, &recovery).unwrap());
    let mut stale = ep.clone();
    stale.key_cipher = "other".into();
    assert!(!save_on(&db.conn, &stale, &recovery).unwrap());
    assert!(load_on(&db.conn, &ep.credential_id).unwrap().is_some());
    db.rotate_account_credential("acct-a", "cipher-rotated")
        .unwrap();
    assert!(load_on(&db.conn, &ep.credential_id).unwrap().is_none());
    assert!(!save_on(&db.conn, &ep, &recovery).unwrap());
    assert!(!clear_matching_on(&db.conn, &ep).unwrap());
    drop(db);
    std::fs::remove_dir_all(dir).unwrap();
}

impl PersistedQuotaRecovery {
    fn window_count_for_test(&self) -> usize {
        self.windows.len()
    }
}

fn import_record(
    db: &Database,
    mut account: crate::models::Account,
) -> crate::db::NodeImportRecord {
    use crate::db::{AccountImportRecord, NodeImportRecord};
    use crate::provider::ConnectionVerificationStatus;
    use std::collections::{HashMap, HashSet};
    let mut account_order: Vec<String> = db
        .list_accounts()
        .unwrap()
        .into_iter()
        .map(|row| row.id)
        .collect();
    if !account_order.contains(&account.id) {
        account_order.push(account.id.clone());
    }
    account.updated_at = Utc::now();
    let config = crate::models::AppConfig {
        gateway_key: "ocg-import-primary-key".into(),
        ..crate::models::AppConfig::default()
    };
    NodeImportRecord {
        platform_links_authoritative: true,
        platform_accounts: Vec::new(),
        platform_links: Vec::new(),
        platform_catalogs: HashMap::new(),
        destination_controls: Vec::new(),
        accounts: vec![AccountImportRecord {
            account,
            custom_config: None,
            capabilities: Vec::new(),
            verification_status: ConnectionVerificationStatus::NotRequired,
            connection_verified_at: None,
            ollama_billing_tier: None,
        }],
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

fn sample_recovery() -> PersistedQuotaRecovery {
    PersistedQuotaRecovery::from_evidence(
        None,
        &QuotaEvidence {
            reason: QuotaReason::QuotaExhausted,
            window: QuotaWindowKind::Unknown,
            resets_at_rfc3339: None,
            resets_in_text: None,
        },
        Utc.with_ymd_and_hms(2026, 9, 20, 0, 0, 0).unwrap(),
        None,
    )
}

#[test]
fn encrypted_import_preserves_recovery_for_same_key_and_clears_replacement() {
    use crate::crypto::{KeyCipher, StaticKeyCipher};
    let cipher = StaticKeyCipher::new("quota-import-roundtrip");
    let (dir, db) = open_db("import-cipher");
    let original = cipher.encrypt("sk-live").unwrap();
    insert_go(&db, "acct-a", &original);
    let recovery = sample_recovery();
    let mut ep = episode(&db, "acct-a");
    ep.epoch = recovery.epoch;
    assert!(save_on(&db.conn, &ep, &recovery).unwrap());
    let reencrypted = cipher.encrypt("sk-live").unwrap();
    assert_ne!(original, reencrypted);
    let mut same = db.get_account("acct-a").unwrap().unwrap();
    same.name = "Renamed".into();
    same.key_cipher = reencrypted;
    db.import_node_state_with_cipher(&import_record(&db, same), Some(&cipher), |_| Ok(()))
        .unwrap();
    let kept = load_on(&db.conn, &ep.credential_id).unwrap().unwrap();
    assert_eq!(kept.epoch, recovery.epoch);
    let merged = db.get_account("acct-a").unwrap().unwrap();
    assert_eq!(merged.name, "Renamed");
    assert_eq!(merged.key_cipher, original);
    drop(db);
    let db = Database::open(dir.clone()).unwrap();
    assert_eq!(
        load_on(&db.conn, &ep.credential_id).unwrap().unwrap().epoch,
        recovery.epoch
    );
    assert_eq!(
        db.get_account("acct-a").unwrap().unwrap().key_cipher,
        original
    );
    let snapshot = crate::routing_snapshot::RoutingSnapshot::load(&db).unwrap();
    let live = snapshot
        .credentials
        .iter()
        .find(|credential| credential.id == "acct-a")
        .unwrap();
    assert!(live.matches_quota_episode(&ep));
    assert!(clear_matching_on(&db.conn, &ep).unwrap());
    assert!(load_on(&db.conn, &ep.credential_id).unwrap().is_none());
    assert!(save_on(&db.conn, &ep, &recovery).unwrap());

    let replacement = cipher.encrypt("sk-other").unwrap();
    let mut replaced = db.get_account("acct-a").unwrap().unwrap();
    replaced.key_cipher = replacement;
    db.import_node_state_with_cipher(&import_record(&db, replaced), Some(&cipher), |_| Ok(()))
        .unwrap();
    assert!(load_on(&db.conn, &ep.credential_id).unwrap().is_none());
    let replaced_snapshot = crate::routing_snapshot::RoutingSnapshot::load(&db).unwrap();
    let replaced_live = replaced_snapshot
        .credentials
        .iter()
        .find(|credential| credential.id == "acct-a")
        .unwrap();
    assert!(!replaced_live.matches_quota_episode(&ep));
    assert!(!clear_matching_on(&db.conn, &ep).unwrap());
    drop(db);
    let db = Database::open(dir.clone()).unwrap();
    assert!(load_on(&db.conn, &ep.credential_id).unwrap().is_none());
    drop(db);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn imported_key_compare_fails_closed_on_decrypt_error() {
    use crate::crypto::{KeyCipher, StaticKeyCipher};
    let host = StaticKeyCipher::new("quota-import-host");
    let other = StaticKeyCipher::new("quota-import-other");
    let (dir, db) = open_db("import-fail-closed");
    insert_go(&db, "acct-a", &host.encrypt("sk-live").unwrap());
    let recovery = sample_recovery();
    let mut ep = episode(&db, "acct-a");
    ep.epoch = recovery.epoch;
    assert!(save_on(&db.conn, &ep, &recovery).unwrap());
    let mut incoming = db.get_account("acct-a").unwrap().unwrap();
    incoming.key_cipher = other.encrypt("sk-live").unwrap();
    assert!(
        db.import_node_state_with_cipher(&import_record(&db, incoming), Some(&other), |_| Ok(()))
            .is_err()
    );
    assert!(load_on(&db.conn, &ep.credential_id).unwrap().is_some());
    drop(db);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn two_keys_keep_independent_recovery_despite_shared_identity() {
    let (dir, db) = open_db("independent");
    insert_go(&db, "acct-a", "cipher-a");
    insert_go(&db, "acct-b", "cipher-b");
    let now = Utc.with_ymd_and_hms(2026, 9, 20, 0, 0, 0).unwrap();
    let recovery = PersistedQuotaRecovery::from_evidence(
        None,
        &QuotaEvidence {
            reason: QuotaReason::QuotaExhausted,
            window: QuotaWindowKind::Unknown,
            resets_at_rfc3339: None,
            resets_in_text: None,
        },
        now,
        None,
    );
    let mut a = episode(&db, "acct-a");
    a.epoch = 1;
    assert!(save_on(&db.conn, &a, &recovery).unwrap());
    assert!(
        load_on(&db.conn, &episode(&db, "acct-b").credential_id)
            .unwrap()
            .is_none()
    );
    assert!(load_on(&db.conn, &a.credential_id).unwrap().is_some());
    drop(db);
    std::fs::remove_dir_all(dir).unwrap();
}

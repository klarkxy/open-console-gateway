use super::*;
use crate::crypto::{KeyCipher, StaticKeyCipher};
use crate::db::Database;
use crate::models::AppConfig;
use crate::state::CoreStateInner;
use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

fn temp_state(label: &str) -> (PathBuf, Arc<CoreStateInner>) {
    let mut dir = std::env::temp_dir();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock should be valid")
        .as_nanos();
    dir.push(format!("ocg-sub-keys-{label}-{nanos}"));
    fs::create_dir_all(&dir).expect("test data directory should be created");
    let db = Database::open(dir.clone()).expect("test database should open");
    let cipher: Arc<dyn KeyCipher + Send + Sync> = Arc::new(StaticKeyCipher::new("test"));
    let state = CoreStateInner::new(db, dir.clone(), cipher).expect("state should initialize");
    (dir, Arc::new(state))
}

fn snapshot_values(state: &CoreStateInner) -> HashSet<String> {
    state.credential_snapshot.read().keys().cloned().collect()
}

#[test]
fn named_application_key_reuses_the_oldest_enabled_case_insensitive_match() {
    let host = MemoryKeys::new("primary");
    let first = create_sub_key(&host, "Codex").unwrap();
    let second = create_sub_key(&host, "codex").unwrap();
    let (reused, created) = get_or_create_named_sub_key(&host, "CODEX").unwrap();
    assert!(!created);
    assert_eq!(reused.id, first.id);
    set_sub_key_enabled(&host, &first.id, false).unwrap();
    let (reused, created) = get_or_create_named_sub_key(&host, "codex").unwrap();
    assert!(!created);
    assert_eq!(reused.id, second.id);
}

struct MemoryKeys {
    keys: std::sync::Mutex<Vec<SubGatewayKey>>,
    primary: std::sync::Mutex<String>,
    snapshot: std::sync::Mutex<CredentialSnapshot>,
    word: AtomicU64,
}

impl MemoryKeys {
    fn new(primary: &str) -> Self {
        let mut snapshot = CredentialSnapshot::new();
        if !primary.is_empty() {
            snapshot.insert(
                primary.to_string(),
                CredentialEntry {
                    id: PRIMARY_KEY_ID.to_string(),
                    name: PRIMARY_KEY_NAME.to_string(),
                },
            );
        }
        Self {
            keys: std::sync::Mutex::new(Vec::new()),
            primary: std::sync::Mutex::new(primary.to_string()),
            snapshot: std::sync::Mutex::new(snapshot),
            word: AtomicU64::new(0),
        }
    }

    fn active_keys(&self) -> Vec<SubGatewayKey> {
        self.keys
            .lock()
            .expect("memory keys")
            .iter()
            .filter(|key| key.is_active())
            .cloned()
            .collect()
    }
}

impl KeyStore for MemoryKeys {
    fn list_active_sub_gateway_keys(&self) -> anyhow::Result<Vec<SubGatewayKey>> {
        Ok(self.active_keys())
    }
    fn get_sub_gateway_key(&self, id: &str) -> anyhow::Result<Option<SubGatewayKey>> {
        Ok(self
            .keys
            .lock()
            .expect("memory keys")
            .iter()
            .find(|key| key.id == id)
            .cloned())
    }
    fn count_active_sub_gateway_keys(&self) -> anyhow::Result<usize> {
        Ok(self.active_keys().len())
    }
    fn insert_sub_gateway_key(&self, key: &SubGatewayKey) -> anyhow::Result<()> {
        self.keys.lock().expect("memory keys").push(key.clone());
        Ok(())
    }
    fn rename_sub_gateway_key(&self, id: &str, name: &str) -> anyhow::Result<bool> {
        let mut keys = self.keys.lock().expect("memory keys");
        match keys.iter_mut().find(|key| key.id == id && key.is_active()) {
            Some(key) => {
                key.name = name.to_string();
                Ok(true)
            }
            None => Ok(false),
        }
    }
    fn set_sub_gateway_key_enabled(&self, id: &str, enabled: bool) -> anyhow::Result<bool> {
        let mut keys = self.keys.lock().expect("memory keys");
        match keys.iter_mut().find(|key| key.id == id && key.is_active()) {
            Some(key) => {
                key.enabled = enabled;
                Ok(true)
            }
            None => Ok(false),
        }
    }
    fn update_sub_gateway_key_value(&self, id: &str, new_value: &str) -> anyhow::Result<bool> {
        let mut keys = self.keys.lock().expect("memory keys");
        match keys.iter_mut().find(|key| key.id == id && key.is_active()) {
            Some(key) => {
                key.key = new_value.to_string();
                Ok(true)
            }
            None => Ok(false),
        }
    }
    fn soft_delete_sub_gateway_key(&self, id: &str, now: DateTime<Utc>) -> anyhow::Result<bool> {
        let mut keys = self.keys.lock().expect("memory keys");
        match keys.iter_mut().find(|key| key.id == id && key.is_active()) {
            Some(key) => {
                key.key.clear();
                key.enabled = false;
                key.deleted_at = Some(now);
                Ok(true)
            }
            None => Ok(false),
        }
    }
    fn active_sub_gateway_key_values(&self) -> anyhow::Result<Vec<String>> {
        Ok(self
            .active_keys()
            .into_iter()
            .map(|key| key.key)
            .filter(|value| !value.is_empty())
            .collect())
    }
    fn sub_gateway_key_value_exists(&self, value: &str) -> anyhow::Result<bool> {
        Ok(self
            .active_keys()
            .iter()
            .any(|key| key.key == value && !key.key.is_empty()))
    }
    fn random_word(&self) -> String {
        format!("w{:04}", self.word.fetch_add(1, Ordering::Relaxed))
    }
}

impl KeyHost for MemoryKeys {
    fn primary_gateway_key(&self) -> String {
        self.primary.lock().expect("primary").clone()
    }
    fn clone_credential_snapshot(&self) -> CredentialSnapshot {
        self.snapshot.lock().expect("snapshot").clone()
    }
    fn replace_credential_snapshot(&self, snapshot: CredentialSnapshot) {
        *self.snapshot.lock().expect("snapshot") = snapshot;
    }
    fn with_credential_snapshot_mut<R>(&self, f: impl FnOnce(&mut CredentialSnapshot) -> R) -> R {
        f(&mut self.snapshot.lock().expect("snapshot"))
    }
    fn load_unique_value_inputs(&self) -> anyhow::Result<(Vec<String>, CredentialSnapshot)> {
        Ok((
            self.active_sub_gateway_key_values()?,
            self.clone_credential_snapshot(),
        ))
    }
    fn load_snapshot_rebuild_inputs(&self) -> anyhow::Result<(Vec<SubGatewayKey>, String)> {
        Ok((self.active_keys(), self.primary_gateway_key()))
    }
}

#[test]
fn create_returns_full_value_and_authenticates_via_snapshot() {
    let (dir, state) = temp_state("create");
    let primary = state.config().gateway_key;
    let created = create_sub_key(&state, " Laptop ").expect("sub key should create");
    assert_eq!(created.name, "Laptop");
    assert!(created.authenticates());
    assert_ne!(created.key, primary);
    let values = snapshot_values(&state);
    assert!(values.contains(&primary));
    assert!(values.contains(&created.key));
    let stored = state
        .db
        .lock()
        .get_sub_gateway_key(&created.id)
        .unwrap()
        .expect("created key should persist");
    assert_eq!(stored.key, created.key);

    drop(state);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn create_rejects_blank_overlong_names_and_the_active_ceiling() {
    let (dir, state) = temp_state("limits");
    assert!(matches!(
        create_sub_key(&state, "  "),
        Err(KeyError::BadRequest(_))
    ));
    assert!(matches!(
        create_sub_key(&state, &"x".repeat(65)),
        Err(KeyError::BadRequest(_))
    ));
    for index in 0..MAX_ACTIVE_SUB_KEYS {
        create_sub_key(&state, &format!("key-{index}"))
            .expect("keys below the ceiling should create");
    }
    let overflow = create_sub_key(&state, "overflow");
    assert_eq!(
        overflow.unwrap_err(),
        KeyError::bad_request(format!(
            "at most {MAX_ACTIVE_SUB_KEYS} active keys are supported"
        ))
    );
    // Tombstones do not count: deleting one frees a slot.
    let retired = state.db.lock().list_active_sub_gateway_keys().unwrap()[0]
        .id
        .clone();
    delete_sub_key(&state, &retired, Utc::now()).expect("delete should work");
    create_sub_key(&state, "fresh").expect("deleted key frees a slot");

    drop(state);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn rename_updates_the_name_snapshot_for_later_rows() {
    let (dir, state) = temp_state("rename");
    let created = create_sub_key(&state, "Laptop").expect("sub key should create");
    rename_sub_key(&state, &created.id, "Deck").expect("rename should work");
    assert_eq!(
        state.client_key_name(&created.id).as_deref(),
        Some("Deck"),
        "the snapshot must serve the new name to log writes"
    );
    assert!(matches!(
        rename_sub_key(&state, "missing", "x"),
        Err(KeyError::BadRequest(_))
    ));

    drop(state);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn disable_is_fail_closed_and_reenable_checks_the_primary_value() {
    let (dir, state) = temp_state("disable");
    let created = create_sub_key(&state, "Laptop").expect("sub key should create");

    set_sub_key_enabled(&state, &created.id, false).expect("disable should work");
    assert!(!snapshot_values(&state).contains(&created.key));
    let stored = state
        .db
        .lock()
        .get_sub_gateway_key(&created.id)
        .unwrap()
        .unwrap();
    assert!(!stored.enabled, "disabled keys keep their plaintext");
    assert_eq!(stored.key, created.key);

    // Schema v27 unique-indexes all live access-key values, so an
    // ordinary set_config cannot adopt a disabled sub key's plaintext.
    let mut config = state.config();
    config.gateway_key = created.key.clone();
    assert!(
        state.set_config(config).is_err(),
        "active access key values must stay unique at the database"
    );

    // Drop the unique index to simulate the pre-v27 unchecked bypass,
    // then re-enabling must still be rejected by the API gate.
    rusqlite::Connection::open(dir.join("data.sqlite"))
        .unwrap()
        .execute_batch("DROP INDEX IF EXISTS idx_access_keys_active_key;")
        .unwrap();
    let mut config = state.config();
    config.gateway_key = created.key.clone();
    state
        .set_config(config)
        .expect("index-less writer can still collide");
    let re_enable = set_sub_key_enabled(&state, &created.id, true);
    assert_eq!(
        re_enable.unwrap_err(),
        KeyError::bad_request("key value collides with the primary key")
    );

    // Repair the collision and re-enable normally.
    let mut config = state.config();
    config.gateway_key = "ocg-primary-restored".to_string();
    state.set_config(config).expect("repair should save");
    set_sub_key_enabled(&state, &created.id, true).expect("re-enable should work");
    assert!(snapshot_values(&state).contains(&created.key));

    drop(state);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn regenerate_swaps_the_snapshot_value_and_keeps_attribution() {
    let (dir, state) = temp_state("regenerate");
    let created = create_sub_key(&state, "Laptop").expect("sub key should create");
    let regenerated = regenerate_sub_key(&state, &created.id).expect("regenerate should work");
    assert_ne!(regenerated.key, created.key);
    let values = snapshot_values(&state);
    assert!(!values.contains(&created.key), "the old value is revoked");
    assert!(values.contains(&regenerated.key));
    assert_eq!(regenerated.id, created.id);
    assert_eq!(
        state.client_key_name(&created.id).as_deref(),
        Some("Laptop")
    );

    drop(state);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn regenerating_a_disabled_sub_key_does_not_grant_the_new_value() {
    let (dir, state) = temp_state("regenerate-disabled");
    let created = create_sub_key(&state, "Laptop").expect("sub key should create");
    set_sub_key_enabled(&state, &created.id, false).expect("disable should work");
    let regenerated = regenerate_sub_key(&state, &created.id).expect("regenerate should work");
    assert!(
        state.credential_entry_for_value(&regenerated.key).is_none(),
        "a disabled key's fresh value must not authenticate"
    );
    let stored = state
        .db
        .lock()
        .get_sub_gateway_key(&created.id)
        .unwrap()
        .unwrap();
    assert!(!stored.enabled, "regeneration must not re-enable the key");
    assert_eq!(stored.key, regenerated.key);

    // Re-enabling (with a non-colliding value) puts the value back.
    set_sub_key_enabled(&state, &created.id, true).expect("re-enable should work");
    assert!(state.credential_entry_for_value(&regenerated.key).is_some());

    drop(state);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn delete_clears_plaintext_and_keeps_the_attribution_record() {
    let (dir, state) = temp_state("delete");
    let created = create_sub_key(&state, "Laptop").expect("sub key should create");
    delete_sub_key(&state, &created.id, Utc::now()).expect("delete should work");
    let tombstone = state
        .db
        .lock()
        .get_sub_gateway_key(&created.id)
        .unwrap()
        .expect("tombstone should persist");
    assert!(tombstone.deleted_at.is_some());
    assert!(tombstone.key.is_empty());
    assert_eq!(tombstone.name, "Laptop");
    assert!(!snapshot_values(&state).contains(&created.key));
    assert!(matches!(
        delete_sub_key(&state, &created.id, Utc::now()),
        Err(KeyError::BadRequest(_))
    ));

    drop(state);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn primary_value_gate_rejects_values_held_by_non_deleted_sub_keys() {
    let (dir, state) = temp_state("gate");
    let created = create_sub_key(&state, "Laptop").expect("sub key should create");
    set_sub_key_enabled(&state, &created.id, false).expect("disable should work");
    {
        let db = state.db.lock();
        assert!(ensure_primary_value_allowed(&db, &created.key).is_err());
    }
    delete_sub_key(&state, &created.id, Utc::now()).expect("delete should work");
    {
        let db = state.db.lock();
        ensure_primary_value_allowed(&db, &created.key)
            .expect("tombstoned values are free for the primary");
    }

    drop(state);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn primary_attribution_survives_an_out_of_model_value_collision() {
    let (dir, state) = temp_state("collision-hardening");
    let created = create_sub_key(&state, "Laptop").expect("sub key should create");
    // An unchecked writer that drops the unique index can still collide;
    // snapshot rebuild must keep attributing the shared value to primary.
    rusqlite::Connection::open(dir.join("data.sqlite"))
        .unwrap()
        .execute_batch("DROP INDEX IF EXISTS idx_access_keys_active_key;")
        .unwrap();
    let mut config = state.config();
    config.gateway_key = created.key.clone();
    state.set_config(config).expect("save should work");
    // Any key API entry point rebuilds the snapshot: the shared value
    // stays attributed to the primary.
    refresh_snapshot(&state);
    let entry = state.credential_entry_for_value(&created.key).unwrap();
    assert_eq!(entry.id, crate::gateway_keys::PRIMARY_KEY_ID);
    // Revoking the sub key never evicts the primary's live entry.
    delete_sub_key(&state, &created.id, Utc::now()).expect("delete should work");
    assert!(
        state.credential_entry_for_value(&created.key).is_some(),
        "the primary keeps authenticating after the colliding sub key is deleted"
    );

    drop(state);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn set_config_refreshes_the_primary_snapshot_entry() {
    let (dir, state) = temp_state("primary-refresh");
    let mut config = AppConfig {
        gateway_key: "ocg-custom-primary".to_string(),
        ..state.config()
    };
    state.set_config(config.clone()).expect("save should work");
    assert!(snapshot_values(&state).contains("ocg-custom-primary"));
    assert_eq!(
        state.client_key_name(PRIMARY_KEY_ID).as_deref(),
        Some(PRIMARY_KEY_NAME)
    );

    config.gateway_key = "  ".to_string();
    assert!(state.set_config(config).is_err(), "blank keys are rejected");
    assert!(snapshot_values(&state).contains("ocg-custom-primary"));

    drop(state);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn key_store_seam_enforces_uniqueness_without_process_host() {
    let store = MemoryKeys::new("ocg-primary");
    let created = create_sub_key(&store, "Laptop").expect("memory host should create");
    assert!(ensure_primary_value_allowed(&store, &created.key).is_err());
    let rotated = generate_primary_value(&store, "ocg-primary").expect("rotate");
    assert_ne!(rotated, created.key);
    assert_ne!(rotated, "ocg-primary");
    assert!(rotated.starts_with("ocg-w"));
}

#[test]
fn key_host_seam_revokes_fail_closed_without_process_host() {
    let store = MemoryKeys::new("ocg-primary");
    let created = create_sub_key(&store, "Laptop").expect("memory host should create");
    assert!(store.clone_credential_snapshot().contains_key(&created.key));
    set_sub_key_enabled(&store, &created.id, false).expect("disable");
    assert!(!store.clone_credential_snapshot().contains_key(&created.key));
    *store.primary.lock().expect("primary") = created.key.clone();
    assert_eq!(
        set_sub_key_enabled(&store, &created.id, true).unwrap_err(),
        KeyError::bad_request("key value collides with the primary key")
    );
    delete_sub_key(&store, &created.id, Utc::now()).expect("delete");
    let tombstone = store
        .get_sub_gateway_key(&created.id)
        .unwrap()
        .expect("tombstone");
    assert!(tombstone.deleted_at.is_some());
    assert!(tombstone.key.is_empty());
    assert_eq!(tombstone.name, "Laptop");
}

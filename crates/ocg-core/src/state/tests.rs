use super::{
    CoreStateInner, DesktopUpdatePhase, DesktopUpdateStartError, build_proxy_model_candidates,
    normalize_client_root_url_override,
};
use crate::crypto::{KeyCipher, StaticKeyCipher};
use crate::db::Database;
use crate::models::{AppConfig, ProxyListDirection, ProxyMode};
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Barrier, Mutex as StdMutex};

fn temp_data_dir(label: &str) -> PathBuf {
    let mut dir = std::env::temp_dir();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock should be after the Unix epoch")
        .as_nanos();
    dir.push(format!("ocg-state-test-{label}-{nanos}"));
    fs::create_dir_all(&dir).expect("test data directory should be created");
    dir
}

#[test]
fn client_root_url_override_normalizes_non_empty_values() {
    assert_eq!(normalize_client_root_url_override(None), Ok(None));
    assert_eq!(normalize_client_root_url_override(Some("   ")), Ok(None));
    assert_eq!(
        normalize_client_root_url_override(Some(" https://ocg.example.com/proxy/v1/ ")),
        Ok(Some("https://ocg.example.com/proxy".to_string()))
    );
    assert!(
        normalize_client_root_url_override(Some("https://ocg.example.com/v1/responses")).is_err()
    );
}

#[test]
fn process_host_adapts_key_and_usage_sync_seams() {
    fn assert_key_store<T: crate::gateway_keys::KeyStore>(_: &T) {}
    fn assert_key_host<T: crate::gateway_keys::KeyHost>(_: &T) {}
    fn assert_usage_store<T: crate::usage_sync::UsageSyncStore>(_: &T) {}
    fn assert_usage_host<T: crate::usage_sync::UsageSyncHost>(_: &T) {}

    let dir = temp_data_dir("host-seams");
    let db = Database::open(dir.clone()).expect("test database should open");
    assert_key_store(&db);
    assert_usage_store(&db);
    let cipher: Arc<dyn KeyCipher + Send + Sync> = Arc::new(StaticKeyCipher::new("state-test"));
    let inner = CoreStateInner::new(db, dir.clone(), cipher).expect("state should initialize");
    assert_key_store(&inner);
    assert_key_host(&inner);
    let snapshot =
        crate::gateway_keys::build_credential_snapshot(&inner, &inner.config().gateway_key)
            .expect("snapshot rebuild through the host");
    assert!(snapshot.contains_key(&inner.config().gateway_key));
    let state = Arc::new(inner);
    assert_usage_host(&state);
    drop(state);
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn reload_failure_restriction_rebuilds_proxy_membership() {
    use crate::provider::{OPENCODE_PROVIDER_ID, UpstreamProtocolKind};
    use crate::provider_contracts::{
        CATALOG_SOURCE_OPENCODE_MODELS, ContractScope, ProtocolOverrideState,
    };
    use chrono::Utc;

    let dir = temp_data_dir("restrict-proxy-membership");
    let db = Database::open(dir.clone()).expect("test database should open");
    let cipher: Arc<dyn KeyCipher + Send + Sync> = Arc::new(StaticKeyCipher::new("state-test"));
    let state = CoreStateInner::new(db, dir.clone(), cipher).expect("state should initialize");
    let scope = ContractScope::provider(OPENCODE_PROVIDER_ID);
    let now = Utc::now();
    state
        .db
        .lock()
        .set_contract_catalog(
            &scope,
            &["gpt-5.6-luna".into(), "gpt-5.6-sol".into()],
            Some(now),
            CATALOG_SOURCE_OPENCODE_MODELS,
            "https://example.test/models",
            now,
        )
        .unwrap();
    // This test concerns removing an enabled route, not protocol discovery.
    // The synthetic model IDs therefore need an explicit operator choice.
    state
        .db
        .lock()
        .set_model_protocol_overrides(
            &scope,
            &["gpt-5.6-luna", "gpt-5.6-sol"].map(|id| {
                (
                    id.to_string(),
                    UpstreamProtocolKind::ChatCompletions,
                    ProtocolOverrideState::ForceOn,
                )
            }),
            now,
        )
        .unwrap();
    state.reload_provider_contracts().unwrap();
    let mut config = state.config();
    config.proxy_mode = ProxyMode::List;
    config.proxy_url = "http://127.0.0.1:9".into();
    config.proxy_list_direction = ProxyListDirection::Whitelist;
    config.proxy_list_models = vec!["gpt-5.6-luna".into(), "gpt-5.6-sol".into()];
    state.set_config(config).unwrap();
    assert_eq!(
        state
            .forward_route_set()
            .client_for("gpt-5.6-sol")
            .1
            .as_str(),
        "proxy"
    );

    let row = state
        .db
        .lock()
        .remove_contract_catalog_models(&scope, &["gpt-5.6-sol".into()], now)
        .unwrap();
    state.restrict_provider_catalog_after_reload_failure(&row);
    assert_eq!(
        state
            .forward_route_set()
            .client_for("gpt-5.6-sol")
            .1
            .as_str(),
        "direct",
        "removed upstream ids must be inert even on the reload-failure path"
    );

    drop(state);
    fs::remove_dir_all(dir).expect("test data directory should be removed");
}

#[test]
fn client_root_url_override_never_replaces_persisted_setting() {
    let dir = temp_data_dir("client-root-override");
    let db = Database::open(dir.clone()).expect("test database should open");
    let persisted = AppConfig {
        gateway_key: "test-gateway-key".to_string(),
        client_root_url: "https://saved.example.com".to_string(),
        ..AppConfig::default()
    };
    db.set_setting(
        "config",
        &serde_json::to_string(&persisted).expect("test config should serialize"),
    )
    .expect("test config should persist");
    let cipher: Arc<dyn KeyCipher + Send + Sync> = Arc::new(StaticKeyCipher::new("state-test"));
    let state = CoreStateInner::new_with_client_root_url_override(
        db,
        dir.clone(),
        cipher,
        Some("https://environment.example.com".to_string()),
    )
    .expect("state should initialize");

    assert_eq!(
        state.settings_config().client_root_url,
        "https://environment.example.com"
    );
    let mut submitted = state.settings_config();
    submitted.connect_timeout_secs = 45;
    state
        .set_config(submitted)
        .expect("other settings should save while the override is active");
    assert_eq!(state.config().client_root_url, "https://saved.example.com");
    let stored = state
        .db
        .lock()
        .get_setting("config")
        .expect("stored config should be readable")
        .expect("stored config should exist");
    let stored: AppConfig =
        serde_json::from_str(&stored).expect("stored config should deserialize");
    assert_eq!(stored.client_root_url, "https://saved.example.com");
    assert_eq!(stored.connect_timeout_secs, 45);

    drop(state);
    fs::remove_dir_all(dir).expect("test data directory should be removed");
}

#[test]
fn gateway_port_override_is_effective_but_never_persisted() {
    let dir = temp_data_dir("gateway-port-override");
    let db = Database::open(dir.clone()).expect("test database should open");
    let cipher: Arc<dyn KeyCipher + Send + Sync> = Arc::new(StaticKeyCipher::new("state-test"));
    let state = CoreStateInner::new(db, dir.clone(), cipher).expect("state should initialize");

    state
        .register_gateway_port_override(19042)
        .expect("desktop host should register the override once");
    assert!(state.gateway_port_from_env());
    assert_eq!(state.settings_config().gateway_port, 19042);
    assert_eq!(state.active_gateway_port(), 19042);
    assert_eq!(state.config().gateway_port, 9042);

    let mut submitted = state.settings_config();
    submitted.connect_timeout_secs = 45;
    state
        .set_config(submitted)
        .expect("other settings should save while the override is active");
    assert_eq!(state.config().gateway_port, 9042);
    assert_eq!(state.config().connect_timeout_secs, 45);
    assert!(state.register_gateway_port_override(19043).is_err());

    drop(state);
    fs::remove_dir_all(dir).expect("test data directory should be removed");
}

#[test]
fn manual_proxy_config_is_normalized_and_persisted() {
    let dir = temp_data_dir("manual-proxy");
    let db = Database::open(dir.clone()).expect("test database should open");
    let cipher: Arc<dyn KeyCipher + Send + Sync> = Arc::new(StaticKeyCipher::new("state-test"));
    let state = CoreStateInner::new(db, dir.clone(), cipher).expect("state should initialize");
    let mut config = state.config();
    config.proxy_mode = ProxyMode::Manual;
    config.proxy_url = " http://127.0.0.1:7890/ ".to_string();

    state
        .set_config(config)
        .expect("manual proxy configuration should save");
    assert_eq!(state.config().proxy_mode, ProxyMode::Manual);
    assert_eq!(state.config().proxy_url, "http://127.0.0.1:7890");

    let stored = state
        .db
        .lock()
        .get_setting("config")
        .unwrap()
        .expect("config should be stored");
    let stored: AppConfig = serde_json::from_str(&stored).unwrap();
    assert_eq!(stored.proxy_mode, ProxyMode::Manual);
    assert_eq!(stored.proxy_url, "http://127.0.0.1:7890");

    drop(state);
    fs::remove_dir_all(dir).expect("test data directory should be removed");
}

#[test]
fn auto_proxy_saves_leftover_invalid_url_without_using_it() {
    let dir = temp_data_dir("auto-proxy-leftover");
    let db = Database::open(dir.clone()).expect("test database should open");
    let cipher: Arc<dyn KeyCipher + Send + Sync> = Arc::new(StaticKeyCipher::new("state-test"));
    let state = CoreStateInner::new(db, dir.clone(), cipher).expect("state should initialize");
    let mut config = state.config();
    config.proxy_mode = ProxyMode::Auto;
    config.proxy_url = "not-a-proxy".to_string();

    state
        .set_config(config)
        .expect("auto mode should ignore leftover invalid proxy URLs");
    assert_eq!(state.config().proxy_mode, ProxyMode::Auto);
    assert_eq!(state.config().proxy_url, "not-a-proxy");

    drop(state);
    fs::remove_dir_all(dir).expect("test data directory should be removed");
}

#[test]
fn startup_retries_committed_browser_profile_cleanup() {
    let dir = temp_data_dir("browser-profile-recovery");
    let db = Database::open(dir.clone()).expect("test database should open");
    let profile_root = dir.join("profiles");
    fs::create_dir_all(&profile_root).expect("legacy profile root should be created");
    let tombstone = profile_root.join(format!(
        ".ocg-profile-delete-deleted-account-{}",
        uuid::Uuid::new_v4().simple()
    ));
    fs::create_dir(&tombstone).expect("profile tombstone should be created");
    fs::write(tombstone.join("Cookies"), b"sensitive").expect("profile data should be created");
    let cipher: Arc<dyn KeyCipher + Send + Sync> = Arc::new(StaticKeyCipher::new("state-test"));

    let state = CoreStateInner::new(db, dir.clone(), cipher).expect("state should initialize");

    assert!(!tombstone.exists());
    drop(state);
    fs::remove_dir_all(dir).expect("test data directory should be removed");
}

#[test]
fn startup_finishes_reset_profile_journal_without_restoring_cookies() {
    use crate::browser::{BrowserProfileOperationKind, StagedBrowserProfiles};
    use crate::models::{Account, AccountSetupStep, AccountType};

    let dir = temp_data_dir("browser-profile-reset-journal");
    let db = Database::open(dir.clone()).expect("test database should open");
    let cipher: Arc<dyn KeyCipher + Send + Sync> = Arc::new(StaticKeyCipher::new("state-test"));
    let now = chrono::Utc::now();
    let account = Account {
        id: "existing-account".into(),
        provider_id: crate::provider::default_provider_id(),

        credential_kind: crate::provider::default_credential_kind(),
        quota_scope: crate::provider::default_quota_scope(),
        name: "existing-account".into(),
        username: None,
        password_cipher: None,
        key_cipher: cipher.encrypt("opaque-key").unwrap(),
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
    };
    db.create_account(&account)
        .expect("test account should be created");
    let profile = dir.join("browser-profiles").join(&account.id);
    fs::create_dir_all(&profile).expect("browser profile should be created");
    fs::write(profile.join("Cookies"), b"old-cookie").expect("browser cookie should be created");
    let staged =
        StagedBrowserProfiles::stage(&dir, &account.id, BrowserProfileOperationKind::ResetProfile)
            .expect("profile reset should be journaled and staged");
    assert!(!profile.exists());
    drop(staged); // simulate a crash before the normal purge step

    let state = CoreStateInner::new(db, dir.clone(), cipher).expect("state should initialize");

    assert!(state.db.lock().get_account(&account.id).unwrap().is_some());
    assert!(!profile.exists(), "reset recovery must not restore cookies");
    assert_eq!(
        fs::read_dir(dir.join("browser-profile-operations"))
            .unwrap()
            .count(),
        0,
        "completed recovery should remove its journal"
    );
    drop(state);
    fs::remove_dir_all(dir).expect("test data directory should be removed");
}

#[test]
fn route_set_snapshot_swaps_atomically_and_stays_self_consistent() {
    use crate::crypto::{KeyCipher, StaticKeyCipher};

    let dir = temp_data_dir("route-set-swap");
    let db = Database::open(dir.clone()).expect("test database should open");
    let cipher: Arc<dyn KeyCipher + Send + Sync> = Arc::new(StaticKeyCipher::new("state-test"));
    let state = CoreStateInner::new(db, dir.clone(), cipher).expect("state should initialize");

    let entry_snapshot = state.forward_route_set();
    assert!(
        std::ptr::eq(
            entry_snapshot.client_for("gpt-5.6-luna").0,
            state.forward_route_set().default_client()
        ),
        "non-list generations resolve to the single process-wide client"
    );

    let now = chrono::Utc::now();
    let scope =
        crate::provider_contracts::ContractScope::provider(crate::provider::OPENCODE_PROVIDER_ID);
    state
        .db
        .lock()
        .set_contract_catalog(
            &scope,
            &["gpt-5.6-luna".into()],
            Some(now),
            crate::provider_contracts::CATALOG_SOURCE_OPENCODE_MODELS,
            "https://example.test/models",
            now,
        )
        .unwrap();
    state.reload_provider_contracts().unwrap();

    let mut list_config = state.config();
    list_config.gateway_key = "gw".into();
    list_config.proxy_mode = crate::models::ProxyMode::List;
    list_config.proxy_url = "http://127.0.0.1:7890".into();
    list_config.proxy_list_direction = crate::models::ProxyListDirection::Whitelist;
    list_config.proxy_list_models = vec!["gpt-5.6-luna".to_string()];
    state
        .set_config(list_config)
        .expect("list config should save");

    // The active set was replaced wholesale with the new generation.
    let next_snapshot = state.forward_route_set();
    assert_eq!(next_snapshot.client_for("gpt-5.6-luna").1.as_str(), "proxy");
    assert_eq!(next_snapshot.client_for("glm-5.3").1.as_str(), "direct");

    // The in-flight snapshot keeps its own consistent routing: same model,
    // same old label, even though the config generation moved on.
    assert_eq!(entry_snapshot.client_for("gpt-5.6-luna").1.as_str(), "auto");

    drop(state);
    fs::remove_dir_all(dir).expect("test data directory should be removed");
}

fn routing_test_account(
    cipher: &Arc<dyn KeyCipher + Send + Sync>,
    id: &str,
) -> crate::models::Account {
    crate::models::Account {
        id: id.into(),
        provider_id: crate::provider::default_provider_id(),
        credential_kind: crate::provider::default_credential_kind(),
        quota_scope: crate::provider::default_quota_scope(),
        name: id.into(),
        username: None,
        password_cipher: None,
        key_cipher: cipher.encrypt(id).unwrap(),
        enabled: true,
        account_type: crate::models::AccountType::Key,
        setup_step: crate::models::AccountSetupStep::Ready,
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

#[test]
fn routing_runtime_resets_for_sticky_and_primary_key_not_invalid_or_timeout() {
    use crate::models::RoutingMode;

    let dir = temp_data_dir("routing-reset");
    let db = Database::open(dir.clone()).expect("test database should open");
    let cipher: Arc<dyn KeyCipher + Send + Sync> = Arc::new(StaticKeyCipher::new("state-test"));
    let state =
        CoreStateInner::new(db, dir.clone(), cipher.clone()).expect("state should initialize");
    let accounts = vec![
        routing_test_account(&cipher, "a"),
        routing_test_account(&cipher, "b"),
    ];
    let pick = || {
        state
            .routing
            .select_account(&accounts, RoutingMode::RoundRobin, false, None, &[])
            .unwrap()
            .id
            .clone()
    };

    // After only "a", keep yields "b" and a reset yields "a".
    assert_eq!(pick(), "a");
    let mut invalid = state.config();
    invalid.routing_mode = RoutingMode::StickyGlobal;
    invalid.connect_timeout_secs = 0;
    assert!(state.set_config(invalid).is_err());
    assert_eq!(
        pick(),
        "b",
        "failed config validation must not reset the active round-robin cursor"
    );

    // Consumed "b"; pick "a" so the cursor is after "a" again.
    assert_eq!(pick(), "a");
    let mut next = state.config();
    next.conversation_sticky = true;
    state
        .set_config(next)
        .expect("conversation sticky change should reset routing");
    assert_eq!(
        pick(),
        "a",
        "conversation sticky change should reset routing"
    );

    // Sticky reset consumed "a"; keep now yields "b", a further reset would yield "a".
    let mut config = state.config();
    config.connect_timeout_secs += 1;
    state.set_config(config).expect("unrelated save");
    assert_eq!(
        pick(),
        "b",
        "an unrelated settings save must not reset routing"
    );

    // Consumed "b"; pick "a" so the cursor is after "a" before key rotation.
    assert_eq!(pick(), "a");
    let mut config = state.config();
    config.gateway_key = "ocg-rotated-primary".to_string();
    state.set_config(config).expect("rotation should save");
    assert_eq!(
        pick(),
        "a",
        "the cursor restarts after the primary key value changes"
    );
    assert!(
        state
            .credential_entry_for_value("ocg-rotated-primary")
            .is_some()
    );

    drop(state);
    fs::remove_dir_all(dir).expect("test data directory should be removed");
}

#[test]
fn settings_revision_advances_only_after_successful_commit() {
    let dir = temp_data_dir("settings-revision");
    let db = Database::open(dir.clone()).expect("test database should open");
    let cipher: Arc<dyn KeyCipher + Send + Sync> = Arc::new(StaticKeyCipher::new("state-test"));
    let state = CoreStateInner::new(db, dir.clone(), cipher).expect("state should initialize");
    let initial_revision = state.settings_revision();

    let mut valid = state.config();
    valid.connect_timeout_secs += 1;
    state.set_config(valid).expect("valid settings should save");
    assert_eq!(state.settings_revision(), initial_revision + 1);

    let committed_revision = state.settings_revision();
    let mut invalid = state.config();
    invalid.connect_timeout_secs = 0;
    assert!(state.set_config(invalid).is_err());
    assert_eq!(state.settings_revision(), committed_revision);

    drop(state);
    fs::remove_dir_all(dir).expect("test data directory should be removed");
}

fn frozen_gateway_wall() -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::from_naive_utc_and_offset(
        chrono::NaiveDate::from_ymd_opt(2024, 1, 2)
            .unwrap()
            .and_hms_opt(3, 4, 5)
            .unwrap(),
        chrono::Utc,
    )
}

#[test]
fn production_core_state_samples_system_gateway_clocks() {
    let dir = temp_data_dir("gateway-clock-system");
    let db = Database::open(dir.clone()).expect("test database should open");
    let cipher: Arc<dyn KeyCipher + Send + Sync> = Arc::new(StaticKeyCipher::new("state-test"));
    let state = CoreStateInner::new(db, dir.clone(), cipher).expect("state should initialize");
    let before_wall = chrono::Utc::now();
    let before_mono = std::time::Instant::now();
    let (wall, mono) = state.sample_gateway_clock();
    let after_wall = chrono::Utc::now();
    let after_mono = std::time::Instant::now();
    assert!(wall >= before_wall - chrono::Duration::seconds(1));
    assert!(wall <= after_wall + chrono::Duration::seconds(1));
    assert!(mono >= before_mono);
    assert!(mono <= after_mono);
    drop(state);
    fs::remove_dir_all(dir).expect("test data directory should be removed");
}

#[test]
fn core_state_injects_immutable_gateway_clock_at_construction() {
    let dir = temp_data_dir("gateway-clock-injected");
    let db = Database::open(dir.clone()).expect("test database should open");
    let cipher: Arc<dyn KeyCipher + Send + Sync> = Arc::new(StaticKeyCipher::new("state-test"));
    let wall = frozen_gateway_wall();
    let mono = std::time::Instant::now() - std::time::Duration::from_secs(3_600);
    let state = CoreStateInner::new_with_test_gateway_clock(
        db,
        dir.clone(),
        cipher,
        move || wall,
        move || mono,
    )
    .expect("state should initialize with injected clocks");
    let (got_wall, got_mono) = state.sample_gateway_clock();
    assert_eq!(got_wall, wall);
    assert_eq!(got_mono, mono);
    drop(state);
    fs::remove_dir_all(dir).expect("test data directory should be removed");
}

#[test]
fn desktop_hooks_are_unset_on_a_headless_host() {
    let dir = temp_data_dir("desktop-headless");
    let db = Database::open(dir.clone()).expect("test database should open");
    let cipher: Arc<dyn KeyCipher + Send + Sync> = Arc::new(StaticKeyCipher::new("state-test"));
    let state = CoreStateInner::new(db, dir.clone(), cipher).expect("state should initialize");
    assert!(!state.auto_start_supported());
    assert!(!state.dock_visibility_supported());
    assert!(!state.desktop_update_supported());
    assert!(!state.desktop_update_status().install_supported);
    drop(state);
    fs::remove_dir_all(dir).expect("test data directory should be removed");
}

#[test]
fn desktop_update_core_state_busy_and_starter_failure() {
    let dir = temp_data_dir("desktop-update-state");
    let db = Database::open(dir.clone()).expect("test database should open");
    let cipher: Arc<dyn KeyCipher + Send + Sync> = Arc::new(StaticKeyCipher::new("state-test"));
    let state =
        Arc::new(CoreStateInner::new(db, dir.clone(), cipher).expect("state should initialize"));

    let started_versions = Arc::new(StdMutex::new(Vec::new()));
    let captured_versions = started_versions.clone();
    state.set_desktop_update_starter(Arc::new(move |expected_version| {
        captured_versions
            .lock()
            .expect("captured versions lock should work")
            .push(expected_version);
        Ok(())
    }));
    assert!(state.desktop_update_supported());
    assert!(state.desktop_update_status().install_supported);

    let barrier = Arc::new(Barrier::new(3));
    let threads = [state.clone(), state.clone()].map(|state| {
        let barrier = barrier.clone();
        std::thread::spawn(move || {
            barrier.wait();
            state.start_desktop_update("9.9.9".to_string())
        })
    });
    barrier.wait();
    let results = threads.map(|thread| thread.join().expect("start thread should not panic"));
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|result| matches!(result, Err(DesktopUpdateStartError::Busy)))
            .count(),
        1
    );
    assert_eq!(
        started_versions
            .lock()
            .expect("started versions lock should work")
            .as_slice(),
        ["9.9.9"]
    );
    assert_eq!(
        state.desktop_update_status().phase,
        DesktopUpdatePhase::Checking
    );
    drop(state);
    fs::remove_dir_all(dir).expect("test data directory should be removed");

    let dir = temp_data_dir("desktop-update-start-failure");
    let db = Database::open(dir.clone()).expect("test database should open");
    let cipher: Arc<dyn KeyCipher + Send + Sync> = Arc::new(StaticKeyCipher::new("state-test"));
    let state = CoreStateInner::new(db, dir.clone(), cipher).expect("state should initialize");
    state.set_desktop_update_starter(Arc::new(|_| anyhow::bail!("starter failed")));

    assert!(matches!(
        state.start_desktop_update("9.9.9".to_string()),
        Err(DesktopUpdateStartError::Starter(_))
    ));
    let status = state.desktop_update_status();
    assert_eq!(status.phase, DesktopUpdatePhase::Failed);
    assert_eq!(status.error.as_deref(), Some("starter failed"));

    drop(state);
    fs::remove_dir_all(dir).expect("test data directory should be removed");
}

#[test]
fn legacy_config_with_embedded_key_list_drops_it_and_keeps_the_scalar() {
    let dir = temp_data_dir("gateway-key-legacy-list");
    let db = Database::open(dir.clone()).expect("test database should open");
    // The never-released PR #43 form stored a key list inside the config
    // JSON; loading it must keep the scalar and ignore the list.
    let legacy = serde_json::json!({
        "gateway_port": 9042,
        "gateway_key": "ocg-legacy-key",
        "gateway_keys": [
            {
                "id": "old-primary",
                "name": "Primary",
                "key": "ocg-legacy-key",
                "enabled": true,
                "created_at": "2026-08-16T00:00:00Z"
            },
            {
                "id": "old-laptop",
                "name": "Laptop",
                "key": "ocg-old-laptop",
                "enabled": true,
                "created_at": "2026-08-16T00:00:00Z"
            }
        ],
        "upstream_base_url": "https://opencode.ai/zen/go" });
    db.set_setting("config", &legacy.to_string())
        .expect("legacy config should persist");
    let cipher: Arc<dyn KeyCipher + Send + Sync> = Arc::new(StaticKeyCipher::new("state-test"));
    let state = CoreStateInner::new(db, dir.clone(), cipher).expect("state should initialize");

    let config = state.config();
    assert!(
        !config.gateway_key.is_empty(),
        "the live primary still authenticates after v27"
    );
    assert_ne!(config.gateway_key, "ocg-old-laptop");
    assert!(
        state
            .credential_entry_for_value(&config.gateway_key)
            .is_some(),
        "access_keys is the database authority for the primary key"
    );
    assert!(
        state.credential_entry_for_value("ocg-old-laptop").is_none(),
        "the embedded sub key list is ignored"
    );

    drop(state);
    fs::remove_dir_all(dir).expect("test data directory should be removed");
}

#[test]
fn empty_config_generates_a_primary_key_value() {
    let dir = temp_data_dir("gateway-key-fresh");
    let db = Database::open(dir.clone()).expect("test database should open");
    let cipher: Arc<dyn KeyCipher + Send + Sync> = Arc::new(StaticKeyCipher::new("state-test"));
    let state = CoreStateInner::new(db, dir.clone(), cipher).expect("state should initialize");

    let config = state.config();
    assert!(!config.gateway_key.is_empty());
    assert!(
        state
            .credential_entry_for_value(&config.gateway_key)
            .is_some()
    );
    assert_eq!(
        state
            .client_key_name(crate::gateway_keys::PRIMARY_KEY_ID)
            .as_deref(),
        Some(crate::gateway_keys::PRIMARY_KEY_NAME),
    );

    drop(state);
    fs::remove_dir_all(dir).expect("test data directory should be removed");
}

#[test]
fn persisted_config_json_is_not_the_primary_key_authority() {
    let dir = temp_data_dir("gateway-key-sanitized");
    let db = Database::open(dir.clone()).expect("test database should open");
    let cipher: Arc<dyn KeyCipher + Send + Sync> = Arc::new(StaticKeyCipher::new("state-test"));
    let state = CoreStateInner::new(db, dir.clone(), cipher).expect("state should initialize");
    let mut config = state.config();
    config.gateway_key = "ocg-authority-primary".to_string();
    state
        .set_config(config)
        .expect("primary rotation should save");
    assert_eq!(state.config().gateway_key, "ocg-authority-primary");
    assert_eq!(
        state
            .db
            .lock()
            .primary_access_key_value()
            .unwrap()
            .as_deref(),
        Some("ocg-authority-primary")
    );
    let stored: serde_json::Value = serde_json::from_str(
        &state
            .db
            .lock()
            .get_setting("config")
            .unwrap()
            .expect("config json should exist"),
    )
    .unwrap();
    assert_eq!(stored["gateway_key"], "");
    drop(state);
    fs::remove_dir_all(dir).expect("test data directory should be removed");
}

#[test]
fn legacy_config_gets_persisted_desktop_defaults() {
    let dir = temp_data_dir("desktop-config-migration");
    let db = Database::open(dir.clone()).expect("test database should open");
    let mut legacy = serde_json::to_value(AppConfig {
        gateway_key: "test-gateway-key".to_string(),
        ..AppConfig::default()
    })
    .expect("test config should serialize");
    {
        let legacy_object = legacy
            .as_object_mut()
            .expect("test config should be an object");
        legacy_object.remove("show_dock_icon");
        legacy_object.remove("routing_mode");
        legacy_object.remove("conversation_sticky");
        legacy_object.remove("proxy_mode");
        legacy_object.remove("proxy_url");
    }
    db.set_setting("config", &legacy.to_string())
        .expect("legacy config should persist");
    let cipher: Arc<dyn KeyCipher + Send + Sync> = Arc::new(StaticKeyCipher::new("state-test"));
    let state = CoreStateInner::new(db, dir.clone(), cipher).expect("state should initialize");

    assert!(state.config().show_dock_icon);
    assert_eq!(
        state.config().routing_mode,
        crate::models::RoutingMode::StrictPriority
    );
    assert!(!state.config().conversation_sticky);
    assert_eq!(state.config().proxy_mode, ProxyMode::Auto);
    assert!(state.config().proxy_url.is_empty());
    let stored = state
        .db
        .lock()
        .get_setting("config")
        .expect("stored config should be readable")
        .expect("stored config should exist");
    assert!(stored.contains("show_dock_icon"));
    assert!(stored.contains("proxy_mode"));
    assert!(stored.contains("proxy_url"));

    drop(state);
    fs::remove_dir_all(dir).expect("test data directory should be removed");
}

#[test]
fn zen_activation_and_contract_reload_share_documented_lock_order() {
    let dir = temp_data_dir("zen-contract-lock-order");
    let db = Database::open(dir.clone()).expect("test database should open");
    let cipher: Arc<dyn KeyCipher + Send + Sync> = Arc::new(StaticKeyCipher::new("state-test"));
    let state =
        Arc::new(CoreStateInner::new(db, dir.clone(), cipher).expect("state should initialize"));
    let barrier = Arc::new(Barrier::new(2));
    let activator = {
        let state = state.clone();
        let barrier = barrier.clone();
        std::thread::spawn(move || {
            barrier.wait();
            for i in 0..8 {
                state
                    .activate_zen_free_model_catalog(crate::kernel::zen::ZenFreeModelCatalog {
                        models: vec![format!("lock-order-free-{i}")],
                        refreshed_at: Some(chrono::Utc::now()),
                        source_url: crate::kernel::zen::ZEN_MODELS_SOURCE_URL.into(),
                    })
                    .expect("zen catalog activation should not deadlock");
            }
        })
    };
    let reloader = {
        let state = state.clone();
        std::thread::spawn(move || {
            barrier.wait();
            for _ in 0..8 {
                state
                    .reload_provider_contracts()
                    .expect("contract reload should not deadlock");
                let _ = state.provider_contracts();
                let _ = state.zen_free_model_catalog();
                let _ = state.forward_route_set();
            }
        })
    };
    activator.join().expect("activator thread");
    reloader.join().expect("reloader thread");
    drop(state);
    fs::remove_dir_all(dir).expect("test data directory should be removed");
}

#[test]
fn zen_activation_installs_the_just_persisted_catalog_with_new_models_off() {
    let dir = temp_data_dir("zen-activation-snapshot");
    let db = Database::open(dir.clone()).expect("test database should open");
    let cipher: Arc<dyn KeyCipher + Send + Sync> = Arc::new(StaticKeyCipher::new("state-test"));
    let state = CoreStateInner::new(db, dir.clone(), cipher).expect("state should initialize");

    let mut config = state.config();
    config.proxy_mode = ProxyMode::List;
    config.proxy_url = "http://127.0.0.1:9".into();
    config.proxy_list_direction = ProxyListDirection::Whitelist;
    config.proxy_list_models = vec!["first-free".into(), "replacement-free".into()];
    state.set_config(config).unwrap();

    for model in ["first-free", "replacement-free"] {
        state
            .activate_zen_free_model_catalog(crate::kernel::zen::ZenFreeModelCatalog {
                models: vec![model.into()],
                refreshed_at: Some(chrono::Utc::now()),
                source_url: crate::kernel::zen::ZEN_MODELS_SOURCE_URL.into(),
            })
            .unwrap();

        let scope = crate::provider_contracts::ContractScope::provider(
            crate::provider::OPENCODE_ZEN_FREE_PROVIDER_ID,
        );
        let contract = state.provider_contracts().scope(&scope).cloned().unwrap();
        assert_eq!(contract.catalog.models, [model]);
        assert!(
            contract
                .models
                .values()
                .find(|row| row.model_id == model)
                .is_some_and(|row| !row.has_enabled_protocol()),
            "new Zen catalog rows must be installed default-off"
        );
        assert_eq!(
            state.forward_route_set().client_for(model).1.as_str(),
            "direct",
            "default-off Zen rows must stay out of the proxy exception set"
        );
    }

    drop(state);
    fs::remove_dir_all(dir).expect("test data directory should be removed");
}

#[test]
fn proxy_candidates_use_exact_upstream_ids_not_public_aliases() {
    let now = chrono::Utc::now();
    let custom_runtime = crate::custom::CustomAccountRuntime {
        account_id: "custom-1".into(),
        enabled: true,
        verification_status: crate::provider::ConnectionVerificationStatus::Verified,
        setup_ready: true,
        has_key: true,
        config: crate::models::AccountCustomConfig {
            account_id: "custom-1".into(),
            endpoint_url: "https://api.example.com/v1/messages".into(),
            upstream_protocol: crate::provider::UpstreamProtocolKind::Messages,
            created_at: now,
            updated_at: now,
        },
        capabilities: vec![crate::models::AccountModelCapability {
            account_id: "custom-1".into(),
            public_model: "lab-opus".into(),
            upstream_model: "vendor/opus".into(),
            protocol: crate::provider::UpstreamProtocolKind::Messages,
            verified_at: None,
            source: "declared".into(),
        }],
        protocol_passthrough: false,
    };
    let mut inactive_custom = custom_runtime.clone();
    inactive_custom.account_id = "custom-disabled".into();
    inactive_custom.enabled = false;
    inactive_custom.capabilities[0].public_model = "disabled-public".into();
    inactive_custom.capabilities[0].upstream_model = "vendor/disabled".into();
    let custom = [custom_runtime, inactive_custom];
    let dynamics = [crate::dynamic::DynamicProviderRuntime {
        preset_id: None,
        id: "11111111-1111-1111-1111-111111111111".into(),
        name: "Lab".into(),
        endpoint_url: "https://lab.example/v1/chat/completions".into(),
        upstream_protocol: crate::provider::UpstreamProtocolKind::ChatCompletions,
        auth_kind: ocg_domain::dynamic::DynamicAuthKind::Bearer,
        mappings: vec![ocg_domain::dynamic::DynamicModelMapping {
            public_model: "lab-chat".into(),
            upstream_model: "vendor/chat".into(),
            upstream_override: None,
        }],
        created_at: now,
        updated_at: now,
        origin: crate::provider::ProviderOrigin::Custom,
        offering: "api".into(),
    }];
    let candidates = build_proxy_model_candidates(
        &crate::provider_contracts::EffectiveContractSet::default(),
        &custom,
        &dynamics,
        &["cpa-model".into()],
    );
    let ids: Vec<&str> = candidates
        .iter()
        .map(|candidate| candidate.id.as_str())
        .collect();
    assert!(ids.contains(&"vendor/opus"), "{ids:?}");
    assert!(ids.contains(&"vendor/chat"), "{ids:?}");
    assert!(ids.contains(&"cpa-model"), "{ids:?}");
    assert!(!ids.contains(&"vendor/disabled"), "{ids:?}");
    assert!(!ids.contains(&"lab-opus"), "{ids:?}");
    assert!(!ids.contains(&"lab-chat"), "{ids:?}");
}

//! Integration tests for the `ocg-core` refactor.
//!
//! Covers the surfaces that the GUI/CLI split could have broken:
//! - `CoreState` persistence (cipher round-trip across reopen)
//! - `CoreState` config round-trip (gateway_key auto-generation, persistence)
//! - Cross-cipher incompatibility: an account encrypted with one cipher
//!   cannot be decrypted by another — the safety property the README warns about.

use chrono::{Duration, Utc};
use ocg_core::crypto::{KeyCipher, MachineBoundCipher, StaticKeyCipher};
use ocg_core::db::{Database, ForwardLogQueryOptions};
use ocg_core::models::{Account, ForwardLog, normalize_client_root_url};
use ocg_core::state::CoreStateInner;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

fn temp_data_dir(label: &str) -> PathBuf {
    let mut dir = std::env::temp_dir();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    dir.push(format!("ocg-core-test-{label}-{nanos}"));
    fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn core_state_persists_account_through_static_cipher() {
    let dir = temp_data_dir("persist");
    let cipher: Arc<dyn KeyCipher + Send + Sync> = Arc::new(StaticKeyCipher::new("secret-A"));
    let db = Database::open(dir.clone()).unwrap();
    let state = Arc::new(CoreStateInner::new(db, dir.clone(), cipher).unwrap());

    let account = Account {
        id: "acct-1".into(),
        provider_id: ocg_core::provider::default_provider_id(),

        credential_kind: ocg_core::provider::default_credential_kind(),
        quota_scope: ocg_core::provider::default_quota_scope(),
        name: "main".into(),
        username: None,
        password_cipher: None,
        key_cipher: state.encrypt_key("sk-ocg-plaintext-123").unwrap(),
        enabled: true,
        account_type: ocg_core::models::AccountType::Key,
        setup_step: ocg_core::models::AccountSetupStep::Ready,
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
    };
    state.db.lock().create_account(&account).unwrap();

    // Reopen with the same cipher — must decrypt cleanly.
    let db2 = Database::open(dir.clone()).unwrap();
    let cipher2: Arc<dyn KeyCipher + Send + Sync> = Arc::new(StaticKeyCipher::new("secret-A"));
    let state2 = Arc::new(CoreStateInner::new(db2, dir, cipher2).unwrap());

    let stored = state2.db.lock().get_account("acct-1").unwrap().unwrap();
    let decrypted = state2.decrypt_key(&stored.key_cipher).unwrap();
    assert_eq!(decrypted, "sk-ocg-plaintext-123");
}

#[test]
fn core_state_with_wrong_cipher_cannot_decrypt_existing_account() {
    let dir = temp_data_dir("mismatch");

    // Write with cipher A.
    {
        let cipher: Arc<dyn KeyCipher + Send + Sync> = Arc::new(StaticKeyCipher::new("A"));
        let db = Database::open(dir.clone()).unwrap();
        let state = Arc::new(CoreStateInner::new(db, dir.clone(), cipher).unwrap());
        let account = Account {
            id: "acct-2".into(),
            provider_id: ocg_core::provider::default_provider_id(),

            credential_kind: ocg_core::provider::default_credential_kind(),
            quota_scope: ocg_core::provider::default_quota_scope(),
            name: "alt".into(),
            username: None,
            password_cipher: None,
            key_cipher: state.encrypt_key("sk-ocg-plaintext-456").unwrap(),
            enabled: true,
            account_type: ocg_core::models::AccountType::Key,
            setup_step: ocg_core::models::AccountSetupStep::Ready,
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
        };
        state.db.lock().create_account(&account).unwrap();
    }

    // Read with cipher B — must fail or return garbage, NEVER the plaintext.
    let db = Database::open(dir.clone()).unwrap();
    let cipher: Arc<dyn KeyCipher + Send + Sync> = Arc::new(StaticKeyCipher::new("B"));
    let state = Arc::new(CoreStateInner::new(db, dir, cipher).unwrap());
    let stored = state.db.lock().get_account("acct-2").unwrap().unwrap();
    let result = state.decrypt_key(&stored.key_cipher);
    match result {
        Err(_) => {} // invalid utf-8 — fine
        Ok(s) => assert_ne!(s, "sk-ocg-plaintext-456"),
    }
}

#[test]
fn core_state_generates_gateway_key_on_first_run_and_persists() {
    let dir = temp_data_dir("gwkey");
    let cipher: Arc<dyn KeyCipher + Send + Sync> = Arc::new(StaticKeyCipher::new("k"));

    // First open — gateway_key should be auto-generated and look like ocg-word-word.
    let db = Database::open(dir.clone()).unwrap();
    let state = Arc::new(CoreStateInner::new(db, dir.clone(), cipher).unwrap());
    let key1 = state.config().gateway_key;
    assert!(
        key1.starts_with("ocg-"),
        "expected auto-generated gateway key, got {key1:?}"
    );

    // Reopen — same key, persisted in settings table.
    let db2 = Database::open(dir).unwrap();
    let cipher2: Arc<dyn KeyCipher + Send + Sync> = Arc::new(StaticKeyCipher::new("k"));
    let state2 = Arc::new(CoreStateInner::new(db2, PathBuf::from("."), cipher2).unwrap());
    assert_eq!(state2.config().gateway_key, key1);
}

#[test]
fn core_state_set_config_persists() {
    let dir = temp_data_dir("cfg");
    let cipher: Arc<dyn KeyCipher + Send + Sync> = Arc::new(StaticKeyCipher::new("k"));
    let db = Database::open(dir.clone()).unwrap();
    let state = Arc::new(CoreStateInner::new(db, dir.clone(), cipher).unwrap());

    let mut cfg = state.config();
    cfg.gateway_port = 9999;
    cfg.client_root_url = "https://gateway.example.com/ocg".into();
    cfg.connect_timeout_secs = 12;
    cfg.non_stream_timeout_secs = 345;
    cfg.stream_idle_timeout_secs = 678;
    state.set_config(cfg).unwrap();

    // Reopen and verify.
    let db2 = Database::open(dir).unwrap();
    let cipher2: Arc<dyn KeyCipher + Send + Sync> = Arc::new(StaticKeyCipher::new("k"));
    let state2 = Arc::new(CoreStateInner::new(db2, PathBuf::from("."), cipher2).unwrap());
    let persisted = state2.config();
    assert_eq!(persisted.gateway_port, 9999);
    assert_eq!(persisted.client_root_url, "https://gateway.example.com/ocg");
    assert_eq!(persisted.connect_timeout_secs, 12);
    assert_eq!(persisted.non_stream_timeout_secs, 345);
    assert_eq!(persisted.stream_idle_timeout_secs, 678);
}

#[test]
fn core_state_scrubs_removed_config_fields() {
    let dir = temp_data_dir("removed-config");
    let db = Database::open(dir.clone()).unwrap();
    db.set_setting(
        "config",
        r#"{"gateway_port":9042,"gateway_key":"gw","upstream_base_url":"https://example.com","auto_start":false,"remote":{"url":"https://old.example.com","token":"remote-secret"}}"#,
    )
    .unwrap();
    let cipher: Arc<dyn KeyCipher + Send + Sync> = Arc::new(StaticKeyCipher::new("k"));
    let state = Arc::new(CoreStateInner::new(db, dir, cipher).unwrap());

    let config = state.config();
    assert_eq!(config.client_root_url, "");
    assert_eq!(config.connect_timeout_secs, 30);
    assert_eq!(config.non_stream_timeout_secs, 900);
    assert_eq!(config.stream_idle_timeout_secs, 300);

    let persisted = state.db.lock().get_setting("config").unwrap().unwrap();
    assert!(!persisted.contains("remote"));
    assert!(!persisted.contains("remote-secret"));
    let persisted: serde_json::Value = serde_json::from_str(&persisted).unwrap();
    assert_eq!(persisted["client_root_url"], "");
    assert_eq!(persisted["connect_timeout_secs"], 30);
    assert_eq!(persisted["non_stream_timeout_secs"], 900);
    assert_eq!(persisted["stream_idle_timeout_secs"], 300);
}

#[test]
fn core_state_migrates_only_the_untouched_legacy_timeout_tuple() {
    let legacy_dir = temp_data_dir("legacy-timeout-defaults");
    let legacy_db = Database::open(legacy_dir.clone()).unwrap();
    legacy_db
        .set_setting(
            "config",
            r#"{"gateway_port":9042,"gateway_key":"gw","upstream_base_url":"https://example.com","client_root_url":"","auto_start":false,"connect_timeout_secs":30,"non_stream_timeout_secs":120,"stream_idle_timeout_secs":300}"#,
        )
        .unwrap();
    let cipher: Arc<dyn KeyCipher + Send + Sync> = Arc::new(StaticKeyCipher::new("k"));
    let state = Arc::new(CoreStateInner::new(legacy_db, legacy_dir, cipher).unwrap());
    assert_eq!(state.config().non_stream_timeout_secs, 900);
    let persisted: serde_json::Value =
        serde_json::from_str(&state.db.lock().get_setting("config").unwrap().unwrap()).unwrap();
    assert_eq!(persisted["non_stream_timeout_secs"], 900);

    let custom_dir = temp_data_dir("customized-timeout-defaults");
    let custom_db = Database::open(custom_dir.clone()).unwrap();
    custom_db
        .set_setting(
            "config",
            r#"{"gateway_port":9042,"gateway_key":"gw","upstream_base_url":"https://example.com","client_root_url":"","auto_start":false,"connect_timeout_secs":31,"non_stream_timeout_secs":120,"stream_idle_timeout_secs":300}"#,
        )
        .unwrap();
    let cipher: Arc<dyn KeyCipher + Send + Sync> = Arc::new(StaticKeyCipher::new("k"));
    let state = Arc::new(CoreStateInner::new(custom_db, custom_dir, cipher).unwrap());
    assert_eq!(state.config().connect_timeout_secs, 31);
    assert_eq!(state.config().non_stream_timeout_secs, 120);
    assert_eq!(state.config().stream_idle_timeout_secs, 300);
}

#[test]
fn client_root_url_normalizes_and_rejects_endpoints() {
    for (input, expected) in [
        ("", ""),
        ("  https://ocg.example.com///  ", "https://ocg.example.com"),
        (
            "http://192.168.1.20:9042/proxy/v1/",
            "http://192.168.1.20:9042/proxy",
        ),
        (
            "https://ocg.example.com/proxy/V1",
            "https://ocg.example.com/proxy",
        ),
        (
            "https://ocg.example.com/reverse/proxy",
            "https://ocg.example.com/reverse/proxy",
        ),
    ] {
        assert_eq!(normalize_client_root_url(input).unwrap(), expected);
    }

    for input in [
        "ocg.example.com",
        "http:example.com/",
        "http:/example.com/",
        "ftp://ocg.example.com",
        "https://user:secret@ocg.example.com",
        "https://ocg.example.com?node=one",
        "https://ocg.example.com#settings",
        "https://ocg.example.com/v1/chat/completions",
        "https://ocg.example.com/proxy/v1/responses",
    ] {
        assert!(
            normalize_client_root_url(input).is_err(),
            "expected {input:?} to be rejected"
        );
    }
}

#[test]
fn machine_bound_cipher_roundtrip_through_core_state() {
    // Sanity check that the GUI's default cipher flows through CoreState correctly.
    let dir = temp_data_dir("machine");
    let cipher: Arc<dyn KeyCipher + Send + Sync> = Arc::new(MachineBoundCipher::new());
    let db = Database::open(dir).unwrap();
    let state = Arc::new(CoreStateInner::new(db, PathBuf::from("."), cipher).unwrap());
    let enc = state.encrypt_key("sk-ocg-machine-bound").unwrap();
    let dec = state.decrypt_key(&enc).unwrap();
    assert_eq!(dec, "sk-ocg-machine-bound");
}

#[test]
fn query_forward_logs_filters_before_limit_and_summarizes_all_matches() {
    let dir = temp_data_dir("filtered-logs");
    let db = Database::open(dir).unwrap();

    for (status, prompt, completion, cached, cost) in [
        ("success", 10, 20, 3, 1.0),
        ("success", 30, 40, 5, 2.0),
        ("error", 90, 90, 90, 9.0),
    ] {
        db.log_forward(&ForwardLog {
            id: 0,
            timestamp: Utc::now(),
            model: "glm-5.2".into(),
            account_id: "selected".into(),
            account_name: "selected".into(),
            route_account_id: None,
            provider_id: None,

            credential_account_id: None,
            client_key_id: None,
            client_key_name: None,
            status: status.into(),
            http_status: Some(200),
            route: String::new(),
            prompt_tokens: prompt,
            completion_tokens: completion,
            cached_tokens: cached,
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
        })
        .unwrap();
    }

    // Push every matching row beyond the old global top-200 window.
    for index in 0..200 {
        db.log_forward(&ForwardLog {
            id: 0,
            timestamp: Utc::now(),
            model: "other".into(),
            account_id: "busy".into(),
            account_name: format!("busy-{index}"),
            route_account_id: None,
            provider_id: None,

            credential_account_id: None,
            client_key_id: None,
            client_key_name: None,
            status: "success".into(),
            http_status: Some(200),
            route: String::new(),
            prompt_tokens: 1_000,
            completion_tokens: 1_000,
            cached_tokens: 1_000,
            cache_creation_tokens: 0,
            cost: Some(100.0),
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
        })
        .unwrap();
    }

    let first = db
        .query_forward_logs(ForwardLogQueryOptions {
            limit: 1,
            offset: 0,
            status: Some("success"),
            account_id: Some("selected"),
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
    assert_eq!(first.items.len(), 1);
    assert_eq!(first.items[0].prompt_tokens, 30);
    assert_eq!(first.summary.total_requests, 2);
    assert_eq!(first.summary.prompt_tokens, 40);
    assert_eq!(first.summary.completion_tokens, 60);
    assert_eq!(first.summary.cached_tokens, 8);
    assert!((first.summary.cost - 3.0).abs() < f64::EPSILON);

    let second = db
        .query_forward_logs(ForwardLogQueryOptions {
            limit: 1,
            offset: 1,
            status: Some("success"),
            account_id: Some("selected"),
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
    assert_eq!(second.items.len(), 1);
    assert_eq!(second.items[0].prompt_tokens, 10);
    assert_eq!(second.summary.total_requests, 2);

    let bounded = db
        .query_forward_logs(ForwardLogQueryOptions {
            limit: 999,
            offset: -1,
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
    assert_eq!(bounded.items.len(), 200);
    assert_eq!(bounded.summary.total_requests, 203);
}

#[test]
fn daily_tokens_by_model_counts_all_rows_with_tokens_regardless_of_cost_state() {
    let dir = temp_data_dir("daily-tokens");
    let db = Database::open(dir).unwrap();
    let today = Utc::now();
    // (model, prompt_tokens, completion_tokens, cost_state, status, day_offset)
    for (model, prompt, completion, cost_state, status, offset) in [
        // priced success today -> counted
        ("glm-5.2", 10_i64, 20_i64, "priced", "success", 0),
        // legacy estimate success today, same model -> aggregated
        ("glm-5.2", 5, 10, "legacy_estimate", "success", 0),
        // priced success yesterday, different model -> separate day/model bucket
        ("kimi-k2.7-code", 100, 50, "priced", "success", 1),
        // free / not_applicable row still has tokens -> counted
        ("glm-5.2", 8, 12, "not_applicable", "success", 0),
        // error row with tokens -> counted (token usage is independent of cost state)
        ("glm-5.2", 2, 3, "not_applicable", "error", 0),
        // zero-token row -> excluded entirely
        ("glm-5.2", 0, 0, "priced", "success", 0),
    ] {
        db.log_forward(&ForwardLog {
            id: 0,
            timestamp: today - Duration::days(offset),
            model: model.into(),
            account_id: "acct".into(),
            account_name: "main".into(),
            route_account_id: None,
            provider_id: None,

            credential_account_id: None,
            client_key_id: None,
            client_key_name: None,
            status: status.into(),
            http_status: Some(200),
            route: String::new(),
            prompt_tokens: prompt,
            completion_tokens: completion,
            cached_tokens: 0,
            cache_creation_tokens: 0,
            cost: None,
            raw_cost_usd: None,
            quota_debit: None,
            effective_paid_cost_usd: None,
            pricing_revision_id: None,
            quota_multiplier: None,
            local_adjustment_multiplier: None,
            service_tier: None,
            cost_state: cost_state.into(),
            error_message: None,
            request_id: None,
            attempt: None,
            error_source: None,
            error_stage: None,
            duration_ms: None,
            diagnostic: None,
        })
        .unwrap();
    }

    let rows = db.daily_tokens_by_model(3).unwrap();
    assert_eq!(rows.len(), 2);
    let glm_today = rows
        .iter()
        .find(|row| row.model == "glm-5.2")
        .expect("glm-5.2 row present");
    // 10+20 + 5+10 + 8+12 + 2+3 = 70
    assert_eq!(glm_today.tokens, 70);
    let kimi_yesterday = rows
        .iter()
        .find(|row| row.model == "kimi-k2.7-code")
        .expect("kimi-k2.7-code row present");
    assert_eq!(kimi_yesterday.tokens, 150);
}

#[test]
fn single_key_upgrade_drill_backfills_logs_to_the_primary_key_id() {
    // Build the pre-multi-key data shape: a config JSON without any key
    // list plus forward_logs rows without a client key.
    let dir = temp_data_dir("upgrade-drill");
    let db = Database::open(dir.clone()).unwrap();
    db.set_config(
        &serde_json::json!({
            "gateway_port": 9042,
            "gateway_key": "ocg-legacy-value",
            "upstream_base_url": "https://opencode.ai/zen/go"
        })
        .to_string(),
    )
    .unwrap();
    for index in 0..3 {
        let mut log = ForwardLog {
            id: 0,
            timestamp: chrono::Utc::now(),
            model: "glm-5.2".into(),
            account_id: "acct".into(),
            account_name: "acct".into(),
            route_account_id: None,
            provider_id: None,

            credential_account_id: None,
            client_key_id: None,
            client_key_name: None,
            status: "success".into(),
            http_status: Some(200),
            route: String::new(),
            prompt_tokens: 1,
            completion_tokens: 1,
            cached_tokens: 0,
            cache_creation_tokens: 0,
            cost: Some(0.5),
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
        };
        log.model = format!("m{index}");
        db.log_forward(&log).unwrap();
    }
    drop(db);

    // "Upgrade": the new binary opens the same data directory.
    let cipher: Arc<StaticKeyCipher> = Arc::new(StaticKeyCipher::new("drill"));
    let state = CoreStateInner::new(
        Database::open(dir.clone()).unwrap(),
        dir.clone(),
        cipher as Arc<dyn KeyCipher + Send + Sync>,
    )
    .unwrap();

    // The legacy scalar still authenticates as the primary key under its
    // fixed hardcoded id.
    let config = state.config();
    assert_eq!(config.gateway_key, "ocg-legacy-value");
    assert!(
        state
            .credential_entry_for_value("ocg-legacy-value")
            .is_some()
    );
    assert_eq!(
        state
            .credential_entry_for_value("ocg-legacy-value")
            .unwrap()
            .id,
        ocg_core::gateway_keys::PRIMARY_KEY_ID
    );

    // Run the startup backfill to completion: historical rows attribute to
    // the fixed primary id and remain filterable under it.
    let primary_id = ocg_core::gateway_keys::PRIMARY_KEY_ID.to_string();
    let primary_name = ocg_core::gateway_keys::PRIMARY_KEY_NAME.to_string();
    let mut more = true;
    while more {
        more = state
            .db
            .lock()
            .backfill_forward_logs_client_key_step(
                &primary_id,
                &primary_name,
                ocg_core::db::FORWARD_LOG_BACKFILL_CHUNK_ROWS,
            )
            .unwrap();
    }
    let page = state
        .db
        .lock()
        .query_forward_logs(ForwardLogQueryOptions {
            limit: 10,
            offset: 0,
            status: None,
            account_id: None,
            provider_id: None,

            route_account_id: None,
            credential_account_id: None,
            model: None,
            key_id: Some(primary_id.as_str()),
            request_id: None,
            start_time: None,
            end_time: None,
            sort_by: None,
            sort_order: None,
        })
        .unwrap();
    assert_eq!(page.summary.total_requests, 3);
    assert!(
        page.items
            .iter()
            .all(|log| log.client_key_name.as_deref() == Some("Primary"))
    );

    drop(state);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn downgrade_drill_keeps_sub_keys_and_never_resurrects_revoked_ones() {
    let dir = temp_data_dir("downgrade-drill");
    let cipher: Arc<StaticKeyCipher> = Arc::new(StaticKeyCipher::new("drill"));
    let state = CoreStateInner::new(
        Database::open(dir.clone()).unwrap(),
        dir.clone(),
        cipher.clone() as Arc<dyn KeyCipher + Send + Sync>,
    )
    .unwrap();
    let enabled = ocg_core::gateway_keys::create_sub_key(&state, "Laptop").unwrap();
    let disabled = ocg_core::gateway_keys::create_sub_key(&state, "Paused").unwrap();
    ocg_core::gateway_keys::set_sub_key_enabled(&state, &disabled.id, false).unwrap();
    let before = state.config();
    let primary_value = before.gateway_key.clone();

    // "Downgrade": an old single-key binary rewrites the stored config with
    // its own (list-free) shape and saves settings; it never reads or
    // rewrites the sub key table.
    let legacy_json = serde_json::to_value(&before).unwrap();
    {
        let db = state.db.lock();
        db.set_setting("config", &legacy_json.to_string()).unwrap();
        // Simulated old-binary settings save: full config rewrite, sub key
        // table untouched.
        assert_eq!(db.count_active_sub_gateway_keys().unwrap(), 2);
    }

    // "Re-upgrade": the new binary loads the stored config and table again.
    drop(state);
    let state = CoreStateInner::new(
        Database::open(dir.clone()).unwrap(),
        dir.clone(),
        cipher as Arc<dyn KeyCipher + Send + Sync>,
    )
    .unwrap();
    let config = state.config();
    // The primary value survived; every sub key is intact with its state.
    assert_eq!(config.gateway_key, primary_value);
    assert!(state.credential_entry_for_value(&primary_value).is_some());
    assert!(state.credential_entry_for_value(&enabled.key).is_some());
    assert!(
        state.credential_entry_for_value(&disabled.key).is_none(),
        "the disabled sub key never authenticates on either binary"
    );
    let reloaded = state
        .db
        .lock()
        .get_sub_gateway_key(&disabled.id)
        .unwrap()
        .unwrap();
    assert!(!reloaded.enabled);
    assert!(reloaded.deleted_at.is_none());

    drop(state);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn backfill_restarts_when_rows_appear_after_completion() {
    let dir = temp_data_dir("backfill-restart");
    let cipher: Arc<StaticKeyCipher> = Arc::new(StaticKeyCipher::new("drill"));
    let state = CoreStateInner::new(
        Database::open(dir.clone()).unwrap(),
        dir.clone(),
        cipher as Arc<dyn KeyCipher + Send + Sync>,
    )
    .unwrap();
    let primary_id = ocg_core::gateway_keys::PRIMARY_KEY_ID.to_string();
    let primary_name = ocg_core::gateway_keys::PRIMARY_KEY_NAME.to_string();
    let run_backfill = |state: &CoreStateInner| {
        let mut more = true;
        while more {
            more = state
                .db
                .lock()
                .backfill_forward_logs_client_key_step(
                    &primary_id,
                    &primary_name,
                    ocg_core::db::FORWARD_LOG_BACKFILL_CHUNK_ROWS,
                )
                .unwrap();
        }
    };

    // Empty table: the backfill completes immediately.
    run_backfill(&state);
    assert_eq!(
        state
            .db
            .lock()
            .forward_log_backfill_marker()
            .unwrap()
            .as_deref(),
        Some(ocg_core::db::BACKFILL_DONE)
    );

    // A downgrade window writes unattributed rows after completion.
    let mut unattributed = ForwardLog {
        id: 0,
        timestamp: chrono::Utc::now(),
        model: "glm-5.2".into(),
        account_id: "acct".into(),
        account_name: "acct".into(),
        route_account_id: None,
        provider_id: None,

        credential_account_id: None,
        client_key_id: None,
        client_key_name: None,
        status: "success".into(),
        http_status: Some(200),
        route: String::new(),
        prompt_tokens: 1,
        completion_tokens: 1,
        cached_tokens: 0,
        cache_creation_tokens: 0,
        cost: None,
        raw_cost_usd: None,
        quota_debit: None,
        effective_paid_cost_usd: None,
        pricing_revision_id: None,
        quota_multiplier: None,
        local_adjustment_multiplier: None,
        service_tier: None,
        cost_state: "not_applicable".into(),
        error_message: None,
        request_id: None,
        attempt: None,
        error_source: None,
        error_stage: None,
        duration_ms: None,
        diagnostic: None,
    };
    unattributed.model = "late".into();
    state.db.lock().log_forward(&unattributed).unwrap();

    // The next run (i.e. after restart) detects the NULL rows and
    // re-attributes them without a full-table scan.
    run_backfill(&state);
    let page = state
        .db
        .lock()
        .query_forward_logs(ForwardLogQueryOptions {
            limit: 10,
            offset: 0,
            status: None,
            account_id: None,
            provider_id: None,

            route_account_id: None,
            credential_account_id: None,
            model: None,
            key_id: Some(&primary_id),
            request_id: None,
            start_time: None,
            end_time: None,
            sort_by: None,
            sort_order: None,
        })
        .unwrap();
    assert_eq!(page.summary.total_requests, 1);
    assert_eq!(page.items[0].model, "late");

    drop(state);
    fs::remove_dir_all(dir).unwrap();
}

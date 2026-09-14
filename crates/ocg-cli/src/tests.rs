use super::{
    Cli, Commands, KeyAction, build_state, key_command, ping_keys, resolve_cipher_with,
    resolve_dashboard_dir, resolve_data_dir, start_serve, status_command, stop_serve,
    toggle_account,
};
use chrono::Utc;
use clap::{CommandFactory, Parser};
use ocg_core::browser::browser_profile_paths;
use ocg_core::crypto::{KeyCipher, StaticKeyCipher};
use ocg_core::models::{Account, AccountSetupStep, AccountType, AccountUpdate};
use ocg_core::provider::{
    BUILTIN_PROVIDERS, CUSTOM_PROVIDER_ID, ConnectionVerificationStatus, CredentialKind,
    OPENCODE_PROVIDER_ID, ZEN_FREE_ACCOUNT_ID,
};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener as StdTcpListener};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn temp_dir(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ocg-cli-test-{}-{}", label, uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn free_port() -> u16 {
    StdTcpListener::bind(("127.0.0.1", 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn test_cipher() -> Arc<dyn KeyCipher + Send + Sync> {
    Arc::new(StaticKeyCipher::new("cli-test-secret"))
}

#[test]
fn exposes_package_version() {
    assert_eq!(
        Cli::command().get_version(),
        Some(env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn serve_accepts_container_bind_address() {
    let cli = Cli::try_parse_from(["ocg-manager-cli", "serve", "--host", "0.0.0.0"]).unwrap();
    let Commands::Serve { host, .. } = cli.command else {
        panic!("expected serve command");
    };
    assert!(host.is_unspecified());
}

#[test]
fn cli_parses_key_and_status_subcommands() {
    let list = Cli::try_parse_from(["ocg-manager-cli", "key", "list"]).unwrap();
    assert!(matches!(
        list.command,
        Commands::Key {
            action: KeyAction::List
        }
    ));

    let add = Cli::try_parse_from([
        "ocg-manager-cli",
        "key",
        "add",
        "main",
        "sk-test",
        "--username",
        "user",
        "--password",
        "pass",
    ])
    .unwrap();
    let Commands::Key {
        action:
            KeyAction::Add {
                name,
                key,
                username,
                password,
            },
    } = add.command
    else {
        panic!("expected key add");
    };
    assert_eq!((name.as_str(), key.as_str()), ("main", "sk-test"));
    assert_eq!(username.as_deref(), Some("user"));
    assert_eq!(password.as_deref(), Some("pass"));

    assert!(matches!(
        Cli::try_parse_from(["ocg-manager-cli", "status"])
            .unwrap()
            .command,
        Commands::Status
    ));
}

#[test]
fn resolve_data_dir_prefers_explicit_path() {
    let explicit = PathBuf::from("/tmp/custom-ocg-data");
    assert_eq!(resolve_data_dir(Some(explicit.clone())), explicit);
    let fallback = resolve_data_dir(None);
    assert!(fallback.ends_with(".ocg-mgr-cli"));
}

fn assert_cipher_matches_static(
    cipher: &Arc<dyn KeyCipher + Send + Sync>,
    secret: &str,
    plaintext: &str,
) {
    let expected = StaticKeyCipher::new(secret);
    let ciphertext = cipher.encrypt(plaintext).unwrap();
    assert_eq!(expected.decrypt(&ciphertext).unwrap(), plaintext);
    let ciphertext = expected.encrypt(plaintext).unwrap();
    assert_eq!(cipher.decrypt(&ciphertext).unwrap(), plaintext);
}

#[test]
fn resolve_cipher_uses_explicit_env_then_file() {
    let dir = temp_dir("cipher");
    let explicit = resolve_cipher_with(
        &dir,
        Some("explicit-secret".into()),
        Some("env-secret".into()),
    )
    .unwrap();
    assert_cipher_matches_static(&explicit, "explicit-secret", "plain-explicit");

    let from_env = resolve_cipher_with(&dir, None, Some("env-secret".into())).unwrap();
    assert_cipher_matches_static(&from_env, "env-secret", "plain-env");

    let file_dir = temp_dir("cipher-file");
    let first = resolve_cipher_with(&file_dir, None, None).unwrap();
    let second = resolve_cipher_with(&file_dir, None, None).unwrap();
    let ciphertext = first.encrypt("roundtrip").unwrap();
    assert_eq!(second.decrypt(&ciphertext).unwrap(), "roundtrip");
    assert!(file_dir.join(".encryption-key").is_file());

    let _ = std::fs::remove_dir_all(dir);
    let _ = std::fs::remove_dir_all(file_dir);
}

#[test]
fn dashboard_dir_prefers_explicit_then_existing_packaged_dist() {
    let root = std::env::temp_dir().join(format!("ocg-cli-dashboard-{}", uuid::Uuid::new_v4()));
    let dist = root.join("dist");
    std::fs::create_dir_all(&dist).unwrap();
    let executable = root.join("ocg-manager-cli");
    let explicit = root.join("custom");

    assert_eq!(
        resolve_dashboard_dir(Some(explicit.clone()), Some(&executable)),
        Some(explicit)
    );
    assert_eq!(
        resolve_dashboard_dir(None, Some(&executable)),
        Some(dist.clone())
    );
    std::fs::remove_dir_all(&dist).unwrap();
    assert_eq!(resolve_dashboard_dir(None, Some(&executable)), None);

    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn key_lifecycle_and_status_cover_cli_account_commands() {
    let dir = temp_dir("keys");
    let cipher = test_cipher();

    key_command(dir.clone(), cipher.clone(), KeyAction::List)
        .await
        .unwrap();

    key_command(
        dir.clone(),
        cipher.clone(),
        KeyAction::Add {
            name: "main".into(),
            key: "sk-main".into(),
            username: Some("  alice  ".into()),
            password: Some("  secret  ".into()),
        },
    )
    .await
    .unwrap();

    key_command(
        dir.clone(),
        cipher.clone(),
        KeyAction::Add {
            name: "blank-creds".into(),
            key: "sk-blank".into(),
            username: Some("   ".into()),
            password: Some("".into()),
        },
    )
    .await
    .unwrap();

    let state = build_state(dir.clone(), cipher.clone()).unwrap();
    let accounts = state
        .db
        .lock()
        .list_accounts()
        .unwrap()
        .into_iter()
        .filter(|account| account.credential_kind == CredentialKind::ApiKey)
        .collect::<Vec<_>>();
    assert_eq!(accounts.len(), 2);
    let main = accounts
        .iter()
        .find(|account| account.name == "main")
        .unwrap()
        .clone();
    assert_eq!(main.username.as_deref(), Some("alice"));
    assert!(main.password_cipher.is_some());
    let blank = accounts
        .iter()
        .find(|account| account.name == "blank-creds")
        .unwrap()
        .clone();
    assert!(blank.username.is_none());
    assert!(blank.password_cipher.is_none());

    let mut pending = blank.clone();
    pending.id = uuid::Uuid::new_v4().to_string();
    pending.name = "pending".into();
    pending.key_cipher = String::new();
    pending.enabled = true;
    pending.account_type = AccountType::Managed;
    pending.setup_step = AccountSetupStep::GoogleAccount;
    state.db.lock().create_account(&pending).unwrap();

    key_command(dir.clone(), cipher.clone(), KeyAction::List)
        .await
        .unwrap();
    status_command(dir.clone(), cipher.clone()).await.unwrap();

    key_command(
        dir.clone(),
        cipher.clone(),
        KeyAction::Disable {
            id: main.id.clone(),
        },
    )
    .await
    .unwrap();
    let disabled = state.db.lock().get_account(&main.id).unwrap().unwrap();
    assert!(!disabled.enabled);

    key_command(
        dir.clone(),
        cipher.clone(),
        KeyAction::Enable {
            id: main.id.clone(),
        },
    )
    .await
    .unwrap();
    let enabled = state.db.lock().get_account(&main.id).unwrap().unwrap();
    assert!(enabled.enabled);

    assert!(toggle_account(&state, &pending.id, true).is_err());
    assert!(
        ping_keys(
            &state,
            Some(pending.id.as_str()),
            "deepseek-v4-flash",
            "ping",
            3,
        )
        .await
        .is_err()
    );

    let blank_profiles = browser_profile_paths(&dir, &blank.id).unwrap();
    assert!(blank_profiles.iter().all(|path| path.starts_with(&dir)));
    for profile in &blank_profiles {
        std::fs::create_dir_all(profile).unwrap();
        std::fs::write(profile.join("Cookies"), b"session").unwrap();
    }

    key_command(
        dir.clone(),
        cipher.clone(),
        KeyAction::Remove {
            id: blank.id.clone(),
        },
    )
    .await
    .unwrap();
    assert!(state.db.lock().get_account(&blank.id).unwrap().is_none());
    assert!(blank_profiles.iter().all(|path| !path.exists()));

    let pending_profile = browser_profile_paths(&dir, &pending.id).unwrap()[0].clone();
    std::fs::create_dir_all(&pending_profile).unwrap();
    std::fs::write(pending_profile.join("SingletonLock"), b"active").unwrap();
    let active_profile = key_command(
        dir.clone(),
        cipher.clone(),
        KeyAction::Remove {
            id: pending.id.clone(),
        },
    )
    .await;
    assert!(active_profile.is_err());
    assert!(state.db.lock().get_account(&pending.id).unwrap().is_some());
    assert!(pending_profile.exists());
    std::fs::remove_file(pending_profile.join("SingletonLock")).unwrap();
    key_command(
        dir.clone(),
        cipher.clone(),
        KeyAction::Remove {
            id: pending.id.clone(),
        },
    )
    .await
    .unwrap();

    let missing = key_command(
        dir.clone(),
        cipher.clone(),
        KeyAction::Remove {
            id: "missing-id".into(),
        },
    )
    .await;
    assert!(missing.is_err());

    let missing_toggle = toggle_account(&state, "missing-id", true);
    assert!(missing_toggle.is_err());

    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn cli_enable_rejects_unroutable_catalog_plans_without_mutation() {
    let dir = temp_dir("enablement-gate");
    let cipher = test_cipher();
    let state = build_state(dir.clone(), cipher.clone()).unwrap();
    let now = Utc::now();
    for plan in BUILTIN_PROVIDERS
        .iter()
        .copied()
        .filter(|plan| !plan.routable && plan.singleton_account_id.is_none())
    {
        let id = uuid::Uuid::new_v4().to_string();
        let draft = Account {
            id: id.clone(),
            provider_id: plan.provider_id.to_string(),

            credential_kind: plan.credential_kind,
            quota_scope: plan.quota_scope,
            name: format!("{}-cli", plan.provider_id),
            username: None,
            password_cipher: None,
            key_cipher: state.encrypt_key("draft-key").unwrap(),
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
        };
        state.db.lock().create_account(&draft).unwrap();
        let before = state.db.lock().get_account(&id).unwrap().unwrap();
        let error = toggle_account(&state, &id, true).expect_err("enable must fail closed");
        assert!(
            error.to_string().contains("not routable"),
            "{}: {error}",
            plan.display_name
        );
        let after = state.db.lock().get_account(&id).unwrap().unwrap();
        assert!(!after.enabled);
        assert_eq!(after.updated_at, before.updated_at);
        toggle_account(&state, &id, false).unwrap();
        key_command(
            dir.clone(),
            cipher.clone(),
            KeyAction::Enable { id: id.clone() },
        )
        .await
        .expect_err("CLI enable must fail closed");
        assert!(!state.db.lock().get_account(&id).unwrap().unwrap().enabled);
    }

    key_command(
        dir.clone(),
        cipher.clone(),
        KeyAction::Add {
            name: "go-main".into(),
            key: "sk-go".into(),
            username: None,
            password: None,
        },
    )
    .await
    .unwrap();
    let go = state
        .db
        .lock()
        .list_accounts()
        .unwrap()
        .into_iter()
        .find(|account| account.name == "go-main")
        .unwrap();
    assert!(!go.enabled);
    key_command(
        dir.clone(),
        cipher.clone(),
        KeyAction::Disable { id: go.id.clone() },
    )
    .await
    .unwrap();
    key_command(
        dir.clone(),
        cipher.clone(),
        KeyAction::Enable { id: go.id.clone() },
    )
    .await
    .unwrap();
    assert!(
        state
            .db
            .lock()
            .get_account(&go.id)
            .unwrap()
            .unwrap()
            .enabled
    );

    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn cli_key_operations_reject_the_provider_owned_zen_singleton() {
    let dir = temp_dir("zen-key-guard");
    let cipher = test_cipher();
    let state = build_state(dir.clone(), cipher.clone()).unwrap();
    let config_before = state.config();
    let zen_before = state
        .db
        .lock()
        .get_account(ZEN_FREE_ACCOUNT_ID)
        .unwrap()
        .unwrap();
    let profile = browser_profile_paths(&dir, ZEN_FREE_ACCOUNT_ID).unwrap()[0].clone();
    std::fs::create_dir_all(&profile).unwrap();
    std::fs::write(profile.join("Cookies"), b"keep").unwrap();

    for action in [
        KeyAction::Enable {
            id: ZEN_FREE_ACCOUNT_ID.into(),
        },
        KeyAction::Disable {
            id: ZEN_FREE_ACCOUNT_ID.into(),
        },
        KeyAction::Remove {
            id: ZEN_FREE_ACCOUNT_ID.into(),
        },
        KeyAction::Ping {
            id: Some(ZEN_FREE_ACCOUNT_ID.into()),
            model: "deepseek-v4-flash-free".into(),
            message: "ping".into(),
            max_tokens: 3,
        },
    ] {
        let error = key_command(dir.clone(), cipher.clone(), action)
            .await
            .expect_err("Zen must not be mutable through CLI key commands");
        assert!(error.to_string().contains("Zen Free"), "{error}");
    }

    let state_after = build_state(dir.clone(), cipher).unwrap();
    let zen_after = state_after
        .db
        .lock()
        .get_account(ZEN_FREE_ACCOUNT_ID)
        .unwrap()
        .unwrap();
    assert_eq!(zen_after.enabled, zen_before.enabled);
    assert_eq!(state_after.config().gateway_key, config_before.gateway_key);
    assert!(profile.join("Cookies").is_file());

    let _ = std::fs::remove_dir_all(dir);
}

async fn spawn_json_upstream(hits: Arc<AtomicUsize>) -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            hits.fetch_add(1, Ordering::SeqCst);
            let mut buf = vec![0_u8; 4096];
            let _ = stream.read(&mut buf).await;
            let body = br#"{"id":"ping","object":"chat.completion","choices":[{"index":0,"message":{"role":"assistant","content":"pong"},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":1}}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes()).await;
            let _ = stream.write_all(body).await;
        }
    });
    (addr, server)
}

#[tokio::test]
async fn ping_keys_hits_configured_upstream_and_handles_empty_targets() {
    let hits = Arc::new(AtomicUsize::new(0));
    let (addr, server) = spawn_json_upstream(hits.clone()).await;

    let dir = temp_dir("ping");
    let cipher = test_cipher();
    let state = build_state(dir.clone(), cipher.clone()).unwrap();
    let mut config = state.config();
    config.upstream_base_url = format!("http://{addr}");
    config.non_stream_timeout_secs = 5;
    state.set_config(config).unwrap();

    key_command(
        dir.clone(),
        cipher.clone(),
        KeyAction::Add {
            name: "pingable".into(),
            key: "sk-ping".into(),
            username: None,
            password: None,
        },
    )
    .await
    .unwrap();
    let account_id = state
        .db
        .lock()
        .list_accounts()
        .unwrap()
        .into_iter()
        .find(|account| account.name == "pingable")
        .unwrap()
        .id;

    ping_keys(&state, None, "deepseek-v4-flash", "ping", 3)
        .await
        .unwrap();
    ping_keys(
        &state,
        Some(account_id.as_str()),
        "deepseek-v4-flash",
        "ping",
        3,
    )
    .await
    .unwrap();
    assert!(hits.load(Ordering::SeqCst) >= 2);

    toggle_account(&state, &account_id, false).unwrap();
    ping_keys(&state, None, "deepseek-v4-flash", "ping", 3)
        .await
        .unwrap();

    let missing = ping_keys(&state, Some("nope"), "deepseek-v4-flash", "ping", 3).await;
    assert!(missing.is_err());

    let key_cipher_before = state
        .db
        .lock()
        .get_account(&account_id)
        .unwrap()
        .expect("pingable account")
        .key_cipher;
    let wrong_cipher: Arc<dyn KeyCipher + Send + Sync> =
        Arc::new(StaticKeyCipher::new("other-secret"));
    let open_error = match build_state(dir.clone(), wrong_cipher) {
        Ok(_) => panic!("wrong host cipher must fail closed on open, not during ping"),
        Err(error) => format!("{error:#}"),
    };
    assert!(open_error.contains("host cipher rejected"), "{open_error}");
    assert!(
        !open_error.contains("sk-ping"),
        "wrong-cipher open must not leak the plaintext key: {open_error}"
    );

    let recovered = build_state(dir.clone(), cipher).unwrap();
    let restored = recovered
        .db
        .lock()
        .get_account(&account_id)
        .unwrap()
        .expect("account still exists after rejected open");
    assert_eq!(restored.key_cipher, key_cipher_before);

    server.abort();
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn start_serve_binds_port_persists_override_and_stops_cleanly() {
    let dir = temp_dir("serve");
    let dash = dir.join("custom-dist");
    std::fs::create_dir_all(&dash).unwrap();
    let port = free_port();
    let cipher = test_cipher();

    let state = start_serve(
        dir.clone(),
        cipher.clone(),
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        Some(port),
        Some(dash.clone()),
    )
    .await
    .unwrap();

    assert_eq!(state.active_gateway_port(), port);
    assert_eq!(state.config().gateway_port, port);
    assert_eq!(state.dashboard_dir(), Some(dash.clone()));
    assert!(std::net::TcpStream::connect(SocketAddr::from(([127, 0, 0, 1], port))).is_ok());

    stop_serve(&state).await;
    assert!(state.gateway.lock().is_none());
    assert!(
        std::net::TcpStream::connect(SocketAddr::from(([127, 0, 0, 1], port))).is_err(),
        "gateway port should reject connections after graceful stop"
    );

    // Reopen and ensure the port override was persisted for the next start.
    let reopened = build_state(dir.clone(), cipher).unwrap();
    assert_eq!(reopened.config().gateway_port, port);

    let _ = std::fs::remove_dir_all(dir);
}

fn custom_draft(state: &ocg_core::state::CoreStateInner, id: &str) -> Account {
    let now = Utc::now();
    Account {
        id: id.to_string(),
        provider_id: CUSTOM_PROVIDER_ID.to_string(),

        credential_kind: CredentialKind::ApiKey,
        quota_scope: ocg_core::provider::QuotaScope::Key,
        name: id.to_string(),
        username: None,
        password_cipher: None,
        key_cipher: state.encrypt_key("custom-cli-key").unwrap(),
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

#[tokio::test]
async fn cli_key_mutations_share_control_plane_revision_in_process() {
    let dir = temp_dir("cli-cas-split");
    let dash = dir.join("dist");
    std::fs::create_dir_all(&dash).unwrap();
    let cipher = test_cipher();
    let port = free_port();
    let serving = start_serve(
        dir.clone(),
        cipher.clone(),
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        Some(port),
        Some(dash),
    )
    .await
    .unwrap();
    let revision_after_serve = serving.settings_revision();

    key_command(
        dir.clone(),
        cipher.clone(),
        KeyAction::Add {
            name: "go-cas".into(),
            key: "sk-cas".into(),
            username: None,
            password: None,
        },
    )
    .await
    .unwrap();
    key_command(dir.clone(), cipher.clone(), KeyAction::List)
        .await
        .unwrap();
    status_command(dir.clone(), cipher.clone()).await.unwrap();

    let go = serving
        .db
        .lock()
        .list_accounts()
        .unwrap()
        .into_iter()
        .find(|account| account.name == "go-cas")
        .expect("CLI key add must be visible to the live serve CoreState via SQLite");
    assert_eq!(go.provider_id, OPENCODE_PROVIDER_ID);
    assert!(!go.enabled);
    assert_eq!(go.setup_step, AccountSetupStep::Ready);
    assert_eq!(go.credential_kind, CredentialKind::ApiKey);
    assert_eq!(
        serving.settings_revision(),
        revision_after_serve,
        "out-of-process CLI key add/list/status cannot bump another CoreState CAS token"
    );

    key_command(
        dir.clone(),
        cipher.clone(),
        KeyAction::Disable { id: go.id.clone() },
    )
    .await
    .unwrap();
    key_command(
        dir.clone(),
        cipher.clone(),
        KeyAction::Enable { id: go.id.clone() },
    )
    .await
    .unwrap();
    assert_eq!(serving.settings_revision(), revision_after_serve);
    assert!(
        serving
            .db
            .lock()
            .get_account(&go.id)
            .unwrap()
            .unwrap()
            .enabled
    );

    let before_toggle = serving.settings_revision();
    toggle_account(&serving, &go.id, false).unwrap();
    assert!(
        !serving
            .db
            .lock()
            .get_account(&go.id)
            .unwrap()
            .unwrap()
            .enabled
    );
    assert_eq!(
        serving.settings_revision(),
        before_toggle + 1,
        "in-process CLI toggle_account must bump the shared settings_revision"
    );

    key_command(
        dir.clone(),
        cipher.clone(),
        KeyAction::Remove { id: go.id.clone() },
    )
    .await
    .unwrap();
    assert!(serving.db.lock().get_account(&go.id).unwrap().is_none());
    assert_eq!(
        serving.settings_revision(),
        before_toggle + 1,
        "out-of-process CLI key remove cannot bump the live serve CAS token"
    );

    stop_serve(&serving).await;
    assert!(serving.gateway.lock().is_none());
    assert_eq!(
        serving.settings_revision(),
        before_toggle + 1,
        "stop_serve must leave settings_revision untouched"
    );
    assert_eq!(serving.config().gateway_port, port);

    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn cli_enable_allows_pending_custom_without_verification() {
    let dir = temp_dir("cli-custom-enable");
    let cipher = test_cipher();
    let state = build_state(dir.clone(), cipher.clone()).unwrap();
    state
        .db
        .lock()
        .create_account(&custom_draft(&state, "cli-custom"))
        .unwrap();
    let before = state
        .db
        .lock()
        .account_verification_state("cli-custom")
        .unwrap()
        .unwrap();
    assert_eq!(before.status, ConnectionVerificationStatus::Pending);
    let revision = state.settings_revision();

    toggle_account(&state, "cli-custom", true)
        .expect("pending Custom may enable; verification is an optional tool");
    let enabled = state.db.lock().get_account("cli-custom").unwrap().unwrap();
    assert!(enabled.enabled);
    let after = state
        .db
        .lock()
        .account_verification_state("cli-custom")
        .unwrap()
        .unwrap();
    assert_eq!(after.status, ConnectionVerificationStatus::Pending);
    assert_eq!(state.settings_revision(), revision + 1);

    key_command(
        dir.clone(),
        cipher,
        KeyAction::Disable {
            id: "cli-custom".into(),
        },
    )
    .await
    .unwrap();
    assert!(
        !state
            .db
            .lock()
            .get_account("cli-custom")
            .unwrap()
            .unwrap()
            .enabled
    );

    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn cli_update_shaped_writes_skip_revision_unlike_dashboard() {
    let dir = temp_dir("cli-update-shape");
    let cipher = test_cipher();
    let state = build_state(dir.clone(), cipher).unwrap();
    state
        .db
        .lock()
        .create_account(&custom_draft(&state, "rename-me"))
        .unwrap();
    let revision = state.settings_revision();
    state
        .db
        .lock()
        .update_account(
            "rename-me",
            &AccountUpdate {
                name: Some("renamed".into()),
                ..AccountUpdate::default()
            },
            None,
            None,
        )
        .unwrap();
    assert_eq!(state.settings_revision(), revision);
    assert_eq!(
        state
            .db
            .lock()
            .get_account("rename-me")
            .unwrap()
            .unwrap()
            .name,
        "renamed"
    );

    let _ = std::fs::remove_dir_all(dir);
}

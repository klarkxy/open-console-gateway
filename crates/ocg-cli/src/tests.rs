use super::{
    BackupCommand, Cli, Commands, KeyAction, attach_api_help, build_state,
    default_generation_data_dir, dispatch_to, key_command, key_command_targeting, ping_owned_serve,
    resolve_cipher_with, resolve_dashboard_dir, resolve_data_dir, start_serve, status_command,
    stop_serve, toggle_account,
};
use crate::api_cmd::{self, ApiRequest, BodySink};
use crate::endpoint;
use crate::listener_owner::{self, Owner};
use chrono::Utc;
use clap::{CommandFactory, Parser};
use ocg_core::browser::browser_profile_paths;
use ocg_core::crypto::{KeyCipher, StaticKeyCipher};
use ocg_core::models::{
    Account, AccountCustomConfigInput, AccountModelCapabilityInput, AccountSetupStep, AccountType,
    AccountUpdate,
};
use ocg_core::provider::{
    BUILTIN_PROVIDERS, CUSTOM_PROVIDER_ID, ConnectionVerificationStatus, CredentialKind,
    OPENCODE_PROVIDER_ID, UpstreamProtocolKind, ZEN_FREE_ACCOUNT_ID,
};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener as StdTcpListener};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex;
use std::time::{Duration, Instant};
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

#[tokio::test]
async fn offline_helpers_cannot_open_data_while_the_host_owns_it() {
    let dir = temp_dir("offline-owner");
    let owner = crate::serve_lock::ServeLock::acquire(&dir).unwrap();
    assert!(
        key_command(dir.clone(), test_cipher(), KeyAction::List)
            .await
            .is_err()
    );
    assert!(
        status_command(dir.clone(), test_cipher(), false)
            .await
            .is_err()
    );
    assert!(!dir.join("data.sqlite").exists());
    drop(owner);
    assert!(
        status_command(dir.clone(), test_cipher(), false)
            .await
            .is_ok()
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn exposes_package_version() {
    assert_eq!(
        Cli::command().get_version(),
        Some(env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn serve_accepts_pinned_host_dir() {
    let cli = Cli::try_parse_from(["ocg", "serve", "--cpa-host-dir", "runtime-build"]).unwrap();
    let Commands::Serve { cpa_host_dir, .. } = cli.command else {
        panic!("expected serve command");
    };
    assert_eq!(
        cpa_host_dir.as_deref(),
        Some(std::path::Path::new("runtime-build"))
    );
}

#[test]
fn serve_accepts_container_bind_address() {
    let cli = Cli::try_parse_from(["ocg", "serve", "--host", "0.0.0.0"]).unwrap();
    let Commands::Serve { host, .. } = cli.command else {
        panic!("expected serve command");
    };
    assert!(host.is_unspecified());
}

#[test]
fn cli_parses_key_and_status_subcommands() {
    let list = Cli::try_parse_from(["ocg", "key", "list"]).unwrap();
    assert!(matches!(
        list.command,
        Commands::Key {
            action: KeyAction::List
        }
    ));

    let add = Cli::try_parse_from([
        "ocg",
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
        Cli::try_parse_from(["ocg", "status"]).unwrap().command,
        Commands::Status { show_key: false }
    ));
    assert!(matches!(
        Cli::try_parse_from(["ocg", "status", "--show-key"])
            .unwrap()
            .command,
        Commands::Status { show_key: true }
    ));
}

#[test]
fn command_name_is_ocg() {
    let mut command = Cli::command();
    attach_api_help(&mut command);
    assert_eq!(command.get_name(), "ocg");
    assert_eq!(command.get_bin_name(), Some("ocg"));
    let mut help = Vec::new();
    command.write_long_help(&mut help).unwrap();
    let text = String::from_utf8(help).unwrap();
    assert!(
        text.contains("Usage: ocg ") || text.contains("Usage: ocg\n"),
        "{text}"
    );
    assert!(text.contains("ocg serve --port 9042"), "{text}");
    assert!(text.contains("cargo build -p ocg-cli --locked"), "{text}");
    assert!(text.contains("~/.ocg3"), "{text}");
    assert!(text.contains("not opened"), "{text}");
    assert!(!text.contains("ocg-manager-cli"), "{text}");
    for name in ["serve", "api", "schema", "backup", "key", "status"] {
        assert!(command.find_subcommand(name).is_some(), "{name}");
    }
}

#[tokio::test]
async fn default_generation_does_not_adopt_previous_data_dir() {
    let home = temp_dir("generation-home");
    let previous = home.join(".ocg-mgr-cli");
    std::fs::create_dir_all(previous.join("cpa/auth")).unwrap();
    let marker = previous.join("cpa/auth/token.json");
    let marker_bytes = b"previous-generation-sentinel";
    std::fs::write(&marker, marker_bytes).unwrap();

    let explicit = home.join("explicit-root");
    let parsed =
        Cli::try_parse_from(["ocg", "--data-dir", explicit.to_str().unwrap(), "status"]).unwrap();
    assert_eq!(resolve_data_dir(parsed.data_dir), explicit);
    status_command(explicit.clone(), test_cipher(), false)
        .await
        .unwrap();
    assert!(explicit.join("data.sqlite").is_file());
    assert!(!home.join(".ocg3").exists());
    assert_eq!(std::fs::read(&marker).unwrap(), marker_bytes);
    assert!(!previous.join("data.sqlite").exists());

    let omitted = Cli::try_parse_from(["ocg", "status"]).unwrap();
    assert!(omitted.data_dir.is_none());
    let chosen = default_generation_data_dir(&home);
    assert_eq!(chosen, home.join(".ocg3"));
    assert_ne!(chosen, previous);
    status_command(chosen.clone(), test_cipher(), false)
        .await
        .unwrap();
    assert!(chosen.join("data.sqlite").is_file());
    assert!(!previous.join("data.sqlite").exists());
    assert_eq!(std::fs::read(&marker).unwrap(), marker_bytes);
    assert!(
        !std::fs::read(explicit.join("data.sqlite"))
            .unwrap()
            .is_empty()
    );
    let _ = std::fs::remove_dir_all(home);
}

#[tokio::test]
async fn api_request_sends_ocg_user_agent() {
    let (addr, mock) = spawn_control_mock(vec![http_json(200, "OK", r#"{"ok":true}"#)]).await;
    let (status, body) = host_request(
        &format!("http://{addr}"),
        "GET",
        "/dashboard/api/v4/contract",
        None,
        false,
        None,
    )
    .await
    .unwrap();
    assert_eq!(status, 200);
    assert_eq!(body, br#"{"ok":true}"#);
    let raw = mock.requests.lock().expect("requests").remove(0);
    let header = raw
        .lines()
        .find(|line| line.to_ascii_lowercase().starts_with("user-agent:"))
        .unwrap_or_else(|| panic!("no user-agent in {raw}"));
    assert_eq!(header.split_once(':').unwrap().1.trim(), "ocg");
    assert!(
        raw.lines()
            .next()
            .unwrap()
            .starts_with("GET /dashboard/api/v4/contract ")
    );
    mock.server.abort();
}

#[test]
fn minimal_cli_does_not_register_application_hosts() {
    let dir = temp_dir("dsh-host-capability");
    let state = build_state(dir.clone(), test_cipher()).unwrap();
    assert!(state.dsh_application_host().is_none());
    assert!(state.byok_application_host().is_none());
    let _ = std::fs::remove_dir_all(dir);
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
    let seeded = file_dir.join(".encryption-key");
    std::fs::write(&seeded, "file-secret").unwrap();
    let from_file = resolve_cipher_with(&file_dir, None, None).unwrap();
    assert_cipher_matches_static(&from_file, "file-secret", "plain-file");

    let bare_dir = temp_dir("cipher-bare");
    let first = resolve_cipher_with(&bare_dir, None, None).unwrap();
    let second = resolve_cipher_with(&bare_dir, None, None).unwrap();
    let ciphertext = first.encrypt("roundtrip").unwrap();
    assert_eq!(second.decrypt(&ciphertext).unwrap(), "roundtrip");
    if cfg!(windows) {
        assert!(!bare_dir.join(".encryption-key").exists());
    } else {
        assert!(bare_dir.join(".encryption-key").is_file());
    }
    let _ = std::fs::remove_dir_all(dir);
    let _ = std::fs::remove_dir_all(file_dir);
    let _ = std::fs::remove_dir_all(bare_dir);
}

#[test]
fn dashboard_dir_prefers_explicit_then_existing_packaged_dist() {
    let root = std::env::temp_dir().join(format!("ocg-cli-dashboard-{}", uuid::Uuid::new_v4()));
    let dist = root.join("dist");
    std::fs::create_dir_all(&dist).unwrap();
    let executable = root.join("ocg");
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

    let offline_add = key_command(
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
    .unwrap_err();
    assert!(offline_add.to_string().contains("serve"), "{offline_add:#}");
    assert!(account_named(&dir, cipher.clone(), "main").is_none());

    let state = build_state(dir.clone(), cipher.clone()).unwrap();
    ocg_core::account_control::create_go_api_key(
        &state,
        "main".into(),
        "sk-main".into(),
        Some("  alice  ".into()),
        Some("  secret  ".into()),
    )
    .unwrap();
    ocg_core::account_control::create_go_api_key(
        &state,
        "blank-creds".into(),
        "sk-blank".into(),
        Some("   ".into()),
        Some("".into()),
    )
    .unwrap();
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
    status_command(dir.clone(), cipher.clone(), false)
        .await
        .unwrap();

    let refused_disable = key_command(
        dir.clone(),
        cipher.clone(),
        KeyAction::Disable {
            id: main.id.clone(),
        },
    )
    .await
    .unwrap_err();
    assert!(
        refused_disable.to_string().contains("serve"),
        "{refused_disable:#}"
    );
    assert!(
        state
            .db
            .lock()
            .get_account(&main.id)
            .unwrap()
            .unwrap()
            .enabled
    );
    toggle_account(&state, &main.id, false).unwrap();
    let disabled = state.db.lock().get_account(&main.id).unwrap().unwrap();
    assert!(!disabled.enabled);

    let refused_enable = key_command(
        dir.clone(),
        cipher.clone(),
        KeyAction::Enable {
            id: main.id.clone(),
        },
    )
    .await
    .unwrap_err();
    assert!(
        refused_enable.to_string().contains("serve"),
        "{refused_enable:#}"
    );
    assert!(
        !state
            .db
            .lock()
            .get_account(&main.id)
            .unwrap()
            .unwrap()
            .enabled
    );
    toggle_account(&state, &main.id, true).unwrap();
    let enabled = state.db.lock().get_account(&main.id).unwrap().unwrap();
    assert!(enabled.enabled);

    assert!(toggle_account(&state, &pending.id, true).is_err());
    let pending_before = state.db.lock().get_account(&pending.id).unwrap().unwrap();
    let ping_error = key_command(
        dir.clone(),
        cipher.clone(),
        KeyAction::Ping {
            id: pending.id.clone(),
            model: "deepseek-v4-flash".into(),
            message: Some("ping".into()),
            max_tokens: Some(3),
        },
    )
    .await
    .unwrap_err();
    assert!(
        ping_error.to_string().contains("requires a running serve"),
        "{ping_error}"
    );
    let pending_after = state.db.lock().get_account(&pending.id).unwrap().unwrap();
    assert_eq!(pending_after.setup_step, pending_before.setup_step);
    assert_eq!(pending_after.key_cipher, pending_before.key_cipher);

    let blank_profiles = browser_profile_paths(&dir, &blank.id).unwrap();
    assert!(blank_profiles.iter().all(|path| path.starts_with(&dir)));
    for profile in &blank_profiles {
        std::fs::create_dir_all(profile).unwrap();
        std::fs::write(profile.join("Cookies"), b"session").unwrap();
    }

    let refused_remove = key_command(
        dir.clone(),
        cipher.clone(),
        KeyAction::Remove {
            id: blank.id.clone(),
        },
    )
    .await
    .unwrap_err();
    assert!(
        refused_remove.to_string().contains("serve"),
        "{refused_remove:#}"
    );
    assert!(state.db.lock().get_account(&blank.id).unwrap().is_some());
    ocg_core::account_control::delete_account(&state, &blank.id, None)
        .await
        .unwrap();
    assert!(state.db.lock().get_account(&blank.id).unwrap().is_none());
    assert!(blank_profiles.iter().all(|path| !path.exists()));

    let pending_profile = browser_profile_paths(&dir, &pending.id).unwrap()[0].clone();
    std::fs::create_dir_all(&pending_profile).unwrap();
    std::fs::write(pending_profile.join("SingletonLock"), b"active").unwrap();
    let active_profile = ocg_core::account_control::delete_account(&state, &pending.id, None).await;
    assert!(active_profile.is_err());
    assert!(state.db.lock().get_account(&pending.id).unwrap().is_some());
    assert!(pending_profile.exists());
    std::fs::remove_file(pending_profile.join("SingletonLock")).unwrap();
    ocg_core::account_control::delete_account(&state, &pending.id, None)
        .await
        .unwrap();

    let missing = ocg_core::account_control::delete_account(&state, "missing-id", None).await;
    assert!(missing.is_err());
    let missing_cli = key_command(
        dir.clone(),
        cipher.clone(),
        KeyAction::Remove {
            id: "missing-id".into(),
        },
    )
    .await
    .unwrap_err();
    assert!(missing_cli.to_string().contains("serve"), "{missing_cli:#}");

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

    ocg_core::account_control::create_go_api_key(
        &state,
        "go-main".into(),
        "sk-go".into(),
        None,
        None,
    )
    .unwrap();
    let go = state
        .db
        .lock()
        .list_accounts()
        .unwrap()
        .into_iter()
        .find(|account| account.name == "go-main")
        .unwrap();
    assert!(go.enabled);
    toggle_account(&state, &go.id, false).unwrap();
    toggle_account(&state, &go.id, true).unwrap();
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
    ] {
        let error = key_command(dir.clone(), cipher.clone(), action)
            .await
            .expect_err("offline key writes must not open a second database");
        assert!(error.to_string().contains("serve"), "{error}");
        assert!(!error.to_string().contains("Zen Free"), "{error}");
    }
    let ping_error = key_command(
        dir.clone(),
        cipher.clone(),
        KeyAction::Ping {
            id: ZEN_FREE_ACCOUNT_ID.into(),
            model: "deepseek-v4-flash-free".into(),
            message: Some("ping".into()),
            max_tokens: Some(3),
        },
    )
    .await
    .expect_err("stopped ping must not open the database to classify Zen");
    assert!(
        ping_error.to_string().contains("requires a running serve"),
        "{ping_error}"
    );
    assert!(!ping_error.to_string().contains("Zen Free"), "{ping_error}");

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

struct ControlMock {
    requests: Arc<Mutex<Vec<String>>>,
    server: tokio::task::JoinHandle<()>,
}

async fn spawn_control_mock(responses: Vec<String>) -> (SocketAddr, ControlMock) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&requests);
    let queued = Arc::new(Mutex::new(responses));
    let server = tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            let mut collected = Vec::new();
            let mut buf = vec![0_u8; 4096];
            loop {
                let Ok(read) = stream.read(&mut buf).await else {
                    break;
                };
                if read == 0 {
                    break;
                }
                collected.extend_from_slice(&buf[..read]);
                if request_complete(&collected) {
                    break;
                }
                if collected.len() > 64 * 1024 {
                    break;
                }
            }
            recorded
                .lock()
                .expect("request log")
                .push(String::from_utf8_lossy(&collected).into_owned());
            let response = {
                let mut queue = queued.lock().expect("response queue");
                if queue.is_empty() {
                    http_json(
                        500,
                        "ERR",
                        r#"{"code":"empty","message":"no queued response"}"#,
                    )
                } else {
                    queue.remove(0)
                }
            };
            let _ = stream.write_all(response.as_bytes()).await;
        }
    });
    (addr, ControlMock { requests, server })
}

fn request_complete(bytes: &[u8]) -> bool {
    let Some(split) = bytes.windows(4).position(|window| window == b"\r\n\r\n") else {
        return false;
    };
    let header = String::from_utf8_lossy(&bytes[..split]);
    let length = header.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.eq_ignore_ascii_case("content-length")
            .then(|| value.trim().parse::<usize>().ok())
            .flatten()
    });
    match length {
        Some(length) => bytes.len() >= split + 4 + length,
        None => true,
    }
}

fn http_json(status: u16, reason: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

fn request_target(raw: &str) -> (String, String) {
    let (head, body) = raw.split_once("\r\n\r\n").unwrap_or((raw, ""));
    let start = head.lines().next().unwrap_or("");
    (start.to_string(), body.to_string())
}

fn assert_no_sqlite_writer(dir: &Path) {
    assert!(!dir.join("data.sqlite").exists(), "ping opened SQLite");
    assert!(!dir.join(".encryption-key").exists());
}

#[tokio::test]
async fn key_ping_stopped_refuses_before_the_database_lock() {
    let dir = temp_dir("ping-stopped");
    let owner = crate::serve_lock::ServeLock::acquire(&dir).unwrap();
    let (addr, mock) = spawn_control_mock(Vec::new()).await;
    let error = dispatch_to(
        Cli {
            data_dir: Some(dir.clone()),
            encryption_key: None,
            endpoint: format!("http://{addr}"),
            endpoint_explicit: true,
            command: Commands::Key {
                action: KeyAction::Ping {
                    id: "acct-exact-1".into(),
                    model: "deepseek-v4-flash".into(),
                    message: Some("custom prompt".into()),
                    max_tokens: Some(11),
                },
            },
        },
        &mut Vec::new(),
    )
    .await
    .expect_err("stopped ping must refuse");
    let text = error.to_string();
    assert!(text.contains("requires a running serve"), "{text}");
    assert!(
        !text.contains("another ocg serve"),
        "ping must not take the serve lock: {text}"
    );
    assert!(mock.requests.lock().expect("requests").is_empty());
    assert_no_sqlite_writer(&dir);
    assert!(
        crate::serve_lock::ServeLock::acquire(&dir).is_err(),
        "ping must leave the existing serve lock held"
    );
    drop(owner);
    mock.server.abort();
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn key_ping_running_posts_exact_account_model_test() {
    let dir = temp_dir("ping-running");
    let owner = crate::serve_lock::ServeLock::acquire(&dir).unwrap();
    let ok = http_json(
        200,
        "OK",
        r#"{"accountId":"acct-exact-1","modelId":"deepseek-v4-flash","protocol":"chat_completions","success":true,"httpStatus":200,"durationMs":4,"error":null,"hop":"http://10.9.9.9:8317","key":"sk-should-not-print"}"#,
    );
    let custom = http_json(
        200,
        "OK",
        r#"{"accountId":"acct-exact-1","modelId":"deepseek-v4-flash","protocol":"messages","success":false,"httpStatus":429,"durationMs":15,"error":"limited https://hop.internal/private sk-live-secret","hop":"http://10.9.9.9:8317","key":"sk-should-not-print"}"#,
    );
    let (addr, mock) = spawn_control_mock(vec![ok, custom]).await;
    listener_owner::write(&dir, &format!("http://{addr}")).unwrap();

    let absent = ping_owned_serve(
        dir.clone(),
        "acct-exact-1".into(),
        "deepseek-v4-flash".into(),
        None,
        None,
    )
    .await
    .expect("absent message uses the control default");
    assert!(absent.contains("[OK] acct-exact-1"), "{absent}");
    assert!(absent.contains("status=200"), "{absent}");
    assert!(!absent.contains("sk-should-not-print"), "{absent}");
    assert!(!absent.contains("10.9.9.9"), "{absent}");

    let observed = ping_owned_serve(
        dir.clone(),
        "acct-exact-1".into(),
        "deepseek-v4-flash".into(),
        Some("custom prompt".into()),
        Some(11),
    )
    .await
    .expect("custom prompt is a control call");
    assert!(observed.contains("[FAIL] acct-exact-1"), "{observed}");
    assert!(observed.contains("status=429"), "{observed}");
    assert!(observed.contains("protocol=messages"), "{observed}");
    assert!(observed.contains("[redacted]"), "{observed}");
    assert!(!observed.contains("sk-live-secret"), "{observed}");
    assert!(!observed.contains("hop.internal"), "{observed}");
    assert!(!observed.contains("sk-should-not-print"), "{observed}");
    assert!(!observed.contains("10.9.9.9"), "{observed}");

    let requests = mock.requests.lock().expect("requests").clone();
    assert_eq!(requests.len(), 2, "{requests:?}");
    let (first_line, first_body) = request_target(&requests[0]);
    assert_eq!(
        first_line,
        "POST /dashboard/api/v4/accounts/acct-exact-1/model-tests HTTP/1.1"
    );
    let first: serde_json::Value = serde_json::from_str(&first_body).unwrap();
    assert_eq!(first, serde_json::json!({"modelId": "deepseek-v4-flash"}));
    assert!(!requests[0].contains("Authorization"));
    assert!(!requests[0].contains("sk-"));
    let (second_line, second_body) = request_target(&requests[1]);
    assert_eq!(
        second_line,
        "POST /dashboard/api/v4/accounts/acct-exact-1/model-tests HTTP/1.1"
    );
    let second: serde_json::Value = serde_json::from_str(&second_body).unwrap();
    assert_eq!(
        second,
        serde_json::json!({
            "maxTokens": 11,
            "message": "custom prompt",
            "modelId": "deepseek-v4-flash"
        })
    );
    assert_no_sqlite_writer(&dir);
    assert!(crate::serve_lock::ServeLock::acquire(&dir).is_err());
    drop(owner);
    mock.server.abort();
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn key_ping_control_error_is_redacted_and_zero_tokens_does_not_send() {
    let dir = temp_dir("ping-error");
    let (addr, mock) = spawn_control_mock(vec![http_json(
        401,
        "Unauthorized",
        r#"{"code":"unavailable","message":"denied https://hop.internal/private sk-live-secret","hop":"http://10.9.9.9:8317"}"#,
    )])
    .await;
    listener_owner::write(&dir, &format!("http://{addr}")).unwrap();
    let error = ping_owned_serve(
        dir.clone(),
        "acct-exact-1".into(),
        "deepseek-v4-flash".into(),
        Some("custom prompt".into()),
        Some(11),
    )
    .await
    .expect_err("control failure stays visible");
    let text = error.to_string();
    assert!(text.contains("401"), "{text}");
    assert!(!text.contains("sk-live-secret"), "{text}");
    assert!(!text.contains("hop.internal"), "{text}");
    assert!(!text.contains("10.9.9.9"), "{text}");
    assert_eq!(mock.requests.lock().expect("requests").len(), 1);

    let blocked = ping_owned_serve(
        dir.clone(),
        "acct-exact-1".into(),
        "deepseek-v4-flash".into(),
        Some("custom prompt".into()),
        Some(0),
    )
    .await
    .expect_err("zero maxTokens is rejected");
    assert!(
        blocked.to_string().contains("maxTokens must be positive"),
        "{blocked}"
    );
    assert_eq!(mock.requests.lock().expect("requests").len(), 1);
    assert_no_sqlite_writer(&dir);
    mock.server.abort();
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

#[tokio::test]
async fn start_serve_schedules_cpa_restore_without_blocking_gateway() {
    let dir = temp_dir("serve-cpa-restore");
    let dash = dir.join("custom-dist");
    std::fs::create_dir_all(&dash).unwrap();
    std::fs::create_dir_all(dir.join("cpa")).unwrap();
    std::fs::write(
        dir.join("cpa").join("managed.json"),
        format!(
            "{{\"currentVersion\":\"7.2.147\",\"assetSha256\":\"{}\",\"port\":8317,\"desiredRunning\":true}}",
            "a".repeat(64)
        ),
    )
    .unwrap();
    let port = free_port();
    let started = Instant::now();
    let state = start_serve(
        dir.clone(),
        test_cipher(),
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        Some(port),
        Some(dash),
    )
    .await
    .unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "CPA restore must not block native CLI gateway startup"
    );
    assert!(std::net::TcpStream::connect(SocketAddr::from(([127, 0, 0, 1], port))).is_ok());
    stop_serve(&state).await;
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

fn create_pending_custom_fixture(state: &ocg_core::state::CoreStateInner, id: &str) {
    // Pending verification is valid; a Custom credential without its HTTP
    // destination and declared catalog is not a valid reopenable fixture.
    state
        .db
        .lock()
        .create_account_with_contract(
            &custom_draft(state, id),
            Some(&AccountCustomConfigInput {
                endpoint_url: "https://custom-cli.example/v1/chat/completions".into(),
                upstream_protocol: UpstreamProtocolKind::ChatCompletions,
            }),
            &[AccountModelCapabilityInput {
                public_model: "cli-custom-model".into(),
                upstream_model: "vendor/cli-custom-model".into(),
                protocol: UpstreamProtocolKind::ChatCompletions,
                source: None,
            }],
        )
        .unwrap();
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

    let zen_error = key_command(
        dir.clone(),
        cipher.clone(),
        KeyAction::Enable {
            id: ZEN_FREE_ACCOUNT_ID.into(),
        },
    )
    .await
    .expect_err("Zen stays rejected on the live host");
    assert!(zen_error.to_string().contains("Zen Free"), "{zen_error}");
    assert_eq!(serving.settings_revision(), revision_after_serve);

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
    assert_eq!(serving.settings_revision(), revision_after_serve + 1);
    let list_while_serving = key_command(dir.clone(), cipher.clone(), KeyAction::List).await;
    assert!(
        list_while_serving.is_err(),
        "key list must fail while serve holds the data directory: {list_while_serving:?}"
    );
    let status_while_serving = status_command(dir.clone(), cipher.clone(), false).await;
    assert!(
        status_while_serving.is_err(),
        "status must fail while serve holds the data directory: {status_while_serving:?}"
    );
    assert_eq!(serving.settings_revision(), revision_after_serve + 1);

    let go = serving
        .db
        .lock()
        .list_accounts()
        .unwrap()
        .into_iter()
        .find(|account| account.name == "go-cas")
        .expect("HTTP key add must be visible in the serving database");
    assert_eq!(go.provider_id, OPENCODE_PROVIDER_ID);
    assert!(go.enabled);
    assert_eq!(go.setup_step, AccountSetupStep::Ready);
    assert_eq!(go.credential_kind, CredentialKind::ApiKey);

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
    assert_eq!(serving.settings_revision(), revision_after_serve + 3);
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
    let core = serving.core();
    toggle_account(&core, &go.id, false).unwrap();
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
        before_toggle + 2,
        "HTTP key remove bumps the live serve revision"
    );

    stop_serve(&serving).await;
    assert!(serving.gateway.lock().is_none());
    assert_eq!(
        serving.settings_revision(),
        before_toggle + 2,
        "stop_serve must leave settings_revision untouched"
    );
    assert!(matches!(listener_owner::inspect(&dir), Owner::Absent));
    assert_eq!(serving.config().gateway_port, port);

    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn cli_enable_allows_pending_custom_without_verification() {
    let dir = temp_dir("cli-custom-enable");
    let cipher = test_cipher();
    let state = build_state(dir.clone(), cipher.clone()).unwrap();
    create_pending_custom_fixture(&state, "cli-custom");
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

    let refused = key_command(
        dir.clone(),
        cipher,
        KeyAction::Disable {
            id: "cli-custom".into(),
        },
    )
    .await
    .unwrap_err();
    assert!(refused.to_string().contains("serve"), "{refused:#}");
    assert!(
        state
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
    create_pending_custom_fixture(&state, "rename-me");
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

fn dead_pid() -> u32 {
    let mut command = if cfg!(windows) {
        std::process::Command::new("cmd")
    } else {
        std::process::Command::new("true")
    };
    if cfg!(windows) {
        command.args(["/C", "exit", "0"]);
    }
    let mut child = command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let pid = child.id();
    child.wait().unwrap();
    pid
}

fn dashboard_cookie(path: &Path) -> Option<String> {
    let value: serde_json::Value = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    value.get("cookies")?.as_array()?.iter().find_map(|cookie| {
        if cookie.get("name")?.as_str()? == "ocg_dashboard_session" {
            let text = cookie.get("value")?.as_str()?;
            (!text.is_empty()).then(|| text.to_string())
        } else {
            None
        }
    })
}

fn account_named(
    dir: &Path,
    cipher: Arc<dyn KeyCipher + Send + Sync>,
    name: &str,
) -> Option<Account> {
    let state = build_state(dir.to_path_buf(), cipher).unwrap();
    state
        .db
        .lock()
        .list_accounts()
        .unwrap()
        .into_iter()
        .find(|account| account.name == name)
}

async fn host_request(
    endpoint: &str,
    method: &str,
    path: &str,
    body: Option<&[u8]>,
    cas_current: bool,
    session_file: Option<&Path>,
) -> Result<(u16, Vec<u8>), api_cmd::ApiFailure> {
    let mut sink = Vec::new();
    let success = api_cmd::execute(
        ApiRequest {
            endpoint: endpoint.to_string(),
            method: method.to_string(),
            path: path.to_string(),
            body: body.map(|bytes| bytes.to_vec()),
            cas_current,
            bearer: None,
            session_file: session_file.map(Path::to_path_buf),
        },
        &mut sink as &mut dyn BodySink,
    )
    .await?;
    Ok((success.status, sink))
}

#[test]
fn help_and_endpoint_flags_match_the_live_contract() {
    let mut command = Cli::command();
    attach_api_help(&mut command);
    assert!(command.find_subcommand("api").is_some());
    assert!(command.find_subcommand("serve").is_some());
    assert!(command.find_subcommand("schema").is_some());
    assert!(command.find_subcommand("key").is_some());
    assert!(command.find_subcommand("status").is_some());

    let explicit = Cli::command()
        .try_get_matches_from(["ocg", "--endpoint", "http://127.0.0.1:9", "status"])
        .unwrap();
    assert_eq!(
        explicit.value_source("endpoint"),
        Some(clap::parser::ValueSource::CommandLine)
    );
    let defaulted = Cli::command()
        .try_get_matches_from(["ocg", "status"])
        .unwrap();
    assert_ne!(
        defaulted.value_source("endpoint"),
        Some(clap::parser::ValueSource::CommandLine)
    );

    let parsed = Cli::try_parse_from(["ocg", "schema", "v4"]).unwrap();
    assert_eq!(parsed.endpoint, endpoint::DEFAULT_ORIGIN);

    let before = Cli::try_parse_from([
        "ocg",
        "--endpoint",
        "http://127.0.0.1:19042",
        "api",
        "GET",
        "/dashboard/api/v4/contract",
    ])
    .unwrap();
    assert_eq!(before.endpoint, "http://127.0.0.1:19042");

    let on_api = Cli::try_parse_from([
        "ocg",
        "api",
        "--endpoint",
        "http://127.0.0.1:19043",
        "GET",
        "/dashboard/api/v4/contract",
    ])
    .unwrap();
    assert_eq!(on_api.endpoint, "http://127.0.0.1:19043");
}

#[tokio::test]
async fn api_and_schema_do_not_create_a_data_directory() {
    let dir = temp_dir("api-no-db");
    std::fs::remove_dir_all(&dir).unwrap();
    let schema =
        Cli::try_parse_from(["ocg", "--data-dir", dir.to_str().unwrap(), "schema", "v4"]).unwrap();
    let mut sink = Vec::new();
    dispatch_to(schema, &mut sink).await.unwrap();
    assert!(String::from_utf8(sink).unwrap().contains("DashboardApiV4"));
    assert!(!dir.exists());

    let api = Cli::try_parse_from([
        "ocg",
        "--data-dir",
        dir.to_str().unwrap(),
        "--endpoint",
        "http://127.0.0.1:1",
        "api",
        "GET",
        "/dashboard/api/v4/contract",
    ])
    .unwrap();
    let error = dispatch_to(api, &mut Vec::<u8>::new()).await.unwrap_err();
    let failure = error
        .downcast_ref::<api_cmd::ApiFailure>()
        .unwrap_or_else(|| panic!("{error:#}"));
    assert_eq!(failure.code, "transport");
    assert!(failure.status.is_none());
    assert!(!dir.exists());
}

#[tokio::test]
async fn loopback_serve_answers_v4_and_a_dead_listener_does_not_own_writes() {
    let dir = temp_dir("loopback-api");
    let cipher = test_cipher();
    let port = free_port();
    let serving = start_serve(
        dir.clone(),
        cipher.clone(),
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        Some(port),
        None,
    )
    .await
    .unwrap();
    let Owner::Live { endpoint } = listener_owner::inspect(&dir) else {
        panic!("serve must publish a live listener marker");
    };
    assert_eq!(endpoint, format!("http://127.0.0.1:{port}"));
    #[cfg(feature = "dsh-local-host")]
    {
        assert!(serving.dsh_application_host().is_some());
        assert!(serving.byok_application_host().is_some());
    }
    #[cfg(not(feature = "dsh-local-host"))]
    {
        assert!(serving.dsh_application_host().is_none());
        assert!(serving.byok_application_host().is_none());
    }
    let capabilities = serving.browser.capabilities().await;
    if crate::native_browser::chromium_executable_available() {
        assert_eq!(capabilities.mode, ocg_core::browser::BrowserMode::Native);
    } else {
        assert_ne!(capabilities.mode, ocg_core::browser::BrowserMode::Native);
        if capabilities.mode == ocg_core::browser::BrowserMode::Unsupported {
            assert!(
                capabilities
                    .reason
                    .as_deref()
                    .is_some_and(|reason| !reason.is_empty())
            );
        }
    }

    let (status, body) = host_request(
        &endpoint,
        "GET",
        "/dashboard/api/v4/contract",
        None,
        false,
        None,
    )
    .await
    .unwrap();
    assert_eq!(status, 200);
    let contract: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let revision = serving.settings_revision();
    assert_eq!(contract["revision"].as_u64().unwrap(), revision);
    assert_eq!(
        contract["processGeneration"].as_u64().unwrap(),
        serving.process_generation()
    );

    let session = dir.join("loopback-session.json");
    let (status, body) = host_request(
        &endpoint,
        "POST",
        "/dashboard/api/v4/auth/register",
        Some(br#"{"username":"loop-admin","password":"correct-horse"}"#),
        true,
        Some(&session),
    )
    .await
    .unwrap();
    assert_eq!(status, 201);
    let cookie = dashboard_cookie(&session).expect("register stores the session cookie");
    let text = String::from_utf8(body).unwrap();
    assert!(!text.contains(&cookie));
    assert!(!text.contains("correct-horse"));
    assert_eq!(serving.settings_revision(), revision);

    let (status, _) = host_request(
        &endpoint,
        "POST",
        "/dashboard/api/v4/auth/logout",
        Some(b"{}"),
        true,
        Some(&session),
    )
    .await
    .unwrap();
    assert_eq!(status, 200);
    assert!(dashboard_cookie(&session).is_none());
    assert_eq!(serving.settings_revision(), revision);

    let (status, body) = host_request(
        &endpoint,
        "POST",
        "/dashboard/api/v4/auth/login",
        Some(br#"{"username":"loop-admin","password":"correct-horse"}"#),
        true,
        Some(&session),
    )
    .await
    .unwrap();
    assert_eq!(status, 200);
    let cookie = dashboard_cookie(&session).expect("login stores a new session cookie");
    assert!(!String::from_utf8(body).unwrap().contains(&cookie));
    assert_eq!(serving.settings_revision(), revision);

    stop_serve(&serving).await;
    assert!(matches!(listener_owner::inspect(&dir), Owner::Absent));

    let pid = dead_pid();
    std::fs::write(
        listener_owner::path_for(&dir),
        format!(r#"{{"pid":{pid},"endpoint":"http://127.0.0.1:9"}}"#),
    )
    .unwrap();
    assert!(matches!(listener_owner::inspect(&dir), Owner::Absent));
    let offline = key_command(
        dir.clone(),
        cipher.clone(),
        KeyAction::Add {
            name: "offline-go".into(),
            key: "sk-offline".into(),
            username: None,
            password: None,
        },
    )
    .await
    .unwrap_err();
    assert!(offline.to_string().contains("serve"), "{offline:#}");
    assert!(account_named(&dir, cipher.clone(), "offline-go").is_none());
    key_command(dir.clone(), cipher.clone(), KeyAction::List)
        .await
        .unwrap();

    std::fs::write(listener_owner::path_for(&dir), b"not-json").unwrap();
    let invalid = key_command(
        dir.clone(),
        cipher.clone(),
        KeyAction::Add {
            name: "blocked".into(),
            key: "sk-blocked".into(),
            username: None,
            password: None,
        },
    )
    .await
    .unwrap_err();
    assert!(invalid.to_string().contains("invalid"), "{invalid:#}");
    status_command(dir.clone(), cipher.clone(), false)
        .await
        .unwrap();
    assert!(account_named(&dir, cipher.clone(), "blocked").is_none());

    listener_owner::write(&dir, "http://127.0.0.1:1").unwrap();
    assert!(matches!(listener_owner::inspect(&dir), Owner::Live { .. }));
    key_command(dir.clone(), cipher.clone(), KeyAction::List)
        .await
        .unwrap();
    let transport = key_command(
        dir.clone(),
        cipher.clone(),
        KeyAction::Add {
            name: "no-fallback".into(),
            key: "sk-nofallback".into(),
            username: None,
            password: None,
        },
    )
    .await
    .unwrap_err();
    let failure = transport
        .downcast_ref::<api_cmd::ApiFailure>()
        .unwrap_or_else(|| panic!("{transport:#}"));
    assert_eq!(failure.code, "transport");
    assert!(!transport.to_string().contains("sk-nofallback"));
    assert!(account_named(&dir, cipher.clone(), "no-fallback").is_none());
    assert!(account_named(&dir, cipher, "offline-go").is_none());

    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn failed_bind_leaves_no_listener_marker() {
    let dir = temp_dir("bind-fail");
    let listener = StdTcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let error = start_serve(
        dir.clone(),
        test_cipher(),
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        Some(port),
        None,
    )
    .await
    .err()
    .expect("an occupied port must fail startup");
    assert!(
        !listener_owner::path_for(&dir).exists(),
        "bind failure must not leave a listener marker: {error:#}"
    );
    drop(listener);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn public_bind_key_write_requires_a_session_and_restart_rejects_stale_cas() {
    let dir = temp_dir("public-bind");
    let cipher = test_cipher();
    let port = free_port();
    let serving = start_serve(
        dir.clone(),
        cipher.clone(),
        IpAddr::V4(Ipv4Addr::UNSPECIFIED),
        Some(port),
        None,
    )
    .await
    .unwrap();
    let Owner::Live { endpoint } = listener_owner::inspect(&dir) else {
        panic!("public bind must publish the loopback client origin");
    };
    assert_eq!(endpoint, format!("http://127.0.0.1:{port}"));
    let revision = serving.settings_revision();
    let secret = "sk-public";
    let denied = key_command(
        dir.clone(),
        cipher.clone(),
        KeyAction::Add {
            name: "no-session".into(),
            key: secret.into(),
            username: None,
            password: None,
        },
    )
    .await
    .unwrap_err();
    let failure = denied
        .downcast_ref::<api_cmd::ApiFailure>()
        .unwrap_or_else(|| panic!("{denied:#}"));
    assert_eq!(failure.status, Some(401));
    assert_eq!(failure.code, "unauthorized");
    assert!(!denied.to_string().contains(secret));
    assert_eq!(serving.settings_revision(), revision);
    assert!(
        serving
            .db
            .lock()
            .list_accounts()
            .unwrap()
            .iter()
            .all(|account| account.name != "no-session")
    );

    let contract = host_request(
        &endpoint,
        "GET",
        "/dashboard/api/v4/contract",
        None,
        false,
        None,
    )
    .await
    .unwrap_err();
    assert_eq!(contract.status, Some(401));

    let (status, body) = host_request(
        &endpoint,
        "GET",
        "/dashboard/api/v4/auth/status",
        None,
        false,
        None,
    )
    .await
    .unwrap();
    assert_eq!(status, 200);
    let auth_status: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(auth_status["local"], false);
    assert_eq!(auth_status["initialized"], false);
    assert_eq!(auth_status["revision"].as_u64().unwrap(), revision);
    let generation = auth_status["processGeneration"].as_u64().unwrap();
    assert_eq!(generation, serving.process_generation());

    let session = dir.join("public-session.json");
    let (status, body) = host_request(
        &endpoint,
        "POST",
        "/dashboard/api/v4/auth/register",
        Some(br#"{"username":"cli-admin","password":"correct-horse"}"#),
        true,
        Some(&session),
    )
    .await
    .unwrap();
    assert_eq!(status, 201);
    assert_eq!(serving.settings_revision(), revision);
    let cookie = dashboard_cookie(&session).expect("public register stores the cookie");
    let text = String::from_utf8(body).unwrap();
    assert!(!text.contains(&cookie));
    assert!(!text.contains("correct-horse"));
    let stored: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&session).unwrap()).unwrap();
    assert_eq!(stored["endpoint"], endpoint);
    assert_eq!(stored["version"], 1);

    let (status, body) = host_request(
        &endpoint,
        "GET",
        "/dashboard/api/v4/contract",
        None,
        false,
        Some(&session),
    )
    .await
    .unwrap();
    assert_eq!(status, 200);
    let contract: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(contract["processGeneration"].as_u64().unwrap(), generation);
    assert_eq!(contract["revision"].as_u64().unwrap(), revision);

    let create = br#"{"name":"go-live","key":"sk-live"}"#;
    let (status, body) = host_request(
        &endpoint,
        "POST",
        "/dashboard/api/v4/accounts",
        Some(create),
        true,
        Some(&session),
    )
    .await
    .unwrap();
    assert_eq!(status, 200);
    let text = String::from_utf8(body.clone()).unwrap();
    assert!(!text.contains("sk-live"));
    let created: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let id = created["account"]["id"].as_str().unwrap().to_string();
    assert_eq!(created["account"]["name"], "go-live");
    assert_eq!(created["account"]["enabled"], true);
    assert!(serving.db.lock().get_account(&id).unwrap().unwrap().enabled);
    let stale_revision = serving.settings_revision();
    let stale_generation = serving.process_generation();
    assert_eq!(stale_revision, revision + 1);
    assert_eq!(stale_generation, generation);

    stop_serve(&serving).await;
    let restarted = start_serve(
        dir.clone(),
        cipher,
        IpAddr::V4(Ipv4Addr::UNSPECIFIED),
        Some(port),
        None,
    )
    .await
    .unwrap();
    assert_ne!(restarted.process_generation(), stale_generation);
    let stale = host_request(
        &endpoint,
        "GET",
        "/dashboard/api/v4/contract",
        None,
        false,
        Some(&session),
    )
    .await
    .unwrap_err();
    assert_eq!(stale.status, Some(401));
    assert_eq!(stale.code, "unauthorized");

    let (status, body) = host_request(
        &endpoint,
        "GET",
        "/dashboard/api/v4/auth/status",
        None,
        false,
        None,
    )
    .await
    .unwrap();
    assert_eq!(status, 200);
    let fresh: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(fresh["initialized"], true);
    assert_eq!(
        fresh["processGeneration"].as_u64().unwrap(),
        restarted.process_generation()
    );
    let before_login = restarted.settings_revision();
    let login = serde_json::json!({
        "username": "cli-admin",
        "password": "correct-horse",
        "expectedRevision": fresh["revision"].as_u64().unwrap(),
        "processGeneration": fresh["processGeneration"].as_u64().unwrap(),
    });
    let (status, body) = host_request(
        &endpoint,
        "POST",
        "/dashboard/api/v4/auth/login",
        Some(&serde_json::to_vec(&login).unwrap()),
        true,
        Some(&session),
    )
    .await
    .unwrap();
    assert_eq!(status, 200);
    assert_eq!(restarted.settings_revision(), before_login);
    let cookie = dashboard_cookie(&session).expect("login replaces the stale cookie");
    assert!(!String::from_utf8(body).unwrap().contains(&cookie));

    let patch = serde_json::json!({
        "enabled": false,
        "expectedRevision": stale_revision,
        "processGeneration": stale_generation,
    });
    let conflict = host_request(
        &endpoint,
        "PATCH",
        &format!("/dashboard/api/v4/accounts/{id}"),
        Some(&serde_json::to_vec(&patch).unwrap()),
        true,
        Some(&session),
    )
    .await
    .unwrap_err();
    assert_eq!(conflict.status, Some(409));
    assert_eq!(conflict.code, "revisionConflict");
    assert_eq!(
        conflict.current_revision,
        Some(restarted.settings_revision())
    );
    assert_eq!(
        conflict.process_generation,
        Some(restarted.process_generation())
    );
    assert_eq!(restarted.settings_revision(), before_login);
    assert!(
        restarted
            .db
            .lock()
            .get_account(&id)
            .unwrap()
            .unwrap()
            .enabled
    );

    stop_serve(&restarted).await;
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn preserved_auth_paths_set_and_clear_the_session_without_cas() {
    let dir = temp_dir("v2-auth");
    let port = free_port();
    let serving = start_serve(
        dir.clone(),
        test_cipher(),
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        Some(port),
        None,
    )
    .await
    .unwrap();
    let endpoint = format!("http://127.0.0.1:{port}");
    let revision = serving.settings_revision();
    let session = dir.join("v2-session.json");
    let (status, body) = host_request(
        &endpoint,
        "POST",
        "/dashboard/api/auth/register",
        Some(br#"{"username":"v2-admin","password":"correct-horse"}"#),
        false,
        Some(&session),
    )
    .await
    .unwrap();
    assert_eq!(status, 201);
    assert_eq!(serving.settings_revision(), revision);
    let cookie = dashboard_cookie(&session).expect("preserved register stores the cookie");
    let text = String::from_utf8(body).unwrap();
    assert!(!text.contains(&cookie));
    assert!(!text.contains("correct-horse"));
    assert_eq!(text, r#"{"ok":true}"#);

    let (status, body) = host_request(
        &endpoint,
        "POST",
        "/dashboard/api/auth/logout",
        None,
        false,
        Some(&session),
    )
    .await
    .unwrap();
    assert_eq!(status, 204);
    assert!(body.is_empty());
    assert!(dashboard_cookie(&session).is_none());
    assert_eq!(serving.settings_revision(), revision);

    stop_serve(&serving).await;
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn absent_chromium_does_not_force_native_mode() {
    let dir = temp_dir("no-browser");
    let state = build_state(dir.clone(), test_cipher()).unwrap();
    crate::native_browser::register_discovered(&state, false).unwrap();
    let capabilities = state.browser.capabilities().await;
    assert_ne!(capabilities.mode, ocg_core::browser::BrowserMode::Native);
    if capabilities.mode == ocg_core::browser::BrowserMode::Unsupported {
        assert!(
            capabilities
                .reason
                .as_deref()
                .is_some_and(|reason| !reason.is_empty())
        );
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn explicit_endpoint_is_one_request_and_absent_marker_does_not_write() {
    let dir = temp_dir("explicit-endpoint");
    let cipher = test_cipher();
    let absent = key_command(
        dir.clone(),
        cipher.clone(),
        KeyAction::Add {
            name: "offline-go".into(),
            key: "sk-offline".into(),
            username: None,
            password: None,
        },
    )
    .await
    .unwrap_err();
    assert!(absent.to_string().contains("serve"), "{absent:#}");
    assert!(account_named(&dir, cipher.clone(), "offline-go").is_none());

    let transport = key_command_targeting(
        dir.clone(),
        cipher.clone(),
        KeyAction::Add {
            name: "explicit-go".into(),
            key: "sk-explicit".into(),
            username: None,
            password: None,
        },
        "http://127.0.0.1:1",
        true,
    )
    .await
    .unwrap_err();
    let failure = transport
        .downcast_ref::<api_cmd::ApiFailure>()
        .unwrap_or_else(|| panic!("{transport:#}"));
    assert_eq!(failure.code, "transport");
    assert_eq!(failure.exit_code, 1);
    assert!(!transport.to_string().contains("sk-explicit"));
    assert!(account_named(&dir, cipher, "explicit-go").is_none());
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn second_serve_is_rejected_and_a_foreign_marker_is_left_untouched() {
    let dir = temp_dir("one-serve");
    let port = free_port();
    let first = start_serve(
        dir.clone(),
        test_cipher(),
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        Some(port),
        None,
    )
    .await
    .unwrap();
    let second = start_serve(
        dir.clone(),
        test_cipher(),
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        Some(free_port()),
        None,
    )
    .await
    .err()
    .expect("a second serve must fail while the first holds the data directory");
    assert!(
        second.to_string().contains("did not open the database"),
        "{second:#}"
    );
    assert_eq!(
        listener_owner::inspect(&dir),
        Owner::Live {
            endpoint: format!("http://127.0.0.1:{port}")
        }
    );
    stop_serve(&first).await;

    let foreign_dir = temp_dir("foreign-serve");
    let mut command = if cfg!(windows) {
        std::process::Command::new("cmd")
    } else {
        std::process::Command::new("sleep")
    };
    if cfg!(windows) {
        command.args(["/C", "ping", "-n", "30", "127.0.0.1"]);
    } else {
        command.arg("30");
    }
    let mut child = command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let pid = child.id();
    let marker = format!(r#"{{"pid":{pid},"endpoint":"http://127.0.0.1:9"}}"#);
    std::fs::write(listener_owner::path_for(&foreign_dir), &marker).unwrap();
    let blocked = start_serve(
        foreign_dir.clone(),
        test_cipher(),
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        Some(9),
        None,
    )
    .await
    .err()
    .expect("a live foreign marker must block serve");
    assert!(
        blocked.to_string().contains("not an exclusive owner"),
        "{blocked:#}"
    );
    assert!(!foreign_dir.join("data.sqlite").exists());
    assert_eq!(
        std::fs::read_to_string(listener_owner::path_for(&foreign_dir)).unwrap(),
        marker
    );
    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_dir_all(dir);
    let _ = std::fs::remove_dir_all(foreign_dir);
}

#[tokio::test]
async fn marker_follows_the_rebound_port_for_the_next_write() {
    let dir = temp_dir("rebind-marker");
    let cipher = test_cipher();
    let port = free_port();
    let mut next = free_port();
    if next == port {
        next = free_port();
    }
    let serving = start_serve(
        dir.clone(),
        cipher.clone(),
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        Some(port),
        None,
    )
    .await
    .unwrap();
    let core = serving.core();
    core.rebind_gateway_listener_if_port_changed(port, next, true)
        .await
        .unwrap();
    let expected = format!("http://127.0.0.1:{next}");
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if let Owner::Live { endpoint } = listener_owner::inspect(&dir)
            && endpoint == expected
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "listener marker was not updated from the bound port"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    key_command(
        dir.clone(),
        cipher,
        KeyAction::Add {
            name: "rebound".into(),
            key: "sk-rebound".into(),
            username: None,
            password: None,
        },
    )
    .await
    .unwrap();
    assert!(
        serving
            .db
            .lock()
            .list_accounts()
            .unwrap()
            .iter()
            .any(|account| account.name == "rebound")
    );
    stop_serve(&serving).await;
    assert!(matches!(listener_owner::inspect(&dir), Owner::Absent));
    let _ = std::fs::remove_dir_all(dir);
}

fn portable_backup_dir(path: &Path) -> String {
    let mut text = path.to_string_lossy().replace('\\', "/");
    while text.len() > 1 && text.ends_with('/') {
        text.pop();
    }
    text
}

fn backup_auth_dir(data_dir: &Path) -> String {
    format!("{}/cpa/auth", portable_backup_dir(data_dir))
}

async fn run_backup(args: &[&str]) -> Result<Vec<u8>, anyhow::Error> {
    let cli = Cli::try_parse_from(args).unwrap();
    let mut sink = Vec::new();
    dispatch_to(cli, &mut sink).await?;
    Ok(sink)
}

fn assert_receipt(bytes: &[u8], state: &str) -> serde_json::Value {
    let text = String::from_utf8(bytes.to_vec()).unwrap();
    assert!(
        !text.contains("synthetic-encryption-key-material"),
        "{text}"
    );
    assert!(!text.contains("synthetic-oauth-token"), "{text}");
    assert!(!text.contains("synthetic-cpa-client-key"), "{text}");
    assert!(!text.contains("portable"), "{text}");
    let value: serde_json::Value = serde_json::from_str(text.trim()).unwrap();
    let mut keys = value
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    keys.sort();
    assert_eq!(keys, vec!["count", "format", "path", "state", "version"]);
    assert_eq!(value["format"], "ocg-directory-snapshot");
    assert_eq!(value["version"], 1);
    assert_eq!(value["state"], state);
    assert!(value["count"].as_u64().unwrap() > 0);
    value
}

#[tokio::test]
async fn backup_command_roundtrip_relocates_owned_cpa_auth_without_printing_secrets() {
    let root = temp_dir("backup-roundtrip");
    let source = root.join("source-root");
    let target = root.join("target-root");
    let output = root.join("snapshot.tar.gz");
    {
        let state = build_state(source.clone(), test_cipher()).unwrap();
        drop(state);
    }
    let sqlite_before = std::fs::read(source.join("data.sqlite")).unwrap();
    std::fs::create_dir_all(source.join("cpa/auth")).unwrap();
    std::fs::create_dir_all(source.join("cpa/versions")).unwrap();
    std::fs::create_dir_all(source.join("policy")).unwrap();
    let managed = br#"{"desiredRunning":true,"note":"keep-bytes"}"#;
    std::fs::write(source.join("cpa/managed.json"), managed).unwrap();
    let auth = backup_auth_dir(&source);
    let yaml = format!(
        "host: \"127.0.0.1\"\nport: 8085\nauth-dir: \"{auth}\"\ndebug: false\napi-keys:\n  - \"synthetic-cpa-client-key\"\nprojection:\n  generation: 7\n  note: keep-projection\n"
    );
    std::fs::write(source.join("cpa/config.yaml"), &yaml).unwrap();
    std::fs::write(source.join("cpa/config.yaml.previous"), &yaml).unwrap();
    std::fs::write(source.join("cpa/auth/token.json"), b"synthetic-oauth-token").unwrap();
    std::fs::write(source.join("cpa/versions/note.txt"), b"installed").unwrap();
    std::fs::write(source.join("policy/future.txt"), b"future-policy").unwrap();
    std::fs::write(
        source.join(".encryption-key"),
        b"synthetic-encryption-key-material",
    )
    .unwrap();
    std::fs::write(
        source.join("cli-listener.json"),
        br#"{"pid":1,"endpoint":"http://127.0.0.1:9"}"#,
    )
    .unwrap();

    let parsed = Cli::try_parse_from([
        "ocg",
        "--data-dir",
        source.to_str().unwrap(),
        "backup",
        "create",
        "--output",
        output.to_str().unwrap(),
    ])
    .unwrap();
    assert!(matches!(
        parsed.command,
        Commands::Backup {
            action: BackupCommand::Create { .. }
        }
    ));
    let mut sink = Vec::new();
    dispatch_to(parsed, &mut sink).await.unwrap();
    let created = assert_receipt(&sink, "created");

    std::fs::create_dir(&target).unwrap();
    let restored_bytes = run_backup(&[
        "ocg",
        "--data-dir",
        target.to_str().unwrap(),
        "backup",
        "restore",
        "--input",
        output.to_str().unwrap(),
    ])
    .await
    .unwrap();
    let restored = assert_receipt(&restored_bytes, "restored");
    assert_eq!(restored["count"], created["count"]);

    let restored_config = std::fs::read_to_string(target.join("cpa/config.yaml")).unwrap();
    let previous = std::fs::read_to_string(target.join("cpa/config.yaml.previous")).unwrap();
    let relocated = backup_auth_dir(&target);
    assert!(restored_config.contains(&relocated), "{restored_config}");
    assert!(previous.contains(&relocated), "{previous}");
    assert!(
        !restored_config.contains("source-root"),
        "{restored_config}"
    );
    assert!(!previous.contains("source-root"), "{previous}");
    assert!(restored_config.contains("synthetic-cpa-client-key"));
    assert!(restored_config.contains("keep-projection"));
    assert_eq!(
        std::fs::read(target.join("cpa/managed.json")).unwrap(),
        managed
    );
    assert_eq!(
        std::fs::read(target.join("cpa/auth/token.json")).unwrap(),
        b"synthetic-oauth-token"
    );
    assert_eq!(
        std::fs::read(target.join("policy/future.txt")).unwrap(),
        b"future-policy"
    );
    assert_eq!(
        std::fs::read(target.join("data.sqlite")).unwrap(),
        sqlite_before
    );
    assert!(!target.join("cli-listener.json").exists());
    assert!(!target.join(".cli-serve.lock").exists());
    assert!(!target.join(".database-open.lock").exists());
    assert_eq!(
        std::fs::read(source.join("data.sqlite")).unwrap(),
        sqlite_before
    );
    assert!(
        std::fs::read_to_string(source.join("cpa/config.yaml"))
            .unwrap()
            .contains("source-root")
    );
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn backup_command_refuses_a_held_serve_lock_and_existing_paths() {
    let root = temp_dir("backup-refuse");
    let source = root.join("source-root");
    std::fs::create_dir_all(source.join("plans")).unwrap();
    std::fs::write(source.join("plans/rank.txt"), b"rank-a").unwrap();
    let output = root.join("snapshot.tar.gz");
    std::fs::write(&output, b"keep-me").unwrap();
    let error = run_backup(&[
        "ocg",
        "--data-dir",
        source.to_str().unwrap(),
        "backup",
        "create",
        "--output",
        output.to_str().unwrap(),
    ])
    .await
    .unwrap_err();
    assert!(format!("{error:#}").contains("already exists"), "{error:#}");
    assert_eq!(std::fs::read(&output).unwrap(), b"keep-me");
    assert_eq!(
        std::fs::read(source.join("plans/rank.txt")).unwrap(),
        b"rank-a"
    );

    std::fs::remove_file(&output).unwrap();
    let owner = crate::serve_lock::ServeLock::acquire(&source).unwrap();
    let error = run_backup(&[
        "ocg",
        "--data-dir",
        source.to_str().unwrap(),
        "backup",
        "create",
        "--output",
        output.to_str().unwrap(),
    ])
    .await
    .unwrap_err();
    let message = format!("{error:#}");
    assert!(message.contains(".cli-serve.lock"), "{message}");
    assert!(message.contains("Stop"), "{message}");
    assert!(!output.exists());
    assert_eq!(
        std::fs::read(source.join("plans/rank.txt")).unwrap(),
        b"rank-a"
    );
    drop(owner);

    run_backup(&[
        "ocg",
        "--data-dir",
        source.to_str().unwrap(),
        "backup",
        "create",
        "--output",
        output.to_str().unwrap(),
    ])
    .await
    .unwrap();
    let archive = std::fs::read(&output).unwrap();
    let target = root.join("target-root");
    std::fs::create_dir(&target).unwrap();
    std::fs::write(target.join("user.txt"), b"keep-user").unwrap();
    let error = run_backup(&[
        "ocg",
        "--data-dir",
        target.to_str().unwrap(),
        "backup",
        "restore",
        "--input",
        output.to_str().unwrap(),
    ])
    .await
    .unwrap_err();
    assert!(format!("{error:#}").contains("not empty"), "{error:#}");
    assert_eq!(
        std::fs::read(target.join("user.txt")).unwrap(),
        b"keep-user"
    );
    assert!(!target.join("plans").exists());
    assert_eq!(std::fs::read(&output).unwrap(), archive);
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn backup_command_hides_explicit_keys_and_rejects_the_wrong_one() {
    let root = temp_dir("backup-redact");
    let source = root.join("source-root");
    let target = root.join("target-root");
    let wrong_target = root.join("wrong-root");
    let output = root.join("snapshot.tar.gz");
    std::fs::create_dir_all(source.join("notes")).unwrap();
    std::fs::write(source.join("notes/plain.txt"), b"synthetic-oauth-token").unwrap();
    std::fs::write(source.join(".encryption-key"), b"synthetic-stale-file-key").unwrap();
    let explicit = "synthetic-cli-explicit-key";
    let wrong = "synthetic-cli-wrong-key";
    let created = run_backup(&[
        "ocg",
        "--data-dir",
        source.to_str().unwrap(),
        "--encryption-key",
        explicit,
        "backup",
        "create",
        "--output",
        output.to_str().unwrap(),
    ])
    .await
    .unwrap();
    let created_text = String::from_utf8(created.clone()).unwrap();
    assert!(!created_text.contains(explicit), "{created_text}");
    assert!(!created_text.contains(wrong), "{created_text}");
    assert!(
        !created_text.contains("synthetic-stale-file-key"),
        "{created_text}"
    );
    assert!(
        !created_text.contains("synthetic-oauth-token"),
        "{created_text}"
    );
    assert_receipt(&created, "created");

    let error = run_backup(&[
        "ocg",
        "--data-dir",
        wrong_target.to_str().unwrap(),
        "--encryption-key",
        wrong,
        "backup",
        "restore",
        "--input",
        output.to_str().unwrap(),
    ])
    .await
    .unwrap_err();
    let message = format!("{error}");
    let debug = format!("{error:?}");
    let alt = format!("{error:#}");
    for rendered in [&message, &debug, &alt] {
        assert!(!rendered.contains(explicit), "{rendered}");
        assert!(!rendered.contains(wrong), "{rendered}");
        assert!(!rendered.contains("synthetic-stale-file-key"), "{rendered}");
        assert!(!rendered.contains("synthetic-oauth-token"), "{rendered}");
    }
    assert!(alt.contains("witness"), "{alt}");
    assert!(!wrong_target.exists());

    std::fs::create_dir(&target).unwrap();
    let restored = run_backup(&[
        "ocg",
        "--data-dir",
        target.to_str().unwrap(),
        "--encryption-key",
        explicit,
        "backup",
        "restore",
        "--input",
        output.to_str().unwrap(),
    ])
    .await
    .unwrap();
    let restored_text = String::from_utf8(restored.clone()).unwrap();
    assert!(!restored_text.contains(explicit), "{restored_text}");
    assert_receipt(&restored, "restored");
    assert_eq!(
        std::fs::read(target.join("notes/plain.txt")).unwrap(),
        b"synthetic-oauth-token"
    );
    assert_eq!(
        std::fs::read(target.join(".encryption-key")).unwrap(),
        b"synthetic-stale-file-key"
    );
    let _ = std::fs::remove_dir_all(root);
}

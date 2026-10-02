use anyhow::Result;
use clap::{Parser, Subcommand};
use ocg_core::account_control::{self, AccountControlError};
use ocg_core::crypto::{KeyCipher, StaticKeyCipher, load_or_create_static_cipher};
use ocg_core::db::Database;
use ocg_core::gateway::{self, GatewayLifecycle, ListenerStopOutcome};
use ocg_core::log_types::{OperationMetadata, OperationOutcome, OperationSource};
use ocg_core::models::{Account, AppConfig};
use ocg_core::provider::CredentialKind;
use ocg_core::skill_install;
use ocg_core::state::{CoreState, CoreStateInner};
use ocg_core::user_operation::UserOperation;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Parser)]
#[command(name = "ocg-manager-cli")]
#[command(about = "Headless CLI for Open Console Gateway")]
#[command(version)]
#[command(after_long_help = r#"FIRST RUN
  ocg-manager-cli serve --port 9042
  Open http://127.0.0.1:9042/dashboard/ in your browser. Add an upstream
  account in Accounts, then copy the Gateway Key and API Base URL from Access
  Center directly into your client. The usual local Base URL ends in /v1.

DOWNLOAD AND UPGRADE
  Get the matching platform archive and SHA256SUMS from the same GitHub
  Release. Verify the checksum, extract the whole archive (binary and dist/),
  and replace that extraction as a unit on upgrade. Keep the data directory.

CAPABILITY BOUNDARY
  The CLI starts the Gateway and offers limited account operations. Use the
  dashboard for other Providers, client Access Keys, Custom API destinations,
  model/protocol settings, routing, proxy, and backup/import.

CODEX SKILL
  Native release builds sync the bundled ocg-manager skill to ~/.agents/skills on
  serve. Run `ocg-manager-cli skill sync` to install or repair it explicitly.
  Existing OCG-managed versions are backed up; an unrelated same-name skill
  is left untouched.

SECRET HANDLING
  Do not paste Keys or passwords into an agent conversation. `key add` and
  --encryption-key accept plaintext process arguments; enter credentials in
  the local dashboard when an agent is helping. status hides the Gateway Key
  unless --show-key is explicitly requested in a private terminal.

Guide: https://github.com/klarkxy/open-console-gateway/tree/main/docs/user"#)]
struct Cli {
    /// Data directory for the CLI (default: ~/.ocg-mgr-cli)
    #[arg(long, global = true)]
    data_dir: Option<PathBuf>,

    /// Encryption key for API key storage.
    /// If omitted, uses OCG_MANAGER_ENCRYPTION_KEY env var or generates one in <data-dir>/.encryption-key.
    #[arg(long, global = true)]
    encryption_key: Option<String>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Start the gateway server and, in native releases, sync the Codex skill
    #[command(
        after_long_help = "The default listener is 127.0.0.1:9042. --port saves the port in SQLite. Keep dist/ beside the executable for the dashboard. A non-loopback --host requires dashboard administrator login; plan authentication, TLS, and network access before exposing it. Native release builds sync the bundled Codex skill when serve starts; the Docker build does not install it on the host."
    )]
    Serve {
        /// Address to listen on
        #[arg(long, default_value = "127.0.0.1")]
        host: IpAddr,
        /// Gateway port (overrides config)
        #[arg(short, long)]
        port: Option<u16>,
        /// Directory containing the built web dashboard (dist)
        #[arg(long)]
        dashboard_dir: Option<PathBuf>,
    },
    /// Manage account API keys (see key --help for scope)
    #[command(
        after_long_help = "key add and key ping target OpenCode Go only. key list includes API-key accounts across Providers, while key remove/enable/disable act on the supplied account ID even for other Providers; confirm identity in the dashboard first. key ping makes a real upstream request and may print an upstream response excerpt. Enter new secrets in the local dashboard when an agent is assisting."
    )]
    Key {
        #[command(subcommand)]
        action: KeyAction,
    },
    /// Show gateway status (Gateway Key hidden by default)
    #[command(
        after_long_help = "Ordinary status output hides the primary Gateway Key. --show-key prints the complete value; use it only in a private terminal and do not copy its output into agent chat, logs, or support tickets."
    )]
    Status {
        /// Print the primary gateway key (only in a private terminal)
        #[arg(long)]
        show_key: bool,
    },
    /// Install or update the bundled Codex skill
    Skill {
        #[command(subcommand)]
        action: SkillAction,
    },
}

#[derive(Subcommand)]
enum SkillAction {
    /// Sync to ~/.agents/skills/ocg-manager; back up a prior OCG-managed copy
    #[command(
        after_long_help = "Install the skill embedded in this binary into the current user's ~/.agents/skills/ocg-manager. Repeating the command is safe when the installed copy matches. When bundled skill content changes, the previous OCG-managed copy is moved to ~/.agents/skill-backups before replacement. A same-name skill without OCG's ownership marker, or a locally edited matching-copy, is left unchanged."
    )]
    Sync,
}

#[derive(Subcommand)]
enum KeyAction {
    /// List all keys and their status
    List,
    /// Add an OpenCode Go key (plaintext argument; prefer the dashboard)
    #[command(
        after_long_help = "The Key and optional --password are process arguments and may appear in shell history or process inspection. When an agent is helping, enter them directly in the local dashboard instead of chat or a tool command."
    )]
    Add {
        /// Display name for the key
        name: String,
        /// The OpenCode-Go API key
        key: String,
        /// OpenCode-Go login account
        #[arg(long)]
        username: Option<String>,
        /// OpenCode-Go login password
        #[arg(long)]
        password: Option<String>,
    },
    /// Remove a key
    Remove {
        /// Account ID
        id: String,
    },
    /// Enable a key
    Enable {
        /// Account ID
        id: String,
    },
    /// Disable a key
    Disable {
        /// Account ID
        id: String,
    },
    /// Ping OpenCode Go with one or all enabled keys; shows real status/body
    Ping {
        /// Account ID; omit to ping every enabled key
        id: Option<String>,
        /// Model to send (default: mimo-v2.5)
        #[arg(long, default_value = ocg_core::models::DEFAULT_ACCOUNT_TEST_MODEL)]
        model: String,
        /// User message (default: "ping")
        #[arg(long, default_value = "ping")]
        message: String,
        /// max_tokens for the ping (default: 3)
        #[arg(long, default_value_t = 3)]
        max_tokens: u32,
    },
}

fn main() -> Result<()> {
    ocg_core::process_log::init();
    ocg_core::cpa_runtime::host::run_internal_supervisor_if_requested();
    run_cli()
}

#[tokio::main]
async fn run_cli() -> Result<()> {
    let cli = Cli::parse();
    if let Commands::Skill { action } = &cli.command {
        return match action {
            SkillAction::Sync => {
                let started_at = chrono::Utc::now();
                let result = skill_install::sync_user_skill();
                record_skill_sync(
                    &resolve_data_dir(cli.data_dir.clone()),
                    started_at,
                    result.is_ok(),
                );
                let result = result?;
                println!("Codex skill {:?}: {}", result.status, result.path.display());
                Ok(())
            }
        };
    }
    let data_dir = resolve_data_dir(cli.data_dir);
    // Skill handling returned above, and the internal supervisor never reaches
    // this process. Docker omits program-log-file via --no-default-features.
    #[cfg(feature = "program-log-file")]
    ocg_core::process_log::activate_file_sink(&data_dir);
    let cipher = resolve_cipher(&data_dir, cli.encryption_key)?;

    match cli.command {
        Commands::Serve {
            host,
            port,
            dashboard_dir,
        } => serve(data_dir, cipher, host, port, dashboard_dir).await,
        Commands::Key { action } => key_command(data_dir, cipher, action).await,
        Commands::Status { show_key } => status_command(data_dir, cipher, show_key).await,
        Commands::Skill { .. } => unreachable!(),
    }
}

fn resolve_data_dir(data_dir: Option<PathBuf>) -> PathBuf {
    data_dir.unwrap_or_else(|| {
        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .map(PathBuf::from)
            .unwrap_or_else(|_| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
        home.join(".ocg-mgr-cli")
    })
}

fn resolve_cipher(
    data_dir: &Path,
    encryption_key: Option<String>,
) -> Result<Arc<dyn KeyCipher + Send + Sync>> {
    let env_key = std::env::var("OCG_MANAGER_ENCRYPTION_KEY").ok();
    resolve_cipher_with(data_dir, encryption_key, env_key)
}

/// Priority: explicit encryption_key > env_key > on-disk key file.
fn resolve_cipher_with(
    data_dir: &Path,
    encryption_key: Option<String>,
    env_key: Option<String>,
) -> Result<Arc<dyn KeyCipher + Send + Sync>> {
    let cipher = match encryption_key {
        Some(secret) => StaticKeyCipher::new(&secret),
        None => match env_key {
            Some(secret) => StaticKeyCipher::new(&secret),
            None => load_or_create_static_cipher(data_dir)?,
        },
    };
    Ok(Arc::new(cipher))
}

fn build_state(
    data_dir: PathBuf,
    cipher: Arc<dyn KeyCipher + Send + Sync>,
) -> Result<Arc<CoreStateInner>> {
    let db = Database::open_with_cipher(data_dir.clone(), cipher.clone())?;
    Ok(Arc::new(CoreStateInner::new(db, data_dir, cipher)?))
}

fn register_dsh_application_host(_state: &Arc<CoreStateInner>) {
    #[cfg(feature = "dsh-local-host")]
    ocg_core::byok_application_host::register(_state);
    #[cfg(feature = "dsh-local-host")]
    ocg_core::dsh_application_host::register(_state);
}

async fn serve(
    data_dir: PathBuf,
    cipher: Arc<dyn KeyCipher + Send + Sync>,
    host: IpAddr,
    port: Option<u16>,
    dashboard_dir: Option<PathBuf>,
) -> Result<()> {
    if cfg!(feature = "install-codex-skill")
        && !cfg!(debug_assertions)
        && let Err(error) = skill_install::sync_user_skill()
    {
        tracing::warn!("Codex skill synchronization failed: {error:#}");
    }
    let state = start_serve(data_dir, cipher, host, port, dashboard_dir).await?;
    println!("press Ctrl+C to stop");
    tokio::signal::ctrl_c().await?;
    println!("shutting down...");
    stop_serve(&state).await;
    Ok(())
}

async fn start_serve(
    data_dir: PathBuf,
    cipher: Arc<dyn KeyCipher + Send + Sync>,
    host: IpAddr,
    port: Option<u16>,
    dashboard_dir: Option<PathBuf>,
) -> Result<Arc<CoreStateInner>> {
    let state = build_state(data_dir, cipher)?;
    ocg_core::cpa_runtime::host::register_owned_host(&state);
    register_dsh_application_host(&state);
    let executable = if dashboard_dir.is_none() {
        std::env::current_exe().ok()
    } else {
        None
    };
    state.set_dashboard_dir(resolve_dashboard_dir(dashboard_dir, executable.as_deref()));

    let operation = UserOperation::new(
        &state,
        OperationSource::Cli,
        "gateway.start",
        "gateway",
        None,
    );
    let mut config = state.config();
    let mut saved_revision = None;
    if let Some(port) = port
        && config.gateway_port != port
    {
        config.gateway_port = port;
        if let Err(error) = state.set_config_recorded(config.clone(), |revision| {
            saved_revision = Some(revision);
        }) {
            operation.complete(
                if saved_revision.is_some() {
                    OperationOutcome::Partial
                } else {
                    OperationOutcome::Failed
                },
                Some("persist.failed"),
                OperationMetadata {
                    revision: saved_revision,
                    changed_fields: if saved_revision.is_some() {
                        vec!["gateway_port".into()]
                    } else {
                        vec![]
                    },
                    ..Default::default()
                },
            );
            return Err(error);
        }
    }

    let handle =
        match gateway::start_gateway_on(state.clone(), SocketAddr::new(host, config.gateway_port))
            .await
        {
            Ok(handle) => handle,
            Err(error) => {
                operation.complete(
                    if saved_revision.is_some() {
                        OperationOutcome::Partial
                    } else {
                        OperationOutcome::Failed
                    },
                    Some("bind.failed"),
                    OperationMetadata {
                        revision: saved_revision,
                        changed_fields: if saved_revision.is_some() {
                            vec!["gateway_port".into()]
                        } else {
                            vec![]
                        },
                        ..Default::default()
                    },
                );
                return Err(error);
            }
        };
    println!("gateway started on http://{}:{}", host, handle.port);
    println!("gateway key: [hidden; use status --show-key in a private terminal]");
    println!("dashboard: http://{}:{}/dashboard/", host, handle.port);
    println!(
        "upstream: {}",
        ocg_core::gateway::free_models::opencode_go_base_url(&config.upstream_base_url)
    );

    {
        let mut gateway_lock = state.gateway.lock();
        *gateway_lock = Some(handle);
    }

    let restorer = state.clone();
    tokio::spawn(async move {
        restorer.restore_owned_cpa_runtime_on_startup().await;
    });

    operation.complete(
        OperationOutcome::Success,
        None,
        OperationMetadata {
            completed_count: Some(1),
            ..OperationMetadata::default()
        },
    );
    state.log_runtime_event(
        "info",
        "gateway",
        &format!("cli gateway started on port {}", config.gateway_port),
    );
    Ok(state)
}

async fn stop_serve(state: &CoreState) {
    let operation =
        UserOperation::new(state, OperationSource::Cli, "gateway.stop", "gateway", None);
    state.stop_owned_cpa_runtime();
    let handle = state.gateway.lock().take();
    if let Some(handle) = handle {
        let stopped = GatewayLifecycle::stop_and_wait(handle).await;
        if stopped == ListenerStopOutcome::Graceful {
            state.clear_gateway_error();
            operation.complete(
                OperationOutcome::Success,
                None,
                OperationMetadata {
                    completed_count: Some(1),
                    ..OperationMetadata::default()
                },
            );
        } else {
            operation.complete(
                OperationOutcome::Failed,
                Some("stop.failed"),
                OperationMetadata::default(),
            );
        }
    }
    state.log_runtime_event("info", "gateway", "cli gateway stopped");
}

fn resolve_dashboard_dir(explicit: Option<PathBuf>, executable: Option<&Path>) -> Option<PathBuf> {
    explicit.or_else(|| {
        let dist = executable?.parent()?.join("dist");
        dist.is_dir().then_some(dist)
    })
}

async fn key_command(
    data_dir: PathBuf,
    cipher: Arc<dyn KeyCipher + Send + Sync>,
    action: KeyAction,
) -> Result<()> {
    let state = build_state(data_dir, cipher)?;
    let db = state.db.lock();

    match action {
        KeyAction::List => {
            let accounts = db
                .list_accounts()?
                .into_iter()
                .filter(|account| account.credential_kind == CredentialKind::ApiKey)
                .collect::<Vec<_>>();
            if accounts.is_empty() {
                println!("no keys configured");
                return Ok(());
            }
            println!("{:<36} {:<20} {:<8}", "id", "name", "enabled");
            for account in accounts {
                println!(
                    "{:<36} {:<20} {:<8}",
                    account.id,
                    account.name,
                    if account.enabled { "yes" } else { "no" },
                );
            }
        }
        KeyAction::Add {
            name,
            key,
            username,
            password,
        } => {
            drop(db);
            let mut operation = UserOperation::new(
                &state,
                OperationSource::Cli,
                "account.create",
                "account",
                None,
            );
            let mut committed = None;
            let result = account_control::create_go_api_key_recorded(
                &state,
                name,
                key,
                username,
                password,
                |id, revision| {
                    operation.subject(id.to_string());
                    committed = Some(revision);
                },
            );
            match result {
                Ok(account) => {
                    operation.subject(account.id.clone());
                    operation.complete(
                        OperationOutcome::Success,
                        None,
                        OperationMetadata {
                            revision: committed,
                            completed_count: Some(1),
                            ..OperationMetadata::default()
                        },
                    );
                    println!("added key {} ({})", account.id, account.name);
                }
                Err(error) => {
                    finish_control_recorded(
                        operation,
                        &error,
                        committed.map(|revision| OperationMetadata {
                            revision: Some(revision),
                            completed_count: Some(1),
                            failed_count: Some(1),
                            ..Default::default()
                        }),
                    );
                    return Err(error.into());
                }
            }
        }
        KeyAction::Remove { id } => {
            drop(db);
            let operation = UserOperation::new(
                &state,
                OperationSource::Cli,
                "account.delete",
                "account",
                Some(id.clone()),
            );
            let loaded = state.db.lock().get_account(&id);
            let account = match loaded {
                Ok(Some(account)) => account,
                Ok(None) => {
                    operation.complete(
                        OperationOutcome::Rejected,
                        Some("notfound"),
                        OperationMetadata::default(),
                    );
                    return Err(anyhow::anyhow!("key not found: {id}"));
                }
                Err(error) => {
                    operation.complete(
                        OperationOutcome::Failed,
                        Some("internal"),
                        OperationMetadata::default(),
                    );
                    return Err(error);
                }
            };
            let mut committed = None;
            match account_control::delete_account_recorded(&state, &id, None, |revision| {
                committed = Some(revision)
            })
            .await
            {
                Ok(revision) => {
                    operation.complete(
                        OperationOutcome::Success,
                        None,
                        OperationMetadata {
                            revision: Some(revision),
                            completed_count: Some(1),
                            ..OperationMetadata::default()
                        },
                    );
                    println!("removed key {} ({})", id, account.name);
                }
                Err(error) => {
                    if let Some(revision) = committed {
                        operation.complete(
                            OperationOutcome::Partial,
                            Some("internal"),
                            OperationMetadata {
                                revision: Some(revision),
                                completed_count: Some(1),
                                failed_count: Some(1),
                                ..OperationMetadata::default()
                            },
                        );
                    } else {
                        finish_control(operation, &error);
                    }
                    return Err(error.into());
                }
            }
        }
        KeyAction::Enable { id } => {
            drop(db);
            toggle_account(&state, &id, true)?;
        }
        KeyAction::Disable { id } => {
            drop(db);
            toggle_account(&state, &id, false)?;
        }
        KeyAction::Ping {
            id,
            model,
            message,
            max_tokens,
        } => {
            drop(db);
            ping_keys(&state, id.as_deref(), &model, &message, max_tokens).await?;
        }
    }
    Ok(())
}

fn toggle_account(state: &Arc<CoreStateInner>, id: &str, enabled: bool) -> Result<()> {
    let action = if enabled {
        "account.enable"
    } else {
        "account.disable"
    };
    let operation = UserOperation::new(
        state,
        OperationSource::Cli,
        action,
        "account",
        Some(id.to_string()),
    );
    let mut committed = None;
    match account_control::set_account_enabled_recorded(state, id, enabled, |revision| {
        committed = Some(revision);
    }) {
        Ok(account) => {
            operation.complete(
                OperationOutcome::Success,
                None,
                OperationMetadata {
                    changed_fields: vec!["enabled".to_string()],
                    revision: committed,
                    completed_count: Some(1),
                    ..OperationMetadata::default()
                },
            );
            println!(
                "{} key {} ({})",
                if enabled { "enabled" } else { "disabled" },
                id,
                account.name
            );
            Ok(())
        }
        Err(error) => {
            finish_control_recorded(
                operation,
                &error,
                committed.map(|revision| OperationMetadata {
                    changed_fields: vec!["enabled".into()],
                    revision: Some(revision),
                    completed_count: Some(1),
                    failed_count: Some(1),
                    ..Default::default()
                }),
            );
            Err(error.into())
        }
    }
}

fn finish_control(operation: UserOperation, error: &AccountControlError) {
    finish_control_recorded(operation, error, None);
}

fn finish_control_recorded(
    operation: UserOperation,
    error: &AccountControlError,
    committed: Option<OperationMetadata>,
) {
    let (outcome, reason) = match error {
        AccountControlError::NotFound => (OperationOutcome::Rejected, "notfound"),
        AccountControlError::Invalid(_) => (OperationOutcome::Rejected, "invalid"),
        AccountControlError::Conflict(_) | AccountControlError::RevisionConflict => {
            (OperationOutcome::Rejected, "conflict")
        }
        AccountControlError::Unavailable(_) => (OperationOutcome::Failed, "unavailable"),
        AccountControlError::Internal(_) => (OperationOutcome::Failed, "internal"),
    };
    match committed {
        Some(metadata) => operation.complete(OperationOutcome::Partial, Some(reason), metadata),
        None => operation.complete(outcome, Some(reason), OperationMetadata::default()),
    }
}

async fn status_command(
    data_dir: PathBuf,
    cipher: Arc<dyn KeyCipher + Send + Sync>,
    show_key: bool,
) -> Result<()> {
    let state = build_state(data_dir, cipher)?;
    let config: AppConfig = state.config();
    let db = state.db.lock();
    // Keep the CLI's historical "account" count credential-oriented: the
    // database-owned Zen Free route is a system card, not a key managed here.
    let accounts = db
        .list_accounts()?
        .into_iter()
        .filter(|account| account.credential_kind == CredentialKind::ApiKey)
        .collect::<Vec<_>>();
    let enabled = accounts.iter().filter(|a| a.enabled).count();

    println!("data dir: {:?}", state.data_dir());
    println!("gateway port: {}", config.gateway_port);
    if show_key {
        println!("gateway key: {}", config.gateway_key);
    } else {
        println!("gateway key: [hidden; use --show-key in a private terminal]");
    }
    println!(
        "upstream: {}",
        ocg_core::gateway::free_models::opencode_go_base_url(&config.upstream_base_url)
    );
    println!("accounts: {} total, {} enabled", accounts.len(), enabled);
    Ok(())
}

/// One-shot ping: decrypts the key, sends a tiny chat completion, prints real upstream status.
/// Used to surface real 401/403/429/200 — what each key actually does upstream, no inference.
async fn ping_one(
    state: &Arc<CoreStateInner>,
    account: &Account,
    model: &str,
    message: &str,
    max_tokens: u32,
) -> (u16, String) {
    let key = match state.decrypt_key(&account.key_cipher) {
        Ok(k) => k,
        Err(e) => return (0, format!("decrypt failed: {e}")),
    };
    let (config, client) = state.upstream_context();
    let url = format!(
        "{}/v1/chat/completions",
        ocg_core::gateway::free_models::opencode_go_base_url(&config.upstream_base_url)
    );
    let body = serde_json::json!({
        "model": model,
        "messages": [{"role": "user", "content": message}],
        "max_tokens": max_tokens,
        "stream": false });
    let started = std::time::Instant::now();
    let resp = client
        .post(&url)
        .header("Authorization", format!("Bearer {key}"))
        .header("Content-Type", "application/json")
        .json(&body)
        .timeout(std::time::Duration::from_secs(
            config.non_stream_timeout_secs,
        ))
        .send()
        .await;
    let elapsed = started.elapsed();
    match resp {
        Ok(r) => {
            let status = r.status().as_u16();
            match r.text().await {
                Ok(text) => {
                    let trimmed = text.chars().take(200).collect::<String>();
                    (status, format!("{}ms {}", elapsed.as_millis(), trimmed))
                }
                Err(error) => {
                    let error = if error.is_timeout() {
                        "response body timed out".to_string()
                    } else {
                        format!("response body failed: {error}")
                    };
                    (
                        0,
                        format!("{}ms {} after HTTP {}", elapsed.as_millis(), error, status),
                    )
                }
            }
        }
        Err(e) => (
            0,
            format!("{}ms request failed: {}", elapsed.as_millis(), e),
        ),
    }
}

async fn ping_keys(
    state: &Arc<CoreStateInner>,
    id: Option<&str>,
    model: &str,
    message: &str,
    max_tokens: u32,
) -> Result<()> {
    let operation = UserOperation::new(
        state,
        OperationSource::Cli,
        "account.ping",
        "account",
        id.map(str::to_string),
    );
    let targets = match load_ping_targets(state, id) {
        Ok(targets) => targets,
        Err(error) => {
            let (outcome, reason) = match &error {
                PingLoadError::NotFound(_) => (OperationOutcome::Rejected, "notfound"),
                PingLoadError::Forbidden => (OperationOutcome::Rejected, "forbidden"),
                PingLoadError::NotReady => (OperationOutcome::Rejected, "notready"),
                PingLoadError::Store(_) => (OperationOutcome::Failed, "internal"),
            };
            operation.complete(outcome, Some(reason), OperationMetadata::default());
            return Err(error.into());
        }
    };
    if targets.is_empty() {
        operation.complete(
            OperationOutcome::Success,
            None,
            OperationMetadata {
                requested_count: Some(0),
                completed_count: Some(0),
                failed_count: Some(0),
                ..OperationMetadata::default()
            },
        );
        println!("no keys to ping");
        return Ok(());
    }
    println!(
        "pinging {} key(s) with model={} message={:?}",
        targets.len(),
        model,
        message
    );
    let mut completed = 0_u32;
    let mut failed = 0_u32;
    for account in &targets {
        let (status, body) = ping_one(state, account, model, message, max_tokens).await;
        if status == 200 {
            completed = completed.saturating_add(1);
        } else {
            failed = failed.saturating_add(1);
        }
        let verdict = if status == 200 { "OK" } else { "FAIL" };
        println!(
            "[{}] {} ({}) status={} {}",
            verdict, account.id, account.name, status, body
        );
    }
    let outcome = if failed == 0 {
        OperationOutcome::Success
    } else if completed == 0 {
        OperationOutcome::Failed
    } else {
        OperationOutcome::Partial
    };
    let related_ids = if targets.len() <= ocg_core::log_types::MAX_RELATED_IDS {
        targets
            .iter()
            .map(|account| account.id.clone())
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    operation.complete(
        outcome,
        (outcome != OperationOutcome::Success).then_some("upstream"),
        OperationMetadata {
            requested_count: Some(u32::try_from(targets.len()).unwrap_or(u32::MAX)),
            completed_count: Some(completed),
            failed_count: Some(failed),
            related_ids,
            ..OperationMetadata::default()
        },
    );
    Ok(())
}

#[derive(Debug)]
enum PingLoadError {
    NotFound(String),
    Forbidden,
    NotReady,
    Store(anyhow::Error),
}

impl std::fmt::Display for PingLoadError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound(id) => write!(formatter, "key not found: {id}"),
            Self::Forbidden => formatter.write_str(
                "Zen Free is provider-owned; use the dashboard provider-settings operation",
            ),
            Self::NotReady => {
                formatter.write_str("account setup is not complete and cannot be pinged")
            }
            Self::Store(error) => write!(formatter, "{error}"),
        }
    }
}

impl std::error::Error for PingLoadError {}

fn load_ping_targets(
    state: &CoreStateInner,
    id: Option<&str>,
) -> std::result::Result<Vec<Account>, PingLoadError> {
    let db = state.db.lock();
    match id {
        Some(id) => match db.get_account(id).map_err(PingLoadError::Store)? {
            Some(account) => {
                if account.is_zen_free() {
                    return Err(PingLoadError::Forbidden);
                }
                if account.credential_kind == CredentialKind::ApiKey
                    && account.provider_id == ocg_core::provider::OPENCODE_PROVIDER_ID
                    && account.setup_step.is_ready()
                    && !account.key_cipher.is_empty()
                {
                    Ok(vec![account])
                } else {
                    Err(PingLoadError::NotReady)
                }
            }
            None => Err(PingLoadError::NotFound(id.to_string())),
        },
        None => Ok(db
            .list_accounts()
            .map_err(PingLoadError::Store)?
            .into_iter()
            .filter(|account| {
                account.credential_kind == CredentialKind::ApiKey
                    && account.provider_id == ocg_core::provider::OPENCODE_PROVIDER_ID
                    && account.setup_step.is_ready()
                    && !account.key_cipher.is_empty()
            })
            .collect()),
    }
}

#[cfg(test)]
mod tests;

fn record_skill_sync(data_dir: &Path, started_at: chrono::DateTime<chrono::Utc>, success: bool) {
    let operation = ocg_core::log_types::OperationLog {
        operation_id: uuid::Uuid::new_v4().to_string(),
        started_at,
        completed_at: Some(chrono::Utc::now()),
        action: "skill.sync".into(),
        source: OperationSource::Cli,
        actor_id: None,
        subject_type: Some("skill".into()),
        subject_id: Some("ocg-manager".into()),
        outcome: if success {
            OperationOutcome::Success
        } else {
            OperationOutcome::Failed
        },
        reason_code: (!success).then(|| "sync.failed".into()),
        metadata: Default::default(),
    };
    if Database::record_existing_operation(data_dir, &operation).is_err() {
        tracing::warn!(
            action = "skill.sync",
            "operation receipt could not be stored"
        );
    }
}

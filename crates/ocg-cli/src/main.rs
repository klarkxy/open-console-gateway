use anyhow::Result;
use clap::{Parser, Subcommand};
use ocg_core::account_control;
use ocg_core::crypto::{KeyCipher, StaticKeyCipher, load_or_create_static_cipher};
use ocg_core::db::Database;
use ocg_core::gateway::{self, GatewayLifecycle};
use ocg_core::models::{Account, AppConfig};
use ocg_core::provider::CredentialKind;
use ocg_core::state::CoreStateInner;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Parser)]
#[command(name = "ocg-manager-cli")]
#[command(about = "Headless CLI for Open Console Gateway")]
#[command(version)]
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
    /// Start the gateway server
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
    /// Manage API keys
    Key {
        #[command(subcommand)]
        action: KeyAction,
    },
    /// Show gateway status
    Status,
}

#[derive(Subcommand)]
enum KeyAction {
    /// List all keys and their status
    List,
    /// Add a new key
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
    /// Ping upstream with one or all enabled keys — shows real status code / body
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
    ocg_core::cpa_runtime::host::run_internal_supervisor_if_requested();
    run_cli()
}

#[tokio::main]
async fn run_cli() -> Result<()> {
    let cli = Cli::parse();
    let data_dir = resolve_data_dir(cli.data_dir);
    let cipher = resolve_cipher(&data_dir, cli.encryption_key)?;

    match cli.command {
        Commands::Serve {
            host,
            port,
            dashboard_dir,
        } => serve(data_dir, cipher, host, port, dashboard_dir).await,
        Commands::Key { action } => key_command(data_dir, cipher, action).await,
        Commands::Status => status_command(data_dir, cipher).await,
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
    ocg_core::dsh_application_host::register(_state);
}

async fn serve(
    data_dir: PathBuf,
    cipher: Arc<dyn KeyCipher + Send + Sync>,
    host: IpAddr,
    port: Option<u16>,
    dashboard_dir: Option<PathBuf>,
) -> Result<()> {
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

    let mut config = state.config();
    if let Some(port) = port {
        config.gateway_port = port;
        state.set_config(config.clone())?;
    }

    let handle =
        gateway::start_gateway_on(state.clone(), SocketAddr::new(host, config.gateway_port))
            .await?;
    println!("gateway started on http://{}:{}", host, handle.port);
    println!("gateway key: {}", config.gateway_key);
    println!("dashboard: http://{}:{}/dashboard/", host, handle.port);
    println!(
        "upstream: {}",
        ocg_core::gateway::free_models::opencode_go_base_url(&config.upstream_base_url)
    );

    {
        let mut gateway_lock = state.gateway.lock();
        *gateway_lock = Some(handle);
    }

    let _ = state.db.lock().log_gateway(
        "info",
        "gateway",
        &format!("cli gateway started on port {}", config.gateway_port),
    );
    Ok(state)
}

async fn stop_serve(state: &CoreStateInner) {
    state.stop_owned_cpa_runtime();
    let handle = state.gateway.lock().take();
    if let Some(handle) = handle {
        let _ = GatewayLifecycle::stop_and_wait(handle).await;
    }
    let _ = state
        .db
        .lock()
        .log_gateway("info", "gateway", "cli gateway stopped");
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
            let account =
                account_control::create_go_api_key(&state, name, key, username, password)?;
            println!("added key {} ({})", account.id, account.name);
        }
        KeyAction::Remove { id } => {
            drop(db);
            let account = state
                .db
                .lock()
                .get_account(&id)?
                .ok_or_else(|| anyhow::anyhow!("key not found: {id}"))?;
            account_control::delete_account(&state, &id, None).await?;
            println!("removed key {} ({})", id, account.name);
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
    let account = account_control::set_account_enabled(state, id, enabled)?;
    println!(
        "{} key {} ({})",
        if enabled { "enabled" } else { "disabled" },
        id,
        account.name
    );
    Ok(())
}

fn reject_zen_key_operation(account: &Account) -> Result<()> {
    if account.is_zen_free() {
        anyhow::bail!("Zen Free is provider-owned; use the dashboard provider-settings operation");
    }
    Ok(())
}

async fn status_command(data_dir: PathBuf, cipher: Arc<dyn KeyCipher + Send + Sync>) -> Result<()> {
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
    println!("gateway key: {}", config.gateway_key);
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
    let targets: Vec<Account> = {
        let db = state.db.lock();
        match id {
            Some(i) => match db.get_account(i)? {
                Some(a) => {
                    reject_zen_key_operation(&a)?;
                    if a.credential_kind == CredentialKind::ApiKey
                        && a.provider_id == ocg_core::provider::OPENCODE_PROVIDER_ID
                        && a.setup_step.is_ready()
                        && !a.key_cipher.is_empty()
                    {
                        vec![a]
                    } else {
                        anyhow::bail!("account setup is not complete and cannot be pinged")
                    }
                }
                None => anyhow::bail!("key not found: {i}"),
            },
            None => db
                .list_accounts()?
                .into_iter()
                .filter(|a| {
                    a.credential_kind == CredentialKind::ApiKey
                        && a.provider_id == ocg_core::provider::OPENCODE_PROVIDER_ID
                        && a.setup_step.is_ready()
                        && !a.key_cipher.is_empty()
                })
                .collect(),
        }
    };
    if targets.is_empty() {
        println!("no keys to ping");
        return Ok(());
    }
    println!(
        "pinging {} key(s) with model={} message={:?}",
        targets.len(),
        model,
        message
    );
    for account in targets {
        let (status, body) = ping_one(state, &account, model, message, max_tokens).await;
        let verdict = if status == 200 { "OK" } else { "FAIL" };
        println!(
            "[{}] {} ({}) status={} {}",
            verdict, account.id, account.name, status, body
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests;

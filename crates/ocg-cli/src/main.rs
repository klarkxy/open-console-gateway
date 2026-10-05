mod api_cmd;
mod endpoint;
mod listener_owner;
mod live_key;
mod native_browser;
mod process_alive;
mod serve_lock;
mod session_file;

use anyhow::{Context, Result};
use clap::{CommandFactory, FromArgMatches, Parser, Subcommand};
#[cfg(test)]
use ocg_core::account_control;
use ocg_core::crypto::{
    KeyCipher, MachineBoundCipher, StaticKeyCipher, load_or_create_static_cipher,
};
use ocg_core::db::Database;
use ocg_core::gateway::{self, GatewayLifecycle};
use ocg_core::models::AppConfig;
use ocg_core::provider::CredentialKind;
use ocg_core::state::{CoreStateInner, GatewayHandle};
use parking_lot::Mutex as ParkingMutex;
use std::io::{self, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

#[derive(Parser)]
#[command(name = "ocg", bin_name = "ocg")]
#[command(about = "Headless CLI for Open Console Gateway")]
#[command(version)]
#[command(after_long_help = r#"FIRST RUN
  ocg serve --port 9042
  The default listener is 127.0.0.1:9042. It serves the V4 dashboard API and
  the inference routes. Loopback clients use the existing local-dashboard
  trust. --host 0.0.0.0 keeps session authentication. No window is opened.
  A dist/ directory, when present, is served as static files only.
  The default data directory is ~/.ocg3. This command does not open, move,
  or delete a previous generation's ~/.ocg-mgr-cli. Pass --data-dir to
  choose a directory.

BUILD AND UPGRADE
  Build this CLI checkout with cargo build -p ocg-cli --locked.
  The executable is ocg (ocg.exe on Windows), under target/debug/
  (target/release/ with --release). Historical desktop release archives
  do not deliver this CLI development checkout. Stop the host and back up
  its data directory before replacing the binary; keep the matching source
  and operator guide together.

CAPABILITY BOUNDARY
  api METHOD PATH calls a running host. schema prints an offline catalog.
  backup create and backup restore copy a stopped data directory. They do
  not open the database, contact a provider, or start CPA. Stop serve first.
  key add/remove/enable/disable call the persistent serve process once.
  cli-listener.json projects that process's address; it is not an exclusive
  owner. A missing marker fails the change. Pass --endpoint to name one host.
  A failed change is not retried. key list and status read SQLite. key ping
  requires a running serve and calls that process's exact-account model-test.
  It does not open the database or call a provider directly. Copying the data
  directory is a stopped filesystem copy; it is not an api route.

SECRET HANDLING
  Do not paste Keys or passwords into an agent conversation. `key add` and
  --encryption-key accept plaintext process arguments. api reads request
  bodies from --input and the gateway key from --key-file, and it does not
  print those values or session cookies. status hides the Gateway Key unless
  --show-key is explicitly requested in a private terminal.

Guide: https://github.com/klarkxy/open-console-gateway/tree/main/docs/user"#)]
struct Cli {
    /// Data directory (default: ~/.ocg3). A previous-generation ~/.ocg-mgr-cli directory is not opened, moved, or deleted.
    #[arg(long, global = true)]
    data_dir: Option<PathBuf>,

    /// Encryption key for API key storage.
    /// If omitted, uses OCG_MANAGER_ENCRYPTION_KEY env var or generates one in <data-dir>/.encryption-key.
    #[arg(long, global = true)]
    encryption_key: Option<String>,

    /// Origin of a running host. Credentials and fragments are rejected.
    #[arg(long, global = true, default_value = "http://127.0.0.1:9042")]
    endpoint: String,

    /// Set when the operator passed --endpoint. The default is not a key-write target.
    #[arg(skip)]
    endpoint_explicit: bool,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Start the gateway server
    #[command(
        after_long_help = "The default listener is 127.0.0.1:9042. --port saves the port in SQLite. Loopback binds use local-dashboard trust. A public bind requires the dashboard session and still checks Host. --dashboard-dir serves static files and does not open a window. --cpa-host-dir is the pinned ocg-cpa-host directory that contains manifest.json and the executable. An empty profile still starts this control API. Inference stays unavailable until that host, an applied projection, and the policy callback are ready. Saved product settings stay saved until start applies them. Ctrl+C stops the listener and any CPA child this process started. A second serve for the same data directory is refused while the first process holds its lock. After the listener moves, this process republishes cli-listener.json from the bound address."
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
        /// Pinned ocg-cpa-host directory (manifest.json and the executable)
        #[arg(long)]
        cpa_host_dir: Option<PathBuf>,
    },
    /// Send one HTTP request to a running host
    Api {
        /// HTTP method
        method: String,
        /// Absolute path, beginning with /
        path: String,
        /// JSON body file, or - for stdin
        #[arg(long)]
        input: Option<PathBuf>,
        /// Write the response body here instead of stdout
        #[arg(long)]
        output: Option<PathBuf>,
        /// Fill absent expectedRevision and processGeneration from public auth status
        #[arg(long)]
        cas_current: bool,
        /// Gateway bearer token file, used only on inference routes
        #[arg(long)]
        key_file: Option<PathBuf>,
        /// Cookie jar bound to this exact endpoint
        #[arg(long)]
        session_file: Option<PathBuf>,
    },
    /// Print the offline v3 or v4 schema catalog
    #[command(
        after_long_help = "schema does not contact a host and does not open the data directory. v4 is the live HTTP catalog. v3 remains the offline document for types remounted under /dashboard/api/v4. api does not call /dashboard/api/v3."
    )]
    Schema {
        /// v3 or v4
        version: String,
    },
    /// Copy a stopped data directory to or from an offline snapshot
    #[command(
        after_long_help = "backup create reads one stopped data directory and writes a new snapshot file. backup restore writes that snapshot into a data directory that does not exist yet or is empty. Both commands refuse a running serve or database lock instead of waiting. They do not open Database, create an encryption key file, call a provider, stop an unknown process, or start CPA. The JSON receipt lists path, format, version, count, and state. It does not print file contents or secrets. serve, api, and schema are unchanged."
    )]
    Backup {
        #[command(subcommand)]
        action: BackupCommand,
    },
    /// Manage account API keys (see key --help for scope)
    #[command(
        after_long_help = "key add targets OpenCode Go only. key list includes API-key accounts across Providers, while key remove/enable/disable act on the supplied account ID even for other Providers; confirm identity in the dashboard first. add, remove, enable, and disable call the persistent serve process once and do not open the database. A missing or dead cli-listener.json is not a second writer. Pass --endpoint to name one host. A failed request is not sent again and is not applied to SQLite. list and status read this data directory. key ping requires a running serve and calls POST /dashboard/api/v4/accounts/{id}/model-tests for that exact account. It does not open the database, select another account, or call a provider directly. --endpoint does not bypass a stopped serve. Output shows the observed status and a redacted error. Enter new secrets in the local dashboard when an agent is assisting."
    )]
    Key {
        #[command(subcommand)]
        action: KeyAction,
    },
    /// Show gateway status (Gateway Key hidden by default)
    #[command(
        after_long_help = "Ordinary status output hides the primary Gateway Key. --show-key prints the complete value; use it only in a private terminal and do not copy its output into agent chat, logs, or support tickets. status reads SQLite while the host is stopped. While serve is running, use api GET /dashboard/api/v4/gateway/status. status also prints cpa listener, installed, running, applied, ready, and unavailable. It does not print CPA secrets."
    )]
    Status {
        /// Print the primary gateway key (only in a private terminal)
        #[arg(long)]
        show_key: bool,
    },
}

#[derive(Subcommand)]
enum BackupCommand {
    /// Write a snapshot of a stopped data directory
    Create {
        /// New snapshot file. An existing path is left untouched.
        #[arg(long)]
        output: PathBuf,
    },
    /// Restore a snapshot into a new empty data directory
    Restore {
        /// Snapshot file produced by backup create
        #[arg(long)]
        input: PathBuf,
    },
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
    /// Test one exact account model through the running serve
    Ping {
        /// Exact account ID
        id: String,
        /// Model id sent as modelId
        #[arg(long, default_value = ocg_core::models::DEFAULT_ACCOUNT_TEST_MODEL)]
        model: String,
        /// User message. Omit to keep the server's minimal verification body.
        #[arg(long)]
        message: Option<String>,
        /// Positive maxTokens. Omit to keep the server's minimal verification body.
        #[arg(long)]
        max_tokens: Option<u32>,
    },
}

fn main() {
    ocg_core::cpa_runtime::host::run_internal_supervisor_if_requested();
    if let Err(error) = run_cli() {
        if let Some(failure) = error.downcast_ref::<api_cmd::ApiFailure>() {
            eprintln!("{}", failure.stderr_line());
            std::process::exit(failure.exit_code);
        }
        eprintln!("{error:#}");
        std::process::exit(1);
    }
}

#[tokio::main]
async fn run_cli() -> Result<()> {
    let cli = parse_cli();
    let mut stdout = StdoutSink;
    dispatch_to(cli, &mut stdout).await
}

fn parse_cli() -> Cli {
    let mut command = Cli::command();
    attach_api_help(&mut command);
    match command.try_get_matches() {
        Ok(matches) => {
            let endpoint_explicit =
                matches.value_source("endpoint") == Some(clap::parser::ValueSource::CommandLine);
            let mut cli = Cli::from_arg_matches(&matches).unwrap_or_else(|error| error.exit());
            cli.endpoint_explicit = endpoint_explicit;
            cli
        }
        Err(error) => error.exit(),
    }
}

fn attach_api_help(command: &mut clap::Command) {
    if let Some(api) = command.find_subcommand_mut("api") {
        let updated = api.clone().after_long_help(api_cmd::API_HELP);
        *api = updated;
    }
}

struct StdoutSink;

impl api_cmd::BodySink for StdoutSink {
    fn write_chunk(&mut self, bytes: &[u8]) -> io::Result<()> {
        let mut out = io::stdout().lock();
        out.write_all(bytes)?;
        out.flush()
    }
}

async fn dispatch_to(cli: Cli, sink: &mut dyn api_cmd::BodySink) -> Result<()> {
    match cli.command {
        Commands::Api {
            method,
            path,
            input,
            output,
            cas_current,
            key_file,
            session_file,
        } => {
            api_cmd::invoke(
                api_cmd::Invocation {
                    endpoint: cli.endpoint,
                    method,
                    path,
                    input,
                    output,
                    cas_current,
                    key_file,
                    session_file,
                },
                sink,
            )
            .await?;
            Ok(())
        }
        Commands::Schema { version } => {
            let json = api_cmd::schema_json(&version)?;
            sink.write_chunk(json.as_bytes())
                .map_err(|error| anyhow::anyhow!("{error}"))?;
            Ok(())
        }
        Commands::Backup { action } => {
            let explicit_key = cli.encryption_key;
            let data_dir = resolve_data_dir(cli.data_dir);
            let witness = cipher_witness(explicit_key);
            let receipt = match action {
                BackupCommand::Create { output } => {
                    ocg_core::backup::create_snapshot(&data_dir, &output, witness)?
                }
                BackupCommand::Restore { input } => {
                    ocg_core::backup::restore_snapshot(&data_dir, &input, witness)?
                }
            };
            sink.write_chunk(receipt.to_json_line().as_bytes())
                .map_err(|error| anyhow::anyhow!("{error}"))?;
            Ok(())
        }
        command => {
            let data_dir = resolve_data_dir(cli.data_dir);
            match command {
                Commands::Key {
                    action:
                        KeyAction::Ping {
                            id,
                            model,
                            message,
                            max_tokens,
                        },
                } => {
                    let line = ping_owned_serve(data_dir, id, model, message, max_tokens).await?;
                    println!("{line}");
                    Ok(())
                }
                command => {
                    let cipher = resolve_cipher(&data_dir, cli.encryption_key)?;
                    match command {
                        Commands::Serve {
                            host,
                            port,
                            dashboard_dir,
                            cpa_host_dir,
                        } => serve(data_dir, cipher, host, port, dashboard_dir, cpa_host_dir).await,
                        Commands::Key { action } => {
                            key_command_targeting(
                                data_dir,
                                cipher,
                                action,
                                &cli.endpoint,
                                cli.endpoint_explicit,
                            )
                            .await
                        }
                        Commands::Status { show_key } => {
                            status_command(data_dir, cipher, show_key).await
                        }
                        Commands::Api { .. }
                        | Commands::Schema { .. }
                        | Commands::Backup { .. } => unreachable!(),
                    }
                }
            }
        }
    }
}

fn cipher_witness(explicit: Option<String>) -> ocg_core::backup::CipherWitness {
    let env = std::env::var("OCG_MANAGER_ENCRYPTION_KEY")
        .ok()
        .filter(|value| !value.trim().is_empty());
    ocg_core::backup::CipherWitness::from_sources(explicit, env)
}

fn cli_home_dir() -> PathBuf {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
}

/// `<home>/.ocg3`. This does not create, open, move, or delete
/// `<home>/.ocg-mgr-cli`.
fn default_generation_data_dir(home: &Path) -> PathBuf {
    home.join(".ocg3")
}

fn resolve_data_dir(data_dir: Option<PathBuf>) -> PathBuf {
    data_dir.unwrap_or_else(|| default_generation_data_dir(&cli_home_dir()))
}

fn resolve_cipher(
    data_dir: &Path,
    encryption_key: Option<String>,
) -> Result<Arc<dyn KeyCipher + Send + Sync>> {
    let env_key = std::env::var("OCG_MANAGER_ENCRYPTION_KEY").ok();
    resolve_cipher_with(data_dir, encryption_key, env_key)
}

/// Priority: explicit encryption_key > env_key > on-disk key file.
/// On Windows the desktop host seals credentials with the machine cipher and
/// stores no key file, so that cipher is the fallback when the file is absent.
fn resolve_cipher_with(
    data_dir: &Path,
    encryption_key: Option<String>,
    env_key: Option<String>,
) -> Result<Arc<dyn KeyCipher + Send + Sync>> {
    if let Some(secret) = encryption_key {
        return Ok(Arc::new(StaticKeyCipher::new(&secret)));
    }
    if let Some(secret) = env_key {
        return Ok(Arc::new(StaticKeyCipher::new(&secret)));
    }
    let key_path = data_dir.join(".encryption-key");
    if key_path.is_file() {
        return Ok(Arc::new(load_or_create_static_cipher(data_dir)?));
    }
    #[cfg(windows)]
    {
        Ok(Arc::new(MachineBoundCipher::new()))
    }
    #[cfg(not(windows))]
    {
        Ok(Arc::new(load_or_create_static_cipher(data_dir)?))
    }
}

fn build_state(
    data_dir: PathBuf,
    cipher: Arc<dyn KeyCipher + Send + Sync>,
) -> Result<Arc<CoreStateInner>> {
    let db = Database::open_with_cipher(data_dir.clone(), cipher.clone())?;
    Ok(Arc::new(CoreStateInner::new(db, data_dir, cipher)?))
}

const KEY_WRITE_GUIDANCE: &str = "account changes require the persistent serve process. Start `ocg serve` for this data directory, or pass --endpoint to send one request. key list and status can still read SQLite. This command did not change the database.";

struct RunningServe {
    state: Arc<CoreStateInner>,
    lock: ParkingMutex<Option<serve_lock::ServeLock>>,
    stop_watcher: tokio::sync::watch::Sender<bool>,
    watcher: ParkingMutex<Option<tokio::task::JoinHandle<()>>>,
}

impl Deref for RunningServe {
    type Target = CoreStateInner;

    fn deref(&self) -> &CoreStateInner {
        &self.state
    }
}

impl RunningServe {
    #[cfg(test)]
    fn core(&self) -> Arc<CoreStateInner> {
        Arc::clone(&self.state)
    }
}

async fn serve(
    data_dir: PathBuf,
    cipher: Arc<dyn KeyCipher + Send + Sync>,
    host: IpAddr,
    port: Option<u16>,
    dashboard_dir: Option<PathBuf>,
    cpa_host_dir: Option<PathBuf>,
) -> Result<()> {
    let running =
        start_serve_with_artifact(data_dir, cipher, host, port, dashboard_dir, cpa_host_dir)
            .await?;
    println!("press Ctrl+C to stop");
    tokio::signal::ctrl_c().await?;
    println!("shutting down...");
    stop_serve(&running).await;
    Ok(())
}

async fn start_serve(
    data_dir: PathBuf,
    cipher: Arc<dyn KeyCipher + Send + Sync>,
    host: IpAddr,
    port: Option<u16>,
    dashboard_dir: Option<PathBuf>,
) -> Result<RunningServe> {
    start_serve_with_artifact(data_dir, cipher, host, port, dashboard_dir, None).await
}

async fn start_serve_with_artifact(
    data_dir: PathBuf,
    cipher: Arc<dyn KeyCipher + Send + Sync>,
    host: IpAddr,
    port: Option<u16>,
    dashboard_dir: Option<PathBuf>,
    cpa_host_dir: Option<PathBuf>,
) -> Result<RunningServe> {
    let lock = serve_lock::ServeLock::acquire(&data_dir)?;
    listener_owner::refuse_foreign_writer(&data_dir)?;
    let state = build_state(data_dir, cipher)?;
    if let Some(dir) = cpa_host_dir {
        ocg_core::cpa_execution::set_artifact_dir(&state, dir);
    }
    ocg_core::cpa_runtime::host::register_owned_host(&state);
    #[cfg(feature = "dsh-local-host")]
    {
        ocg_core::byok_application_host::register(&state);
        ocg_core::dsh_application_host::register(&state);
    }
    if let Err(error) = native_browser::register(&state) {
        state.stop_owned_cpa_runtime();
        return Err(error);
    }
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

    // console_router is the library default inside start_gateway_on. That
    // start also runs the existing usage-sync workers. Do not install a
    // narrower factory first.
    let handle =
        match gateway::start_gateway_on(state.clone(), SocketAddr::new(host, config.gateway_port))
            .await
        {
            Ok(handle) => handle,
            Err(error) => {
                abandon_start(&state, None, false).await;
                return Err(error);
            }
        };
    let endpoint = published_endpoint(host, handle.listen_addr);
    if let Err(error) = listener_owner::write(&state.data_dir(), &endpoint) {
        abandon_start(&state, Some(handle), false).await;
        return Err(error);
    }
    let bound_port = handle.port;
    {
        let mut gateway_lock = state.gateway.lock();
        *gateway_lock = Some(handle);
    }
    let restoring = Arc::clone(&state);
    tokio::spawn(async move {
        ocg_core::cpa_execution::restore_pinned_if_desired(&restoring).await;
    });
    println!("gateway started on http://{host}:{bound_port}");
    println!("gateway key: [hidden; use status --show-key in a private terminal]");
    println!("dashboard api: http://{host}:{bound_port}/dashboard/api/v4",);
    println!("inference: http://{host}:{bound_port}/v1");
    println!(
        "upstream: {}",
        ocg_core::gateway::free_models::opencode_go_base_url(&config.upstream_base_url)
    );
    let (stop_watcher, watcher) = spawn_marker_watcher(Arc::clone(&state), host);
    let _ = state.db.lock().log_gateway(
        "info",
        "gateway",
        &format!("cli gateway started on port {bound_port}"),
    );
    Ok(RunningServe {
        state,
        lock: ParkingMutex::new(Some(lock)),
        stop_watcher,
        watcher: ParkingMutex::new(Some(watcher)),
    })
}

fn spawn_marker_watcher(
    state: Arc<CoreStateInner>,
    host: IpAddr,
) -> (
    tokio::sync::watch::Sender<bool>,
    tokio::task::JoinHandle<()>,
) {
    let (sender, mut receiver) = tokio::sync::watch::channel(false);
    let watcher = tokio::spawn(async move {
        loop {
            tokio::select! {
                biased;
                changed = receiver.changed() => {
                    if changed.is_err() || *receiver.borrow() {
                        break;
                    }
                }
                _ = tokio::time::sleep(Duration::from_millis(250)) => {
                    let bound = state.gateway.lock().as_ref().map(|handle| handle.listen_addr);
                    let Some(bound) = bound else {
                        break;
                    };
                    let endpoint = published_endpoint(host, bound);
                    let _ = listener_owner::update_endpoint_if_ours(&state.data_dir(), &endpoint);
                }
            }
        }
    });
    (sender, watcher)
}

async fn abandon_start(state: &CoreStateInner, handle: Option<GatewayHandle>, published: bool) {
    state.stop_owned_cpa_runtime();
    if let Some(handle) = handle {
        let _ = GatewayLifecycle::stop_and_wait(handle).await;
    }
    native_browser::close_owned(&state.data_dir());
    if published {
        listener_owner::remove_if_ours(&state.data_dir());
    }
}

fn published_endpoint(requested: IpAddr, bound: SocketAddr) -> String {
    let port = bound.port();
    let ip = if requested.is_unspecified() {
        if requested.is_ipv6() {
            IpAddr::V6(Ipv6Addr::LOCALHOST)
        } else {
            IpAddr::V4(Ipv4Addr::LOCALHOST)
        }
    } else {
        bound.ip()
    };
    match ip {
        IpAddr::V6(ip) => format!("http://[{ip}]:{port}"),
        IpAddr::V4(ip) => format!("http://{ip}:{port}"),
    }
}

async fn stop_serve(running: &RunningServe) {
    let _ = running.stop_watcher.send(true);
    let watcher = running.watcher.lock().take();
    if let Some(watcher) = watcher {
        let _ = watcher.await;
    }
    running.state.stop_owned_cpa_runtime();
    let handle = running.state.gateway.lock().take();
    if let Some(handle) = handle {
        let _ = GatewayLifecycle::stop_and_wait(handle).await;
    }
    native_browser::close_owned(&running.state.data_dir());
    listener_owner::remove_if_ours(&running.state.data_dir());
    let _ = running
        .state
        .db
        .lock()
        .log_gateway("info", "gateway", "cli gateway stopped");
    let _ = running.lock.lock().take();
}

fn resolve_dashboard_dir(explicit: Option<PathBuf>, executable: Option<&Path>) -> Option<PathBuf> {
    explicit.or_else(|| {
        let dist = executable?.parent()?.join("dist");
        dist.is_dir().then_some(dist)
    })
}

#[cfg(test)]
async fn key_command(
    data_dir: PathBuf,
    cipher: Arc<dyn KeyCipher + Send + Sync>,
    action: KeyAction,
) -> Result<()> {
    key_command_targeting(data_dir, cipher, action, endpoint::DEFAULT_ORIGIN, false).await
}

async fn ping_owned_serve(
    data_dir: PathBuf,
    id: String,
    model: String,
    message: Option<String>,
    max_tokens: Option<u32>,
) -> Result<String> {
    let endpoint = match listener_owner::inspect(&data_dir) {
        listener_owner::Owner::Live { endpoint } => endpoint,
        listener_owner::Owner::Absent => {
            anyhow::bail!(
                "key ping requires a running serve. This command did not open the database."
            )
        }
        listener_owner::Owner::Unreadable { message } => anyhow::bail!(
            "{message} key ping requires a running serve. This command did not open the database."
        ),
    };
    live_key::ping(&endpoint, &id, &model, message.as_deref(), max_tokens).await
}

async fn key_command_targeting(
    data_dir: PathBuf,
    cipher: Arc<dyn KeyCipher + Send + Sync>,
    action: KeyAction,
    endpoint: &str,
    endpoint_explicit: bool,
) -> Result<()> {
    if let KeyAction::Ping {
        id,
        model,
        message,
        max_tokens,
    } = action
    {
        let line = ping_owned_serve(data_dir, id, model, message, max_tokens).await?;
        println!("{line}");
        return Ok(());
    }
    if matches!(action, KeyAction::List) {
        let _offline_owner = serve_lock::ServeLock::acquire(&data_dir)
            .context("offline reads need a stopped host; use api against the running host")?;
        let state = build_state(data_dir, cipher)?;
        return key_action_offline(&state, action).await;
    }
    let target = if endpoint_explicit {
        endpoint.to_string()
    } else {
        match listener_owner::inspect(&data_dir) {
            listener_owner::Owner::Live { endpoint } => endpoint,
            listener_owner::Owner::Unreadable { message } => {
                anyhow::bail!("{message} {KEY_WRITE_GUIDANCE}")
            }
            listener_owner::Owner::Absent => anyhow::bail!("{KEY_WRITE_GUIDANCE}"),
        }
    };
    match action {
        KeyAction::Add {
            name,
            key,
            username,
            password,
        } => live_key::add(&target, name, key, username, password).await,
        KeyAction::Remove { id } => live_key::remove(&target, &id).await,
        KeyAction::Enable { id } => live_key::set_enabled(&target, &id, true).await,
        KeyAction::Disable { id } => live_key::set_enabled(&target, &id, false).await,
        KeyAction::List | KeyAction::Ping { .. } => unreachable!(),
    }
}

async fn key_action_offline(state: &Arc<CoreStateInner>, action: KeyAction) -> Result<()> {
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
        KeyAction::Add { .. }
        | KeyAction::Remove { .. }
        | KeyAction::Enable { .. }
        | KeyAction::Disable { .. }
        | KeyAction::Ping { .. } => {
            anyhow::bail!("{KEY_WRITE_GUIDANCE}")
        }
    }
    Ok(())
}

#[cfg(test)]
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

async fn status_command(
    data_dir: PathBuf,
    cipher: Arc<dyn KeyCipher + Send + Sync>,
    show_key: bool,
) -> Result<()> {
    let _offline_owner = serve_lock::ServeLock::acquire(&data_dir).context(
        "offline status needs a stopped host; use api GET /dashboard/api/v4/gateway/status",
    )?;
    let state = build_state(data_dir, cipher)?;
    let config: AppConfig = state.config();
    let (total, enabled) = {
        let db = state.db.lock();
        // Keep the CLI's historical "account" count credential-oriented: the
        // database-owned Zen Free route is a system card, not a key managed here.
        let accounts = db
            .list_accounts()?
            .into_iter()
            .filter(|account| account.credential_kind == CredentialKind::ApiKey)
            .collect::<Vec<_>>();
        let enabled = accounts.iter().filter(|account| account.enabled).count();
        (accounts.len(), enabled)
    };
    let report = ocg_core::cpa_execution::execution_report(&state);

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
    println!("accounts: {total} total, {enabled} enabled");
    println!(
        "cpa listener: {}",
        if report.listener_bound {
            "bound"
        } else {
            "not-bound"
        }
    );
    println!(
        "cpa installed: {}",
        if report.installed { "yes" } else { "no" }
    );
    println!("cpa running: {}", if report.running { "yes" } else { "no" });
    println!(
        "cpa applied: {}",
        if report.applied_revision == 0 {
            "none".to_string()
        } else {
            report.applied_revision.to_string()
        }
    );
    println!(
        "cpa ready: {}",
        if report.inference_ready { "yes" } else { "no" }
    );
    println!(
        "cpa unavailable: {}",
        if report.unavailable { "yes" } else { "no" }
    );
    Ok(())
}

#[cfg(test)]
mod tests;

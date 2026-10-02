use std::{
    env,
    fs::{self, OpenOptions},
    io::{ErrorKind, Write},
    net::SocketAddr,
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, anyhow, bail};
use axum::{
    Json, Router,
    body::Body,
    extract::{Request, State},
    http::{HeaderMap, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::get,
};
use serde::{Deserialize, Serialize};
use tokio::{net::TcpListener, process::Child, sync::Mutex, time::timeout};
use url::Url;
use uuid::Uuid;

const DEFAULT_CONTROL_ADDR: &str = "0.0.0.0:9080";
const DEFAULT_TOKEN_FILE: &str = "/run/ocg-browser/control-token";
const DEFAULT_PROFILE_ROOT: &str = "/profiles";
const DEFAULT_BROWSER_BINARY: &str = "/usr/bin/chromium";
const DEFAULT_DISPLAY: &str = ":99";
const DEFAULT_VNC_WS_URL: &str = "ws://browser:6080/websockify";
const MAX_URL_LEN: usize = 2048;

#[derive(Clone, Debug)]
struct Config {
    control_addr: SocketAddr,
    token_file: PathBuf,
    profile_root: PathBuf,
    browser_binary: PathBuf,
    display: String,
    vnc_ws_url: String,
    shutdown_timeout: Duration,
    strict_healthcheck: bool,
}

impl Config {
    fn from_env() -> Result<Self> {
        let control_addr = env_value("OCG_BROWSER_CONTROL_ADDR", DEFAULT_CONTROL_ADDR)
            .parse()
            .context("invalid OCG_BROWSER_CONTROL_ADDR")?;
        let shutdown_timeout = env_value("OCG_BROWSER_SHUTDOWN_TIMEOUT_SECS", "15")
            .parse::<u64>()
            .context("invalid OCG_BROWSER_SHUTDOWN_TIMEOUT_SECS")?;
        let strict_healthcheck = match env_value("OCG_BROWSER_STRICT_HEALTHCHECK", "1").as_str() {
            "1" | "true" | "TRUE" => true,
            "0" | "false" | "FALSE" => false,
            _ => bail!("OCG_BROWSER_STRICT_HEALTHCHECK must be 0 or 1"),
        };

        Ok(Self {
            control_addr,
            token_file: PathBuf::from(env_value(
                "OCG_BROWSER_CONTROL_TOKEN_FILE",
                DEFAULT_TOKEN_FILE,
            )),
            profile_root: PathBuf::from(env_value(
                "OCG_BROWSER_PROFILE_ROOT",
                DEFAULT_PROFILE_ROOT,
            )),
            browser_binary: PathBuf::from(env_value("OCG_BROWSER_BINARY", DEFAULT_BROWSER_BINARY)),
            display: env_value("OCG_BROWSER_DISPLAY", DEFAULT_DISPLAY),
            vnc_ws_url: validate_vnc_ws_url(&env_value(
                "OCG_BROWSER_VNC_WS_URL",
                DEFAULT_VNC_WS_URL,
            ))?,
            shutdown_timeout: Duration::from_secs(shutdown_timeout.clamp(1, 60)),
            strict_healthcheck,
        })
    }

    #[cfg(test)]
    fn for_test(root: &Path, browser_binary: PathBuf) -> Self {
        Self {
            control_addr: "127.0.0.1:0".parse().expect("test address"),
            token_file: root.join("run/control-token"),
            profile_root: root.join("profiles"),
            browser_binary,
            display: ":99".into(),
            vnc_ws_url: DEFAULT_VNC_WS_URL.into(),
            shutdown_timeout: Duration::from_secs(2),
            strict_healthcheck: false,
        }
    }
}

fn env_value(name: &str, default: &str) -> String {
    env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| default.to_string())
}

#[derive(Clone)]
struct AppState {
    config: Arc<Config>,
    token: Arc<String>,
    controller: Arc<Mutex<BrowserController>>,
}

impl AppState {
    fn new(config: Config, token: String) -> Self {
        Self {
            config: Arc::new(config),
            token: Arc::new(token),
            controller: Arc::new(Mutex::new(BrowserController::default())),
        }
    }

    async fn stop(&self) {
        let mut controller = self.controller.lock().await;
        if let Err(error) = controller.stop(self.config.shutdown_timeout).await {
            tracing::warn!("failed to stop Chromium cleanly: {error:#}");
        }
    }
}

#[derive(Default)]
struct BrowserController {
    current: Option<BrowserSession>,
    retired: Vec<RetiredBrowserSession>,
}

struct BrowserSession {
    account_id: String,
    child: Child,
    profile_dir: PathBuf,
    #[cfg(unix)]
    process_group_id: Option<u32>,
    started_at_unix_ms: u128,
}

struct RetiredBrowserSession {
    profile_dir: PathBuf,
    #[cfg(unix)]
    process_group_id: Option<u32>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct StartSessionRequest {
    account_id: String,
    url: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct StopSessionRequest {
    #[serde(default)]
    account_id: Option<String>,
}

#[derive(Debug, Serialize)]
struct HealthResponse {
    status: &'static str,
}

#[derive(Debug, Serialize)]
struct SessionResponse {
    status: &'static str,
    active: bool,
    account_id: Option<String>,
    started_at_unix_ms: Option<u128>,
    switched: bool,
    vnc_ws_url: String,
}

#[derive(Debug, Serialize)]
struct ErrorResponse {
    error: String,
}

#[derive(Debug)]
struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    fn bad_request(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: message.into(),
        }
    }

    fn conflict(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::CONFLICT,
            message: message.into(),
        }
    }

    fn internal(error: anyhow::Error) -> Self {
        tracing::error!(error = %error, "browser worker request failed");
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: "browser worker operation failed".into(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(ErrorResponse {
                error: self.message,
            }),
        )
            .into_response()
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    use std::io::IsTerminal;
    let configured = std::env::var("RUST_LOG").ok();
    let filter = configured
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .map(tracing_subscriber::EnvFilter::try_new)
        .transpose();
    let invalid = filter.is_err();
    let installed = tracing_subscriber::fmt()
        .with_env_filter(
            filter
                .ok()
                .flatten()
                .unwrap_or_else(|| tracing_subscriber::EnvFilter::new("warn,ocg=info")),
        )
        .with_writer(std::io::stderr)
        .log_internal_errors(false)
        .with_ansi(std::io::stderr().is_terminal())
        .compact()
        .try_init()
        .is_ok();
    if installed && invalid {
        tracing::warn!("invalid RUST_LOG; using the default program log filter");
    }
    let config = Config::from_env()?;
    ensure_safe_directory(&config.profile_root, 0o700)
        .context("failed to prepare browser profile root")?;
    recover_stale_chromium_profile_locks(&config.profile_root)
        .context("failed to recover stale browser profile locks")?;
    let token = ensure_control_token(&config.token_file)
        .context("failed to prepare browser control token")?;
    let state = AppState::new(config.clone(), token);
    let router = build_router(state.clone());
    let listener = TcpListener::bind(config.control_addr)
        .await
        .with_context(|| format!("failed to listen on {}", config.control_addr))?;

    tracing::info!(address = %config.control_addr, "browser worker listening");
    axum::serve(listener, router)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("browser worker server failed")?;
    state.stop().await;
    Ok(())
}

fn build_router(state: AppState) -> Router {
    let protected = Router::new()
        .route(
            "/session",
            get(get_session).post(start_session).delete(stop_session),
        )
        .route_layer(middleware::from_fn_with_state(state.clone(), require_auth));

    Router::new()
        .route("/health", get(health))
        .merge(protected)
        .with_state(state)
}

async fn require_auth(
    State(state): State<AppState>,
    headers: HeaderMap,
    request: Request<Body>,
    next: Next,
) -> Response {
    let supplied = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "));

    if supplied.is_some_and(|value| constant_time_eq(value.as_bytes(), state.token.as_bytes())) {
        next.run(request).await
    } else {
        (
            StatusCode::UNAUTHORIZED,
            Json(ErrorResponse {
                error: "invalid browser worker token".into(),
            }),
        )
            .into_response()
    }
}

async fn health(State(state): State<AppState>) -> impl IntoResponse {
    if state.config.strict_healthcheck && !browser_services_ready(&state.config).await {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(HealthResponse { status: "starting" }),
        );
    }

    (StatusCode::OK, Json(HealthResponse { status: "ok" }))
}

async fn get_session(State(state): State<AppState>) -> Result<Json<SessionResponse>, ApiError> {
    let mut controller = state.controller.lock().await;
    controller.reap_exited().map_err(ApiError::internal)?;
    Ok(Json(controller.response(&state.config.vnc_ws_url, false)))
}

async fn start_session(
    State(state): State<AppState>,
    Json(input): Json<StartSessionRequest>,
) -> Result<Json<SessionResponse>, ApiError> {
    let account_id = validate_account_id(&input.account_id).map_err(ApiError::bad_request)?;
    let target_url = validate_target_url(&input.url).map_err(ApiError::bad_request)?;
    let profile_dir =
        prepare_profile(&state.config.profile_root, &account_id).map_err(ApiError::internal)?;

    let mut controller = state.controller.lock().await;
    controller.reap_exited().map_err(ApiError::internal)?;
    let switched = controller
        .start_or_open(
            &state.config,
            &account_id,
            &profile_dir,
            target_url.as_str(),
        )
        .await
        .map_err(ApiError::internal)?;

    Ok(Json(
        controller.response(&state.config.vnc_ws_url, switched),
    ))
}

async fn stop_session(
    State(state): State<AppState>,
    input: Option<Json<StopSessionRequest>>,
) -> Result<Json<SessionResponse>, ApiError> {
    let expected_account = input.and_then(|Json(input)| input.account_id);
    if let Some(account_id) = expected_account.as_deref() {
        validate_account_id(account_id).map_err(ApiError::bad_request)?;
    }

    let mut controller = state.controller.lock().await;
    controller.reap_exited().map_err(ApiError::internal)?;
    if let (Some(expected), Some(current)) = (expected_account.as_deref(), &controller.current)
        && expected != current.account_id
    {
        return Err(ApiError::conflict(
            "another account owns the active browser session",
        ));
    }
    controller
        .stop(state.config.shutdown_timeout)
        .await
        .map_err(ApiError::internal)?;
    Ok(Json(controller.response(&state.config.vnc_ws_url, false)))
}

impl BrowserController {
    async fn start_or_open(
        &mut self,
        config: &Config,
        account_id: &str,
        profile_dir: &Path,
        target_url: &str,
    ) -> Result<bool> {
        if self
            .current
            .as_ref()
            .is_some_and(|session| session.account_id == account_id)
        {
            open_new_tab(config, profile_dir, target_url).await?;
            return Ok(false);
        }

        let switched = self.current.is_some();
        self.stop(config.shutdown_timeout).await?;
        self.reap_retired()?;
        if self
            .retired
            .iter()
            .any(|session| session.profile_dir == profile_dir)
        {
            bail!(
                "browser profile is still in use by a shutting-down Chromium process; retry shortly"
            );
        }
        let child = spawn_browser(config, profile_dir, target_url, false)?;
        #[cfg(unix)]
        let process_group_id = child.id();
        self.current = Some(BrowserSession {
            account_id: account_id.to_string(),
            child,
            profile_dir: profile_dir.to_path_buf(),
            #[cfg(unix)]
            process_group_id,
            started_at_unix_ms: now_unix_ms(),
        });

        tokio::time::sleep(Duration::from_millis(300)).await;
        self.reap_exited()?;
        if self.current.is_none() {
            bail!("Chromium exited during startup");
        }
        Ok(switched)
    }

    async fn stop(&mut self, graceful_timeout: Duration) -> Result<()> {
        let Some(mut session) = self.current.take() else {
            return Ok(());
        };

        terminate_child(&mut session.child, graceful_timeout).await?;
        self.retire(session);
        self.reap_retired()
    }

    fn reap_exited(&mut self) -> Result<()> {
        self.reap_retired()?;
        let exited = match self.current.as_mut() {
            Some(session) => session
                .child
                .try_wait()
                .context("failed to inspect Chromium process")?
                .is_some(),
            None => false,
        };
        if exited {
            let session = self
                .current
                .take()
                .expect("exited browser session must still be present");
            self.retire(session);
            self.reap_retired()?;
        }
        Ok(())
    }

    fn retire(&mut self, session: BrowserSession) {
        self.retired.push(RetiredBrowserSession {
            profile_dir: session.profile_dir,
            #[cfg(unix)]
            process_group_id: session.process_group_id,
        });
    }

    fn reap_retired(&mut self) -> Result<()> {
        let mut retired = std::mem::take(&mut self.retired);
        while let Some(session) = retired.pop() {
            if !profile_process_group_has_exited(&session)? {
                self.retired.push(session);
                continue;
            }
            if let Err(error) = remove_owned_profile_locks(&session.profile_dir) {
                self.retired.push(session);
                self.retired.append(&mut retired);
                return Err(error);
            }
        }
        Ok(())
    }

    fn response(&self, vnc_ws_url: &str, switched: bool) -> SessionResponse {
        match &self.current {
            Some(session) => SessionResponse {
                status: "running",
                active: true,
                account_id: Some(session.account_id.clone()),
                started_at_unix_ms: Some(session.started_at_unix_ms),
                switched,
                vnc_ws_url: vnc_ws_url.to_string(),
            },
            None => SessionResponse {
                status: "idle",
                active: false,
                account_id: None,
                started_at_unix_ms: None,
                switched,
                vnc_ws_url: vnc_ws_url.to_string(),
            },
        }
    }
}

#[cfg(unix)]
fn profile_process_group_has_exited(session: &RetiredBrowserSession) -> Result<bool> {
    use nix::{errno::Errno, sys::signal::kill, unistd::Pid};

    let Some(process_group_id) = session.process_group_id else {
        return Ok(false);
    };
    let process_group_id = i32::try_from(process_group_id)
        .context("Chromium process group id exceeded the supported range")?;
    match kill(Pid::from_raw(-process_group_id), None) {
        Ok(()) | Err(Errno::EPERM) => {
            #[cfg(target_os = "linux")]
            return Ok(!process_group_has_live_members(process_group_id)?);
            #[cfg(not(target_os = "linux"))]
            Ok(false)
        }
        Err(Errno::ESRCH) => Ok(true),
        Err(error) => Err(error).context("failed to inspect Chromium process group"),
    }
}

#[cfg(target_os = "linux")]
fn process_group_has_live_members(process_group_id: i32) -> Result<bool> {
    for entry in fs::read_dir("/proc").context("failed to inspect Linux process table")? {
        let entry = entry?;
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() else {
            continue;
        };
        let stat = match fs::read_to_string(format!("/proc/{pid}/stat")) {
            Ok(stat) => stat,
            Err(error) if error.kind() == ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        let Some((_, fields)) = stat.rsplit_once(')') else {
            continue;
        };
        let mut fields = fields.split_whitespace();
        let Some(state) = fields.next() else {
            continue;
        };
        let _parent_pid = fields.next();
        let Some(group_id) = fields.next() else {
            continue;
        };
        if group_id == process_group_id.to_string() && !matches!(state, "Z" | "X") {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(not(unix))]
fn profile_process_group_has_exited(_session: &RetiredBrowserSession) -> Result<bool> {
    Ok(true)
}

fn spawn_browser(
    config: &Config,
    profile_dir: &Path,
    target_url: &str,
    new_tab: bool,
) -> Result<Child> {
    let mut command = tokio::process::Command::new(&config.browser_binary);
    command
        .args(browser_args(profile_dir, target_url, new_tab))
        .env("DISPLAY", &config.display)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .kill_on_drop(true);

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.as_std_mut().process_group(0);
    }

    command
        .spawn()
        .with_context(|| format!("failed to start {}", config.browser_binary.display()))
}

fn browser_args(profile_dir: &Path, target_url: &str, new_tab: bool) -> Vec<String> {
    let mut args = vec![
        format!("--user-data-dir={}", profile_dir.display()),
        "--no-first-run".into(),
        "--no-default-browser-check".into(),
        // Keep the sidecar profile self-contained; it has no desktop keyring.
        "--password-store=basic".into(),
        "--ozone-platform=x11".into(),
        "--window-size=1440,900".into(),
    ];
    args.push(if new_tab {
        "--new-tab".into()
    } else {
        "--new-window".into()
    });
    args.push(target_url.to_string());
    args
}

async fn open_new_tab(config: &Config, profile_dir: &Path, target_url: &str) -> Result<()> {
    let mut child = spawn_browser(config, profile_dir, target_url, true)?;
    match timeout(Duration::from_secs(10), child.wait()).await {
        Ok(result) => {
            let status = result.context("failed to wait for Chromium tab opener")?;
            if status.success() {
                Ok(())
            } else {
                bail!("Chromium tab opener exited with {status}")
            }
        }
        Err(_) => {
            terminate_child(&mut child, Duration::from_secs(1)).await?;
            bail!("Chromium did not hand the new tab to the active browser")
        }
    }
}

async fn terminate_child(child: &mut Child, graceful_timeout: Duration) -> Result<()> {
    if child
        .try_wait()
        .context("failed to inspect Chromium before shutdown")?
        .is_some()
    {
        return Ok(());
    }

    #[cfg(unix)]
    if let Some(pid) = child.id() {
        use nix::{
            sys::signal::{Signal, killpg},
            unistd::Pid,
        };
        if let Err(error) = killpg(Pid::from_raw(pid as i32), Signal::SIGTERM)
            && error != nix::errno::Errno::ESRCH
        {
            return Err(error).context("failed to send SIGTERM to Chromium");
        }
    }

    #[cfg(not(unix))]
    child
        .start_kill()
        .context("failed to request Chromium shutdown")?;

    match timeout(graceful_timeout, child.wait()).await {
        Ok(result) => {
            result.context("failed to wait for Chromium shutdown")?;
            Ok(())
        }
        Err(_) => {
            #[cfg(unix)]
            if let Some(pid) = child.id() {
                use nix::{
                    sys::signal::{Signal, killpg},
                    unistd::Pid,
                };
                let _ = killpg(Pid::from_raw(pid as i32), Signal::SIGKILL);
            }
            child
                .kill()
                .await
                .context("failed to force Chromium shutdown")?;
            Ok(())
        }
    }
}

fn validate_account_id(value: &str) -> std::result::Result<String, String> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err("account_id contains unsafe path characters".into());
    }
    Ok(value.to_string())
}

fn validate_target_url(value: &str) -> std::result::Result<Url, String> {
    let value = value.trim();
    if value.is_empty() || value.len() > MAX_URL_LEN {
        return Err("url must contain between 1 and 2048 characters".into());
    }
    let parsed = Url::parse(value).map_err(|_| "url must be an absolute HTTPS URL".to_string())?;
    if parsed.scheme() != "https" {
        return Err("url must use HTTPS".into());
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err("url must not contain credentials".into());
    }
    match parsed.host_str() {
        Some(
            "accounts.google.com"
            | "github.com"
            | "opencode.ai"
            | "console.opencode.ai"
            | "auth.opencode.ai",
        ) => Ok(parsed),
        _ => Err("url host is not allowed".into()),
    }
}

fn validate_vnc_ws_url(value: &str) -> Result<String> {
    let parsed = Url::parse(value).context("invalid OCG_BROWSER_VNC_WS_URL")?;
    if !matches!(parsed.scheme(), "ws" | "wss")
        || parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
    {
        bail!("OCG_BROWSER_VNC_WS_URL must be a ws:// or wss:// URL without credentials");
    }
    Ok(parsed.to_string())
}

fn prepare_profile(root: &Path, account_id: &str) -> Result<PathBuf> {
    ensure_safe_directory(root, 0o700)?;
    let root = root
        .canonicalize()
        .with_context(|| format!("failed to resolve profile root {}", root.display()))?;
    let profile = root.join(account_id);
    ensure_safe_directory(&profile, 0o700)?;
    let profile = profile
        .canonicalize()
        .with_context(|| format!("failed to resolve profile {}", profile.display()))?;
    if profile.parent() != Some(root.as_path()) {
        bail!("profile path escaped its configured root");
    }
    Ok(profile)
}

fn ensure_safe_directory(path: &Path, _mode: u32) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                bail!("{} must be a real directory", path.display());
            }
        }
        Err(error) if error.kind() == ErrorKind::NotFound => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                let mut builder = fs::DirBuilder::new();
                builder.recursive(true).mode(_mode).create(path)?;
            }
            #[cfg(not(unix))]
            fs::create_dir_all(path)?;
        }
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

fn remove_owned_profile_locks(profile: &Path) -> Result<()> {
    for marker in ["SingletonLock", "SingletonSocket", "SingletonCookie"] {
        let path = profile.join(marker);
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
                bail!(
                    "browser lock marker {} is unexpectedly a directory",
                    path.display()
                );
            }
            Ok(_) => fs::remove_file(&path).with_context(|| {
                format!("failed to remove stale browser lock {}", path.display())
            })?,
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn recover_stale_chromium_profile_locks(root: &Path) -> Result<()> {
    for entry in fs::read_dir(root)
        .with_context(|| format!("failed to inspect browser profile root {}", root.display()))?
    {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let account_id = entry.file_name();
        let Some(account_id) = account_id.to_str() else {
            continue;
        };
        if file_type.is_symlink() || !file_type.is_dir() || validate_account_id(account_id).is_err()
        {
            continue;
        }
        recover_stale_chromium_profile_lock(&entry.path())?;
    }
    Ok(())
}

fn recover_stale_chromium_profile_lock(profile: &Path) -> Result<()> {
    let lock_path = profile.join("SingletonLock");
    let target = match fs::read_link(&lock_path) {
        Ok(target) => target,
        Err(error)
            if error.kind() == ErrorKind::NotFound || error.kind() == ErrorKind::InvalidInput =>
        {
            return Ok(());
        }
        Err(error) => return Err(error.into()),
    };
    let Some(owner_pid) = chromium_lock_owner_pid(&target) else {
        return Ok(());
    };
    if chromium_process_is_running(owner_pid)? {
        return Ok(());
    }
    remove_owned_profile_locks(profile)
}

fn chromium_lock_owner_pid(target: &Path) -> Option<u32> {
    target
        .file_name()?
        .to_str()?
        .rsplit_once('-')?
        .1
        .parse()
        .ok()
}

#[cfg(unix)]
fn chromium_process_is_running(pid: u32) -> Result<bool> {
    use nix::{errno::Errno, sys::signal::kill, unistd::Pid};

    let pid = i32::try_from(pid).context("Chromium lock owner PID exceeded the supported range")?;
    if pid <= 0 {
        return Ok(true);
    }
    match kill(Pid::from_raw(pid), None) {
        Ok(()) | Err(Errno::EPERM) => Ok(true),
        Err(Errno::ESRCH) => Ok(false),
        Err(error) => Err(error).context("failed to inspect Chromium lock owner"),
    }
}

#[cfg(not(unix))]
fn chromium_process_is_running(_pid: u32) -> Result<bool> {
    Ok(true)
}

fn ensure_control_token(path: &Path) -> Result<String> {
    if let Some(parent) = path.parent() {
        ensure_safe_directory(parent, 0o700)?;
    }
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                bail!("control token path must be a regular file");
            }
            return read_and_validate_token(path);
        }
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }

    let token = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("control token path has no parent"))?;
    let temporary = parent.join(format!(".control-token-{}", Uuid::new_v4().simple()));
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temporary)?;
    file.write_all(token.as_bytes())?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    drop(file);
    match fs::hard_link(&temporary, path) {
        Ok(()) => {
            fs::remove_file(&temporary)?;
        }
        Err(error) => {
            let _ = fs::remove_file(&temporary);
            if error.kind() == ErrorKind::AlreadyExists {
                return read_and_validate_token(path);
            }
            return Err(error.into());
        }
    }
    Ok(token)
}

fn read_and_validate_token(path: &Path) -> Result<String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(path)?.permissions().mode();
        if mode & 0o077 != 0 {
            bail!("control token file must not be accessible by group or other users");
        }
    }
    let token = fs::read_to_string(path)?;
    let token = token.trim();
    if token.len() != 64
        || !token
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        bail!("control token must contain exactly 64 lowercase hexadecimal characters");
    }
    Ok(token.to_string())
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0_u8, |difference, (left, right)| {
            difference | (left ^ right)
        })
        == 0
}

fn now_unix_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

async fn browser_services_ready(config: &Config) -> bool {
    let display_number = config.display.trim_start_matches(':').split('.').next();
    let display_ready = display_number
        .filter(|value| value.bytes().all(|byte| byte.is_ascii_digit()))
        .map(|value| PathBuf::from(format!("/tmp/.X11-unix/X{value}")))
        .is_some_and(|path| path.exists());
    if !display_ready {
        return false;
    }

    let vnc = tokio::net::TcpStream::connect("127.0.0.1:5900");
    let websocket = tokio::net::TcpStream::connect("127.0.0.1:6080");
    matches!(
        timeout(Duration::from_secs(1), async {
            tokio::try_join!(vnc, websocket)
        })
        .await,
        Ok(Ok(_))
    )
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut terminate = signal(SignalKind::terminate()).expect("install SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {},
            _ = terminate.recv() => {},
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

#[cfg(test)]
mod tests;

//! Owned Chromium-family windows for dashboard browser profiles.
//!
//! The launcher registers `core.browser` native hooks. It starts only the
//! child it can track, in a dedicated profile under this data directory.
//! Shutdown signals that process group and then removes Singleton* files
//! under those profile paths. It does not search for or signal a user's
//! already-running browser, and it does not read credential stores.

use ocg_core::browser::browser_profile_paths;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::env;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

#[derive(Debug, Default)]
struct BrowserProcessState {
    children: HashMap<String, Vec<Child>>,
}

static OWNED: std::sync::LazyLock<Mutex<HashMap<PathBuf, Arc<Mutex<BrowserProcessState>>>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));

pub fn register(state: &ocg_core::state::CoreStateInner) -> anyhow::Result<()> {
    register_discovered(state, discover_browser().is_some())
}

#[cfg(test)]
pub(crate) fn chromium_executable_available() -> bool {
    discover_browser().is_some()
}

/// Install hooks only when a Chromium-family executable exists.
///
/// No executable leaves a configured remote worker in place. It does not
/// replace that worker with an unavailable reason.
pub(crate) fn register_discovered(
    state: &ocg_core::state::CoreStateInner,
    executable_present: bool,
) -> anyhow::Result<()> {
    if !executable_present {
        return Ok(());
    }
    let data_dir = state.data_dir();
    let processes = shared_processes(&data_dir);
    let launch_dir = data_dir.clone();
    let launch_processes = Arc::clone(&processes);
    let stop_dir = data_dir;
    state.browser.register_native_hooks(
        Arc::new(move |account_id, url| {
            open_external_browser(
                launch_dir.clone(),
                Arc::clone(&launch_processes),
                account_id,
                url,
            )
            .map(|_| ())
            .map_err(anyhow::Error::msg)
        }),
        Arc::new(move |account_id| {
            stop_external_browser(&processes, account_id, Some(stop_dir.as_path()))
                .map_err(anyhow::Error::msg)
        }),
    )
}

pub fn close_owned(data_dir: &Path) {
    let processes = OWNED.lock().get(data_dir).cloned();
    if let Some(processes) = processes {
        let _ = close_all_browser_processes(&processes, Some(data_dir));
    }
}

fn shared_processes(data_dir: &Path) -> Arc<Mutex<BrowserProcessState>> {
    OWNED
        .lock()
        .entry(data_dir.to_path_buf())
        .or_insert_with(|| Arc::new(Mutex::new(BrowserProcessState::default())))
        .clone()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BrowserKind {
    Edge,
    Chrome,
    Chromium,
}

impl BrowserKind {
    fn display_name(self) -> &'static str {
        match self {
            Self::Edge => "Microsoft Edge",
            Self::Chrome => "Google Chrome",
            Self::Chromium => "Chromium",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BrowserExecutable {
    kind: BrowserKind,
    path: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BrowserPlatform {
    Windows,
    Macos,
    Linux,
}

#[derive(Debug, Default)]
struct BrowserEnvironment {
    program_files: Option<PathBuf>,
    program_files_x86: Option<PathBuf>,
    local_app_data: Option<PathBuf>,
    home: Option<PathBuf>,
    path_entries: Vec<PathBuf>,
}

impl BrowserEnvironment {
    fn current() -> Self {
        Self {
            program_files: env::var_os("ProgramFiles").map(PathBuf::from),
            program_files_x86: env::var_os("ProgramFiles(x86)").map(PathBuf::from),
            local_app_data: env::var_os("LOCALAPPDATA").map(PathBuf::from),
            home: env::var_os("HOME")
                .or_else(|| env::var_os("USERPROFILE"))
                .map(PathBuf::from),
            path_entries: env::var_os("PATH")
                .map(|value| env::split_paths(&value).collect())
                .unwrap_or_default(),
        }
    }
}

fn open_external_browser(
    data_dir: PathBuf,
    processes: Arc<Mutex<BrowserProcessState>>,
    account_id: &str,
    url: &str,
) -> Result<String, String> {
    validate_account_id(account_id)?;
    validate_browser_url(url)?;

    let executable = discover_browser().ok_or_else(browser_install_error)?;
    let profile_dir = prepare_native_profile_dir(&data_dir, account_id)?;

    let mut command = Command::new(&executable.path);
    command
        .args(browser_arguments(&profile_dir, url))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command.spawn().map_err(|error| {
        format!(
            "failed to start {} ({}): {error}",
            executable.kind.display_name(),
            executable.path.display()
        )
    })?;
    thread::sleep(Duration::from_millis(300));
    if let Some(status) = child.try_wait().map_err(|error| {
        format!(
            "failed to inspect {} startup: {error}",
            executable.kind.display_name()
        )
    })? {
        if status.success() {
            return Ok(url.to_string());
        }
        return Err(format!(
            "{} exited during startup with {status}; reinstall or start the browser manually and retry",
            executable.kind.display_name()
        ));
    }

    let mut processes = processes.lock();
    reap_finished_processes(&mut processes);
    processes
        .children
        .entry(account_id.to_string())
        .or_default()
        .push(child);
    Ok(url.to_string())
}

fn prepare_native_profile_dir(data_dir: &Path, account_id: &str) -> Result<PathBuf, String> {
    let paths = browser_profile_paths(data_dir, account_id).map_err(|error| error.to_string())?;
    prepare_native_profile_dir_from_paths(paths)
}

fn prepare_native_profile_dir_from_paths(paths: Vec<PathBuf>) -> Result<PathBuf, String> {
    let profile_dir = paths
        .into_iter()
        .next()
        .ok_or_else(|| "browser profile root is unavailable".to_string())?;
    std::fs::create_dir_all(&profile_dir).map_err(|error| {
        format!(
            "failed to create browser profile {}: {error}",
            profile_dir.display()
        )
    })?;
    Ok(profile_dir)
}

fn close_all_browser_processes(
    processes: &Arc<Mutex<BrowserProcessState>>,
    data_dir: Option<&Path>,
) -> Result<(), String> {
    let mut processes = processes.lock();
    let mut errors = Vec::new();
    let mut finished_accounts = Vec::new();
    for (account_id, children) in &mut processes.children {
        terminate_children(children, &mut errors);
        if children.is_empty() {
            match data_dir.map(|data_dir| remove_owned_profile_locks(data_dir, account_id)) {
                Some(Err(error)) => errors.push(error),
                Some(Ok(())) | None => finished_accounts.push(account_id.clone()),
            }
        }
    }
    for account_id in finished_accounts {
        processes.children.remove(&account_id);
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

fn stop_external_browser(
    processes: &Arc<Mutex<BrowserProcessState>>,
    account_id: &str,
    data_dir: Option<&Path>,
) -> Result<(), String> {
    validate_account_id(account_id)?;
    let mut processes = processes.lock();
    let Some(children) = processes.children.get_mut(account_id) else {
        return Ok(());
    };
    let mut errors = Vec::new();
    terminate_children(children, &mut errors);
    if children.is_empty()
        && let Some(data_dir) = data_dir
        && let Err(error) = remove_owned_profile_locks(data_dir, account_id)
    {
        errors.push(error);
    }
    if children.is_empty() && errors.is_empty() {
        processes.children.remove(account_id);
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

fn remove_owned_profile_locks(data_dir: &Path, account_id: &str) -> Result<(), String> {
    let paths = browser_profile_paths(data_dir, account_id).map_err(|error| error.to_string())?;
    remove_owned_profile_locks_at_paths(paths)
}

fn remove_owned_profile_locks_at_paths(paths: Vec<PathBuf>) -> Result<(), String> {
    for profile in paths {
        for marker in ["SingletonLock", "SingletonSocket", "SingletonCookie"] {
            let path = profile.join(marker);
            match std::fs::symlink_metadata(&path) {
                Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
                    return Err(format!(
                        "browser lock marker {} is unexpectedly a directory",
                        path.display()
                    ));
                }
                Ok(_) => std::fs::remove_file(&path).map_err(|error| {
                    format!(
                        "failed to remove stale browser lock {}: {error}",
                        path.display()
                    )
                })?,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(format!(
                        "failed to inspect browser lock {}: {error}",
                        path.display()
                    ));
                }
            }
        }
    }
    Ok(())
}

fn terminate_children(children: &mut Vec<Child>, errors: &mut Vec<String>) {
    let mut remaining = Vec::new();
    for mut child in children.drain(..) {
        match child.try_wait() {
            Ok(Some(_)) => continue,
            Ok(None) => {
                if let Err(error) = terminate_browser_process(&mut child) {
                    errors.push(format!(
                        "failed to close browser process {} cleanly: {error}",
                        child.id()
                    ));
                    remaining.push(child);
                }
            }
            Err(error) => {
                errors.push(format!(
                    "failed to inspect browser process {}: {error}",
                    child.id()
                ));
                remaining.push(child);
            }
        }
    }
    *children = remaining;
}

fn terminate_browser_process(child: &mut Child) -> Result<(), String> {
    let graceful_error = request_graceful_browser_exit(child).err();
    if graceful_error.is_none() && wait_for_child_exit(child, Duration::from_secs(10))? {
        return Ok(());
    }

    force_browser_exit(child).map_err(|force_error| match graceful_error {
        Some(graceful_error) => {
            format!("{graceful_error}; forced shutdown also failed: {force_error}")
        }
        None => force_error,
    })?;
    if wait_for_child_exit(child, Duration::from_secs(5))? {
        Ok(())
    } else {
        Err("browser did not exit after the graceful and forced shutdown timeouts".into())
    }
}

fn wait_for_child_exit(child: &mut Child, timeout: Duration) -> Result<bool, String> {
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return Ok(true),
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(100)),
            Ok(None) => return Ok(false),
            Err(error) => return Err(format!("failed to inspect browser shutdown: {error}")),
        }
    }
}

fn refuse_unowned_pid(pid: u32) -> Result<(), String> {
    if pid == 0 {
        Err("refusing to signal process 0".into())
    } else {
        Ok(())
    }
}

#[cfg(unix)]
fn request_graceful_browser_exit(child: &mut Child) -> Result<(), String> {
    refuse_unowned_pid(child.id())?;
    crate::process_alive::signal_process_group(child.id(), false)
}

#[cfg(windows)]
fn request_graceful_browser_exit(child: &mut Child) -> Result<(), String> {
    refuse_unowned_pid(child.id())?;
    let mut command = Command::new("taskkill");
    use std::os::windows::process::CommandExt;
    command.creation_flags(0x0800_0000);
    let status = command
        .args(["/PID", &child.id().to_string(), "/T"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|error| format!("failed to run taskkill: {error}"))?;
    if status.success() || child.try_wait().ok().flatten().is_some() {
        Ok(())
    } else {
        Err(format!("taskkill returned {status}"))
    }
}

#[cfg(not(any(unix, windows)))]
fn request_graceful_browser_exit(child: &mut Child) -> Result<(), String> {
    refuse_unowned_pid(child.id())?;
    child
        .kill()
        .map_err(|error| format!("failed to request browser shutdown: {error}"))
}

#[cfg(unix)]
fn force_browser_exit(child: &mut Child) -> Result<(), String> {
    refuse_unowned_pid(child.id())?;
    crate::process_alive::signal_process_group(child.id(), true)
}

#[cfg(windows)]
fn force_browser_exit(child: &mut Child) -> Result<(), String> {
    refuse_unowned_pid(child.id())?;
    let mut command = Command::new("taskkill");
    use std::os::windows::process::CommandExt;
    command.creation_flags(0x0800_0000);
    let status = command
        .args(["/PID", &child.id().to_string(), "/T", "/F"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    finish_windows_forced_exit(child, status)
}

#[cfg(windows)]
fn finish_windows_forced_exit(
    child: &mut Child,
    taskkill: std::io::Result<std::process::ExitStatus>,
) -> Result<(), String> {
    if taskkill.as_ref().is_ok_and(|status| status.success())
        || child
            .try_wait()
            .map_err(|error| format!("inspect owned browser: {error}"))?
            .is_some()
    {
        return Ok(());
    }
    // The spawning process already owns a process handle. A restricted token
    // may refuse taskkill's fresh process lookup while this handle can still
    // terminate our child. Never open or signal an unrelated process here.
    child.kill().map_err(|error| {
        let taskkill = match taskkill {
            Ok(status) => status.to_string(),
            Err(error) => error.to_string(),
        };
        format!("taskkill failed ({taskkill}); owned browser handle shutdown failed: {error}")
    })
}

#[cfg(not(any(unix, windows)))]
fn force_browser_exit(child: &mut Child) -> Result<(), String> {
    refuse_unowned_pid(child.id())?;
    child
        .kill()
        .map_err(|error| format!("failed to force browser shutdown: {error}"))
}

fn reap_finished_processes(processes: &mut BrowserProcessState) {
    processes.children.retain(|_, children| {
        children.retain_mut(|child| !matches!(child.try_wait(), Ok(Some(_))));
        !children.is_empty()
    });
}

fn validate_account_id(account_id: &str) -> Result<(), String> {
    ocg_core::browser::validate_account_id(account_id).map_err(|_| "invalid account id".to_string())
}

fn validate_browser_url(url: &str) -> Result<(), String> {
    let parsed = reqwest::Url::parse(url).map_err(|_| "invalid browser URL".to_string())?;
    if parsed.scheme() != "https" || parsed.host_str().is_none() {
        return Err("browser URL must be an absolute HTTPS URL".to_string());
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err("browser URL must not include credentials".to_string());
    }
    Ok(())
}

fn browser_arguments(profile_dir: &Path, url: &str) -> Vec<OsString> {
    let mut profile_argument = OsString::from("--user-data-dir=");
    profile_argument.push(profile_dir.as_os_str());
    vec![
        profile_argument,
        OsString::from("--no-first-run"),
        OsString::from("--no-default-browser-check"),
        OsString::from("--new-window"),
        OsString::from(url),
    ]
}

fn discover_browser() -> Option<BrowserExecutable> {
    let platform = if cfg!(target_os = "windows") {
        BrowserPlatform::Windows
    } else if cfg!(target_os = "macos") {
        BrowserPlatform::Macos
    } else {
        BrowserPlatform::Linux
    };
    discover_browser_with(
        platform,
        &BrowserEnvironment::current(),
        is_browser_executable,
    )
}

fn is_browser_executable(path: &Path) -> bool {
    let Ok(metadata) = path.metadata() else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

fn discover_browser_with(
    platform: BrowserPlatform,
    environment: &BrowserEnvironment,
    exists: impl Fn(&Path) -> bool,
) -> Option<BrowserExecutable> {
    browser_candidates(platform, environment)
        .into_iter()
        .find(|candidate| exists(&candidate.path))
}

fn browser_candidates(
    platform: BrowserPlatform,
    environment: &BrowserEnvironment,
) -> Vec<BrowserExecutable> {
    let mut candidates = Vec::new();
    match platform {
        BrowserPlatform::Windows => {
            for base in [
                &environment.program_files_x86,
                &environment.program_files,
                &environment.local_app_data,
            ] {
                push_under(
                    &mut candidates,
                    BrowserKind::Edge,
                    base,
                    "Microsoft/Edge/Application/msedge.exe",
                );
            }
            push_path_names(
                &mut candidates,
                BrowserKind::Edge,
                &environment.path_entries,
                &["msedge.exe"],
            );
            for base in [
                &environment.program_files,
                &environment.program_files_x86,
                &environment.local_app_data,
            ] {
                push_under(
                    &mut candidates,
                    BrowserKind::Chrome,
                    base,
                    "Google/Chrome/Application/chrome.exe",
                );
            }
            push_path_names(
                &mut candidates,
                BrowserKind::Chrome,
                &environment.path_entries,
                &["chrome.exe"],
            );
        }
        BrowserPlatform::Macos => {
            push_candidate(
                &mut candidates,
                BrowserKind::Chrome,
                PathBuf::from("/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"),
            );
            push_under(
                &mut candidates,
                BrowserKind::Chrome,
                &environment.home,
                "Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
            );
            push_path_names(
                &mut candidates,
                BrowserKind::Chrome,
                &environment.path_entries,
                &["google-chrome", "chrome"],
            );
            push_candidate(
                &mut candidates,
                BrowserKind::Edge,
                PathBuf::from("/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge"),
            );
            push_under(
                &mut candidates,
                BrowserKind::Edge,
                &environment.home,
                "Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
            );
            push_path_names(
                &mut candidates,
                BrowserKind::Edge,
                &environment.path_entries,
                &["microsoft-edge"],
            );
            push_candidate(
                &mut candidates,
                BrowserKind::Chromium,
                PathBuf::from("/Applications/Chromium.app/Contents/MacOS/Chromium"),
            );
            push_under(
                &mut candidates,
                BrowserKind::Chromium,
                &environment.home,
                "Applications/Chromium.app/Contents/MacOS/Chromium",
            );
            push_path_names(
                &mut candidates,
                BrowserKind::Chromium,
                &environment.path_entries,
                &["chromium", "chromium-browser"],
            );
        }
        BrowserPlatform::Linux => {
            push_path_names(
                &mut candidates,
                BrowserKind::Chrome,
                &environment.path_entries,
                &["google-chrome", "google-chrome-stable", "chrome"],
            );
            push_path_names(
                &mut candidates,
                BrowserKind::Chromium,
                &environment.path_entries,
                &["chromium", "chromium-browser"],
            );
            push_path_names(
                &mut candidates,
                BrowserKind::Edge,
                &environment.path_entries,
                &["microsoft-edge", "microsoft-edge-stable"],
            );
        }
    }
    candidates
}

fn push_candidate(candidates: &mut Vec<BrowserExecutable>, kind: BrowserKind, path: PathBuf) {
    candidates.push(BrowserExecutable { kind, path });
}

fn push_under(
    candidates: &mut Vec<BrowserExecutable>,
    kind: BrowserKind,
    base: &Option<PathBuf>,
    suffix: &str,
) {
    if let Some(base) = base {
        push_candidate(candidates, kind, base.join(suffix));
    }
}

fn push_path_names(
    candidates: &mut Vec<BrowserExecutable>,
    kind: BrowserKind,
    path_entries: &[PathBuf],
    names: &[&str],
) {
    for name in names {
        for directory in path_entries {
            push_candidate(candidates, kind, directory.join(name));
        }
    }
}

fn browser_install_error() -> String {
    if cfg!(target_os = "windows") {
        "Microsoft Edge or Google Chrome is required; install one and retry".to_string()
    } else if cfg!(target_os = "macos") {
        "Google Chrome, Microsoft Edge, or Chromium is required; install one and retry".to_string()
    } else {
        "Google Chrome, Chromium, or Microsoft Edge was not found in PATH; install one and retry"
            .to_string()
    }
}

#[cfg(test)]
fn delete_browser_profile_dirs_at_paths(paths: Vec<PathBuf>) -> Result<(), String> {
    for profile_dir in paths {
        if profile_dir.exists() {
            std::fs::remove_dir_all(&profile_dir).map_err(|error| {
                format!(
                    "failed to remove browser profile {}: {error}",
                    profile_dir.display()
                )
            })?;
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "native_browser/tests.rs"]
mod tests;

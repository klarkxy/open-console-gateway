//! Native-host implementation of the DSH application installer.
//!
//! This module is deliberately DSH-specific. It materializes one immutable,
//! app-owned plugin source, invokes the official `dsh plugin` command with an
//! argument array, and hands the selected Gateway Key to DSH through a private
//! one-time file. It never revives the retired generic Applications connector.
//! The Host writes and restores only the live credential-handoff; claim files
//! belong to the plugin.

use crate::dsh_application::{
    DshApplicationError, DshApplicationHostRequest, DshApplicationInspection, DshApplicationPhase,
    DshApplicationResult,
};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Read, Write};
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
#[cfg(not(windows))]
use std::process::{Command, Stdio};
#[cfg(not(windows))]
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

const PACKAGE_NAME: &str = "@open-console-gateway/dsh-plugin";
const PROFILE: &str = "web";
const PACKAGE_ROOT: &str = "applications/dsh/packages-v1";
const BOOTSTRAP_FILE: &str = "applications/dsh/credential-handoff";
const BOOTSTRAP_CLAIM_MARKER: &str = ".claimed-";
const GATEWAY_PLACEHOLDER: &str = "__OCG_GATEWAY_V1_URL__";
const BOOTSTRAP_PLACEHOLDER: &str = "__OCG_CREDENTIAL_BOOTSTRAP_PATH_JSON__";
const MAX_PACKAGE_FILES: usize = 16;
const MAX_PACKAGE_BYTES: u64 = 1024 * 1024;
const MAX_COMMAND_OUTPUT: usize = 16 * 1024;
const VERSION_TIMEOUT: Duration = Duration::from_secs(10);
const INSTALL_TIMEOUT: Duration = Duration::from_secs(120);
const RECONCILE_TIMEOUT: Duration = Duration::from_secs(2);

const PACKAGE_FILES: &[(&str, &str)] = &[
    (
        "package.json",
        include_str!("../../../integrations/dsh-plugin/package.json"),
    ),
    (
        "index.js",
        include_str!("../../../integrations/dsh-plugin/index.js"),
    ),
    (
        "cordis.patch.yml",
        include_str!("../../../integrations/dsh-plugin/cordis.patch.yml"),
    ),
    (
        "README.md",
        include_str!("../../../integrations/dsh-plugin/README.md"),
    ),
];

pub fn register(core: &crate::state::CoreState) {
    let home = std::env::var_os("DSH_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| user_home().join(".dsh"));
    let host = Arc::new(DshDesktopHost {
        data_dir: absolute_host_path(core.data_dir()),
        home: absolute_host_path(home),
        runner: Arc::new(ProcessCommandRunner),
        dsh_executable: None,
        operation: Mutex::new(()),
    });
    core.set_dsh_application_host(Arc::new(move |request| host.execute(request)));
}

fn absolute_host_path(path: PathBuf) -> PathBuf {
    if path.is_absolute() {
        return path;
    }
    std::env::current_dir()
        .map(|current| current.join(&path))
        .unwrap_or(path)
}

struct DshDesktopHost {
    data_dir: PathBuf,
    home: PathBuf,
    runner: Arc<dyn CommandRunner>,
    dsh_executable: Option<PathBuf>,
    operation: Mutex<()>,
}

impl DshDesktopHost {
    fn execute(
        &self,
        request: DshApplicationHostRequest,
    ) -> DshApplicationResult<DshApplicationInspection> {
        let _operation = self
            .operation
            .lock()
            .map_err(|_| internal("DSH application operation lock is poisoned"))?;
        match request {
            DshApplicationHostRequest::Inspect { gateway_v1_url } => self.inspect(&gateway_v1_url),
            DshApplicationHostRequest::Install {
                expected_fingerprint,
                gateway_v1_url,
                secret,
            } => self.install(
                &expected_fingerprint,
                &gateway_v1_url,
                secret.expose_to_host(),
            ),
        }
    }

    fn inspect(&self, gateway_v1_url: &str) -> DshApplicationResult<DshApplicationInspection> {
        let target_paths = self.target_paths();
        let Some(executable) = self.resolve_dsh_executable() else {
            return Ok(DshApplicationInspection {
                phase: DshApplicationPhase::NotDetected,
                detected: false,
                installed: false,
                install_supported: false,
                activation_required: false,
                version: None,
                detail: Some("DSH was not found on PATH".into()),
                target_paths,
                fingerprint: None,
            });
        };
        let version = self.read_version(&executable)?;
        if !compatible_version(&version) {
            return Ok(DshApplicationInspection {
                phase: DshApplicationPhase::Incompatible,
                detected: true,
                installed: false,
                install_supported: false,
                activation_required: false,
                version: Some(version.clone()),
                detail: Some(format!(
                    "DSH {version} is not a supported 0.1.5-rc.1 or 0.1.5-rc.2 build"
                )),
                target_paths,
                fingerprint: Some(self.fingerprint(&executable, &version, gateway_v1_url)?),
            });
        }

        let package = self.render_package(gateway_v1_url)?;
        let registration = self.registration_state(&package);
        let handoff_pending = credential_handoff_pending(&self.bootstrap_path());
        let fingerprint = Some(self.fingerprint(&executable, &version, gateway_v1_url)?);
        let (phase, installed, install_supported, detail) = match registration {
            RegistrationState::Absent => (
                DshApplicationPhase::Ready,
                false,
                true,
                Some("Ready to install the OCG provider into the DSH web profile".into()),
            ),
            RegistrationState::Exact if handoff_pending => (
                DshApplicationPhase::Installed,
                true,
                true,
                Some("Installed. Start or restart DSH once to import the selected Key".into()),
            ),
            RegistrationState::Exact => (
                DshApplicationPhase::Installed,
                true,
                true,
                Some("Installed. DSH refreshes the authenticated OCG model catalog on use".into()),
            ),
            RegistrationState::OwnedOlder(_) => (
                DshApplicationPhase::Ready,
                false,
                true,
                Some("An older OCG-managed DSH plugin is installed and can be updated".into()),
            ),
            RegistrationState::Conflict(detail) => {
                (DshApplicationPhase::Conflict, false, false, Some(detail))
            }
        };
        Ok(DshApplicationInspection {
            phase,
            detected: true,
            installed,
            install_supported,
            activation_required: installed && handoff_pending,
            version: Some(version),
            detail,
            target_paths,
            fingerprint,
        })
    }

    fn install(
        &self,
        expected_fingerprint: &str,
        gateway_v1_url: &str,
        secret: &str,
    ) -> DshApplicationResult<DshApplicationInspection> {
        if expected_fingerprint.is_empty() {
            return Err(DshApplicationError::invalid(
                "expectedFingerprint is required",
            ));
        }
        if secret.is_empty() || secret.contains(['\0', '\r', '\n']) {
            return Err(DshApplicationError::invalid(
                "the selected Gateway Key cannot be handed to DSH",
            ));
        }
        let before = self.inspect(gateway_v1_url)?;
        if before.fingerprint.as_deref() != Some(expected_fingerprint) {
            return Err(DshApplicationError::conflict(
                "DSH installation state changed after it was inspected",
            ));
        }
        if !before.install_supported {
            return Err(DshApplicationError::precondition(
                before
                    .detail
                    .unwrap_or_else(|| "DSH installation is unavailable".into()),
            ));
        }
        let executable = self
            .resolve_dsh_executable()
            .ok_or_else(|| DshApplicationError::precondition("DSH was not found on PATH"))?;
        let package = self.render_package(gateway_v1_url)?;
        let registration_before = self.registration_state(&package);
        if matches!(registration_before, RegistrationState::Conflict(_)) {
            return Err(DshApplicationError::conflict(
                "the DSH profile has a conflicting package with the OCG plugin name",
            ));
        }
        package.materialize()?;

        let bootstrap = self.bootstrap_path();
        let bootstrap_before = read_optional(&bootstrap)?;
        // Plugin apply() may rename this live file to a claim immediately.
        // The Host never deletes `.claimed-*` files: it cannot tell a stale
        // remnant from that in-flight claim.
        write_private_atomic(&self.data_dir, &bootstrap, secret.as_bytes())?;

        let command = CommandSpec {
            executable: executable.path.clone(),
            display_executable: executable.display.clone(),
            args: vec![
                OsString::from("plugin"),
                OsString::from("--profile"),
                OsString::from(PROFILE),
                OsString::from("add"),
                dsh_package_argument(&package.path),
                OsString::from("--config.auto-install-peers=true"),
            ],
            timeout: INSTALL_TIMEOUT,
        };
        let output = match self.runner.run(&command) {
            Ok(output) if output.success => output,
            Ok(output) => {
                restore_optional(&self.data_dir, &bootstrap, bootstrap_before.as_deref())?;
                self.restore_registration(&executable, &package, &registration_before)?;
                return Err(DshApplicationError::precondition(command_failure(
                    &command, &output,
                )));
            }
            Err(error) => {
                restore_optional(&self.data_dir, &bootstrap, bootstrap_before.as_deref())?;
                self.restore_registration(&executable, &package, &registration_before)?;
                return Err(DshApplicationError::precondition(error));
            }
        };
        drop(output);

        let started = Instant::now();
        loop {
            match self.registration_state(&package) {
                RegistrationState::Exact => break,
                RegistrationState::Conflict(detail) => {
                    restore_optional(&self.data_dir, &bootstrap, bootstrap_before.as_deref())?;
                    self.restore_registration(&executable, &package, &registration_before)?;
                    return Err(DshApplicationError::conflict(detail));
                }
                _ if started.elapsed() >= RECONCILE_TIMEOUT => {
                    restore_optional(&self.data_dir, &bootstrap, bootstrap_before.as_deref())?;
                    self.restore_registration(&executable, &package, &registration_before)?;
                    return Err(DshApplicationError::precondition(
                        "DSH finished the package command but did not register the exact OCG plugin source",
                    ));
                }
                _ => thread::sleep(Duration::from_millis(25)),
            }
        }
        self.inspect(gateway_v1_url)
    }

    fn restore_registration(
        &self,
        executable: &ResolvedExecutable,
        expected: &RenderedPackage,
        before: &RegistrationState,
    ) -> DshApplicationResult<()> {
        let current = self.registration_state(expected);
        if registration_matches(&current, before) {
            return Ok(());
        }
        if !matches!(current, RegistrationState::Exact) {
            return Err(DshApplicationError::conflict(
                "DSH registration changed again while the failed install was being restored; no external registration was modified",
            ));
        }
        let (action, target) = match before {
            RegistrationState::Absent => ("remove", OsString::from(PACKAGE_NAME)),
            RegistrationState::Exact => ("add", dsh_package_argument(&expected.path)),
            RegistrationState::OwnedOlder(source) => ("add", dsh_package_argument(source)),
            RegistrationState::Conflict(_) => {
                return Err(internal(
                    "cannot restore a conflicting DSH package registration",
                ));
            }
        };
        let command = CommandSpec {
            executable: executable.path.clone(),
            display_executable: executable.display.clone(),
            args: vec![
                OsString::from("plugin"),
                OsString::from("--profile"),
                OsString::from(PROFILE),
                OsString::from(action),
                target,
                OsString::from("--config.auto-install-peers=true"),
            ],
            timeout: INSTALL_TIMEOUT,
        };
        let output = self
            .runner
            .run(&command)
            .map_err(|error| internal(format!("failed to restore DSH registration: {error}")))?;
        if !output.success {
            return Err(internal(format!(
                "failed to restore DSH registration: {}",
                command_failure(&command, &output)
            )));
        }
        let started = Instant::now();
        loop {
            let current = self.registration_state(expected);
            if registration_matches(&current, before) {
                return Ok(());
            }
            if started.elapsed() >= RECONCILE_TIMEOUT {
                return Err(internal(
                    "DSH registration could not be restored after a failed install",
                ));
            }
            thread::sleep(Duration::from_millis(25));
        }
    }

    fn read_version(&self, executable: &ResolvedExecutable) -> DshApplicationResult<String> {
        let command = CommandSpec {
            executable: executable.path.clone(),
            display_executable: executable.display.clone(),
            args: vec![OsString::from("--version")],
            timeout: VERSION_TIMEOUT,
        };
        let output = self
            .runner
            .run(&command)
            .map_err(DshApplicationError::precondition)?;
        if !output.success {
            return Err(DshApplicationError::precondition(command_failure(
                &command, &output,
            )));
        }
        let version = output.stdout.trim();
        if version.is_empty() || version.contains(['\0', '\r', '\n']) {
            return Err(DshApplicationError::precondition(
                "DSH returned an invalid version",
            ));
        }
        Ok(version.to_string())
    }

    fn render_package(&self, gateway_v1_url: &str) -> DshApplicationResult<RenderedPackage> {
        if !valid_gateway_v1_url(gateway_v1_url) {
            return Err(DshApplicationError::invalid(
                "Gateway client URL cannot be used by the DSH plugin",
            ));
        }
        let bootstrap_json = serde_json::to_string(&self.bootstrap_path().to_string_lossy())
            .map_err(|error| internal(error.to_string()))?;
        let mut files = BTreeMap::new();
        for (relative, template) in PACKAGE_FILES {
            let relative = safe_relative_path(relative)?;
            let rendered = template
                .replace(GATEWAY_PLACEHOLDER, gateway_v1_url)
                .replace(BOOTSTRAP_PLACEHOLDER, &bootstrap_json);
            if rendered.contains(GATEWAY_PLACEHOLDER) || rendered.contains(BOOTSTRAP_PLACEHOLDER) {
                return Err(internal("DSH plugin package has an unresolved placeholder"));
            }
            files.insert(relative, rendered.into_bytes());
        }
        let digest = package_digest(&files);
        let trusted_root = self.data_dir.join(PACKAGE_ROOT);
        let path = trusted_root.join(&digest[..24]);
        Ok(RenderedPackage {
            trusted_root,
            path,
            files,
            digest,
        })
    }

    fn registration_state(&self, expected: &RenderedPackage) -> RegistrationState {
        let manifest = self
            .home
            .join("profiles")
            .join(PROFILE)
            .join("package.json");
        let content = match fs::read(&manifest) {
            Ok(content) => content,
            Err(error) if error.kind() == ErrorKind::NotFound => return RegistrationState::Absent,
            Err(error) => {
                return RegistrationState::Conflict(format!(
                    "could not read the DSH web profile: {error}"
                ));
            }
        };
        let value: Value = match serde_json::from_slice(&content) {
            Ok(value) => value,
            Err(_) => {
                return RegistrationState::Conflict(
                    "the DSH web profile manifest is not valid JSON".into(),
                );
            }
        };
        let dependency = value
            .get("dependencies")
            .and_then(Value::as_object)
            .and_then(|dependencies| dependencies.get(PACKAGE_NAME));
        let bundle = value
            .pointer("/dsh/profile/bundles")
            .and_then(Value::as_array)
            .is_some_and(|bundles| {
                bundles
                    .iter()
                    .any(|item| item.as_str() == Some(PACKAGE_NAME))
            });
        match (dependency, bundle) {
            (None, false) => RegistrationState::Absent,
            (Some(Value::String(spec)), true) => {
                let source = match dependency_source(spec, &manifest) {
                    Ok(source) => source,
                    Err(detail) => return RegistrationState::Conflict(detail),
                };
                if same_lexical_path(&source, &expected.path) && expected.exists_and_matches() {
                    RegistrationState::Exact
                } else if is_owned_package_source(&source, &expected.trusted_root) {
                    RegistrationState::OwnedOlder(source)
                } else {
                    RegistrationState::Conflict(
                        "DSH has a same-name package that is not an OCG-managed source".into(),
                    )
                }
            }
            _ => RegistrationState::Conflict(
                "DSH has only part of the OCG plugin registration".into(),
            ),
        }
    }

    fn fingerprint(
        &self,
        executable: &ResolvedExecutable,
        version: &str,
        gateway_v1_url: &str,
    ) -> DshApplicationResult<String> {
        let package = self.render_package(gateway_v1_url)?;
        let manifest = read_optional(
            &self
                .home
                .join("profiles")
                .join(PROFILE)
                .join("package.json"),
        )?;
        let bootstrap = read_optional(&self.bootstrap_path())?;
        let mut hash = Sha256::new();
        hash.update(b"open-console-gateway-dsh-install-v1\0");
        hash.update(executable.path.to_string_lossy().as_bytes());
        hash.update([0]);
        hash.update(version.as_bytes());
        hash.update([0]);
        hash.update(package.digest.as_bytes());
        hash.update([0]);
        hash.update(Sha256::digest(manifest.as_deref().unwrap_or_default()));
        hash.update([0]);
        hash.update(Sha256::digest(bootstrap.as_deref().unwrap_or_default()));
        Ok(format!("{:x}", hash.finalize()))
    }

    fn bootstrap_path(&self) -> PathBuf {
        self.data_dir.join(BOOTSTRAP_FILE)
    }

    fn target_paths(&self) -> Vec<String> {
        vec![
            self.home
                .join("profiles")
                .join(PROFILE)
                .join("package.json")
                .display()
                .to_string(),
            self.data_dir.join(PACKAGE_ROOT).display().to_string(),
            self.bootstrap_path().display().to_string(),
        ]
    }

    fn resolve_dsh_executable(&self) -> Option<ResolvedExecutable> {
        resolve_executable(
            self.dsh_executable
                .as_deref()
                .unwrap_or_else(|| Path::new("dsh")),
        )
    }
}

fn handoff_claim_prefix(path: &Path) -> Option<String> {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(|name| format!("{name}{BOOTSTRAP_CLAIM_MARKER}"))
}

fn handoff_claim_files(path: &Path) -> Vec<PathBuf> {
    let Some(parent) = path.parent() else {
        return Vec::new();
    };
    let Some(prefix) = handoff_claim_prefix(path) else {
        return Vec::new();
    };
    let Ok(entries) = fs::read_dir(parent) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .filter(|entry| {
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                return false;
            };
            name.starts_with(&prefix)
                && entry
                    .file_type()
                    .map(|kind| kind.is_file())
                    .unwrap_or(false)
        })
        .map(|entry| entry.path())
        .collect()
}

fn credential_handoff_pending(path: &Path) -> bool {
    path.exists() || !handoff_claim_files(path).is_empty()
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RegistrationState {
    Absent,
    Exact,
    OwnedOlder(PathBuf),
    Conflict(String),
}

fn registration_matches(left: &RegistrationState, right: &RegistrationState) -> bool {
    match (left, right) {
        (RegistrationState::Absent, RegistrationState::Absent)
        | (RegistrationState::Exact, RegistrationState::Exact) => true,
        (RegistrationState::OwnedOlder(left), RegistrationState::OwnedOlder(right)) => {
            same_lexical_path(left, right)
        }
        (RegistrationState::Conflict(left), RegistrationState::Conflict(right)) => left == right,
        _ => false,
    }
}

#[derive(Debug, Clone)]
struct RenderedPackage {
    trusted_root: PathBuf,
    path: PathBuf,
    files: BTreeMap<PathBuf, Vec<u8>>,
    digest: String,
}

impl RenderedPackage {
    fn exists_and_matches(&self) -> bool {
        safe_directory_chain(&self.trusted_root, &self.path)
            && package_files_from_disk(&self.path)
                .map(|actual| actual == self.files)
                .unwrap_or(false)
    }

    fn materialize(&self) -> DshApplicationResult<()> {
        let parent = self
            .path
            .parent()
            .ok_or_else(|| internal("invalid DSH plugin package directory"))?;
        ensure_safe_directory_chain(&self.trusted_root, parent)?;
        match fs::symlink_metadata(&self.path) {
            Ok(_) if self.exists_and_matches() => return Ok(()),
            Ok(_) => {
                return Err(DshApplicationError::conflict(
                    "the immutable DSH plugin package does not match its digest",
                ));
            }
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(internal(error.to_string())),
        }

        let temporary = parent.join(format!(
            ".ocg-dsh-package-{}.tmp",
            uuid::Uuid::new_v4().simple()
        ));
        fs::create_dir(&temporary).map_err(|error| internal(error.to_string()))?;
        struct TempDirectoryGuard(PathBuf);
        impl Drop for TempDirectoryGuard {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
        let guard = TempDirectoryGuard(temporary.clone());
        if !safe_directory_chain(&self.trusted_root, &temporary) {
            return Err(DshApplicationError::conflict(
                "the temporary DSH plugin package escaped its trusted root",
            ));
        }
        for (relative, bytes) in &self.files {
            let destination = temporary.join(relative);
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&destination)
                .map_err(|error| internal(error.to_string()))?;
            file.write_all(bytes)
                .and_then(|_| file.sync_all())
                .map_err(|error| internal(error.to_string()))?;
        }
        if package_files_from_disk(&temporary).ok().as_ref() != Some(&self.files) {
            return Err(internal("failed to materialize the DSH plugin package"));
        }
        match fs::rename(&temporary, &self.path) {
            Ok(()) => {
                std::mem::forget(guard);
                sync_parent(&self.path)?;
            }
            Err(_) if self.exists_and_matches() => return Ok(()),
            Err(error) if self.path.exists() => {
                return Err(DshApplicationError::conflict(format!(
                    "the immutable DSH plugin package appeared with different contents: {error}"
                )));
            }
            Err(error) => return Err(internal(error.to_string())),
        }
        if !self.exists_and_matches() {
            return Err(internal("published DSH plugin package failed verification"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
struct ResolvedExecutable {
    path: PathBuf,
    display: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CommandSpec {
    executable: PathBuf,
    display_executable: String,
    args: Vec<OsString>,
    timeout: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct CommandOutput {
    success: bool,
    stdout: String,
    stderr: String,
}

trait CommandRunner: Send + Sync {
    fn run(&self, command: &CommandSpec) -> Result<CommandOutput, String>;
}

struct ProcessCommandRunner;

impl CommandRunner for ProcessCommandRunner {
    fn run(&self, command: &CommandSpec) -> Result<CommandOutput, String> {
        #[cfg(windows)]
        {
            run_windows_command(command)
        }
        #[cfg(not(windows))]
        {
            run_non_windows_command(command)
        }
    }
}

#[cfg(not(windows))]
fn run_non_windows_command(command: &CommandSpec) -> Result<CommandOutput, String> {
    let mut process = Command::new(&command.executable);
    process
        .args(&command.args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    process.process_group(0);
    let mut child = process
        .spawn()
        .map_err(|error| format!("failed to start {}: {error}", command.display_executable))?;
    let process_group = child.id();
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "failed to capture DSH stdout".to_string())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "failed to capture DSH stderr".to_string())?;
    let (stdout_tx, stdout_rx) = mpsc::channel();
    let (stderr_tx, stderr_rx) = mpsc::channel();
    thread::spawn(move || {
        let _ = stdout_tx.send(read_bounded(stdout));
    });
    thread::spawn(move || {
        let _ = stderr_tx.send(read_bounded(stderr));
    });
    let started = Instant::now();
    let deadline = started + command.timeout;
    let mut timed_out = false;
    let status = loop {
        match child.try_wait().map_err(|error| error.to_string())? {
            Some(status) => break Some(status),
            None if Instant::now() >= deadline => {
                timed_out = true;
                break None;
            }
            None => thread::sleep(Duration::from_millis(20)),
        }
    };
    // Match the Windows job object: once the leader is done or the timeout
    // fires, kill the rest of the group so inherited pipes cannot pin drain.
    terminate_non_windows_process_tree(&mut child, process_group);
    let drain_deadline = Instant::now()
        + if timed_out {
            Duration::from_secs(5)
        } else {
            deadline
                .saturating_duration_since(Instant::now())
                .max(Duration::from_millis(250))
        };
    let stdout = pipe_or_timeout(
        collect_command_pipe(stdout_rx, drain_deadline, "DSH stdout reader panicked"),
        timed_out,
        command,
    )?;
    let stderr = pipe_or_timeout(
        collect_command_pipe(stderr_rx, drain_deadline, "DSH stderr reader panicked"),
        timed_out,
        command,
    )?;
    let Some(status) = status else {
        return Err(format!("{} timed out", command.display_executable));
    };
    Ok(CommandOutput {
        success: status.success(),
        stdout: redact_output(&stdout),
        stderr: redact_output(&stderr),
    })
}

#[cfg(not(windows))]
enum PipeCollectError {
    Timeout,
    Panicked(String),
}

#[cfg(not(windows))]
fn collect_command_pipe(
    rx: mpsc::Receiver<String>,
    deadline: Instant,
    panicked: &str,
) -> Result<String, PipeCollectError> {
    match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
        Ok(value) => Ok(value),
        Err(mpsc::RecvTimeoutError::Timeout) => Err(PipeCollectError::Timeout),
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            Err(PipeCollectError::Panicked(panicked.to_string()))
        }
    }
}

#[cfg(not(windows))]
fn pipe_or_timeout(
    result: Result<String, PipeCollectError>,
    timed_out: bool,
    command: &CommandSpec,
) -> Result<String, String> {
    match result {
        Ok(value) if !timed_out => Ok(value),
        Err(PipeCollectError::Panicked(message)) if !timed_out => Err(message),
        _ => Err(format!("{} timed out", command.display_executable)),
    }
}

#[cfg(not(windows))]
fn terminate_non_windows_process_tree(child: &mut std::process::Child, process_group: u32) {
    #[cfg(unix)]
    {
        use nix::sys::signal::{Signal, killpg};
        use nix::unistd::Pid;
        let _ = killpg(Pid::from_raw(process_group as i32), Signal::SIGKILL);
    }
    let _ = child.kill();
    let _ = child.wait();
}

fn user_home() -> PathBuf {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn compatible_version(raw: &str) -> bool {
    matches!(raw, "0.1.5-rc.1" | "0.1.5-rc.2")
}

fn valid_gateway_v1_url(value: &str) -> bool {
    !value.is_empty()
        && !value.contains(['\0', '\r', '\n'])
        && (value.starts_with("http://") || value.starts_with("https://"))
        && value.ends_with("/v1")
}

fn safe_relative_path(value: &str) -> DshApplicationResult<PathBuf> {
    let path = Path::new(value);
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir
                    | std::path::Component::RootDir
                    | std::path::Component::Prefix(_)
            )
        })
    {
        return Err(DshApplicationError::invalid(
            "unsafe DSH plugin template path",
        ));
    }
    Ok(path.to_path_buf())
}

fn package_digest(files: &BTreeMap<PathBuf, Vec<u8>>) -> String {
    let mut hash = Sha256::new();
    hash.update(b"open-console-gateway-dsh-package-v1\0");
    for (path, bytes) in files {
        hash.update(path.to_string_lossy().as_bytes());
        hash.update([0]);
        hash.update(bytes);
        hash.update([0]);
    }
    format!("{:x}", hash.finalize())
}

fn read_optional(path: &Path) -> DshApplicationResult<Option<Vec<u8>>> {
    match fs::read(path) {
        Ok(bytes) if bytes.len() <= MAX_PACKAGE_BYTES as usize => Ok(Some(bytes)),
        Ok(_) => Err(DshApplicationError::precondition(
            "DSH integration state exceeds the size limit",
        )),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(internal(error.to_string())),
    }
}

fn restore_optional(
    trusted_root: &Path,
    path: &Path,
    bytes: Option<&[u8]>,
) -> DshApplicationResult<()> {
    match bytes {
        Some(bytes) => write_private_atomic(trusted_root, path, bytes),
        None => match fs::remove_file(path) {
            Ok(()) => sync_parent(path),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
            Err(error) => Err(internal(error.to_string())),
        },
    }
}

fn dependency_source(spec: &str, manifest: &Path) -> Result<PathBuf, String> {
    let value = spec
        .strip_prefix("file:")
        .or_else(|| spec.strip_prefix("link:"))
        .ok_or_else(|| "DSH same-name dependency is not a local OCG package source".to_string())?;
    let value = value.strip_prefix("//").unwrap_or(value);
    #[cfg(windows)]
    let value = value
        .strip_prefix('/')
        .filter(|path| path.as_bytes().get(1) == Some(&b':'))
        .unwrap_or(value);
    let path = PathBuf::from(value);
    let resolved = if path.is_absolute() {
        path
    } else {
        manifest
            .parent()
            .ok_or_else(|| "DSH profile manifest has no parent".to_string())?
            .join(path)
    };
    canonical_lexical_path(&resolved)
}

fn is_owned_package_source(source: &Path, trusted_root: &Path) -> bool {
    let Ok(source) = canonical_lexical_path(source) else {
        return false;
    };
    let Ok(root) = canonical_lexical_path(trusted_root) else {
        return false;
    };
    let Some(directory_name) = source.file_name().and_then(|value| value.to_str()) else {
        return false;
    };
    if source.parent() != Some(root.as_path())
        || directory_name.len() != 24
        || !directory_name
            .bytes()
            .all(|value| value.is_ascii_hexdigit())
        || !safe_directory_chain(&root, &source)
    {
        return false;
    }
    let Ok(files) = package_files_from_disk(&source) else {
        return false;
    };
    let name_matches = files
        .get(Path::new("package.json"))
        .and_then(|bytes| serde_json::from_slice::<Value>(bytes).ok())
        .and_then(|value| value.get("name").and_then(Value::as_str).map(str::to_owned))
        .is_some_and(|name| name == PACKAGE_NAME);
    name_matches && package_digest(&files).get(..24) == Some(directory_name)
}

fn package_files_from_disk(root: &Path) -> Result<BTreeMap<PathBuf, Vec<u8>>, String> {
    let metadata = fs::symlink_metadata(root).map_err(|error| error.to_string())?;
    if !metadata.file_type().is_dir() || is_link_or_reparse(root) {
        return Err("DSH plugin source is not a regular directory".into());
    }
    let mut files = BTreeMap::new();
    collect_package_files(root, root, &mut files)?;
    if files.is_empty() {
        return Err("DSH plugin source is empty".into());
    }
    Ok(files)
}

fn collect_package_files(
    root: &Path,
    directory: &Path,
    files: &mut BTreeMap<PathBuf, Vec<u8>>,
) -> Result<(), String> {
    for entry in fs::read_dir(directory).map_err(|error| error.to_string())? {
        let entry = entry.map_err(|error| error.to_string())?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path).map_err(|error| error.to_string())?;
        if is_link_or_reparse(&path) {
            return Err("DSH plugin source contains a link".into());
        }
        if metadata.file_type().is_dir() {
            collect_package_files(root, &path, files)?;
            continue;
        }
        if !metadata.file_type().is_file() || metadata.len() > MAX_PACKAGE_BYTES {
            return Err("DSH plugin source contains an unsupported file".into());
        }
        if files.len() >= MAX_PACKAGE_FILES {
            return Err("DSH plugin source contains too many files".into());
        }
        let relative = path
            .strip_prefix(root)
            .map_err(|_| "DSH plugin file escaped its root".to_string())?
            .to_path_buf();
        files.insert(relative, fs::read(path).map_err(|error| error.to_string())?);
    }
    Ok(())
}

fn canonical_lexical_path(path: &Path) -> Result<PathBuf, String> {
    if !path.is_absolute() {
        return Err("DSH integration path must be absolute".into());
    }
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if !normalized.pop() {
                    return Err("DSH integration path escapes its root".into());
                }
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    Ok(normalized)
}

fn same_lexical_path(left: &Path, right: &Path) -> bool {
    canonical_lexical_path(left).ok() == canonical_lexical_path(right).ok()
}

fn is_link_or_reparse(path: &Path) -> bool {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return false;
    };
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes()
            & windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT
            != 0
    }
    #[cfg(not(windows))]
    {
        false
    }
}

fn ensure_safe_directory_chain(trusted_root: &Path, target: &Path) -> DshApplicationResult<()> {
    let root = canonical_lexical_path(trusted_root).map_err(DshApplicationError::conflict)?;
    let target = canonical_lexical_path(target).map_err(DshApplicationError::conflict)?;
    let relative = target
        .strip_prefix(&root)
        .map_err(|_| DshApplicationError::conflict("DSH integration escaped its trusted root"))?;
    fs::create_dir_all(&root).map_err(|error| internal(error.to_string()))?;
    let canonical_root = fs::canonicalize(&root).map_err(|error| internal(error.to_string()))?;
    let mut current = root;
    for component in relative.components() {
        current.push(component.as_os_str());
        match fs::symlink_metadata(&current) {
            Ok(metadata) => {
                if !metadata.file_type().is_dir() || is_link_or_reparse(&current) {
                    return Err(DshApplicationError::conflict(
                        "DSH integration directory contains a link or non-directory ancestor",
                    ));
                }
            }
            Err(error) if error.kind() == ErrorKind::NotFound => {
                fs::create_dir(&current).map_err(|error| internal(error.to_string()))?;
            }
            Err(error) => return Err(internal(error.to_string())),
        }
        let canonical = fs::canonicalize(&current).map_err(|error| internal(error.to_string()))?;
        if !canonical.starts_with(&canonical_root) {
            return Err(DshApplicationError::conflict(
                "DSH integration directory escaped its trusted root",
            ));
        }
    }
    Ok(())
}

fn safe_directory_chain(trusted_root: &Path, target: &Path) -> bool {
    let Ok(root) = canonical_lexical_path(trusted_root) else {
        return false;
    };
    let Ok(target) = canonical_lexical_path(target) else {
        return false;
    };
    let Ok(relative) = target.strip_prefix(&root) else {
        return false;
    };
    let Ok(canonical_root) = fs::canonicalize(&root) else {
        return false;
    };
    let mut current = root;
    for component in relative.components() {
        current.push(component.as_os_str());
        let Ok(metadata) = fs::symlink_metadata(&current) else {
            return false;
        };
        if !metadata.file_type().is_dir() || is_link_or_reparse(&current) {
            return false;
        }
        let Ok(canonical) = fs::canonicalize(&current) else {
            return false;
        };
        if !canonical.starts_with(&canonical_root) {
            return false;
        }
    }
    true
}

fn resolve_executable(configured: &Path) -> Option<ResolvedExecutable> {
    if configured.components().count() > 1 || configured.is_absolute() {
        return configured.is_file().then(|| ResolvedExecutable {
            path: configured.to_path_buf(),
            display: configured.display().to_string(),
        });
    }
    let path = std::env::var_os("PATH")?;
    let names = executable_names(configured);
    for directory in std::env::split_paths(&path) {
        for name in &names {
            let candidate = directory.join(name);
            if candidate.is_file() {
                return Some(ResolvedExecutable {
                    display: candidate.display().to_string(),
                    path: candidate,
                });
            }
        }
    }
    None
}

fn executable_names(name: &Path) -> Vec<OsString> {
    #[cfg(windows)]
    {
        if name.extension().is_some() {
            vec![name.as_os_str().to_owned()]
        } else {
            vec![
                OsString::from(format!("{}.exe", name.display())),
                OsString::from(format!("{}.cmd", name.display())),
                OsString::from(format!("{}.bat", name.display())),
                name.as_os_str().to_owned(),
            ]
        }
    }
    #[cfg(not(windows))]
    {
        vec![name.as_os_str().to_owned()]
    }
}

fn dsh_package_argument(package_path: &Path) -> OsString {
    // The Windows command line quotes once at launch. Pre-quoting here would
    // embed quote characters into argv and turn paths containing `&` into cmd
    // syntax after `cmd /d /s /c` wraps the same argument again.
    package_path.as_os_str().to_owned()
}

fn read_bounded<R: Read>(mut reader: R) -> String {
    let mut kept = Vec::new();
    let mut chunk = [0_u8; 4096];
    loop {
        match reader.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(count) => {
                let remaining = MAX_COMMAND_OUTPUT.saturating_sub(kept.len());
                kept.extend_from_slice(&chunk[..count.min(remaining)]);
            }
        }
    }
    String::from_utf8_lossy(&kept).into_owned()
}

fn redact_output(value: &str) -> String {
    value
        .lines()
        .map(|line| {
            let lower = line.to_ascii_lowercase();
            if lower.contains("ocg_gateway_key")
                || lower.contains("credential-handoff")
                || lower.contains("authorization:")
                || lower.contains("api key")
                || lower.contains("apikey")
                || lower.contains("bearer ")
            {
                "[redacted]"
            } else {
                line
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn command_failure(command: &CommandSpec, output: &CommandOutput) -> String {
    let message = if output.stderr.trim().is_empty() {
        output.stdout.trim()
    } else {
        output.stderr.trim()
    };
    if message.is_empty() {
        format!("{} exited unsuccessfully", command.display_executable)
    } else {
        format!(
            "{} exited unsuccessfully: {message}",
            command.display_executable
        )
    }
}

fn internal(message: impl Into<String>) -> DshApplicationError {
    DshApplicationError::internal(message)
}

fn write_private_atomic(
    trusted_root: &Path,
    destination: &Path,
    bytes: &[u8],
) -> DshApplicationResult<()> {
    let parent = destination
        .parent()
        .ok_or_else(|| internal("DSH credential handoff has no parent"))?;
    ensure_safe_directory_chain(trusted_root, parent)?;
    if let Ok(metadata) = fs::symlink_metadata(destination)
        && (!metadata.file_type().is_file() || is_link_or_reparse(destination))
    {
        return Err(DshApplicationError::conflict(
            "DSH credential handoff target is not a regular file",
        ));
    }
    let temporary = parent.join(format!(".ocg-dsh-{}.tmp", uuid::Uuid::new_v4().simple()));
    struct TempGuard(PathBuf);
    impl Drop for TempGuard {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.0);
        }
    }
    let guard = TempGuard(temporary.clone());
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&temporary)
        .map_err(|error| internal(error.to_string()))?;
    #[cfg(windows)]
    set_private_permissions(&temporary)?;
    file.write_all(bytes)
        .and_then(|_| file.sync_all())
        .map_err(|error| internal(error.to_string()))?;
    drop(file);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&temporary, fs::Permissions::from_mode(0o600))
            .map_err(|error| internal(error.to_string()))?;
    }
    #[cfg(windows)]
    if destination.exists() {
        set_private_permissions(destination)?;
    }
    replace_file(&temporary, destination)?;
    std::mem::forget(guard);
    #[cfg(windows)]
    set_private_permissions(destination)?;
    sync_parent(destination)
}

#[cfg(windows)]
fn set_private_permissions(path: &Path) -> DshApplicationResult<()> {
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{CloseHandle, ERROR_SUCCESS, GENERIC_ALL, LocalFree};
    use windows_sys::Win32::Security::Authorization::{
        EXPLICIT_ACCESS_W, SE_FILE_OBJECT, SET_ACCESS, SetEntriesInAclW, SetNamedSecurityInfoW,
        TRUSTEE_IS_SID, TRUSTEE_IS_USER, TRUSTEE_W,
    };
    use windows_sys::Win32::Security::{
        CreateWellKnownSid, DACL_SECURITY_INFORMATION, GetTokenInformation, NO_INHERITANCE,
        PROTECTED_DACL_SECURITY_INFORMATION, PSID, SECURITY_MAX_SID_SIZE, TOKEN_QUERY, TOKEN_USER,
        TokenUser, WinLocalSystemSid,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    struct HandleGuard(windows_sys::Win32::Foundation::HANDLE);
    impl Drop for HandleGuard {
        fn drop(&mut self) {
            unsafe { CloseHandle(self.0) };
        }
    }
    let mut token = std::ptr::null_mut();
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(internal(std::io::Error::last_os_error().to_string()));
    }
    let _token = HandleGuard(token);
    let mut needed = 0u32;
    unsafe { GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut needed) };
    if needed < std::mem::size_of::<TOKEN_USER>() as u32 {
        return Err(internal("Windows did not return the current user SID"));
    }
    let word = std::mem::size_of::<usize>();
    let mut user_storage = vec![0usize; (needed as usize).div_ceil(word)];
    if unsafe {
        GetTokenInformation(
            token,
            TokenUser,
            user_storage.as_mut_ptr().cast(),
            needed,
            &mut needed,
        )
    } == 0
    {
        return Err(internal(std::io::Error::last_os_error().to_string()));
    }
    let user_sid = unsafe { (*(user_storage.as_ptr().cast::<TOKEN_USER>())).User.Sid };
    let mut system_storage = vec![0usize; (SECURITY_MAX_SID_SIZE as usize).div_ceil(word)];
    let mut system_len = SECURITY_MAX_SID_SIZE;
    let system_sid: PSID = system_storage.as_mut_ptr().cast();
    if unsafe {
        CreateWellKnownSid(
            WinLocalSystemSid,
            std::ptr::null_mut(),
            system_sid,
            &mut system_len,
        )
    } == 0
    {
        return Err(internal(std::io::Error::last_os_error().to_string()));
    }
    let trustee = |sid: PSID| TRUSTEE_W {
        pMultipleTrustee: std::ptr::null_mut(),
        MultipleTrusteeOperation: 0,
        TrusteeForm: TRUSTEE_IS_SID,
        TrusteeType: TRUSTEE_IS_USER,
        ptstrName: sid.cast::<u16>(),
    };
    let entries = [
        EXPLICIT_ACCESS_W {
            grfAccessPermissions: GENERIC_ALL,
            grfAccessMode: SET_ACCESS,
            grfInheritance: NO_INHERITANCE,
            Trustee: trustee(user_sid),
        },
        EXPLICIT_ACCESS_W {
            grfAccessPermissions: GENERIC_ALL,
            grfAccessMode: SET_ACCESS,
            grfInheritance: NO_INHERITANCE,
            Trustee: trustee(system_sid),
        },
    ];
    let mut acl = std::ptr::null_mut();
    let status = unsafe {
        SetEntriesInAclW(
            entries.len() as u32,
            entries.as_ptr(),
            std::ptr::null(),
            &mut acl,
        )
    };
    if status != ERROR_SUCCESS {
        return Err(internal(
            std::io::Error::from_raw_os_error(status as i32).to_string(),
        ));
    }
    struct LocalGuard(*mut c_void);
    impl Drop for LocalGuard {
        fn drop(&mut self) {
            unsafe { LocalFree(self.0) };
        }
    }
    let _acl = LocalGuard(acl.cast());
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let status = unsafe {
        SetNamedSecurityInfoW(
            wide.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            acl,
            std::ptr::null_mut(),
        )
    };
    if status != ERROR_SUCCESS {
        return Err(internal(
            std::io::Error::from_raw_os_error(status as i32).to_string(),
        ));
    }
    verify_windows_private_dacl(path, user_sid, system_sid)
}

#[cfg(windows)]
fn verify_windows_private_dacl(
    path: &Path,
    user_sid: windows_sys::Win32::Security::PSID,
    system_sid: windows_sys::Win32::Security::PSID,
) -> DshApplicationResult<()> {
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{ERROR_SUCCESS, GENERIC_ALL, LocalFree};
    use windows_sys::Win32::Security::Authorization::{GetNamedSecurityInfoW, SE_FILE_OBJECT};
    use windows_sys::Win32::Security::{
        ACCESS_ALLOWED_ACE, ACL_SIZE_INFORMATION, AclSizeInformation, DACL_SECURITY_INFORMATION,
        EqualSid, GetAce, GetAclInformation, GetSecurityDescriptorControl, PSECURITY_DESCRIPTOR,
        SE_DACL_PROTECTED,
    };
    use windows_sys::Win32::Storage::FileSystem::FILE_ALL_ACCESS;
    use windows_sys::Win32::System::SystemServices::ACCESS_ALLOWED_ACE_TYPE;

    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut acl = std::ptr::null_mut();
    let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    let status = unsafe {
        GetNamedSecurityInfoW(
            wide.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut acl,
            std::ptr::null_mut(),
            &mut descriptor,
        )
    };
    if status != ERROR_SUCCESS {
        return Err(internal(
            std::io::Error::from_raw_os_error(status as i32).to_string(),
        ));
    }
    struct LocalGuard(*mut c_void);
    impl Drop for LocalGuard {
        fn drop(&mut self) {
            unsafe { LocalFree(self.0) };
        }
    }
    let _descriptor = LocalGuard(descriptor.cast());
    if acl.is_null() {
        return Err(DshApplicationError::precondition(
            "Windows private DACL is missing",
        ));
    }
    let mut control = 0u16;
    let mut revision = 0u32;
    if unsafe { GetSecurityDescriptorControl(descriptor, &mut control, &mut revision) } == 0
        || control & SE_DACL_PROTECTED == 0
    {
        return Err(DshApplicationError::precondition(
            "Windows private DACL is not protected",
        ));
    }
    let mut info = ACL_SIZE_INFORMATION::default();
    if unsafe {
        GetAclInformation(
            acl,
            (&mut info as *mut ACL_SIZE_INFORMATION).cast(),
            std::mem::size_of::<ACL_SIZE_INFORMATION>() as u32,
            AclSizeInformation,
        )
    } == 0
        || info.AceCount != 2
    {
        return Err(DshApplicationError::precondition(
            "Windows private DACL contains unexpected access entries",
        ));
    }
    let mut saw_user = false;
    let mut saw_system = false;
    for index in 0..info.AceCount {
        let mut raw_ace: *mut c_void = std::ptr::null_mut();
        if unsafe { GetAce(acl, index, &mut raw_ace) } == 0 || raw_ace.is_null() {
            return Err(internal(std::io::Error::last_os_error().to_string()));
        }
        let ace = unsafe { &*(raw_ace.cast::<ACCESS_ALLOWED_ACE>()) };
        if ace.Header.AceType as u32 != ACCESS_ALLOWED_ACE_TYPE
            || ace.Header.AceFlags != 0
            || (ace.Mask != GENERIC_ALL && ace.Mask != FILE_ALL_ACCESS)
        {
            return Err(DshApplicationError::precondition(
                "Windows private DACL contains an unexpected access rule",
            ));
        }
        let sid = std::ptr::addr_of!(ace.SidStart).cast_mut().cast();
        if unsafe { EqualSid(sid, user_sid) } != 0 {
            if saw_user {
                return Err(DshApplicationError::precondition(
                    "Windows private DACL repeats the user rule",
                ));
            }
            saw_user = true;
        } else if unsafe { EqualSid(sid, system_sid) } != 0 {
            if saw_system {
                return Err(DshApplicationError::precondition(
                    "Windows private DACL repeats the system rule",
                ));
            }
            saw_system = true;
        } else {
            return Err(DshApplicationError::precondition(
                "Windows private DACL grants access to another identity",
            ));
        }
    }
    if !saw_user || !saw_system {
        return Err(DshApplicationError::precondition(
            "Windows private DACL does not protect the current user",
        ));
    }
    Ok(())
}

#[cfg(windows)]
fn replace_file(source: &Path, destination: &Path) -> DshApplicationResult<()> {
    use std::os::windows::ffi::OsStrExt;
    type Bool = i32;
    unsafe extern "system" {
        fn ReplaceFileW(
            replaced: *const u16,
            replacement: *const u16,
            backup: *const u16,
            flags: u32,
            exclude: *mut std::ffi::c_void,
            reserved: *mut std::ffi::c_void,
        ) -> Bool;
        fn MoveFileExW(existing: *const u16, new: *const u16, flags: u32) -> Bool;
    }
    const REPLACEFILE_WRITE_THROUGH: u32 = 0x0000_0001;
    const MOVEFILE_WRITE_THROUGH: u32 = 0x0000_0008;
    let wide = |path: &Path| {
        path.as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect::<Vec<_>>()
    };
    let destination_exists = destination.exists();
    let source = wide(source);
    let destination = wide(destination);
    let ok = unsafe {
        if destination_exists {
            ReplaceFileW(
                destination.as_ptr(),
                source.as_ptr(),
                std::ptr::null(),
                REPLACEFILE_WRITE_THROUGH,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        } else {
            MoveFileExW(
                source.as_ptr(),
                destination.as_ptr(),
                MOVEFILE_WRITE_THROUGH,
            )
        }
    };
    if ok == 0 {
        Err(internal(std::io::Error::last_os_error().to_string()))
    } else {
        Ok(())
    }
}

#[cfg(not(windows))]
fn replace_file(source: &Path, destination: &Path) -> DshApplicationResult<()> {
    fs::rename(source, destination).map_err(|error| internal(error.to_string()))
}

#[cfg(unix)]
fn sync_parent(path: &Path) -> DshApplicationResult<()> {
    let parent = path
        .parent()
        .ok_or_else(|| internal("DSH integration target has no parent"))?;
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| internal(error.to_string()))
}

#[cfg(not(unix))]
fn sync_parent(_path: &Path) -> DshApplicationResult<()> {
    Ok(())
}

#[cfg(windows)]
struct WindowsProcessTree(windows_sys::Win32::Foundation::HANDLE);

#[cfg(windows)]
impl WindowsProcessTree {
    fn new() -> Result<Self, String> {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::JobObjects::{
            CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
            SetInformationJobObject,
        };
        unsafe {
            let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if job.is_null() {
                return Err(format!(
                    "failed to create DSH process boundary: {}",
                    std::io::Error::last_os_error()
                ));
            }
            let mut information: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            information.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            if SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                (&information as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            ) == 0
            {
                let error = std::io::Error::last_os_error();
                CloseHandle(job);
                return Err(format!("failed to configure DSH process boundary: {error}"));
            }
            Ok(Self(job))
        }
    }

    fn assign(&self, process: windows_sys::Win32::Foundation::HANDLE) -> Result<(), String> {
        if unsafe {
            windows_sys::Win32::System::JobObjects::AssignProcessToJobObject(self.0, process)
        } == 0
        {
            return Err(format!(
                "failed to contain DSH process tree: {}",
                std::io::Error::last_os_error()
            ));
        }
        Ok(())
    }

    fn terminate(&self) {
        unsafe { windows_sys::Win32::System::JobObjects::TerminateJobObject(self.0, 1) };
    }
}

#[cfg(windows)]
impl Drop for WindowsProcessTree {
    fn drop(&mut self) {
        unsafe { windows_sys::Win32::Foundation::CloseHandle(self.0) };
    }
}

#[cfg(windows)]
struct OwnedWindowsHandle(windows_sys::Win32::Foundation::HANDLE);

#[cfg(windows)]
impl OwnedWindowsHandle {
    fn into_file(mut self) -> File {
        use std::os::windows::io::FromRawHandle;
        let handle = self.0;
        self.0 = std::ptr::null_mut();
        unsafe { File::from_raw_handle(handle as _) }
    }
}

#[cfg(windows)]
impl Drop for OwnedWindowsHandle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { windows_sys::Win32::Foundation::CloseHandle(self.0) };
        }
    }
}

#[cfg(windows)]
fn windows_pipe(parent_reads: bool) -> Result<(OwnedWindowsHandle, OwnedWindowsHandle), String> {
    use windows_sys::Win32::Foundation::{HANDLE, HANDLE_FLAG_INHERIT, SetHandleInformation};
    use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
    use windows_sys::Win32::System::Pipes::CreatePipe;
    unsafe {
        let mut read: HANDLE = std::ptr::null_mut();
        let mut write: HANDLE = std::ptr::null_mut();
        let attributes = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: std::ptr::null_mut(),
            bInheritHandle: 1,
        };
        if CreatePipe(&mut read, &mut write, &attributes, 0) == 0 {
            return Err(format!(
                "failed to create DSH output pipe: {}",
                std::io::Error::last_os_error()
            ));
        }
        let read = OwnedWindowsHandle(read);
        let write = OwnedWindowsHandle(write);
        let parent_handle = if parent_reads { read.0 } else { write.0 };
        if SetHandleInformation(parent_handle, HANDLE_FLAG_INHERIT, 0) == 0 {
            return Err(format!(
                "failed to protect DSH output pipe: {}",
                std::io::Error::last_os_error()
            ));
        }
        Ok((read, write))
    }
}

#[cfg(windows)]
fn run_windows_command(command: &CommandSpec) -> Result<CommandOutput, String> {
    use windows_sys::Win32::Foundation::{WAIT_OBJECT_0, WAIT_TIMEOUT};
    use windows_sys::Win32::System::Threading::{
        CREATE_NO_WINDOW, CREATE_SUSPENDED, CreateProcessW, GetExitCodeProcess,
        PROCESS_INFORMATION, ResumeThread, STARTF_USESTDHANDLES, STARTUPINFOW, WaitForSingleObject,
    };
    let (application, mut command_line) = windows_process_command_line(command)?;
    let (stdout_read, stdout_write) = windows_pipe(true)?;
    let (stderr_read, stderr_write) = windows_pipe(true)?;
    let (stdin_read, stdin_write) = windows_pipe(false)?;
    let job = WindowsProcessTree::new()?;
    let mut startup: STARTUPINFOW = unsafe { std::mem::zeroed() };
    startup.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
    startup.dwFlags = STARTF_USESTDHANDLES;
    startup.hStdInput = stdin_read.0;
    startup.hStdOutput = stdout_write.0;
    startup.hStdError = stderr_write.0;
    let mut process_information: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
    if unsafe {
        CreateProcessW(
            application.as_ptr(),
            command_line.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            1,
            CREATE_SUSPENDED | CREATE_NO_WINDOW,
            std::ptr::null(),
            std::ptr::null(),
            &startup,
            &mut process_information,
        )
    } == 0
    {
        return Err(format!(
            "failed to start {}: {}",
            command.display_executable,
            std::io::Error::last_os_error()
        ));
    }
    let process = OwnedWindowsHandle(process_information.hProcess);
    let primary_thread = OwnedWindowsHandle(process_information.hThread);
    job.assign(process.0).inspect_err(|_| job.terminate())?;
    drop(stdout_write);
    drop(stderr_write);
    drop(stdin_read);
    drop(stdin_write);
    let stdout_file = stdout_read.into_file();
    let stderr_file = stderr_read.into_file();
    let stdout_reader = thread::spawn(move || read_bounded(stdout_file));
    let stderr_reader = thread::spawn(move || read_bounded(stderr_file));
    if unsafe { ResumeThread(primary_thread.0) } == u32::MAX {
        job.terminate();
        let _ = stdout_reader.join();
        let _ = stderr_reader.join();
        return Err(format!(
            "failed to resume {}: {}",
            command.display_executable,
            std::io::Error::last_os_error()
        ));
    }
    drop(primary_thread);
    let started = Instant::now();
    let mut timed_out = false;
    loop {
        match unsafe { WaitForSingleObject(process.0, 20) } {
            WAIT_OBJECT_0 => break,
            WAIT_TIMEOUT if started.elapsed() >= command.timeout => {
                timed_out = true;
                break;
            }
            WAIT_TIMEOUT => {}
            _ => {
                job.terminate();
                let _ = stdout_reader.join();
                let _ = stderr_reader.join();
                return Err(format!(
                    "failed while waiting for {}: {}",
                    command.display_executable,
                    std::io::Error::last_os_error()
                ));
            }
        }
    }
    job.terminate();
    unsafe { WaitForSingleObject(process.0, 5_000) };
    let stdout = stdout_reader
        .join()
        .map_err(|_| "DSH stdout reader panicked".to_string())?;
    let stderr = stderr_reader
        .join()
        .map_err(|_| "DSH stderr reader panicked".to_string())?;
    if timed_out {
        return Err(format!("{} timed out", command.display_executable));
    }
    let mut exit_code = 1u32;
    if unsafe { GetExitCodeProcess(process.0, &mut exit_code) } == 0 {
        return Err(format!(
            "failed to read {} exit status: {}",
            command.display_executable,
            std::io::Error::last_os_error()
        ));
    }
    Ok(CommandOutput {
        success: exit_code == 0,
        stdout: redact_output(&stdout),
        stderr: redact_output(&stderr),
    })
}

#[cfg(windows)]
fn windows_process_command_line(command: &CommandSpec) -> Result<(Vec<u16>, Vec<u16>), String> {
    use std::os::windows::ffi::OsStrExt;
    let extension = command
        .executable
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default();
    let (application, command_line) =
        if extension.eq_ignore_ascii_case("cmd") || extension.eq_ignore_ascii_case("bat") {
            let command_processor = std::env::var_os("ComSpec")
                .map(PathBuf::from)
                .filter(|path| path.is_file())
                .or_else(|| {
                    std::env::var_os("SystemRoot")
                        .map(PathBuf::from)
                        .map(|root| root.join("System32/cmd.exe"))
                        .filter(|path| path.is_file())
                })
                .ok_or_else(|| "Windows command processor was not found".to_string())?;
            let script = command
                .executable
                .to_str()
                .ok_or_else(|| "DSH command path is not Unicode".to_string())?;
            let mut shell_command = String::from("\"");
            for value in std::iter::once(script).chain(
                command
                    .args
                    .iter()
                    .map(|value| value.to_str().unwrap_or("\0")),
            ) {
                if value.contains(['\0', '\r', '\n', '%', '!']) {
                    return Err("DSH batch command contains unsafe characters".into());
                }
                shell_command.push_str(&quote_windows_argument_always(value));
                shell_command.push(' ');
            }
            shell_command.pop();
            shell_command.push('"');
            let processor = command_processor
                .to_str()
                .ok_or_else(|| "Windows command processor path is not Unicode".to_string())?
                .to_string();
            (
                command_processor,
                format!(
                    "{} /d /s /c {}",
                    quote_windows_argument(&processor),
                    shell_command
                ),
            )
        } else {
            let executable = command
                .executable
                .to_str()
                .ok_or_else(|| "DSH command path is not Unicode".to_string())?;
            let mut line = quote_windows_argument(executable);
            for argument in &command.args {
                let argument = argument
                    .to_str()
                    .ok_or_else(|| "DSH command argument is not Unicode".to_string())?;
                if argument.contains('\0') {
                    return Err("DSH command argument contains NUL".into());
                }
                line.push(' ');
                line.push_str(&quote_windows_argument(argument));
            }
            (command.executable.clone(), line)
        };
    let mut application = application.as_os_str().encode_wide().collect::<Vec<_>>();
    application.push(0);
    let mut command_line = command_line.encode_utf16().collect::<Vec<_>>();
    command_line.push(0);
    Ok((application, command_line))
}

#[cfg(windows)]
fn quote_windows_argument(value: &str) -> String {
    if !value.is_empty()
        && !value
            .chars()
            .any(|character| character.is_whitespace() || character == '"')
    {
        return value.to_string();
    }
    quote_windows_argument_always(value)
}

#[cfg(windows)]
fn quote_windows_argument_always(value: &str) -> String {
    let mut quoted = String::from("\"");
    let mut backslashes = 0usize;
    for character in value.chars() {
        if character == '\\' {
            backslashes += 1;
            continue;
        }
        if character == '"' {
            quoted.push_str(&"\\".repeat(backslashes * 2 + 1));
            quoted.push('"');
        } else {
            quoted.push_str(&"\\".repeat(backslashes));
            quoted.push(character);
        }
        backslashes = 0;
    }
    quoted.push_str(&"\\".repeat(backslashes * 2));
    quoted.push('"');
    quoted
}
#[cfg(test)]
#[path = "dsh_application_host/tests.rs"]
mod tests;

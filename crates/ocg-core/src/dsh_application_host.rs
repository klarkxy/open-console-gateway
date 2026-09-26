//! Native-host implementation of the DSH application installer.
//!
//! This module is deliberately DSH-specific. It materializes one immutable,
//! app-owned plugin source, invokes the official `dsh plugin` command with an
//! argument array, and hands the selected Gateway Key to DSH through a private
//! one-time file. It never revives the retired generic Applications connector.
//! The Host writes and restores only the live credential-handoff; claim files
//! belong to the plugin.

use crate::dsh_application::{
    DshApplicationError, DshApplicationHostRequest, DshApplicationInspection,
    DshApplicationOutcome, DshApplicationPhase, DshApplicationResult, DshDiscoveredProfile,
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
const DEFAULT_WEB_RUNTIME: &str = "http://127.0.0.1:3080";
const DEFAULT_DESKTOP_RUNTIME: &str = "http://127.0.0.1:19387";
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
    let configured_home = std::env::var_os("DSH_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    let user_home = absolute_host_path(user_home());
    let home = configured_home
        .clone()
        .unwrap_or_else(|| user_home.join(".dsh"));
    let host = Arc::new(DshDesktopHost {
        data_dir: absolute_host_path(core.data_dir()),
        home: absolute_host_path(home),
        profile: PROFILE.into(),
        legacy_bootstrap: true,
        scan_user_home: configured_home.is_none().then_some(user_home),
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
    profile: String,
    legacy_bootstrap: bool,
    scan_user_home: Option<PathBuf>,
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
            DshApplicationHostRequest::Inspect {
                gateway_v1_url,
                profile_path,
                runtime_url,
            } => {
                let host = self.for_profile(profile_path.as_deref())?;
                if host.uses_http_runtime(runtime_url.as_deref()) {
                    host.inspect_http(&gateway_v1_url, runtime_url.as_deref(), None)
                } else {
                    host.inspect(&gateway_v1_url)
                }
            }
            DshApplicationHostRequest::Install {
                expected_fingerprint,
                gateway_v1_url,
                profile_path,
                runtime_url,
                secret,
            } => {
                let host = self.for_profile(profile_path.as_deref())?;
                if host.uses_http_runtime(runtime_url.as_deref()) {
                    host.install_http(
                        &expected_fingerprint,
                        &gateway_v1_url,
                        runtime_url.as_deref(),
                        secret.expose_to_host(),
                    )
                } else {
                    host.install(
                        &expected_fingerprint,
                        &gateway_v1_url,
                        secret.expose_to_host(),
                    )
                }
            }
            DshApplicationHostRequest::Uninstall {
                expected_fingerprint,
                gateway_v1_url,
                profile_path,
                runtime_url,
            } => {
                let host = self.for_profile(profile_path.as_deref())?;
                if host.uses_http_runtime(runtime_url.as_deref()) {
                    host.uninstall_http(
                        &expected_fingerprint,
                        &gateway_v1_url,
                        runtime_url.as_deref(),
                    )
                } else {
                    Err(DshApplicationError::precondition(
                        "this DSH target has no running-address uninstall; supply a runtime URL",
                    ))
                }
            }
        }
    }

    fn for_profile(&self, requested: Option<&str>) -> DshApplicationResult<Self> {
        let (home, profile) = match requested {
            None => (self.home.clone(), self.profile.clone()),
            Some(path)
                if same_lexical_path(
                    Path::new(path),
                    &self.home.join("profiles").join(&self.profile),
                ) =>
            {
                (self.home.clone(), self.profile.clone())
            }
            Some(path) => {
                let found = self
                    .discovered_profiles()
                    .into_iter()
                    .find(|candidate| {
                        same_lexical_path(Path::new(&candidate.path), Path::new(path))
                    })
                    .ok_or_else(|| {
                        DshApplicationError::precondition(
                            "selected DSH profile was not found in the allowed homes",
                        )
                    })?;
                (PathBuf::from(found.home), found.name)
            }
        };
        let legacy_bootstrap = same_lexical_path(&home, &self.home) && profile == self.profile;
        Ok(Self {
            data_dir: self.data_dir.clone(),
            home,
            profile,
            legacy_bootstrap,
            scan_user_home: self.scan_user_home.clone(),
            runner: self.runner.clone(),
            dsh_executable: self.dsh_executable.clone(),
            operation: Mutex::new(()),
        })
    }

    fn inspect(&self, gateway_v1_url: &str) -> DshApplicationResult<DshApplicationInspection> {
        let target_paths = self.target_paths();
        let discovered_profiles = self.discovered_profiles();
        let selected_profile_path = self
            .home
            .join("profiles")
            .join(&self.profile)
            .display()
            .to_string();
        let Some(executable) = self.resolve_dsh_executable() else {
            return Ok(DshApplicationInspection {
                selected_profile_path,
                phase: DshApplicationPhase::NotDetected,
                detected: false,
                installed: false,
                install_supported: false,
                activation_required: false,
                version: None,
                detail: Some("DSH was not found on PATH".into()),
                target_paths,
                discovered_profiles,
                fingerprint: None,
                runtime_url: None,
                uninstall_supported: false,
                enabled: false,
                application: None,
            });
        };
        let version = self.read_version(&executable)?;
        // Version is diagnostic/CAS evidence, not an installation allowlist.
        // Let DSH's plugin command and registration readback determine success.

        let package = self.render_package(gateway_v1_url)?;
        let registration = self.registration_state(&package);
        let handoff_pending = credential_handoff_pending(&self.bootstrap_path());
        let fingerprint = Some(self.fingerprint(&executable, &version, gateway_v1_url)?);
        let (phase, installed, install_supported, detail) = match registration {
            RegistrationState::Absent => (
                DshApplicationPhase::Ready,
                false,
                true,
                Some(format!("Ready to install the OCG provider into the DSH {} profile", self.profile)),
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
            RegistrationState::EditorExact if handoff_pending => (
                DshApplicationPhase::Installed,
                true,
                true,
                Some("Installed in DSH Editor. Restart Editor once to import the selected Key".into()),
            ),
            RegistrationState::EditorExact => (
                DshApplicationPhase::Installed,
                true,
                true,
                Some("Installed in the DSH Editor managed profile. Restart Editor to load the OCG provider".into()),
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
            selected_profile_path,
            phase,
            detected: true,
            installed,
            install_supported,
            activation_required: installed && handoff_pending,
            version: Some(version),
            detail,
            target_paths,
            discovered_profiles,
            fingerprint,
            runtime_url: None,
            uninstall_supported: false,
            enabled: installed,
            application: None,
        })
    }

    fn uses_http_runtime(&self, runtime_url: Option<&str>) -> bool {
        runtime_url.is_some_and(|value| !value.trim().is_empty())
            || self.profile == "web"
            || self.profile == "desktop"
    }

    fn resolve_runtime_url(&self, runtime_url: Option<&str>) -> DshApplicationResult<String> {
        let supplied = runtime_url.map(str::trim).filter(|value| !value.is_empty());
        let raw = match supplied {
            Some(url) => url.to_owned(),
            None => match self.profile.as_str() {
                "web" => DEFAULT_WEB_RUNTIME.to_owned(),
                "desktop" => DEFAULT_DESKTOP_RUNTIME.to_owned(),
                _ => {
                    return Err(DshApplicationError::invalid(
                        "this DSH target needs an explicit runtime URL",
                    ));
                }
            },
        };
        let origin = runtime::DshRuntimeOrigin::parse(&raw).map_err(|_| {
            DshApplicationError::invalid("DSH runtime URL is not a permitted loopback HTTP origin")
        })?;
        Ok(origin.as_str().to_owned())
    }

    fn inspect_http(
        &self,
        gateway_v1_url: &str,
        runtime_url: Option<&str>,
        observed: Option<HttpObservedChange>,
    ) -> DshApplicationResult<DshApplicationInspection> {
        let runtime_url = self.resolve_runtime_url(runtime_url)?;
        let origin = runtime::DshRuntimeOrigin::parse(&runtime_url).map_err(|_| {
            DshApplicationError::invalid("DSH runtime URL is not a permitted loopback HTTP origin")
        })?;
        let target_paths = self.http_target_paths();
        let discovered_profiles = self.discovered_profiles();
        let selected_profile_path = self
            .home
            .join("profiles")
            .join(&self.profile)
            .display()
            .to_string();
        let grant = auth::read_browser_session_grant(&self.home);
        let grant_digest = grant.as_ref().ok().map(|secret| secret.digest());
        let fingerprint =
            Some(self.runtime_fingerprint(gateway_v1_url, origin.as_str(), grant_digest, None)?);
        let mut inspection = DshApplicationInspection {
            selected_profile_path,
            phase: DshApplicationPhase::NotDetected,
            detected: false,
            installed: false,
            install_supported: false,
            activation_required: false,
            version: None,
            detail: None,
            target_paths,
            discovered_profiles,
            fingerprint,
            runtime_url: Some(origin.as_str().to_owned()),
            uninstall_supported: false,
            enabled: false,
            application: None,
        };
        let secret = match grant {
            Ok(secret) => secret,
            Err(auth::BrowserGrantError::Missing) => {
                inspection.detail = Some(auth::BrowserGrantError::Missing.message().into());
                return Ok(inspection);
            }
            Err(auth::BrowserGrantError::Unsupported) => {
                inspection.phase = DshApplicationPhase::Incompatible;
                inspection.detail = Some(auth::BrowserGrantError::Unsupported.message().into());
                return Ok(inspection);
            }
        };
        let cookie = secret.mint_cookie(&origin).map_err(|_| {
            DshApplicationError::precondition(auth::BrowserGrantError::Unsupported.message())
        })?;
        let client = match runtime::DshRuntimeClient::connect_session(origin.as_str(), cookie) {
            Ok(client) => client,
            Err(error) if error.kind == runtime::DshRuntimeErrorKind::Invalid => {
                return Err(DshApplicationError::invalid(error.message));
            }
            Err(_) => {
                inspection.detail = Some("DSH running address is not reachable".into());
                return Ok(inspection);
            }
        };
        let bundles = match client.list_bundles() {
            Ok(bundles) => bundles,
            Err(error) => return Ok(self.http_connect_failure(inspection, error)),
        };
        let plugins = match client.list_plugins() {
            Ok(plugins) => plugins,
            Err(error) => return Ok(self.http_connect_failure(inspection, error)),
        };
        let bundle = bundles.iter().find(|bundle| bundle.name == PACKAGE_NAME);
        let plugin = plugins
            .iter()
            .find(|plugin| plugin.module_name == PACKAGE_NAME);
        let handoff_pending = credential_handoff_pending(&self.bootstrap_path());
        inspection.detected = true;
        inspection.fingerprint = Some(self.runtime_fingerprint(
            gateway_v1_url,
            origin.as_str(),
            grant_digest,
            bundle,
        )?);
        if let Some(bundle) = bundle {
            inspection.installed = bundle.installed;
            inspection.enabled = bundle.enabled;
            inspection.uninstall_supported = bundle.installed && bundle.removable;
            inspection.install_supported = true;
            let restart_required = bundle.installed
                && bundle.enabled
                && plugin.is_none_or(|plugin| plugin.fiber_phase.as_deref() != Some("active"));
            inspection.activation_required = inspection.installed && handoff_pending;
            inspection.phase = if inspection.installed {
                DshApplicationPhase::Installed
            } else {
                DshApplicationPhase::Ready
            };
            inspection.application = if restart_required {
                Some(DshApplicationOutcome::RestartRequired)
            } else {
                None
            };
            inspection.detail = Some(if !inspection.installed {
                format!("Ready to install the OCG provider at {}", origin.as_str())
            } else if handoff_pending {
                "Installed. Start or restart DSH once to import the selected Key".into()
            } else if restart_required {
                "Installed. Restart DSH to load the OCG plugin".into()
            } else {
                "Installed. DSH refreshes the authenticated OCG model catalog on use".into()
            });
        } else {
            inspection.phase = DshApplicationPhase::Ready;
            inspection.install_supported = true;
            inspection.detail = Some(format!(
                "Ready to install the OCG provider at {}",
                origin.as_str()
            ));
        }
        if let Some(observed) = observed {
            apply_observed_change(&mut inspection, observed);
        }
        Ok(inspection)
    }

    fn http_connect_failure(
        &self,
        mut inspection: DshApplicationInspection,
        error: runtime::DshRuntimeError,
    ) -> DshApplicationInspection {
        inspection.detected = false;
        inspection.install_supported = false;
        inspection.uninstall_supported = false;
        inspection.detail = Some(
            match error.kind {
                runtime::DshRuntimeErrorKind::Unauthenticated
                | runtime::DshRuntimeErrorKind::Forbidden => {
                    "DSH running address refused the local session"
                }
                _ => "DSH running address is not reachable",
            }
            .into(),
        );
        inspection
    }

    fn runtime_fingerprint(
        &self,
        gateway_v1_url: &str,
        runtime_url: &str,
        grant_digest: Option<[u8; 32]>,
        bundle: Option<&runtime::DshRuntimeBundle>,
    ) -> DshApplicationResult<String> {
        let package = self.render_package(gateway_v1_url)?;
        let bootstrap = read_optional(&self.bootstrap_path())?;
        let mut hash = Sha256::new();
        hash.update(b"open-console-gateway-dsh-runtime-v1\0");
        hash.update(self.home.to_string_lossy().as_bytes());
        hash.update([0]);
        hash.update(self.profile.as_bytes());
        hash.update([0]);
        hash.update(runtime_url.as_bytes());
        hash.update([0]);
        hash.update(package.digest.as_bytes());
        hash.update([0]);
        hash.update(grant_digest.unwrap_or([0u8; 32]));
        hash.update([0]);
        hash.update(Sha256::digest(bootstrap.as_deref().unwrap_or_default()));
        if let Some(bundle) = bundle {
            hash.update(bundle.name.as_bytes());
            hash.update([
                u8::from(bundle.installed),
                u8::from(bundle.enabled),
                u8::from(bundle.removable),
            ]);
            hash.update(bundle.version.as_deref().unwrap_or_default().as_bytes());
        }
        Ok(format!("{:x}", hash.finalize()))
    }

    fn install_http(
        &self,
        expected_fingerprint: &str,
        gateway_v1_url: &str,
        runtime_url: Option<&str>,
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
        let before = self.inspect_http(gateway_v1_url, runtime_url, None)?;
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
        let package = self.render_package(gateway_v1_url)?;
        package.materialize()?;
        let origin = before
            .runtime_url
            .as_deref()
            .ok_or_else(|| DshApplicationError::invalid("DSH runtime URL is missing"))?;
        let grant = auth::read_browser_session_grant(&self.home)
            .map_err(|error| DshApplicationError::precondition(error.message()))?;
        let parsed = runtime::DshRuntimeOrigin::parse(origin).map_err(|_| {
            DshApplicationError::invalid("DSH runtime URL is not a permitted loopback HTTP origin")
        })?;
        let cookie = grant.mint_cookie(&parsed).map_err(|_| {
            DshApplicationError::precondition(auth::BrowserGrantError::Unsupported.message())
        })?;
        let client =
            runtime::DshRuntimeClient::connect_session(origin, cookie).map_err(|error| {
                if error.kind == runtime::DshRuntimeErrorKind::Invalid {
                    DshApplicationError::invalid(error.message)
                } else {
                    DshApplicationError::precondition("DSH running address is not reachable")
                }
            })?;
        let spec = package.path.to_string_lossy().into_owned();
        match client.inspect(&spec) {
            Ok(runtime::DshSpecInspection::Accepted { .. }) => {}
            Ok(runtime::DshSpecInspection::Refused { problem })
                if problem == "already-installed" => {}
            Ok(runtime::DshSpecInspection::Refused { .. }) => {
                return Err(DshApplicationError::precondition(
                    "DSH refused the OCG plugin source",
                ));
            }
            Err(error) if error.kind == runtime::DshRuntimeErrorKind::Unauthenticated => {
                return Err(DshApplicationError::precondition(
                    "DSH running address refused the local session",
                ));
            }
            Err(_) => {
                return Err(DshApplicationError::precondition(
                    "DSH running address is not reachable",
                ));
            }
        }
        let bootstrap = self.bootstrap_path();
        let bootstrap_before = read_optional(&bootstrap)?;
        write_private_atomic(&self.data_dir, &bootstrap, secret.as_bytes())?;
        let request_id = uuid::Uuid::new_v4().to_string();
        // DSH identifies unchanged local dependencies by an explicit package
        // name; a repeated bare directory spec is rejected as ambiguous.
        let install_spec = if before.installed {
            format!("{PACKAGE_NAME}@file:{}", spec.replace('\\', "/"))
        } else {
            spec
        };
        let change = match client.install_bundle(
            &install_spec,
            runtime::DshInstallOptions {
                enabled: Some(true),
                request_id: Some(request_id.clone()),
            },
        ) {
            Ok(result) => result.change,
            Err(error) if error.is_unknown() => {
                match error.request_id().map(|id| client.wait_for_install(id)) {
                    Some(Ok(Some(change))) => change,
                    Some(Ok(None)) | Some(Err(_)) | None => {
                        let mut inspection =
                            self.inspect_http(gateway_v1_url, runtime_url, None)?;
                        inspection.application = None;
                        inspection.detail = Some(
                            "DSH install response was lost; the running address was rechecked without repeating install"
                                .into(),
                        );
                        return Ok(inspection);
                    }
                }
            }
            Err(error) if error.kind == runtime::DshRuntimeErrorKind::Remote => {
                return self.inspect_http(
                    gateway_v1_url,
                    runtime_url,
                    Some(HttpObservedChange::failed(
                        None,
                        error.remote_code.as_deref(),
                    )),
                );
            }
            Err(_) => {
                let mut inspection = self.inspect_http(gateway_v1_url, runtime_url, None)?;
                inspection.application = None;
                inspection.detail = Some("DSH running address did not confirm the install".into());
                return Ok(inspection);
            }
        };
        if is_definitive_no_side_effect(&change) {
            restore_live_if_present(&self.data_dir, &bootstrap, bootstrap_before.as_deref())?;
        }
        self.inspect_http(
            gateway_v1_url,
            runtime_url,
            Some(HttpObservedChange::from_change(&change)),
        )
    }

    fn uninstall_http(
        &self,
        expected_fingerprint: &str,
        gateway_v1_url: &str,
        runtime_url: Option<&str>,
    ) -> DshApplicationResult<DshApplicationInspection> {
        if expected_fingerprint.is_empty() {
            return Err(DshApplicationError::invalid(
                "expectedFingerprint is required",
            ));
        }
        let before = self.inspect_http(gateway_v1_url, runtime_url, None)?;
        if before.fingerprint.as_deref() != Some(expected_fingerprint) {
            return Err(DshApplicationError::conflict(
                "DSH installation state changed after it was inspected",
            ));
        }
        if !before.uninstall_supported {
            return Err(DshApplicationError::precondition(
                before
                    .detail
                    .unwrap_or_else(|| "DSH uninstallation is unavailable".into()),
            ));
        }
        let origin = before
            .runtime_url
            .as_deref()
            .ok_or_else(|| DshApplicationError::invalid("DSH runtime URL is missing"))?;
        let grant = auth::read_browser_session_grant(&self.home)
            .map_err(|error| DshApplicationError::precondition(error.message()))?;
        let parsed = runtime::DshRuntimeOrigin::parse(origin).map_err(|_| {
            DshApplicationError::invalid("DSH runtime URL is not a permitted loopback HTTP origin")
        })?;
        let cookie = grant.mint_cookie(&parsed).map_err(|_| {
            DshApplicationError::precondition(auth::BrowserGrantError::Unsupported.message())
        })?;
        let client = runtime::DshRuntimeClient::connect_session(origin, cookie).map_err(|_| {
            DshApplicationError::precondition("DSH running address is not reachable")
        })?;
        let change = match client.remove_bundle(PACKAGE_NAME) {
            Ok(change) => change,
            Err(error) if error.kind == runtime::DshRuntimeErrorKind::Remote => {
                return self.inspect_http(
                    gateway_v1_url,
                    runtime_url,
                    Some(HttpObservedChange::failed(
                        None,
                        error.remote_code.as_deref(),
                    )),
                );
            }
            Err(_) => {
                let mut inspection = self.inspect_http(gateway_v1_url, runtime_url, None)?;
                inspection.application = None;
                inspection.detail =
                    Some("DSH running address did not confirm the uninstall".into());
                return Ok(inspection);
            }
        };
        let observed = HttpObservedChange::from_change(&change);
        let after = self.inspect_http(gateway_v1_url, runtime_url, Some(observed.clone()))?;
        if after.installed && observed.outcome == DshApplicationOutcome::Applied {
            let mut partial = after;
            partial.detail = Some("DSH still lists the OCG plugin after uninstall".into());
            return Ok(partial);
        }
        Ok(after)
    }

    fn discovered_profiles(&self) -> Vec<DshDiscoveredProfile> {
        match &self.scan_user_home {
            Some(user_home) => discover_profiles(user_home),
            None => discover_home_profiles(&self.home),
        }
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
        let editor_restore = self.editor_restore_plan()?;
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
            dsh_home: self.home.clone(),
            args: vec![
                OsString::from("plugin"),
                OsString::from("--profile"),
                OsString::from(self.profile.as_str()),
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
        if let Some(plan) = editor_restore
            && let Err(error) = plan.commit(&package)
        {
            restore_optional(&self.data_dir, &bootstrap, bootstrap_before.as_deref())?;
            self.restore_registration(&executable, &package, &registration_before)?;
            return Err(error);
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
            RegistrationState::EditorExact => {
                ("add", dsh_package_argument(&self.editor_source_path()))
            }
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
            dsh_home: self.home.clone(),
            args: vec![
                OsString::from("plugin"),
                OsString::from("--profile"),
                OsString::from(self.profile.as_str()),
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
            dsh_home: self.home.clone(),
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
            .join(&self.profile)
            .join("package.json");
        let content = match fs::read(&manifest) {
            Ok(content) => content,
            Err(error) if error.kind() == ErrorKind::NotFound => return RegistrationState::Absent,
            Err(error) => {
                return RegistrationState::Conflict(format!(
                    "could not read the selected DSH profile: {error}"
                ));
            }
        };
        let value: Value = match serde_json::from_slice(&content) {
            Ok(value) => value,
            Err(_) => {
                return RegistrationState::Conflict(
                    "the selected DSH profile manifest is not valid JSON".into(),
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
                if spec == "*" {
                    return self
                        .editor_source_registration(expected)
                        .unwrap_or_else(|| {
                            RegistrationState::Conflict(
                                "DSH has a same-name package that OCG does not own".into(),
                            )
                        });
                }
                let source = match dependency_source(spec, &manifest) {
                    Ok(source) => source,
                    Err(detail) => return RegistrationState::Conflict(detail),
                };
                if same_lexical_path(&source, &self.editor_source_path()) {
                    return self
                        .editor_source_registration(expected)
                        .unwrap_or_else(|| {
                            RegistrationState::Conflict(
                                "DSH Editor has a same-name package that OCG does not own".into(),
                            )
                        });
                }
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

    fn editor_source_path(&self) -> PathBuf {
        self.home
            .join("user-plugins")
            .join("@open-console-gateway")
            .join("dsh-plugin")
    }

    fn editor_source_registration(&self, expected: &RenderedPackage) -> Option<RegistrationState> {
        let source = self.editor_source_path();
        if !self.editor_owned_profile() || !plain_directory(&source) {
            return None;
        }
        let marker_path = source.join(".ocg-owner.json");
        if is_link_or_reparse(&marker_path) {
            return None;
        }
        let marker: Value = serde_json::from_slice(&fs::read(marker_path).ok()?).ok()?;
        if marker.get("owner").and_then(Value::as_str) != Some("open-console-gateway") {
            return None;
        }
        let state: Value =
            serde_json::from_slice(&fs::read(self.home.join("dsh-plugins.json")).ok()?).ok()?;
        let registered = state.get("installed")?.as_array()?.iter().any(|item| {
            item.get("name").and_then(Value::as_str) == Some(PACKAGE_NAME)
                && item.get("spec").and_then(Value::as_str) == Some("ocg-manager")
        });
        if !registered {
            return None;
        }
        let files_match = expected.files.iter().all(|(path, bytes)| {
            fs::read(source.join(path)).ok().as_deref() == Some(bytes.as_slice())
        });
        if marker.get("digest").and_then(Value::as_str) == Some(expected.digest.as_str())
            && files_match
        {
            Some(RegistrationState::EditorExact)
        } else {
            Some(RegistrationState::OwnedOlder(source))
        }
    }

    fn editor_owned_profile(&self) -> bool {
        if self.profile != "dsh-editor" {
            return false;
        }
        let marker = self
            .home
            .join("profiles")
            .join(&self.profile)
            .join(".dsh-editor-owner.json");
        if is_link_or_reparse(&marker) {
            return false;
        }
        fs::read(marker)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .is_some_and(|value| {
                value.get("app").and_then(Value::as_str) == Some("dsh-editor")
                    && value.get("schema").and_then(Value::as_u64) == Some(1)
            })
    }

    fn editor_restore_plan(&self) -> DshApplicationResult<Option<EditorRestorePlan>> {
        if self.profile != "dsh-editor" {
            return Ok(None);
        }
        let owner = self
            .home
            .join("profiles")
            .join(&self.profile)
            .join(".dsh-editor-owner.json");
        match fs::symlink_metadata(&owner) {
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(internal(error.to_string())),
            Ok(_) if !self.editor_owned_profile() => {
                return Err(DshApplicationError::conflict(
                    "DSH Editor profile ownership marker is invalid",
                ));
            }
            Ok(_) => {}
        }
        let source = self.editor_source_path();
        let source_exists = fs::symlink_metadata(&source).is_ok();
        if source_exists {
            if !plain_directory(&source) {
                return Err(DshApplicationError::conflict(
                    "DSH Editor has a conflicting OCG plugin source",
                ));
            }
            if is_link_or_reparse(&source.join(".ocg-owner.json")) {
                return Err(DshApplicationError::conflict(
                    "DSH Editor OCG plugin ownership marker is a link",
                ));
            }
            let marker: Value = serde_json::from_slice(
                &fs::read(source.join(".ocg-owner.json")).map_err(|_| {
                    DshApplicationError::conflict(
                        "DSH Editor has a same-name plugin not owned by OCG",
                    )
                })?,
            )
            .map_err(|_| {
                DshApplicationError::conflict("DSH Editor OCG plugin ownership marker is invalid")
            })?;
            if marker.get("owner").and_then(Value::as_str) != Some("open-console-gateway") {
                return Err(DshApplicationError::conflict(
                    "DSH Editor has a same-name plugin not owned by OCG",
                ));
            }
        }
        let state_path = self.home.join("dsh-plugins.json");
        if is_link_or_reparse(&state_path) {
            return Err(DshApplicationError::conflict(
                "DSH Editor plugin state is a link",
            ));
        }
        let state_before = read_optional(&state_path)?;
        let state = editor_plugin_state(state_before.as_deref())?;
        if state
            .get("installed")
            .and_then(Value::as_array)
            .is_some_and(|items| {
                items.iter().any(|item| {
                    item.get("name").and_then(Value::as_str) == Some(PACKAGE_NAME)
                        && item.get("spec").and_then(Value::as_str) != Some("ocg-manager")
                })
            })
        {
            return Err(DshApplicationError::conflict(
                "DSH Editor already tracks this plugin from another source",
            ));
        }
        Ok(Some(EditorRestorePlan {
            home: self.home.clone(),
            source,
            source_exists,
            source_marker_before: if source_exists {
                read_optional(&self.editor_source_path().join(".ocg-owner.json"))?
            } else {
                None
            },
            state_path,
            state_before,
        }))
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
                .join(&self.profile)
                .join("package.json"),
        )?;
        let bootstrap = read_optional(&self.bootstrap_path())?;
        let mut hash = Sha256::new();
        hash.update(b"open-console-gateway-dsh-install-v1\0");
        hash.update(self.home.to_string_lossy().as_bytes());
        hash.update([0]);
        hash.update(self.profile.as_bytes());
        hash.update([0]);
        hash.update(executable.path.to_string_lossy().as_bytes());
        hash.update([0]);
        hash.update(version.as_bytes());
        hash.update([0]);
        hash.update(package.digest.as_bytes());
        hash.update([0]);
        hash.update(Sha256::digest(manifest.as_deref().unwrap_or_default()));
        hash.update([0]);
        hash.update(Sha256::digest(bootstrap.as_deref().unwrap_or_default()));
        if self.editor_owned_profile() {
            let state = read_optional(&self.home.join("dsh-plugins.json"))?;
            hash.update(Sha256::digest(state.as_deref().unwrap_or_default()));
            let owner = read_optional(&self.editor_source_path().join(".ocg-owner.json"))?;
            hash.update(Sha256::digest(owner.as_deref().unwrap_or_default()));
        }
        Ok(format!("{:x}", hash.finalize()))
    }

    fn bootstrap_path(&self) -> PathBuf {
        if self.legacy_bootstrap {
            return self.data_dir.join(BOOTSTRAP_FILE);
        }
        let mut hash = Sha256::new();
        hash.update(self.home.to_string_lossy().as_bytes());
        hash.update([0]);
        hash.update(self.profile.as_bytes());
        let digest = format!("{:x}", hash.finalize());
        self.data_dir
            .join("applications/dsh/handoffs")
            .join(&digest[..24])
    }

    fn target_paths(&self) -> Vec<String> {
        let mut paths = vec![
            self.home
                .join("profiles")
                .join(&self.profile)
                .join("package.json")
                .display()
                .to_string(),
            self.data_dir.join(PACKAGE_ROOT).display().to_string(),
            self.bootstrap_path().display().to_string(),
        ];
        if self.editor_owned_profile() {
            paths.push(self.editor_source_path().display().to_string());
            paths.push(self.home.join("dsh-plugins.json").display().to_string());
        }
        paths
    }

    fn http_target_paths(&self) -> Vec<String> {
        vec![
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

struct EditorRestorePlan {
    home: PathBuf,
    source: PathBuf,
    source_exists: bool,
    source_marker_before: Option<Vec<u8>>,
    state_path: PathBuf,
    state_before: Option<Vec<u8>>,
}

fn editor_plugin_state(bytes: Option<&[u8]>) -> DshApplicationResult<Value> {
    let value = match bytes {
        Some(bytes) => serde_json::from_slice::<Value>(bytes).map_err(|_| {
            DshApplicationError::conflict("DSH Editor plugin state is not valid JSON")
        })?,
        None => serde_json::json!({"schema": 1, "overrides": {}, "presets": {}, "installed": []}),
    };
    if value.get("schema").and_then(Value::as_u64) != Some(1)
        || !value.get("installed").is_some_and(Value::is_array)
    {
        return Err(DshApplicationError::conflict(
            "DSH Editor plugin state has an unsupported schema",
        ));
    }
    Ok(value)
}

impl EditorRestorePlan {
    fn commit(self, package: &RenderedPackage) -> DshApplicationResult<()> {
        if read_optional(&self.state_path)? != self.state_before
            || fs::symlink_metadata(&self.source).is_ok() != self.source_exists
            || is_link_or_reparse(&self.source.join(".ocg-owner.json"))
            || read_optional(&self.source.join(".ocg-owner.json"))? != self.source_marker_before
        {
            return Err(DshApplicationError::conflict(
                "DSH Editor plugin state changed during installation",
            ));
        }
        let mut state = editor_plugin_state(self.state_before.as_deref())?;
        let installed = state
            .get_mut("installed")
            .and_then(Value::as_array_mut)
            .ok_or_else(|| internal("DSH Editor plugin state has no installed list"))?;
        installed.retain(|item| item.get("name").and_then(Value::as_str) != Some(PACKAGE_NAME));
        let package_manifest: Value = serde_json::from_slice(
            package
                .files
                .get(Path::new("package.json"))
                .ok_or_else(|| internal("OCG DSH package has no manifest"))?,
        )
        .map_err(|error| internal(error.to_string()))?;
        let version = package_manifest
            .get("version")
            .and_then(Value::as_str)
            .unwrap_or("");
        installed.push(
            serde_json::json!({"name": PACKAGE_NAME, "spec": "ocg-manager", "version": version}),
        );
        let mut state_bytes =
            serde_json::to_vec_pretty(&state).map_err(|error| internal(error.to_string()))?;
        state_bytes.push(b'\n');
        if state_bytes.len() > MAX_PACKAGE_BYTES as usize {
            return Err(DshApplicationError::precondition(
                "DSH Editor plugin state exceeds the size limit",
            ));
        }

        let parent = self
            .source
            .parent()
            .ok_or_else(|| internal("DSH Editor plugin source has no parent"))?;
        ensure_safe_directory_chain(&self.home, parent)?;
        let nonce = uuid::Uuid::new_v4().simple().to_string();
        let stage = parent.join(format!(".ocg-stage-{nonce}"));
        let backup = parent.join(format!(".ocg-backup-{nonce}"));
        fs::create_dir(&stage).map_err(|error| internal(error.to_string()))?;
        let staged = (|| -> DshApplicationResult<()> {
            for (relative, bytes) in &package.files {
                fs::write(stage.join(relative), bytes)
                    .map_err(|error| internal(error.to_string()))?;
            }
            let marker =
                serde_json::json!({"owner": "open-console-gateway", "digest": package.digest});
            fs::write(
                stage.join(".ocg-owner.json"),
                serde_json::to_vec(&marker).map_err(|error| internal(error.to_string()))?,
            )
            .map_err(|error| internal(error.to_string()))?;
            Ok(())
        })();
        if let Err(error) = staged {
            let _ = fs::remove_dir_all(&stage);
            return Err(error);
        }
        if self.source_exists
            && let Err(error) = fs::rename(&self.source, &backup)
        {
            let _ = fs::remove_dir_all(&stage);
            return Err(internal(error.to_string()));
        }
        if let Err(error) = fs::rename(&stage, &self.source) {
            let restored = if self.source_exists {
                fs::rename(&backup, &self.source).map_err(|io| io.to_string())
            } else {
                Ok(())
            };
            let _ = fs::remove_dir_all(&stage);
            if let Err(restore) = restored {
                return Err(internal(format!(
                    "{error}; DSH Editor plugin source rollback failed: {restore}"
                )));
            }
            return Err(internal(error.to_string()));
        }
        if let Err(error) = write_private_atomic(&self.home, &self.state_path, &state_bytes) {
            let state_restore =
                restore_optional(&self.home, &self.state_path, self.state_before.as_deref());
            let source_restore = (|| -> DshApplicationResult<()> {
                if !safe_directory_chain(&self.home, &self.source) {
                    return Err(DshApplicationError::conflict(
                        "DSH Editor OCG source changed during rollback",
                    ));
                }
                fs::remove_dir_all(&self.source).map_err(|io| internal(io.to_string()))?;
                if self.source_exists {
                    fs::rename(&backup, &self.source).map_err(|io| internal(io.to_string()))?;
                }
                Ok(())
            })();
            if let Err(restore) = state_restore.and(source_restore) {
                return Err(internal(format!(
                    "{error}; DSH Editor state rollback failed: {restore}"
                )));
            }
            return Err(error);
        }
        if self.source_exists && safe_directory_chain(&self.home, &backup) {
            let _ = fs::remove_dir_all(&backup);
        }
        Ok(())
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
    EditorExact,
    OwnedOlder(PathBuf),
    Conflict(String),
}

fn registration_matches(left: &RegistrationState, right: &RegistrationState) -> bool {
    match (left, right) {
        (RegistrationState::Absent, RegistrationState::Absent)
        | (RegistrationState::Exact, RegistrationState::Exact)
        | (RegistrationState::EditorExact, RegistrationState::EditorExact) => true,
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
    dsh_home: PathBuf,
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
        .env("DSH_HOME", &command.dsh_home)
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

/// Inspect only the user's `.dsh` and `.dsh-*` homes, one profile level deep.
/// Directory links and invalid manifests are not presented as real profiles.
fn discover_profiles(user_home: &Path) -> Vec<DshDiscoveredProfile> {
    let Ok(entries) = fs::read_dir(user_home) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if name != ".dsh" && !(name.starts_with(".dsh-") && name.len() > ".dsh-".len()) {
            continue;
        }
        found.extend(discover_home_profiles(&entry.path()));
    }
    found.sort_by(|left, right| (&left.home, &left.name).cmp(&(&right.home, &right.name)));
    found
}

fn discover_home_profiles(home: &Path) -> Vec<DshDiscoveredProfile> {
    let profiles = home.join("profiles");
    if !plain_directory(home) || !plain_directory(&profiles) {
        return Vec::new();
    }
    let Ok(entries) = fs::read_dir(&profiles) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if name.is_empty()
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        {
            continue;
        }
        let profile = entry.path();
        if !plain_directory(&profile) {
            continue;
        }
        let manifest = profile.join("package.json");
        let Ok(metadata) = fs::symlink_metadata(&manifest) else {
            continue;
        };
        if !metadata.is_file() || is_link_or_reparse(&manifest) || metadata.len() > 1024 * 1024 {
            continue;
        }
        let Ok(content) = fs::read(&manifest) else {
            continue;
        };
        let Ok(value) = serde_json::from_slice::<Value>(&content) else {
            continue;
        };
        if !value.pointer("/dsh/profile").is_some_and(Value::is_object) {
            continue;
        }
        found.push(DshDiscoveredProfile {
            home: home.display().to_string(),
            name: name.to_owned(),
            path: profile.display().to_string(),
        });
    }
    found.sort_by(|left, right| left.name.cmp(&right.name));
    found
}

fn plain_directory(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|metadata| metadata.is_dir() && !is_link_or_reparse(path))
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

fn map_runtime_application(application: runtime::DshRuntimeApplication) -> DshApplicationOutcome {
    match application {
        runtime::DshRuntimeApplication::Applied => DshApplicationOutcome::Applied,
        runtime::DshRuntimeApplication::RestartRequired => DshApplicationOutcome::RestartRequired,
        runtime::DshRuntimeApplication::Overridden => DshApplicationOutcome::Overridden,
        runtime::DshRuntimeApplication::Failed => DshApplicationOutcome::Failed,
        runtime::DshRuntimeApplication::Cancelled => DshApplicationOutcome::Cancelled,
    }
}

#[derive(Clone)]
struct HttpObservedChange {
    outcome: DshApplicationOutcome,
    stage: Option<String>,
    error_code: Option<String>,
}

impl HttpObservedChange {
    fn from_change(change: &runtime::DshChangeResult) -> Self {
        Self {
            outcome: map_runtime_application(change.application),
            stage: change.stage.clone(),
            error_code: change.error_code.clone(),
        }
    }

    fn failed(stage: Option<&str>, error_code: Option<&str>) -> Self {
        Self {
            outcome: DshApplicationOutcome::Failed,
            stage: stage.map(str::to_owned),
            error_code: error_code.map(str::to_owned),
        }
    }
}

fn apply_observed_change(inspection: &mut DshApplicationInspection, observed: HttpObservedChange) {
    inspection.application = Some(observed.outcome);
    match observed.outcome {
        DshApplicationOutcome::Applied => {}
        DshApplicationOutcome::RestartRequired => {
            inspection.detail = Some("Installed. Restart DSH to load the OCG plugin".into());
        }
        DshApplicationOutcome::Failed
        | DshApplicationOutcome::Cancelled
        | DshApplicationOutcome::Overridden => {
            inspection.detail = Some(format_observed_detail(
                observed.outcome,
                observed.stage.as_deref(),
                observed.error_code.as_deref(),
            ));
        }
    }
}

fn format_observed_detail(
    outcome: DshApplicationOutcome,
    stage: Option<&str>,
    error_code: Option<&str>,
) -> String {
    let mut detail = match outcome {
        DshApplicationOutcome::Failed => "DSH running address reported a failed change".to_owned(),
        DshApplicationOutcome::Cancelled => "DSH running address cancelled the change".to_owned(),
        DshApplicationOutcome::Overridden => "DSH running address overrode the change".to_owned(),
        DshApplicationOutcome::Applied | DshApplicationOutcome::RestartRequired => {
            return String::new();
        }
    };
    if let Some(stage) = stage.filter(|value| safe_runtime_token(value)) {
        detail.push_str(&format!(" ({stage})"));
    }
    if let Some(code) = error_code.filter(|value| safe_runtime_token(value)) {
        detail.push_str(&format!(" [{code}]"));
    }
    detail
}

fn safe_runtime_token(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn is_definitive_no_side_effect(change: &runtime::DshChangeResult) -> bool {
    change.application == runtime::DshRuntimeApplication::Cancelled && change.changed != Some(true)
}

fn restore_live_if_present(
    trusted_root: &Path,
    path: &Path,
    bytes: Option<&[u8]>,
) -> DshApplicationResult<()> {
    if !path.exists() {
        return Ok(());
    }
    restore_optional(trusted_root, path, bytes)
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

pub(super) fn is_link_or_reparse(path: &Path) -> bool {
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
fn windows_environment_block(home: &Path) -> Result<Vec<u16>, String> {
    use std::os::windows::ffi::OsStrExt;

    let mut entries: Vec<(OsString, OsString)> = std::env::vars_os()
        .filter(|(name, _)| !name.to_string_lossy().eq_ignore_ascii_case("DSH_HOME"))
        .collect();
    entries.push((OsString::from("DSH_HOME"), home.as_os_str().to_os_string()));
    entries.sort_by_key(|(name, _)| name.to_string_lossy().to_ascii_uppercase());
    let mut block = Vec::new();
    for (name, value) in entries {
        let key: Vec<u16> = name.encode_wide().collect();
        let value: Vec<u16> = value.encode_wide().collect();
        if key.contains(&0) || value.contains(&0) {
            return Err("DSH command environment contains an invalid NUL".into());
        }
        block.extend(key);
        block.push(b'=' as u16);
        block.extend(value);
        block.push(0);
    }
    block.push(0);
    Ok(block)
}

#[cfg(windows)]
fn run_windows_command(command: &CommandSpec) -> Result<CommandOutput, String> {
    use windows_sys::Win32::Foundation::{WAIT_OBJECT_0, WAIT_TIMEOUT};
    use windows_sys::Win32::System::Threading::{
        CREATE_NO_WINDOW, CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, CreateProcessW,
        GetExitCodeProcess, PROCESS_INFORMATION, ResumeThread, STARTF_USESTDHANDLES, STARTUPINFOW,
        WaitForSingleObject,
    };
    let (application, mut command_line) = windows_process_command_line(command)?;
    let environment = windows_environment_block(&command.dsh_home)?;
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
            CREATE_SUSPENDED | CREATE_NO_WINDOW | CREATE_UNICODE_ENVIRONMENT,
            environment.as_ptr().cast(),
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

mod auth;
pub(crate) mod runtime;

#[cfg(test)]
#[path = "dsh_application_host/tests.rs"]
mod tests;

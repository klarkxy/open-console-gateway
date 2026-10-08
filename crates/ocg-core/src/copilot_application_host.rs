//! Native VS Code lifecycle, with immutable package and Profile-owned handoff.
use crate::byok_application::{ByokError, ByokResult};
use crate::byok_application_host::fs as safe;
use crate::copilot_application::*;
use crate::copilot_extension_package::{self as package, EXTENSION_ID, EXTENSION_VERSION};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
mod runtime;
use runtime::{Installation, ProcessRunner, Runner, discover};

const HANDOFF: &str = "credential-handoff.json";
const CLAIMED: &str = "credential-handoff.claimed.json";
const ACK: &str = "activation.json";

pub fn register(core: &crate::state::CoreState) {
    let data_dir = crate::dsh_application_host::absolute_host_path(core.data_dir());
    let runner: Arc<dyn Runner> = Arc::new(ProcessRunner);
    let operation = Mutex::new(());
    core.set_copilot_application_host(Arc::new(move |request| {
        let _guard = operation
            .lock()
            .map_err(|_| ByokError::internal("Copilot operation lock is unavailable"))?;
        // Code updates can move cli.js to another version directory between requests.
        Host {
            data_dir: data_dir.clone(),
            installations: discover(),
            runner: runner.clone(),
        }
        .execute(request)
    }));
}

struct Host {
    data_dir: PathBuf,
    installations: Vec<Installation>,
    runner: Arc<dyn Runner>,
}
#[derive(Clone)]
struct Target {
    wire: CopilotTarget,
    installation: Installation,
    storage: PathBuf,
    extensions: PathBuf,
    receipt: PathBuf,
}
impl Host {
    fn target(&self, input: CopilotTarget) -> ByokResult<Option<Target>> {
        let installation = match input.installation.as_deref() {
            Some(id) => self.installations.iter().find(|i| i.info.id == id),
            None => self.installations.first(),
        };
        let Some(installation) = installation else {
            if input.installation.is_some() {
                return Err(ByokError::invalid(
                    "The selected VS Code installation is unavailable",
                ));
            }
            return Ok(None);
        };
        let user_data = absolute(
            input
                .user_data_dir
                .as_deref()
                .unwrap_or(&installation.info.user_data_dir),
        )?;
        let extensions = absolute(
            input
                .extensions_dir
                .as_deref()
                .unwrap_or(&installation.info.extensions_dir),
        )?;
        if installation.portable.is_some()
            && (user_data != absolute(&installation.info.user_data_dir)?
                || extensions != absolute(&installation.info.extensions_dir)?)
        {
            return Err(ByokError::precondition(
                "Portable VS Code ignores directory overrides; use its detected portable directories",
            ));
        }
        let profile = input.profile.filter(|s| !s.trim().is_empty());
        if profile
            .as_ref()
            .is_some_and(|p| p.len() > 256 || p.chars().any(char::is_control))
        {
            return Err(ByokError::invalid("VS Code Profile name is invalid"));
        }
        let storage = profile_storage(&user_data, profile.as_deref())?.join(EXTENSION_ID);
        safe::reject_symlink_ancestors(&storage)?;
        safe::reject_symlink_ancestors(&extensions)?;
        let wire = CopilotTarget {
            installation: Some(installation.info.id.clone()),
            profile,
            user_data_dir: Some(user_data.to_string_lossy().into()),
            extensions_dir: Some(extensions.to_string_lossy().into()),
        };
        let hash = safe::sha256_hex(
            &serde_json::to_vec(&wire)
                .map_err(|_| ByokError::internal("Cannot encode Copilot target"))?,
        );
        Ok(Some(Target {
            wire,
            installation: installation.clone(),
            storage,
            extensions,
            receipt: self
                .data_dir
                .join("applications/copilot/receipts")
                .join(format!("{hash}.json")),
        }))
    }
    fn execute(&self, request: CopilotApplicationHostRequest) -> ByokResult<CopilotInspection> {
        match request {
            CopilotApplicationHostRequest::Inspect { target } => self.inspect_input(target),
            CopilotApplicationHostRequest::Install {
                target,
                expected_fingerprint,
                gateway_v1_url,
                secret,
            } => {
                let target = self.required_target(target)?;
                let before = self.require_fingerprint(&target, &expected_fingerprint)?;
                if !before.install_supported {
                    return Err(ByokError::precondition(
                        "Copilot installation is unavailable or foreign; inspect the target",
                    ));
                }
                if read_json(&target.storage.join(CLAIMED))?.is_some() {
                    return Err(ByokError::conflict(
                        "VS Code is importing the previous connection; reload and inspect before reconnecting",
                    ));
                }
                let gateway_v1_url = validated_gateway(&gateway_v1_url)?;
                let bytes = package::vsix_bytes()?;
                let artifact = self
                    .data_dir
                    .join("applications/copilot/packages")
                    .join(format!("{}.vsix", safe::sha256_hex(&bytes)));
                if let Some(existing) = safe::read_regular_file(&artifact)? {
                    if existing != bytes {
                        return Err(ByokError::conflict("Managed Copilot package changed"));
                    }
                } else {
                    safe::ensure_safe_directory_chain(artifact.parent().unwrap())?;
                    safe::write_private_atomic(&artifact, &bytes)?;
                }
                // Install/readback before any credential handoff; a CLI failure never overwrites the existing connection.
                let mut args = target.args();
                args.extend([
                    "--install-extension".into(),
                    artifact.to_string_lossy().into(),
                    "--force".into(),
                ]);
                let previous = read_json(&target.receipt)?;
                let attempt = (|| {
                    self.runner.run(&target.installation, &args)?;
                    let installed = self.installed(&target)?;
                    if installed.as_deref() != Some(EXTENSION_VERSION) {
                        return Err(ByokError::precondition(
                            "VS Code did not register the expected Copilot extension; inspect before retrying",
                        ));
                    }
                    if runtime_at(&target, EXTENSION_VERSION)?.as_deref()
                        != Some(package::runtime_digest().as_str())
                    {
                        return Err(ByokError::conflict(
                            "VS Code installed a different Copilot runtime",
                        ));
                    }
                    Ok::<_, ByokError>(())
                })();
                if let Err(error) = attempt {
                    // Restore the retained OCG artifact if a failed CLI changed the installed runtime.
                    if let Some(prior) = &previous {
                        if registered_version(&target)? != before.extension_version
                            || before
                                .extension_version
                                .as_deref()
                                .map(|version| runtime_at(&target, version))
                                .transpose()?
                                .flatten()
                                .as_deref()
                                != prior["runtimeDigest"].as_str()
                        {
                            let rollback = prior["packagePath"].as_str().map(PathBuf::from).ok_or_else(||ByokError::precondition("Copilot update failed; previous package provenance is unavailable"))?;
                            let allowed = self.data_dir.join("applications/copilot/packages");
                            let rollback = safe::canonical_lexical_path(&rollback)?;
                            if !rollback.starts_with(allowed) {
                                return Err(ByokError::conflict(
                                    "Copilot rollback package is outside the managed directory",
                                ));
                            }
                            let retained = safe::read_regular_file(&rollback)?.ok_or_else(||ByokError::precondition("Copilot update failed; the previous package is unavailable"))?;
                            if rollback.file_stem().and_then(|s| s.to_str())
                                != Some(safe::sha256_hex(&retained).as_str())
                            {
                                return Err(ByokError::conflict(
                                    "Retained Copilot package changed",
                                ));
                            }
                            let mut restore = target.args();
                            restore.extend([
                                "--install-extension".into(),
                                rollback.to_string_lossy().into(),
                                "--force".into(),
                            ]);
                            if self.runner.run(&target.installation, &restore).is_err()
                                || self.installed(&target)? != before.extension_version
                                || before
                                    .extension_version
                                    .as_deref()
                                    .map(|v| runtime_at(&target, v))
                                    .transpose()?
                                    .flatten()
                                    .as_deref()
                                    != prior["runtimeDigest"].as_str()
                            {
                                return Err(ByokError::precondition(
                                    "Copilot update partially changed the installation; inspect and restore its retained VSIX before reconnecting",
                                ));
                            }
                        }
                    } else if registered_version(&target)?.as_deref() == Some(EXTENSION_VERSION)
                        && runtime_at(&target, EXTENSION_VERSION)?.as_deref()
                            == Some(package::runtime_digest().as_str())
                    {
                        safe::ensure_safe_directory_chain(target.receipt.parent().unwrap())?;
                        let provenance = json!({"schemaVersion":1,"target":target.wire,"storagePath":target.storage,"runtimeDigest":package::runtime_digest(),"packagePath":artifact,"connectionId":uuid::Uuid::new_v4().to_string()});
                        safe::write_private_atomic(
                            &target.receipt,
                            &serde_json::to_vec(&provenance).unwrap(),
                        )?;
                    }
                    return Err(error);
                }
                let connection_id = uuid::Uuid::new_v4().to_string();
                let receipt = json!({"schemaVersion":1,"target":target.wire,"storagePath":target.storage,"runtimeDigest":package::runtime_digest(),"packagePath":artifact,"connectionId":connection_id});
                safe::ensure_safe_directory_chain(&target.storage)?;
                safe::ensure_safe_directory_chain(target.receipt.parent().unwrap())?;
                // Persist provenance first. If handoff writing fails, inspection reports managed pending rather than foreign installed.
                safe::write_private_atomic(
                    &target.receipt,
                    &serde_json::to_vec(&receipt).unwrap(),
                )?;
                let handoff = json!({"schemaVersion":1,"operation":"connect","connectionId":connection_id,"storagePath":target.storage,"runtimeDigest":package::runtime_digest(),"gatewayV1Url":validated_gateway(&gateway_v1_url)?,"key":secret.expose_to_host()});
                safe::write_private_atomic(
                    &target.storage.join(HANDOFF),
                    &serde_json::to_vec(&handoff).unwrap(),
                )?;
                let open_error = self.open(&target).err();
                let mut result = self.inspect(&target)?;
                if open_error.is_some() {
                    result.detail=Some("Extension installed; open the selected VS Code Profile to import the pending connection".into());
                }
                Ok(result)
            }
            CopilotApplicationHostRequest::Disconnect {
                target,
                expected_fingerprint,
            } => {
                let target = self.required_target(target)?;
                let state = self.require_fingerprint(&target, &expected_fingerprint)?;
                if !state.uninstall_supported {
                    return Err(ByokError::precondition(
                        "Only an OCG-owned Copilot connection can be disconnected",
                    ));
                }
                self.disconnect(&target)?;
                self.inspect(&target)
            }
            CopilotApplicationHostRequest::Uninstall {
                target,
                expected_fingerprint,
            } => {
                let target = self.required_target(target)?;
                let state = self.require_fingerprint(&target, &expected_fingerprint)?;
                if !state.uninstall_supported {
                    return Err(ByokError::precondition(
                        "Only an OCG-owned Copilot extension can be removed",
                    ));
                }
                if state.status != CopilotStatus::Disconnected {
                    self.disconnect(&target)?;
                    let pending = self.inspect(&target)?;
                    if pending.status != CopilotStatus::Disconnected {
                        return Ok(pending);
                    }
                }
                let others = self.other_profiles(&target)?;
                let mut args = target.args();
                args.extend(["--uninstall-extension".into(), EXTENSION_ID.into()]);
                self.runner.run(&target.installation, &args)?;
                if self.installed(&target)?.is_some() {
                    return Err(ByokError::precondition(
                        "VS Code did not remove the selected Profile extension",
                    ));
                }
                for other in others {
                    if registered_version(&other)?.is_some()
                        && runtime_at(&other, EXTENSION_VERSION)?.is_none()
                    {
                        return Err(ByokError::precondition(
                            "VS Code removed shared files; reinstall the other Profile before continuing",
                        ));
                    }
                }
                for p in [
                    &target.receipt,
                    &target.storage.join(HANDOFF),
                    &target.storage.join(ACK),
                ] {
                    safe::remove_regular_file(p)?;
                }
                self.inspect(&target)
            }
        }
    }
    fn inspect_input(&self, input: CopilotTarget) -> ByokResult<CopilotInspection> {
        if let Some(target) = self.target(input.clone())? {
            self.inspect(&target)
        } else {
            let mut view = CopilotInspection::unsupported(input);
            view.status = CopilotStatus::NotDetected;
            view.discovered_installations =
                self.installations.iter().map(|i| i.info.clone()).collect();
            Ok(view)
        }
    }
    fn required_target(&self, input: CopilotTarget) -> ByokResult<Target> {
        self.target(input)?
            .ok_or_else(|| ByokError::precondition("VS Code is not detected on this host"))
    }
    fn installed(&self, target: &Target) -> ByokResult<Option<String>> {
        let mut args = target.args();
        args.extend(["--list-extensions".into(), "--show-versions".into()]);
        let output = self.runner.run(&target.installation, &args)?;
        let matches = output
            .lines()
            .filter_map(|s| s.trim().split_once('@'))
            .filter(|(id, _)| id.eq_ignore_ascii_case(EXTENSION_ID))
            .map(|(_, v)| v.to_owned())
            .collect::<Vec<_>>();
        if matches.len() > 1 {
            return Err(ByokError::conflict(
                "Duplicate Copilot extension registrations",
            ));
        }
        Ok(matches.into_iter().next())
    }
    fn inspect(&self, target: &Target) -> ByokResult<CopilotInspection> {
        let version = registered_version(target)?;
        let receipt = read_json(&target.receipt)?;
        let ack = read_json(&target.storage.join(ACK))?;
        let pending = read_json(&target.storage.join(HANDOFF))?;
        let claimed = read_json(&target.storage.join(CLAIMED))?;
        let digest = version
            .as_deref()
            .map(|v| runtime_at(target, v))
            .transpose()?
            .flatten();
        let owned = receipt.as_ref().is_some_and(|r| {
            r["schemaVersion"] == 1
                && r["target"] == serde_json::to_value(&target.wire).unwrap()
                && r["storagePath"] == json!(target.storage)
                && r["connectionId"]
                    .as_str()
                    .is_some_and(|s| uuid::Uuid::parse_str(s).is_ok())
                && r["runtimeDigest"]
                    .as_str()
                    .is_some_and(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()))
                && digest.as_deref().is_some_and(|actual| {
                    r["runtimeDigest"].as_str() == Some(actual)
                        || actual == package::runtime_digest()
                })
        });
        let mut status = if version.is_some() {
            if owned {
                CopilotStatus::InstalledPending
            } else {
                CopilotStatus::Conflict
            }
        } else {
            CopilotStatus::Ready
        };
        let recognized = ack.as_ref().filter(|a| {
            owned
                && a["schemaVersion"] == 1
                && receipt.as_ref().is_some_and(|r| {
                    a["connectionId"] == r["connectionId"]
                        && a["runtimeDigest"] == r["runtimeDigest"]
                })
        });
        let connection_status = recognized
            .and_then(|a| a["status"].as_str())
            .filter(|s| {
                [
                    "connected",
                    "disconnected",
                    "authentication_failed",
                    "unavailable",
                    "metadata_required",
                ]
                .contains(s)
            })
            .map(str::to_owned);
        if pending.is_none()
            && claimed.is_none()
            && let Some(s) = connection_status.as_deref()
        {
            status = match s {
                "connected" | "metadata_required" => CopilotStatus::Connected,
                "disconnected" => CopilotStatus::Disconnected,
                _ => CopilotStatus::ConnectionError,
            };
        }
        let vscode_version = target.installation.info.version.clone();
        let compatible = vscode_version.as_deref().is_some_and(|v| {
            let mut p = v.split(['.', '-']);
            let major = p.next().and_then(|x| x.parse::<u32>().ok()).unwrap_or(0);
            let minor = p.next().and_then(|x| x.parse::<u32>().ok()).unwrap_or(0);
            major > 1 || (major == 1 && minor >= 141)
        });
        let state_bytes=serde_json::to_vec(&json!({"target":target.wire,"installationVersion":vscode_version,"cliPath":target.installation.cli,"version":version,"runtime":digest,"receipt":receipt,"ack":ack,"handoff":pending.as_ref().map(|p|safe::sha256_hex(&serde_json::to_vec(p).unwrap())),"claim":claimed.as_ref().map(|p|safe::sha256_hex(&serde_json::to_vec(p).unwrap()))})).unwrap();
        let mut installations = self
            .installations
            .iter()
            .map(|i| i.info.clone())
            .collect::<Vec<_>>();
        if let Some(i) = installations
            .iter_mut()
            .find(|i| Some(&i.id) == target.wire.installation.as_ref())
        {
            i.version = vscode_version;
        }
        Ok(CopilotInspection {
            target: target.wire.clone(),
            status,
            installed: version.is_some(),
            install_supported: compatible && status != CopilotStatus::Conflict,
            uninstall_supported: version.is_some() && owned,
            activation_required: status == CopilotStatus::InstalledPending,
            discovered_installations: installations,
            fingerprint: Some(safe::sha256_hex(&state_bytes)),
            extension_version: version,
            detail: if !compatible {
                Some("VS Code 1.141 or newer is required".into())
            } else {
                None
            },
            connection_status,
            model_count: recognized
                .and_then(|a| a["modelCount"].as_u64())
                .and_then(|n| u32::try_from(n).ok()),
            metadata_missing: recognized
                .and_then(|a| a["metadataMissing"].as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .filter(|s| s.len() <= 512 && !s.chars().any(char::is_control))
                        .take(10000)
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default(),
        })
    }
    fn require_fingerprint(
        &self,
        target: &Target,
        expected: &str,
    ) -> ByokResult<CopilotInspection> {
        let before = self.inspect(target)?;
        if before.fingerprint.as_deref() != Some(expected) {
            return Err(ByokError::conflict(
                "Copilot target changed; inspect again before confirming",
            ));
        }
        Ok(before)
    }
    fn open(&self, target: &Target) -> ByokResult<()> {
        let mut args = target.args();
        args.extend([
            "--open-url".into(),
            format!("{}://{EXTENSION_ID}/connect", target.installation.scheme),
        ]);
        self.runner.run(&target.installation, &args).map(|_| ())
    }
    fn disconnect(&self, target: &Target) -> ByokResult<()> {
        let mut receipt = read_json(&target.receipt)?.ok_or_else(|| {
            ByokError::precondition("The selected Copilot connection is not owned by OCG")
        })?;
        if read_json(&target.storage.join(CLAIMED))?.is_some() {
            return Err(ByokError::conflict(
                "VS Code is importing this connection; reload and inspect before disconnecting",
            ));
        }
        let id = uuid::Uuid::new_v4().to_string();
        receipt["connectionId"] = id.clone().into();
        safe::write_private_atomic(&target.receipt, &serde_json::to_vec(&receipt).unwrap())?;
        safe::write_private_atomic(&target.storage.join(HANDOFF),&serde_json::to_vec(&json!({"schemaVersion":1,"operation":"disconnect","connectionId":id,"storagePath":target.storage,"runtimeDigest":receipt["runtimeDigest"]})).unwrap())?;
        let _ = self.open(target);
        Ok(())
    }
    fn other_profiles(&self, target: &Target) -> ByokResult<Vec<Target>> {
        let Some(parent) = target.receipt.parent() else {
            return Ok(vec![]);
        };
        let mut others = vec![];
        for (index, entry) in fs::read_dir(parent).map_err(safe::io_internal)?.enumerate() {
            if index > 1024 {
                return Err(ByokError::conflict("Too many Copilot ownership receipts"));
            }
            let path = entry.map_err(safe::io_internal)?.path();
            if path == target.receipt || path.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }
            if let Some(receipt) = read_json(&path)?
                && receipt["target"]["extensionsDir"] == json!(target.extensions)
            {
                let wire: CopilotTarget = serde_json::from_value(receipt["target"].clone())
                    .map_err(|_| {
                        ByokError::conflict("Copilot ownership receipt target is malformed")
                    })?;
                if let Some(other) = self.target(wire)? {
                    others.push(other);
                }
            }
        }
        Ok(others)
    }
}
impl Target {
    fn args(&self) -> Vec<String> {
        let mut args = vec![
            "--user-data-dir".into(),
            self.wire.user_data_dir.clone().unwrap(),
            "--extensions-dir".into(),
            self.wire.extensions_dir.clone().unwrap(),
        ];
        if let Some(profile) = &self.wire.profile {
            args.extend(["--profile".into(), profile.clone()]);
        }
        args
    }
}
fn absolute(value: &str) -> ByokResult<PathBuf> {
    safe::canonical_lexical_path(Path::new(value))
}
fn read_json(path: &Path) -> ByokResult<Option<Value>> {
    let Some(bytes) = safe::read_regular_file(path)? else {
        return Ok(None);
    };
    if bytes.len() > 8 * 1024 * 1024 {
        return Err(ByokError::precondition(
            "Copilot state file exceeds its bounded limit",
        ));
    }
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|_| ByokError::conflict("Copilot state file is malformed"))
}
fn profile_storage(user_data: &Path, profile: Option<&str>) -> ByokResult<PathBuf> {
    let Some(name) = profile else {
        return Ok(user_data.join("User/globalStorage"));
    };
    let state =
        read_json(&user_data.join("User/globalStorage/storage.json"))?.ok_or_else(|| {
            ByokError::precondition("Open the named VS Code Profile once before installing")
        })?;
    let rows = state["userDataProfiles"]
        .as_array()
        .ok_or_else(|| ByokError::precondition("No VS Code named Profiles were found"))?;
    let matches = rows
        .iter()
        .filter(|r| r["name"].as_str() == Some(name))
        .collect::<Vec<_>>();
    if matches.len() != 1 {
        return Err(ByokError::precondition(
            "The named VS Code Profile is missing or ambiguous",
        ));
    }
    let row = matches[0];
    if row["useDefaultFlags"]["globalState"] == true {
        return Err(ByokError::precondition(
            "This Profile shares default global state; select the default Profile for its OCG connection",
        ));
    }
    let location = row["location"].as_str().ok_or_else(|| {
        ByokError::precondition("This VS Code Profile location needs manual extension setup")
    })?;
    if location.is_empty() || location.contains(['/', '\\']) || location == "." || location == ".."
    {
        return Err(ByokError::conflict("VS Code Profile location is invalid"));
    }
    Ok(user_data
        .join("User/profiles")
        .join(location)
        .join("globalStorage"))
}
fn runtime_at(target: &Target, version: &str) -> ByokResult<Option<String>> {
    if !version
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || ['.', '-', '+'].contains(&c))
    {
        return Err(ByokError::conflict("VS Code extension version is invalid"));
    }
    let root = target.extensions.join(format!("{EXTENSION_ID}-{version}"));
    let manifest = read_json(&root.join("package.json"))?;
    if let Some(m) = manifest
        && (m["publisher"] != "open-console-gateway"
            || m["name"] != "copilot"
            || m["version"] != version)
    {
        return Err(ByokError::conflict(
            "Installed Copilot extension identity changed",
        ));
    }
    Ok(safe::read_regular_file(&root.join("dist/extension.cjs"))?.map(|b| safe::sha256_hex(&b)))
}
#[cfg(test)]
mod tests;

fn registered_version(target: &Target) -> ByokResult<Option<String>> {
    let mut file = target.extensions.join("extensions.json");
    if target.wire.profile.is_some() {
        let user = PathBuf::from(target.wire.user_data_dir.as_ref().unwrap());
        let state = read_json(&user.join("User/globalStorage/storage.json"))?
            .ok_or_else(|| ByokError::precondition("VS Code Profile state is unavailable"))?;
        let row = state["userDataProfiles"]
            .as_array()
            .and_then(|a| {
                a.iter()
                    .find(|r| r["name"].as_str() == target.wire.profile.as_deref())
            })
            .ok_or_else(|| ByokError::precondition("VS Code Profile is unavailable"))?;
        if row["useDefaultFlags"]["extensions"] != true {
            file = target
                .storage
                .parent()
                .and_then(Path::parent)
                .unwrap()
                .join("extensions.json");
        }
    }
    let Some(rows) = read_json(&file)? else {
        return Ok(None);
    };
    let rows = rows
        .as_array()
        .ok_or_else(|| ByokError::conflict("VS Code extension registrations are malformed"))?;
    let matches = rows
        .iter()
        .filter(|r| {
            r["identifier"]["id"]
                .as_str()
                .is_some_and(|s| s.eq_ignore_ascii_case(EXTENSION_ID))
        })
        .collect::<Vec<_>>();
    if matches.len() > 1 {
        return Err(ByokError::conflict(
            "Duplicate Copilot extension registrations",
        ));
    }
    matches
        .first()
        .map(|r| {
            r["version"]
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| ByokError::conflict("Copilot registration version is missing"))
        })
        .transpose()
}

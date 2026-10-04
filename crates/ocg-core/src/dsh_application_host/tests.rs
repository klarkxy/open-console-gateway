use super::*;
use serde_json::json;
use std::sync::Mutex as StdMutex;
use std::thread;
use std::time::{Duration, Instant};

struct FakeRunner {
    version: String,
    commands: StdMutex<Vec<CommandSpec>>,
}

struct FailingAddRunner {
    manifest: PathBuf,
    commands: StdMutex<Vec<CommandSpec>>,
    fail_next_add: StdMutex<bool>,
    write_foreign_on_failure: bool,
    preserve_manifest_on_failure: bool,
}

fn mutate_test_manifest(manifest: &Path, package_spec: Option<String>) -> Result<(), String> {
    let mut value = match fs::read(manifest) {
        Ok(bytes) => serde_json::from_slice::<Value>(&bytes).map_err(|error| error.to_string())?,
        Err(error) if error.kind() == ErrorKind::NotFound => serde_json::json!({
            "name": "dsh-profile-web",
            "private": true,
            "dsh": { "profile": { "bundles": [] } },
            "dependencies": {}
        }),
        Err(error) => return Err(error.to_string()),
    };
    let object = value
        .as_object_mut()
        .ok_or_else(|| "test profile manifest is not an object".to_string())?;
    let dependencies = object
        .entry("dependencies")
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()
        .ok_or_else(|| "test dependencies are not an object".to_string())?;
    let installing = package_spec.is_some();
    match package_spec {
        Some(spec) => {
            dependencies.insert(PACKAGE_NAME.into(), Value::String(spec));
        }
        None => {
            dependencies.remove(PACKAGE_NAME);
        }
    }
    let dsh = object
        .entry("dsh")
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()
        .ok_or_else(|| "test dsh config is not an object".to_string())?;
    let profile = dsh
        .entry("profile")
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()
        .ok_or_else(|| "test profile config is not an object".to_string())?;
    let bundles = profile
        .entry("bundles")
        .or_insert_with(|| serde_json::json!([]))
        .as_array_mut()
        .ok_or_else(|| "test bundles are not an array".to_string())?;
    bundles.retain(|item| item.as_str() != Some(PACKAGE_NAME));
    if installing {
        bundles.push(Value::String(PACKAGE_NAME.into()));
    }
    fs::create_dir_all(manifest.parent().unwrap()).map_err(|error| error.to_string())?;
    fs::write(manifest, serde_json::to_vec_pretty(&value).unwrap())
        .map_err(|error| error.to_string())
}

impl CommandRunner for FakeRunner {
    fn run(&self, command: &CommandSpec) -> Result<CommandOutput, String> {
        self.commands.lock().unwrap().push(command.clone());
        if command.args == [OsString::from("--version")] {
            return Ok(CommandOutput {
                success: true,
                stdout: self.version.clone(),
                stderr: String::new(),
            });
        }
        let package = command
            .args
            .get(4)
            .and_then(|value| value.to_str())
            .ok_or_else(|| "missing package path".to_string())?
            .trim_matches('"')
            .to_string();
        let profile = command.args[2].to_string_lossy();
        let manifest = command
            .dsh_home
            .join("profiles")
            .join(profile.as_ref())
            .join("package.json");
        fs::create_dir_all(manifest.parent().unwrap()).map_err(|error| error.to_string())?;
        fs::write(
            &manifest,
            serde_json::to_vec_pretty(&serde_json::json!({
                "name": "dsh-profile-web",
                "private": true,
                "dsh": { "profile": { "bundles": [PACKAGE_NAME] } },
                "dependencies": { PACKAGE_NAME: format!("file:{package}") }
            }))
            .unwrap(),
        )
        .map_err(|error| error.to_string())?;
        Ok(CommandOutput {
            success: true,
            stdout: String::new(),
            stderr: String::new(),
        })
    }
}

impl CommandRunner for FailingAddRunner {
    fn run(&self, command: &CommandSpec) -> Result<CommandOutput, String> {
        self.commands.lock().unwrap().push(command.clone());
        if command.args == [OsString::from("--version")] {
            return Ok(CommandOutput {
                success: true,
                stdout: "0.1.5-rc.2\n".into(),
                stderr: String::new(),
            });
        }
        let action = command.args.get(3).and_then(|value| value.to_str());
        match action {
            Some("add") => {
                let package = command
                    .args
                    .get(4)
                    .and_then(|value| value.to_str())
                    .ok_or_else(|| "missing package path".to_string())?
                    .trim_matches('"');
                let fail = std::mem::take(&mut *self.fail_next_add.lock().unwrap());
                if fail && self.preserve_manifest_on_failure {
                    return Ok(CommandOutput {
                        success: false,
                        stdout: String::new(),
                        stderr: "simulated add failure with no registration change".into(),
                    });
                }
                let package_spec = if fail && self.write_foreign_on_failure {
                    "https://example.test/concurrent.tgz".to_string()
                } else {
                    format!("file:{package}")
                };
                mutate_test_manifest(&self.manifest, Some(package_spec))?;
                if fail {
                    return Ok(CommandOutput {
                        success: false,
                        stdout: String::new(),
                        stderr: "simulated add failure after mutation".into(),
                    });
                }
            }
            Some("remove") => mutate_test_manifest(&self.manifest, None)?,
            _ => return Err("unexpected DSH plugin command".into()),
        }
        Ok(CommandOutput {
            success: true,
            stdout: String::new(),
            stderr: String::new(),
        })
    }
}

fn claimed_handoff(live: &Path, token: &str) -> PathBuf {
    live.with_file_name(format!(
        "{}{BOOTSTRAP_CLAIM_MARKER}{token}",
        live.file_name().unwrap().to_string_lossy()
    ))
}

fn fixture(name: &str) -> (PathBuf, DshDesktopHost, Arc<FakeRunner>) {
    fixture_with_version(name, "0.1.5-rc.2")
}

fn fixture_with_version(name: &str, version: &str) -> (PathBuf, DshDesktopHost, Arc<FakeRunner>) {
    let root =
        std::env::temp_dir().join(format!("ocg-dsh-{name}-{}", uuid::Uuid::new_v4().simple()));
    #[cfg(not(windows))]
    let root = {
        // Positive fixtures need plain ancestors even when the OS temp path is an alias.
        fs::create_dir_all(&root).unwrap();
        fs::canonicalize(&root).unwrap()
    };
    let data_dir = root.join("data");
    let home = root.join("home");
    fs::create_dir_all(&data_dir).unwrap();
    fs::create_dir_all(&home).unwrap();
    let executable = root.join(if cfg!(windows) { "dsh.cmd" } else { "dsh" });
    fs::write(&executable, b"test-only").unwrap();
    let runner = Arc::new(FakeRunner {
        version: version.into(),
        commands: StdMutex::new(Vec::new()),
    });
    let host = DshDesktopHost {
        data_dir,
        home,
        profile: PROFILE.into(),
        legacy_bootstrap: true,
        scan_user_home: None,
        runner: runner.clone(),
        dsh_executable: Some(executable),
        operation: Mutex::new(()),
    };
    (root, host, runner)
}

#[test]
fn discovery_reads_only_named_dsh_homes_and_valid_profile_manifests() {
    let root = std::env::temp_dir().join(format!(
        "ocg-dsh-discovery-{}",
        uuid::Uuid::new_v4().simple()
    ));
    fs::create_dir_all(&root).unwrap();
    let manifest = br#"{"dsh":{"profile":{"bundles":[]}}}"#;
    for (home, profile) in [
        (".dsh", "web"),
        (".dsh-editor", "dsh-editor"),
        (".dsh-spaces", "spaces-hub"),
        (".dshother", "ignored"),
    ] {
        let path = root.join(home).join("profiles").join(profile);
        fs::create_dir_all(&path).unwrap();
        fs::write(path.join("package.json"), manifest).unwrap();
    }
    let invalid = root.join(".dsh-editor/profiles/invalid");
    fs::create_dir_all(&invalid).unwrap();
    fs::write(invalid.join("package.json"), b"not json").unwrap();

    let found = discover_profiles(&root);
    assert_eq!(found.len(), 3);
    assert_eq!(
        found
            .iter()
            .map(|profile| profile.name.as_str())
            .collect::<Vec<_>>(),
        ["web", "dsh-editor", "spaces-hub"]
    );
    assert!(
        found
            .iter()
            .all(|profile| Path::new(&profile.path).join("package.json").is_file())
    );
    assert!(!root.join(".dsh/profiles/web/node_modules").exists());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn profile_discovery_remains_available_without_a_dsh_executable() {
    let (root, mut host, _runner) = fixture("discovery-without-cli");
    let scan_root = root.join("user");
    let profile = scan_root.join(".dsh-editor/profiles/dsh-editor");
    fs::create_dir_all(&profile).unwrap();
    fs::write(profile.join("package.json"), br#"{"dsh":{"profile":{}}}"#).unwrap();
    host.scan_user_home = Some(scan_root);
    host.dsh_executable = Some(root.join("no-dsh-executable"));

    let inspected = host.inspect("http://127.0.0.1:9042/v1").unwrap();
    assert_eq!(inspected.phase, DshApplicationPhase::NotDetected);
    assert_eq!(inspected.discovered_profiles.len(), 1);
    assert_eq!(inspected.discovered_profiles[0].name, "dsh-editor");
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn selected_profile_install_uses_its_home_profile_and_private_handoff() {
    let (root, mut host, runner) = fixture("selected-profile");
    let user = root.join("user");
    let editor_home = user.join(".dsh-editor");
    let editor_profile = editor_home.join("profiles/dsh-editor");
    fs::create_dir_all(&editor_profile).unwrap();
    fs::write(
        editor_profile.join("package.json"),
        br#"{"dsh":{"profile":{"bundles":[]}}}"#,
    )
    .unwrap();
    fs::write(
        editor_profile.join(".dsh-editor-owner.json"),
        br#"{"app":"dsh-editor","schema":1}"#,
    )
    .unwrap();
    host.scan_user_home = Some(user);
    let gateway = "http://127.0.0.1:9042/v1";
    let path = editor_profile.display().to_string();
    let found = host.discovered_profiles();
    assert!(
        found
            .iter()
            .any(|profile| same_lexical_path(Path::new(&profile.path), Path::new(&path))),
        "wanted {path}; found {found:?}"
    );
    let inspected = host
        .execute(DshApplicationHostRequest::Inspect {
            gateway_v1_url: gateway.into(),
            profile_path: Some(path.clone()),
            runtime_url: None,
        })
        .unwrap();
    assert!(same_lexical_path(
        Path::new(&inspected.selected_profile_path),
        Path::new(&path)
    ));
    assert_eq!(inspected.phase, DshApplicationPhase::Ready);

    let installed = host
        .execute(DshApplicationHostRequest::Install {
            expected_fingerprint: inspected.fingerprint.unwrap(),
            gateway_v1_url: gateway.into(),
            profile_path: Some(path.clone()),
            runtime_url: None,
            secret: crate::dsh_application::DshGatewaySecret::new("editor-test-key".into()),
        })
        .unwrap();
    assert!(same_lexical_path(
        Path::new(&installed.selected_profile_path),
        Path::new(&path)
    ));
    assert_eq!(installed.phase, DshApplicationPhase::Installed);
    let target = host.for_profile(Some(&path)).unwrap();
    assert_eq!(
        fs::read(target.bootstrap_path()).unwrap(),
        b"editor-test-key"
    );
    assert_ne!(target.bootstrap_path(), host.bootstrap_path());
    assert!(!host.home.join("profiles/web/package.json").exists());
    let editor_source = editor_home.join("user-plugins/@open-console-gateway/dsh-plugin");
    assert!(editor_source.join("index.js").is_file());
    let editor_state: Value =
        serde_json::from_slice(&fs::read(editor_home.join("dsh-plugins.json")).unwrap()).unwrap();
    assert!(
        editor_state["installed"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["name"] == PACKAGE_NAME && item["spec"] == "ocg-manager")
    );
    mutate_test_manifest(&editor_profile.join("package.json"), Some("*".into())).unwrap();
    let after_editor_restart = host
        .execute(DshApplicationHostRequest::Inspect {
            gateway_v1_url: gateway.into(),
            profile_path: Some(path.clone()),
            runtime_url: None,
        })
        .unwrap();
    assert_eq!(after_editor_restart.phase, DshApplicationPhase::Installed);
    let commands = runner.commands.lock().unwrap();
    let add = commands
        .iter()
        .find(|command| command.args.get(3) == Some(&OsString::from("add")))
        .unwrap();
    assert_eq!(add.args[2], "dsh-editor");
    assert_eq!(add.dsh_home, editor_home);
    drop(commands);

    let outside_scope = root.join("user/.dshother/profiles/web");
    fs::create_dir_all(&outside_scope).unwrap();
    fs::write(
        outside_scope.join("package.json"),
        br#"{"dsh":{"profile":{}}}"#,
    )
    .unwrap();
    assert!(
        host.for_profile(Some(&outside_scope.display().to_string()))
            .is_err()
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn default_web_can_be_selected_before_its_manifest_exists() {
    let (root, host, runner) = fixture("explicit-default-web");
    let path = host.home.join("profiles/web").display().to_string();
    let fake = HttpPluginFake::start(HttpPluginState::empty());
    write_browser_grant(&host.home);
    let inspected = host
        .execute(DshApplicationHostRequest::Inspect {
            gateway_v1_url: "http://127.0.0.1:9042/v1".into(),
            profile_path: Some(path.clone()),
            runtime_url: Some(fake.origin()),
        })
        .unwrap();
    assert_eq!(inspected.phase, DshApplicationPhase::Ready);
    assert_eq!(
        inspected.runtime_url.as_deref(),
        Some(fake.origin().as_str())
    );
    assert!(!host.home.join("profiles/web/package.json").exists());
    let installed = host
        .execute(DshApplicationHostRequest::Install {
            expected_fingerprint: inspected.fingerprint.unwrap(),
            gateway_v1_url: "http://127.0.0.1:9042/v1".into(),
            profile_path: Some(path),
            runtime_url: Some(fake.origin()),
            secret: crate::dsh_application::DshGatewaySecret::new("default-web-key".into()),
        })
        .unwrap();
    assert_eq!(installed.phase, DshApplicationPhase::Installed);
    assert!(installed.installed);
    assert!(runner.commands.lock().unwrap().is_empty());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn editor_source_collision_blocks_install_before_external_writes() {
    let (root, mut host, runner) = fixture("editor-source-collision");
    let user = root.join("user");
    let home = user.join(".dsh-editor");
    let profile = home.join("profiles/dsh-editor");
    fs::create_dir_all(&profile).unwrap();
    fs::write(
        profile.join("package.json"),
        br#"{"dsh":{"profile":{"bundles":[]}}}"#,
    )
    .unwrap();
    fs::write(
        profile.join(".dsh-editor-owner.json"),
        br#"{"app":"dsh-editor","schema":1}"#,
    )
    .unwrap();
    let foreign = home.join("user-plugins/@open-console-gateway/dsh-plugin");
    fs::create_dir_all(&foreign).unwrap();
    fs::write(foreign.join("package.json"), b"foreign package").unwrap();
    host.scan_user_home = Some(user);
    let path = profile.display().to_string();
    let inspected = host
        .execute(DshApplicationHostRequest::Inspect {
            gateway_v1_url: "http://127.0.0.1:9042/v1".into(),
            profile_path: Some(path.clone()),
            runtime_url: None,
        })
        .unwrap();
    let error = host
        .execute(DshApplicationHostRequest::Install {
            expected_fingerprint: inspected.fingerprint.unwrap(),
            gateway_v1_url: "http://127.0.0.1:9042/v1".into(),
            profile_path: Some(path.clone()),
            runtime_url: None,
            secret: crate::dsh_application::DshGatewaySecret::new("test-key".into()),
        })
        .unwrap_err();
    assert_eq!(
        error.kind,
        crate::dsh_application::DshApplicationErrorKind::Conflict
    );
    assert_eq!(
        fs::read(foreign.join("package.json")).unwrap(),
        b"foreign package"
    );
    assert!(
        !host
            .for_profile(Some(&path))
            .unwrap()
            .bootstrap_path()
            .exists()
    );
    assert!(
        !runner
            .commands
            .lock()
            .unwrap()
            .iter()
            .any(|command| command.args.get(3) == Some(&OsString::from("add")))
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn installation_is_not_gated_by_dsh_version() {
    let (root, host, runner) = fixture("versions-ungated");
    let gateway = "http://127.0.0.1:9042/v1";
    let inspected = host.inspect(gateway).unwrap();
    assert_eq!(inspected.phase, DshApplicationPhase::Ready);
    assert!(inspected.install_supported);
    assert_eq!(inspected.version, None);
    assert!(
        runner.commands.lock().unwrap().is_empty(),
        "status inspect must not spawn dsh --version"
    );
    let installed = host
        .install(
            inspected.fingerprint.as_deref().unwrap(),
            gateway,
            "test-key",
        )
        .unwrap();
    assert_eq!(installed.phase, DshApplicationPhase::Installed);
    assert_eq!(installed.version, None);
    assert!(
        !runner
            .commands
            .lock()
            .unwrap()
            .iter()
            .any(|command| command.args == [OsString::from("--version")]),
        "install verifies compatibility via plugin add, not --version"
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn inspect_does_not_wait_on_a_stalling_version_cli() {
    struct StallingVersionRunner {
        commands: StdMutex<Vec<CommandSpec>>,
    }
    impl CommandRunner for StallingVersionRunner {
        fn run(&self, command: &CommandSpec) -> Result<CommandOutput, String> {
            self.commands.lock().unwrap().push(command.clone());
            if command.args == [OsString::from("--version")] {
                thread::sleep(Duration::from_secs(8));
                return Ok(CommandOutput {
                    success: true,
                    stdout: "0.1.5-rc.2\n".into(),
                    stderr: String::new(),
                });
            }
            Err("plugin command should not run during inspect".into())
        }
    }

    let (root, mut host, _) = fixture("stall-version");
    let runner = Arc::new(StallingVersionRunner {
        commands: StdMutex::new(Vec::new()),
    });
    host.runner = runner.clone();
    let started = Instant::now();
    let inspected = host.inspect("http://127.0.0.1:9042/v1").unwrap();
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_millis(800),
        "inspect waited on --version for {elapsed:?}"
    );
    assert_eq!(inspected.phase, DshApplicationPhase::Ready);
    assert!(inspected.install_supported);
    assert_eq!(inspected.version, None);
    assert!(runner.commands.lock().unwrap().is_empty());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn opt_in_cli_version_diagnostic_keeps_the_install_class_timeout() {
    let (root, host, runner) = fixture("version-timeout");
    let version = host
        .read_version(&host.resolve_dsh_executable().expect("exe"))
        .unwrap();
    assert_eq!(version, "0.1.5-rc.2");
    assert_eq!(runner.commands.lock().unwrap()[0].timeout, VERSION_TIMEOUT);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn relative_host_paths_are_resolved_before_use() {
    let relative = PathBuf::from("relative-ocg-data").join("nested");
    let absolute = absolute_host_path(relative.clone());
    assert!(absolute.is_absolute());
    assert!(absolute.ends_with(relative));
}

#[test]
fn install_materializes_only_the_owned_plugin_and_keeps_the_key_off_argv() {
    let (root, host, runner) = fixture("install");
    let gateway = "http://127.0.0.1:9042/v1";
    let inspected = host.inspect(gateway).unwrap();
    assert_eq!(inspected.phase, DshApplicationPhase::Ready);
    let secret = "ocg-test-secret";
    let installed = host
        .install(inspected.fingerprint.as_deref().unwrap(), gateway, secret)
        .unwrap();
    assert_eq!(installed.phase, DshApplicationPhase::Installed);
    assert!(installed.installed);
    let handoff = fs::read(host.bootstrap_path()).unwrap();
    assert_eq!(handoff, secret.as_bytes());
    let commands = runner.commands.lock().unwrap();
    assert_eq!(
        commands.len(),
        1,
        "plugin add only; inspect no longer launches --version"
    );
    let add = &commands[0];
    assert_eq!(add.args[0], "plugin");
    assert_eq!(add.args[1], "--profile");
    assert_eq!(add.args[2], PROFILE);
    assert_eq!(add.args[3], "add");
    assert!(
        add.args
            .iter()
            .all(|arg| !arg.to_string_lossy().contains(secret))
    );
    assert_eq!(add.args[5], "--config.auto-install-peers=true");
    let package_path = PathBuf::from(add.args[4].to_string_lossy().trim_matches('"'));
    let source = fs::read_to_string(package_path.join("index.js")).unwrap();
    assert!(source.contains("/models"));
    assert!(source.contains("Authorization"));
    assert!(!source.contains(secret));
    assert!(!source.contains(GATEWAY_PLACEHOLDER));
    assert!(!source.contains(BOOTSTRAP_PLACEHOLDER));
    drop(commands);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn inspect_counts_a_claimed_handoff_as_activation_required() {
    let (root, host, _runner) = fixture("claimed-handoff");
    let gateway = "http://127.0.0.1:9042/v1";
    let inspected = host.inspect(gateway).unwrap();
    let installed = host
        .install(
            inspected.fingerprint.as_deref().unwrap(),
            gateway,
            "ocg-test-secret",
        )
        .unwrap();
    assert!(installed.activation_required);
    assert!(installed.installed);

    let live = host.bootstrap_path();
    let claim = live.with_file_name(format!(
        "{}.claimed-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        live.file_name().unwrap().to_string_lossy()
    ));
    fs::rename(&live, &claim).unwrap();

    let pending = host.inspect(gateway).unwrap();
    assert!(pending.installed);
    assert!(
        pending.activation_required,
        "an in-flight claimed handoff must keep activationRequired true"
    );
    assert!(!live.exists());
    assert!(claim.exists());

    fs::remove_file(&claim).unwrap();
    let consumed = host.inspect(gateway).unwrap();
    assert!(consumed.installed);
    assert!(!consumed.activation_required);

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn inspect_counts_live_plus_stale_claims_and_multiple_claims_as_pending() {
    let (root, host, _runner) = fixture("pending-claims");
    let gateway = "http://127.0.0.1:9042/v1";
    let inspected = host.inspect(gateway).unwrap();
    host.install(
        inspected.fingerprint.as_deref().unwrap(),
        gateway,
        "live-key",
    )
    .unwrap();
    let live = host.bootstrap_path();
    let stale = live.with_file_name(format!(
        "{}.claimed-0000000000001000-aa",
        live.file_name().unwrap().to_string_lossy()
    ));
    fs::write(&stale, b"stale-key").unwrap();
    let with_live = host.inspect(gateway).unwrap();
    assert!(with_live.activation_required);
    assert!(live.exists());
    assert!(stale.exists());

    fs::remove_file(&live).unwrap();
    let newer = live.with_file_name(format!(
        "{}.claimed-0000000000002000-bb",
        live.file_name().unwrap().to_string_lossy()
    ));
    fs::write(&newer, b"newer-key").unwrap();
    let claims_only = host.inspect(gateway).unwrap();
    assert!(
        claims_only.activation_required,
        "multiple leftover claims must not report a false idle state"
    );
    assert!(!live.exists());

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn install_and_update_write_live_without_deleting_claims() {
    let (root, host, _runner) = fixture("keep-claims-on-install");
    let gateway = "http://127.0.0.1:9042/v1";
    let inspected = host.inspect(gateway).unwrap();
    host.install(
        inspected.fingerprint.as_deref().unwrap(),
        gateway,
        "first-key",
    )
    .unwrap();
    let live = host.bootstrap_path();
    let stale = claimed_handoff(&live, "0000000000001000-aa");
    let active = claimed_handoff(&live, "0000000000002000-bb");
    fs::write(&stale, b"stale-key").unwrap();
    fs::write(&active, b"in-flight-key").unwrap();
    let again = host.inspect(gateway).unwrap();
    host.install(again.fingerprint.as_deref().unwrap(), gateway, "second-key")
        .unwrap();
    assert_eq!(fs::read(&live).unwrap(), b"second-key");
    assert_eq!(fs::read(&stale).unwrap(), b"stale-key");
    assert_eq!(fs::read(&active).unwrap(), b"in-flight-key");
    assert!(host.inspect(gateway).unwrap().activation_required);

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn failed_install_restores_live_without_deleting_claims() {
    let (root, mut host, _runner) = fixture("keep-claims-on-rollback");
    let runner = Arc::new(FailingAddRunner {
        manifest: host.home.join("profiles/web/package.json"),
        commands: StdMutex::new(Vec::new()),
        fail_next_add: StdMutex::new(true),
        write_foreign_on_failure: false,
        preserve_manifest_on_failure: false,
    });
    host.runner = runner;
    let gateway = "http://127.0.0.1:9042/v1";
    let previous_handoff = b"previous-key";
    write_private_atomic(&host.data_dir, &host.bootstrap_path(), previous_handoff).unwrap();
    let live = host.bootstrap_path();
    let stale = claimed_handoff(&live, "0000000000001000-aa");
    let active = claimed_handoff(&live, "0000000000002000-bb");
    fs::write(&stale, b"stale-key").unwrap();
    fs::write(&active, b"in-flight-key").unwrap();
    let inspected = host.inspect(gateway).unwrap();

    let error = host
        .install(
            inspected.fingerprint.as_deref().unwrap(),
            gateway,
            "replacement-key",
        )
        .unwrap_err();

    assert_eq!(
        error.kind,
        crate::dsh_application::DshApplicationErrorKind::Precondition
    );
    assert_eq!(fs::read(&live).unwrap(), previous_handoff);
    assert_eq!(fs::read(&stale).unwrap(), b"stale-key");
    assert_eq!(fs::read(&active).unwrap(), b"in-flight-key");

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn stale_fingerprint_stops_before_any_install_effect() {
    let (root, host, runner) = fixture("stale");
    let gateway = "http://127.0.0.1:9042/v1";
    let inspected = host.inspect(gateway).unwrap();
    fs::create_dir_all(host.home.join("profiles/web")).unwrap();
    fs::write(
        host.home.join("profiles/web/package.json"),
        br#"{"name":"changed","dependencies":{}}"#,
    )
    .unwrap();
    let error = host
        .install(
            inspected.fingerprint.as_deref().unwrap(),
            gateway,
            "ocg-test-secret",
        )
        .unwrap_err();
    assert_eq!(
        error.kind,
        crate::dsh_application::DshApplicationErrorKind::Conflict
    );
    assert!(!host.bootstrap_path().exists());
    assert_eq!(runner.commands.lock().unwrap().len(), 0);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn abandoned_temporary_package_does_not_block_a_retry() {
    let (root, host, _runner) = fixture("package-retry");
    let package = host.render_package("http://127.0.0.1:9042/v1").unwrap();
    let parent = package.path.parent().unwrap();
    fs::create_dir_all(parent).unwrap();
    let abandoned = parent.join(".ocg-dsh-package-interrupted.tmp");
    fs::create_dir(&abandoned).unwrap();
    fs::write(abandoned.join("package.json"), b"partial").unwrap();

    package.materialize().unwrap();

    assert!(package.exists_and_matches());
    assert!(
        abandoned.exists(),
        "an unrelated abandoned staging path is ignored"
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn same_name_foreign_dependency_is_a_blocking_conflict() {
    let (root, host, _runner) = fixture("foreign");
    let profile = host.home.join("profiles/web");
    fs::create_dir_all(&profile).unwrap();
    fs::write(
        profile.join("package.json"),
        serde_json::to_vec(&serde_json::json!({
            "dsh": { "profile": { "bundles": [PACKAGE_NAME] } },
            "dependencies": { PACKAGE_NAME: "https://example.test/foreign.tgz" }
        }))
        .unwrap(),
    )
    .unwrap();
    let inspected = host.inspect("http://127.0.0.1:9042/v1").unwrap();
    assert_eq!(inspected.phase, DshApplicationPhase::Conflict);
    assert!(!inspected.install_supported);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn failed_add_restores_absent_registration_and_previous_handoff() {
    let (root, mut host, _runner) = fixture("failed-add-rollback");
    let runner = Arc::new(FailingAddRunner {
        manifest: host.home.join("profiles/web/package.json"),
        commands: StdMutex::new(Vec::new()),
        fail_next_add: StdMutex::new(true),
        write_foreign_on_failure: false,
        preserve_manifest_on_failure: false,
    });
    host.runner = runner.clone();
    let gateway = "http://127.0.0.1:9042/v1";
    let previous_handoff = b"previous-key";
    let original_profile = serde_json::json!({
        "name": "dsh-profile-web",
        "private": true,
        "custom": { "preserve": true },
        "dsh": { "profile": { "bundles": ["unrelated-package"] } },
        "dependencies": { "unrelated-package": "1.2.3" }
    });
    fs::create_dir_all(host.home.join("profiles/web")).unwrap();
    fs::write(
        host.home.join("profiles/web/package.json"),
        serde_json::to_vec_pretty(&original_profile).unwrap(),
    )
    .unwrap();
    write_private_atomic(&host.data_dir, &host.bootstrap_path(), previous_handoff).unwrap();
    let inspected = host.inspect(gateway).unwrap();

    let error = host
        .install(
            inspected.fingerprint.as_deref().unwrap(),
            gateway,
            "replacement-key",
        )
        .unwrap_err();

    assert_eq!(
        error.kind,
        crate::dsh_application::DshApplicationErrorKind::Precondition
    );
    assert_eq!(fs::read(host.bootstrap_path()).unwrap(), previous_handoff);
    let restored_profile: Value =
        serde_json::from_slice(&fs::read(host.home.join("profiles/web/package.json")).unwrap())
            .unwrap();
    assert_eq!(restored_profile, original_profile);
    let commands = runner.commands.lock().unwrap();
    assert_eq!(
        commands.len(),
        2,
        "failed add and rollback remove; inspect does not spawn --version"
    );
    assert_eq!(commands[0].args[3], "add");
    assert_eq!(commands[1].args[3], "remove");
    drop(commands);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn failed_add_does_not_remove_a_concurrent_foreign_registration() {
    let (root, mut host, _runner) = fixture("failed-add-foreign");
    let manifest = host.home.join("profiles/web/package.json");
    let runner = Arc::new(FailingAddRunner {
        manifest: manifest.clone(),
        commands: StdMutex::new(Vec::new()),
        fail_next_add: StdMutex::new(true),
        write_foreign_on_failure: true,
        preserve_manifest_on_failure: false,
    });
    host.runner = runner.clone();
    let gateway = "http://127.0.0.1:9042/v1";
    let inspected = host.inspect(gateway).unwrap();

    let error = host
        .install(
            inspected.fingerprint.as_deref().unwrap(),
            gateway,
            "replacement-key",
        )
        .unwrap_err();

    assert_eq!(
        error.kind,
        crate::dsh_application::DshApplicationErrorKind::Conflict
    );
    assert!(!host.bootstrap_path().exists());
    let profile: Value = serde_json::from_slice(&fs::read(manifest).unwrap()).unwrap();
    assert_eq!(
        profile.pointer("/dependencies/@open-console-gateway~1dsh-plugin"),
        Some(&Value::String("https://example.test/concurrent.tgz".into()))
    );
    let commands = runner.commands.lock().unwrap();
    assert_eq!(
        commands.len(),
        1,
        "failed add only; inspect does not spawn --version"
    );
    assert_eq!(commands[0].args[3], "add");
    drop(commands);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn failed_add_of_registered_missing_cache_keeps_one_command_and_the_original_error() {
    for (name, partial) in [
        ("registered-absent-add", false),
        ("registered-partial-add", true),
    ] {
        let (root, mut host, _runner) = fixture(name);
        let gateway = "http://127.0.0.1:9042/v1";
        let package = host.render_package(gateway).unwrap();
        let nested = nested_relative(&package).to_path_buf();
        if partial {
            let nested_path = package.path.join(&nested);
            fs::create_dir_all(nested_path.parent().unwrap()).unwrap();
            fs::write(&nested_path, package.files.get(&nested).unwrap()).unwrap();
        } else {
            assert!(!package.path.exists(), "{name}");
        }
        let manifest = host.home.join("profiles/web/package.json");
        mutate_test_manifest(&manifest, Some(format!("file:{}", package.path.display()))).unwrap();
        let manifest_before = fs::read(&manifest).unwrap();
        let runner = Arc::new(FailingAddRunner {
            manifest: manifest.clone(),
            commands: StdMutex::new(Vec::new()),
            fail_next_add: StdMutex::new(true),
            write_foreign_on_failure: false,
            preserve_manifest_on_failure: true,
        });
        host.runner = runner.clone();
        write_private_atomic(
            &host.data_dir,
            &host.bootstrap_path(),
            b"previous-registered-key",
        )
        .unwrap();
        let inspected = host.inspect(gateway).unwrap();
        assert!(inspected.install_supported, "{name}");
        assert!(!inspected.installed, "{name}");

        let error = host
            .install(
                inspected.fingerprint.as_deref().unwrap(),
                gateway,
                CACHE_SECRET,
            )
            .unwrap_err();

        assert_eq!(
            error.kind,
            crate::dsh_application::DshApplicationErrorKind::Precondition,
            "{name}: {}",
            error.message
        );
        assert!(
            error
                .message
                .contains("simulated add failure with no registration change"),
            "{name}: {}",
            error.message
        );
        assert!(
            !error.message.contains("could not be restored"),
            "{name}: {}",
            error.message
        );
        assert!(!error.message.contains(CACHE_SECRET), "{name}");
        assert_eq!(
            fs::read(host.bootstrap_path()).unwrap(),
            b"previous-registered-key",
            "{name}"
        );
        assert_eq!(fs::read(&manifest).unwrap(), manifest_before, "{name}");
        let spec = serde_json::from_slice::<Value>(&manifest_before)
            .unwrap()
            .pointer("/dependencies/@open-console-gateway~1dsh-plugin")
            .and_then(Value::as_str)
            .unwrap()
            .to_string();
        assert!(spec.starts_with("file:"), "{name}: {spec}");
        assert!(!spec.starts_with("http"), "{name}: {spec}");
        if partial {
            assert_eq!(
                fs::read(package.path.join(&nested)).unwrap(),
                package.files.get(&nested).unwrap().as_slice(),
                "{name}"
            );
        }
        let commands = runner.commands.lock().unwrap();
        assert_eq!(commands.len(), 1, "{name}");
        assert_eq!(commands[0].args[3], "add");
        drop(commands);
        fs::remove_dir_all(root).unwrap();
    }
}

fn owned_older_path(state: &RegistrationState) -> &Path {
    match state {
        RegistrationState::OwnedOlder(source) => source,
        other => panic!("expected an owned older registration, observed {other:?}"),
    }
}

fn snapshot_files(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    walk_files(root)
        .into_iter()
        .map(|path| {
            let bytes = fs::read(&path).unwrap();
            (path, bytes)
        })
        .collect()
}

fn write_shifted_owned_package(package: &RenderedPackage) -> PathBuf {
    let mut files = package.files.clone();
    let nested = nested_relative(package).to_path_buf();
    let bytes = files
        .get_mut(&nested)
        .expect("rendered package has a nested file");
    assert!(!bytes.is_empty(), "nested package file is empty");
    bytes[0] ^= 0xff;
    let digest = package_digest(&files).expect("shifted package can be hashed");
    let directory = package.trusted_root.join(&digest[..24]);
    assert_ne!(directory, package.path);
    for (relative, contents) in &files {
        let destination = directory.join(relative);
        fs::create_dir_all(destination.parent().unwrap()).unwrap();
        fs::write(destination, contents).unwrap();
    }
    assert!(is_owned_package_source(&directory, &package.trusted_root));
    assert!(!recoverable_registered_cache(&directory, package));
    directory
}

#[test]
fn cache_recovery_accepts_absent_identity_and_current_partial_or_absent_sources() {
    let gateway = "http://127.0.0.1:9042/v1";
    let (root, host, _runner) = fixture("transition-unregistered");
    let package = host.render_package(gateway).unwrap();
    let before = host.registration_state(&package);
    assert_eq!(before, RegistrationState::Absent);
    package.materialize().unwrap();
    let after = host.registration_state(&package);
    assert_eq!(after, RegistrationState::Absent);
    assert_eq!(
        registration_transition(&before, &after, &package),
        RegistrationTransition::Accepted
    );
    assert!(package.exists_and_matches());
    fs::remove_dir_all(root).unwrap();

    for (name, partial) in [
        ("transition-absent-cache", false),
        ("transition-partial-cache", true),
    ] {
        let (root, host, _runner) = fixture(name);
        let package = host.render_package(gateway).unwrap();
        let nested = nested_relative(&package).to_path_buf();
        let nested_bytes = package.files.get(&nested).unwrap().clone();
        if partial {
            let nested_path = package.path.join(&nested);
            fs::create_dir_all(nested_path.parent().unwrap()).unwrap();
            fs::write(&nested_path, &nested_bytes).unwrap();
        } else {
            assert!(!package.path.exists(), "{name}");
        }
        mutate_test_manifest(
            &host.home.join("profiles/web/package.json"),
            Some(format!("file:{}", package.path.display())),
        )
        .unwrap();
        let before = host.registration_state(&package);
        assert!(
            same_lexical_path(owned_older_path(&before), &package.path),
            "{name}"
        );
        package.materialize().unwrap();
        let after = host.registration_state(&package);
        assert_eq!(after, RegistrationState::Exact, "{name}");
        assert_eq!(
            registration_transition(&before, &after, &package),
            RegistrationTransition::Accepted,
            "{name}"
        );
        assert_eq!(
            registration_transition(&after, &after, &package),
            RegistrationTransition::Accepted,
            "{name}"
        );
        if partial {
            assert_eq!(
                fs::read(package.path.join(&nested)).unwrap(),
                nested_bytes,
                "{name}"
            );
        }
        fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn genuine_older_owned_source_stays_accepted_and_keeps_its_bytes() {
    let (root, host, runner) = fixture("transition-older-source");
    let gateway = "http://127.0.0.1:9042/v1";
    let package = host.render_package(gateway).unwrap();
    let older = write_shifted_owned_package(&package);
    let older_bytes = snapshot_files(&older);
    mutate_test_manifest(
        &host.home.join("profiles/web/package.json"),
        Some(format!("file:{}", older.display())),
    )
    .unwrap();
    let before = host.registration_state(&package);
    assert!(same_lexical_path(owned_older_path(&before), &older));
    package.materialize().unwrap();
    let after = host.registration_state(&package);
    assert!(same_lexical_path(owned_older_path(&after), &older));
    assert_eq!(
        registration_transition(&before, &after, &package),
        RegistrationTransition::Accepted
    );
    assert_eq!(snapshot_files(&older), older_bytes);
    assert_eq!(
        registration_transition(
            &RegistrationState::OwnedOlder(older.clone()),
            &RegistrationState::Exact,
            &package
        ),
        RegistrationTransition::Rejected
    );

    let inspected = host.inspect(gateway).unwrap();
    assert!(inspected.install_supported);
    let installed = host
        .install(
            inspected.fingerprint.as_deref().unwrap(),
            gateway,
            CACHE_SECRET,
        )
        .unwrap();
    assert_eq!(installed.phase, DshApplicationPhase::Installed);
    assert_package_hides_secret(&package, CACHE_SECRET);
    assert_eq!(snapshot_files(&older), older_bytes);
    let commands = runner.commands.lock().unwrap();
    assert_eq!(commands.len(), 1);
    assert_eq!(commands[0].args[3], "add");
    drop(commands);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn registration_transition_rejects_foreign_conflict_and_changed_owned_source() {
    let (root, host, runner) = fixture("transition-reject");
    let gateway = "http://127.0.0.1:9042/v1";
    let package = host.render_package(gateway).unwrap();
    let manifest = host.home.join("profiles/web/package.json");
    mutate_test_manifest(&manifest, Some(format!("file:{}", package.path.display()))).unwrap();
    let current = host.registration_state(&package);
    assert!(same_lexical_path(owned_older_path(&current), &package.path));

    let older = write_shifted_owned_package(&package);
    let older_bytes = snapshot_files(&older);
    mutate_test_manifest(&manifest, Some(format!("file:{}", older.display()))).unwrap();
    let older_state = host.registration_state(&package);
    assert!(same_lexical_path(owned_older_path(&older_state), &older));
    assert_ne!(owned_older_path(&current), owned_older_path(&older_state));
    assert_eq!(
        registration_transition(&current, &older_state, &package),
        RegistrationTransition::Rejected
    );
    assert_eq!(
        registration_transition(&older_state, &RegistrationState::Exact, &package),
        RegistrationTransition::Rejected
    );
    assert_eq!(
        registration_transition(&older_state, &older_state, &package),
        RegistrationTransition::Accepted
    );
    let changed = rejected_registration_transition(&older_state);
    assert_eq!(
        changed.kind,
        crate::dsh_application::DshApplicationErrorKind::Conflict
    );
    assert_eq!(
        changed.message,
        "DSH registration changed while the OCG package cache was recovered"
    );

    let foreign = root.join("foreign-plugin");
    fs::create_dir_all(foreign.join("locale")).unwrap();
    fs::write(
        foreign.join("package.json"),
        b"{\"name\":\"foreign-plugin\"}",
    )
    .unwrap();
    fs::write(foreign.join("locale/en.json"), b"keep-foreign-locale").unwrap();
    let foreign_bytes = snapshot_files(&foreign);
    mutate_test_manifest(&manifest, Some(format!("file:{}", foreign.display()))).unwrap();
    let conflict = host.registration_state(&package);
    let RegistrationState::Conflict(detail) = &conflict else {
        panic!("foreign source was not a conflict: {conflict:?}");
    };
    assert_eq!(
        registration_transition(&current, &conflict, &package),
        RegistrationTransition::Rejected
    );
    assert_eq!(
        registration_transition(&older_state, &conflict, &package),
        RegistrationTransition::Rejected
    );
    assert_eq!(
        registration_transition(&conflict, &conflict, &package),
        RegistrationTransition::Rejected
    );
    let error = rejected_registration_transition(&conflict);
    assert_eq!(
        error.kind,
        crate::dsh_application::DshApplicationErrorKind::Conflict
    );
    assert_eq!(error.message, *detail);
    assert_eq!(snapshot_files(&foreign), foreign_bytes);
    assert_eq!(snapshot_files(&older), older_bytes);
    assert!(!package.path.exists());
    assert!(!host.bootstrap_path().exists());
    assert!(runner.commands.lock().unwrap().is_empty());

    let spelled = package
        .path
        .parent()
        .unwrap()
        .join(".")
        .join(package.path.file_name().unwrap());
    assert_eq!(
        registration_transition(
            &RegistrationState::OwnedOlder(spelled.clone()),
            &RegistrationState::Exact,
            &package
        ),
        RegistrationTransition::Accepted
    );
    assert_eq!(
        registration_transition(
            &RegistrationState::OwnedOlder(package.path.clone()),
            &RegistrationState::OwnedOlder(spelled),
            &package
        ),
        RegistrationTransition::Accepted
    );
    for (before, after, decision) in [
        (
            RegistrationState::Absent,
            RegistrationState::Absent,
            RegistrationTransition::Accepted,
        ),
        (
            RegistrationState::Exact,
            RegistrationState::Exact,
            RegistrationTransition::Accepted,
        ),
        (
            RegistrationState::EditorExact,
            RegistrationState::EditorExact,
            RegistrationTransition::Accepted,
        ),
        (
            RegistrationState::Absent,
            RegistrationState::Exact,
            RegistrationTransition::Rejected,
        ),
        (
            RegistrationState::Exact,
            RegistrationState::Absent,
            RegistrationTransition::Rejected,
        ),
        (
            RegistrationState::EditorExact,
            RegistrationState::Exact,
            RegistrationTransition::Rejected,
        ),
        (
            RegistrationState::Exact,
            RegistrationState::EditorExact,
            RegistrationTransition::Rejected,
        ),
        (
            RegistrationState::Exact,
            RegistrationState::OwnedOlder(package.path.clone()),
            RegistrationTransition::Rejected,
        ),
        (
            RegistrationState::Conflict("same".into()),
            RegistrationState::Conflict("same".into()),
            RegistrationTransition::Rejected,
        ),
        (
            RegistrationState::OwnedOlder(package.path.clone()),
            RegistrationState::Conflict("foreign".into()),
            RegistrationTransition::Rejected,
        ),
    ] {
        assert_eq!(
            registration_transition(&before, &after, &package),
            decision,
            "{before:?} -> {after:?}"
        );
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn foreign_registration_stops_before_handoff_and_preserves_its_source() {
    let (root, host, runner) = fixture("foreign-before-handoff");
    let gateway = "http://127.0.0.1:9042/v1";
    let package = host.render_package(gateway).unwrap();
    let foreign = root.join("foreign-plugin");
    fs::create_dir_all(foreign.join("locale")).unwrap();
    fs::write(
        foreign.join("package.json"),
        b"{\"name\":\"foreign-plugin\",\"keep\":true}",
    )
    .unwrap();
    fs::write(foreign.join("locale/en.json"), b"keep-foreign-locale").unwrap();
    let foreign_bytes = snapshot_files(&foreign);
    let manifest = host.home.join("profiles/web/package.json");
    mutate_test_manifest(&manifest, Some(format!("file:{}", foreign.display()))).unwrap();
    let manifest_bytes = fs::read(&manifest).unwrap();

    let inspected = host.inspect(gateway).unwrap();
    assert_eq!(inspected.phase, DshApplicationPhase::Conflict);
    assert!(!inspected.install_supported);
    let error = host
        .install(
            inspected.fingerprint.as_deref().unwrap(),
            gateway,
            CACHE_SECRET,
        )
        .unwrap_err();
    assert_eq!(
        error.kind,
        crate::dsh_application::DshApplicationErrorKind::Precondition
    );
    assert_eq!(error.message, inspected.detail.unwrap());
    assert!(!error.message.contains(CACHE_SECRET));
    assert!(!host.bootstrap_path().exists());
    assert!(runner.commands.lock().unwrap().is_empty());
    assert_eq!(snapshot_files(&foreign), foreign_bytes);
    assert_eq!(fs::read(&manifest).unwrap(), manifest_bytes);
    assert!(!package.path.exists());
    fs::remove_dir_all(root).unwrap();
}

fn process_probe_root(name: &str) -> PathBuf {
    let root =
        std::env::temp_dir().join(format!("ocg-dsh-{name}-{}", uuid::Uuid::new_v4().simple()));
    fs::create_dir_all(&root).unwrap();
    root
}

#[test]
fn dsh_package_argument_is_the_literal_package_path() {
    let path = PathBuf::from(if cfg!(windows) {
        r"C:\Users\OCG & Co\plugin source"
    } else {
        "/tmp/OCG & Co/plugin source"
    });
    assert_eq!(dsh_package_argument(&path), path.as_os_str());
}

#[cfg(windows)]
#[test]
fn windows_cmd_launch_preserves_package_path_with_spaces_and_ampersand() {
    let root = process_probe_root("win-cmd-amp");
    let package = root.join("pkg dir & plugin").join("source");
    fs::create_dir_all(&package).unwrap();
    let captured = root.join("captured.txt");
    let script = root.join("probe.cmd");
    fs::write(
        &script,
        format!(
            "@echo off\r\n\
             setlocal DisableDelayedExpansion\r\n\
             set \"ARG=%~1\"\r\n\
             set \"NEXT=%~2\"\r\n\
             setlocal EnableDelayedExpansion\r\n\
             >\"{captured}\" echo(!ARG!\r\n\
             >>\"{captured}\" echo(!NEXT!\r\n\
             >>\"{captured}\" echo(!DSH_HOME!\r\n",
            captured = captured.display()
        ),
    )
    .unwrap();

    let output = ProcessCommandRunner
        .run(&CommandSpec {
            executable: script,
            display_executable: "probe.cmd".into(),
            dsh_home: root.clone(),
            args: vec![dsh_package_argument(&package), OsString::from("next-arg")],
            timeout: Duration::from_secs(10),
        })
        .unwrap_or_else(|error| panic!("cmd probe failed: {error}"));
    assert!(
        output.success,
        "stdout={} stderr={}",
        output.stdout, output.stderr
    );
    let got = fs::read_to_string(&captured).unwrap();
    let lines = got
        .lines()
        .map(|line| line.trim_end_matches('\r'))
        .collect::<Vec<_>>();
    assert_eq!(
        lines,
        [
            package.to_str().expect("package path is Unicode"),
            "next-arg",
            root.to_str().expect("DSH home path is Unicode")
        ]
    );
    fs::remove_dir_all(root).unwrap();
}

#[cfg(windows)]
#[test]
fn windows_cmd_launch_rejects_percent_exclamation_and_newlines() {
    let root = process_probe_root("win-cmd-unsafe");
    let script = root.join("probe.cmd");
    fs::write(&script, b"@echo off\r\n").unwrap();
    for argument in ["has%percent", "has!bang", "has\nnewline", "has\rreturn"] {
        let error = ProcessCommandRunner
            .run(&CommandSpec {
                executable: script.clone(),
                display_executable: "probe.cmd".into(),
                dsh_home: root.clone(),
                args: vec![OsString::from(argument)],
                timeout: Duration::from_secs(5),
            })
            .expect_err(argument);
        assert!(error.contains("unsafe characters"), "{argument}: {error}");
    }
    fs::remove_dir_all(root).unwrap();
}

#[cfg(unix)]
fn write_unix_script(path: &Path, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

#[cfg(unix)]
fn unix_shell_command(
    script: PathBuf,
    display: &str,
    extra_args: Vec<OsString>,
    timeout: Duration,
) -> CommandSpec {
    // cargo test runs this crate's tests as threads in one process. A sibling
    // thread can fork while write_unix_script still holds a write fd; the child
    // inherits that fd until exec, so Linux execve of the same inode returns
    // ETXTBSY (rust-lang/rust#114554). /bin/sh is a stable inode and opens the
    // script O_RDONLY, which is allowed while a writer exists.
    let mut args = Vec::with_capacity(extra_args.len() + 1);
    args.push(script.into_os_string());
    args.extend(extra_args);
    CommandSpec {
        executable: PathBuf::from("/bin/sh"),
        display_executable: display.into(),
        dsh_home: std::env::temp_dir(),
        args,
        timeout,
    }
}

#[cfg(unix)]
fn unix_pid_gone_or_zombie(pid: i32) -> bool {
    let output = std::process::Command::new("ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .output()
        .unwrap();
    let status = String::from_utf8_lossy(&output.stdout);
    status.trim().is_empty() || status.trim().starts_with('Z')
}

#[cfg(unix)]
fn unix_wait_until(mut condition: impl FnMut() -> bool, detail: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !condition() {
        assert!(
            Instant::now() < deadline,
            "unix process probe timed out: {detail}"
        );
        thread::sleep(Duration::from_millis(20));
    }
}

#[cfg(unix)]
#[test]
fn unix_command_timeout_or_cleanup_does_not_hang_on_descendant_output_pipes() {
    let root = process_probe_root("unix-drain");
    let script = root.join("hold-stdout");
    let pidfile = root.join("descendant.pid");
    write_unix_script(
        &script,
        r#"pidfile="$1"
sleep 60 &
echo $! > "$pidfile"
exit 0"#,
    );

    let started = Instant::now();
    let result = ProcessCommandRunner.run(&unix_shell_command(
        script,
        "hold-stdout",
        vec![OsString::from(pidfile.as_os_str())],
        Duration::from_secs(2),
    ));
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_secs(8),
        "output drain hung for {elapsed:?}: {result:?}"
    );
    match result {
        Ok(output) => assert!(
            output.success,
            "stdout={} stderr={}",
            output.stdout, output.stderr
        ),
        Err(error) => assert!(
            error.contains("timed out"),
            "unexpected command error: {error}"
        ),
    }
    let pid = fs::read_to_string(&pidfile)
        .unwrap_or_else(|error| panic!("descendant pid was not recorded: {error}"))
        .trim()
        .parse::<i32>()
        .unwrap();
    unix_wait_until(|| unix_pid_gone_or_zombie(pid), "descendant still running");
    fs::remove_dir_all(root).unwrap();
}

#[cfg(unix)]
#[test]
fn unix_command_timeout_terminates_a_live_process_group() {
    let root = process_probe_root("unix-timeout");
    let script = root.join("sleep-leader");
    let pidfile = root.join("leader.pid");
    write_unix_script(
        &script,
        r#"pidfile="$1"
echo $$ > "$pidfile"
sleep 60 &
echo $! >> "$pidfile"
exec sleep 60"#,
    );

    let started = Instant::now();
    let error = ProcessCommandRunner
        .run(&unix_shell_command(
            script,
            "sleep-leader",
            vec![OsString::from(pidfile.as_os_str())],
            Duration::from_millis(400),
        ))
        .expect_err("sleeping process group must time out");
    let elapsed = started.elapsed();
    assert!(
        error.contains("timed out"),
        "unexpected command error: {error}"
    );
    assert!(
        elapsed < Duration::from_secs(8),
        "timeout path hung for {elapsed:?}"
    );
    unix_wait_until(|| pidfile.exists(), "leader pid was not recorded");
    let pids = fs::read_to_string(&pidfile)
        .unwrap()
        .lines()
        .map(|line| line.trim().parse::<i32>().unwrap())
        .collect::<Vec<_>>();
    assert!(!pids.is_empty());
    for pid in pids {
        unix_wait_until(
            || unix_pid_gone_or_zombie(pid),
            &format!("pid {pid} still running"),
        );
    }
    fs::remove_dir_all(root).unwrap();
}

#[cfg(unix)]
#[test]
fn unix_command_returns_short_process_output() {
    let root = process_probe_root("unix-output");
    let script = root.join("echo-output");
    write_unix_script(&script, "printf 'hello-dsh\\n'");
    let output = ProcessCommandRunner
        .run(&unix_shell_command(
            script,
            "echo-output",
            Vec::new(),
            Duration::from_secs(5),
        ))
        .unwrap();
    assert!(output.success, "stderr={}", output.stderr);
    assert_eq!(output.stdout.trim(), "hello-dsh");
    fs::remove_dir_all(root).unwrap();
}

const FIXTURE_GRANT_SECRET: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";

fn write_browser_grant(home: &Path) {
    fs::write(
        home.join(".credentials.yaml"),
        format!(
            "version: 1\nrecords:\n  client-connection/browser-session:\n    kind: grant\n    payload:\n      version: 1\n      secret: {FIXTURE_GRANT_SECRET}\n  refs/other:\n    kind: ref\n    payload: {{}}\n"
        ),
    )
    .unwrap();
}

fn ocg_bundle(installed: bool, enabled: bool, removable: bool) -> Value {
    json!({
        "name": PACKAGE_NAME,
        "enabled": enabled,
        "installed": installed,
        "removable": removable,
        "version": "0.1.0"
    })
}

struct HttpPluginState {
    bundles: Vec<Value>,
    plugins: Vec<Value>,
    inspect_problem: Option<String>,
    install_application: String,
    remove_application: String,
    drop_install: bool,
    wait_null: bool,
    hang_status: bool,
    methods: Vec<String>,
}

impl HttpPluginState {
    fn empty() -> Self {
        Self {
            bundles: Vec::new(),
            plugins: Vec::new(),
            inspect_problem: None,
            install_application: "applied".into(),
            remove_application: "applied".into(),
            drop_install: false,
            wait_null: false,
            hang_status: false,
            methods: Vec::new(),
        }
    }
}

struct HttpPluginFake {
    port: u16,
    state: Arc<Mutex<HttpPluginState>>,
    stop: Arc<std::sync::atomic::AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl HttpPluginFake {
    fn start(state: HttpPluginState) -> Self {
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().unwrap().port();
        let state = Arc::new(Mutex::new(state));
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let thread_state = state.clone();
        let thread_stop = stop.clone();
        let thread = std::thread::spawn(move || {
            while !thread_stop.load(std::sync::atomic::Ordering::SeqCst) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                        if let Ok((rpc_id, method, _body)) = read_rpc(&mut stream) {
                            let mut state = thread_state.lock().unwrap();
                            state.methods.push(method.clone());
                            if state.hang_status {
                                drop(state);
                                thread::sleep(Duration::from_secs(5));
                                continue;
                            }
                            if method.ends_with("installBundle") && state.drop_install {
                                state.drop_install = false;
                                state.bundles = vec![ocg_bundle(true, true, true)];
                                continue;
                            }
                            let value = match method.as_str() {
                                "pluginManager/listBundles" => Value::Array(state.bundles.clone()),
                                "pluginManager/listPlugins" => Value::Array(state.plugins.clone()),
                                "pluginManager/inspect" => {
                                    if let Some(problem) = &state.inspect_problem {
                                        json!({ "status": "refused", "problem": problem })
                                    } else {
                                        json!({
                                            "status": "accepted",
                                            "kind": "path",
                                            "name": PACKAGE_NAME,
                                            "bundle": true
                                        })
                                    }
                                }
                                "pluginManager/installBundle" => {
                                    let application = state.install_application.clone();
                                    let stage = if application == "failed" {
                                        "stop-profile"
                                    } else {
                                        "install"
                                    };
                                    if application != "failed" && application != "cancelled" {
                                        state.bundles = vec![ocg_bundle(true, true, true)];
                                        if application == "applied" {
                                            state.plugins = vec![json!({
                                                "moduleName": PACKAGE_NAME,
                                                "enabled": true,
                                                "fiberPhase": "active"
                                            })];
                                        } else {
                                            state.plugins = Vec::new();
                                        }
                                    }
                                    json!({
                                        "changed": application != "failed" && application != "cancelled",
                                        "application": application,
                                        "stage": stage,
                                        "target": PACKAGE_NAME,
                                        "bundle": PACKAGE_NAME
                                    })
                                }
                                "pluginManager/removeBundle" => {
                                    let application = state.remove_application.clone();
                                    if application == "applied" {
                                        state
                                            .bundles
                                            .retain(|bundle| bundle["name"] != PACKAGE_NAME);
                                        state
                                            .plugins
                                            .retain(|plugin| plugin["moduleName"] != PACKAGE_NAME);
                                    }
                                    json!({
                                        "changed": application == "applied",
                                        "application": application,
                                        "stage": "remove",
                                        "target": PACKAGE_NAME,
                                        "bundle": PACKAGE_NAME
                                    })
                                }
                                "pluginManager/waitForInstall" => {
                                    if state.wait_null {
                                        Value::Null
                                    } else {
                                        state.plugins = vec![json!({
                                            "moduleName": PACKAGE_NAME,
                                            "enabled": true,
                                            "fiberPhase": "active"
                                        })];
                                        json!({
                                            "changed": true,
                                            "application": "applied",
                                            "stage": "install",
                                            "target": PACKAGE_NAME,
                                            "bundle": PACKAGE_NAME
                                        })
                                    }
                                }
                                _ => Value::Null,
                            };
                            write_rpc(&mut stream, &rpc_id, value);
                        }
                    }
                    Err(_) => break,
                }
            }
        });
        Self {
            port,
            state,
            stop,
            thread: Some(thread),
        }
    }

    fn origin(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    fn methods(&self) -> Vec<String> {
        self.state.lock().unwrap().methods.clone()
    }
}

impl Drop for HttpPluginFake {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        let _ = std::net::TcpStream::connect(("127.0.0.1", self.port));
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn read_rpc(stream: &mut std::net::TcpStream) -> std::io::Result<(String, String, Value)> {
    use std::io::Read;
    let mut buf = Vec::new();
    let mut chunk = [0u8; 512];
    loop {
        let read = stream.read(&mut chunk)?;
        if read == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..read]);
        if buf.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
        if buf.len() > 64 * 1024 {
            return Err(std::io::Error::other("headers too large"));
        }
    }
    let header_end = buf
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or_else(|| std::io::Error::other("incomplete headers"))?;
    let header_text = String::from_utf8_lossy(&buf[..header_end]);
    let mut content_length = 0usize;
    for line in header_text.split("\r\n") {
        if let Some(value) = line
            .split_once(':')
            .filter(|(name, _)| name.eq_ignore_ascii_case("content-length"))
            .map(|(_, value)| value.trim().parse().unwrap_or(0))
        {
            content_length = value;
        }
    }
    let mut leftover = buf[header_end + 4..].to_vec();
    while leftover.len() < content_length {
        let read = stream.read(&mut chunk)?;
        if read == 0 {
            break;
        }
        leftover.extend_from_slice(&chunk[..read]);
    }
    leftover.truncate(content_length);
    let body: Value = serde_json::from_slice(&leftover).unwrap_or(Value::Null);
    let rpc_id = body
        .get("rpcId")
        .and_then(Value::as_str)
        .unwrap_or("missing")
        .to_owned();
    let method = body
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    Ok((rpc_id, method, body))
}

fn write_rpc(stream: &mut std::net::TcpStream, rpc_id: &str, value: Value) {
    use std::io::Write;
    let body = serde_json::to_vec(&json!({
        "type": "server-response",
        "rpcId": rpc_id,
        "result": { "ok": true, "value": value }
    }))
    .unwrap();
    let _ = stream.write_all(
        format!(
            "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
            body.len()
        )
        .as_bytes(),
    );
    let _ = stream.write_all(&body);
}

#[test]
fn http_web_install_uninstall_and_restart_do_not_use_desktop_cli() {
    let (root, host, runner) = fixture("http-web");
    write_browser_grant(&host.home);
    let fake = HttpPluginFake::start(HttpPluginState::empty());
    let gateway = "http://127.0.0.1:9042/v1";
    let inspected = host
        .execute(DshApplicationHostRequest::Inspect {
            gateway_v1_url: gateway.into(),
            profile_path: None,
            runtime_url: Some(fake.origin()),
        })
        .unwrap();
    assert_eq!(inspected.phase, DshApplicationPhase::Ready);
    assert_eq!(
        inspected.runtime_url.as_deref(),
        Some(fake.origin().as_str())
    );
    let installed = host
        .execute(DshApplicationHostRequest::Install {
            expected_fingerprint: inspected.fingerprint.clone().unwrap(),
            gateway_v1_url: gateway.into(),
            profile_path: None,
            runtime_url: Some(fake.origin()),
            secret: crate::dsh_application::DshGatewaySecret::new("http-key".into()),
        })
        .unwrap();
    assert_eq!(installed.phase, DshApplicationPhase::Installed);
    assert!(installed.installed);
    assert!(installed.enabled);
    assert!(installed.uninstall_supported);
    assert_eq!(installed.application, Some(DshApplicationOutcome::Applied));
    assert_eq!(installed.version, None);
    assert!(
        installed
            .target_paths
            .iter()
            .all(|path| !path.ends_with("package.json"))
    );
    assert_eq!(fs::read(host.bootstrap_path()).unwrap(), b"http-key");

    fake.state.lock().unwrap().install_application = "restart-required".into();
    fake.state.lock().unwrap().bundles = Vec::new();
    fake.state.lock().unwrap().plugins = Vec::new();
    let again = host
        .execute(DshApplicationHostRequest::Inspect {
            gateway_v1_url: gateway.into(),
            profile_path: None,
            runtime_url: Some(fake.origin()),
        })
        .unwrap();
    let restarted = host
        .execute(DshApplicationHostRequest::Install {
            expected_fingerprint: again.fingerprint.unwrap(),
            gateway_v1_url: gateway.into(),
            profile_path: None,
            runtime_url: Some(fake.origin()),
            secret: crate::dsh_application::DshGatewaySecret::new("http-key-2".into()),
        })
        .unwrap();
    assert_eq!(
        restarted.application,
        Some(DshApplicationOutcome::RestartRequired)
    );
    assert!(restarted.installed);

    let ready_for_remove = host
        .execute(DshApplicationHostRequest::Inspect {
            gateway_v1_url: gateway.into(),
            profile_path: None,
            runtime_url: Some(fake.origin()),
        })
        .unwrap();
    let removed = host
        .execute(DshApplicationHostRequest::Uninstall {
            expected_fingerprint: ready_for_remove.fingerprint.unwrap(),
            gateway_v1_url: gateway.into(),
            profile_path: None,
            runtime_url: Some(fake.origin()),
        })
        .unwrap();
    assert!(!removed.installed);
    assert!(host.bootstrap_path().exists());
    assert!(host.home.join(".credentials.yaml").exists());
    assert!(runner.commands.lock().unwrap().is_empty());
    assert!(
        fake.methods()
            .iter()
            .any(|method| method == "pluginManager/installBundle")
    );
    assert!(
        fake.methods()
            .iter()
            .any(|method| method == "pluginManager/removeBundle")
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn http_inspect_lists_plugins_to_detect_restart_and_does_not_spawn_cli() {
    let (root, host, runner) = fixture("http-inspect-plugins");
    write_browser_grant(&host.home);
    let mut state = HttpPluginState::empty();
    state.bundles = vec![ocg_bundle(true, true, true)];
    let fake = HttpPluginFake::start(state);
    let inspected = host
        .execute(DshApplicationHostRequest::Inspect {
            gateway_v1_url: "http://127.0.0.1:9042/v1".into(),
            profile_path: None,
            runtime_url: Some(fake.origin()),
        })
        .unwrap();
    assert_eq!(inspected.phase, DshApplicationPhase::Installed);
    assert_eq!(
        inspected.application,
        Some(DshApplicationOutcome::RestartRequired)
    );
    assert!(inspected.install_supported);
    let methods = fake.methods();
    assert!(
        methods
            .iter()
            .any(|method| method == "pluginManager/listBundles")
    );
    assert!(
        methods
            .iter()
            .any(|method| method == "pluginManager/listPlugins")
    );
    assert!(runner.commands.lock().unwrap().is_empty());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn http_inspect_unresponsive_runtime_returns_within_the_inspect_budget() {
    let (root, host, _runner) = fixture("http-inspect-hang");
    write_browser_grant(&host.home);
    let mut state = HttpPluginState::empty();
    state.hang_status = true;
    let fake = HttpPluginFake::start(state);
    let started = Instant::now();
    let inspected = host
        .execute(DshApplicationHostRequest::Inspect {
            gateway_v1_url: "http://127.0.0.1:9042/v1".into(),
            profile_path: None,
            runtime_url: Some(fake.origin()),
        })
        .unwrap();
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_secs(5),
        "hung inspect waited {elapsed:?}"
    );
    assert_eq!(inspected.phase, DshApplicationPhase::NotDetected);
    assert!(!inspected.install_supported);
    assert_eq!(
        inspected.detail.as_deref(),
        Some("DSH running address is not reachable")
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn http_partial_failure_and_lost_response_keep_handoff_without_duplicate_install() {
    let (root, host, runner) = fixture("http-partial");
    write_browser_grant(&host.home);
    let mut failed = HttpPluginState::empty();
    failed.install_application = "failed".into();
    let fake = HttpPluginFake::start(failed);
    let gateway = "http://127.0.0.1:9042/v1";
    let inspected = host
        .execute(DshApplicationHostRequest::Inspect {
            gateway_v1_url: gateway.into(),
            profile_path: None,
            runtime_url: Some(fake.origin()),
        })
        .unwrap();
    let result = host
        .execute(DshApplicationHostRequest::Install {
            expected_fingerprint: inspected.fingerprint.unwrap(),
            gateway_v1_url: gateway.into(),
            profile_path: None,
            runtime_url: Some(fake.origin()),
            secret: crate::dsh_application::DshGatewaySecret::new("keep-handoff".into()),
        })
        .unwrap();
    assert_eq!(result.application, Some(DshApplicationOutcome::Failed));
    assert!(!result.installed);
    assert!(
        result
            .detail
            .as_deref()
            .is_some_and(|detail| detail.contains("stop-profile"))
    );
    assert_eq!(fs::read(host.bootstrap_path()).unwrap(), b"keep-handoff");

    let mut lost = HttpPluginState::empty();
    lost.drop_install = true;
    let lost_fake = HttpPluginFake::start(lost);
    let before = host
        .execute(DshApplicationHostRequest::Inspect {
            gateway_v1_url: gateway.into(),
            profile_path: None,
            runtime_url: Some(lost_fake.origin()),
        })
        .unwrap();
    let recovered = host
        .execute(DshApplicationHostRequest::Install {
            expected_fingerprint: before.fingerprint.unwrap(),
            gateway_v1_url: gateway.into(),
            profile_path: None,
            runtime_url: Some(lost_fake.origin()),
            secret: crate::dsh_application::DshGatewaySecret::new("lost-response".into()),
        })
        .unwrap();
    assert!(recovered.installed);
    assert_eq!(recovered.application, Some(DshApplicationOutcome::Applied));
    assert_eq!(
        lost_fake
            .methods()
            .iter()
            .filter(|method| method.as_str() == "pluginManager/installBundle")
            .count(),
        1
    );
    assert!(
        lost_fake
            .methods()
            .iter()
            .any(|method| method == "pluginManager/waitForInstall")
    );
    assert!(runner.commands.lock().unwrap().is_empty());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn http_stale_fingerprint_and_wrong_url_have_no_runtime_side_effects() {
    let (root, host, runner) = fixture("http-stale");
    write_browser_grant(&host.home);
    let fake = HttpPluginFake::start(HttpPluginState::empty());
    let gateway = "http://127.0.0.1:9042/v1";
    let inspected = host
        .execute(DshApplicationHostRequest::Inspect {
            gateway_v1_url: gateway.into(),
            profile_path: None,
            runtime_url: Some(fake.origin()),
        })
        .unwrap();
    fake.state.lock().unwrap().bundles = vec![ocg_bundle(true, true, true)];
    let error = host
        .execute(DshApplicationHostRequest::Install {
            expected_fingerprint: inspected.fingerprint.unwrap(),
            gateway_v1_url: gateway.into(),
            profile_path: None,
            runtime_url: Some(fake.origin()),
            secret: crate::dsh_application::DshGatewaySecret::new("stale-key".into()),
        })
        .unwrap_err();
    assert_eq!(
        error.kind,
        crate::dsh_application::DshApplicationErrorKind::Conflict
    );
    assert!(!host.bootstrap_path().exists());
    assert!(
        !fake
            .methods()
            .iter()
            .any(|method| method == "pluginManager/installBundle")
    );

    let rejected = host.execute(DshApplicationHostRequest::Inspect {
        gateway_v1_url: gateway.into(),
        profile_path: None,
        runtime_url: Some("http://192.168.1.9:3080".into()),
    });
    assert_eq!(
        rejected.unwrap_err().kind,
        crate::dsh_application::DshApplicationErrorKind::Invalid
    );
    assert!(runner.commands.lock().unwrap().is_empty());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn http_wait_for_install_null_is_unknown_and_reinstall_targets_runtime_package_name() {
    let gateway = "http://127.0.0.1:9042/v1";
    let (unknown_root, unknown_host, unknown_runner) = fixture("http-wait-null");
    write_browser_grant(&unknown_host.home);
    let mut unknown = HttpPluginState::empty();
    unknown.drop_install = true;
    unknown.wait_null = true;
    let fake = HttpPluginFake::start(unknown);
    let inspected = unknown_host
        .execute(DshApplicationHostRequest::Inspect {
            gateway_v1_url: gateway.into(),
            profile_path: None,
            runtime_url: Some(fake.origin()),
        })
        .unwrap();
    let lost = unknown_host
        .execute(DshApplicationHostRequest::Install {
            expected_fingerprint: inspected.fingerprint.unwrap(),
            gateway_v1_url: gateway.into(),
            profile_path: None,
            runtime_url: Some(fake.origin()),
            secret: crate::dsh_application::DshGatewaySecret::new("unknown-key".into()),
        })
        .unwrap();
    assert_eq!(lost.application, None);
    assert_eq!(
        fake.methods()
            .iter()
            .filter(|method| method.as_str() == "pluginManager/installBundle")
            .count(),
        1
    );
    assert!(unknown_runner.commands.lock().unwrap().is_empty());
    fs::remove_dir_all(unknown_root).unwrap();

    let (collision_root, collision_host, collision_runner) = fixture("http-collision");
    write_browser_grant(&collision_host.home);
    let mut present = HttpPluginState::empty();
    present.bundles = vec![ocg_bundle(true, true, true)];
    let collision = HttpPluginFake::start(present);
    let seen = collision_host
        .execute(DshApplicationHostRequest::Inspect {
            gateway_v1_url: gateway.into(),
            profile_path: None,
            runtime_url: Some(collision.origin()),
        })
        .unwrap();
    let replaced = collision_host
        .execute(DshApplicationHostRequest::Install {
            expected_fingerprint: seen.fingerprint.unwrap(),
            gateway_v1_url: gateway.into(),
            profile_path: None,
            runtime_url: Some(collision.origin()),
            secret: crate::dsh_application::DshGatewaySecret::new("foreign-key".into()),
        })
        .unwrap();
    assert_eq!(replaced.application, Some(DshApplicationOutcome::Applied));
    assert!(
        collision
            .methods()
            .iter()
            .any(|method| method == "pluginManager/installBundle")
    );
    assert!(collision_host.bootstrap_path().exists());
    assert!(collision_runner.commands.lock().unwrap().is_empty());
    fs::remove_dir_all(collision_root).unwrap();

    let (cancel_root, cancel_host, cancel_runner) = fixture("http-cancelled");
    write_browser_grant(&cancel_host.home);
    let mut cancelled = HttpPluginState::empty();
    cancelled.install_application = "cancelled".into();
    let cancel_fake = HttpPluginFake::start(cancelled);
    fs::create_dir_all(cancel_host.bootstrap_path().parent().unwrap()).unwrap();
    fs::write(cancel_host.bootstrap_path(), b"prior-live").unwrap();
    let ready = cancel_host
        .execute(DshApplicationHostRequest::Inspect {
            gateway_v1_url: gateway.into(),
            profile_path: None,
            runtime_url: Some(cancel_fake.origin()),
        })
        .unwrap();
    let cancelled_result = cancel_host
        .execute(DshApplicationHostRequest::Install {
            expected_fingerprint: ready.fingerprint.unwrap(),
            gateway_v1_url: gateway.into(),
            profile_path: None,
            runtime_url: Some(cancel_fake.origin()),
            secret: crate::dsh_application::DshGatewaySecret::new("cancelled-key".into()),
        })
        .unwrap();
    assert_eq!(
        cancelled_result.application,
        Some(DshApplicationOutcome::Cancelled)
    );
    assert_eq!(
        fs::read(cancel_host.bootstrap_path()).unwrap(),
        b"prior-live"
    );
    assert!(cancel_runner.commands.lock().unwrap().is_empty());
    fs::remove_dir_all(cancel_root).unwrap();
}

const CACHE_SECRET: &str = "ocg-test-secret-cache";

fn write_rendered_package(package: &RenderedPackage, directory: &Path) {
    for (relative, bytes) in &package.files {
        let destination = directory.join(relative);
        fs::create_dir_all(destination.parent().unwrap()).unwrap();
        fs::write(destination, bytes).unwrap();
    }
}

fn nested_relative(package: &RenderedPackage) -> &Path {
    package
        .files
        .keys()
        .find(|path| path.components().count() > 1)
        .map(PathBuf::as_path)
        .expect("rendered package has a nested locale file")
}

fn assert_package_hides_secret(package: &RenderedPackage, secret: &str) {
    assert!(package.published_package_ok());
    for relative in package.files.keys() {
        let bytes = fs::read(package.path.join(relative)).unwrap();
        assert!(
            !bytes
                .windows(secret.len())
                .any(|window| window == secret.as_bytes()),
            "package file {} contains the gateway key",
            relative.display()
        );
    }
}

fn http_install(
    host: &DshDesktopHost,
    origin: &str,
    gateway: &str,
    secret: &str,
) -> DshApplicationResult<DshApplicationInspection> {
    let inspected = host
        .execute(DshApplicationHostRequest::Inspect {
            gateway_v1_url: gateway.into(),
            profile_path: None,
            runtime_url: Some(origin.into()),
        })
        .unwrap();
    host.execute(DshApplicationHostRequest::Install {
        expected_fingerprint: inspected.fingerprint.expect("fingerprint"),
        gateway_v1_url: gateway.into(),
        profile_path: None,
        runtime_url: Some(origin.into()),
        secret: crate::dsh_application::DshGatewaySecret::new(secret.into()),
    })
}

fn http_uninstall(
    host: &DshDesktopHost,
    origin: &str,
    gateway: &str,
) -> DshApplicationResult<DshApplicationInspection> {
    let inspected = host
        .execute(DshApplicationHostRequest::Inspect {
            gateway_v1_url: gateway.into(),
            profile_path: None,
            runtime_url: Some(origin.into()),
        })
        .unwrap();
    host.execute(DshApplicationHostRequest::Uninstall {
        expected_fingerprint: inspected.fingerprint.expect("fingerprint"),
        gateway_v1_url: gateway.into(),
        profile_path: None,
        runtime_url: Some(origin.into()),
    })
}

#[cfg(windows)]
fn link_directory(target: &Path, link: &Path) -> std::io::Result<()> {
    use base64::Engine;
    use std::os::windows::process::CommandExt;
    let quote = |path: &Path| {
        path.to_string_lossy()
            .trim_start_matches(r"\\?\")
            .replace('\'', "''")
    };
    let script = format!(
        "New-Item -ItemType Junction -Path '{}' -Target '{}' -ErrorAction Stop | Out-Null",
        quote(link),
        quote(target)
    );
    let bytes = script
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect::<Vec<_>>();
    let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
    let result = std::process::Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-EncodedCommand", &encoded])
        .creation_flags(0x08000000)
        .output()?;
    if result.status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other(format!(
            "junction fixture creation failed: {}",
            String::from_utf8_lossy(&result.stderr)
        )))
    }
}

#[cfg(not(windows))]
fn link_directory(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

fn unlink_directory(path: &Path) {
    #[cfg(windows)]
    {
        let _ = fs::remove_dir(path);
    }
    #[cfg(not(windows))]
    {
        let _ = fs::remove_file(path);
    }
}

#[test]
fn http_missing_package_cache_recovers_across_repeat_install_and_reinstall() {
    let (root, host, runner) = fixture("http-cache-recovery");
    write_browser_grant(&host.home);
    let fake = HttpPluginFake::start(HttpPluginState::empty());
    let gateway = "http://127.0.0.1:9042/v1";
    let package = host.render_package(gateway).unwrap();
    fs::create_dir_all(&package.path).unwrap();

    let installed = http_install(&host, &fake.origin(), gateway, CACHE_SECRET).unwrap();
    assert_eq!(installed.phase, DshApplicationPhase::Installed);
    assert_package_hides_secret(&package, CACHE_SECRET);
    assert_eq!(
        fs::read(host.bootstrap_path()).unwrap(),
        CACHE_SECRET.as_bytes()
    );

    let repeated = http_install(&host, &fake.origin(), gateway, CACHE_SECRET).unwrap();
    assert!(repeated.installed);
    assert_package_hides_secret(&package, CACHE_SECRET);

    let removed = http_uninstall(&host, &fake.origin(), gateway).unwrap();
    assert!(!removed.installed);
    assert_package_hides_secret(&package, CACHE_SECRET);

    fs::remove_dir_all(&package.path).unwrap();
    fs::create_dir_all(&package.path).unwrap();
    let reinstalled = http_install(&host, &fake.origin(), gateway, CACHE_SECRET).unwrap();
    assert!(reinstalled.installed);
    assert_package_hides_secret(&package, CACHE_SECRET);

    for relative in package.files.keys() {
        if relative.components().count() > 1 {
            fs::remove_file(package.path.join(relative)).unwrap();
        }
    }
    assert!(!package.published_package_ok());
    let nested = http_install(&host, &fake.origin(), gateway, CACHE_SECRET).unwrap();
    assert!(nested.installed);
    assert_package_hides_secret(&package, CACHE_SECRET);
    assert!(runner.commands.lock().unwrap().is_empty());
    assert!(
        fake.methods()
            .iter()
            .any(|method| method == "pluginManager/installBundle")
    );
    assert!(
        fake.methods()
            .iter()
            .any(|method| method == "pluginManager/removeBundle")
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn rejected_package_cache_keeps_modified_unexpected_and_linked_bytes() {
    let (root, host, runner) = fixture("http-cache-reject");
    write_browser_grant(&host.home);
    let fake = HttpPluginFake::start(HttpPluginState::empty());
    let gateway = "http://127.0.0.1:9042/v1";
    let package = host.render_package(gateway).unwrap();
    fs::create_dir_all(&package.path).unwrap();
    let manifest = package.path.join("package.json");
    fs::write(&manifest, b"{\"name\":\"foreign-cache\"}").unwrap();

    let error = http_install(&host, &fake.origin(), gateway, CACHE_SECRET).unwrap_err();
    assert_eq!(
        error.kind,
        crate::dsh_application::DshApplicationErrorKind::Conflict
    );
    assert!(!error.message.contains(CACHE_SECRET));
    assert_eq!(
        fs::read(&manifest).unwrap(),
        b"{\"name\":\"foreign-cache\"}"
    );
    assert!(!package.path.join("index.js").exists());
    assert!(!host.bootstrap_path().exists());

    fs::remove_dir_all(&package.path).unwrap();
    let empty_extra = package.path.join("unexpected-empty");
    fs::create_dir_all(&empty_extra).unwrap();
    let error = http_install(&host, &fake.origin(), gateway, CACHE_SECRET).unwrap_err();
    assert_eq!(
        error.kind,
        crate::dsh_application::DshApplicationErrorKind::Conflict
    );
    assert!(empty_extra.is_dir());
    assert!(fs::read_dir(&empty_extra).unwrap().next().is_none());
    assert!(!package.path.join("package.json").exists());

    fs::remove_dir_all(&package.path).unwrap();
    fs::create_dir_all(&package.path).unwrap();
    let extra = package.path.join("notes.txt");
    fs::write(&extra, b"keep-notes").unwrap();
    let error = http_install(&host, &fake.origin(), gateway, CACHE_SECRET).unwrap_err();
    assert_eq!(
        error.kind,
        crate::dsh_application::DshApplicationErrorKind::Conflict
    );
    assert_eq!(fs::read(&extra).unwrap(), b"keep-notes");
    assert!(!package.path.join("index.js").exists());

    fs::remove_dir_all(&package.path).unwrap();
    fs::create_dir_all(&package.path).unwrap();
    let outside = root.join("linked-outside");
    fs::create_dir_all(&outside).unwrap();
    fs::write(outside.join("marker.txt"), b"keep-outside").unwrap();
    let linked = package.path.join("extra-link");
    link_directory(&outside, &linked).expect("package link fixture");
    let error = package.materialize().unwrap_err();
    assert_eq!(
        error.kind,
        crate::dsh_application::DshApplicationErrorKind::Conflict
    );
    assert_eq!(
        fs::read(outside.join("marker.txt")).unwrap(),
        b"keep-outside"
    );
    assert!(is_link_or_reparse(&linked));
    assert!(!package.path.join("index.js").exists());
    assert!(
        !fake
            .methods()
            .iter()
            .any(|method| method == "pluginManager/installBundle")
    );
    assert!(runner.commands.lock().unwrap().is_empty());
    unlink_directory(&linked);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn package_root_and_ancestor_links_do_not_receive_package_bytes() {
    let (root, host, runner) = fixture("package-anchor-links");
    let gateway = "http://127.0.0.1:9042/v1";
    let package = host.render_package(gateway).unwrap();
    let outside = root.join("outside");
    fs::create_dir_all(&outside).unwrap();
    fs::write(outside.join("marker.txt"), b"keep-outside").unwrap();

    let applications = host.data_dir.join("applications");
    link_directory(&outside, &applications).expect("ancestor link fixture");
    let inspected = host.inspect(gateway).unwrap();
    let error = host
        .install(
            inspected.fingerprint.as_deref().unwrap(),
            gateway,
            CACHE_SECRET,
        )
        .unwrap_err();
    assert_eq!(
        error.kind,
        crate::dsh_application::DshApplicationErrorKind::Conflict
    );
    assert!(!error.message.contains(CACHE_SECRET));
    assert_eq!(
        fs::read(outside.join("marker.txt")).unwrap(),
        b"keep-outside"
    );
    assert!(!outside.join("dsh").exists());
    assert!(is_link_or_reparse(&applications));
    assert!(runner.commands.lock().unwrap().is_empty());
    unlink_directory(&applications);

    let dsh_dir = host.data_dir.join("applications").join("dsh");
    fs::create_dir_all(&dsh_dir).unwrap();
    let packages = dsh_dir.join("packages-v1");
    link_directory(&outside, &packages).expect("package root link fixture");
    let error = package.materialize().unwrap_err();
    assert_eq!(
        error.kind,
        crate::dsh_application::DshApplicationErrorKind::Conflict
    );
    assert_eq!(
        fs::read(outside.join("marker.txt")).unwrap(),
        b"keep-outside"
    );
    assert!(is_link_or_reparse(&packages));
    assert!(!outside.join(package.path.file_name().unwrap()).exists());
    unlink_directory(&packages);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn nested_locale_cache_accepts_normalized_digest_and_rejects_modified_bytes() {
    let (root, host, _) = fixture("digest-ownership");
    let gateway = "http://127.0.0.1:9042/v1";
    let package = host.render_package(gateway).unwrap();
    let nested = nested_relative(&package);
    write_rendered_package(&package, &package.path);
    assert!(is_owned_package_source(
        &package.path,
        &package.trusted_root
    ));

    let legacy = legacy_native_package_digest(&package.files);
    #[cfg(windows)]
    {
        assert_ne!(legacy, package.digest);
        let legacy_path = package.trusted_root.join(&legacy[..24]);
        assert_ne!(legacy_path, package.path);
        write_rendered_package(&package, &legacy_path);
        assert!(is_owned_package_source(&legacy_path, &package.trusted_root));
        let legacy_nested = legacy_path.join(nested);
        let mut changed = fs::read(&legacy_nested).unwrap();
        changed[0] ^= 0xff;
        fs::write(&legacy_nested, &changed).unwrap();
        assert!(!is_owned_package_source(
            &legacy_path,
            &package.trusted_root
        ));
    }
    #[cfg(not(windows))]
    assert_eq!(legacy, package.digest);

    let normalized_nested = package.path.join(nested);
    let mut changed = fs::read(&normalized_nested).unwrap();
    changed[0] ^= 0xff;
    fs::write(&normalized_nested, &changed).unwrap();
    assert!(!is_owned_package_source(
        &package.path,
        &package.trusted_root
    ));

    let arbitrary = package.trusted_root.join("0123456789abcdef01234567");
    assert_ne!(arbitrary, package.path);
    assert_ne!(
        arbitrary.file_name().and_then(|value| value.to_str()),
        Some(&legacy[..24])
    );
    write_rendered_package(&package, &arbitrary);
    assert!(!is_owned_package_source(&arbitrary, &package.trusted_root));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn cli_registered_partial_cache_installs_again_without_adopting_foreign_bytes() {
    let gateway = "http://127.0.0.1:9042/v1";
    let (root, host, runner) = fixture("cli-partial-cache");
    let package = host.render_package(gateway).unwrap();
    let nested = nested_relative(&package);
    let nested_path = package.path.join(nested);
    fs::create_dir_all(nested_path.parent().unwrap()).unwrap();
    fs::write(&nested_path, package.files.get(nested).unwrap()).unwrap();
    let kept = fs::read(&nested_path).unwrap();
    mutate_test_manifest(
        &host.home.join("profiles/web/package.json"),
        Some(format!("file:{}", package.path.display())),
    )
    .unwrap();

    let inspected = host.inspect(gateway).unwrap();
    assert_eq!(inspected.phase, DshApplicationPhase::Ready);
    assert!(inspected.install_supported);
    assert!(!inspected.installed);
    let installed = host
        .install(
            inspected.fingerprint.as_deref().unwrap(),
            gateway,
            CACHE_SECRET,
        )
        .unwrap();
    assert_eq!(installed.phase, DshApplicationPhase::Installed);
    assert_package_hides_secret(&package, CACHE_SECRET);
    assert_eq!(fs::read(&nested_path).unwrap(), kept);

    let again = host.inspect(gateway).unwrap();
    host.install(again.fingerprint.as_deref().unwrap(), gateway, CACHE_SECRET)
        .unwrap();
    assert_package_hides_secret(&package, CACHE_SECRET);
    let commands = runner.commands.lock().unwrap();
    assert_eq!(commands.len(), 2);
    assert!(commands.iter().all(|command| {
        command
            .args
            .iter()
            .all(|arg| !arg.to_string_lossy().contains(CACHE_SECRET))
    }));
    drop(commands);
    fs::remove_dir_all(root).unwrap();

    let (foreign_root, foreign_host, foreign_runner) = fixture("cli-foreign-partial");
    let foreign_package = foreign_host.render_package(gateway).unwrap();
    let foreign_dir = foreign_package.trusted_root.join("abcdefabcdefabcdefabcd");
    assert_ne!(foreign_dir, foreign_package.path);
    fs::create_dir_all(foreign_dir.join("locale")).unwrap();
    let foreign_manifest = foreign_dir.join("package.json");
    fs::write(
        &foreign_manifest,
        b"{\"name\":\"@open-console-gateway/dsh-plugin\"}",
    )
    .unwrap();
    mutate_test_manifest(
        &foreign_host.home.join("profiles/web/package.json"),
        Some(format!("file:{}", foreign_dir.display())),
    )
    .unwrap();
    let seen = foreign_host.inspect(gateway).unwrap();
    assert_eq!(seen.phase, DshApplicationPhase::Conflict);
    assert!(!seen.install_supported);
    let error = foreign_host
        .install(seen.fingerprint.as_deref().unwrap(), gateway, CACHE_SECRET)
        .unwrap_err();
    assert_eq!(
        error.kind,
        crate::dsh_application::DshApplicationErrorKind::Precondition
    );
    assert!(!error.message.contains(CACHE_SECRET));
    assert_eq!(
        fs::read(&foreign_manifest).unwrap(),
        b"{\"name\":\"@open-console-gateway/dsh-plugin\"}"
    );
    assert!(
        fs::read_dir(foreign_dir.join("locale"))
            .unwrap()
            .next()
            .is_none()
    );
    assert!(!foreign_package.path.exists());
    assert!(foreign_runner.commands.lock().unwrap().is_empty());
    fs::remove_dir_all(foreign_root).unwrap();

    #[cfg(windows)]
    {
        let (legacy_root, legacy_host, legacy_runner) = fixture("cli-legacy-partial");
        let legacy_package = legacy_host.render_package(gateway).unwrap();
        let legacy = legacy_native_package_digest(&legacy_package.files);
        assert_ne!(legacy, legacy_package.digest);
        let legacy_dir = legacy_package.trusted_root.join(&legacy[..24]);
        assert_ne!(legacy_dir, legacy_package.path);
        let legacy_nested = nested_relative(&legacy_package);
        let legacy_file = legacy_dir.join(legacy_nested);
        fs::create_dir_all(legacy_file.parent().unwrap()).unwrap();
        fs::write(
            &legacy_file,
            legacy_package.files.get(legacy_nested).unwrap(),
        )
        .unwrap();
        let legacy_kept = fs::read(&legacy_file).unwrap();
        mutate_test_manifest(
            &legacy_host.home.join("profiles/web/package.json"),
            Some(format!("file:{}", legacy_dir.display())),
        )
        .unwrap();
        let ready = legacy_host.inspect(gateway).unwrap();
        assert!(ready.install_supported);
        legacy_host
            .install(ready.fingerprint.as_deref().unwrap(), gateway, CACHE_SECRET)
            .unwrap();
        assert_package_hides_secret(&legacy_package, CACHE_SECRET);
        assert_eq!(fs::read(&legacy_file).unwrap(), legacy_kept);
        let leftover = walk_files(&legacy_dir);
        assert_eq!(leftover, vec![legacy_file]);
        assert!(!legacy_runner.commands.lock().unwrap().is_empty());
        fs::remove_dir_all(legacy_root).unwrap();
    }
}

#[cfg_attr(not(windows), allow(dead_code))]
fn walk_files(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut directories = vec![root.to_path_buf()];
    while let Some(directory) = directories.pop() {
        for entry in fs::read_dir(&directory).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                directories.push(path);
            } else {
                files.push(path);
            }
        }
    }
    files.sort();
    files
}

#[test]
fn changed_destination_keeps_the_detached_package_backup() {
    let (root, host, _) = fixture("backup-preserve");
    let package = host.render_package("http://127.0.0.1:9042/v1").unwrap();
    fs::create_dir_all(&package.path).unwrap();
    let backup = package
        .path
        .parent()
        .unwrap()
        .join(".ocg-dsh-package-test.bak");
    fs::rename(&package.path, &backup).unwrap();
    fs::create_dir_all(&package.path).unwrap();
    fs::write(package.path.join("external.txt"), b"external-change").unwrap();
    let snapshot = std::collections::BTreeMap::new();
    let preserved = restore_missing_only_backup(
        &package.trusted_root,
        &backup,
        &package.path,
        &package.files,
        &snapshot,
    );
    assert_eq!(preserved, BackupRestore::Preserved);
    assert_eq!(
        fs::read(package.path.join("external.txt")).unwrap(),
        b"external-change"
    );
    assert!(backup.is_dir());
    assert!(fs::read_dir(&backup).unwrap().next().is_none());

    fs::remove_dir_all(&package.path).unwrap();
    let restored = restore_missing_only_backup(
        &package.trusted_root,
        &backup,
        &package.path,
        &package.files,
        &snapshot,
    );
    assert_eq!(restored, BackupRestore::Restored);
    assert!(package.path.is_dir());
    assert!(!path_is_present(&backup));
    assert!(fs::read_dir(&package.path).unwrap().next().is_none());
    assert!(!format!("{restored:?}").contains(CACHE_SECRET));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn recreated_destination_and_changed_detached_path_are_not_rolled_back() {
    let (root, host, _) = fixture("unsafe-rollback");
    let package = host.render_package("http://127.0.0.1:9042/v1").unwrap();
    fs::create_dir_all(&package.path).unwrap();
    fs::write(package.path.join("kept.txt"), b"original-package").unwrap();
    let detached = package
        .path
        .parent()
        .unwrap()
        .join(".ocg-dsh-package-unsafe.bak");
    rename_package_directory(&package.trusted_root, &package.path, &detached).unwrap();
    fs::write(&package.path, b"recreated-destination").unwrap();
    fs::remove_dir_all(&detached).unwrap();
    fs::write(&detached, b"changed-detached").unwrap();

    let error =
        rollback_renamed_package(&package.trusted_root, &detached, &package.path).unwrap_err();
    assert_eq!(
        error.kind,
        crate::dsh_application::DshApplicationErrorKind::Conflict
    );
    assert!(error.message.contains(&detached.display().to_string()));
    assert!(!error.message.contains(CACHE_SECRET));
    assert_eq!(fs::read(&package.path).unwrap(), b"recreated-destination");
    assert_eq!(fs::read(&detached).unwrap(), b"changed-detached");
    fs::remove_dir_all(root).unwrap();
}

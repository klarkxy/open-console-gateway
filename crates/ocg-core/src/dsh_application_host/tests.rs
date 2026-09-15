use super::*;
use std::sync::Mutex as StdMutex;

struct FakeRunner {
    manifest: PathBuf,
    commands: StdMutex<Vec<CommandSpec>>,
}

struct FailingAddRunner {
    manifest: PathBuf,
    commands: StdMutex<Vec<CommandSpec>>,
    fail_next_add: StdMutex<bool>,
    write_foreign_on_failure: bool,
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
                stdout: "0.1.5-rc.2\n".into(),
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
        fs::create_dir_all(self.manifest.parent().unwrap()).map_err(|error| error.to_string())?;
        fs::write(
            &self.manifest,
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
    let root =
        std::env::temp_dir().join(format!("ocg-dsh-{name}-{}", uuid::Uuid::new_v4().simple()));
    let data_dir = root.join("data");
    let home = root.join("home");
    fs::create_dir_all(&data_dir).unwrap();
    fs::create_dir_all(&home).unwrap();
    let executable = root.join(if cfg!(windows) { "dsh.cmd" } else { "dsh" });
    fs::write(&executable, b"test-only").unwrap();
    let runner = Arc::new(FakeRunner {
        manifest: home.join("profiles/web/package.json"),
        commands: StdMutex::new(Vec::new()),
    });
    let host = DshDesktopHost {
        data_dir,
        home,
        runner: runner.clone(),
        dsh_executable: Some(executable),
        operation: Mutex::new(()),
    };
    (root, host, runner)
}

#[test]
fn supported_version_range_is_explicit() {
    assert!(compatible_version("0.1.5-rc.1"));
    assert!(compatible_version("0.1.5-rc.2"));
    assert!(!compatible_version("0.1.5"));
    assert!(!compatible_version("0.1.6"));
    assert!(!compatible_version("0.1.4"));
    assert!(!compatible_version("0.1.5-rc.0"));
    assert!(!compatible_version("0.1.5-rc.3"));
    assert!(!compatible_version("0.2.0"));
    assert!(!compatible_version("garbage"));
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
        4,
        "inspect, install preflight, add, and readback commands"
    );
    let add = &commands[2];
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
    assert_eq!(runner.commands.lock().unwrap().len(), 2);
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
        4,
        "inspect, preflight, failed add, rollback remove"
    );
    assert_eq!(commands[2].args[3], "add");
    assert_eq!(commands[3].args[3], "remove");
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
    assert_eq!(commands.len(), 3, "inspect, preflight, failed add only");
    assert_eq!(commands[2].args[3], "add");
    drop(commands);
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
             >>\"{captured}\" echo(!NEXT!\r\n",
            captured = captured.display()
        ),
    )
    .unwrap();

    let output = ProcessCommandRunner
        .run(&CommandSpec {
            executable: script,
            display_executable: "probe.cmd".into(),
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
            "next-arg"
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
    let result = ProcessCommandRunner.run(&CommandSpec {
        executable: script,
        display_executable: "hold-stdout".into(),
        args: vec![OsString::from(pidfile.as_os_str())],
        timeout: Duration::from_secs(2),
    });
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
        .run(&CommandSpec {
            executable: script,
            display_executable: "sleep-leader".into(),
            args: vec![OsString::from(pidfile.as_os_str())],
            timeout: Duration::from_millis(400),
        })
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
        .run(&CommandSpec {
            executable: script,
            display_executable: "echo-output".into(),
            args: Vec::new(),
            timeout: Duration::from_secs(5),
        })
        .unwrap();
    assert!(output.success, "stderr={}", output.stderr);
    assert_eq!(output.stdout.trim(), "hello-dsh");
    fs::remove_dir_all(root).unwrap();
}

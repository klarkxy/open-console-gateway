use super::*;
#[cfg(unix)]
use std::path::PathBuf;
use std::sync::Arc;

#[test]
fn stream_redaction_flushes_prompt_without_waiting_for_more_output() {
    let secrets = Arc::new(Mutex::new(vec![b"management-secret".to_vec()]));
    let mut redactor = StreamRedactor::new(secrets);
    let prompt = b"Codex device code: ABCD-1234\n";
    assert_eq!(redactor.push(prompt), prompt);
}

#[test]
fn stream_redaction_protects_every_chunk_boundary_and_overlapping_prefix() {
    let text = b"x management-secret y abcdef z";
    for split in 0..=text.len() {
        let secrets = Arc::new(Mutex::new(normalize_secrets(vec![
            b"management-secret".to_vec(),
            b"abc".to_vec(),
            b"abcdef".to_vec(),
        ])));
        let mut redactor = StreamRedactor::new(secrets);
        let output = [
            redactor.push(&text[..split]),
            redactor.push(&text[split..]),
            redactor.finish(),
        ]
        .concat();
        assert_eq!(output, b"x [REDACTED] y [REDACTED] z", "split {split}");
    }
}

#[cfg(unix)]
#[test]
fn unix_supervisor_entry() {
    let Some(executable) = std::env::var_os("OCG_CPA_TEST_EXECUTABLE") else {
        return;
    };
    let config = std::env::var_os("OCG_CPA_TEST_CONFIG").unwrap();
    supervisor::run(
        std::path::Path::new(&executable),
        std::path::Path::new(&config),
        std::env::var("OCG_CPA_TEST_DEVICE").as_deref() == Ok("1"),
    );
}

#[cfg(unix)]
fn unix_fixture(dir: &std::path::Path, body: &str) -> CpaRuntimeProcessSpec {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(dir).unwrap();
    let executable = dir.join("fake CPA executable");
    let config_path = dir.join("config with spaces.yaml");
    std::fs::write(&executable, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::write(&config_path, "host: 127.0.0.1\n").unwrap();
    CpaRuntimeProcessSpec {
        codex_device_login: false,
        executable,
        config_path,
        working_dir: dir.to_owned(),
        management_password: CpaRuntimeSecret::new("test-management-secret"),
        log_secrets: Vec::new(),
    }
}

#[cfg(unix)]
fn unix_wait_until(mut condition: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(9);
    while !condition() {
        assert!(
            std::time::Instant::now() < deadline,
            "Unix lifecycle timed out"
        );
        thread::sleep(std::time::Duration::from_millis(20));
    }
}

#[cfg(unix)]
fn unix_process_exited(pid: &str) -> bool {
    // Orphaned killed descendants can remain zombies under container PID 1;
    // they are terminated and cannot hold descriptors, even before reaping.
    let output = std::process::Command::new("ps")
        .args(["-o", "stat=", "-p", pid.trim()])
        .output()
        .unwrap();
    let status = String::from_utf8_lossy(&output.stdout);
    status.trim().is_empty() || status.trim().starts_with('Z')
}

#[cfg(unix)]
#[test]
fn unix_host_fixture_entry() {
    let Some(dir) = std::env::var_os("OCG_CPA_TEST_HOST_DIR") else {
        return;
    };
    let dir = PathBuf::from(dir);
    let spec = unix_fixture(
        &dir,
        "trap '' TERM\necho $$ > leader.pid\nsleep 60 &\necho $! > descendant.pid\necho ready\nwait",
    );
    let session = spawn_unix_owned(&spec).unwrap();
    unix_wait_until(|| session.logs().stdout.contains("ready"));
    std::fs::write(dir.join("host-ready"), "ready").unwrap();
    loop {
        thread::sleep(std::time::Duration::from_secs(60));
    }
}

#[cfg(unix)]
#[test]
fn unix_host_sigkill_closes_lifetime_pipe_and_kills_cpa_tree() {
    let dir = std::env::temp_dir().join(format!("ocg host death {}", uuid::Uuid::new_v4()));
    let mut host = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "cpa_runtime::host::tests::unix_host_fixture_entry",
            "--nocapture",
        ])
        .env("OCG_CPA_TEST_HOST_DIR", &dir)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    unix_wait_until(|| dir.join("host-ready").is_file());
    let leader = std::fs::read_to_string(dir.join("leader.pid")).unwrap();
    let descendant = std::fs::read_to_string(dir.join("descendant.pid")).unwrap();
    assert!(!unix_process_exited(&leader));
    assert!(!unix_process_exited(&descendant));
    host.kill().unwrap();
    host.wait().unwrap();
    unix_wait_until(|| unix_process_exited(&leader) && unix_process_exited(&descendant));
    std::fs::remove_dir_all(dir).unwrap();
}

#[cfg(unix)]
#[test]
fn unix_early_leader_exit_cleans_descendant_logs_and_repeated_stop_spares_unrelated_process() {
    let dir = std::env::temp_dir().join(format!("ocg early exit {}", uuid::Uuid::new_v4()));
    let spec = unix_fixture(
        &dir,
        "(trap '' TERM; exec sleep 60) &\necho $! > descendant.pid\necho leader-done\nexit 0",
    );
    let mut unrelated = std::process::Command::new("sleep")
        .arg("60")
        .spawn()
        .unwrap();
    for _ in 0..2 {
        let mut session = spawn_unix_owned(&spec).unwrap();
        unix_wait_until(|| session.logs().stdout.contains("leader-done"));
        let descendant = std::fs::read_to_string(dir.join("descendant.pid")).unwrap();
        unix_wait_until(|| !session.is_running()); // Reap before stop and Drop.
        let started = std::time::Instant::now();
        assert!(session.stop().unwrap().stdout.contains("leader-done"));
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
        unix_wait_until(|| unix_process_exited(&descendant));
        assert!(unrelated.try_wait().unwrap().is_none());
    }
    unrelated.kill().unwrap();
    unrelated.wait().unwrap();
    std::fs::remove_dir_all(dir).unwrap();
}

#[cfg(unix)]
#[test]
fn unix_lifetime_writer_is_cloexec_and_eof_stops_while_other_child_lives() {
    use std::os::fd::AsRawFd;
    let dir = std::env::temp_dir().join(format!("ocg eof {}", uuid::Uuid::new_v4()));
    let spec = unix_fixture(&dir, "trap '' TERM\necho ready\nwhile :; do sleep 1; done");
    let mut session = spawn_unix_owned(&spec).unwrap();
    let fd = session.lifetime.as_ref().unwrap().as_raw_fd();
    let flags = unsafe { nix::libc::fcntl(fd, nix::libc::F_GETFD) };
    assert_ne!(flags, -1);
    assert_ne!(flags & nix::libc::FD_CLOEXEC, 0);
    let mut unrelated = std::process::Command::new("sleep")
        .arg("60")
        .spawn()
        .unwrap();
    unix_wait_until(|| session.logs().stdout.contains("ready"));
    drop(session.lifetime.take());
    unix_wait_until(|| !session.is_running());
    session.stop().unwrap();
    assert!(unrelated.try_wait().unwrap().is_none());
    unrelated.kill().unwrap();
    unrelated.wait().unwrap();
    std::fs::remove_dir_all(dir).unwrap();
}

#[cfg(unix)]
#[test]
fn unix_supervisor_preserves_spaced_paths_config_argv_and_redacts_secret_logs() {
    let dir = std::env::temp_dir().join(format!("ocg paths {}", uuid::Uuid::new_v4()));
    let mut spec = unix_fixture(
        &dir,
        "printf 'arg1=%s\\narg2=%s\\n' \"$1\" \"$2\"\nprintf '%s\\n' \"$MANAGEMENT_PASSWORD\"\nprintf '%s\\n' \"$MANAGEMENT_PASSWORD\" >&2\nexit 0",
    );
    spec.log_secrets.push(spec.management_password.clone());
    let mut session = spawn_unix_owned(&spec).unwrap();
    unix_wait_until(|| !session.is_running());
    let logs = session.stop().unwrap();
    assert!(logs.stdout.contains("arg1=--config\n"));
    assert!(
        logs.stdout
            .contains(&format!("arg2={}\n", spec.config_path.display()))
    );
    assert!(logs.stdout.contains("[REDACTED]"));
    assert!(logs.stderr.contains("[REDACTED]"));
    assert!(!logs.stdout.contains("test-management-secret"));
    assert!(!logs.stderr.contains("test-management-secret"));
    std::fs::remove_dir_all(dir).unwrap();
}

#[cfg(unix)]
#[test]
fn unix_log_reader_finishes_when_an_open_writer_survives_shutdown() {
    use std::io::Write;
    let (reader, mut writer) = std::os::unix::net::UnixStream::pair().unwrap();
    let buffer = Arc::new(Mutex::new(String::new()));
    let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let handle = spawn_unix_reader(
        reader,
        buffer.clone(),
        Arc::new(Mutex::new(Vec::new())),
        done.clone(),
    );
    writer.write_all(b"last log line\n").unwrap();
    unix_wait_until(|| buffer.lock().contains("last log line"));
    done.store(true, std::sync::atomic::Ordering::Release);
    unix_wait_until(|| handle.is_finished());
    handle.join().unwrap();
    assert_eq!(&*buffer.lock(), "last log line\n");
    drop(writer);
}

#[cfg(windows)]
fn utf16_to_string(wide: &[u16]) -> String {
    let end = wide
        .iter()
        .position(|unit| *unit == 0)
        .unwrap_or(wide.len());
    String::from_utf16_lossy(&wide[..end])
}

#[cfg(windows)]
#[test]
fn command_line_quotes_paths_and_omits_the_management_password() {
    let line = utf16_to_string(
        &windows_command_line(
            Path::new(r"C:\data\cpa\versions\7.2.147\cli-proxy-api.exe"),
            Path::new(r"C:\data\cpa\config.yaml"),
        )
        .unwrap(),
    );
    assert!(line.contains("--config"));
    assert!(line.contains("cli-proxy-api.exe"));
    assert!(!line.contains("MANAGEMENT_PASSWORD"));
    assert!(!line.contains("secret"));
}

#[cfg(windows)]
#[test]
fn environment_block_sets_management_password() {
    let block = windows_environment("cpa-test-secret");
    let text = String::from_utf16_lossy(&block);
    assert!(text.contains("MANAGEMENT_PASSWORD=cpa-test-secret"));
}

#[cfg(unix)]
#[test]
fn unix_command_rejects_missing_executable() {
    let spec = CpaRuntimeProcessSpec {
        codex_device_login: false,
        executable: PathBuf::from("/no/such/cpa-binary"),
        config_path: PathBuf::from("/tmp/config.yaml"),
        working_dir: PathBuf::from("/tmp"),
        management_password: CpaRuntimeSecret::new("secret"),
        log_secrets: Vec::new(),
    };
    spawn_unix_owned(&spec)
        .err()
        .expect("missing executable must fail");
}

#[cfg(unix)]
#[test]
fn owned_sleep_process_is_group_contained_and_stoppable() {
    let dir = std::env::temp_dir().join(format!("ocg-cpa-unix-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let script = dir.join("sleep-child");
    std::fs::write(&script, "#!/bin/sh\nexec sleep 20\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let config = dir.join("config.yaml");
    std::fs::write(&config, "host: \"127.0.0.1\"\n").unwrap();
    let spec = CpaRuntimeProcessSpec {
        codex_device_login: false,
        executable: script,
        config_path: config,
        working_dir: dir.clone(),
        management_password: CpaRuntimeSecret::new("cpa-test-secret"),
        log_secrets: vec![CpaRuntimeSecret::new("cpa-test-secret")],
    };
    let session = spawn_unix_owned(&spec).expect("sleep child should start");
    session
        .stop()
        .expect("owned sleep should exit after process-group signal");
    let _ = std::fs::remove_dir_all(dir);
}

#[cfg(unix)]
#[test]
fn owned_process_ignoring_term_is_killed_reaped_and_returns_logs() {
    use std::os::unix::fs::PermissionsExt;
    let dir = std::env::temp_dir().join(format!("ocg-cpa-force-stop-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let script = dir.join("ignore-term");
    std::fs::write(
        &script,
        "#!/bin/sh\ntrap '' TERM\necho ready\nwhile :; do sleep 1; done\n",
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let config = dir.join("config.yaml");
    std::fs::write(&config, "host: 127.0.0.1\n").unwrap();
    let spec = CpaRuntimeProcessSpec {
        codex_device_login: false,
        executable: script,
        config_path: config,
        working_dir: dir.clone(),
        management_password: CpaRuntimeSecret::new("test-secret"),
        log_secrets: Vec::new(),
    };
    let session = spawn_unix_owned(&spec).unwrap();
    let pid = nix::unistd::Pid::from_raw(session.child.id() as i32);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !session.logs().stdout.contains("ready") {
        assert!(
            std::time::Instant::now() < deadline,
            "child did not become ready"
        );
        thread::sleep(std::time::Duration::from_millis(10));
    }
    let started = std::time::Instant::now();
    let logs = session
        .stop()
        .expect("successful forced termination is a successful stop");
    assert!(started.elapsed() >= std::time::Duration::from_secs(5));
    assert!(logs.stdout.contains("ready"));
    assert_eq!(
        nix::sys::wait::waitpid(pid, None),
        Err(nix::errno::Errno::ECHILD)
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[cfg(windows)]
#[test]
fn owned_cmd_process_is_job_contained_and_stoppable() {
    use std::os::windows::ffi::OsStrExt;
    let exe = std::path::PathBuf::from(r"C:\Windows\System32\cmd.exe");
    let application: Vec<u16> = exe
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let command_line: Vec<u16> =
        std::ffi::OsString::from(r"C:\Windows\System32\cmd.exe /c ping -n 20 127.0.0.1")
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
    let environment = windows_environment("cpa-test-secret");
    let session = spawn_process(
        &application,
        command_line,
        environment,
        Some(exe.parent().unwrap()),
        vec![b"cpa-test-secret".to_vec()],
    )
    .expect("cmd ping should start");
    assert!(session.is_running());
    session
        .stop()
        .expect("owned cmd should exit after TerminateJobObject");
}

#[cfg(windows)]
#[test]
fn decide_owned_stop_table() {
    use windows_sys::Win32::Foundation::{WAIT_FAILED, WAIT_OBJECT_0, WAIT_TIMEOUT};
    for (label, terminated, wait, expected) in [
        (
            "confirmed-exit-joins-readers",
            true,
            WAIT_OBJECT_0,
            OwnedStopDecision::JoinReaders,
        ),
        (
            "terminate-failed-detaches",
            false,
            WAIT_OBJECT_0,
            OwnedStopDecision::TerminateFailed,
        ),
        (
            "wait-timeout-detaches",
            true,
            WAIT_TIMEOUT,
            OwnedStopDecision::WaitTimedOut,
        ),
        (
            "wait-failed-detaches",
            true,
            WAIT_FAILED,
            OwnedStopDecision::WaitFailed,
        ),
    ] {
        assert_eq!(decide_owned_stop(terminated, wait), expected, "{label}");
    }
}

#[test]
fn stream_redaction_covers_secrets_split_across_chunks() {
    let secrets = Arc::new(Mutex::new(normalize_secrets(vec![
        b"management-secret".to_vec(),
        b"inference-secret".to_vec(),
    ])));
    let mut redactor = StreamRedactor::new(secrets.clone());
    let mut output = redactor.push(b"before management-");
    output.extend(redactor.push(b"secret and inference-se"));
    output.extend(redactor.push(b"cret after"));
    output.extend(redactor.finish());
    let text = String::from_utf8(output).unwrap();
    assert_eq!(text, "before [REDACTED] and [REDACTED] after");

    secrets.lock().push(b"rotated-secret".to_vec());
    let mut output = redactor.push(b" rotated-");
    output.extend(redactor.push(b"secret"));
    output.extend(redactor.finish());
    assert_eq!(String::from_utf8(output).unwrap(), " [REDACTED]");
}

#[test]
fn stream_redaction_drops_empty_secrets_and_does_not_match_them() {
    let secrets = Arc::new(Mutex::new(normalize_secrets(vec![
        Vec::new(),
        b"token".to_vec(),
        Vec::new(),
    ])));
    assert_eq!(secrets.lock().as_slice(), [b"token".to_vec()]);
    let mut redactor = StreamRedactor::new(secrets);
    let output = [redactor.push(b"pre token post"), redactor.finish()].concat();
    assert_eq!(String::from_utf8(output).unwrap(), "pre [REDACTED] post");
}

#[test]
fn stream_redaction_longer_secret_added_later_wins_prefix() {
    let secrets = Arc::new(Mutex::new(normalize_secrets(vec![b"abc".to_vec()])));
    let mut redactor = StreamRedactor::new(secrets.clone());
    {
        let mut known = secrets.lock();
        if !known.iter().any(|secret| secret == b"abcdef") {
            known.push(b"abcdef".to_vec());
            known.sort_by_key(|secret| std::cmp::Reverse(secret.len()));
        }
    }
    let output = [redactor.push(b"abcdef"), redactor.finish()].concat();
    assert_eq!(String::from_utf8(output).unwrap(), "[REDACTED]");
}

#[cfg(windows)]
fn process_still_active(process: &OwnedHandle) -> bool {
    const STILL_ACTIVE: u32 = 259;
    let mut code = 0u32;
    unsafe {
        windows_sys::Win32::System::Threading::GetExitCodeProcess(process.0, &mut code) != 0
            && code == STILL_ACTIVE
    }
}

#[cfg(windows)]
fn spawn_suspended_unassigned() -> (OwnedHandle, OwnedHandle, JobObject) {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::System::Threading::{
        CREATE_NO_WINDOW, CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, CreateProcessW,
        PROCESS_INFORMATION, STARTUPINFOW,
    };
    let exe = std::path::PathBuf::from(r"C:\Windows\System32\cmd.exe");
    let application: Vec<u16> = exe
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let mut command_line: Vec<u16> =
        std::ffi::OsString::from(r"C:\Windows\System32\cmd.exe /c ping -n 20 127.0.0.1")
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
    let mut environment = windows_environment("cpa-test-secret");
    let mut startup: STARTUPINFOW = unsafe { std::mem::zeroed() };
    startup.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
    let mut information: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
    let created = unsafe {
        CreateProcessW(
            application.as_ptr(),
            command_line.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            0,
            CREATE_SUSPENDED | CREATE_NO_WINDOW | CREATE_UNICODE_ENVIRONMENT,
            environment.as_mut_ptr().cast(),
            std::ptr::null(),
            &startup,
            &mut information,
        )
    };
    assert_ne!(created, 0, "CREATE_SUSPENDED cmd should start");
    (
        OwnedHandle(information.hProcess),
        OwnedHandle(information.hThread),
        JobObject::new().expect("job"),
    )
}

#[cfg(windows)]
#[test]
fn unassigned_suspended_process_is_killed_by_its_handle_not_the_job() {
    let (process, thread, job) = spawn_suspended_unassigned();
    job.terminate();
    assert!(
        process_still_active(&process),
        "TerminateJobObject must not reach a process that was never assigned"
    );
    abandon_unassigned_process(&process);
    assert!(
        !process_still_active(&process),
        "the exact created process handle must be terminated and waited"
    );
    drop(thread);
}

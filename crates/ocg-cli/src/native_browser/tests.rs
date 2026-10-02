use super::{
    BrowserEnvironment, BrowserKind, BrowserPlatform, BrowserProcessState, browser_arguments,
    close_all_browser_processes, delete_browser_profile_dirs_at_paths, discover_browser_with,
    open_external_browser, prepare_native_profile_dir_from_paths,
    remove_owned_profile_locks_at_paths, stop_external_browser, validate_account_id,
    validate_browser_url,
};
use parking_lot::Mutex;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;

fn test_environment() -> BrowserEnvironment {
    BrowserEnvironment {
        program_files: Some(PathBuf::from("C:/Program Files")),
        program_files_x86: Some(PathBuf::from("C:/Program Files (x86)")),
        local_app_data: Some(PathBuf::from("C:/Users/test/AppData/Local")),
        home: Some(PathBuf::from("/Users/test")),
        path_entries: vec![PathBuf::from("/first/bin"), PathBuf::from("/second/bin")],
    }
}

#[cfg(windows)]
#[test]
fn failed_taskkill_uses_held_child_handle_without_touching_another_child() {
    use std::os::windows::process::{CommandExt, ExitStatusExt};
    struct OwnedProbe(std::process::Child);
    impl Drop for OwnedProbe {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let spawn = || {
        OwnedProbe(
            Command::new("ping.exe")
                .args(["-n", "30", "127.0.0.1"])
                .creation_flags(0x0800_0000)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        )
    };
    let mut owned = spawn();
    let mut unrelated = spawn();
    super::finish_windows_forced_exit(&mut owned.0, Ok(std::process::ExitStatus::from_raw(1)))
        .unwrap();
    assert!(super::wait_for_child_exit(&mut owned.0, std::time::Duration::from_secs(2)).unwrap());
    assert!(unrelated.0.try_wait().unwrap().is_none());
    // A second close is successful even when taskkill cannot find the exited process.
    super::finish_windows_forced_exit(
        &mut owned.0,
        Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "fixture executable absent",
        )),
    )
    .unwrap();
}

#[test]
fn account_id_rejects_path_traversal_and_unsafe_characters() {
    for invalid in ["", "../other", "a/b", "a\\b", ".", "hello world", "账号"] {
        assert_eq!(
            validate_account_id(invalid),
            Err("invalid account id".into())
        );
    }
    assert!(validate_account_id("8a16f15c-02ef-4320_a").is_ok());
    assert!(validate_account_id(&"a".repeat(129)).is_err());
}

#[test]
fn windows_prefers_edge_over_chrome() {
    let environment = test_environment();
    let chrome = PathBuf::from("C:/Program Files/Google/Chrome/Application/chrome.exe");
    let edge = PathBuf::from("C:/Users/test/AppData/Local/Microsoft/Edge/Application/msedge.exe");
    let found = discover_browser_with(BrowserPlatform::Windows, &environment, |path| {
        path == edge || path == chrome
    })
    .unwrap();
    assert_eq!(found.kind, BrowserKind::Edge);
    assert_eq!(found.path, edge);
}

#[test]
fn macos_prefers_chrome_then_edge_then_chromium() {
    let environment = test_environment();
    let edge = PathBuf::from("/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge");
    let chromium = PathBuf::from("/Applications/Chromium.app/Contents/MacOS/Chromium");
    let found = discover_browser_with(BrowserPlatform::Macos, &environment, |path| {
        path == edge || path == chromium
    })
    .unwrap();
    assert_eq!(found.kind, BrowserKind::Edge);
}

#[test]
fn empty_environment_discovers_no_browser() {
    let environment = BrowserEnvironment {
        program_files: None,
        program_files_x86: None,
        local_app_data: None,
        home: None,
        path_entries: Vec::new(),
    };
    assert!(discover_browser_with(BrowserPlatform::Windows, &environment, |_| true).is_none());
    assert!(discover_browser_with(BrowserPlatform::Linux, &environment, |_| true).is_none());
    assert!(discover_browser_with(BrowserPlatform::Macos, &environment, |_| false).is_none());
    let macos = discover_browser_with(BrowserPlatform::Macos, &environment, |_| true).unwrap();
    assert_eq!(macos.kind, BrowserKind::Chrome);
    assert_eq!(
        macos.path,
        PathBuf::from("/Applications/Google Chrome.app/Contents/MacOS/Google Chrome")
    );
}

#[test]
fn linux_uses_path_and_prefers_chrome_then_chromium_then_edge() {
    let environment = test_environment();
    let chromium = PathBuf::from("/first/bin/chromium");
    let edge = PathBuf::from("/first/bin/microsoft-edge");
    let found = discover_browser_with(BrowserPlatform::Linux, &environment, |path| {
        path == chromium || path == edge
    })
    .unwrap();
    assert_eq!(found.kind, BrowserKind::Chromium);
    assert_eq!(found.path, chromium);
}

#[test]
fn browser_arguments_use_only_profile_and_non_automation_flags() {
    let arguments = browser_arguments(
        Path::new("D:/data/browser-profiles/account-1"),
        "https://opencode.ai/auth",
    );
    let arguments = arguments
        .into_iter()
        .map(|value| value.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    assert_eq!(arguments.len(), 5);
    assert!(arguments[0].starts_with("--user-data-dir="));
    assert!(arguments.contains(&"--no-first-run".to_string()));
    assert!(arguments.contains(&"--no-default-browser-check".to_string()));
    assert!(arguments.contains(&"--new-window".to_string()));
    assert_eq!(arguments.last().unwrap(), "https://opencode.ai/auth");
    assert!(!arguments.iter().any(|argument| {
        argument.contains("remote-debugging")
            || argument.contains("automation")
            || argument == "--no-sandbox"
            || argument.contains("disable-web-security")
    }));
}

#[test]
fn browser_url_requires_https_without_credentials() {
    assert!(validate_browser_url("https://opencode.ai/zen/go").is_ok());
    assert_eq!(
        validate_browser_url("http://opencode.ai/zen/go"),
        Err("browser URL must be an absolute HTTPS URL".into())
    );
    assert_eq!(
        validate_browser_url("/relative"),
        Err("invalid browser URL".into())
    );
    assert_eq!(
        validate_browser_url("https://user:pass@opencode.ai/"),
        Err("browser URL must not include credentials".into())
    );
    assert_eq!(
        validate_browser_url("not a url"),
        Err("invalid browser URL".into())
    );
}

#[test]
fn host_open_rejects_invalid_account_id_and_urls_before_launch() {
    for (label, account_id, url, expected) in [
        (
            "invalid account id",
            "../other",
            "https://opencode.ai/auth",
            "invalid account id",
        ),
        (
            "non-https url",
            "account-1",
            "http://opencode.ai/auth",
            "browser URL must be an absolute HTTPS URL",
        ),
        (
            "credential url",
            "account-1",
            "https://user:pass@opencode.ai/",
            "browser URL must not include credentials",
        ),
    ] {
        let processes = Arc::new(Mutex::new(BrowserProcessState::default()));
        let error =
            open_external_browser(PathBuf::from("."), processes, account_id, url).unwrap_err();
        assert_eq!(error, expected, "{label}");
    }
}

#[test]
fn stop_missing_account_does_not_touch_another_owned_child() {
    let processes = Arc::new(Mutex::new(BrowserProcessState::default()));
    let mut command = if cfg!(windows) {
        Command::new("ping")
    } else {
        Command::new("sleep")
    };
    if cfg!(windows) {
        command.args(["-n", "30", "127.0.0.1"]);
    } else {
        command.arg("30");
    }
    let paused = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let pid = paused.id();
    processes
        .lock()
        .children
        .insert("other-account".into(), vec![paused]);
    assert!(stop_external_browser(&processes, "account-1", None).is_ok());
    assert!(crate::process_alive::process_is_running(pid));
    let mut child = processes
        .lock()
        .children
        .remove("other-account")
        .unwrap()
        .pop()
        .unwrap();
    child.kill().unwrap();
    let _ = child.wait();
}

#[test]
fn close_all_with_no_tracked_processes_succeeds() {
    let processes = Arc::new(Mutex::new(BrowserProcessState::default()));
    assert!(close_all_browser_processes(&processes, None).is_ok());
}

#[test]
fn finished_owned_child_is_forgotten_without_signaling_anyone_else() {
    let processes = Arc::new(Mutex::new(BrowserProcessState::default()));
    let mut command = if cfg!(windows) {
        Command::new("cmd")
    } else {
        Command::new("true")
    };
    if cfg!(windows) {
        command.args(["/C", "exit", "0"]);
    }
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    child.wait().unwrap();
    processes
        .lock()
        .children
        .insert("account-1".into(), vec![child]);
    stop_external_browser(&processes, "account-1", None).unwrap();
    assert!(processes.lock().children.is_empty());
}

#[test]
fn native_profile_open_and_lock_cleanup_use_resolved_external_paths() {
    let data_dir = std::env::temp_dir().join(format!(
        "ocg-native-browser-profile-data-test-{}",
        uuid::Uuid::new_v4()
    ));
    let external_root = std::env::temp_dir().join(format!(
        "ocg-native-browser-profile-external-test-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&data_dir).unwrap();
    std::fs::create_dir_all(&external_root).unwrap();
    let account_id = "account-1";
    let paths = ocg_core::browser::browser_profile_paths_with_override(
        &data_dir,
        account_id,
        Some(external_root.to_str().unwrap()),
    )
    .unwrap();
    assert_eq!(paths[0], external_root.join(account_id));
    assert_eq!(paths[1], data_dir.join("profiles").join(account_id));

    let opened_profile = prepare_native_profile_dir_from_paths(paths.clone()).unwrap();
    assert_eq!(opened_profile, external_root.join(account_id));
    std::fs::create_dir_all(&paths[1]).unwrap();
    for profile in &paths {
        std::fs::write(profile.join("SingletonLock"), b"owned").unwrap();
    }
    remove_owned_profile_locks_at_paths(paths.clone()).unwrap();
    assert!(
        paths
            .iter()
            .all(|profile| !profile.join("SingletonLock").exists())
    );

    std::fs::create_dir(paths[0].join("SingletonLock")).unwrap();
    let error = remove_owned_profile_locks_at_paths(paths.clone()).unwrap_err();
    assert!(error.contains("unexpectedly a directory"));
    assert!(paths[0].join("SingletonLock").is_dir());

    std::fs::remove_dir(paths[0].join("SingletonLock")).unwrap();
    delete_browser_profile_dirs_at_paths(paths.clone()).unwrap();
    assert!(paths.iter().all(|profile| !profile.exists()));
    std::fs::remove_dir_all(data_dir).unwrap();
    std::fs::remove_dir_all(external_root).unwrap();
}

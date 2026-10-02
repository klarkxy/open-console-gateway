use super::*;
use crate::runtime_log::Level;
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};

#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl Write for Capture {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn capture(filter: EnvFilter, emit: impl FnOnce()) -> String {
    let bytes = Capture::default();
    let writer = bytes.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .finish();
    tracing::subscriber::with_default(subscriber, emit);
    let output = bytes.0.lock().unwrap().clone();
    String::from_utf8(output).unwrap()
}

#[test]
fn defaults_and_invalid_filters_keep_dependency_noise_out() {
    for configured in [None, Some(""), Some("ocg=invalid-level")] {
        let (filter, invalid) = filter(configured);
        assert_eq!(invalid, configured == Some("ocg=invalid-level"));
        let output = capture(filter, || {
            tracing::debug!("internal-debug");
            tracing::info!("internal-info");
            tracing::info!(target: "dependency", "dependency-info");
            tracing::warn!(target: "dependency", "dependency-warning");
        });
        assert!(!output.contains("internal-debug"));
        assert!(output.contains("internal-info"));
        assert!(!output.contains("dependency-info"));
        assert!(output.contains("dependency-warning"));
    }
}

#[test]
fn explicit_filter_and_dynamic_severity_are_applied() {
    let (filter, invalid) = filter(Some("ocg_core=error"));
    assert!(!invalid);
    let output = capture(filter, || {
        event(Level::Warn, "background", "suppressed-warning");
        event(Level::Error, "background", "visible-error");
    });
    assert!(!output.contains("suppressed-warning"));
    assert!(output.contains("visible-error"));
    assert!(output.contains("background"));
}

#[test]
fn request_failure_output_is_correlated_without_upstream_content() {
    use crate::gateway::diagnostics::{
        ErrorDiagnostic, RequestTrace, emit_failure, serialize_diagnostic,
    };
    let trace = RequestTrace::new();
    let mut diagnostic = ErrorDiagnostic::new(
        &trace,
        2,
        "upstream",
        "headers",
        crate::kernel::protocol::ApiFormat::ChatCompletions,
    );
    diagnostic.upstream_status = Some(429);
    diagnostic.upstream_error = Some(serde_json::json!({"content": "private-upstream-message"}));
    let output = capture(EnvFilter::new("warn"), || {
        emit_failure(&serialize_diagnostic(diagnostic))
    });
    assert!(output.contains(&trace.request_id));
    assert!(output.contains("429"));
    assert!(!output.contains("private-upstream-message"));
}

#[test]
fn process_request_events_are_independent_of_dashboard_threshold() {
    let dir = std::env::temp_dir().join(format!("ocg-process-log-{}", uuid::Uuid::new_v4()));
    let mut db = crate::db::Database::open(dir.clone()).unwrap();
    db.log_level = Level::Error;
    let trace = crate::gateway::diagnostics::RequestTrace::new();
    let output = capture(EnvFilter::new("ocg_core=debug"), || {
        crate::gateway::diagnostics::log_event(
            &trace,
            "debug",
            "request",
            "prepared-test-event",
            Some(3),
            serde_json::json!({"bytes": 42}),
        );
    });
    assert!(output.contains("prepared-test-event"));
    assert!(output.contains(&trace.request_id));
    assert!(db.list_gateway_logs(20).unwrap().is_empty());
    drop(db);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn host_metadata_redacts_credentials() {
    let output = capture(EnvFilter::new("info"), || {
        event(
            Level::Info,
            "background",
            "authorization: Bearer sk-process-test-secret",
        );
    });
    assert!(!output.contains("sk-process-test-secret"));
}

#[test]
fn trace_request_shape_is_available_when_dashboard_logging_is_error_only() {
    use crate::crypto::StaticKeyCipher;
    let dir = std::env::temp_dir().join(format!("ocg-process-trace-{}", uuid::Uuid::new_v4()));
    let mut db = crate::db::Database::open(dir.clone()).unwrap();
    db.log_level = Level::Error;
    let state = Arc::new(
        crate::state::CoreStateInner::new(db, dir.clone(), Arc::new(StaticKeyCipher::new("test")))
            .unwrap(),
    );
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let trace = crate::gateway::diagnostics::RequestTrace::new();
    let output = capture(EnvFilter::new("ocg_core::gateway=trace"), || {
        runtime.block_on(crate::gateway::debug_capture::capture_client(
            &state,
            &trace,
            &axum::http::HeaderMap::new(),
            bytes::Bytes::from_static(
                br#"{"messages":[{"role":"user","content":"private-request-content"}]}"#,
            ),
        ));
    });
    assert!(output.contains("request_shape"));
    assert!(output.contains(&trace.request_id));
    assert!(!output.contains("private-request-content"));
    assert!(state.db.lock().list_gateway_logs(20).unwrap().is_empty());
    drop(state);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn repeated_initialization_and_closed_stderr_do_not_interrupt_work() {
    use std::process::{Command, Stdio};
    const CHILD_MARKER: &str = "OCG_TEST_PROCESS_LOG_CHILD";
    if std::env::var_os(CHILD_MARKER).is_some() {
        init();
        init();
        std::io::stdin().read_exact(&mut [0]).unwrap();
        crate::gateway::diagnostics::emit_failure("{}");
        event(Level::Warn, "background", "diagnostic after pipe closure");
        return;
    }
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "process_log::tests::repeated_initialization_and_closed_stderr_do_not_interrupt_work",
            "--nocapture",
        ])
        .env(CHILD_MARKER, "1")
        .env("RUST_LOG", "warn")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    drop(child.stderr.take());
    child.stdin.take().unwrap().write_all(&[1]).unwrap();
    assert!(child.wait().unwrap().success());
}

struct TempDir(std::path::PathBuf);

impl TempDir {
    fn new(label: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("ocg-program-log-{label}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}

impl std::ops::Deref for TempDir {
    type Target = std::path::Path;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl AsRef<std::path::Path> for TempDir {
    fn as_ref(&self) -> &std::path::Path {
        &self.0
    }
}

impl AsRef<std::ffi::OsStr> for TempDir {
    fn as_ref(&self) -> &std::ffi::OsStr {
        self.0.as_os_str()
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let logs = self.0.join("logs");
        // Unlink a symlink or junction before the recursive delete so a
        // reparse point cannot redirect removal into another directory.
        #[cfg(unix)]
        let _ = std::fs::remove_file(&logs);
        #[cfg(windows)]
        let _ = std::fs::remove_dir(&logs);
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn program_log(dir: &std::path::Path) -> std::path::PathBuf {
    dir.join("logs").join("program.log")
}

fn spawn_log_child(test: &str, mode: &str, dir: &std::path::Path) -> std::process::Child {
    std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", test, "--nocapture"])
        .env("OCG_TEST_PROGRAM_LOG_CHILD", mode)
        .env("OCG_TEST_PROGRAM_LOG_DIR", dir)
        .env("RUST_LOG", "warn")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap()
}

fn child_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(std::env::var("OCG_TEST_PROGRAM_LOG_DIR").unwrap())
}

fn run_program_log_child() -> bool {
    let Some(mode) = std::env::var("OCG_TEST_PROGRAM_LOG_CHILD").ok() else {
        return false;
    };
    let dir = child_dir();
    match mode.as_str() {
        "closed-stderr" => {
            init();
            let _ = std::io::stdin().read_exact(&mut [0]);
            activate_file_sink(&dir);
            tracing::warn!("diagnostic-after-closed-stderr");
        }
        "late" => {
            init();
            tracing::warn!("before-file-sink");
            activate_file_sink(&dir);
            tracing::warn!("after-file-sink");
        }
        "off" => {
            init();
            activate_file_sink(&dir);
            tracing::warn!("file-sink-disabled-by-env");
        }
        "embedding" => {
            assert!(
                tracing_subscriber::fmt()
                    .with_writer(std::io::sink)
                    .with_ansi(false)
                    .try_init()
                    .is_ok()
            );
            init();
            activate_file_sink(&dir);
            tracing::warn!("embedding-subscriber-event");
        }
        "contended" => {
            init();
            activate_file_sink(&dir);
            tracing::warn!("diagnostic-after-lock-contention");
        }
        "file-error" => {
            init();
            activate_file_sink(&dir);
            tracing::warn!("diagnostic-after-file-error");
        }
        "secrets" => {
            init();
            activate_file_sink(&dir);
            tracing::warn!(
                "https://user:opaque-secret@example.test/v1 body=\"raw-body-payload-secret\" \u{1b}[31mcolored\u{1b}[0m"
            );
            tracing::warn!(api_key = "opaque-secret", "credential-field");
            tracing::warn!(body = %"private request text\nprivate second line", "display body");
        }
        other => panic!("unknown program log child mode {other}"),
    }
    true
}

#[test]
fn file_sink_rotates_before_overflow_and_keeps_five_bounded_files() {
    let dir = TempDir::new("bounds");
    let mut sink = FileSink::acquire(&dir).unwrap();
    let max = usize::try_from(MAX_PROGRAM_LOG_BYTES).unwrap();
    let six_mib = 6 * 1024 * 1024;
    sink.commit(&vec![b'q'; six_mib]);
    sink.commit(&vec![b'r'; six_mib]);
    let logs = dir.join("logs");
    let active = std::fs::read(logs.join("program.log")).unwrap();
    let previous = std::fs::read(logs.join("program.log.1")).unwrap();
    assert!(active.len() <= max);
    assert!(previous.len() <= max);
    assert!(active.iter().all(|byte| *byte == b'r'));
    assert!(previous.iter().all(|byte| *byte == b'q'));
    assert!(!logs.join("program.log.2").exists());

    for index in 0..6u8 {
        sink.commit(&vec![b'A' + index; max]);
    }
    drop(sink);
    for name in [
        "program.log",
        "program.log.1",
        "program.log.2",
        "program.log.3",
        "program.log.4",
    ] {
        let bytes = std::fs::read(logs.join(name)).unwrap();
        assert!(bytes.len() <= max, "{name} is {} bytes", bytes.len());
        assert_eq!(bytes.len(), max, "{name}");
        assert!(bytes.first() != Some(&b'A'), "{name} kept the oldest event");
    }
    assert!(!logs.join("program.log.5").exists());
    assert!(logs.join(".program-log.lock").is_file());
}

#[test]
fn oversized_event_is_capped_with_a_visible_marker_and_redacted() {
    let dir = TempDir::new("oversize");
    let mut sink = FileSink::acquire(&dir).unwrap();
    let max = usize::try_from(MAX_PROGRAM_LOG_BYTES).unwrap();
    let mut event = b"authorization: Bearer sk-process-test-secret\n".to_vec();
    event.extend(std::iter::repeat_n(b'b', max));
    event.extend(b"SECRET-TAIL-MUST-NOT-FIT");
    sink.commit(&sanitize_event(&event));
    drop(sink);
    let bytes = std::fs::read(program_log(&dir)).unwrap();
    let text = String::from_utf8_lossy(&bytes);
    assert!(bytes.len() <= max);
    assert!(text.contains("[truncated]"));
    assert!(!text.contains("sk-process-test-secret"));
    assert!(!text.contains("SECRET-TAIL-MUST-NOT-FIT"));
}

#[test]
fn restart_appends_without_removing_the_lock_file() {
    let dir = TempDir::new("restart");
    let mut sink = FileSink::acquire(&dir).unwrap();
    sink.commit(b"first-generation\n");
    drop(sink);
    let mut sink = FileSink::acquire(&dir).unwrap();
    sink.commit(b"second-generation\n");
    drop(sink);
    let text = std::fs::read_to_string(program_log(&dir)).unwrap();
    assert!(text.contains("first-generation"));
    assert!(text.contains("second-generation"));
    assert!(dir.join("logs").join(".program-log.lock").is_file());
}

#[test]
fn rotation_failure_disables_the_sink_without_panicking() {
    let dir = TempDir::new("rotate-error");
    let mut sink = FileSink::acquire(&dir).unwrap();
    sink.commit(b"kept-before-rotation-failure\n");
    std::fs::create_dir(dir.join("logs").join("program.log.4")).unwrap();
    let max = usize::try_from(MAX_PROGRAM_LOG_BYTES).unwrap();
    sink.commit(&vec![b'z'; max]);
    assert!(sink.disabled);
    sink.commit(b"must-not-append-after-disable\n");
    drop(sink);
    let text = std::fs::read_to_string(program_log(&dir)).unwrap();
    assert!(text.contains("kept-before-rotation-failure"));
    assert!(!text.contains("must-not-append-after-disable"));
    assert!(!text.contains("[truncated]"));
}

#[test]
fn symlink_or_non_file_destinations_are_refused() {
    let dir = TempDir::new("refuse");
    let logs = dir.join("logs");
    std::fs::write(&logs, b"not-a-directory").unwrap();
    assert!(FileSink::acquire(&dir).is_err());
    assert_eq!(std::fs::read(&logs).unwrap(), b"not-a-directory");

    let real = dir.join("real-logs");
    std::fs::create_dir(&real).unwrap();
    std::fs::remove_file(&logs).unwrap();
    assert!(
        make_directory_reparse(&logs, &real),
        "could not create a symlink or junction for the program log directory"
    );
    assert!(FileSink::acquire(&dir).is_err());
    assert!(!real.join("program.log").exists());
    assert!(!real.join(".program-log.lock").exists());
}

fn make_directory_reparse(link: &std::path::Path, target: &std::path::Path) -> bool {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, link).is_ok()
    }
    #[cfg(windows)]
    {
        if std::os::windows::fs::symlink_dir(target, link).is_ok() {
            return true;
        }
        use std::os::windows::process::CommandExt;
        let mut command = std::process::Command::new("cmd");
        command
            .raw_arg(format!(
                "/C mklink /J \"{}\" \"{}\"",
                link.display(),
                target.display()
            ))
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        command.status().is_ok_and(|status| status.success())
    }
}

#[test]
fn poisoned_lock_still_accepts_a_later_event() {
    struct ClearOnDrop;
    impl Drop for ClearOnDrop {
        fn drop(&mut self) {
            sink_lock().take();
        }
    }
    let dir = TempDir::new("poison");
    let _clear = ClearOnDrop;
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _guard = sink_lock();
        panic!("poison program log lock");
    }));
    *sink_lock() = Some(FileSink::acquire(&dir).unwrap());
    commit_formatted(b"after-poison\n");
    let text = std::fs::read_to_string(program_log(&dir)).unwrap();
    assert!(text.contains("after-poison"));
}

#[test]
fn late_activation_records_only_later_events() {
    if run_program_log_child() {
        return;
    }
    let dir = TempDir::new("late");
    let output = spawn_log_child(
        "process_log::tests::late_activation_records_only_later_events",
        "late",
        &dir,
    )
    .wait_with_output()
    .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = std::fs::read_to_string(program_log(&dir)).unwrap();
    assert!(text.contains("after-file-sink"));
    assert!(!text.contains("before-file-sink"));
}

#[test]
fn closed_stderr_still_writes_the_file_and_exits() {
    if run_program_log_child() {
        return;
    }
    let dir = TempDir::new("closed-stderr");
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "process_log::tests::closed_stderr_still_writes_the_file_and_exits",
            "--nocapture",
        ])
        .env("OCG_TEST_PROGRAM_LOG_CHILD", "closed-stderr")
        .env("OCG_TEST_PROGRAM_LOG_DIR", &dir)
        .env("RUST_LOG", "warn")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    drop(child.stderr.take());
    child.stdin.take().unwrap().write_all(&[1]).unwrap();
    assert!(child.wait().unwrap().success());
    let text = std::fs::read_to_string(program_log(&dir)).unwrap();
    assert!(text.contains("diagnostic-after-closed-stderr"));
}

#[test]
fn explicit_off_and_embedding_subscriber_do_not_open_a_file() {
    if run_program_log_child() {
        return;
    }
    let off_dir = TempDir::new("off");
    let off = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "process_log::tests::explicit_off_and_embedding_subscriber_do_not_open_a_file",
            "--nocapture",
        ])
        .env("OCG_TEST_PROGRAM_LOG_CHILD", "off")
        .env("OCG_TEST_PROGRAM_LOG_DIR", &off_dir)
        .env("OCG_PROGRAM_LOG_FILE", "off")
        .env("RUST_LOG", "warn")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(off.success());
    assert!(!program_log(&off_dir).exists());

    let embedded = TempDir::new("embedding");
    let embedded_status = spawn_log_child(
        "process_log::tests::explicit_off_and_embedding_subscriber_do_not_open_a_file",
        "embedding",
        &embedded,
    )
    .wait()
    .unwrap();
    assert!(embedded_status.success());
    assert!(!program_log(&embedded).exists());
}

#[test]
fn contended_lock_and_file_errors_leave_stderr_work_running() {
    if run_program_log_child() {
        return;
    }
    let dir = TempDir::new("contention");
    let sink = FileSink::acquire(&dir).unwrap();
    let child = spawn_log_child(
        "process_log::tests::contended_lock_and_file_errors_leave_stderr_work_running",
        "contended",
        &dir,
    )
    .wait_with_output()
    .unwrap();
    drop(sink);
    assert!(child.status.success());
    let stderr = String::from_utf8_lossy(&child.stderr);
    assert!(stderr.contains("diagnostic-after-lock-contention"));
    assert!(stderr.contains("program log file disabled"));
    let text = std::fs::read_to_string(program_log(&dir)).unwrap_or_default();
    assert!(!text.contains("diagnostic-after-lock-contention"));

    let broken = TempDir::new("file-error");
    std::fs::write(broken.join("logs"), b"blocked").unwrap();
    let child = spawn_log_child(
        "process_log::tests::contended_lock_and_file_errors_leave_stderr_work_running",
        "file-error",
        &broken,
    )
    .wait_with_output()
    .unwrap();
    assert!(child.status.success());
    let stderr = String::from_utf8_lossy(&child.stderr);
    assert!(stderr.contains("diagnostic-after-file-error"));
    assert!(stderr.contains("program log file disabled"));
    assert_eq!(std::fs::read(broken.join("logs")).unwrap(), b"blocked");
}

#[test]
fn real_subscriber_sanitizes_stderr_and_file_identically() {
    if run_program_log_child() {
        return;
    }
    let dir = TempDir::new("sanitized-sinks");
    let child = spawn_log_child(
        "process_log::tests::real_subscriber_sanitizes_stderr_and_file_identically",
        "secrets",
        &dir,
    )
    .wait_with_output()
    .unwrap();
    assert!(child.status.success());
    let stderr = String::from_utf8(child.stderr).unwrap();
    let file = std::fs::read_to_string(program_log(&dir)).unwrap();
    for output in [&stderr, &file] {
        assert!(!output.contains("opaque-secret"));
        assert!(!output.contains("raw-body-payload-secret"));
        assert!(!output.contains("private request"));
        assert!(!output.contains("private second line"));
        assert!(!output.contains('\u{1b}'));
        assert!(output.contains("credential-field"));
        assert!(output.contains("example.test/v1"));
    }
    assert_eq!(stderr, file);
}

#[test]
fn preexisting_oversized_family_is_refused_without_changing_content() {
    for filename in ["program.log", "program.log.2"] {
        let dir = TempDir::new("preexisting-oversize");
        let logs = dir.join("logs");
        std::fs::create_dir(&logs).unwrap();
        let file = std::fs::File::create(logs.join(filename)).unwrap();
        file.set_len(MAX_PROGRAM_LOG_BYTES + 1).unwrap();
        drop(file);
        assert!(FileSink::acquire(&dir).is_err());
        assert_eq!(
            std::fs::metadata(logs.join(filename)).unwrap().len(),
            MAX_PROGRAM_LOG_BYTES + 1
        );
        assert_eq!(std::fs::read_dir(&logs).unwrap().count(), 1);
    }
}

#[test]
fn linked_data_directory_and_linked_parent_do_not_touch_the_destination() {
    let dir = TempDir::new("linked-ancestors");
    let target = TempDir::new("linked-target");
    let link = dir.join("linked");
    if !make_directory_reparse(&link, &target) {
        return;
    }
    assert!(FileSink::acquire(&link).is_err());
    assert!(FileSink::acquire(&link.join("child")).is_err());
    assert_eq!(std::fs::read_dir(&*target).unwrap().count(), 0);
    #[cfg(windows)]
    std::fs::remove_dir(link).unwrap();
    #[cfg(unix)]
    std::fs::remove_file(link).unwrap();
}

#[test]
fn existing_family_permissions_are_private_after_activation() {
    let dir = TempDir::new("private-family");
    let logs = dir.join("logs");
    std::fs::create_dir(&logs).unwrap();
    std::fs::write(logs.join("program.log"), b"preserved\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&logs, std::fs::Permissions::from_mode(0o777)).unwrap();
        std::fs::set_permissions(
            logs.join("program.log"),
            std::fs::Permissions::from_mode(0o666),
        )
        .unwrap();
    }
    let sink = FileSink::acquire(&dir).unwrap();
    for path in family_paths(&logs).into_iter().filter(|path| path.exists()) {
        crate::fs_privacy::permissions_are_private(&path).unwrap();
    }
    crate::fs_privacy::permissions_are_private(&logs).unwrap();
    drop(sink);
    assert_eq!(
        std::fs::read(logs.join("program.log")).unwrap(),
        b"preserved\n"
    );
}

#[test]
fn formatting_buffer_is_bounded_before_sanitization() {
    let mut writer = TeeWriter::new();
    writer
        .write_all(&vec![b'x'; max_event_bytes() + 64])
        .unwrap();
    assert!(writer.buffer.len() <= max_event_bytes());
    assert!(writer.buffer.ends_with(TRUNCATION_MARKER));
    assert!(writer.truncated);
    writer.write_all(b"must-not-accumulate").unwrap();
    assert!(writer.buffer.len() <= max_event_bytes());
    // This test inspects accumulation; don't print the synthetic giant event.
    writer.buffer.clear();
}

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn temp_root(label: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("ocg-backup-smoke-{label}-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&dir).unwrap();
    dir
}

#[test]
fn executable_backup_smoke_redacts_stdout_and_stderr() {
    let root = temp_root("exe");
    let source = root.join("source");
    let target = root.join("target");
    let output = root.join("snapshot.tar.gz");
    fs::create_dir_all(source.join("notes")).unwrap();
    fs::write(source.join("notes/plain.txt"), b"synthetic-oauth-token").unwrap();
    fs::write(source.join(".encryption-key"), b"synthetic-stale-file-key").unwrap();
    let exe = env!("CARGO_BIN_EXE_ocg");
    let explicit = "synthetic-cli-explicit-key";
    let wrong = "synthetic-cli-wrong-key";

    let created = Command::new(exe)
        .env_remove("OCG_MANAGER_ENCRYPTION_KEY")
        .args([
            "--data-dir",
            source.to_str().unwrap(),
            "--encryption-key",
            explicit,
            "backup",
            "create",
            "--output",
            output.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&created.stdout);
    let stderr = String::from_utf8_lossy(&created.stderr);
    assert!(created.status.success(), "stdout={stdout} stderr={stderr}");
    assert!(stdout.contains("\"state\":\"created\""), "{stdout}");
    assert!(!stdout.contains(explicit), "{stdout}");
    assert!(!stdout.contains(wrong), "{stdout}");
    assert!(!stdout.contains("synthetic-stale-file-key"), "{stdout}");
    assert!(!stdout.contains("synthetic-oauth-token"), "{stdout}");
    assert!(!stderr.contains(explicit), "{stderr}");
    assert!(!stderr.contains("synthetic-stale-file-key"), "{stderr}");

    let rejected = Command::new(exe)
        .env_remove("OCG_MANAGER_ENCRYPTION_KEY")
        .args([
            "--data-dir",
            target.to_str().unwrap(),
            "--encryption-key",
            wrong,
            "backup",
            "restore",
            "--input",
            output.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&rejected.stdout);
    let stderr = String::from_utf8_lossy(&rejected.stderr);
    assert!(!rejected.status.success(), "{stdout}");
    assert!(
        !stdout.contains(explicit) && !stdout.contains(wrong),
        "{stdout}"
    );
    assert!(!stderr.contains(explicit), "{stderr}");
    assert!(!stderr.contains(wrong), "{stderr}");
    assert!(!stderr.contains("synthetic-stale-file-key"), "{stderr}");
    assert!(!stderr.contains("synthetic-oauth-token"), "{stderr}");
    assert!(stderr.contains("witness"), "{stderr}");
    assert!(!target.exists());

    let restored = Command::new(exe)
        .env_remove("OCG_MANAGER_ENCRYPTION_KEY")
        .args([
            "--data-dir",
            target.to_str().unwrap(),
            "--encryption-key",
            explicit,
            "backup",
            "restore",
            "--input",
            output.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&restored.stdout);
    let stderr = String::from_utf8_lossy(&restored.stderr);
    assert!(restored.status.success(), "stdout={stdout} stderr={stderr}");
    assert!(stdout.contains("\"state\":\"restored\""), "{stdout}");
    assert!(!stdout.contains(explicit), "{stdout}");
    assert!(!stderr.contains(explicit), "{stderr}");
    assert_eq!(
        fs::read(target.join("notes/plain.txt")).unwrap(),
        b"synthetic-oauth-token"
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn executable_ocg_leaves_previous_generation_data_in_place() {
    let root = temp_root("generation");
    let home = root.join("home");
    let previous = home.join(".ocg-mgr-cli");
    let explicit = root.join("explicit-data");
    fs::create_dir_all(previous.join("cpa/auth")).unwrap();
    let marker = previous.join("cpa/auth/token.json");
    let marker_bytes = b"previous-generation-sentinel";
    fs::write(&marker, marker_bytes).unwrap();
    let exe = env!("CARGO_BIN_EXE_ocg");

    let help = Command::new(exe)
        .args(["--help"])
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .env("NO_COLOR", "1")
        .env_remove("CLICOLOR_FORCE")
        .env_remove("FORCE_COLOR")
        .env_remove("OCG_MANAGER_ENCRYPTION_KEY")
        .output()
        .unwrap();
    let help_text = format!(
        "{}{}",
        String::from_utf8_lossy(&help.stdout),
        String::from_utf8_lossy(&help.stderr)
    );
    assert!(help.status.success(), "{help_text}");
    assert!(
        help_text.contains("Usage: ocg ") || help_text.contains("Usage: ocg\n"),
        "{help_text}"
    );
    assert!(help_text.contains("~/.ocg3"), "{help_text}");
    assert!(!help_text.contains("ocg-manager-cli"), "{help_text}");
    assert!(!home.join(".ocg3").exists());
    assert_eq!(fs::read(&marker).unwrap(), marker_bytes);

    let schema = Command::new(exe)
        .args(["schema", "v4"])
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .env_remove("OCG_MANAGER_ENCRYPTION_KEY")
        .output()
        .unwrap();
    let schema_out = String::from_utf8_lossy(&schema.stdout);
    let schema_err = String::from_utf8_lossy(&schema.stderr);
    assert!(schema.status.success(), "{schema_out} {schema_err}");
    assert!(schema_out.contains("DashboardApiV4"), "{schema_out}");
    assert!(!home.join(".ocg3").exists());
    assert_eq!(fs::read(&marker).unwrap(), marker_bytes);

    let explicit_status = Command::new(exe)
        .args(["--data-dir", explicit.to_str().unwrap(), "status"])
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .env("OCG_MANAGER_ENCRYPTION_KEY", "synthetic-generation-cipher")
        .output()
        .unwrap();
    let explicit_out = String::from_utf8_lossy(&explicit_status.stdout);
    let explicit_err = String::from_utf8_lossy(&explicit_status.stderr);
    assert!(
        explicit_status.status.success(),
        "{explicit_out} {explicit_err}"
    );
    assert!(explicit_out.contains("explicit-data"), "{explicit_out}");
    assert!(!explicit_out.contains(".ocg3"), "{explicit_out}");
    assert!(!explicit_out.contains(".ocg-mgr-cli"), "{explicit_out}");
    assert!(explicit.join("data.sqlite").is_file());
    assert!(!home.join(".ocg3").exists());
    assert!(!previous.join("data.sqlite").exists());
    assert_eq!(fs::read(&marker).unwrap(), marker_bytes);

    let status = Command::new(exe)
        .args(["status"])
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .env("OCG_MANAGER_ENCRYPTION_KEY", "synthetic-generation-cipher")
        .output()
        .unwrap();
    let status_out = String::from_utf8_lossy(&status.stdout);
    let status_err = String::from_utf8_lossy(&status.stderr);
    assert!(status.status.success(), "{status_out} {status_err}");
    assert!(status_out.contains(".ocg3"), "{status_out}");
    assert!(!status_out.contains(".ocg-mgr-cli"), "{status_out}");
    assert!(home.join(".ocg3").join("data.sqlite").is_file());
    assert!(!previous.join("data.sqlite").exists());
    assert_eq!(fs::read(&marker).unwrap(), marker_bytes);
    assert!(explicit.join("data.sqlite").is_file());
    let _ = fs::remove_dir_all(root);
}

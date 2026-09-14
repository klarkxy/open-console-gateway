use super::*;
use crate::crypto::{KeyCipher, StaticKeyCipher};
use crate::db::{CpaCatalogModel, Database};
use sha2::{Digest, Sha256};
use std::io::{Cursor, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use zip::ZipWriter;
use zip::write::SimpleFileOptions;

fn temp_dir(label: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("ocg-cpa-runtime-{label}-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_zip(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut cursor = Cursor::new(Vec::new());
    {
        let mut zip = ZipWriter::new(&mut cursor);
        for (name, bytes) in files {
            zip.start_file(*name, SimpleFileOptions::default()).unwrap();
            zip.write_all(bytes).unwrap();
        }
        zip.finish().unwrap();
    }
    cursor.into_inner()
}

fn write_tar_gz(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut tar_bytes = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut tar_bytes);
        for (name, bytes) in files {
            let mut header = tar::Header::new_gnu();
            header.set_size(bytes.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            builder.append_data(&mut header, *name, *bytes).unwrap();
        }
        builder.finish().unwrap();
    }
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(&tar_bytes).unwrap();
    encoder.finish().unwrap()
}

fn write_tar_gz_symlink(name: &str, target: &str) -> Vec<u8> {
    let mut tar_bytes = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut tar_bytes);
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Symlink);
        header.set_size(0);
        header.set_cksum();
        builder.append_link(&mut header, name, target).unwrap();
        builder.finish().unwrap();
    }
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(&tar_bytes).unwrap();
    encoder.finish().unwrap()
}

#[test]
fn windows_release_version_rejects_path_unsafe_names() {
    assert!(normalize_release_version("v7.2.147").is_ok());
    assert!(normalize_release_version("../7.2").is_err());
    for unsafe_version in [".", "..", "CON", "7.2.", "7..2", "v"] {
        assert!(
            normalize_release_version(unsafe_version).is_err(),
            "{unsafe_version}"
        );
    }
}

#[test]
fn shipped_desktop_assets_use_official_cli_proxy_api_names() {
    assert_eq!(
        CpaReleaseAsset::WindowsAmd64Zip.file_name("7.2.147"),
        "CLIProxyAPI_7.2.147_windows_amd64.zip"
    );
    assert_eq!(
        CpaReleaseAsset::DarwinAmd64TarGz.file_name("7.2.147"),
        "CLIProxyAPI_7.2.147_darwin_amd64.tar.gz"
    );
    assert_eq!(
        CpaReleaseAsset::DarwinAarch64TarGz.file_name("7.2.147"),
        "CLIProxyAPI_7.2.147_darwin_aarch64.tar.gz"
    );
    assert_eq!(
        CpaReleaseAsset::LinuxAmd64TarGz.file_name("7.2.147"),
        "CLIProxyAPI_7.2.147_linux_amd64.tar.gz"
    );
    assert_eq!(
        CpaReleaseAsset::WindowsAmd64Zip.archive_kind(),
        extract::CpaArchiveKind::Zip
    );
    assert_eq!(
        CpaReleaseAsset::DarwinAmd64TarGz.archive_kind(),
        extract::CpaArchiveKind::TarGz
    );
    assert_eq!(
        CpaReleaseAsset::DarwinAarch64TarGz.archive_kind(),
        extract::CpaArchiveKind::TarGz
    );
    assert_eq!(
        CpaReleaseAsset::LinuxAmd64TarGz.archive_kind(),
        extract::CpaArchiveKind::TarGz
    );
    #[cfg(all(windows, target_arch = "x86_64"))]
    assert_eq!(
        current_cpa_release_asset(),
        Some(CpaReleaseAsset::WindowsAmd64Zip)
    );
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    assert_eq!(
        current_cpa_release_asset(),
        Some(CpaReleaseAsset::DarwinAmd64TarGz)
    );
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    assert_eq!(
        current_cpa_release_asset(),
        Some(CpaReleaseAsset::DarwinAarch64TarGz)
    );
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    assert_eq!(
        current_cpa_release_asset(),
        Some(CpaReleaseAsset::LinuxAmd64TarGz)
    );
}

#[test]
fn checksums_txt_matches_exact_filename() {
    let text = "aabbccddeeff00112233445566778899aabbccddeeff00112233445566778899  CLIProxyAPI_7.2.147_windows_amd64.zip\n";
    assert_eq!(
        parse_checksum(text, "CLIProxyAPI_7.2.147_windows_amd64.zip").unwrap(),
        "aabbccddeeff00112233445566778899aabbccddeeff00112233445566778899"
    );
    assert!(parse_checksum(text, "CLIProxyAPI_7.2.147_windows_aarch64.zip").is_err());
}

#[test]
fn managed_json_roundtrip_omits_pid_and_secrets() {
    let dir = temp_dir("managed");
    save_managed(
        &dir,
        &ManagedCpa {
            current_version: "7.2.147".into(),
            previous_version: Some("7.2.140".into()),
            asset_sha256: "a".repeat(64),
            port: 8317,
        },
    )
    .unwrap();
    let encoded = fs::read_to_string(managed_path(&dir)).unwrap();
    assert!(!encoded.contains("pid"));
    assert!(!encoded.contains("secret"));
    assert!(!encoded.contains("key"));
    let loaded = load_managed(&dir).unwrap().unwrap();
    assert_eq!(loaded.port, 8317);
    assert_eq!(loaded.current_version, "7.2.147");
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn extract_rejects_traversal_duplicates_and_symlinks() {
    let dir = temp_dir("extract");
    let zip_path = dir.join("ok.zip");
    fs::write(&zip_path, write_zip(&[("cli-proxy-api.exe", b"mz")])).unwrap();
    extract::extract_zip(&zip_path, &dir.join("ok")).unwrap();
    assert!(dir.join("ok/cli-proxy-api.exe").is_file());

    let traversal = dir.join("trav.zip");
    fs::write(
        &traversal,
        write_zip(&[("../evil.exe", b"mz"), ("cli-proxy-api.exe", b"mz")]),
    )
    .unwrap();
    assert!(extract::extract_zip(&traversal, &dir.join("trav")).is_err());
    assert!(!dir.join("evil.exe").exists());

    let dup = dir.join("dup.zip");
    fs::write(
        &dup,
        write_zip(&[("cli-proxy-api.exe", b"a"), ("./cli-proxy-api.exe", b"b")]),
    )
    .unwrap();
    assert!(extract::extract_zip(&dup, &dir.join("dup")).is_err());

    let case_dup = dir.join("case-dup.zip");
    fs::write(
        &case_dup,
        write_zip(&[("CPA.exe", b"a"), ("cpa.EXE", b"b")]),
    )
    .unwrap();
    assert!(extract::extract_zip(&case_dup, &dir.join("case-dup")).is_err());

    for (label, name) in [
        ("ads", "cli-proxy-api.exe:stream"),
        ("reserved", "CON.txt"),
        ("trailing", "folder. /cli-proxy-api.exe"),
    ] {
        let path = dir.join(format!("{label}.zip"));
        fs::write(&path, write_zip(&[(name, b"mz")])).unwrap();
        assert!(
            extract::extract_zip(&path, &dir.join(label)).is_err(),
            "{name}"
        );
    }

    assert!(extract::is_unix_symlink(Some(0o120_777)));
    assert!(!extract::is_unix_symlink(Some(0o100_644)));
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn tar_gz_extract_rejects_unsafe_duplicates_and_symlinks() {
    let dir = temp_dir("extract-tar");
    let archive = dir.join("ok.tar.gz");
    fs::write(&archive, write_tar_gz(&[("CLIProxyAPI", b"elf")])).unwrap();
    extract::extract_tar_gz(&archive, &dir.join("ok")).unwrap();
    assert!(dir.join("ok/CLIProxyAPI").is_file());
    assert_eq!(
        find_managed_executable(&dir.join("ok")).unwrap(),
        dir.join("ok/CLIProxyAPI")
    );

    let reserved = dir.join("reserved.tar.gz");
    fs::write(&reserved, write_tar_gz(&[("CON.txt", b"elf")])).unwrap();
    assert!(extract::extract_tar_gz(&reserved, &dir.join("reserved")).is_err());

    let ads = dir.join("ads.tar.gz");
    fs::write(&ads, write_tar_gz(&[("CLIProxyAPI:stream", b"elf")])).unwrap();
    assert!(extract::extract_tar_gz(&ads, &dir.join("ads")).is_err());

    let dup = dir.join("dup.tar.gz");
    fs::write(
        &dup,
        write_tar_gz(&[("CLIProxyAPI", b"a"), ("./CLIProxyAPI", b"b")]),
    )
    .unwrap();
    assert!(extract::extract_tar_gz(&dup, &dir.join("dup")).is_err());

    let symlink = dir.join("link.tar.gz");
    fs::write(&symlink, write_tar_gz_symlink("CLIProxyAPI", "../evil")).unwrap();
    assert!(extract::extract_tar_gz(&symlink, &dir.join("link")).is_err());
    assert!(!dir.join("evil").exists());

    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn snapshot_without_host_stays_unsupported_and_names_shipped_desktops() {
    let dir = temp_dir("unsupported-host");
    let cipher: Arc<dyn KeyCipher + Send + Sync> = Arc::new(StaticKeyCipher::new("cpa-runtime"));
    let state =
        CoreStateInner::new(Database::open(dir.clone()).unwrap(), dir.clone(), cipher).unwrap();
    let snapshot = state.cpa_runtime_snapshot();
    assert!(!snapshot.supported);
    assert_eq!(
        snapshot.unavailable_reason.as_deref(),
        Some(UNAVAILABLE_REASON)
    );
    drop(state);
    fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn install_without_host_fails_closed_before_download() {
    let dir = temp_dir("install-no-host");
    let cipher: Arc<dyn KeyCipher + Send + Sync> = Arc::new(StaticKeyCipher::new("cpa-runtime"));
    let state =
        CoreStateInner::new(Database::open(dir.clone()).unwrap(), dir.clone(), cipher).unwrap();
    let error = state
        .install_cpa_runtime(state.settings_revision(), state.process_generation(), None)
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        CpaRuntimeError::Unavailable(message) if message == UNAVAILABLE_REASON
    ));
    drop(state);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn fingerprints_are_stable_and_hints_are_redacted() {
    let secret = "cpa-super-secret-key";
    assert_eq!(
        fingerprint_key(secret),
        format!("{:x}", Sha256::digest(secret.as_bytes()))
    );
    let hint = key_hint(secret);
    assert!(hint.starts_with("••••"));
    assert!(hint.ends_with("key"));
    assert!(!hint.contains("super-secret"));
}

#[test]
fn log_tail_is_bounded() {
    let mut buffer = String::new();
    append_log_tail(&mut buffer, "one\n", 8);
    append_log_tail(&mut buffer, "two\nthree\n", 8);
    assert!(buffer.len() <= 8);
    assert!(!buffer.contains("one"));
}

#[test]
fn config_yaml_is_loopback_only_and_lists_protected_key() {
    let dir = temp_dir("config");
    write_config_yaml(
        &dir.join("config.yaml"),
        8319,
        &dir.join("auth"),
        "infer",
        &["extra".into()],
    )
    .unwrap();
    let text = fs::read_to_string(dir.join("config.yaml")).unwrap();
    assert!(text.contains("host: \"127.0.0.1\""));
    assert!(text.contains("port: 8319"));
    assert!(text.contains("secret-key: \"\""));
    assert!(!text.contains("mgmt"));
    assert!(text.contains("infer"));
    assert!(text.contains("extra"));
    assert_eq!(parse_api_keys_from_yaml(&text).unwrap(), ["infer", "extra"]);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn managed_config_parser_rejects_missing_malformed_and_duplicate_keys() {
    assert!(parse_api_keys_from_yaml("host: 127.0.0.1\n").is_err());
    assert!(parse_api_keys_from_yaml("api-keys:\n  - unquoted\n").is_err());
    assert!(parse_api_keys_from_yaml("api-keys:\n  unexpected: value\n").is_err());
    assert!(parse_api_keys_from_yaml("api-keys:\n  - \"same\"\n  - \"same\"\n").is_err());
    assert!(parse_api_keys_from_yaml("api-keys:\n  - \"bad\\nkey\"\n").is_err());
}

#[test]
fn update_secrets_require_readable_valid_config_and_preserve_extras() {
    let dir = temp_dir("strict-update-secrets");
    let cipher: Arc<dyn KeyCipher + Send + Sync> = Arc::new(StaticKeyCipher::new("cpa-runtime"));
    let state =
        CoreStateInner::new(Database::open(dir.clone()).unwrap(), dir.clone(), cipher).unwrap();
    save_managed(
        &dir,
        &ManagedCpa {
            current_version: "7.2.147".into(),
            previous_version: None,
            asset_sha256: "a".repeat(64),
            port: 8317,
        },
    )
    .unwrap();
    state
        .persist_managed_connection(
            8317,
            "management-key",
            "protected-key",
            vec!["model".into()],
        )
        .unwrap();
    let config = runtime_dir(&dir).join(CONFIG_NAME);

    assert!(state.managed_secrets(InstallMode::Update, &config).is_err());
    fs::write(&config, "api-keys:\n  - broken\n").unwrap();
    assert!(state.managed_secrets(InstallMode::Update, &config).is_err());
    fs::remove_file(&config).unwrap();
    fs::create_dir(&config).unwrap();
    assert!(state.managed_secrets(InstallMode::Update, &config).is_err());
    fs::remove_dir(&config).unwrap();

    write_config_yaml(
        &config,
        8317,
        &runtime_dir(&dir).join("auth"),
        "protected-key",
        &["extra-one".into(), "extra-two".into()],
    )
    .unwrap();
    let secrets = state.managed_secrets(InstallMode::Update, &config).unwrap();
    assert_eq!(secrets.management_key, "management-key");
    assert_eq!(secrets.inference_key, "protected-key");
    assert_eq!(secrets.extra_keys, ["extra-one", "extra-two"]);

    drop(state);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn fresh_install_does_not_replace_undecryptable_saved_secrets() {
    struct DecryptFailingCipher;

    impl KeyCipher for DecryptFailingCipher {
        fn encrypt(&self, _plaintext: &str) -> anyhow::Result<String> {
            anyhow::bail!("encryption is unavailable")
        }

        fn decrypt(&self, _ciphertext: &str) -> anyhow::Result<String> {
            anyhow::bail!("saved secret is unreadable")
        }
    }

    let dir = temp_dir("strict-saved-secrets");
    let good_cipher: Arc<dyn KeyCipher + Send + Sync> =
        Arc::new(StaticKeyCipher::new("correct-cipher"));
    let state = CoreStateInner::new(
        Database::open(dir.clone()).unwrap(),
        dir.clone(),
        good_cipher,
    )
    .unwrap();
    state
        .persist_managed_connection(
            8317,
            "management-key",
            "protected-key",
            vec!["model".into()],
        )
        .unwrap();
    let before = state.db.lock().cpa_integration().unwrap().unwrap();
    drop(state);

    let wrong_cipher: Arc<dyn KeyCipher + Send + Sync> = Arc::new(DecryptFailingCipher);
    let state = CoreStateInner::new(
        Database::open(dir.clone()).unwrap(),
        dir.clone(),
        wrong_cipher,
    )
    .unwrap();
    let error =
        match state.managed_secrets(InstallMode::Fresh, &runtime_dir(&dir).join(CONFIG_NAME)) {
            Ok(_) => panic!("fresh install must not replace unreadable saved secrets"),
            Err(error) => error,
        };
    assert!(matches!(error, CpaRuntimeError::Failed(_)));
    assert_eq!(
        state
            .db
            .lock()
            .cpa_integration()
            .unwrap()
            .unwrap()
            .management_key_cipher,
        before.management_key_cipher
    );

    drop(state);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn atomic_write_replaces_an_existing_file() {
    let dir = temp_dir("atomic-replace");
    let path = dir.join("managed.json");
    atomic_write(&path, b"before").unwrap();
    atomic_write(&path, b"after").unwrap();
    assert_eq!(fs::read(&path).unwrap(), b"after");
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn failed_update_commit_never_prunes_existing_previous_version() {
    let dir = temp_dir("failed-commit-prune");
    let versions = runtime_dir(&dir).join("versions");
    for version in ["7.2.147", "7.2.140", "7.2.130"] {
        fs::create_dir_all(versions.join(version)).unwrap();
    }

    let result = prune_versions_after_commit(
        Err(CpaRuntimeError::Failed("commit failed".into())),
        &versions,
        "7.2.150",
        Some("7.2.147"),
    );

    assert!(result.is_err());
    assert!(versions.join("7.2.147").is_dir());
    assert!(versions.join("7.2.140").is_dir());
    assert!(versions.join("7.2.130").is_dir());
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn committed_update_ignores_version_cleanup_failure() {
    let dir = temp_dir("cleanup-failure");
    let result = prune_versions_after_commit(
        Ok(()),
        &runtime_dir(&dir).join("versions"),
        "invalid/current",
        Some("7.2.147"),
    );
    assert!(result.is_ok());
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn managed_json_rejects_unsafe_version_components() {
    let dir = temp_dir("unsafe-managed");
    let root = runtime_dir(&dir);
    fs::create_dir_all(&root).unwrap();
    fs::write(
        root.join(MANAGED_NAME),
        format!(
            "{{\"currentVersion\":\"..\",\"assetSha256\":\"{}\",\"port\":8317}}",
            "a".repeat(64)
        ),
    )
    .unwrap();
    assert!(load_managed(&dir).is_err());
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn snapshot_owned_follows_managed_json_not_process() {
    let dir = temp_dir("owned");
    let cipher: Arc<dyn KeyCipher + Send + Sync> = Arc::new(StaticKeyCipher::new("cpa-runtime"));
    let state =
        CoreStateInner::new(Database::open(dir.clone()).unwrap(), dir.clone(), cipher).unwrap();
    save_managed(
        &dir,
        &ManagedCpa {
            current_version: "7.2.147".into(),
            previous_version: None,
            asset_sha256: "b".repeat(64),
            port: 8317,
        },
    )
    .unwrap();
    let snapshot = state.cpa_runtime_snapshot();
    assert!(snapshot.installed);
    assert!(snapshot.owned);
    assert!(!snapshot.running);
    drop(state);
    fs::remove_dir_all(dir).unwrap();
}

struct StoppedHost;

impl CpaRuntimeProcessHost for StoppedHost {
    fn start_owned(&self, _spec: &CpaRuntimeProcessSpec) -> Result<(), CpaRuntimeError> {
        Ok(())
    }

    fn stop_owned(&self) -> Result<(), CpaRuntimeError> {
        Ok(())
    }

    fn owned_running(&self) -> bool {
        false
    }

    fn logs(&self) -> CpaRuntimeLogTail {
        CpaRuntimeLogTail {
            stdout: String::new(),
            stderr: String::new(),
        }
    }

    fn add_log_secret(&self, _secret: &CpaRuntimeSecret) {}
}

struct RecordingHost {
    running: AtomicBool,
    stops: AtomicUsize,
    starts: Mutex<Vec<String>>,
}

impl RecordingHost {
    fn new(running: bool) -> Self {
        Self {
            running: AtomicBool::new(running),
            stops: AtomicUsize::new(0),
            starts: Mutex::new(Vec::new()),
        }
    }
}

impl CpaRuntimeProcessHost for RecordingHost {
    fn start_owned(&self, spec: &CpaRuntimeProcessSpec) -> Result<(), CpaRuntimeError> {
        self.starts.lock().push(
            spec.working_dir
                .file_name()
                .unwrap()
                .to_string_lossy()
                .to_string(),
        );
        self.running.store(true, Ordering::SeqCst);
        Ok(())
    }

    fn stop_owned(&self) -> Result<(), CpaRuntimeError> {
        self.stops.fetch_add(1, Ordering::SeqCst);
        self.running.store(false, Ordering::SeqCst);
        Ok(())
    }

    fn owned_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }

    fn logs(&self) -> CpaRuntimeLogTail {
        CpaRuntimeLogTail {
            stdout: String::new(),
            stderr: String::new(),
        }
    }

    fn add_log_secret(&self, _secret: &CpaRuntimeSecret) {}
}

#[tokio::test]
async fn stopped_managed_runtime_lists_configured_client_keys() {
    let dir = temp_dir("stopped-keys");
    let cipher: Arc<dyn KeyCipher + Send + Sync> = Arc::new(StaticKeyCipher::new("cpa-runtime"));
    let state =
        CoreStateInner::new(Database::open(dir.clone()).unwrap(), dir.clone(), cipher).unwrap();
    state.set_cpa_runtime_host(Arc::new(StoppedHost));
    save_managed(
        &dir,
        &ManagedCpa {
            current_version: "7.2.147".into(),
            previous_version: None,
            asset_sha256: "b".repeat(64),
            port: 8317,
        },
    )
    .unwrap();
    write_config_yaml(
        &runtime_dir(&dir).join(CONFIG_NAME),
        8317,
        &runtime_dir(&dir).join("auth"),
        "protected-key",
        &["extra-key".into()],
    )
    .unwrap();
    state
        .persist_managed_connection(
            8317,
            "management-key",
            "protected-key",
            vec!["model".into()],
        )
        .unwrap();

    let keys = state.list_cpa_runtime_keys().await.unwrap();
    assert_eq!(keys.len(), 2);
    assert!(keys.iter().any(|key| key.protected));
    assert!(keys.iter().any(|key| !key.protected));

    drop(state);
    fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn client_key_mutation_requires_owner_manifest() {
    let dir = temp_dir("external-key-block");
    let cipher: Arc<dyn KeyCipher + Send + Sync> = Arc::new(StaticKeyCipher::new("cpa-runtime"));
    let state =
        CoreStateInner::new(Database::open(dir.clone()).unwrap(), dir.clone(), cipher).unwrap();
    state.set_cpa_runtime_host(Arc::new(StoppedHost));
    let error = state
        .create_cpa_runtime_key(state.settings_revision(), state.process_generation())
        .await
        .unwrap_err();
    assert!(
        matches!(error, CpaRuntimeError::Invalid(message) if message.contains("not installed"))
    );
    drop(state);
    fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn candidate_probe_checks_health_management_version_and_inference_key_without_completion() {
    use axum::http::HeaderMap;
    use axum::routing::get;
    use axum::{Json, Router};
    use serde_json::json;

    async fn health() -> Json<serde_json::Value> {
        Json(json!({"status": "ok"}))
    }
    async fn accounts(headers: HeaderMap) -> impl axum::response::IntoResponse {
        assert_eq!(
            headers
                .get("authorization")
                .and_then(|value| value.to_str().ok()),
            Some("Bearer management-key")
        );
        ([("x-cpa-version", "7.2.147")], Json(json!({"files": []})))
    }
    async fn models(headers: HeaderMap) -> Json<serde_json::Value> {
        assert_eq!(
            headers
                .get("authorization")
                .and_then(|value| value.to_str().ok()),
            Some("Bearer inference-key")
        );
        Json(json!({"data": [{"id": "model"}]}))
    }

    let app = Router::new()
        .route("/healthz", get(health))
        .route("/v0/management/auth-files", get(accounts))
        .route("/v1/models", get(models));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let dir = temp_dir("candidate-probe");
    let cipher: Arc<dyn KeyCipher + Send + Sync> = Arc::new(StaticKeyCipher::new("cpa-runtime"));
    let state =
        CoreStateInner::new(Database::open(dir.clone()).unwrap(), dir.clone(), cipher).unwrap();
    let models = state
        .probe_candidate(address.port(), "management-key", "inference-key")
        .await
        .unwrap();
    assert_eq!(CpaCatalogModel::ids(&models), ["model"]);
    drop(state);
    fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn occupied_managed_port_never_stops_an_unknown_process() {
    let dir = temp_dir("external-port");
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    save_managed(
        &dir,
        &ManagedCpa {
            current_version: "7.2.147".into(),
            previous_version: None,
            asset_sha256: "a".repeat(64),
            port,
        },
    )
    .unwrap();
    let cipher: Arc<dyn KeyCipher + Send + Sync> = Arc::new(StaticKeyCipher::new("cpa-runtime"));
    let state =
        CoreStateInner::new(Database::open(dir.clone()).unwrap(), dir.clone(), cipher).unwrap();
    let host = Arc::new(RecordingHost::new(false));
    state.set_cpa_runtime_host(host.clone());

    let error = state
        .start_cpa_runtime(state.settings_revision(), state.process_generation())
        .await
        .unwrap_err();
    assert!(matches!(error, CpaRuntimeError::Conflict(_)));
    assert_eq!(host.stops.load(Ordering::SeqCst), 0);
    drop(listener);
    drop(state);
    fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn fresh_runtime_accepts_authenticated_empty_catalog_without_publishing_models() {
    use axum::http::HeaderMap;
    use axum::routing::get;
    use axum::{Json, Router};
    use serde_json::json;

    async fn accounts(headers: HeaderMap) -> impl axum::response::IntoResponse {
        assert_eq!(headers["authorization"], "Bearer management-key");
        ([("x-cpa-version", "7.2.151")], Json(json!({"files": []})))
    }
    async fn models(headers: HeaderMap) -> Json<serde_json::Value> {
        assert_eq!(headers["authorization"], "Bearer inference-key");
        Json(json!({"data": []}))
    }
    let app = Router::new()
        .route("/healthz", get(|| async { Json(json!({"status": "ok"})) }))
        .route("/v0/management/auth-files", get(accounts))
        .route("/v1/models", get(models));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let dir = temp_dir("empty-catalog");
    let cipher: Arc<dyn KeyCipher + Send + Sync> = Arc::new(StaticKeyCipher::new("cpa-runtime"));
    let state =
        CoreStateInner::new(Database::open(dir.clone()).unwrap(), dir.clone(), cipher).unwrap();

    let models = state
        .probe_candidate(port, "management-key", "inference-key")
        .await
        .unwrap();
    assert!(models.is_empty());
    state
        .persist_managed_connection(port, "management-key", "inference-key", models)
        .unwrap();
    assert!(
        !state
            .db
            .lock()
            .get_account(CPA_ACCOUNT_ID)
            .unwrap()
            .unwrap()
            .enabled
    );
    assert!(state.cpa_model_catalog().is_empty());

    let report = CpaClient::new(
        &state.config(),
        &format!("http://127.0.0.1:{port}"),
        "management-key".into(),
        "inference-key".into(),
        false,
    )
    .unwrap()
    .test()
    .await
    .unwrap();
    assert!(report.reachable && report.management_ready && report.inference_ready);
    assert_eq!(report.model_count, 0);

    // An empty authenticated result must also replace an older catalog.
    state
        .persist_managed_connection(
            port,
            "management-key",
            "inference-key",
            vec!["stale".into()],
        )
        .unwrap();
    state
        .persist_managed_connection(port, "management-key", "inference-key", vec![])
        .unwrap();
    assert!(state.cpa_model_catalog().is_empty());
    assert!(
        state
            .db
            .lock()
            .cpa_model_catalog()
            .unwrap()
            .unwrap()
            .models
            .is_empty()
    );
    server.abort();
    drop(state);
    fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn failed_rollback_restores_config_manifest_and_former_running_version() {
    use axum::extract::State;
    use axum::routing::get;
    use axum::{Json, Router};
    use serde_json::json;

    async fn health() -> Json<serde_json::Value> {
        Json(json!({"status": "ok"}))
    }
    async fn accounts() -> impl axum::response::IntoResponse {
        ([("x-cpa-version", "7.2.147")], Json(json!({"files": []})))
    }
    #[derive(Clone)]
    struct ProbeCount(Arc<AtomicUsize>);
    async fn models(State(count): State<ProbeCount>) -> impl axum::response::IntoResponse {
        if count.0.fetch_add(1, Ordering::SeqCst) < PROBE_ATTEMPTS {
            (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": "inference-key"})),
            )
        } else {
            (
                axum::http::StatusCode::OK,
                Json(json!({"data": [{"id": "model"}]})),
            )
        }
    }
    let app = Router::new()
        .route("/healthz", get(health))
        .route("/v0/management/auth-files", get(accounts))
        .route("/v1/models", get(models))
        .with_state(ProbeCount(Arc::new(AtomicUsize::new(0))));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let dir = temp_dir("rollback-restore");
    let root = runtime_dir(&dir);
    for (version, sha) in [("7.2.147", "a"), ("7.2.140", "b")] {
        let version_dir = root.join("versions").join(version);
        fs::create_dir_all(&version_dir).unwrap();
        fs::write(version_dir.join("cli-proxy-api.exe"), b"mz").unwrap();
        fs::write(version_dir.join(ASSET_SHA_NAME), sha.repeat(64)).unwrap();
    }
    write_config_yaml(
        &root.join(CONFIG_NAME),
        port,
        &root.join("auth"),
        "inference-key",
        &["current-extra".into()],
    )
    .unwrap();
    let current_config = fs::read(root.join(CONFIG_NAME)).unwrap();
    write_config_yaml(
        &root.join(PREVIOUS_CONFIG_NAME),
        port,
        &root.join("auth"),
        "inference-key",
        &["previous-extra".into()],
    )
    .unwrap();
    save_managed(
        &dir,
        &ManagedCpa {
            current_version: "7.2.147".into(),
            previous_version: Some("7.2.140".into()),
            asset_sha256: "a".repeat(64),
            port,
        },
    )
    .unwrap();
    let cipher: Arc<dyn KeyCipher + Send + Sync> = Arc::new(StaticKeyCipher::new("cpa-runtime"));
    let state =
        CoreStateInner::new(Database::open(dir.clone()).unwrap(), dir.clone(), cipher).unwrap();
    state
        .persist_managed_connection(
            port,
            "management-key",
            "inference-key",
            vec!["current-model".into()],
        )
        .unwrap();
    let catalog_before = state.db.lock().cpa_model_catalog().unwrap();
    let routing_before = state.cpa_model_catalog();
    let host = Arc::new(RecordingHost::new(true));
    state.set_cpa_runtime_host(host.clone());

    let error = state
        .rollback_cpa_runtime(state.settings_revision(), state.process_generation())
        .await
        .unwrap_err();
    assert!(!error.to_string().contains("inference-key"));
    assert_eq!(fs::read(root.join(CONFIG_NAME)).unwrap(), current_config);
    assert_eq!(
        load_managed(&dir).unwrap().unwrap().current_version,
        "7.2.147"
    );
    assert!(host.owned_running());
    assert_eq!(
        host.starts.lock().last().map(String::as_str),
        Some("7.2.147")
    );
    assert_eq!(state.db.lock().cpa_model_catalog().unwrap(), catalog_before);
    assert_eq!(state.cpa_model_catalog().as_ref(), routing_before.as_ref());
    drop(state);
    fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn successful_rollback_replaces_or_clears_catalog_and_bumps_once() {
    for (label, models) in [
        ("nonempty-catalog", vec!["rollback-model".into()]),
        ("empty-catalog", vec![]),
    ] {
        assert_successful_rollback_catalog(label, models).await;
    }
}

async fn assert_successful_rollback_catalog(label: &str, expected_models: Vec<String>) {
    use axum::extract::State;
    use axum::routing::get;
    use axum::{Json, Router};
    use serde_json::json;

    async fn health() -> Json<serde_json::Value> {
        Json(json!({"status": "ok"}))
    }
    async fn accounts() -> impl axum::response::IntoResponse {
        ([("x-cpa-version", "7.2.140")], Json(json!({"files": []})))
    }
    async fn models(State(models): State<Vec<String>>) -> Json<serde_json::Value> {
        Json(json!({"data": models.iter().map(|id| json!({"id": id})).collect::<Vec<_>>()}))
    }

    let app = Router::new()
        .route("/healthz", get(health))
        .route("/v0/management/auth-files", get(accounts))
        .route("/v1/models", get(models))
        .with_state(expected_models.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let dir = temp_dir("rollback-catalog");
    let root = runtime_dir(&dir);
    for (version, sha) in [("7.2.147", "a"), ("7.2.140", "b")] {
        let version_dir = root.join("versions").join(version);
        fs::create_dir_all(&version_dir).unwrap();
        fs::write(version_dir.join("cli-proxy-api.exe"), b"mz").unwrap();
        fs::write(version_dir.join(ASSET_SHA_NAME), sha.repeat(64)).unwrap();
    }
    write_config_yaml(
        &root.join(CONFIG_NAME),
        port,
        &root.join("auth"),
        "inference-key",
        &["current-extra".into()],
    )
    .unwrap();
    write_config_yaml(
        &root.join(PREVIOUS_CONFIG_NAME),
        port,
        &root.join("auth"),
        "inference-key",
        &["previous-extra".into()],
    )
    .unwrap();
    save_managed(
        &dir,
        &ManagedCpa {
            current_version: "7.2.147".into(),
            previous_version: Some("7.2.140".into()),
            asset_sha256: "a".repeat(64),
            port,
        },
    )
    .unwrap();
    let cipher: Arc<dyn KeyCipher + Send + Sync> = Arc::new(StaticKeyCipher::new("cpa-runtime"));
    let state =
        CoreStateInner::new(Database::open(dir.clone()).unwrap(), dir.clone(), cipher).unwrap();
    state
        .persist_managed_connection(
            port,
            "management-key",
            "inference-key",
            vec!["current-model".into()],
        )
        .unwrap();
    let host = Arc::new(RecordingHost::new(false));
    state.set_cpa_runtime_host(host);
    let revision = state.settings_revision();

    state
        .rollback_cpa_runtime(revision, state.process_generation())
        .await
        .unwrap();

    assert_eq!(state.settings_revision(), revision + 1, "{label}");
    assert_eq!(
        state.cpa_model_catalog().as_ref(),
        &expected_models,
        "{label}"
    );
    let managed = load_managed(&dir).unwrap().unwrap();
    assert_eq!(managed.current_version, "7.2.140");
    assert_eq!(managed.previous_version.as_deref(), Some("7.2.147"));
    assert_eq!(managed.asset_sha256, "b".repeat(64));
    drop(state);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn failed_first_install_logs_survive_without_owner_until_next_lifecycle_operation() {
    let dir = temp_dir("failed-install-logs");
    let cipher: Arc<dyn KeyCipher + Send + Sync> = Arc::new(StaticKeyCipher::new("cpa-runtime"));
    let state =
        CoreStateInner::new(Database::open(dir.clone()).unwrap(), dir.clone(), cipher).unwrap();
    state.set_cpa_runtime_host(Arc::new(StoppedHost));
    state.cpa_runtime.set_phase(CpaRuntimePhase::Failed, None);
    state.cpa_runtime.cache_failure_logs(CpaRuntimeLogTail {
        stdout: "candidate stdout".into(),
        stderr: "candidate stderr".into(),
    });

    let logs = state.cpa_runtime_logs().unwrap();
    assert_eq!(logs.stdout, "candidate stdout");
    assert_eq!(logs.stderr, "candidate stderr");
    drop(state.cpa_runtime.begin_lifecycle_operation("install"));
    state.cpa_runtime.set_phase(CpaRuntimePhase::Idle, None);
    assert!(state.cpa_runtime_logs().is_err());

    drop(state);
    fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn download_caps_a_body_when_content_length_is_missing() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buf = vec![0; 1024];
        let _ = socket.read(&mut buf).await;
        let _ = socket
            .write_all(b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n")
            .await;
        let _ = socket.write_all(&vec![b'x'; 1024 * 1024]).await;
    });

    let client = reqwest::Client::new();
    let error = download_bytes(&client, &format!("http://{addr}/asset"), 64)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("size limit"), "{error}");
}

#[tokio::test]
async fn download_rejects_an_advertised_oversize_content_length() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buf = vec![0; 1024];
        let _ = socket.read(&mut buf).await;
        let _ = socket
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 1000\r\n\r\n")
            .await;
        let _ = socket.write_all(&vec![b'x'; 1000]).await;
    });

    let client = reqwest::Client::new();
    let error = download_bytes(&client, &format!("http://{addr}/asset"), 64)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("size limit"), "{error}");
}

#[tokio::test]
async fn download_accepts_a_body_at_the_size_limit() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buf = vec![0; 1024];
        let _ = socket.read(&mut buf).await;
        let body = vec![b'y'; 64];
        let header = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", body.len());
        let _ = socket.write_all(header.as_bytes()).await;
        let _ = socket.write_all(&body).await;
    });

    let client = reqwest::Client::new();
    let bytes = download_bytes(&client, &format!("http://{addr}/asset"), 64)
        .await
        .unwrap();
    assert_eq!(bytes, vec![b'y'; 64]);
}

#[tokio::test]
async fn download_follows_a_redirect_and_keeps_the_exact_bytes() {
    use axum::Router;
    use axum::response::Redirect;
    use axum::routing::get;

    let app = Router::new()
        .route("/asset", get(|| async { Redirect::temporary("/real") }))
        .route("/real", get(|| async { "payload-bytes" }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let client = reqwest::Client::new();
    let bytes = download_bytes(&client, &format!("http://{addr}/asset"), 64)
        .await
        .unwrap();
    assert_eq!(bytes, b"payload-bytes");
}

#[tokio::test]
async fn remove_deletes_owned_auth_before_managed_json_and_is_retryable() {
    let dir = temp_dir("remove-auth");
    let root = runtime_dir(&dir);
    fs::create_dir_all(root.join("auth")).unwrap();
    fs::write(root.join("auth").join("oauth.json"), b"secret-token").unwrap();
    fs::create_dir_all(root.join("logs")).unwrap();
    fs::create_dir_all(root.join("versions").join("7.2.147")).unwrap();
    write_config_yaml(
        &root.join(CONFIG_NAME),
        8317,
        &root.join("auth"),
        "inference-key",
        &[],
    )
    .unwrap();
    save_managed(
        &dir,
        &ManagedCpa {
            current_version: "7.2.147".into(),
            previous_version: None,
            asset_sha256: "a".repeat(64),
            port: 8317,
        },
    )
    .unwrap();
    let cipher: Arc<dyn KeyCipher + Send + Sync> = Arc::new(StaticKeyCipher::new("cpa-runtime"));
    let state =
        CoreStateInner::new(Database::open(dir.clone()).unwrap(), dir.clone(), cipher).unwrap();
    state.set_cpa_runtime_host(Arc::new(StoppedHost));
    state
        .persist_managed_connection(
            8317,
            "management-key",
            "inference-key",
            vec!["model".into()],
        )
        .unwrap();

    state
        .remove_cpa_runtime(state.settings_revision(), state.process_generation())
        .await
        .unwrap();
    assert!(!root.join("auth").exists());
    assert!(!root.join(MANAGED_NAME).exists());

    fs::create_dir_all(root.join("auth")).unwrap();
    fs::write(root.join("auth").join("leftover.json"), b"token").unwrap();
    save_managed(
        &dir,
        &ManagedCpa {
            current_version: "7.2.147".into(),
            previous_version: None,
            asset_sha256: "a".repeat(64),
            port: 8317,
        },
    )
    .unwrap();
    state
        .remove_cpa_runtime(state.settings_revision(), state.process_generation())
        .await
        .unwrap();
    assert!(!root.join("auth").exists());
    assert!(!root.join(MANAGED_NAME).exists());

    drop(state);
    fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn remove_without_owner_does_not_delete_auth() {
    let dir = temp_dir("remove-external-auth");
    let root = runtime_dir(&dir);
    fs::create_dir_all(root.join("auth")).unwrap();
    fs::write(root.join("auth").join("oauth.json"), b"external-token").unwrap();
    let cipher: Arc<dyn KeyCipher + Send + Sync> = Arc::new(StaticKeyCipher::new("cpa-runtime"));
    let state =
        CoreStateInner::new(Database::open(dir.clone()).unwrap(), dir.clone(), cipher).unwrap();
    state.set_cpa_runtime_host(Arc::new(StoppedHost));

    let error = state
        .remove_cpa_runtime(state.settings_revision(), state.process_generation())
        .await
        .unwrap_err();
    assert!(
        matches!(error, CpaRuntimeError::Invalid(message) if message.contains("not installed"))
    );
    assert!(root.join("auth").join("oauth.json").is_file());

    drop(state);
    fs::remove_dir_all(dir).unwrap();
}

struct ProbeHost {
    running: AtomicBool,
    starts: AtomicUsize,
    stops: AtomicUsize,
    port: u16,
}

impl ProbeHost {
    fn new(port: u16) -> Self {
        Self {
            running: AtomicBool::new(false),
            starts: AtomicUsize::new(0),
            stops: AtomicUsize::new(0),
            port,
        }
    }
}

impl CpaRuntimeProcessHost for ProbeHost {
    fn start_owned(&self, _spec: &CpaRuntimeProcessSpec) -> Result<(), CpaRuntimeError> {
        use axum::routing::get;
        use axum::{Json, Router};
        use serde_json::json;

        self.starts.fetch_add(1, Ordering::SeqCst);
        let port = self.port;
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("CPA probe runtime");
            runtime.block_on(async move {
                let app = Router::new()
                    .route(
                        "/healthz",
                        get(|| async { Json(json!({ "status": "ok" })) }),
                    )
                    .route(
                        "/v0/management/auth-files",
                        get(|| async {
                            ([("x-cpa-version", "7.2.147")], Json(json!({ "files": [] })))
                        }),
                    )
                    .route(
                        "/v1/models",
                        get(|| async { Json(json!({ "data": [{ "id": "model" }] })) }),
                    );
                let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
                    .await
                    .expect("CPA probe bind");
                let _ = ready_tx.send(());
                let _ = axum::serve(listener, app).await;
            });
        });
        ready_rx.recv().expect("CPA probe thread should start");
        wait_for_http_ok(port);
        self.running.store(true, Ordering::SeqCst);
        Ok(())
    }

    fn stop_owned(&self) -> Result<(), CpaRuntimeError> {
        self.stops.fetch_add(1, Ordering::SeqCst);
        self.running.store(false, Ordering::SeqCst);
        Ok(())
    }

    fn owned_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }

    fn logs(&self) -> CpaRuntimeLogTail {
        CpaRuntimeLogTail {
            stdout: String::new(),
            stderr: String::new(),
        }
    }

    fn add_log_secret(&self, _secret: &CpaRuntimeSecret) {}
}

fn wait_for_http_ok(port: u16) {
    use std::io::{Read, Write};
    use std::time::Duration;

    for _ in 0..200 {
        if let Ok(mut stream) = std::net::TcpStream::connect(("127.0.0.1", port)) {
            let _ = stream.set_read_timeout(Some(Duration::from_millis(50)));
            let _ = stream.set_write_timeout(Some(Duration::from_millis(50)));
            if stream
                .write_all(b"GET /healthz HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
                .is_ok()
            {
                let mut buf = [0u8; 32];
                if let Ok(n) = stream.read(&mut buf)
                    && n >= 12
                    && buf.starts_with(b"HTTP/1.1 200")
                {
                    return;
                }
            }
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("CPA probe server did not become ready on port {port}");
}

fn free_loopback_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
}

fn prepare_managed_runtime(dir: &std::path::Path, state: &CoreStateInner, port: u16) {
    let root = runtime_dir(dir);
    let version_dir = root.join("versions").join("7.2.147");
    fs::create_dir_all(&version_dir).unwrap();
    fs::write(version_dir.join("cli-proxy-api.exe"), b"mz").unwrap();
    write_config_yaml(
        &root.join(CONFIG_NAME),
        port,
        &root.join("auth"),
        "inference-key",
        &[],
    )
    .unwrap();
    save_managed(
        dir,
        &ManagedCpa {
            current_version: "7.2.147".into(),
            previous_version: None,
            asset_sha256: "a".repeat(64),
            port,
        },
    )
    .unwrap();
    state
        .persist_managed_connection(
            port,
            "management-key",
            "inference-key",
            vec!["model".into()],
        )
        .unwrap();
}

fn assert_revision_conflict(error: CpaRuntimeError) {
    assert_eq!(error, CpaRuntimeError::Conflict("revisionConflict".into()));
}

#[tokio::test]
async fn successful_start_bumps_revision_and_rejects_stale_stop() {
    let port = free_loopback_port();
    let dir = temp_dir("start-cas");
    let cipher: Arc<dyn KeyCipher + Send + Sync> = Arc::new(StaticKeyCipher::new("cpa-runtime"));
    let state =
        CoreStateInner::new(Database::open(dir.clone()).unwrap(), dir.clone(), cipher).unwrap();
    prepare_managed_runtime(&dir, &state, port);
    let host = Arc::new(ProbeHost::new(port));
    state.set_cpa_runtime_host(host.clone());
    let revision = state.settings_revision();
    let generation = state.process_generation();

    state.start_cpa_runtime(revision, generation).await.unwrap();
    assert_eq!(host.starts.load(Ordering::SeqCst), 1);
    assert!(host.owned_running());
    assert_eq!(state.settings_revision(), revision + 1);

    assert_revision_conflict(
        state
            .stop_cpa_runtime(revision, generation)
            .expect_err("stale stop token must not stop the process"),
    );
    assert_eq!(host.stops.load(Ordering::SeqCst), 0);
    assert!(host.owned_running());

    assert_revision_conflict(
        state
            .start_cpa_runtime(revision, generation)
            .await
            .expect_err("stale start token must not start again"),
    );
    assert_eq!(host.starts.load(Ordering::SeqCst), 1);

    drop(state);
    fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn already_running_start_does_not_bump_but_stop_does() {
    let dir = temp_dir("start-noop-cas");
    save_managed(
        &dir,
        &ManagedCpa {
            current_version: "7.2.147".into(),
            previous_version: None,
            asset_sha256: "a".repeat(64),
            port: 8317,
        },
    )
    .unwrap();
    let cipher: Arc<dyn KeyCipher + Send + Sync> = Arc::new(StaticKeyCipher::new("cpa-runtime"));
    let state =
        CoreStateInner::new(Database::open(dir.clone()).unwrap(), dir.clone(), cipher).unwrap();
    let host = Arc::new(RecordingHost::new(true));
    state.set_cpa_runtime_host(host.clone());
    let revision = state.settings_revision();
    let generation = state.process_generation();

    state.start_cpa_runtime(revision, generation).await.unwrap();
    assert_eq!(state.settings_revision(), revision);
    assert_eq!(host.starts.lock().len(), 0);

    state.stop_cpa_runtime(revision, generation).unwrap();
    assert_eq!(host.stops.load(Ordering::SeqCst), 1);
    assert!(!host.owned_running());
    assert_eq!(state.settings_revision(), revision + 1);

    assert_revision_conflict(
        state
            .stop_cpa_runtime(revision, generation)
            .expect_err("stale stop token must not stop again"),
    );
    assert_eq!(host.stops.load(Ordering::SeqCst), 1);

    assert_revision_conflict(
        state
            .start_cpa_runtime(revision, generation)
            .await
            .expect_err("stale start token must not start after stop"),
    );
    assert_eq!(host.starts.lock().len(), 0);

    drop(state);
    fs::remove_dir_all(dir).unwrap();
}

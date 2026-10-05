//! Synthetic lock and manifest bytes. These tests do not read
//! `runtime/cpa/artifact-lock.json` and are not host acceptance.

use super::*;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

const PRODUCTION_BYTES: &[u8] = b"synthetic-production-host";
const FIXTURE_BYTES: &[u8] = b"synthetic-fixture-host";
const LINUX_BYTES: &[u8] = b"synthetic-linux-host";
const MAC_BYTES: &[u8] = b"synthetic-macos-host";

fn digest(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(bytes))
}

fn filled(byte: &str) -> String {
    byte.repeat(32)
}

fn windows() -> Platform {
    Platform {
        os: "windows",
        arch: "x86_64",
    }
}

fn record(
    os: &str,
    arch: &str,
    variant: &str,
    executable: &str,
    sha: &str,
    verification: &str,
) -> Value {
    json!({
        "os": os,
        "arch": arch,
        "variant": variant,
        "executable": executable,
        "sha256": sha,
        "verification": verification
    })
}

fn standard_lock() -> Value {
    json!({
        "schemaVersion": 1,
        "sourceCommit": PINNED_COMMIT,
        "sourceVersion": PINNED_VERSION,
        "protocolVersion": 1,
        "buildIdentity": filled("11"),
        "hostSHA256": filled("22"),
        "overlaySHA256": filled("33"),
        "requiredCapabilities": REQUIRED_CAPABILITIES,
        "artifacts": [
            record("windows", "x86_64", "production", "ocg-cpa-host.exe", &digest(PRODUCTION_BYTES), "host-suite"),
            record("windows", "x86_64", "native-loopback-fixture", "ocg-cpa-host.exe", &digest(FIXTURE_BYTES), "host-suite"),
            record("linux", "amd64", "production", "ocg-cpa-host-linux-amd64", &digest(LINUX_BYTES), "compiled-only"),
            record("darwin", "arm64", "production", "ocg-cpa-host-darwin-arm64", &digest(MAC_BYTES), "compiled-only")
        ]
    })
}

fn manifest(
    os: &str,
    arch: &str,
    variant: &str,
    executable: &str,
    tags: Value,
    claimed_sha: Option<&str>,
) -> Vec<u8> {
    let mut value = json!({
        "sourceCommit": PINNED_COMMIT,
        "sourceVersion": PINNED_VERSION,
        "protocolVersion": 1,
        "buildIdentity": filled("11"),
        "hostSHA256": filled("22"),
        "overlaySHA256": filled("33"),
        "capabilities": REQUIRED_CAPABILITIES,
        "variant": variant,
        "buildTags": tags,
        "os": os,
        "arch": arch,
        "executable": executable
    });
    if let Some(sha) = claimed_sha {
        value["executableSHA256"] = json!(sha);
    }
    serde_json::to_vec(&value).unwrap()
}

fn production_manifest() -> Vec<u8> {
    manifest(
        "win32",
        "x64",
        "production",
        "ocg-cpa-host.exe",
        json!([]),
        Some(&digest(PRODUCTION_BYTES)),
    )
}

fn expect_err(
    lock: &Value,
    manifest_bytes: &[u8],
    executable: &[u8],
    platform: Platform,
    variant: Variant,
    needle: &str,
) {
    let error = verify_material(
        &serde_json::to_vec(lock).unwrap(),
        manifest_bytes,
        executable,
        platform,
        variant,
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains(needle), "{error}");
}

struct Temp(PathBuf);

impl Temp {
    fn new(label: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("ocg-cpa-artifact-{label}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn symlink_file(target: &Path, link: &Path) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        std::os::windows::fs::symlink_file(target, link)
    }
    #[cfg(not(windows))]
    {
        std::os::unix::fs::symlink(target, link)
    }
}

#[test]
fn compile_selection_follows_the_feature_and_the_target() {
    let platform = current_platform().expect("supported compile target");
    if cfg!(windows) {
        assert_eq!(platform.os, "windows");
    } else if cfg!(target_os = "linux") {
        assert_eq!(platform.os, "linux");
    } else if cfg!(target_os = "macos") {
        assert_eq!(platform.os, "macos");
    }
    if cfg!(target_arch = "x86_64") {
        assert_eq!(platform.arch, "x86_64");
    } else if cfg!(target_arch = "aarch64") {
        assert_eq!(platform.arch, "aarch64");
    }
    #[cfg(feature = "ollama-cloud-loopback-test")]
    assert_eq!(selected_variant(), Variant::NativeLoopbackFixture);
    #[cfg(not(feature = "ollama-cloud-loopback-test"))]
    assert_eq!(selected_variant(), Variant::Production);
    assert_eq!(PINNED_COMMIT, "6fecc6e5567912661654a4eaf9b8f5436facd1c2");
    assert_eq!(PINNED_VERSION, "v8.0.10");
    assert_eq!(PINNED_VERSION_CANONICAL, "8.0.10");
    assert_eq!(REQUIRED_CAPABILITIES.len(), 16);
    assert_eq!(REQUIRED_CAPABILITIES[15], "native-final-endpoint-pin-v1");
    assert!(version_accepted(Some("v8.0.10")).is_ok());
    assert!(version_accepted(Some("8.0.10")).is_ok());
    assert!(version_accepted(Some("9.0.0")).is_err());
}

#[test]
fn injected_lock_selects_one_variant_digest() {
    let lock = standard_lock();
    let production = verify_material(
        &serde_json::to_vec(&lock).unwrap(),
        &production_manifest(),
        PRODUCTION_BYTES,
        windows(),
        Variant::Production,
    )
    .expect("production synthetic record");
    assert_eq!(production.sha256, digest(PRODUCTION_BYTES));
    assert_ne!(production.sha256, digest(FIXTURE_BYTES));
    assert_ne!(production.sha256, PINNED_SHA256);
    assert_eq!(production.executable, "ocg-cpa-host.exe");
    assert_eq!(production.source_commit, PINNED_COMMIT);

    let fixture_manifest = manifest(
        "windows",
        "x86_64",
        "native-loopback-fixture",
        "ocg-cpa-host.exe",
        json!(["ocg_native_loopback_fixture"]),
        None,
    );
    let fixture = verify_material(
        &serde_json::to_vec(&lock).unwrap(),
        &fixture_manifest,
        FIXTURE_BYTES,
        windows(),
        Variant::NativeLoopbackFixture,
    )
    .expect("fixture synthetic record");
    assert_eq!(fixture.sha256, digest(FIXTURE_BYTES));
}

#[test]
fn normalized_lock_tokens_select_the_same_platform() {
    let sha = digest(PRODUCTION_BYTES);
    let lock = json!({
        "schemaVersion": 1,
        "sourceCommit": PINNED_COMMIT,
        "sourceVersion": PINNED_VERSION,
        "protocolVersion": 1,
        "buildIdentity": filled("11"),
        "hostSHA256": filled("22"),
        "overlaySHA256": filled("33"),
        "requiredCapabilities": REQUIRED_CAPABILITIES,
        "artifacts": [record("win32", "amd64", "production", "ocg-cpa-host.exe", &sha, "host-suite")]
    });
    let selected = verify_material(
        &serde_json::to_vec(&lock).unwrap(),
        &production_manifest(),
        PRODUCTION_BYTES,
        windows(),
        Variant::Production,
    )
    .unwrap();
    assert_eq!(selected.sha256, sha);

    let mac = verify_material(
        &serde_json::to_vec(&standard_lock()).unwrap(),
        &manifest(
            "darwin",
            "arm64",
            "production",
            "ocg-cpa-host-darwin-arm64",
            json!([]),
            None,
        ),
        MAC_BYTES,
        Platform {
            os: "macos",
            arch: "aarch64",
        },
        Variant::Production,
    )
    .unwrap();
    assert_eq!(mac.sha256, digest(MAC_BYTES));
    assert_eq!(mac.executable, "ocg-cpa-host-darwin-arm64");
}

#[test]
fn tamper_identity_platform_capability_and_variant_fail_closed() {
    let mut lock = standard_lock();
    let host_manifest = production_manifest();
    lock["sourceCommit"] = json!("0000000000000000000000000000000000000000");
    expect_err(
        &lock,
        &host_manifest,
        PRODUCTION_BYTES,
        windows(),
        Variant::Production,
        "required source",
    );

    let mut lock = standard_lock();
    lock["buildIdentity"] = json!(filled("ab").to_ascii_uppercase());
    expect_err(
        &lock,
        &host_manifest,
        PRODUCTION_BYTES,
        windows(),
        Variant::Production,
        "required source",
    );

    let mut lock = standard_lock();
    lock["protocolVersion"] = json!("1");
    expect_err(
        &lock,
        &host_manifest,
        PRODUCTION_BYTES,
        windows(),
        Variant::Production,
        "required source",
    );

    let mut lock = standard_lock();
    lock["requiredCapabilities"].as_array_mut().unwrap().pop();
    expect_err(
        &lock,
        &host_manifest,
        PRODUCTION_BYTES,
        windows(),
        Variant::Production,
        "capability set is incomplete",
    );

    let mut manifest_value: Value = serde_json::from_slice(&host_manifest).unwrap();
    manifest_value["buildIdentity"] = json!(filled("44"));
    expect_err(
        &standard_lock(),
        &serde_json::to_vec(&manifest_value).unwrap(),
        PRODUCTION_BYTES,
        windows(),
        Variant::Production,
        "does not match the required artifact",
    );

    let mut manifest_value: Value = serde_json::from_slice(&host_manifest).unwrap();
    manifest_value["capabilities"].as_array_mut().unwrap().pop();
    expect_err(
        &standard_lock(),
        &serde_json::to_vec(&manifest_value).unwrap(),
        PRODUCTION_BYTES,
        windows(),
        Variant::Production,
        "missing a required capability",
    );

    expect_err(
        &standard_lock(),
        &manifest(
            "linux",
            "x86_64",
            "production",
            "ocg-cpa-host.exe",
            json!([]),
            None,
        ),
        PRODUCTION_BYTES,
        windows(),
        Variant::Production,
        "os/arch",
    );

    expect_err(
        &standard_lock(),
        &manifest(
            "windows",
            "x86_64",
            "native-loopback-fixture",
            "ocg-cpa-host.exe",
            json!(["ocg_native_loopback_fixture"]),
            None,
        ),
        PRODUCTION_BYTES,
        windows(),
        Variant::Production,
        "variant",
    );

    expect_err(
        &standard_lock(),
        &manifest(
            "windows",
            "x86_64",
            "production",
            "ocg-cpa-host.exe",
            json!(["ocg_native_loopback_fixture"]),
            None,
        ),
        PRODUCTION_BYTES,
        windows(),
        Variant::Production,
        "build tags",
    );

    expect_err(
        &standard_lock(),
        &production_manifest(),
        b"tampered-host-bytes",
        windows(),
        Variant::Production,
        "hash",
    );

    let mut claimed = serde_json::from_slice::<Value>(&production_manifest()).unwrap();
    claimed["executableSHA256"] = json!(digest(FIXTURE_BYTES));
    expect_err(
        &standard_lock(),
        &serde_json::to_vec(&claimed).unwrap(),
        PRODUCTION_BYTES,
        windows(),
        Variant::Production,
        "does not match the required artifact",
    );
}

#[test]
fn duplicate_missing_unsafe_and_placeholder_records_fail_closed() {
    let mut lock = standard_lock();
    lock["artifacts"].as_array_mut().unwrap().push(record(
        "win32",
        "x64",
        "production",
        "ocg-cpa-host.exe",
        &digest(PRODUCTION_BYTES),
        "host-suite",
    ));
    expect_err(
        &lock,
        &production_manifest(),
        PRODUCTION_BYTES,
        windows(),
        Variant::Production,
        "duplicated",
    );

    let mut lock = standard_lock();
    lock["artifacts"].as_array_mut().unwrap().retain(|item| {
        item["os"].as_str() != Some("windows") || item["variant"].as_str() != Some("production")
    });
    expect_err(
        &lock,
        &production_manifest(),
        PRODUCTION_BYTES,
        windows(),
        Variant::Production,
        "missing",
    );

    expect_err(
        &standard_lock(),
        &manifest(
            "macos",
            "aarch64",
            "native-loopback-fixture",
            "ocg-cpa-host-darwin-arm64",
            json!(["ocg_native_loopback_fixture"]),
            None,
        ),
        MAC_BYTES,
        Platform {
            os: "macos",
            arch: "aarch64",
        },
        Variant::NativeLoopbackFixture,
        "missing",
    );

    let mut lock = standard_lock();
    lock["artifacts"].as_array_mut().unwrap().push(record(
        "freebsd",
        "x86_64",
        "production",
        "ocg-cpa-host",
        &digest(b"other"),
        "host-suite",
    ));
    expect_err(
        &lock,
        &production_manifest(),
        PRODUCTION_BYTES,
        windows(),
        Variant::Production,
        "unsupported",
    );

    let mut lock = standard_lock();
    lock["artifacts"][0]["sha256"] = json!(PINNED_SHA256);
    expect_err(
        &lock,
        &production_manifest(),
        PRODUCTION_BYTES,
        windows(),
        Variant::Production,
        "placeholder",
    );

    let mut lock = standard_lock();
    lock["artifacts"][0]["sha256"] = json!(digest(PRODUCTION_BYTES).to_ascii_uppercase());
    expect_err(
        &lock,
        &production_manifest(),
        PRODUCTION_BYTES,
        windows(),
        Variant::Production,
        "record is invalid",
    );

    let mut lock = standard_lock();
    lock["artifacts"]
        .as_array_mut()
        .unwrap()
        .get_mut(0)
        .unwrap()
        .as_object_mut()
        .unwrap()
        .remove("sha256");
    expect_err(
        &lock,
        &production_manifest(),
        PRODUCTION_BYTES,
        windows(),
        Variant::Production,
        "record is invalid",
    );

    for name in ["../ocg-cpa-host.exe", "a/b", "con.exe", "..", ""] {
        let mut lock = standard_lock();
        lock["artifacts"][0]["executable"] = json!(name);
        expect_err(
            &lock,
            &production_manifest(),
            PRODUCTION_BYTES,
            windows(),
            Variant::Production,
            "unsafe",
        );
    }

    let empty = verify_material(
        b" \n\t",
        &production_manifest(),
        PRODUCTION_BYTES,
        windows(),
        Variant::Production,
    )
    .unwrap_err()
    .to_string();
    assert!(empty.contains("empty"), "{empty}");
    let mut huge = vec![b' '; io::MAX_CONFIG_BYTES + 1];
    huge[0] = b'{';
    let error = verify_material(
        &huge,
        &production_manifest(),
        PRODUCTION_BYTES,
        windows(),
        Variant::Production,
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("too large"), "{error}");

    let mut lock = standard_lock();
    lock["schemaVersion"] = json!(2);
    expect_err(
        &lock,
        &production_manifest(),
        PRODUCTION_BYTES,
        windows(),
        Variant::Production,
        "schema is not 1",
    );
}

#[test]
fn directory_bytes_are_rechecked_and_a_symlink_is_not_regular() {
    let temp = Temp::new("dir");
    let lock = serde_json::to_vec(&standard_lock()).unwrap();
    let manifest = production_manifest();
    std::fs::write(temp.0.join("manifest.json"), &manifest).unwrap();
    let executable = temp.0.join("ocg-cpa-host.exe");
    std::fs::write(&executable, PRODUCTION_BYTES).unwrap();
    let artifact = verify_directory_with_lock(&lock, &temp.0, windows(), Variant::Production)
        .expect("regular synthetic file");
    assert_eq!(artifact.sha256, digest(PRODUCTION_BYTES));
    assert_eq!(artifact.executable, executable);

    std::fs::write(&executable, b"tampered").unwrap();
    let error = verify_directory_with_lock(&lock, &temp.0, windows(), Variant::Production)
        .unwrap_err()
        .to_string();
    assert!(error.contains("hash"), "{error}");

    std::fs::remove_file(&executable).unwrap();
    std::fs::create_dir(&executable).unwrap();
    let error = verify_directory_with_lock(&lock, &temp.0, windows(), Variant::Production)
        .unwrap_err()
        .to_string();
    assert!(error.contains("regular"), "{error}");
    std::fs::remove_dir(&executable).unwrap();

    let real = temp.0.join("real-host.exe");
    std::fs::write(&real, PRODUCTION_BYTES).unwrap();
    match symlink_file(&real, &executable) {
        Ok(()) => {
            let error = verify_directory_with_lock(&lock, &temp.0, windows(), Variant::Production)
                .unwrap_err()
                .to_string();
            assert!(error.contains("reparse"), "{error}");
        }
        Err(error) => {
            let denied = matches!(
                error.kind(),
                std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::Unsupported
            ) || matches!(error.raw_os_error(), Some(1314 | 5));
            assert!(denied, "symlink fixture was not created: {error}");
        }
    }
}

fn hosted_artifact(dir: &Path, bytes: &[u8], sha: &str) -> Artifact {
    std::fs::create_dir_all(dir).unwrap();
    let executable = dir.join("selected-host.bin");
    std::fs::write(&executable, bytes).unwrap();
    Artifact {
        dir: dir.to_path_buf(),
        executable,
        sha256: sha.to_string(),
        source_commit: PINNED_COMMIT.to_string(),
        source_version: PINNED_VERSION.to_string(),
        protocol_version: 1,
        capabilities: REQUIRED_CAPABILITIES
            .iter()
            .map(|name| (*name).to_string())
            .collect(),
    }
}

fn plant_consistent(data: &Path, sha: &str, bytes: &[u8]) {
    let directory = io::version_dir(data, sha);
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join(io::executable_name());
    std::fs::write(&path, bytes).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    std::fs::write(directory.join(".asset-sha256"), format!("{sha}\n")).unwrap();
}

fn variant_wire(variant: Variant) -> &'static str {
    match variant {
        Variant::Production => "production",
        Variant::NativeLoopbackFixture => "native-loopback-fixture",
    }
}

#[test]
fn lifecycle_accepts_only_the_selected_record() {
    let selection = synthetic_selection().expect("compile platform");
    let flipped = match selection.variant {
        Variant::Production => Variant::NativeLoopbackFixture,
        Variant::NativeLoopbackFixture => Variant::Production,
    };
    let other_platform = if selection.platform.os == "linux" && selection.platform.arch == "x86_64"
    {
        Platform {
            os: "windows",
            arch: "x86_64",
        }
    } else {
        Platform {
            os: "linux",
            arch: "x86_64",
        }
    };
    assert_eq!(
        trusted_sha_for_lock(&selection.lock, selection.platform, selection.variant).unwrap(),
        selection.selected_sha
    );
    assert_eq!(
        trusted_sha_for_lock(&selection.lock, selection.platform, flipped).unwrap(),
        selection.alternate_sha
    );
    assert_eq!(
        trusted_sha_for_lock(&selection.lock, other_platform, Variant::Production).unwrap(),
        selection.other_platform_sha
    );
    assert_ne!(selection.selected_sha, selection.alternate_sha);
    assert_ne!(selection.selected_sha, selection.other_platform_sha);
    assert_ne!(selection.selected_sha, PINNED_SHA256);

    let temp = Temp::new("lifecycle");
    let data = temp.0.join("data");
    let artifact = hosted_artifact(
        &temp.0.join("src"),
        selection.selected_bytes,
        &selection.selected_sha,
    );
    let installed = install_for_lock(
        &data,
        &artifact,
        &selection.lock,
        selection.platform,
        selection.variant,
    )
    .expect("selected install");
    assert_eq!(
        installed.file_name().and_then(|name| name.to_str()),
        Some(io::executable_name())
    );
    assert_eq!(
        installed.parent().unwrap(),
        io::version_dir(&data, &selection.selected_sha)
    );
    assert_eq!(std::fs::read(&installed).unwrap(), selection.selected_bytes);
    let marker = io::version_dir(&data, &selection.selected_sha).join(".asset-sha256");
    assert_eq!(
        std::fs::read_to_string(&marker).unwrap(),
        format!("{}\n", selection.selected_sha)
    );
    assert_eq!(
        installed_for_lock(
            &data,
            &selection.selected_sha,
            &selection.lock,
            selection.platform,
            selection.variant
        )
        .unwrap(),
        installed
    );

    std::fs::write(&installed, b"tampered-installed").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&installed, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let tampered = installed_for_lock(
        &data,
        &selection.selected_sha,
        &selection.lock,
        selection.platform,
        selection.variant,
    )
    .unwrap_err()
    .to_string();
    assert!(tampered.contains("hash"), "{tampered}");
    std::fs::write(&installed, selection.selected_bytes).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&installed, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    assert!(
        installed_for_lock(
            &data,
            &selection.selected_sha,
            &selection.lock,
            selection.platform,
            selection.variant
        )
        .is_ok()
    );
    std::fs::write(&marker, format!("{}\n\n", selection.selected_sha)).unwrap();
    assert!(
        installed_for_lock(
            &data,
            &selection.selected_sha,
            &selection.lock,
            selection.platform,
            selection.variant
        )
        .is_err()
    );
    std::fs::write(&marker, selection.selected_sha.as_bytes()).unwrap();
    assert!(
        installed_for_lock(
            &data,
            &selection.selected_sha,
            &selection.lock,
            selection.platform,
            selection.variant
        )
        .is_ok()
    );

    let untrusted_bytes = b"synthetic-untrusted-host";
    let untrusted_sha = digest(untrusted_bytes);
    assert_ne!(untrusted_sha, selection.selected_sha);
    let untrusted = hosted_artifact(&temp.0.join("untrusted"), untrusted_bytes, &untrusted_sha);
    let rejected = install_for_lock(
        &data,
        &untrusted,
        &selection.lock,
        selection.platform,
        selection.variant,
    )
    .unwrap_err()
    .to_string();
    assert!(rejected.contains("selected record"), "{rejected}");
    assert!(!io::version_dir(&data, &untrusted_sha).exists());
    plant_consistent(&data, &untrusted_sha, untrusted_bytes);
    let lookup = installed_for_lock(
        &data,
        &untrusted_sha,
        &selection.lock,
        selection.platform,
        selection.variant,
    )
    .unwrap_err()
    .to_string();
    assert!(lookup.contains("selected record"), "{lookup}");

    for (bytes, sha) in [
        (selection.alternate_bytes, selection.alternate_sha.as_str()),
        (selection.other_bytes, selection.other_platform_sha.as_str()),
    ] {
        let alternate = hosted_artifact(&temp.0.join(sha), bytes, sha);
        let error = install_for_lock(
            &data,
            &alternate,
            &selection.lock,
            selection.platform,
            selection.variant,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("selected record"), "{error}");
        assert!(!io::version_dir(&data, sha).exists());
        plant_consistent(&data, sha, bytes);
        let found = installed_for_lock(
            &data,
            sha,
            &selection.lock,
            selection.platform,
            selection.variant,
        )
        .unwrap_err()
        .to_string();
        assert!(found.contains("selected record"), "{found}");
    }

    let mut placeholder = artifact.clone();
    placeholder.sha256 = PINNED_SHA256.to_string();
    let error = install_for_lock(
        &data,
        &placeholder,
        &selection.lock,
        selection.platform,
        selection.variant,
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("placeholder"), "{error}");
    plant_consistent(&data, PINNED_SHA256, b"present");
    let planted = installed_for_lock(
        &data,
        PINNED_SHA256,
        &selection.lock,
        selection.platform,
        selection.variant,
    )
    .unwrap_err()
    .to_string();
    assert!(planted.contains("placeholder"), "{planted}");
}

#[test]
fn missing_invalid_and_duplicate_locks_do_not_install() {
    let selection = synthetic_selection().expect("compile platform");
    let wire = variant_wire(selection.variant);

    let mut missing = serde_json::from_slice::<Value>(&selection.lock).unwrap();
    missing["artifacts"].as_array_mut().unwrap().retain(|item| {
        item["os"].as_str() != Some(selection.platform.os) || item["variant"].as_str() != Some(wire)
    });
    reject_lock("missing-record", &missing, "missing", &selection);

    let mut duplicated = serde_json::from_slice::<Value>(&selection.lock).unwrap();
    let extra = duplicated["artifacts"][0].clone();
    duplicated["artifacts"].as_array_mut().unwrap().push(extra);
    reject_lock("duplicate-record", &duplicated, "duplicated", &selection);

    let mut invalid = serde_json::from_slice::<Value>(&selection.lock).unwrap();
    invalid["schemaVersion"] = json!(2);
    reject_lock("invalid-schema", &invalid, "schema is not 1", &selection);

    let mut broken = serde_json::from_slice::<Value>(&selection.lock).unwrap();
    broken["artifacts"]
        .as_array_mut()
        .unwrap()
        .get_mut(0)
        .unwrap()
        .as_object_mut()
        .unwrap()
        .remove("sha256");
    reject_lock("invalid-record", &broken, "record is invalid", &selection);

    let mut placeholder = serde_json::from_slice::<Value>(&selection.lock).unwrap();
    placeholder["artifacts"][0]["sha256"] = json!(PINNED_SHA256);
    reject_lock(
        "placeholder-record",
        &placeholder,
        "placeholder",
        &selection,
    );

    let temp = Temp::new("empty-lock");
    let data = temp.0.join("data");
    let artifact = hosted_artifact(
        &temp.0.join("src"),
        selection.selected_bytes,
        &selection.selected_sha,
    );
    let error = install_for_lock(
        &data,
        &artifact,
        b" \n\t",
        selection.platform,
        selection.variant,
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("empty"), "{error}");
    assert!(!io::version_dir(&data, &selection.selected_sha).exists());
    plant_consistent(&data, &selection.selected_sha, selection.selected_bytes);
    let lookup = installed_for_lock(
        &data,
        &selection.selected_sha,
        b" \n\t",
        selection.platform,
        selection.variant,
    )
    .unwrap_err()
    .to_string();
    assert!(lookup.contains("empty"), "{lookup}");
}

fn reject_lock(label: &str, lock: &Value, needle: &str, selection: &SyntheticSelection) {
    let temp = Temp::new(label);
    let data = temp.0.join("data");
    let bytes = serde_json::to_vec(lock).unwrap();
    let artifact = hosted_artifact(
        &temp.0.join("src"),
        selection.selected_bytes,
        &selection.selected_sha,
    );
    let error = install_for_lock(
        &data,
        &artifact,
        &bytes,
        selection.platform,
        selection.variant,
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains(needle), "{error}");
    assert!(!io::version_dir(&data, &selection.selected_sha).exists());
    plant_consistent(&data, &selection.selected_sha, selection.selected_bytes);
    let lookup = installed_for_lock(
        &data,
        &selection.selected_sha,
        &bytes,
        selection.platform,
        selection.variant,
    )
    .unwrap_err()
    .to_string();
    assert!(lookup.contains(needle), "{lookup}");
}

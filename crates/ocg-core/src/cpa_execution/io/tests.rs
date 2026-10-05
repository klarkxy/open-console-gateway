use super::*;
use std::io::Cursor;
use std::path::{Component, Path, PathBuf};

fn digest(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(bytes))
}

struct Temp(PathBuf);

impl Temp {
    fn new(label: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("ocg-cpa-io-{label}-{}", uuid::Uuid::new_v4()));
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

#[cfg(windows)]
#[test]
fn private_file_permissions_cover_a_path_past_max_path() {
    let temp = Temp::new("long-private");
    let mut dir = temp.0.clone();
    while dir.as_os_str().len() < 250 {
        dir.push("segment");
    }
    let file = dir.join("secret.txt");
    let extended = format!(r"\\?\{}", file.display());
    std::fs::create_dir_all(format!(r"\\?\{}", dir.display())).expect("long directory");
    std::fs::write(&extended, b"secret").expect("long file");
    set_private_file(&file).expect("private DACL on a path past MAX_PATH");
    assert_eq!(std::fs::read(&extended).unwrap(), b"secret");
}

#[test]
fn reparse_attribute_bit_is_rejected() {
    assert!(is_reparse_attribute(FILE_ATTRIBUTE_REPARSE_POINT));
    assert!(is_reparse_attribute(FILE_ATTRIBUTE_REPARSE_POINT | 0x20));
    assert!(!is_reparse_attribute(0x20));
    assert!(!is_reparse_attribute(0));
}

#[test]
fn version_dir_keeps_a_digest_inside_versions() {
    let temp = Temp::new("version");
    let sha = "ab".repeat(32);
    let path = version_dir(&temp.0, &sha);
    assert_eq!(
        path.file_name().and_then(|name| name.to_str()),
        Some(sha.as_str())
    );
    let rejected = version_dir(&temp.0, "../escape");
    assert_eq!(
        rejected.file_name().and_then(|name| name.to_str()),
        Some(".rejected-digest")
    );
    assert!(
        !rejected
            .components()
            .any(|component| matches!(component, Component::ParentDir))
    );
}

#[test]
fn streamed_copy_stops_above_the_caller_cap() {
    let mut input = Cursor::new(b"12345");
    let mut output = Vec::new();
    let error = copy_hashed(&mut input, &mut output, 4).unwrap_err();
    assert!(error.to_string().contains("too large"), "{error}");
    assert!(MAX_EXECUTABLE_BYTES > MAX_CONFIG_BYTES as u64);
}

#[test]
fn copy_rehashes_regular_bytes_and_rejects_the_wrong_cases() {
    let temp = Temp::new("copy");
    // The destination directory is made private. A source inside it loses
    // inherited access on Windows, so the fixture source stays outside.
    let source_dir = Temp::new("copy-source");
    let bytes = b"copied-host-bytes";
    let sha = digest(bytes);
    let source = source_dir.0.join("ocg-cpa-host.exe");
    let destination = temp.0.join("out.bin");
    std::fs::write(&source, bytes).unwrap();
    copy_verified(&source, &destination, &sha).expect("synthetic copy");
    assert_eq!(std::fs::read(&destination).unwrap(), bytes);
    confirm_installed_executable(&destination, &sha).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::symlink_metadata(&destination)
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o700);
    }

    std::fs::write(&destination, b"changed-host-bytes").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&destination, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let error = confirm_installed_executable(&destination, &sha)
        .unwrap_err()
        .to_string();
    assert!(error.contains("hash"), "{error}");

    let mismatch = temp.0.join("mismatch.bin");
    let error = copy_verified(&source, &mismatch, &"ab".repeat(32))
        .unwrap_err()
        .to_string();
    assert!(error.contains("hash"), "{error}");
    assert!(!mismatch.exists());

    let error = copy_verified(&source, &temp.0.join("ignored.bin"), "not-a-digest")
        .unwrap_err()
        .to_string();
    assert!(error.contains("invalid"), "{error}");

    let directory = temp.0.join("nested");
    std::fs::create_dir(&directory).unwrap();
    let error = copy_verified(&directory, &temp.0.join("from-dir.bin"), &sha)
        .unwrap_err()
        .to_string();
    assert!(error.contains("regular"), "{error}");

    let escaped = temp.0.join("..").join("ocg-outside.exe");
    let error = copy_verified(&escaped, &temp.0.join("escaped.bin"), &sha)
        .unwrap_err()
        .to_string();
    assert!(error.contains("unsafe"), "{error}");
}

#[test]
fn symlink_source_is_rejected_when_the_process_can_create_one() {
    let temp = Temp::new("link");
    let bytes = b"link-target-bytes";
    let sha = digest(bytes);
    let source = temp.0.join("target.bin");
    std::fs::write(&source, bytes).unwrap();
    let link = temp.0.join("link.bin");
    match symlink_file(&source, &link) {
        Ok(()) => {
            let error = copy_verified(&link, &temp.0.join("from-link.bin"), &sha)
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

#[cfg(unix)]
#[test]
fn world_writable_source_and_loose_install_mode_are_rejected() {
    use std::os::unix::fs::PermissionsExt;
    let temp = Temp::new("mode");
    let bytes = b"mode-host-bytes";
    let sha = digest(bytes);
    let source = temp.0.join("source.bin");
    std::fs::write(&source, bytes).unwrap();
    std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o666)).unwrap();
    let error = copy_verified(&source, &temp.0.join("out.bin"), &sha)
        .unwrap_err()
        .to_string();
    assert!(error.contains("permissions"), "{error}");

    std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o644)).unwrap();
    let destination = temp.0.join("installed.bin");
    copy_verified(&source, &destination, &sha).unwrap();
    std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o755)).unwrap();
    let public_exec = temp.0.join("public-exec.bin");
    copy_verified(&source, &public_exec, &sha).unwrap();
    let installed_mode = std::fs::symlink_metadata(&public_exec)
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(installed_mode, 0o700);
    std::fs::set_permissions(&destination, std::fs::Permissions::from_mode(0o644)).unwrap();
    let error = confirm_installed_executable(&destination, &sha)
        .unwrap_err()
        .to_string();
    assert!(error.contains("permissions"), "{error}");
}

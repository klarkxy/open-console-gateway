//! Bounded reads and private atomic writes for BYOK targets and receipts.
use super::{ByokError, ByokResult};
use crate::dsh_application::{DshApplicationError, DshApplicationErrorKind};
use crate::dsh_application_host::{is_link_or_reparse, replace_file, sync_parent};
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Read, Write};
use std::path::{Path, PathBuf};

// Full native catalogs repeat model instructions (Codex ~20 KiB per model).
// Keep I/O bounded without the old ~190-model effective catalog ceiling.
pub const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;

pub fn canonical_lexical_path(path: &Path) -> ByokResult<PathBuf> {
    if !path.is_absolute() {
        return Err(ByokError::invalid("BYOK target path must be absolute"));
    }
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if !normalized.pop() {
                    return Err(ByokError::invalid("BYOK target path escapes its root"));
                }
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    Ok(normalized)
}

pub fn ensure_safe_directory_chain(target: &Path) -> ByokResult<()> {
    let target = canonical_lexical_path(target)?;
    let mut missing = Vec::new();
    let mut cursor = target.clone();
    let mut found_existing = false;
    loop {
        match fs::symlink_metadata(&cursor) {
            Ok(metadata) => {
                if is_link_or_reparse(&cursor) {
                    return Err(ByokError::conflict(
                        "BYOK directory contains a link or reparse ancestor",
                    ));
                }
                if !metadata.file_type().is_dir() {
                    return Err(ByokError::conflict(
                        "BYOK directory contains a non-directory ancestor",
                    ));
                }
                found_existing = true;
            }
            Err(error) if error.kind() == ErrorKind::NotFound => {
                missing.push(cursor.clone());
            }
            Err(error) => return Err(io_internal(error)),
        }
        match cursor.parent() {
            Some(parent) if parent != cursor => cursor = parent.to_path_buf(),
            _ => break,
        }
    }
    if !found_existing {
        return Err(ByokError::invalid(
            "BYOK directory has no existing ancestor",
        ));
    }
    missing.reverse();
    for current in missing {
        fs::create_dir(&current)
            .map_err(|error| ByokError::internal(format!("create_dir: {error}")))?;
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_dir() && !is_link_or_reparse(&current) => {}
            _ => {
                return Err(ByokError::conflict(
                    "BYOK directory contains a link or non-directory ancestor",
                ));
            }
        }
    }
    Ok(())
}

pub fn reject_symlink_ancestors(path: &Path) -> ByokResult<()> {
    let path = canonical_lexical_path(path)?;
    let mut cursor = path.parent().map(|parent| parent.to_path_buf());
    while let Some(dir) = cursor {
        match fs::symlink_metadata(&dir) {
            Ok(metadata) => {
                if is_link_or_reparse(&dir) {
                    return Err(ByokError::conflict(
                        "BYOK path has a link or reparse ancestor",
                    ));
                }
                if !metadata.file_type().is_dir() {
                    return Err(ByokError::conflict(
                        "BYOK path has a non-directory ancestor",
                    ));
                }
            }
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(io_internal(error)),
        }
        cursor = match dir.parent() {
            Some(parent) if parent != dir.as_path() => Some(parent.to_path_buf()),
            _ => None,
        };
    }
    Ok(())
}

fn unreachable_path(error: &std::io::Error) -> bool {
    error.raw_os_error() == Some(20) || error.kind() == ErrorKind::NotADirectory
}

pub fn reject_if_link_or_non_file(path: &Path) -> ByokResult<()> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(io_internal(error)),
        Ok(metadata) => {
            if !metadata.file_type().is_file() || is_link_or_reparse(path) {
                Err(ByokError::conflict("BYOK path is not a regular file"))
            } else {
                Ok(())
            }
        }
    }
}

pub fn read_regular_file(path: &Path) -> ByokResult<Option<Vec<u8>>> {
    match fs::symlink_metadata(path) {
        // NotADirectory covers a catalog whose parent exists as a file: on
        // Linux that stat returns ENOTDIR rather than ENOENT, and the catalog
        // is simply absent either way.
        Err(error) if error.kind() == ErrorKind::NotFound || unreachable_path(&error) => {
            reject_symlink_ancestors(path)?;
            return Ok(None);
        }
        Err(error) => return Err(io_internal(error)),
        Ok(metadata) => {
            if !metadata.file_type().is_file() || is_link_or_reparse(path) {
                return Err(ByokError::conflict("BYOK path is not a regular file"));
            }
            if metadata.len() > MAX_FILE_BYTES {
                return Err(ByokError::invalid(
                    "Configuration file exceeds the 64 MiB size bound",
                ));
            }
        }
    }
    reject_symlink_ancestors(path)?;
    let file = open_nofollow_read(path)?;
    reject_if_link_or_non_file(path)?;
    let mut bytes = Vec::new();
    file.take(MAX_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(io_internal)?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(ByokError::invalid(
            "Configuration file exceeds the 64 MiB size bound",
        ));
    }
    Ok(Some(bytes))
}

pub fn write_private_atomic(destination: &Path, bytes: &[u8]) -> ByokResult<()> {
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(ByokError::invalid(
            "Configuration file exceeds the 64 MiB size bound",
        ));
    }
    let parent = destination
        .parent()
        .ok_or_else(|| ByokError::internal("BYOK file has no parent"))?;
    ensure_safe_directory_chain(parent)?;
    reject_if_link_or_non_file(destination)?;
    let temporary = parent.join(format!(".ocg-byok-{}.tmp", uuid::Uuid::new_v4().simple()));
    struct TempGuard(PathBuf);
    impl Drop for TempGuard {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.0);
        }
    }
    let guard = TempGuard(temporary.clone());
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&temporary)
        .map_err(|error| ByokError::internal(format!("open temp: {error}")))?;
    #[cfg(windows)]
    crate::dsh_application_host::set_private_permissions(&temporary).map_err(map_dsh)?;
    file.write_all(bytes)
        .and_then(|_| file.sync_all())
        .map_err(|error| ByokError::internal(format!("write temp: {error}")))?;
    drop(file);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&temporary, fs::Permissions::from_mode(0o600))
            .map_err(|error| ByokError::internal(format!("chmod temp: {error}")))?;
    }
    #[cfg(windows)]
    if destination.exists() {
        crate::dsh_application_host::set_private_permissions(destination).map_err(map_dsh)?;
    }
    replace_file(&temporary, destination)
        .map_err(|error| ByokError::internal(format!("replace: {}", error.message)))?;
    std::mem::forget(guard);
    #[cfg(windows)]
    crate::dsh_application_host::set_private_permissions(destination).map_err(map_dsh)?;
    sync_parent(destination).map_err(map_dsh)
}

pub fn remove_regular_file(path: &Path) -> ByokResult<()> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(io_internal(error)),
        Ok(metadata) => {
            if !metadata.file_type().is_file() || is_link_or_reparse(path) {
                return Err(ByokError::conflict("BYOK path is not a regular file"));
            }
            reject_symlink_ancestors(path)?;
            fs::remove_file(path).map_err(io_internal)
        }
    }
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

pub fn content_hash(bytes: Option<&[u8]>) -> String {
    match bytes {
        None => "absent".into(),
        Some(bytes) => sha256_hex(bytes),
    }
}

pub fn io_internal(error: std::io::Error) -> ByokError {
    ByokError::internal(error.to_string())
}

pub fn map_dsh(error: DshApplicationError) -> ByokError {
    match error.kind {
        DshApplicationErrorKind::Invalid => ByokError::invalid(error.message),
        DshApplicationErrorKind::Precondition => ByokError::precondition(error.message),
        DshApplicationErrorKind::Conflict => ByokError::conflict(error.message),
        DshApplicationErrorKind::Internal => ByokError::internal(error.message),
    }
}

fn open_nofollow_read(path: &Path) -> ByokResult<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(nix::libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    options.open(path).map_err(io_internal)
}

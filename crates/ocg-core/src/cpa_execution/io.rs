//! Private files and local process helpers for the owned CPA runtime.
//!
//! Paths stay on the data directory. Nothing here contacts a provider.

use super::ExecutionError;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Component, Path, PathBuf};

pub(super) const MAX_CONFIG_BYTES: usize = 1024 * 1024;
pub(super) const MAX_EXECUTABLE_BYTES: u64 = 128 * 1024 * 1024;

pub(super) fn runtime_root(data_dir: &Path) -> PathBuf {
    data_dir.join("cpa")
}

pub(super) fn config_path(data_dir: &Path) -> PathBuf {
    runtime_root(data_dir).join("config.yaml")
}

pub(super) fn previous_config_path(data_dir: &Path) -> PathBuf {
    runtime_root(data_dir).join("config.yaml.previous")
}

pub(super) fn auth_dir(data_dir: &Path) -> PathBuf {
    runtime_root(data_dir).join("auth")
}

pub(super) fn private_dir(data_dir: &Path) -> PathBuf {
    runtime_root(data_dir).join("private")
}

pub(super) fn version_dir(data_dir: &Path, sha: &str) -> PathBuf {
    let segment = if super::lowercase_hex_64(sha) {
        sha
    } else {
        ".rejected-digest"
    };
    runtime_root(data_dir).join("versions").join(segment)
}

pub(super) fn executable_name() -> &'static str {
    if cfg!(windows) {
        "ocg-cpa-host.exe"
    } else {
        "ocg-cpa-host"
    }
}

pub(super) fn portable_auth_dir(data_dir: &Path) -> Result<String, ExecutionError> {
    let absolute = if data_dir.is_absolute() {
        data_dir.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|_| ExecutionError::Invalid("current directory is unavailable".into()))?
            .join(data_dir)
    };
    if absolute
        .components()
        .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
    {
        return Err(ExecutionError::Invalid(
            "data directory must not contain . or ..".into(),
        ));
    }
    let mut text = absolute.to_string_lossy().replace('\\', "/");
    while text.len() > 1 && text.ends_with('/') {
        text.pop();
    }
    Ok(format!("{text}/cpa/auth"))
}

pub(super) fn ensure_dir(path: &Path) -> Result<(), ExecutionError> {
    fs::create_dir_all(path)
        .map_err(|_| ExecutionError::Invalid("CPA directory could not be created".into()))?;
    // The replacement DACL is protected and not inheritable. Children that
    // only had an inherited ACE would become unreadable to their owner.
    // Stamp the same private DACL onto existing children first.
    if let Ok(entries) = fs::read_dir(path) {
        for entry in entries.flatten() {
            let child = entry.path();
            let Ok(metadata) = fs::symlink_metadata(&child) else {
                continue;
            };
            if is_reparse_metadata(&metadata) {
                continue;
            }
            if metadata.is_dir() {
                set_private_dir(&child)?;
            } else if metadata.is_file() {
                set_private_file(&child)?;
            }
        }
    }
    set_private_dir(path)
}

pub(super) fn reserve_loopback_port(preferred: u16) -> Result<u16, ExecutionError> {
    if preferred != 0 && bind_ok(preferred) {
        return Ok(preferred);
    }
    let listener = TcpListener::bind("127.0.0.1:0")
        .map_err(|_| ExecutionError::Unavailable("could not reserve a loopback port".into()))?;
    let port = listener
        .local_addr()
        .map_err(|_| ExecutionError::Unavailable("could not read the reserved port".into()))?
        .port();
    drop(listener);
    Ok(port)
}

fn bind_ok(port: u16) -> bool {
    TcpListener::bind(("127.0.0.1", port)).is_ok()
}

pub(super) fn read_bounded(path: &Path) -> Result<Option<Vec<u8>>, ExecutionError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => {
            return Err(ExecutionError::Invalid(
                "CPA config could not be read".into(),
            ));
        }
    };
    if is_reparse_metadata(&metadata) || !metadata.is_file() {
        return Err(ExecutionError::Invalid(
            "CPA config must be a regular file".into(),
        ));
    }
    if metadata.len() > MAX_CONFIG_BYTES as u64 {
        return Err(ExecutionError::Invalid(
            "CPA config is larger than 1 MiB".into(),
        ));
    }
    let mut file = File::open(path)
        .map_err(|_| ExecutionError::Invalid("CPA config could not be read".into()))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|_| ExecutionError::Invalid("CPA config could not be read".into()))?;
    if bytes.len() > MAX_CONFIG_BYTES {
        return Err(ExecutionError::Invalid(
            "CPA config is larger than 1 MiB".into(),
        ));
    }
    Ok(Some(bytes))
}

pub(super) fn atomic_write_private(destination: &Path, bytes: &[u8]) -> Result<(), ExecutionError> {
    if bytes.len() > MAX_CONFIG_BYTES {
        return Err(ExecutionError::Invalid(
            "CPA config is larger than 1 MiB".into(),
        ));
    }
    let parent = destination
        .parent()
        .ok_or_else(|| ExecutionError::Invalid("CPA config path has no parent".into()))?;
    ensure_dir(parent)?;
    let temp_name = format!(
        ".{}.tmp-{}",
        destination
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("cpa"),
        uuid::Uuid::new_v4()
    );
    let temp = parent.join(temp_name);
    let write_result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)
            .map_err(|_| {
                ExecutionError::Invalid("CPA config temp file could not be created".into())
            })?;
        file.write_all(bytes)
            .map_err(|_| ExecutionError::Invalid("CPA config could not be written".into()))?;
        file.sync_all()
            .map_err(|_| ExecutionError::Invalid("CPA config could not be synced".into()))?;
        drop(file);
        set_private_file(&temp)?;
        replace_file(&temp, destination)
    })();
    if write_result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    write_result
}

pub(super) fn set_private_file(path: &Path) -> Result<(), ExecutionError> {
    #[cfg(windows)]
    {
        crate::private_file::set_private_permissions(path).map_err(|_| {
            ExecutionError::Invalid("private file permissions could not be set".into())
        })
    }
    #[cfg(not(windows))]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).map_err(|_| {
            ExecutionError::Invalid("private file permissions could not be set".into())
        })
    }
}

pub(super) fn set_private_dir(path: &Path) -> Result<(), ExecutionError> {
    #[cfg(windows)]
    {
        crate::private_file::set_private_permissions(path).map_err(|_| {
            ExecutionError::Invalid("private directory permissions could not be set".into())
        })
    }
    #[cfg(not(windows))]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|_| {
            ExecutionError::Invalid("private directory permissions could not be set".into())
        })
    }
}

#[cfg(windows)]
fn replace_file(source: &Path, destination: &Path) -> Result<(), ExecutionError> {
    use std::os::windows::ffi::OsStrExt;
    unsafe extern "system" {
        fn ReplaceFileW(
            replaced: *const u16,
            replacement: *const u16,
            backup: *const u16,
            flags: u32,
            exclude: *mut std::ffi::c_void,
            reserved: *mut std::ffi::c_void,
        ) -> i32;
        fn MoveFileExW(existing: *const u16, new: *const u16, flags: u32) -> i32;
    }
    const REPLACEFILE_WRITE_THROUGH: u32 = 0x0000_0001;
    const MOVEFILE_WRITE_THROUGH: u32 = 0x0000_0008;
    let wide = |path: &Path| {
        path.as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect::<Vec<_>>()
    };
    let source = wide(source);
    let destination_wide = wide(destination);
    let replaced = unsafe {
        if destination.exists() {
            ReplaceFileW(
                destination_wide.as_ptr(),
                source.as_ptr(),
                std::ptr::null(),
                REPLACEFILE_WRITE_THROUGH,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        } else {
            MoveFileExW(
                source.as_ptr(),
                destination_wide.as_ptr(),
                MOVEFILE_WRITE_THROUGH,
            )
        }
    };
    if replaced == 0 {
        Err(ExecutionError::Invalid(
            "CPA config could not be replaced".into(),
        ))
    } else {
        Ok(())
    }
}

#[cfg(not(windows))]
fn replace_file(source: &Path, destination: &Path) -> Result<(), ExecutionError> {
    fs::rename(source, destination)
        .map_err(|_| ExecutionError::Invalid("CPA config could not be replaced".into()))
}

pub(super) fn copy_verified(
    source: &Path,
    destination: &Path,
    expected_sha: &str,
) -> Result<(), ExecutionError> {
    if !super::lowercase_hex_64(expected_sha) {
        return Err(ExecutionError::Invalid(
            "pinned CPA selected digest is invalid".into(),
        ));
    }
    if path_rejected(source) || path_rejected(destination) {
        return Err(ExecutionError::Invalid(
            "pinned CPA executable path is unsafe".into(),
        ));
    }
    // Read the pinned source before the destination directory is made private.
    // On Windows that DACL replace drops inherited access on a sibling source.
    let mut input = open_regular_source(source)?;
    let parent = destination
        .parent()
        .ok_or_else(|| ExecutionError::Invalid("artifact path has no parent".into()))?;
    ensure_dir(parent)?;
    let temp = parent.join(format!(
        ".{}.tmp-{}",
        executable_name(),
        uuid::Uuid::new_v4()
    ));
    let result = (|| {
        let actual = {
            let mut output = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp)
                .map_err(|_| {
                    ExecutionError::Invalid("pinned CPA executable could not be copied".into())
                })?;
            let actual = copy_hashed(&mut input, &mut output, MAX_EXECUTABLE_BYTES)?;
            output.sync_all().map_err(|_| {
                ExecutionError::Invalid("pinned CPA executable could not be synced".into())
            })?;
            actual
        };
        if actual != expected_sha {
            return Err(ExecutionError::Invalid(
                "pinned CPA executable hash does not match the selected record".into(),
            ));
        }
        set_installed_permissions(&temp)?;
        replace_file(&temp, destination)?;
        if let Err(error) = confirm_installed_executable(destination, expected_sha) {
            let _ = fs::remove_file(destination);
            return Err(error);
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

pub(super) const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;

pub(super) fn is_reparse_attribute(attributes: u32) -> bool {
    attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

pub(super) fn is_reparse_metadata(metadata: &fs::Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        return is_reparse_attribute(metadata.file_attributes());
    }
    #[cfg(not(windows))]
    {
        false
    }
}

pub(super) fn safe_executable_name(name: &str) -> bool {
    if name.is_empty() || name.len() > 128 || !name.is_ascii() {
        return false;
    }
    let bytes = name.as_bytes();
    if bytes[0] == b'.' || bytes[0] == b'-' || name.ends_with('.') || name.ends_with('-') {
        return false;
    }
    if name.contains("..")
        || !bytes
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return false;
    }
    let stem = name.split('.').next().unwrap_or(name);
    let stem = stem.to_ascii_uppercase();
    const RESERVED: &[&str] = &[
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
        "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];
    !RESERVED.contains(&stem.as_str())
}

enum FileShape {
    Reparse,
    NotRegular,
    Regular(fs::Metadata),
}

fn file_shape(path: &Path) -> Option<FileShape> {
    let metadata = fs::symlink_metadata(path).ok()?;
    if is_reparse_metadata(&metadata) {
        return Some(FileShape::Reparse);
    }
    if !metadata.is_file() {
        return Some(FileShape::NotRegular);
    }
    Some(FileShape::Regular(metadata))
}

pub(super) fn read_manifest_bytes(path: &Path) -> Result<Vec<u8>, ExecutionError> {
    let metadata = match file_shape(path) {
        Some(FileShape::Regular(metadata)) => metadata,
        Some(FileShape::Reparse) => {
            return Err(ExecutionError::Invalid(
                "pinned CPA manifest is a reparse point".into(),
            ));
        }
        Some(FileShape::NotRegular) => {
            return Err(ExecutionError::Invalid(
                "pinned CPA manifest is not a regular file".into(),
            ));
        }
        None => {
            return Err(ExecutionError::Invalid(
                "pinned CPA manifest could not be read".into(),
            ));
        }
    };
    if metadata.len() > MAX_CONFIG_BYTES as u64 {
        return Err(ExecutionError::Invalid(
            "pinned CPA manifest is too large".into(),
        ));
    }
    if !source_permissions_ok(&metadata) {
        return Err(ExecutionError::Invalid(
            "pinned CPA manifest permissions are not private".into(),
        ));
    }
    let mut file = open_no_follow(
        path,
        "pinned CPA manifest could not be read",
        "pinned CPA manifest is a reparse point",
    )?;
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(MAX_CONFIG_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| ExecutionError::Invalid("pinned CPA manifest could not be read".into()))?;
    if bytes.len() > MAX_CONFIG_BYTES {
        return Err(ExecutionError::Invalid(
            "pinned CPA manifest is too large".into(),
        ));
    }
    Ok(bytes)
}

pub(super) fn hash_regular_executable(path: &Path) -> Result<String, ExecutionError> {
    let metadata = match file_shape(path) {
        Some(FileShape::Regular(metadata)) => metadata,
        Some(FileShape::Reparse) => {
            return Err(ExecutionError::Invalid(
                "pinned CPA executable is a reparse point".into(),
            ));
        }
        Some(FileShape::NotRegular) => {
            return Err(ExecutionError::Invalid(
                "pinned CPA executable is not a regular file".into(),
            ));
        }
        None => {
            return Err(ExecutionError::Invalid(
                "pinned CPA executable could not be read".into(),
            ));
        }
    };
    if metadata.len() > MAX_EXECUTABLE_BYTES {
        return Err(ExecutionError::Invalid(
            "pinned CPA executable is too large".into(),
        ));
    }
    if !source_permissions_ok(&metadata) {
        return Err(ExecutionError::Invalid(
            "pinned CPA executable permissions are not private".into(),
        ));
    }
    let mut file = open_no_follow(
        path,
        "pinned CPA executable could not be read",
        "pinned CPA executable is a reparse point",
    )?;
    hash_reader(&mut file, MAX_EXECUTABLE_BYTES)
}

pub(super) fn confirm_installed_executable(
    path: &Path,
    expected_sha: &str,
) -> Result<(), ExecutionError> {
    if !super::lowercase_hex_64(expected_sha) {
        return Err(ExecutionError::Invalid(
            "pinned CPA selected digest is invalid".into(),
        ));
    }
    let metadata = match file_shape(path) {
        Some(FileShape::Regular(metadata)) => metadata,
        Some(FileShape::Reparse) => {
            return Err(ExecutionError::Invalid(
                "pinned CPA executable is a reparse point".into(),
            ));
        }
        Some(FileShape::NotRegular) => {
            return Err(ExecutionError::Invalid(
                "pinned CPA executable is not a regular file".into(),
            ));
        }
        None => {
            return Err(ExecutionError::Invalid(
                "pinned CPA executable could not be read".into(),
            ));
        }
    };
    if !installed_permissions_ok(&metadata) {
        return Err(ExecutionError::Invalid(
            "pinned CPA executable permissions are not private".into(),
        ));
    }
    let mut file = open_no_follow(
        path,
        "pinned CPA executable could not be read",
        "pinned CPA executable is a reparse point",
    )?;
    let actual = hash_reader(&mut file, MAX_EXECUTABLE_BYTES)?;
    if actual != expected_sha {
        return Err(ExecutionError::Invalid(
            "pinned CPA executable hash does not match the selected record".into(),
        ));
    }
    Ok(())
}

fn source_permissions_ok(metadata: &fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = metadata.permissions().mode();
        return mode & 0o400 != 0 && mode & 0o002 == 0;
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        true
    }
}

fn installed_permissions_ok(metadata: &fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        return metadata.permissions().mode() & 0o777 == 0o700;
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        true
    }
}

fn set_installed_permissions(path: &Path) -> Result<(), ExecutionError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|_| {
            ExecutionError::Invalid("pinned CPA executable permissions are not private".into())
        })?;
    }
    #[cfg(windows)]
    {
        set_private_file(path)?;
    }
    Ok(())
}

fn open_regular_source(path: &Path) -> Result<File, ExecutionError> {
    let metadata = match file_shape(path) {
        Some(FileShape::Regular(metadata)) => metadata,
        Some(FileShape::Reparse) => {
            return Err(ExecutionError::Invalid(
                "pinned CPA executable is a reparse point".into(),
            ));
        }
        Some(FileShape::NotRegular) => {
            return Err(ExecutionError::Invalid(
                "pinned CPA executable is not a regular file".into(),
            ));
        }
        None => {
            return Err(ExecutionError::Invalid(
                "pinned CPA executable could not be read".into(),
            ));
        }
    };
    if metadata.len() > MAX_EXECUTABLE_BYTES {
        return Err(ExecutionError::Invalid(
            "pinned CPA executable is too large".into(),
        ));
    }
    if !source_permissions_ok(&metadata) {
        return Err(ExecutionError::Invalid(
            "pinned CPA executable permissions are not private".into(),
        ));
    }
    open_no_follow(
        path,
        "pinned CPA executable could not be read",
        "pinned CPA executable is a reparse point",
    )
}

fn open_no_follow(
    path: &Path,
    unreadable: &'static str,
    reparse: &'static str,
) -> Result<File, ExecutionError> {
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        let _ = reparse;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        return OpenOptions::new()
            .read(true)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)
            .map_err(|_| ExecutionError::Invalid(unreadable.into()));
    }
    #[cfg(not(windows))]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let flags = if cfg!(target_os = "macos") {
            0x0100
        } else {
            0x20000
        };
        OpenOptions::new()
            .read(true)
            .custom_flags(flags)
            .open(path)
            .map_err(|error| {
                if error.kind() == std::io::ErrorKind::FilesystemLoop {
                    ExecutionError::Invalid(reparse.into())
                } else {
                    ExecutionError::Invalid(unreadable.into())
                }
            })
    }
}

fn path_rejected(path: &Path) -> bool {
    if path.as_os_str().is_empty() || path.as_os_str().len() > 4096 {
        return true;
    }
    if path
        .components()
        .any(|component| matches!(component, Component::ParentDir | Component::CurDir))
    {
        return true;
    }
    match path.file_name().and_then(|name| name.to_str()) {
        Some(name) => !safe_executable_name(name),
        None => true,
    }
}

fn copy_hashed<R: Read, W: Write>(
    input: &mut R,
    output: &mut W,
    max_bytes: u64,
) -> Result<String, ExecutionError> {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    let mut total = 0u64;
    loop {
        let read = input.read(&mut buffer).map_err(|_| {
            ExecutionError::Invalid("pinned CPA executable could not be read".into())
        })?;
        if read == 0 {
            break;
        }
        total = total.saturating_add(read as u64);
        if total > max_bytes {
            return Err(ExecutionError::Invalid(
                "pinned CPA executable is too large".into(),
            ));
        }
        hasher.update(&buffer[..read]);
        output.write_all(&buffer[..read]).map_err(|_| {
            ExecutionError::Invalid("pinned CPA executable could not be copied".into())
        })?;
    }
    Ok(hex::encode(hasher.finalize()))
}

fn hash_reader<R: Read>(input: &mut R, max_bytes: u64) -> Result<String, ExecutionError> {
    let mut sink = std::io::sink();
    copy_hashed(input, &mut sink, max_bytes)
}

#[cfg(test)]
mod tests;

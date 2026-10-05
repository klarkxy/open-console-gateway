//! Offline whole-directory snapshot for a stopped data directory.
//!
//! Create and restore copy files and verify ciphertext on a read-only SQLite
//! copy. They do not open [`crate::db::Database`], migrate, repair rows, start
//! CPA, contact a provider, create an encryption key file, or stop another
//! process. Exclusive locks are try-locks on the existing sentinel inodes and
//! are never unlinked.

#[cfg(windows)]
use crate::crypto::MachineBoundCipher;
use crate::crypto::{KeyCipher, LOCAL_CIPHER_V2_PREFIX, StaticKeyCipher};
use anyhow::{Context, Result, bail};
use fs2::FileExt;
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

pub const FORMAT: &str = "ocg-directory-snapshot";
pub const FORMAT_VERSION: u32 = 1;

const MANIFEST_NAME: &str = "ocg-snapshot-manifest-v1.json";
const EMPTY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

/// Product sqlite can be much larger than a CPA release archive.
const MAX_ARCHIVE_BYTES: u64 = 4 * 1024 * 1024 * 1024;
const MAX_UNCOMPRESSED_BYTES: u64 = 4 * 1024 * 1024 * 1024;
const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const MAX_OWNED_CONFIG_BYTES: u64 = 1024 * 1024;
const MAX_KEY_FILE_BYTES: u64 = 4096;
const MAX_MANIFEST_BYTES: u64 = 32 * 1024 * 1024;
const CIPHER_WITNESS_PLAINTEXT: &str = "ocg-snapshot-cipher-witness-v1";
const MAX_ENTRIES: usize = 100_000;
const MAX_RELATIVE_BYTES: usize = 4096;
const MAX_DEPTH: usize = 128;

const EXCLUDED_ROOT_FILES: &[&str] = &[
    ".cli-serve.lock",
    ".database-open-gate.lock",
    ".database-open.lock",
    "cli-listener.json",
    "cli-listener.json.tmp",
];

const LOCK_FILES: &[&str] = &[
    ".database-open.lock",
    ".database-open-gate.lock",
    ".cli-serve.lock",
];

/// In-memory cipher selection for one snapshot. Debug redacts every secret.
///
/// Precedence is explicit, then env, then `.encryption-key`, then machine.
/// A key file on disk does not win when an override is set. This value does
/// not create a key file.
#[derive(Clone)]
pub struct CipherWitness {
    explicit: Option<String>,
    env: Option<String>,
    machine: Option<Arc<dyn KeyCipher + Send + Sync>>,
}

impl CipherWitness {
    pub fn none() -> Self {
        Self {
            explicit: None,
            env: None,
            machine: None,
        }
    }

    pub fn explicit(secret: impl Into<String>) -> Self {
        Self {
            explicit: Some(secret.into()),
            env: None,
            machine: None,
        }
    }

    pub fn from_sources(explicit: Option<String>, env: Option<String>) -> Self {
        Self {
            explicit,
            env,
            machine: None,
        }
    }

    /// Test stand-in for another machine. Used only when no explicit key, env
    /// key, or key file is present. Production restore leaves this empty and
    /// uses the host machine cipher.
    pub fn injected_machine(cipher: Arc<dyn KeyCipher + Send + Sync>) -> Self {
        Self {
            explicit: None,
            env: None,
            machine: Some(cipher),
        }
    }
}

impl std::fmt::Debug for CipherWitness {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CipherWitness")
            .field("explicit", &self.explicit.as_ref().map(|_| "[redacted]"))
            .field("env", &self.env.as_ref().map(|_| "[redacted]"))
            .field("machine", &self.machine.as_ref().map(|_| "injected"))
            .finish()
    }
}

/// Operator receipt. `to_json_line` emits only path, format, version, count, and state.
#[derive(Debug, Clone)]
pub struct SnapshotReceipt {
    pub path: PathBuf,
    pub format: &'static str,
    pub version: u32,
    pub count: u64,
    pub state: &'static str,
}

impl SnapshotReceipt {
    pub fn to_json_line(&self) -> String {
        let value = serde_json::json!({
            "path": self.path.display().to_string(),
            "format": self.format,
            "version": self.version,
            "count": self.count,
            "state": self.state,
        });
        format!("{value}\n")
    }
}

pub fn create_snapshot(
    source: &Path,
    output: &Path,
    witness: CipherWitness,
) -> Result<SnapshotReceipt> {
    let source_absolute = absolute_real_path(source)?;
    let output_absolute = absolute_real_path(output)?;
    validate_source_root(&source_absolute)?;
    validate_output_destination(&source_absolute, &output_absolute)?;
    let _locks = acquire_writer_locks(&source_absolute)?;
    let source_dir = portable_data_dir(&source_absolute);
    validate_owned_cpa(&source_absolute, &source_dir)?;
    let resolved = resolve_cipher(&source_absolute, &witness)?;
    let inspection = inspect_sqlite(&source_absolute, Some(resolved.cipher.as_ref()))?;
    let mut payloads = walk_tree(&source_absolute, WalkMode::Source)?;
    payloads.sort_by(|left, right| left.relative.cmp(&right.relative));
    ensure_tree_shape(&payloads)?;
    let identity = inspection.identity;
    let cipher = cipher_record(&resolved, inspection.legacy_unauthenticated)?;
    let entries = payload_entries(&payloads);
    let manifest = Manifest {
        format: FORMAT.to_string(),
        version: FORMAT_VERSION,
        app_version: env!("CARGO_PKG_VERSION").to_string(),
        sqlite_user_version: identity.as_ref().map(|item| item.user_version),
        schema_version: identity.as_ref().map(|item| item.schema_version),
        source_data_dir: source_dir,
        cipher,
        entries,
        content_hash: String::new(),
    };
    let mut manifest = manifest;
    manifest.content_hash = content_hash_of(&manifest)?;
    write_archive(&output_absolute, &payloads, &manifest)?;
    Ok(SnapshotReceipt {
        path: output.to_path_buf(),
        format: FORMAT,
        version: FORMAT_VERSION,
        count: payloads.len() as u64,
        state: "created",
    })
}

pub fn restore_snapshot(
    target: &Path,
    input: &Path,
    witness: CipherWitness,
) -> Result<SnapshotReceipt> {
    let target_absolute = absolute_real_path(target)?;
    let input_absolute = absolute_real_path(input)?;
    validate_input_archive(&input_absolute)?;
    validate_restore_target(&target_absolute)?;
    let parent = target_absolute
        .parent()
        .context("restore target has no parent")?;
    let stage_path = parent.join(format!(
        ".ocg-restore-stage-{}",
        uuid::Uuid::new_v4().simple()
    ));
    let mut stage = StageGuard::create(&stage_path)?;
    extract_archive(&input_absolute, stage.path())?;
    let manifest = load_manifest_file(&stage.path().join(MANIFEST_NAME))?;
    verify_manifest(&manifest)?;
    verify_stage_tree(stage.path(), &manifest)?;
    verify_restored_database(stage.path(), &manifest, &witness)?;
    verify_restore_source(&target_absolute, &manifest)?;
    remove_manifest_file(stage.path())?;
    relocate_owned_cpa(stage.path(), &manifest, &target_absolute)?;
    clear_runtime_markers(stage.path())?;
    validate_restore_target(&target_absolute)?;
    stage.publish(&target_absolute)?;
    Ok(SnapshotReceipt {
        path: target.to_path_buf(),
        format: FORMAT,
        version: FORMAT_VERSION,
        count: manifest.entries.len() as u64,
        state: "restored",
    })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Manifest {
    format: String,
    version: u32,
    app_version: String,
    sqlite_user_version: Option<i64>,
    schema_version: Option<i64>,
    source_data_dir: String,
    cipher: CipherRecord,
    entries: Vec<FileEntry>,
    content_hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CipherRecord {
    prerequisite: String,
    portable: bool,
    witness: String,
    legacy_ciphertext: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FileEntry {
    path: String,
    size: u64,
    sha256: String,
}

#[derive(Debug, Clone)]
struct Payload {
    relative: String,
    absolute: PathBuf,
    size: u64,
    sha256: String,
    directory: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WalkMode {
    Source,
    Verify,
}

struct SqliteIdentity {
    user_version: i64,
    schema_version: i64,
}

struct ExclusiveFile {
    _file: File,
}

impl ExclusiveFile {
    fn try_acquire(data_dir: &Path, name: &str) -> Result<Self> {
        let path = data_dir.join(name);
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .with_context(|| format!("failed to open lock {}", path.display()))?;
        match file.try_lock_exclusive() {
            Ok(()) => Ok(Self { _file: file }),
            Err(error) if lock_is_contended(&error) => {
                bail!(
                    "Stop the running host before backup. {} is held, so this command did not wait, did not open the database, and did not stop another process.",
                    path.display()
                )
            }
            Err(error) => Err(error).context(format!("acquire lock {}", path.display())),
        }
    }
}

fn lock_is_contended(error: &std::io::Error) -> bool {
    error.raw_os_error() == fs2::lock_contended_error().raw_os_error()
}

fn acquire_writer_locks(data_dir: &Path) -> Result<Vec<ExclusiveFile>> {
    let mut held = Vec::with_capacity(LOCK_FILES.len());
    for name in LOCK_FILES {
        held.push(ExclusiveFile::try_acquire(data_dir, name)?);
    }
    Ok(held)
}

struct OutputGuard {
    path: PathBuf,
    armed: bool,
}

impl OutputGuard {
    fn arm(path: PathBuf) -> Self {
        Self { path, armed: true }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for OutputGuard {
    fn drop(&mut self) {
        if self.armed {
            let _ = fs::remove_file(&self.path);
        }
    }
}

struct StageGuard {
    path: PathBuf,
    armed: bool,
}

impl StageGuard {
    fn create(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            reject_reparse_ancestors(parent)?;
        }
        match fs::symlink_metadata(path) {
            Ok(_) => bail!("restore stage already exists: {}", path.display()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).context("inspect restore stage"),
        }
        create_private_dir(path)?;
        Ok(Self {
            path: path.to_path_buf(),
            armed: true,
        })
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn disarm(&mut self) {
        self.armed = false;
    }

    fn publish(&mut self, target: &Path) -> Result<()> {
        validate_restore_target(target)?;
        #[cfg(test)]
        invoke_publish_before(target);
        if !target_exists(target)? {
            // Disarm before rename. After a successful rename the stage path is
            // the published tree; a still-armed guard must not delete it.
            self.disarm();
            if let Err(error) = fs::rename(&self.path, target) {
                self.armed = true;
                return Err(error).context("publish restored data directory");
            }
            return Ok(());
        }
        // remove_dir succeeds only for a directory that is empty at this
        // instant. A sentinel or user file created after validation makes it
        // fail, and this path never renames that directory aside.
        if let Err(_error) = fs::remove_dir(target) {
            bail!(
                "restore target is not empty: {}. This command did not rename or replace it.",
                target.display()
            );
        }
        #[cfg(test)]
        invoke_publish_after_empty_removal(target);
        self.disarm();
        if let Err(error) = fs::rename(&self.path, target) {
            self.armed = true;
            if !target_exists(target)? {
                let _ = fs::create_dir(target);
            }
            return Err(error).context("publish restored data directory");
        }
        Ok(())
    }
}

impl Drop for StageGuard {
    fn drop(&mut self) {
        if self.armed {
            let _ = remove_tree(&self.path);
        }
    }
}

fn validate_source_root(source: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(source)
        .with_context(|| format!("source data directory {}", source.display()))?;
    if is_reparse_metadata(&metadata) {
        bail!(
            "source data directory is a reparse point: {}",
            source.display()
        );
    }
    if !metadata.is_dir() {
        bail!(
            "source data directory is not a directory: {}",
            source.display()
        );
    }
    reject_reparse_ancestors(source)?;
    Ok(())
}

fn validate_output_destination(source: &Path, output: &Path) -> Result<()> {
    if output == source || path_within(output, source)? {
        bail!("snapshot output must be outside the source data directory");
    }
    let parent = output.parent().context("snapshot output has no parent")?;
    let parent_meta = fs::symlink_metadata(parent)
        .with_context(|| format!("snapshot output directory {}", parent.display()))?;
    if is_reparse_metadata(&parent_meta) || !parent_meta.is_dir() {
        bail!("snapshot output directory is not a real directory");
    }
    reject_reparse_ancestors(parent)?;
    match fs::symlink_metadata(output) {
        Ok(_) => bail!("snapshot output already exists: {}", output.display()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).context("inspect snapshot output"),
    }
}

fn validate_input_archive(input: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(input)
        .with_context(|| format!("snapshot input {}", input.display()))?;
    if is_reparse_metadata(&metadata) {
        bail!("snapshot input is a reparse point: {}", input.display());
    }
    if !metadata.is_file() {
        bail!("snapshot input is not a file: {}", input.display());
    }
    if metadata.len() > MAX_ARCHIVE_BYTES {
        bail!("snapshot archive exceeds 4 GiB");
    }
    reject_reparse_ancestors(input)?;
    Ok(())
}

fn validate_restore_target(target: &Path) -> Result<()> {
    if let Some(parent) = target.parent() {
        let parent_meta = fs::symlink_metadata(parent)
            .with_context(|| format!("restore target directory {}", parent.display()))?;
        if is_reparse_metadata(&parent_meta) || !parent_meta.is_dir() {
            bail!("restore target directory is not a real directory");
        }
        reject_reparse_ancestors(parent)?;
    }
    match fs::symlink_metadata(target) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Ok(metadata) => {
            if is_reparse_metadata(&metadata) {
                bail!("restore target is a reparse point: {}", target.display());
            }
            if !metadata.is_dir() {
                bail!("restore target already exists: {}", target.display());
            }
            let mut entries = fs::read_dir(target)
                .with_context(|| format!("read restore target {}", target.display()))?;
            if entries.next().is_some() {
                bail!(
                    "restore target is not empty: {}. This command has no replace mode.",
                    target.display()
                );
            }
            Ok(())
        }
        Err(error) => Err(error).context("inspect restore target"),
    }
}

fn target_exists(target: &Path) -> Result<bool> {
    match fs::symlink_metadata(target) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error).context("inspect restore target"),
    }
}

fn absolute_real_path(path: &Path) -> Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .context("current directory")?
            .join(path)
    };
    if absolute
        .components()
        .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
    {
        bail!("path must not contain . or ..: {}", path.display());
    }
    Ok(absolute)
}

fn path_within(path: &Path, parent: &Path) -> Result<bool> {
    let path = absolute_real_path(path)?;
    let parent = absolute_real_path(parent)?;
    Ok(path.starts_with(&parent) && path != parent)
}

fn portable_data_dir(path: &Path) -> String {
    let mut text = path.to_string_lossy().replace('\\', "/");
    while text.len() > 1 && text.ends_with('/') {
        text.pop();
    }
    text
}

fn reject_reparse_ancestors(path: &Path) -> Result<()> {
    let mut current = path.parent();
    let mut depth = 0usize;
    while let Some(item) = current {
        depth += 1;
        if depth > MAX_DEPTH {
            bail!("path is too deep: {}", path.display());
        }
        if path_is_reparse(item)? {
            bail!(
                "refusing a path whose ancestor is a reparse point: {}",
                item.display()
            );
        }
        let next = item.parent();
        if next.as_deref() == Some(item) {
            break;
        }
        current = next;
    }
    Ok(())
}

fn path_is_reparse(path: &Path) -> Result<bool> {
    let metadata =
        fs::symlink_metadata(path).with_context(|| format!("inspect path {}", path.display()))?;
    Ok(is_reparse_metadata(&metadata))
}

fn is_reparse_metadata(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}

struct SqliteInspection {
    identity: Option<SqliteIdentity>,
    legacy_unauthenticated: bool,
}

fn inspect_sqlite(data_dir: &Path, cipher: Option<&dyn KeyCipher>) -> Result<SqliteInspection> {
    let database = data_dir.join("data.sqlite");
    if !regular_file_exists(&database)? {
        return Ok(SqliteInspection {
            identity: None,
            legacy_unauthenticated: false,
        });
    }
    let temp =
        std::env::temp_dir().join(format!("ocg-backup-id-{}", uuid::Uuid::new_v4().simple()));
    create_private_dir(&temp)?;
    let _cleanup = RemoveDir(temp.clone());
    copy_regular_file(&database, &temp.join("data.sqlite"))?;
    for suffix in ["data.sqlite-wal", "data.sqlite-shm"] {
        let source = data_dir.join(suffix);
        if regular_file_exists(&source)? {
            copy_regular_file(&source, &temp.join(suffix))?;
        }
    }
    let connection =
        Connection::open_with_flags(&temp.join("data.sqlite"), OpenFlags::SQLITE_OPEN_READ_ONLY)
            .with_context(|| {
                format!(
                    "failed to read {} without opening the database",
                    database.display()
                )
            })?;
    connection
        .busy_timeout(std::time::Duration::from_millis(0))
        .context("set sqlite busy timeout")?;
    connection
        .pragma_update(None, "query_only", 1i32)
        .context("set sqlite query_only")?;
    let identity = identity_on(&connection)?;
    let legacy_unauthenticated = if let Some(cipher) = cipher {
        crate::db::probe_snapshot_ciphertext(&connection, cipher)?.legacy_unauthenticated
    } else {
        false
    };
    Ok(SqliteInspection {
        identity: Some(identity),
        legacy_unauthenticated,
    })
}

fn identity_on(connection: &Connection) -> Result<SqliteIdentity> {
    let user_version: i64 = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .context("read sqlite user_version")?;
    let has_table: i64 = connection.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'schema_version'",
        [],
        |row| row.get(0),
    )?;
    let schema_version = if has_table == 0 {
        0
    } else {
        connection
            .query_row(
                "SELECT COALESCE(MAX(version), 0) FROM schema_version",
                [],
                |row| row.get::<_, i64>(0),
            )
            .context("read schema_version")?
    };
    Ok(SqliteIdentity {
        user_version,
        schema_version,
    })
}

struct RemoveDir(PathBuf);

impl Drop for RemoveDir {
    fn drop(&mut self) {
        let _ = remove_tree(&self.0);
    }
}

fn walk_tree(root: &Path, mode: WalkMode) -> Result<Vec<Payload>> {
    let mut payloads = Vec::new();
    walk_directory(root, "", mode, &mut payloads)?;
    Ok(payloads)
}

fn walk_directory(
    directory: &Path,
    prefix: &str,
    mode: WalkMode,
    payloads: &mut Vec<Payload>,
) -> Result<()> {
    if prefix.matches('/').count() > MAX_DEPTH {
        bail!("data directory is too deep");
    }
    let mut children = fs::read_dir(directory)
        .with_context(|| format!("read {}", directory.display()))?
        .collect::<Result<Vec<_>, _>>()?;
    children.sort_by_key(|entry| entry.file_name());
    for child in children {
        let name = child.file_name();
        let name = name
            .to_str()
            .context("data directory entry is not Unicode")?;
        let path = directory.join(name);
        let metadata =
            fs::symlink_metadata(&path).with_context(|| format!("inspect {}", path.display()))?;
        if is_reparse_metadata(&metadata) {
            bail!("refusing a reparse point: {}", path.display());
        }
        let relative = if prefix.is_empty() {
            name.to_string()
        } else {
            format!("{prefix}/{name}")
        };
        if relative.len() > MAX_RELATIVE_BYTES {
            bail!("snapshot path is too long");
        }
        validate_relative_components(&relative)?;
        if prefix.is_empty() && name == MANIFEST_NAME {
            match mode {
                WalkMode::Source => {
                    bail!("data directory contains the reserved snapshot manifest name")
                }
                WalkMode::Verify => continue,
            }
        }
        if prefix.is_empty() && EXCLUDED_ROOT_FILES.contains(&name) {
            if !metadata.is_file() {
                bail!("runtime marker must be a regular file: {name}");
            }
            match mode {
                WalkMode::Source => continue,
                WalkMode::Verify => bail!("snapshot must not contain runtime marker {name}"),
            }
        }
        if metadata.is_dir() {
            payloads.push(Payload {
                relative: format!("{relative}/"),
                absolute: path.clone(),
                size: 0,
                sha256: EMPTY_SHA256.to_string(),
                directory: true,
            });
            walk_directory(&path, &relative, mode, payloads)?;
            continue;
        }
        if !metadata.is_file() {
            bail!("refusing a special file: {}", path.display());
        }
        reject_hard_link(&path)?;
        let (size, sha256) = hash_file(&path)?;
        payloads.push(Payload {
            relative,
            absolute: path,
            size,
            sha256,
            directory: false,
        });
    }
    Ok(())
}

fn hash_file(path: &Path) -> Result<(u64, String)> {
    let mut file = File::open(path).with_context(|| format!("read {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut total = 0u64;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        total = total
            .checked_add(read as u64)
            .filter(|total| *total <= MAX_FILE_BYTES)
            .context("snapshot file exceeds 2 GiB")?;
        hasher.update(&buffer[..read]);
    }
    Ok((total, hex::encode(hasher.finalize())))
}

fn reject_hard_link(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let metadata = fs::symlink_metadata(path)?;
        if metadata.is_file() && metadata.nlink() > 1 {
            bail!("refusing a hard link: {}", path.display());
        }
        return Ok(());
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Foundation::HANDLE;
        use windows_sys::Win32::Storage::FileSystem::{
            BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
        };
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS)
            .open(path)
            .with_context(|| format!("inspect links for {}", path.display()))?;
        let mut info = unsafe { std::mem::zeroed::<BY_HANDLE_FILE_INFORMATION>() };
        let ok = unsafe { GetFileInformationByHandle(file.as_raw_handle() as HANDLE, &mut info) };
        if ok == 0 {
            return Err(io::Error::last_os_error())
                .context(format!("inspect links for {}", path.display()));
        }
        if info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            bail!("refusing a reparse point: {}", path.display());
        }
        if info.nNumberOfLinks > 1 {
            bail!("refusing a hard link: {}", path.display());
        }
        return Ok(());
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = path;
        Ok(())
    }
}

fn validate_relative_components(relative: &str) -> Result<()> {
    if relative.is_empty()
        || relative.contains('\\')
        || relative.contains('\0')
        || relative.starts_with('/')
    {
        bail!("snapshot path is unsafe");
    }
    let bare = relative.trim_end_matches('/');
    if bare.is_empty() || bare.contains("//") {
        bail!("snapshot path is unsafe");
    }
    let mut depth = 0usize;
    for component in bare.split('/') {
        depth += 1;
        if depth > MAX_DEPTH || is_unsafe_component(component) {
            bail!("snapshot path is unsafe");
        }
    }
    Ok(())
}

fn is_unsafe_component(text: &str) -> bool {
    if text.is_empty()
        || text == "."
        || text == ".."
        || text.ends_with(['.', ' '])
        || text.contains(['\0', ':'])
        || text.chars().any(|ch| ch.is_control())
        || text.len() > 255
    {
        return true;
    }
    let stem = text.split('.').next().unwrap_or(text).to_ascii_uppercase();
    matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || stem
            .strip_prefix("COM")
            .or_else(|| stem.strip_prefix("LPT"))
            .is_some_and(|suffix| suffix.len() == 1 && matches!(suffix.as_bytes()[0], b'1'..=b'9'))
}

fn ensure_tree_shape(payloads: &[Payload]) -> Result<()> {
    let nodes = payloads
        .iter()
        .map(|payload| (collision_key(&payload.relative), payload.directory))
        .collect::<Vec<_>>();
    ensure_nodes(&nodes)
}

fn ensure_nodes(nodes: &[(String, bool)]) -> Result<()> {
    let mut seen: BTreeMap<String, bool> = BTreeMap::new();
    for (key, is_dir) in nodes {
        if key.is_empty() || seen.contains_key(key) {
            bail!("snapshot contains a duplicate or case-conflicting path");
        }
        let mut ancestor = String::new();
        let components = key.split('/').collect::<Vec<_>>();
        for component in components.iter().take(components.len().saturating_sub(1)) {
            if !ancestor.is_empty() {
                ancestor.push('/');
            }
            ancestor.push_str(component);
            if seen.get(&ancestor) == Some(&false) {
                bail!("snapshot contains a file and directory collision");
            }
        }
        if !is_dir
            && seen
                .keys()
                .any(|seen_key| seen_key.starts_with(&format!("{key}/")))
        {
            bail!("snapshot contains a file and directory collision");
        }
        seen.insert(key.clone(), *is_dir);
    }
    Ok(())
}

fn collision_key(relative: &str) -> String {
    relative
        .trim_end_matches('/')
        .replace('\\', "/")
        .to_ascii_lowercase()
}

fn payload_entries(payloads: &[Payload]) -> Vec<FileEntry> {
    payloads
        .iter()
        .map(|payload| FileEntry {
            path: payload.relative.clone(),
            size: payload.size,
            sha256: payload.sha256.clone(),
        })
        .collect()
}

struct ResolvedCipher {
    source: &'static str,
    cipher: Arc<dyn KeyCipher + Send + Sync>,
}

fn resolve_cipher(data_dir: &Path, witness: &CipherWitness) -> Result<ResolvedCipher> {
    if let Some(secret) = &witness.explicit {
        if secret.is_empty() {
            bail!("caller encryption key is empty");
        }
        return Ok(ResolvedCipher {
            source: "explicit",
            cipher: Arc::new(StaticKeyCipher::new(secret)),
        });
    }
    if let Some(secret) = &witness.env {
        if secret.is_empty() {
            bail!("caller encryption key is empty");
        }
        return Ok(ResolvedCipher {
            source: "env",
            cipher: Arc::new(StaticKeyCipher::new(secret)),
        });
    }
    let key_path = data_dir.join(".encryption-key");
    if regular_file_exists(&key_path)? {
        let secret = read_key_file(&key_path)?;
        return Ok(ResolvedCipher {
            source: "file",
            cipher: Arc::new(StaticKeyCipher::new(&secret)),
        });
    }
    if let Some(cipher) = &witness.machine {
        return Ok(ResolvedCipher {
            source: "machine",
            cipher: Arc::clone(cipher),
        });
    }
    #[cfg(windows)]
    {
        return Ok(ResolvedCipher {
            source: "machine",
            cipher: Arc::new(MachineBoundCipher::new()),
        });
    }
    #[cfg(not(windows))]
    {
        bail!(
            "snapshot has no encryption key file and no caller key. This command did not create a key file."
        );
    }
}

fn read_key_file(path: &Path) -> Result<String> {
    if path_is_reparse(path)? {
        bail!("encryption key file is a reparse point");
    }
    let metadata = fs::symlink_metadata(path).context("read encryption key file")?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > MAX_KEY_FILE_BYTES {
        bail!("encryption key file is empty or too large");
    }
    let bytes = fs::read(path).context("read encryption key file")?;
    if bytes.is_empty() || bytes.len() as u64 > MAX_KEY_FILE_BYTES {
        bail!("encryption key file is empty or too large");
    }
    let secret = String::from_utf8(bytes).context("encryption key file is not UTF-8")?;
    if secret.is_empty() {
        bail!("encryption key file is empty or too large");
    }
    Ok(secret)
}

fn seal_witness(cipher: &dyn KeyCipher) -> Result<String> {
    let sealed = cipher
        .encrypt(CIPHER_WITNESS_PLAINTEXT)
        .context("snapshot cipher witness could not be sealed")?;
    if !sealed.starts_with(LOCAL_CIPHER_V2_PREFIX) {
        bail!("snapshot cipher witness is not authenticated");
    }
    Ok(sealed)
}

fn open_witness(cipher: &dyn KeyCipher, sealed: &str) -> Result<()> {
    if !sealed.starts_with(LOCAL_CIPHER_V2_PREFIX) {
        bail!("snapshot cipher witness is not authenticated");
    }
    let opened = cipher.decrypt(sealed).context(
        "snapshot cipher witness did not authenticate. The caller key or machine does not match this snapshot. The key was not printed.",
    )?;
    if opened != CIPHER_WITNESS_PLAINTEXT {
        bail!(
            "snapshot cipher witness did not authenticate. The caller key or machine does not match this snapshot. The key was not printed."
        );
    }
    Ok(())
}

fn cipher_record(resolved: &ResolvedCipher, legacy_unauthenticated: bool) -> Result<CipherRecord> {
    Ok(CipherRecord {
        prerequisite: resolved.source.to_string(),
        portable: resolved.source == "file",
        witness: seal_witness(resolved.cipher.as_ref())?,
        legacy_ciphertext: if legacy_unauthenticated {
            "present-not-proof"
        } else {
            "none"
        }
        .to_string(),
    })
}

fn content_hash_of(manifest: &Manifest) -> Result<String> {
    let material = HashMaterial {
        format: &manifest.format,
        version: manifest.version,
        app_version: &manifest.app_version,
        sqlite_user_version: manifest.sqlite_user_version,
        schema_version: manifest.schema_version,
        source_data_dir: &manifest.source_data_dir,
        cipher: &manifest.cipher,
        entries: &manifest.entries,
    };
    let bytes = serde_json::to_vec(&material).context("encode snapshot manifest hash")?;
    Ok(hex::encode(Sha256::digest(bytes)))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct HashMaterial<'a> {
    format: &'a str,
    version: u32,
    app_version: &'a str,
    sqlite_user_version: Option<i64>,
    schema_version: Option<i64>,
    source_data_dir: &'a str,
    cipher: &'a CipherRecord,
    entries: &'a [FileEntry],
}

fn validate_owned_cpa(source: &Path, source_dir: &str) -> Result<()> {
    let cpa = source.join("cpa");
    let managed = regular_file_exists(&cpa.join("managed.json"))?;
    let config = regular_file_exists(&cpa.join("config.yaml"))?;
    let previous = regular_file_exists(&cpa.join("config.yaml.previous"))?;
    if !managed {
        if config || previous {
            bail!(
                "CPA config is not an owned managed runtime; refusing to snapshot an external auth-dir"
            );
        }
        return Ok(());
    }
    if !config {
        bail!("owned CPA managed.json requires config.yaml");
    }
    let expected = format!("{source_dir}/cpa/auth");
    ensure_auth_dir(&cpa.join("config.yaml"), &expected)?;
    if previous {
        ensure_auth_dir(&cpa.join("config.yaml.previous"), &expected)?;
    }
    Ok(())
}

fn ensure_auth_dir(path: &Path, expected: &str) -> Result<()> {
    let actual = read_auth_dir(path)?;
    if !same_path(&actual, expected) {
        bail!("owned CPA auth-dir does not match the source data directory");
    }
    Ok(())
}

fn read_owned_config(path: &Path) -> Result<Vec<u8>> {
    if path_is_reparse(path)? {
        bail!("CPA config is a reparse point");
    }
    let metadata =
        fs::symlink_metadata(path).with_context(|| format!("read {}", path.display()))?;
    if !metadata.is_file() {
        bail!("CPA config is not a file");
    }
    if metadata.len() > MAX_OWNED_CONFIG_BYTES {
        bail!("managed CPA config exceeds 1 MiB");
    }
    let bytes = fs::read(path).with_context(|| format!("read {}", path.display()))?;
    if bytes.len() as u64 > MAX_OWNED_CONFIG_BYTES {
        bail!("managed CPA config exceeds 1 MiB");
    }
    Ok(bytes)
}

fn read_auth_dir(path: &Path) -> Result<String> {
    let bytes = read_owned_config(path)?;
    let text = String::from_utf8(bytes).context("CPA config is not UTF-8")?;
    let value: serde_yaml_ng::Value = serde_yaml_ng::from_str(&text)
        .map_err(|_| anyhow::anyhow!("managed CPA config.yaml could not be parsed"))?;
    let mapping = match &value {
        serde_yaml_ng::Value::Mapping(mapping) => mapping,
        _ => bail!("managed CPA config.yaml must be a mapping"),
    };
    let key = serde_yaml_ng::Value::String("auth-dir".to_string());
    match mapping.get(&key) {
        Some(serde_yaml_ng::Value::String(auth_dir)) if !auth_dir.is_empty() => {
            Ok(auth_dir.clone())
        }
        Some(_) => bail!("managed CPA auth-dir must be a string"),
        None => bail!("managed CPA config.yaml has no auth-dir"),
    }
}

fn rewrite_auth_dir(path: &Path, auth_dir: &str) -> Result<()> {
    let bytes = read_owned_config(path)?;
    let text = String::from_utf8(bytes).context("CPA config is not UTF-8")?;
    let mut value: serde_yaml_ng::Value = serde_yaml_ng::from_str(&text)
        .map_err(|_| anyhow::anyhow!("managed CPA config.yaml could not be parsed"))?;
    let mapping = match &mut value {
        serde_yaml_ng::Value::Mapping(mapping) => mapping,
        _ => bail!("managed CPA config.yaml must be a mapping"),
    };
    let key = serde_yaml_ng::Value::String("auth-dir".to_string());
    if !mapping.contains_key(&key) {
        bail!("managed CPA config.yaml has no auth-dir");
    }
    mapping.insert(key, serde_yaml_ng::Value::String(auth_dir.to_string()));
    let mut rendered = serde_yaml_ng::to_string(&value)
        .map_err(|_| anyhow::anyhow!("managed CPA config.yaml could not be written"))?;
    if let Some(rest) = rendered.strip_prefix("---\n") {
        rendered = rest.to_string();
    }
    if let Some(rest) = rendered.strip_prefix("---\r\n") {
        rendered = rest.to_string();
    }
    let mut file = OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(path)
        .with_context(|| format!("replace {}", path.display()))?;
    file.write_all(rendered.as_bytes())?;
    file.sync_all()?;
    tighten_file(path)?;
    Ok(())
}

fn relocate_owned_cpa(stage: &Path, manifest: &Manifest, target: &Path) -> Result<()> {
    let cpa = stage.join("cpa");
    let managed = regular_file_exists(&cpa.join("managed.json"))?;
    let config = regular_file_exists(&cpa.join("config.yaml"))?;
    let previous = regular_file_exists(&cpa.join("config.yaml.previous"))?;
    if !managed {
        if config || previous {
            bail!(
                "CPA config is not an owned managed runtime; refusing to restore an external auth-dir"
            );
        }
        return Ok(());
    }
    if !config {
        bail!("owned CPA managed.json requires config.yaml");
    }
    let expected = format!(
        "{}/cpa/auth",
        manifest.source_data_dir.trim_end_matches('/')
    );
    ensure_auth_dir(&cpa.join("config.yaml"), &expected)?;
    if previous {
        ensure_auth_dir(&cpa.join("config.yaml.previous"), &expected)?;
    }
    let relocated = format!("{}/cpa/auth", portable_data_dir(target));
    if same_path(&relocated, &expected) {
        bail!("restore target reuses the source CPA auth-dir");
    }
    let current_path = cpa.join("config.yaml");
    let previous_path = cpa.join("config.yaml.previous");
    let current_before = read_owned_text(&current_path)?;
    let previous_before = if previous {
        Some(read_owned_text(&previous_path)?)
    } else {
        None
    };
    rewrite_auth_dir(&current_path, &relocated)?;
    if previous {
        rewrite_auth_dir(&previous_path, &relocated)?;
    }
    let current_after = read_owned_text(&current_path)?;
    let previous_after = if previous {
        Some(read_owned_text(&previous_path)?)
    } else {
        None
    };
    rebase_staged_execution(
        stage,
        &current_before,
        &current_after,
        previous_before.as_deref(),
        previous_after.as_deref(),
    )?;
    Ok(())
}

fn read_owned_text(path: &Path) -> Result<String> {
    let bytes = read_owned_config(path)?;
    String::from_utf8(bytes).context("CPA config is not UTF-8")
}

/// Update only a known accepted tuple after the staged auth-dir rewrite.
///
/// A missing settings table or execution row stays unknown. The real database
/// is opened for writing only when the record actually changes.
fn rebase_staged_execution(
    stage: &Path,
    current_before: &str,
    current_after: &str,
    previous_before: Option<&str>,
    previous_after: Option<&str>,
) -> Result<()> {
    let database = stage.join("data.sqlite");
    if !regular_file_exists(&database)? {
        return Ok(());
    }
    let Some(json) = read_execution_record_copy(&database)? else {
        return Ok(());
    };
    let updated = crate::cpa_execution::rebase_archived_execution_record(
        &json,
        current_before,
        current_after,
        previous_before,
        previous_after,
    )
    .map_err(|error| anyhow::anyhow!("{error}"))?;
    let Some(updated) = updated else {
        return Ok(());
    };
    write_execution_record(&database, &updated)
}

fn read_execution_record_copy(database: &Path) -> Result<Option<String>> {
    let temp = std::env::temp_dir().join(format!(
        "ocg-backup-rebase-{}",
        uuid::Uuid::new_v4().simple()
    ));
    create_private_dir(&temp)?;
    let _cleanup = RemoveDir(temp.clone());
    copy_regular_file(database, &temp.join("data.sqlite"))?;
    let connection =
        Connection::open_with_flags(&temp.join("data.sqlite"), OpenFlags::SQLITE_OPEN_READ_ONLY)
            .with_context(|| format!("read {}", database.display()))?;
    connection
        .busy_timeout(std::time::Duration::from_millis(0))
        .context("set sqlite busy timeout")?;
    connection
        .pragma_update(None, "query_only", 1i32)
        .context("set sqlite query_only")?;
    let has_table: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'settings'",
            [],
            |row| row.get(0),
        )
        .context("read settings table")?;
    if has_table == 0 {
        return Ok(None);
    }
    connection
        .query_row(
            "SELECT value FROM settings WHERE key = ?1",
            [crate::cpa_execution::EXECUTION_RECORD_KEY],
            |row| row.get(0),
        )
        .optional()
        .context("read CPA execution record")
}

fn write_execution_record(database: &Path, json: &str) -> Result<()> {
    let connection = Connection::open_with_flags(database, OpenFlags::SQLITE_OPEN_READ_WRITE)
        .with_context(|| format!("open {}", database.display()))?;
    connection
        .busy_timeout(std::time::Duration::from_millis(0))
        .context("set sqlite busy timeout")?;
    let changed = connection
        .execute(
            "UPDATE settings SET value = ?1 WHERE key = ?2",
            rusqlite::params![json, crate::cpa_execution::EXECUTION_RECORD_KEY],
        )
        .context("update CPA execution record")?;
    if changed != 1 {
        bail!("CPA execution record was not updated");
    }
    Ok(())
}

fn same_path(left: &str, right: &str) -> bool {
    let normalize = |value: &str| {
        let mut text = value.replace('\\', "/");
        while text.len() > 1 && text.ends_with('/') {
            text.pop();
        }
        text
    };
    let left = normalize(left);
    let right = normalize(right);
    #[cfg(windows)]
    {
        left.eq_ignore_ascii_case(&right)
    }
    #[cfg(not(windows))]
    {
        left == right
    }
}

fn write_archive(output: &Path, payloads: &[Payload], manifest: &Manifest) -> Result<()> {
    let file = create_private_file(output)?;
    let mut guard = OutputGuard::arm(output.to_path_buf());
    let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::default());
    let mut builder = tar::Builder::new(encoder);
    let mut uncompressed = 0u64;
    for payload in payloads {
        uncompressed = uncompressed
            .checked_add(payload.size)
            .filter(|total| *total <= MAX_UNCOMPRESSED_BYTES)
            .context("snapshot uncompressed size exceeds 4 GiB")?;
        append_payload(&mut builder, payload)?;
    }
    let manifest_bytes = serde_json::to_vec(manifest).context("encode snapshot manifest")?;
    if manifest_bytes.len() as u64 > MAX_MANIFEST_BYTES {
        bail!("snapshot manifest exceeds 32 MiB");
    }
    uncompressed = uncompressed
        .checked_add(manifest_bytes.len() as u64)
        .filter(|total| *total <= MAX_UNCOMPRESSED_BYTES)
        .context("snapshot uncompressed size exceeds 4 GiB")?;
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::Regular);
    header.set_size(manifest_bytes.len() as u64);
    header.set_mode(0o600);
    header.set_mtime(0);
    header.set_cksum();
    builder
        .append_data(&mut header, MANIFEST_NAME, manifest_bytes.as_slice())
        .context("write snapshot manifest")?;
    let encoder = builder.into_inner().context("finish snapshot archive")?;
    let file = encoder.finish().context("finish snapshot compression")?;
    file.sync_all().context("sync snapshot archive")?;
    let _ = uncompressed;
    guard.disarm();
    Ok(())
}

fn append_payload<W: Write>(builder: &mut tar::Builder<W>, payload: &Payload) -> Result<()> {
    let mut header = tar::Header::new_gnu();
    header.set_mtime(0);
    if payload.directory {
        header.set_entry_type(tar::EntryType::Directory);
        header.set_size(0);
        header.set_mode(0o700);
        header.set_cksum();
        builder
            .append_data(&mut header, &payload.relative, io::empty())
            .with_context(|| format!("archive {}", payload.relative))?;
        return Ok(());
    }
    header.set_entry_type(tar::EntryType::Regular);
    header.set_size(payload.size);
    header.set_mode(0o600);
    header.set_cksum();
    let file = File::open(&payload.absolute)
        .with_context(|| format!("read {}", payload.absolute.display()))?;
    let mut reader = HashReader {
        inner: file,
        hasher: Sha256::new(),
        total: 0,
    };
    builder
        .append_data(&mut header, &payload.relative, &mut reader)
        .with_context(|| format!("archive {}", payload.relative))?;
    let total = reader.total;
    let actual = hex::encode(reader.hasher.finalize());
    if total != payload.size || actual != payload.sha256 {
        bail!("snapshot file changed while it was being archived");
    }
    Ok(())
}

struct HashReader<R> {
    inner: R,
    hasher: Sha256,
    total: u64,
}

impl<R: Read> Read for HashReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let read = self.inner.read(buffer)?;
        self.hasher.update(&buffer[..read]);
        self.total = self.total.saturating_add(read as u64);
        Ok(read)
    }
}

fn extract_archive(archive: &Path, destination: &Path) -> Result<()> {
    let file = File::open(archive).with_context(|| format!("open {}", archive.display()))?;
    let decoder = flate2::read::GzDecoder::new(file);
    let mut tar = tar::Archive::new(decoder);
    tar.set_overwrite(false);
    tar.set_preserve_permissions(false);
    tar.set_preserve_mtime(false);
    let mut seen: Vec<(String, bool)> = Vec::new();
    let mut uncompressed = 0u64;
    let mut entries = 0usize;
    let mut saw_manifest = false;
    for entry in tar
        .entries()
        .context("snapshot archive is not a valid tar.gz")?
    {
        entries += 1;
        if entries > MAX_ENTRIES {
            bail!("snapshot archive has too many entries");
        }
        let mut entry = entry.context("read snapshot archive entry")?;
        let entry_type = entry.header().entry_type();
        if matches!(
            entry_type,
            tar::EntryType::XHeader
                | tar::EntryType::XGlobalHeader
                | tar::EntryType::GNULongName
                | tar::EntryType::GNULongLink
        ) {
            io::copy(&mut entry, &mut io::sink())?;
            continue;
        }
        if entry_type.is_symlink()
            || entry_type.is_hard_link()
            || matches!(
                entry_type,
                tar::EntryType::Link | tar::EntryType::Symlink | tar::EntryType::Fifo
            )
            || unix_symlink_mode(entry.header().mode().ok())
        {
            bail!("snapshot archive must not contain links");
        }
        if !entry_type.is_file() && !entry_type.is_dir() {
            bail!("snapshot archive contains an unsupported entry");
        }
        let raw = entry
            .path()
            .context("snapshot archive path")?
            .to_string_lossy()
            .into_owned();
        if raw.as_bytes().contains(&0) || raw.contains('\\') {
            bail!("snapshot path is unsafe");
        }
        if !entry_type.is_dir() && raw.ends_with('/') {
            bail!("snapshot file path is unsafe");
        }
        let directory = entry_type.is_dir();
        let mut relative = normalize_archive_path(&raw)?;
        if directory && !relative.ends_with('/') {
            relative.push('/');
        }
        if relative == MANIFEST_NAME {
            if directory {
                bail!("snapshot manifest must be a file");
            }
            saw_manifest = true;
        } else if EXCLUDED_ROOT_FILES.contains(&relative.as_str()) {
            bail!("snapshot must not contain runtime marker {relative}");
        }
        let key = collision_key(&relative);
        if seen.iter().any(|(seen_key, _)| seen_key == &key) {
            bail!("snapshot contains a duplicate or case-conflicting path");
        }
        seen.push((key, directory));
        ensure_nodes(&seen)?;
        let out_path = destination.join(relative.trim_end_matches('/'));
        if !out_path.starts_with(destination) {
            bail!("snapshot path is unsafe");
        }
        let declared = entry.size();
        if declared > MAX_FILE_BYTES
            || (!directory && relative == MANIFEST_NAME && declared > MAX_MANIFEST_BYTES)
        {
            bail!("snapshot entry exceeds its size limit");
        }
        uncompressed = uncompressed
            .checked_add(declared)
            .filter(|total| *total <= MAX_UNCOMPRESSED_BYTES)
            .context("snapshot uncompressed size exceeds 4 GiB")?;
        if directory {
            if declared != 0 {
                bail!("snapshot directory entry has a payload");
            }
            ensure_dir_inside(destination, &out_path)?;
            io::copy(&mut entry, &mut io::sink())?;
            if path_is_reparse(&out_path)? {
                bail!("snapshot extract path resolved to a reparse point");
            }
            continue;
        }
        if let Some(parent) = out_path.parent() {
            ensure_dir_inside(destination, parent)?;
        }
        let mut output = create_private_file(&out_path)?;
        let copied = io::copy(
            &mut (&mut entry).take(declared.saturating_add(1)),
            &mut output,
        )?;
        output.sync_all()?;
        drop(output);
        if copied != declared {
            bail!("snapshot entry size did not match the archive");
        }
        if path_is_reparse(&out_path)? {
            bail!("snapshot extract path resolved to a reparse point");
        }
    }
    if !saw_manifest {
        bail!("snapshot archive has no manifest");
    }
    Ok(())
}

fn unix_symlink_mode(mode: Option<u32>) -> bool {
    mode.is_some_and(|mode| mode & 0o170_000 == 0o120_000)
}

fn normalize_archive_path(raw: &str) -> Result<String> {
    let path = Path::new(raw);
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => {
                let text = part.to_str().context("snapshot path is not Unicode")?;
                if is_unsafe_component(text) {
                    bail!("snapshot path is unsafe");
                }
                parts.push(text.to_string());
            }
            Component::CurDir => {}
            Component::Prefix(_) | Component::RootDir | Component::ParentDir => {
                bail!("snapshot path is unsafe");
            }
        }
    }
    if parts.is_empty() {
        bail!("snapshot path is empty");
    }
    let mut relative = parts.join("/");
    if raw.ends_with('/') {
        relative.push('/');
    }
    validate_relative_components(&relative)?;
    Ok(relative)
}

fn load_manifest_file(path: &Path) -> Result<Manifest> {
    if path_is_reparse(path)? {
        bail!("snapshot manifest is a reparse point");
    }
    let bytes = fs::read(path).context("read snapshot manifest")?;
    if bytes.len() as u64 > MAX_MANIFEST_BYTES {
        bail!("snapshot manifest exceeds 32 MiB");
    }
    serde_json::from_slice(&bytes).context("snapshot manifest is not the v1 format")
}

fn verify_manifest(manifest: &Manifest) -> Result<()> {
    if manifest.format != FORMAT {
        bail!("snapshot format is not {FORMAT}");
    }
    if manifest.version != FORMAT_VERSION {
        bail!("snapshot format version is not supported");
    }
    if manifest.app_version.is_empty() || manifest.source_data_dir.is_empty() {
        bail!("snapshot manifest is incomplete");
    }
    if manifest.source_data_dir.contains('\0') {
        bail!("snapshot source path is unsafe");
    }
    match manifest.cipher.prerequisite.as_str() {
        "explicit" | "env" | "file" | "machine" => {}
        _ => bail!("snapshot cipher prerequisite is not supported"),
    }
    if manifest.cipher.portable != (manifest.cipher.prerequisite == "file") {
        bail!("snapshot cipher portability does not match its prerequisite");
    }
    if !manifest.cipher.witness.starts_with(LOCAL_CIPHER_V2_PREFIX) {
        bail!("snapshot cipher witness is not authenticated");
    }
    match manifest.cipher.legacy_ciphertext.as_str() {
        "none" | "present-not-proof" => {}
        _ => bail!("snapshot legacy ciphertext state is not supported"),
    }
    let has_key_file = manifest
        .entries
        .iter()
        .any(|entry| entry.path == ".encryption-key");
    if manifest.cipher.prerequisite == "file" && !has_key_file {
        bail!("snapshot claims a key file that is not in the archive");
    }
    if manifest.cipher.prerequisite == "machine" && has_key_file {
        bail!("snapshot key file does not match the cipher prerequisite");
    }
    if manifest.entries.len() > MAX_ENTRIES {
        bail!("snapshot archive has too many entries");
    }
    let mut nodes = Vec::new();
    for entry in &manifest.entries {
        validate_relative_components(&entry.path)?;
        if entry.path == MANIFEST_NAME || EXCLUDED_ROOT_FILES.contains(&entry.path.as_str()) {
            bail!("snapshot must not contain runtime marker {}", entry.path);
        }
        if entry.size > MAX_FILE_BYTES || !is_lower_hex(&entry.sha256) {
            bail!("snapshot entry is invalid");
        }
        let directory = entry.path.ends_with('/');
        if directory && (entry.size != 0 || entry.sha256 != EMPTY_SHA256) {
            bail!("snapshot directory entry is invalid");
        }
        nodes.push((collision_key(&entry.path), directory));
    }
    ensure_nodes(&nodes)?;
    let actual = content_hash_of(manifest)?;
    if actual != manifest.content_hash {
        bail!("snapshot content hash does not match");
    }
    let sqlite_fields = manifest.sqlite_user_version.is_some() || manifest.schema_version.is_some();
    if sqlite_fields
        && (manifest.sqlite_user_version.is_none() || manifest.schema_version.is_none())
    {
        bail!("snapshot database identity is incomplete");
    }
    Ok(())
}

fn is_lower_hex(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn verify_stage_tree(stage: &Path, manifest: &Manifest) -> Result<()> {
    let mut actual = walk_tree(stage, WalkMode::Verify)?;
    actual.sort_by(|left, right| left.relative.cmp(&right.relative));
    let mut expected = manifest.entries.clone();
    expected.sort_by(|left, right| left.path.cmp(&right.path));
    if actual.len() != expected.len() {
        bail!("snapshot contents do not match the manifest");
    }
    for (payload, entry) in actual.iter().zip(expected.iter()) {
        if payload.relative != entry.path
            || payload.size != entry.size
            || payload.sha256 != entry.sha256
        {
            bail!("snapshot entry hash does not match {}", entry.path);
        }
    }
    Ok(())
}

fn verify_restored_database(
    stage: &Path,
    manifest: &Manifest,
    witness: &CipherWitness,
) -> Result<()> {
    let resolved = resolve_cipher(stage, witness)?;
    open_witness(resolved.cipher.as_ref(), &manifest.cipher.witness)?;
    let inspection = inspect_sqlite(stage, Some(resolved.cipher.as_ref()))?;
    let legacy = inspection.legacy_unauthenticated;
    if legacy != (manifest.cipher.legacy_ciphertext == "present-not-proof") {
        bail!("snapshot legacy ciphertext state does not match the database");
    }
    let identity = inspection.identity;
    match (
        identity,
        manifest.schema_version,
        manifest.sqlite_user_version,
    ) {
        (None, None, None) => Ok(()),
        (Some(actual), Some(schema), Some(user_version)) => {
            if actual.schema_version != schema || actual.user_version != user_version {
                bail!("snapshot database identity does not match the manifest");
            }
            if actual.schema_version > i64::from(crate::db::CURRENT_SCHEMA_VERSION) {
                bail!(
                    "snapshot schema version {} is newer than this build supports ({})",
                    actual.schema_version,
                    crate::db::CURRENT_SCHEMA_VERSION
                );
            }
            Ok(())
        }
        _ => bail!("snapshot database identity does not match the manifest"),
    }
}

fn verify_restore_source(target: &Path, manifest: &Manifest) -> Result<()> {
    if same_path(&portable_data_dir(target), &manifest.source_data_dir) {
        bail!("restore target is the snapshot source data directory");
    }
    Ok(())
}

fn remove_manifest_file(stage: &Path) -> Result<()> {
    let path = stage.join(MANIFEST_NAME);
    if path_is_reparse(&path)? {
        bail!("snapshot manifest is a reparse point");
    }
    fs::remove_file(&path).context("remove snapshot manifest from stage")?;
    Ok(())
}

fn clear_runtime_markers(stage: &Path) -> Result<()> {
    // The versioned child is an install cache. A restored projection is not a
    // child this process has installed.
    let versions = stage.join("cpa").join("versions");
    match fs::symlink_metadata(&versions) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Ok(metadata) if is_reparse_metadata(&metadata) => {
            bail!("restored CPA child cache is a reparse point")
        }
        Ok(metadata) if metadata.is_dir() => {
            fs::remove_dir_all(&versions).context("remove restored CPA child cache")?;
        }
        Ok(_) => bail!("restored CPA child cache is not a directory"),
        Err(error) => return Err(error).context("inspect restored CPA child cache"),
    }
    for name in EXCLUDED_ROOT_FILES {
        let path = stage.join(name);
        match fs::symlink_metadata(&path) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Ok(metadata) if is_reparse_metadata(&metadata) => {
                bail!("runtime marker is a reparse point")
            }
            Ok(metadata) if metadata.is_file() => {
                fs::remove_file(&path).with_context(|| format!("remove runtime marker {name}"))?;
            }
            Ok(_) => bail!("runtime marker is not a file"),
            Err(error) => return Err(error).context("inspect runtime marker"),
        }
    }
    Ok(())
}

fn regular_file_exists(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Ok(metadata) if is_reparse_metadata(&metadata) => {
            bail!("refusing a reparse point: {}", path.display())
        }
        Ok(metadata) => Ok(metadata.is_file()),
        Err(error) => Err(error).with_context(|| format!("inspect {}", path.display())),
    }
}

fn copy_regular_file(from: &Path, to: &Path) -> Result<()> {
    if path_is_reparse(from)? {
        bail!("refusing a reparse point: {}", from.display());
    }
    let mut input = File::open(from).with_context(|| format!("read {}", from.display()))?;
    let mut output = create_private_file(to)?;
    let mut total = 0u64;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = input.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        total = total
            .checked_add(read as u64)
            .filter(|total| *total <= MAX_FILE_BYTES)
            .context("snapshot file exceeds 2 GiB")?;
        output.write_all(&buffer[..read])?;
    }
    output.sync_all()?;
    let _ = total;
    Ok(())
}

fn ensure_dir_inside(stage: &Path, directory: &Path) -> Result<()> {
    if directory == stage {
        return Ok(());
    }
    if !directory.starts_with(stage) {
        bail!("refusing to create a directory outside the restore stage");
    }
    if let Some(parent) = directory.parent() {
        ensure_dir_inside(stage, parent)?;
    }
    match fs::symlink_metadata(directory) {
        Ok(metadata) => {
            if is_reparse_metadata(&metadata) || !metadata.is_dir() {
                bail!("restore path is not a real directory");
            }
            Ok(())
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            create_private_dir(directory)?;
            if path_is_reparse(directory)? {
                bail!("restore path resolved to a reparse point");
            }
            Ok(())
        }
        Err(error) => Err(error).context("create restore directory"),
    }
}

fn create_private_dir(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        fs::DirBuilder::new()
            .mode(0o700)
            .create(path)
            .with_context(|| format!("create {}", path.display()))?;
    }
    #[cfg(not(unix))]
    {
        fs::create_dir(path).with_context(|| format!("create {}", path.display()))?;
    }
    tighten_dir(path)?;
    Ok(())
}

fn create_private_file(path: &Path) -> Result<File> {
    let file = {
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(path)
        }
        #[cfg(not(unix))]
        {
            OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .open(path)
        }
    }
    .with_context(|| format!("create {}", path.display()))?;
    if let Err(error) = private_file_permissions(path) {
        let _ = fs::remove_file(path);
        return Err(error);
    }
    Ok(file)
}

fn private_file_permissions(path: &Path) -> Result<()> {
    #[cfg(test)]
    if FAIL_OWNED_FILE_PERMISSION.with(|cell| cell.get()) {
        bail!("private permission setup failed");
    }
    tighten_file(path)
}

fn tighten_dir(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .with_context(|| format!("set permissions on {}", path.display()))?;
    }
    #[cfg(windows)]
    {
        crate::private_file::set_private_permissions(path).map_err(|error| {
            anyhow::anyhow!(
                "failed to set private permissions on {}: {error}",
                path.display()
            )
        })?;
    }
    Ok(())
}

fn tighten_file(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))
            .with_context(|| format!("set permissions on {}", path.display()))?;
    }
    #[cfg(windows)]
    {
        crate::private_file::set_private_permissions(path).map_err(|error| {
            anyhow::anyhow!(
                "failed to set private permissions on {}: {error}",
                path.display()
            )
        })?;
    }
    Ok(())
}

fn remove_tree(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Ok(metadata) if metadata.is_dir() && !is_reparse_metadata(&metadata) => {
            fs::remove_dir_all(path)
        }
        Ok(_) => fs::remove_file(path),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
thread_local! {
    static FAIL_OWNED_FILE_PERMISSION: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static PUBLISH_BEFORE: std::cell::Cell<Option<fn(&Path)>> = const { std::cell::Cell::new(None) };
    static PUBLISH_AFTER_EMPTY_REMOVAL: std::cell::Cell<Option<fn(&Path)>> = const { std::cell::Cell::new(None) };
}

#[cfg(test)]
fn invoke_publish_before(target: &Path) {
    if let Some(hook) = PUBLISH_BEFORE.with(|cell| cell.get()) {
        hook(target);
    }
}

#[cfg(test)]
fn invoke_publish_after_empty_removal(target: &Path) {
    if let Some(hook) = PUBLISH_AFTER_EMPTY_REMOVAL.with(|cell| cell.get()) {
        hook(target);
    }
}

#[cfg(test)]
fn set_fail_owned_file_permission(fail: bool) {
    FAIL_OWNED_FILE_PERMISSION.with(|cell| cell.set(fail));
}

#[cfg(test)]
fn set_publish_hooks(before: Option<fn(&Path)>, after: Option<fn(&Path)>) {
    PUBLISH_BEFORE.with(|cell| cell.set(before));
    PUBLISH_AFTER_EMPTY_REMOVAL.with(|cell| cell.set(after));
}

#[cfg(test)]
#[path = "backup/tests.rs"]
mod tests;

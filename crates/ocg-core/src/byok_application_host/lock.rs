//! Client-compatible cross-process locks. Codex/Kimi have no file protocol.
//!
//! MiniMax uses an empty `{config}.lock` directory plus a sibling
//! `{config}.lock.ocg-owner` file that records pid, token, and directory
//! identity (Unix dev+ino, Windows volume/file index). Heartbeat updates
//! mtime through the captured directory handle. Path identity and sidecar
//! ownership are checked before mutate, heartbeat, release, and reclaim.
//! After mkdir, a proven-dead OCG sidecar from a former directory may be
//! retired before writing our owner. Successors and live/foreign/unparseable
//! sidecars are left untouched. A lock directory left empty with no sidecar at
//! all is the crash window between mkdir and the owner write; it is reclaimed
//! once it has sat untouched well past one full acquire wait.
//! ZCode uses `{config}.lock/owner-ocg-*.json` and reclaims only those
//! markers with a proven-dead PID.
use super::fs::{canonical_lexical_path, io_internal};
use super::{ByokError, ByokResult};
use crate::byok_application::ByokClient;
use serde_json::{Value, json};
use std::fs::{self, File, OpenOptions};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const OCG_OWNER_PREFIX: &str = "owner-ocg-";
const OCG_KIND: &str = "ocg";

pub struct CrossProcessLock {
    kind: LockKind,
}

enum LockKind {
    None,
    MiniMax {
        lock_dir: PathBuf,
        sidecar: PathBuf,
        identity: DirId,
        token: String,
        handle: File,
        stop: Arc<AtomicBool>,
        failed: Arc<AtomicBool>,
        heartbeat: Option<JoinHandle<()>>,
    },
    Zcode {
        lock_dir: PathBuf,
        owner_file: PathBuf,
    },
}

impl CrossProcessLock {
    pub fn acquire(client: ByokClient, target: &Path, policy: &LockPolicy) -> ByokResult<Self> {
        match client {
            ByokClient::Codex | ByokClient::Kimi => Ok(Self {
                kind: LockKind::None,
            }),
            ByokClient::Minimax => acquire_minimax(target, policy),
            ByokClient::Zcode => acquire_zcode(target, policy),
        }
    }

    pub fn assert_held(&self) -> ByokResult<()> {
        match &self.kind {
            LockKind::None => Ok(()),
            LockKind::MiniMax {
                lock_dir,
                sidecar,
                identity,
                token,
                handle,
                failed,
                ..
            } => {
                if failed.load(Ordering::SeqCst) {
                    return Err(ByokError::internal(
                        "MiniMax lock heartbeat failed; mutual exclusion is not guaranteed",
                    ));
                }
                if !owns_current_directory(handle, *identity, lock_dir, sidecar, token) {
                    return Err(ByokError::internal(
                        "MiniMax lock no longer identifies this directory",
                    ));
                }
                Ok(())
            }
            LockKind::Zcode {
                lock_dir,
                owner_file,
            } => {
                if !lock_dir.is_dir() || !owner_file.is_file() {
                    return Err(ByokError::internal("ZCode lock is no longer held"));
                }
                Ok(())
            }
        }
    }
}

impl Drop for CrossProcessLock {
    fn drop(&mut self) {
        match &mut self.kind {
            LockKind::None => {}
            LockKind::MiniMax {
                lock_dir,
                sidecar,
                identity,
                token,
                handle,
                stop,
                heartbeat,
                ..
            } => {
                stop.store(true, Ordering::SeqCst);
                if let Some(worker) = heartbeat.take() {
                    worker.thread().unpark();
                    let _ = worker.join();
                }
                if owns_current_directory(handle, *identity, lock_dir, sidecar, token) {
                    let _ = fs::remove_file(sidecar);
                    let _ = fs::remove_dir(lock_dir);
                }
            }
            LockKind::Zcode {
                lock_dir,
                owner_file,
            } => {
                let _ = fs::remove_file(owner_file);
                let _ = fs::remove_dir(lock_dir);
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct LockPolicy {
    pub minimax_retry: Duration,
    pub minimax_max_wait: Duration,
    pub minimax_heartbeat: Duration,
    pub zcode_retry_delays_ms: Vec<u64>,
    pub zcode_max_wait: Duration,
}

impl Default for LockPolicy {
    fn default() -> Self {
        Self {
            minimax_retry: Duration::from_millis(25),
            minimax_max_wait: Duration::from_millis(10_000),
            minimax_heartbeat: Duration::from_millis(2_000),
            zcode_retry_delays_ms: vec![25, 50, 100, 200, 400],
            zcode_max_wait: Duration::from_millis(8_000),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DirId {
    #[cfg(unix)]
    dev: u64,
    #[cfg(unix)]
    ino: u64,
    #[cfg(windows)]
    volume: u32,
    #[cfg(windows)]
    index_high: u32,
    #[cfg(windows)]
    index_low: u32,
}

pub(crate) fn lock_dir_for(target: &Path) -> ByokResult<PathBuf> {
    let target = canonical_lexical_path(target)?;
    let mut lock = target.into_os_string();
    lock.push(".lock");
    Ok(PathBuf::from(lock))
}

pub(crate) fn ocg_sidecar_for(lock_dir: &Path) -> PathBuf {
    let mut sidecar = lock_dir.as_os_str().to_os_string();
    sidecar.push(".ocg-owner");
    PathBuf::from(sidecar)
}

fn acquire_minimax(target: &Path, policy: &LockPolicy) -> ByokResult<CrossProcessLock> {
    let lock_dir = lock_dir_for(target)?;
    let sidecar = ocg_sidecar_for(&lock_dir);
    let started = Instant::now();
    loop {
        match fs::create_dir(&lock_dir) {
            Ok(()) => return finish_minimax_hold(lock_dir.clone(), sidecar.clone(), policy),
            Err(error) if error.kind() == ErrorKind::AlreadyExists => {
                if reclaim_dead_ocg_minimax(&lock_dir, &sidecar)?
                    || reclaim_abandoned_minimax(&lock_dir, &sidecar, policy)?
                {
                    continue;
                }
                if started.elapsed() >= policy.minimax_max_wait {
                    return Err(ByokError::precondition(
                        "MiniMax configuration is locked by another process",
                    ));
                }
                thread::sleep(policy.minimax_retry);
            }
            Err(error) => return Err(io_internal(error)),
        }
    }
}

fn finish_minimax_hold(
    lock_dir: PathBuf,
    sidecar: PathBuf,
    policy: &LockPolicy,
) -> ByokResult<CrossProcessLock> {
    let handle = match open_directory_for_times(&lock_dir) {
        Ok(handle) => handle,
        Err(error) => {
            let _ = fs::remove_dir(&lock_dir);
            return Err(io_internal(error));
        }
    };
    let identity = match directory_identity(&handle) {
        Ok(identity) => identity,
        Err(error) => {
            let _ = fs::remove_dir(&lock_dir);
            return Err(error);
        }
    };
    if !captured_dir_still_current(&handle, identity, &lock_dir) {
        return Err(ByokError::internal(
            "MiniMax lock no longer identifies this directory",
        ));
    }
    if let Err(error) = retire_orphan_former_sidecar(&lock_dir, &sidecar, &handle, identity) {
        remove_dir_if_current(&lock_dir, &handle, identity);
        return Err(error);
    }
    if !captured_dir_still_current(&handle, identity, &lock_dir) {
        return Err(ByokError::internal(
            "MiniMax lock no longer identifies this directory",
        ));
    }
    let token = uuid::Uuid::new_v4().simple().to_string();
    if let Err(error) = write_ocg_owner_file(&sidecar, &token, identity) {
        remove_dir_if_current(&lock_dir, &handle, identity);
        return Err(error);
    }
    let cleanup = match handle.try_clone().map_err(io_internal) {
        Ok(handle) => handle,
        Err(error) => {
            let _ = fs::remove_file(&sidecar);
            remove_dir_if_current(&lock_dir, &handle, identity);
            return Err(error);
        }
    };
    match complete_minimax_hold(
        lock_dir.clone(),
        sidecar.clone(),
        identity,
        token,
        handle,
        policy,
    ) {
        Ok(held) => Ok(held),
        Err(error) => {
            let _ = fs::remove_file(&sidecar);
            remove_dir_if_current(&lock_dir, &cleanup, identity);
            Err(error)
        }
    }
}

fn complete_minimax_hold(
    lock_dir: PathBuf,
    sidecar: PathBuf,
    identity: DirId,
    token: String,
    handle: File,
    policy: &LockPolicy,
) -> ByokResult<CrossProcessLock> {
    handle
        .set_modified(SystemTime::now())
        .map_err(io_internal)?;
    let heartbeat_handle = handle.try_clone().map_err(io_internal)?;
    let stop = Arc::new(AtomicBool::new(false));
    let failed = Arc::new(AtomicBool::new(false));
    let heartbeat = spawn_mtime_heartbeat(Heartbeat {
        handle: heartbeat_handle,
        identity,
        lock_dir: lock_dir.clone(),
        sidecar: sidecar.clone(),
        token: token.clone(),
        stop: stop.clone(),
        failed: failed.clone(),
        interval: policy.minimax_heartbeat,
    });
    let held = CrossProcessLock {
        kind: LockKind::MiniMax {
            lock_dir,
            sidecar,
            identity,
            token,
            handle,
            stop,
            failed,
            heartbeat: Some(heartbeat),
        },
    };
    held.assert_held()?;
    Ok(held)
}

fn reclaim_dead_ocg_minimax(lock_dir: &Path, sidecar: &Path) -> ByokResult<bool> {
    let Some(owner) = read_ocg_owner(sidecar)? else {
        return Ok(false);
    };
    if process_alive(owner.pid) {
        return Ok(false);
    }
    let Some(current) = path_dir_id(lock_dir)? else {
        return Ok(false);
    };
    if current != owner.identity {
        return Ok(false);
    }
    match fs::read_dir(lock_dir) {
        Ok(entries) => {
            if entries.filter_map(Result::ok).next().is_some() {
                return Ok(false);
            }
        }
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(io_internal(error)),
    }
    if path_dir_id(lock_dir)? != Some(owner.identity) {
        return Ok(false);
    }
    let _ = fs::remove_dir(lock_dir);
    if lock_dir.exists() {
        return Ok(false);
    }
    if sidecar_matches(sidecar, &owner.token, owner.identity) {
        let _ = fs::remove_file(sidecar);
    }
    Ok(true)
}

/// Recovers the crash window where `{config}.lock` was created but the process
/// died before `.ocg-owner` was written. That directory has no sidecar and no
/// entries, so nothing identifies an owner and `reclaim_dead_ocg_minimax`
/// cannot touch it. A live holder heartbeats the mtime every
/// `minimax_heartbeat`, so a directory untouched for `minimax_max_wait * 2` is
/// not a holder waiting out its own acquire.
fn reclaim_abandoned_minimax(
    lock_dir: &Path,
    sidecar: &Path,
    policy: &LockPolicy,
) -> ByokResult<bool> {
    if sidecar.exists() {
        return Ok(false);
    }
    match fs::read_dir(lock_dir) {
        Ok(entries) => {
            if entries.filter_map(Result::ok).next().is_some() {
                return Ok(false);
            }
        }
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(io_internal(error)),
    }
    let stale_after = policy
        .minimax_max_wait
        .checked_mul(2)
        .unwrap_or(policy.minimax_max_wait);
    let modified = fs::metadata(lock_dir).and_then(|meta| meta.modified());
    let abandoned = match modified {
        Ok(modified) => modified.elapsed().is_ok_and(|age| age >= stale_after),
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(false),
        // mtime is the only signal available for an anonymous directory, so an
        // unreadable one is left for its owner rather than deleted on a guess.
        Err(_) => return Ok(false),
    };
    if !abandoned {
        return Ok(false);
    }
    if sidecar.exists() {
        return Ok(false);
    }
    match fs::read_dir(lock_dir) {
        Ok(entries) => {
            if entries.filter_map(Result::ok).next().is_some() {
                return Ok(false);
            }
        }
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(io_internal(error)),
    }
    let _ = fs::remove_dir(lock_dir);
    Ok(!lock_dir.exists())
}

fn acquire_zcode(target: &Path, policy: &LockPolicy) -> ByokResult<CrossProcessLock> {
    let lock_dir = lock_dir_for(target)?;
    let token = uuid::Uuid::new_v4().simple().to_string();
    let owner_file = lock_dir.join(format!(
        "{OCG_OWNER_PREFIX}{}-{token}.json",
        std::process::id()
    ));
    let started = Instant::now();
    let mut attempt = 0usize;
    loop {
        match try_create_zcode_lock(&lock_dir, &owner_file) {
            Ok(()) => {
                return Ok(CrossProcessLock {
                    kind: LockKind::Zcode {
                        lock_dir,
                        owner_file,
                    },
                });
            }
            Err(error)
                if matches!(
                    error.kind,
                    crate::byok_application::ByokErrorKind::Precondition
                        | crate::byok_application::ByokErrorKind::Conflict
                ) =>
            {
                let _ = fs::remove_file(&owner_file);
                if reclaim_dead_ocg_zcode(&lock_dir)? {
                    continue;
                }
            }
            Err(error) => return Err(error),
        }
        let elapsed = started.elapsed();
        if elapsed >= policy.zcode_max_wait {
            return Err(ByokError::precondition(
                "ZCode configuration is locked by another process",
            ));
        }
        let delay_ms = policy
            .zcode_retry_delays_ms
            .get(attempt.min(policy.zcode_retry_delays_ms.len().saturating_sub(1)))
            .copied()
            .unwrap_or(400);
        attempt = attempt.saturating_add(1);
        let remaining = policy.zcode_max_wait.saturating_sub(elapsed);
        thread::sleep(Duration::from_millis(delay_ms).min(remaining));
    }
}

fn try_create_zcode_lock(lock_dir: &Path, owner_file: &Path) -> ByokResult<()> {
    fs::create_dir(lock_dir).map_err(|error| {
        if error.kind() == ErrorKind::AlreadyExists {
            ByokError::precondition("ZCode configuration is locked by another process")
        } else {
            io_internal(error)
        }
    })?;
    let token = uuid::Uuid::new_v4().simple().to_string();
    let identity = match open_directory_for_times(lock_dir) {
        Ok(handle) => directory_identity(&handle)?,
        Err(error) => {
            let _ = fs::remove_dir(lock_dir);
            return Err(io_internal(error));
        }
    };
    if let Err(error) = write_ocg_owner_file(owner_file, &token, identity) {
        let _ = fs::remove_file(owner_file);
        let _ = fs::remove_dir(lock_dir);
        return Err(error);
    }
    let owners = list_lock_entries(lock_dir)?;
    let expected = owner_file
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    if owners.len() != 1 || owners[0] != expected {
        let _ = fs::remove_file(owner_file);
        let _ = fs::remove_dir(lock_dir);
        return Err(ByokError::conflict(
            "ZCode file lock ownership changed during acquire",
        ));
    }
    Ok(())
}

fn reclaim_dead_ocg_zcode(lock_dir: &Path) -> ByokResult<bool> {
    let Ok(metadata) = fs::symlink_metadata(lock_dir) else {
        return Ok(false);
    };
    if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
        return Ok(false);
    }
    let entries = list_lock_entries(lock_dir)?;
    if entries.is_empty() {
        return Ok(false);
    }
    let mut ocg_files = Vec::new();
    for name in &entries {
        if !name.starts_with(OCG_OWNER_PREFIX) {
            return Ok(false);
        }
        let path = lock_dir.join(name);
        let Some(owner) = read_ocg_owner(&path)? else {
            return Ok(false);
        };
        if process_alive(owner.pid) {
            return Ok(false);
        }
        ocg_files.push(path);
    }
    for path in ocg_files {
        let _ = fs::remove_file(path);
    }
    let _ = fs::remove_dir(lock_dir);
    Ok(!lock_dir.exists())
}

struct Heartbeat {
    handle: File,
    identity: DirId,
    lock_dir: PathBuf,
    sidecar: PathBuf,
    token: String,
    stop: Arc<AtomicBool>,
    failed: Arc<AtomicBool>,
    interval: Duration,
}

fn spawn_mtime_heartbeat(beat: Heartbeat) -> JoinHandle<()> {
    let Heartbeat {
        handle,
        identity,
        lock_dir,
        sidecar,
        token,
        stop,
        failed,
        interval,
    } = beat;
    thread::spawn(move || {
        while !stop.load(Ordering::SeqCst) {
            thread::park_timeout(interval);
            if stop.load(Ordering::SeqCst) {
                break;
            }
            if !owns_current_directory(&handle, identity, &lock_dir, &sidecar, &token)
                || handle.set_modified(SystemTime::now()).is_err()
            {
                failed.store(true, Ordering::SeqCst);
                break;
            }
        }
    })
}

#[cfg(test)]
pub(crate) fn set_directory_mtime(path: &Path) -> ByokResult<()> {
    let file = open_directory_for_times(path).map_err(io_internal)?;
    file.set_modified(SystemTime::now()).map_err(io_internal)
}

#[cfg(test)]
pub(crate) fn backdate_directory_mtime(path: &Path, when: SystemTime) -> ByokResult<()> {
    let file = open_directory_for_times(path).map_err(io_internal)?;
    file.set_modified(when).map_err(io_internal)
}

#[cfg(test)]
pub(crate) fn capture_dir_id(path: &Path) -> ByokResult<DirId> {
    let handle = open_directory_for_times(path).map_err(io_internal)?;
    directory_identity(&handle)
}

#[cfg(test)]
pub(crate) fn sidecar_json(pid: u32, token: &str, identity: DirId) -> String {
    format!("{}\n", identity.to_json(pid, token))
}

fn open_directory_for_times(path: &Path) -> std::io::Result<File> {
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
        const FILE_READ_ATTRIBUTES: u32 = 0x0080;
        const FILE_WRITE_ATTRIBUTES: u32 = 0x0100;
        const FILE_SHARE_READ: u32 = 0x1;
        const FILE_SHARE_WRITE: u32 = 0x2;
        const FILE_SHARE_DELETE: u32 = 0x4;
        OpenOptions::new()
            .access_mode(FILE_READ_ATTRIBUTES | FILE_WRITE_ATTRIBUTES)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(path)
    }
    #[cfg(not(windows))]
    {
        File::open(path)
    }
}

fn directory_identity(file: &File) -> ByokResult<DirId> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let meta = file.metadata().map_err(io_internal)?;
        Ok(DirId {
            dev: meta.dev(),
            ino: meta.ino(),
        })
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Foundation::HANDLE;
        use windows_sys::Win32::Storage::FileSystem::{
            BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
        };
        let mut info = unsafe { std::mem::zeroed::<BY_HANDLE_FILE_INFORMATION>() };
        let ok = unsafe { GetFileInformationByHandle(file.as_raw_handle() as HANDLE, &mut info) };
        if ok == 0 {
            return Err(io_internal(std::io::Error::last_os_error()));
        }
        Ok(DirId {
            volume: info.dwVolumeSerialNumber,
            index_high: info.nFileIndexHigh,
            index_low: info.nFileIndexLow,
        })
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = file;
        Err(ByokError::internal("directory identity is unsupported"))
    }
}

fn path_dir_id(path: &Path) -> ByokResult<Option<DirId>> {
    match open_directory_for_times(path) {
        Ok(handle) => directory_identity(&handle).map(Some),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(io_internal(error)),
    }
}

fn captured_dir_still_current(handle: &File, identity: DirId, lock_dir: &Path) -> bool {
    matches!(directory_identity(handle), Ok(id) if id == identity)
        && matches!(path_dir_id(lock_dir), Ok(Some(id)) if id == identity)
}

fn remove_dir_if_current(lock_dir: &Path, handle: &File, identity: DirId) {
    if captured_dir_still_current(handle, identity, lock_dir) {
        let _ = fs::remove_dir(lock_dir);
    }
}

fn retire_orphan_former_sidecar(
    lock_dir: &Path,
    sidecar: &Path,
    handle: &File,
    current: DirId,
) -> ByokResult<()> {
    if !captured_dir_still_current(handle, current, lock_dir) {
        return Ok(());
    }
    let Some(owner) = read_ocg_owner(sidecar)? else {
        return Ok(());
    };
    if process_alive(owner.pid) {
        return Ok(());
    }
    if !captured_dir_still_current(handle, current, lock_dir) {
        return Ok(());
    }
    if process_alive(owner.pid) || !sidecar_matches(sidecar, &owner.token, owner.identity) {
        return Ok(());
    }
    if !captured_dir_still_current(handle, current, lock_dir) {
        return Ok(());
    }
    let _ = fs::remove_file(sidecar);
    Ok(())
}

fn owns_current_directory(
    handle: &File,
    identity: DirId,
    lock_dir: &Path,
    sidecar: &Path,
    token: &str,
) -> bool {
    match directory_identity(handle) {
        Ok(from_handle) if from_handle == identity => {}
        _ => return false,
    }
    match path_dir_id(lock_dir) {
        Ok(Some(from_path)) if from_path == identity => {}
        _ => return false,
    }
    sidecar_matches(sidecar, token, identity)
}

fn sidecar_matches(sidecar: &Path, token: &str, identity: DirId) -> bool {
    matches!(
        read_ocg_owner(sidecar),
        Ok(Some(owner)) if owner.token == token && owner.identity == identity
    )
}

struct OcgOwner {
    pid: u32,
    token: String,
    identity: DirId,
}

impl DirId {
    fn to_json(self, pid: u32, token: &str) -> Value {
        #[cfg(unix)]
        {
            json!({
                "kind": OCG_KIND,
                "pid": pid,
                "token": token,
                "createdAt": now_ms(),
                "dev": self.dev,
                "ino": self.ino,
            })
        }
        #[cfg(windows)]
        {
            json!({
                "kind": OCG_KIND,
                "pid": pid,
                "token": token,
                "createdAt": now_ms(),
                "volume": self.volume,
                "indexHigh": self.index_high,
                "indexLow": self.index_low,
            })
        }
        #[cfg(not(any(unix, windows)))]
        {
            json!({
                "kind": OCG_KIND,
                "pid": pid,
                "token": token,
                "createdAt": now_ms(),
            })
        }
    }

    fn from_json(value: &Value) -> Option<Self> {
        #[cfg(unix)]
        {
            Some(Self {
                dev: value.get("dev")?.as_u64()?,
                ino: value.get("ino")?.as_u64()?,
            })
        }
        #[cfg(windows)]
        {
            Some(Self {
                volume: value.get("volume")?.as_u64()? as u32,
                index_high: value.get("indexHigh")?.as_u64()? as u32,
                index_low: value.get("indexLow")?.as_u64()? as u32,
            })
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = value;
            None
        }
    }
}

fn write_ocg_owner_file(path: &Path, token: &str, identity: DirId) -> ByokResult<()> {
    let payload = format!("{}\n", identity.to_json(std::process::id(), token));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path).map_err(io_internal)?;
    use std::io::Write;
    file.write_all(payload.as_bytes()).map_err(io_internal)
}

fn read_ocg_owner(path: &Path) -> ByokResult<Option<OcgOwner>> {
    let Some(bytes) = super::fs::read_regular_file(path)? else {
        return Ok(None);
    };
    let parsed: Value = match serde_json::from_slice(&bytes) {
        Ok(value) => value,
        Err(_) => return Ok(None),
    };
    if parsed.get("kind").and_then(Value::as_str) != Some(OCG_KIND) {
        return Ok(None);
    }
    let Some(pid) = parsed
        .get("pid")
        .and_then(Value::as_u64)
        .filter(|pid| *pid > 0 && *pid <= u64::from(u32::MAX))
    else {
        return Ok(None);
    };
    let Some(token) = parsed.get("token").and_then(Value::as_str) else {
        return Ok(None);
    };
    let Some(identity) = DirId::from_json(&parsed) else {
        return Ok(None);
    };
    Ok(Some(OcgOwner {
        pid: pid as u32,
        token: token.to_string(),
        identity,
    }))
}

fn list_lock_entries(lock_dir: &Path) -> ByokResult<Vec<String>> {
    let mut names = Vec::new();
    let entries = match fs::read_dir(lock_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(names),
        Err(error) => return Err(io_internal(error)),
    };
    for entry in entries {
        let entry = entry.map_err(io_internal)?;
        names.push(entry.file_name().to_string_lossy().into_owned());
    }
    Ok(names)
}

fn process_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        use nix::errno::Errno;
        use nix::sys::signal::{Signal, kill};
        use nix::unistd::Pid;
        match kill(Pid::from_raw(pid as i32), None as Option<Signal>) {
            Ok(()) => true,
            Err(Errno::ESRCH) => false,
            Err(_) => true,
        }
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::{CloseHandle, ERROR_INVALID_PARAMETER};
        use windows_sys::Win32::System::Threading::{
            OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
        };
        let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        if handle.is_null() {
            return std::io::Error::last_os_error().raw_os_error()
                != Some(ERROR_INVALID_PARAMETER as i32);
        }
        unsafe { CloseHandle(handle) };
        true
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = pid;
        true
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
#[path = "lock/tests.rs"]
mod tests;

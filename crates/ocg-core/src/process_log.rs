//! Program diagnostics on stderr, with an optional rotating file sink.
//!
//! Executable hosts install the subscriber before startup. Libraries and
//! CoreState construction never change the caller's tracing subscriber.
//! The file sink starts later, and only when the desktop owner or the native
//! CLI asks for it. It does not replace stderr. One sanitized buffer is what
//! both outputs receive.

use std::fs::{DirBuilder, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard};

use fs2::FileExt;
use tracing_subscriber::EnvFilter;

const DEFAULT_FILTER: &str = "warn,ocg=info";
const MAX_PROGRAM_LOG_BYTES: u64 = 10 * 1024 * 1024;
const TRUNCATION_MARKER: &[u8] = b"\n[truncated]\n";
const LOCK_FILE_NAME: &str = ".program-log.lock";
const BODY_LABELS: &[&str] = &[
    "request_body",
    "response_body",
    "upstream_body",
    "error_body",
    "payload",
    "body",
];

static SUBSCRIBER_INSTALLED: AtomicBool = AtomicBool::new(false);
static NOTICE_SENT: AtomicBool = AtomicBool::new(false);
static SINK: Mutex<Option<FileSink>> = Mutex::new(None);

/// Initialize process diagnostics. An embedding host's existing subscriber
/// remains authoritative; repeated calls are harmless.
pub fn init() {
    let configured = std::env::var("RUST_LOG").ok();
    let (filter, invalid) = filter(configured.as_deref());
    let installed = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(TeeWriter::new)
        .log_internal_errors(false)
        .with_ansi(false)
        .compact()
        .try_init()
        .is_ok();
    if installed {
        SUBSCRIBER_INSTALLED.store(true, Ordering::Release);
    }
    if installed && invalid {
        tracing::warn!("invalid RUST_LOG; using the default program log filter");
    }
}

/// Append sanitized diagnostics to `<data-dir>/logs/program.log`.
///
/// Best effort: contention, permission errors, and a closed stderr leave the
/// process running. `OCG_PROGRAM_LOG_FILE=off` and a process that did not
/// install this subscriber stay stderr-only. A second call is a no-op.
pub fn activate_file_sink(data_dir: &Path) {
    if !SUBSCRIBER_INSTALLED.load(Ordering::Acquire) || program_log_file_off() {
        return;
    }
    let mut guard = sink_lock();
    if guard.is_some() {
        return;
    }
    match FileSink::acquire(data_dir) {
        Ok(sink) => *guard = Some(sink),
        Err(_) => {
            drop(guard);
            notice();
        }
    }
}

fn filter(value: Option<&str>) -> (EnvFilter, bool) {
    match value.filter(|value| !value.trim().is_empty()) {
        Some(value) => match EnvFilter::try_new(value) {
            Ok(filter) => (filter, false),
            Err(_) => (EnvFilter::new(DEFAULT_FILTER), true),
        },
        None => (EnvFilter::new(DEFAULT_FILTER), false),
    }
}

/// Program-only diagnostics for hosts with dynamically selected severity.
/// Callers supply metadata, never request bodies or credentials.
pub fn diagnostic(level: &str, category: &str, message: impl std::fmt::Display) {
    match crate::runtime_log::Level::parse(level) {
        Some(level) => event(level, category, message),
        None => tracing::warn!(category, "invalid program event severity"),
    }
}

/// Dynamic-severity host events contain sanitized metadata, never content.
pub(crate) fn event(
    level: crate::runtime_log::Level,
    category: &str,
    message: impl std::fmt::Display,
) {
    let message = crate::redaction::redact_text(&message.to_string());
    use crate::runtime_log::Level;
    match level {
        Level::Trace => tracing::trace!(category, "{message}"),
        Level::Debug => tracing::debug!(category, "{message}"),
        Level::Info => tracing::info!(category, "{message}"),
        Level::Warn => tracing::warn!(category, "{message}"),
        Level::Error => tracing::error!(category, "{message}"),
    }
}

fn program_log_file_off() -> bool {
    std::env::var("OCG_PROGRAM_LOG_FILE")
        .ok()
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("off"))
}

fn sink_lock() -> MutexGuard<'static, Option<FileSink>> {
    SINK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn notice() {
    if NOTICE_SENT.swap(true, Ordering::Relaxed) {
        return;
    }
    let mut stderr = std::io::stderr();
    let _ = stderr.write_all(b"ocg: program log file disabled; stderr diagnostics continue\n");
    let _ = stderr.flush();
}

fn commit_formatted(bytes: &[u8]) {
    if bytes.is_empty() {
        return;
    }
    let mut guard = sink_lock();
    let Some(sink) = guard.as_mut() else {
        return;
    };
    if sink.disabled {
        return;
    }
    sink.commit(bytes);
    let disabled = sink.disabled;
    drop(guard);
    if disabled {
        notice();
    }
}

struct TeeWriter {
    stderr: io::Stderr,
    buffer: Vec<u8>,
    truncated: bool,
}

impl TeeWriter {
    fn new() -> Self {
        Self {
            stderr: io::stderr(),
            buffer: Vec::new(),
            truncated: false,
        }
    }
}

impl Write for TeeWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if buf.is_empty() || self.truncated {
            return Ok(buf.len());
        }
        let max = max_event_bytes();
        let room = max.saturating_sub(self.buffer.len());
        if buf.len() <= room {
            self.buffer.extend_from_slice(buf);
        } else {
            self.buffer.extend_from_slice(&buf[..room]);
            mark_truncated(&mut self.buffer);
            self.truncated = true;
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        if self.buffer.is_empty() {
            self.truncated = false;
            return Ok(());
        }
        let buffered = std::mem::take(&mut self.buffer);
        self.truncated = false;
        let sanitized = sanitize_event(&buffered);
        let _ = self.stderr.write_all(&sanitized);
        let _ = self.stderr.flush();
        commit_formatted(&sanitized);
        Ok(())
    }
}

impl Drop for TeeWriter {
    fn drop(&mut self) {
        let _ = self.flush();
    }
}

struct FileSink {
    directory: PathBuf,
    // Held for the process lifetime. Closing it releases the family lock.
    // The lock file itself stays on disk.
    _lock: File,
    active: Option<File>,
    active_len: u64,
    disabled: bool,
}

impl FileSink {
    fn acquire(data_dir: &Path) -> io::Result<Self> {
        let directory = data_dir.join("logs");
        refuse_reparse_ancestors(&directory)?;
        prepare_directory(&directory)?;
        refuse_family(&directory)?;
        refuse_oversized(&directory)?;
        enforce_private_family(&directory)?;
        let lock_path = directory.join(LOCK_FILE_NAME);
        let lock = open_lock(&lock_path)?;
        lock.try_lock_exclusive()?;
        let active_path = directory.join("program.log");
        let active = open_append(&active_path)?;
        let active_len = active.metadata()?.len();
        if active_len > MAX_PROGRAM_LOG_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "program log family already exceeds 10 MiB",
            ));
        }
        Ok(Self {
            directory,
            _lock: lock,
            active: Some(active),
            active_len,
            disabled: false,
        })
    }

    fn commit(&mut self, raw: &[u8]) {
        if self.disabled || raw.is_empty() {
            return;
        }
        // The formatter has already sanitized and bounded this whole event.
        // Sanitize once so both sinks receive the same record.
        let event = raw;
        if event.len() as u64 > MAX_PROGRAM_LOG_BYTES {
            self.disabled = true;
            return;
        }
        let fits = self.active.is_some()
            && self.active_len.saturating_add(event.len() as u64) <= MAX_PROGRAM_LOG_BYTES;
        if !fits && self.rotate().is_err() {
            self.disabled = true;
            return;
        }
        if self.write_event(event).is_err() {
            self.disabled = true;
        }
    }

    fn rotate(&mut self) -> io::Result<()> {
        self.active.take();
        let rotated = rotate_names(&self.directory);
        let path = self.directory.join("program.log");
        match (rotated, open_append(&path)) {
            (Ok(()), Ok(file)) => {
                self.active_len = file.metadata()?.len();
                self.active = Some(file);
                Ok(())
            }
            (Err(error), Ok(file)) => {
                self.active_len = file.metadata().map(|meta| meta.len()).unwrap_or(0);
                self.active = Some(file);
                Err(error)
            }
            (_, Err(error)) => Err(error),
        }
    }

    fn write_event(&mut self, event: &[u8]) -> io::Result<()> {
        let active = self
            .active
            .as_mut()
            .ok_or_else(|| io::Error::other("program log is closed"))?;
        active.write_all(event)?;
        active.flush()?;
        self.active_len = self.active_len.saturating_add(event.len() as u64);
        Ok(())
    }
}

fn prepare_directory(directory: &Path) -> io::Result<()> {
    match std::fs::symlink_metadata(directory) {
        Ok(meta) if !is_reparse(&meta) && meta.is_dir() => Ok(()),
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "program log directory is not a real directory",
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let mut builder = DirBuilder::new();
            builder.recursive(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            builder.create(directory)
        }
        Err(error) => Err(error),
    }
}

/// `symlink_metadata` does not follow. A linked ancestor fails before create.
fn refuse_reparse_ancestors(directory: &Path) -> io::Result<()> {
    let mut current = directory.to_path_buf();
    loop {
        match std::fs::symlink_metadata(&current) {
            Ok(meta) if is_reparse(&meta) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "program log path has a link or reparse ancestor",
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        if !current.pop() {
            break;
        }
    }
    Ok(())
}

fn refuse_family(directory: &Path) -> io::Result<()> {
    for path in family_paths(directory) {
        refuse_regular_file(&path)?;
    }
    Ok(())
}

/// Leave an already oversized active file or archive untouched.
fn refuse_oversized(directory: &Path) -> io::Result<()> {
    for index in 0..5u32 {
        let path = log_path(directory, index);
        match std::fs::symlink_metadata(&path) {
            Ok(meta)
                if !is_reparse(&meta) && meta.is_file() && meta.len() > MAX_PROGRAM_LOG_BYTES =>
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "program log family already exceeds 10 MiB",
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn enforce_private_family(directory: &Path) -> io::Result<()> {
    crate::fs_privacy::set_private_permissions(directory)?;
    for path in family_paths(directory) {
        match std::fs::symlink_metadata(&path) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
            Ok(meta) if is_reparse(&meta) || !meta.is_file() => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "program log path is not a regular file",
                ));
            }
            Ok(_) => crate::fs_privacy::set_private_permissions(&path)?,
        }
    }
    Ok(())
}

fn family_paths(directory: &Path) -> [PathBuf; 6] {
    [
        directory.join("program.log"),
        directory.join("program.log.1"),
        directory.join("program.log.2"),
        directory.join("program.log.3"),
        directory.join("program.log.4"),
        directory.join(LOCK_FILE_NAME),
    ]
}

fn log_path(directory: &Path, index: u32) -> PathBuf {
    if index == 0 {
        directory.join("program.log")
    } else {
        directory.join(format!("program.log.{index}"))
    }
}

fn refuse_regular_file(path: &Path) -> io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if !is_reparse(&meta) && meta.is_file() => Ok(()),
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "program log path is not a regular file",
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn is_reparse(meta: &std::fs::Metadata) -> bool {
    crate::fs_privacy::metadata_is_reparse(meta)
}

fn rotate_names(directory: &Path) -> io::Result<()> {
    refuse_family(directory)?;
    let oldest = log_path(directory, 4);
    match std::fs::symlink_metadata(&oldest) {
        Ok(meta) if !is_reparse(&meta) && meta.is_file() => std::fs::remove_file(&oldest)?,
        Ok(_) => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "program log archive is not a regular file",
            ));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    for index in (0..4u32).rev() {
        let source = log_path(directory, index);
        let destination = log_path(directory, index + 1);
        match std::fs::symlink_metadata(&source) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
            Ok(meta) if is_reparse(&meta) || !meta.is_file() => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "program log path is not a regular file",
                ));
            }
            Ok(meta) if meta.len() > MAX_PROGRAM_LOG_BYTES => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "program log family already exceeds 10 MiB",
                ));
            }
            Ok(_) => {}
        }
        refuse_regular_file(&destination)?;
        std::fs::rename(&source, &destination)?;
    }
    Ok(())
}

fn open_lock(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.create(true).read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    nofollow(&mut options);
    let file = options.open(path)?;
    reject_opened_file(path)?;
    crate::fs_privacy::set_private_permissions(path)?;
    Ok(file)
}

fn open_append(path: &Path) -> io::Result<File> {
    refuse_regular_file(path)?;
    let mut options = OpenOptions::new();
    options.create(true).append(true).read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    nofollow(&mut options);
    let file = options.open(path)?;
    reject_opened_file(path)?;
    crate::fs_privacy::set_private_permissions(path)?;
    Ok(file)
}

fn nofollow(options: &mut OpenOptions) {
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
}

fn reject_opened_file(path: &Path) -> io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if !is_reparse(&meta) && meta.is_file() => Ok(()),
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "program log path is not a regular file",
        )),
        Err(error) => Err(error),
    }
}

fn sanitize_event(raw: &[u8]) -> Vec<u8> {
    let text = String::from_utf8_lossy(raw);
    let trailing_newline = text.ends_with('\n');
    let stripped = strip_ansi(&text);
    let omitted = omit_body_payloads(&stripped);
    let redacted = crate::redaction::redact_text(&omitted);
    let urls = redact_url_userinfo(&redacted);
    let mut bytes = urls.into_bytes();
    if trailing_newline && !bytes.ends_with(b"\n") {
        bytes.push(b'\n');
    }
    cap_event(&mut bytes);
    bytes
}

fn strip_ansi(text: &str) -> String {
    if !text.as_bytes().contains(&0x1b) {
        return text.to_string();
    }
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == 0x1b {
            index = align_char_boundary(text, skip_ansi(bytes, index));
            continue;
        }
        let next = text[index..].chars().next().unwrap();
        out.push(next);
        index += next.len_utf8();
    }
    out
}

fn skip_ansi(bytes: &[u8], index: usize) -> usize {
    let next = index + 1;
    if next >= bytes.len() {
        return bytes.len();
    }
    match bytes[next] {
        b'[' => skip_csi(bytes, next + 1),
        b']' => skip_osc(bytes, next + 1),
        b'(' | b')' | b'*' | b'+' => (next + 2).min(bytes.len()),
        _ => next + 1,
    }
}

fn skip_csi(bytes: &[u8], mut index: usize) -> usize {
    while index < bytes.len() {
        let byte = bytes[index];
        index += 1;
        if (0x40..=0x7e).contains(&byte) {
            break;
        }
    }
    index
}

fn skip_osc(bytes: &[u8], mut index: usize) -> usize {
    while index < bytes.len() {
        if bytes[index] == 0x07 {
            return index + 1;
        }
        if bytes[index] == 0x1b && index + 1 < bytes.len() && bytes[index + 1] == b'\\' {
            return index + 2;
        }
        index += 1;
    }
    index
}

fn align_char_boundary(text: &str, mut index: usize) -> usize {
    if index > text.len() {
        return text.len();
    }
    while index < text.len() && !text.is_char_boundary(index) {
        index += 1;
    }
    index
}

fn omit_body_payloads(text: &str) -> String {
    if !text
        .bytes()
        .any(|byte| matches!(byte.to_ascii_lowercase(), b'b' | b'e' | b'p' | b'r' | b'u'))
    {
        return text.to_string();
    }
    let lower = text.to_ascii_lowercase();
    let mut out = String::with_capacity(text.len());
    let mut index = 0;
    while index < text.len() {
        if let Some(value_at) = body_value_start(&lower, index) {
            out.push_str(&text[index..value_at]);
            out.push_str("[omitted]");
            index = assigned_value_end(text.as_bytes(), value_at);
            continue;
        }
        let next = text[index..].chars().next().unwrap();
        out.push(next);
        index += next.len_utf8();
    }
    out
}

fn body_value_start(lower: &str, index: usize) -> Option<usize> {
    if index > 0 {
        let previous = lower[..index].chars().next_back()?;
        if is_body_label_char(previous) {
            return None;
        }
    }
    let bytes = lower.as_bytes();
    for label in BODY_LABELS {
        if !lower[index..].starts_with(label) {
            continue;
        }
        let mut cursor = index + label.len();
        if cursor < bytes.len() && is_body_label_byte(bytes[cursor]) {
            continue;
        }
        while cursor < bytes.len() && matches!(bytes[cursor], b' ' | b'\t') {
            cursor += 1;
        }
        if cursor < bytes.len() && matches!(bytes[cursor], b'"' | b'\'') {
            cursor += 1;
        }
        while cursor < bytes.len() && matches!(bytes[cursor], b' ' | b'\t') {
            cursor += 1;
        }
        if cursor < bytes.len() && matches!(bytes[cursor], b'=' | b':') {
            cursor += 1;
            while cursor < bytes.len() && matches!(bytes[cursor], b' ' | b'\t') {
                cursor += 1;
            }
            return Some(cursor);
        }
    }
    None
}

fn is_body_label_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.')
}

fn is_body_label_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.')
}

fn assigned_value_end(bytes: &[u8], start: usize) -> usize {
    if start >= bytes.len() {
        return start;
    }
    match bytes[start] {
        quote @ (b'"' | b'\'') => {
            let mut cursor = start + 1;
            while cursor < bytes.len() {
                if bytes[cursor] == b'\\' {
                    cursor = (cursor + 2).min(bytes.len());
                    continue;
                }
                if bytes[cursor] == quote {
                    return cursor + 1;
                }
                cursor += 1;
            }
            bytes.len()
        }
        _ => {
            // Display fields are unquoted and may contain spaces or line
            // breaks. Their end cannot be distinguished from arbitrary body
            // text, so omit the remaining event rather than leaking a suffix.
            bytes.len()
        }
    }
}

fn redact_url_userinfo(text: &str) -> String {
    if !text.contains("://") {
        return text.to_string();
    }
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut index = 0;
    while index < text.len() {
        if let Some(authority) = scheme_authority_start(bytes, index)
            && let Some((at, end)) = userinfo_span(bytes, authority)
        {
            out.push_str(&text[index..authority]);
            out.push_str("<redacted>");
            out.push_str(&text[at..end]);
            index = end;
            continue;
        }
        let next = text[index..].chars().next().unwrap();
        out.push(next);
        index += next.len_utf8();
    }
    out
}

fn scheme_authority_start(bytes: &[u8], index: usize) -> Option<usize> {
    if index > 0 && bytes[index - 1].is_ascii_alphanumeric() {
        return None;
    }
    let mut cursor = index;
    if cursor >= bytes.len() || !bytes[cursor].is_ascii_alphabetic() {
        return None;
    }
    cursor += 1;
    while cursor < bytes.len()
        && (bytes[cursor].is_ascii_alphanumeric() || matches!(bytes[cursor], b'+' | b'-' | b'.'))
    {
        cursor += 1;
    }
    if cursor + 2 < bytes.len()
        && bytes[cursor] == b':'
        && bytes[cursor + 1] == b'/'
        && bytes[cursor + 2] == b'/'
    {
        Some(cursor + 3)
    } else {
        None
    }
}

fn userinfo_span(bytes: &[u8], start: usize) -> Option<(usize, usize)> {
    let mut at = None;
    let mut cursor = start;
    while cursor < bytes.len() && !is_url_delimiter(bytes[cursor]) {
        if bytes[cursor] == b'@' {
            at = Some(cursor);
            break;
        }
        cursor += 1;
    }
    let at = at?;
    if at == start {
        return None;
    }
    let mut end = at + 1;
    while end < bytes.len() && !is_url_delimiter(bytes[end]) {
        end += 1;
    }
    Some((at, end))
}

fn is_url_delimiter(byte: u8) -> bool {
    matches!(
        byte,
        b'/' | b'?' | b'#' | b' ' | b'\t' | b'\n' | b'\r' | b'"' | b'\''
    )
}

fn max_event_bytes() -> usize {
    usize::try_from(MAX_PROGRAM_LOG_BYTES).unwrap_or(usize::MAX)
}

fn cap_event(bytes: &mut Vec<u8>) {
    if bytes.len() <= max_event_bytes() {
        return;
    }
    mark_truncated(bytes);
}

fn mark_truncated(bytes: &mut Vec<u8>) {
    let max = max_event_bytes();
    let mut keep = max.saturating_sub(TRUNCATION_MARKER.len()).min(bytes.len());
    while keep > 0 && !is_utf8_boundary(bytes, keep) {
        keep -= 1;
    }
    bytes.truncate(keep);
    bytes.extend_from_slice(TRUNCATION_MARKER);
}

fn is_utf8_boundary(bytes: &[u8], index: usize) -> bool {
    match bytes.get(index) {
        None => index == bytes.len(),
        Some(&byte) => (byte as i8) >= -0x40,
    }
}

#[cfg(test)]
mod tests;

//! Opt-in development captures, separate from bounded operational diagnostics.
//!
//! Files are written only when `OCG_DEBUG_REQUESTS=1`. Each encoded file is at
//! most 2 MiB, the directory keeps at most 1,000 owned files and 100 MiB, and
//! owned files older than 7 days are removed at startup and before a write.
use super::diagnostics::RequestTrace;
use crate::state::CoreState;
use axum::http::HeaderMap;
use bytes::Bytes;
use serde_json::{Value, json};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

const MAX_FILES: usize = 1000;
const MAX_ENCODED_BYTES: usize = 2 * 1024 * 1024;
const MAX_TOTAL_BYTES: u64 = 100 * 1024 * 1024;
const MAX_AGE: Duration = Duration::from_secs(7 * 24 * 60 * 60);

pub(crate) struct DebugCapture {
    directory: Option<PathBuf>,
    writer: Arc<parking_lot::Mutex<()>>,
}

impl DebugCapture {
    pub(crate) fn from_env(data_dir: &std::path::Path) -> Self {
        let mut directory = requests_enabled(std::env::var("OCG_DEBUG_REQUESTS").ok().as_deref())
            .then(|| {
                std::env::var_os("OCG_DEBUG_DIR")
                    .filter(|value| !value.is_empty())
                    .map(PathBuf::from)
                    .unwrap_or_else(|| data_dir.join("debug-requests"))
            });
        if let Some(path) = directory.clone() {
            match startup_maintain(&path) {
                Ok(()) => {}
                Err("unsafe_path") => {
                    tracing::warn!(reason = "unsafe_path", "debug capture disabled");
                    directory = None;
                }
                Err(code) => {
                    tracing::warn!(reason = code, "debug capture startup maintenance skipped");
                }
            }
        }
        Self {
            directory,
            writer: Arc::new(parking_lot::Mutex::new(())),
        }
    }

    pub(crate) fn enabled(&self) -> bool {
        self.directory.is_some()
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn save(
        &self,
        trace: &RequestTrace,
        stage: &str,
        attempt: u32,
        uri: &str,
        headers: &HeaderMap,
        body: Bytes,
        known_secrets: &[String],
    ) -> Result<(), String> {
        let Some(directory) = self.directory.clone() else {
            return Ok(());
        };
        if !matches!(stage, "client" | "upstream") {
            return Err("unsafe_stage".into());
        }
        // Generated names only. A caller-supplied request id is never a path component.
        let filename = owned_capture_name(stage, attempt).map_err(str::to_string)?;
        let id = trace.request_id.clone();
        let uri = uri.to_string();
        let stage = stage.to_string();
        let headers = headers.clone();
        let secrets = known_secrets.to_vec();
        let writer = self.writer.clone();
        tokio::task::spawn_blocking(move || -> Result<(), String> {
            let encoded =
                build_capture_bytes(&id, &stage, attempt, &uri, &headers, &body, &secrets)
                    .map_err(str::to_string)?;
            let _guard = writer.lock();
            write_capture(&directory, &filename, &encoded).map_err(str::to_string)
        })
        .await
        .map_err(|_| "writer_failed".to_string())?
    }
}

pub(crate) async fn capture_client(
    state: &CoreState,
    trace: &RequestTrace,
    headers: &HeaderMap,
    body: Bytes,
) {
    // Program shape metadata follows RUST_LOG only. Dashboard log_level must not
    // turn this on, and a huge body is not parsed or copied into the event.
    if tracing::enabled!(target: "ocg_core::gateway::diagnostics", tracing::Level::TRACE) {
        let fields = json!({
            "bytes": body.len(),
            "fingerprint": crate::redaction::sha256_hex(&body),
        });
        super::diagnostics::log_event(trace, "trace", "request", "request_shape", None, fields);
    }
    if let Err(code) = state
        .debug_capture
        .save(trace, "client", 0, &trace.path, headers, body, &[])
        .await
    {
        super::diagnostics::log_event(
            trace,
            "warn",
            "debug_capture",
            "capture_failed",
            None,
            json!({"stage": "client", "reason": code}),
        );
    }
}

fn requests_enabled(value: Option<&str>) -> bool {
    value.is_some_and(|value| value.trim() == "1")
}

fn sensitive(name: &str) -> bool {
    crate::redaction::is_sensitive_key(name)
        || name.eq_ignore_ascii_case("key")
        || name.eq_ignore_ascii_case("set-cookie")
}

fn redact_fields(value: &mut Value) {
    redact_fields_at(value, &mut Vec::new());
}

fn redact_fields_at<'a>(value: &'a mut Value, path: &mut Vec<&'a str>) {
    // Preserve definitions only at protocol-defined schema locations. Arbitrary metadata
    // named properties/definitions still receives ordinary credential redaction.
    if matches!(
        path.as_slice(),
        ["tools", "function", "parameters"]
            | ["tools", "input_schema"]
            | ["tools", "parameters"]
            | ["tools", "functionDeclarations", "parameters"]
            | ["response_format", "json_schema", "schema"]
            | ["text", "format", "schema"]
    ) {
        return;
    }
    match value {
        Value::Object(object) => {
            for (name, value) in object {
                if sensitive(name) {
                    *value = Value::String("<redacted>".into());
                } else {
                    path.push(name);
                    redact_fields_at(value, path);
                    path.pop();
                }
            }
        }
        Value::Array(values) => {
            for value in values {
                redact_fields_at(value, path);
            }
        }
        _ => {}
    }
}

pub(crate) fn authentication_secrets(headers: &HeaderMap) -> Vec<String> {
    headers
        .iter()
        .filter(|(name, _)| sensitive(name.as_str()))
        .filter_map(|(_, value)| value.to_str().ok())
        .map(|value| {
            let value = value.trim();
            value
                .strip_prefix("Bearer ")
                .unwrap_or(value)
                .trim()
                .to_string()
        })
        .filter(|value| !value.is_empty())
        .collect()
}

fn assemble_record(
    id: &str,
    step: (&str, u32),
    uri: &str,
    headers: &HeaderMap,
    body: &[u8],
    known_secrets: &[String],
    force_omit_body: bool,
) -> Value {
    let (stage, attempt) = step;
    let mut secrets = authentication_secrets(headers);
    secrets.extend(
        known_secrets
            .iter()
            .filter(|secret| !secret.is_empty())
            .cloned(),
    );
    let clean = |text: &str| {
        secrets
            .iter()
            .filter(|secret| !secret.is_empty())
            .fold(text.to_string(), |text, secret| {
                crate::redaction::redact_known_secret(&text, secret)
            })
    };
    let safe_uri = redact_uri(uri, &clean);
    let safe_headers = redact_headers(headers, &clean);
    let (body, capture_status) = if force_omit_body || body.len() > MAX_ENCODED_BYTES {
        (omitted_body(body), "body_omitted_too_large")
    } else {
        match serde_json::from_slice::<Value>(body) {
            Ok(mut parsed) => {
                redact_fields(&mut parsed);
                let safe = clean(&parsed.to_string());
                (
                    serde_json::from_str::<Value>(&safe).unwrap_or(Value::String(safe)),
                    "complete_redacted",
                )
            }
            // No safe schema for malformed or binary input. Record the gap, not the bytes.
            Err(_) => (omitted_body(body), "invalid_json_omitted"),
        }
    };
    json!({
        "version": 1,
        "timestamp": chrono::Utc::now(),
        "request_id": id,
        "stage": stage,
        "attempt": attempt,
        "method": "POST",
        "uri": safe_uri,
        "headers": safe_headers,
        "capture_status": capture_status,
        "body": body,
    })
}

fn omitted_body(body: &[u8]) -> Value {
    json!({
        "bytes": body.len(),
        "sha256": crate::redaction::sha256_hex(body),
    })
}

fn redact_uri(uri: &str, clean: &impl Fn(&str) -> String) -> String {
    let mut url = reqwest::Url::parse(uri)
        .or_else(|_| reqwest::Url::parse(&format!("http://localhost{uri}")))
        .ok();
    if let Some(url) = url.as_mut() {
        let pairs: Vec<_> = url
            .query_pairs()
            .map(|(key, value)| {
                let value = if sensitive(&key) {
                    "<redacted>".into()
                } else {
                    clean(&value)
                };
                (key.into_owned(), value)
            })
            .collect();
        if url.query().is_some() {
            url.query_pairs_mut().clear().extend_pairs(pairs);
        }
        let _ = url.set_username("");
        let _ = url.set_password(None);
        url.set_fragment(None);
        let redacted = if uri.starts_with('/') {
            format!(
                "{}{}",
                url.path(),
                url.query()
                    .map(|query| format!("?{query}"))
                    .unwrap_or_default()
            )
        } else {
            url.to_string()
        };
        clean(&redacted)
    } else {
        "<invalid-uri>".into()
    }
}

fn redact_headers(headers: &HeaderMap, clean: &impl Fn(&str) -> String) -> Vec<Value> {
    headers
        .iter()
        .map(|(name, value)| {
            json!({
                "name": name.as_str(),
                "value": if sensitive(name.as_str()) {
                    "<redacted>".to_string()
                } else {
                    clean(value.to_str().unwrap_or("<non-utf8>"))
                }
            })
        })
        .collect()
}

fn build_capture_bytes(
    id: &str,
    stage: &str,
    attempt: u32,
    uri: &str,
    headers: &HeaderMap,
    body: &[u8],
    known_secrets: &[String],
) -> Result<Vec<u8>, &'static str> {
    let omit_first = body.len() > MAX_ENCODED_BYTES;
    let record = assemble_record(
        id,
        (stage, attempt),
        uri,
        headers,
        body,
        known_secrets,
        omit_first,
    );
    if let Some(encoded) = encode_if_bounded(&record)? {
        return Ok(encoded);
    }
    if !omit_first {
        let omitted = assemble_record(
            id,
            (stage, attempt),
            uri,
            headers,
            body,
            known_secrets,
            true,
        );
        if let Some(encoded) = encode_if_bounded(&omitted)? {
            return Ok(encoded);
        }
    }
    let minimal = json!({
        "version": 1,
        "timestamp": chrono::Utc::now(),
        "request_id": bounded_request_id(id),
        "stage": stage,
        "attempt": attempt,
        "capture_status": "body_omitted_too_large",
        "uri_omitted": true,
        "headers_omitted": true,
        "body": omitted_body(body),
    });
    encode_if_bounded(&minimal)?.ok_or("capture_too_large")
}

fn bounded_request_id(id: &str) -> Value {
    if id.len() <= 128 && id.bytes().all(|byte| byte.is_ascii_graphic()) {
        Value::String(id.to_string())
    } else {
        json!({"bytes": id.len(), "omitted": "unsafe_request_id"})
    }
}

/// Pretty JSON only when the whole document fits. Oversized output is dropped
/// here so a later fallback cannot publish a cut-off document.
fn encode_if_bounded(record: &Value) -> Result<Option<Vec<u8>>, &'static str> {
    struct Bounded(Vec<u8>);
    impl Write for Bounded {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > MAX_ENCODED_BYTES.saturating_sub(self.0.len()) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::WriteZero,
                    "capture byte limit",
                ));
            }
            self.0.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut encoded = Bounded(Vec::new());
    match serde_json::to_writer_pretty(&mut encoded, record) {
        Ok(()) => Ok(Some(encoded.0)),
        Err(error) if error.io_error_kind() == Some(std::io::ErrorKind::WriteZero) => Ok(None),
        Err(_) => Err("encode_failed"),
    }
}

fn owned_capture_name(stage: &str, attempt: u32) -> Result<String, &'static str> {
    if !matches!(stage, "client" | "upstream") {
        return Err("unsafe_stage");
    }
    let stamp = u64::try_from(chrono::Utc::now().timestamp_micros()).map_err(|_| "unsafe_name")?;
    let name = format!(
        "ocg-{}-{stamp}-{stage}-{attempt}.json",
        uuid::Uuid::new_v4()
    );
    if owned_filename(&name) {
        Ok(name)
    } else {
        Err("unsafe_name")
    }
}

fn startup_maintain(directory: &Path) -> Result<(), &'static str> {
    refuse_reparse_ancestors(directory)?;
    match fs::symlink_metadata(directory) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(io_code(&error)),
        Ok(meta) if crate::fs_privacy::metadata_is_reparse(&meta) || !meta.is_dir() => {
            Err("unsafe_path")
        }
        Ok(_) => {
            ensure_directory(directory)?;
            let _lock = capture_lock(directory)?;
            prune_owned(directory, None)
        }
    }
}

fn write_capture(directory: &Path, filename: &str, bytes: &[u8]) -> Result<(), &'static str> {
    if bytes.len() > MAX_ENCODED_BYTES {
        return Err("capture_too_large");
    }
    let path = owned_capture_path(directory, filename)?;
    ensure_directory(directory)?;
    let _lock = capture_lock(directory)?;
    prune_owned(directory, Some(bytes.len() as u64))?;
    if fs::symlink_metadata(&path).is_ok() {
        return Err("already_exists");
    }
    let temp = path.with_extension("partial");
    let mut created = false;
    let mut published = false;
    let write_result = (|| {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        nofollow(&mut options);
        let mut file = options.open(&temp).map_err(|error| io_code(&error))?;
        created = true;
        if crate::fs_privacy::metadata_is_reparse(
            &file.metadata().map_err(|error| io_code(&error))?,
        ) {
            return Err("unsafe_path");
        }
        // Tighten the empty file before any private content is written,
        // including on Windows where Unix creation modes do not apply.
        crate::fs_privacy::set_private_permissions(&temp).map_err(|error| io_code(&error))?;
        file.write_all(bytes).map_err(|error| io_code(&error))?;
        file.sync_all().map_err(|error| io_code(&error))?;
        drop(file);
        reject_regular_file(&temp)?;
        if fs::symlink_metadata(&path).is_ok() {
            return Err("already_exists");
        }
        fs::rename(&temp, &path).map_err(|error| io_code(&error))?;
        published = true;
        reject_regular_file(&path)?;
        crate::fs_privacy::set_private_permissions(&path).map_err(|error| io_code(&error))?;
        Ok(())
    })();
    if write_result.is_err() && created && !published {
        remove_regular_file(&temp);
    }
    if write_result.is_err() && published {
        remove_regular_file(&path);
    }
    write_result
}

fn owned_capture_path(directory: &Path, filename: &str) -> Result<PathBuf, &'static str> {
    if !owned_filename(filename) || !filename.ends_with(".json") {
        return Err("unsafe_name");
    }
    let path = directory.join(filename);
    match path.file_name().and_then(|name| name.to_str()) {
        Some(name) if name == filename && path.parent() == Some(directory) => Ok(path),
        _ => Err("unsafe_name"),
    }
}

fn ensure_directory(directory: &Path) -> Result<(), &'static str> {
    refuse_reparse_ancestors(directory)?;
    match fs::symlink_metadata(directory) {
        Ok(meta) if crate::fs_privacy::metadata_is_reparse(&meta) || !meta.is_dir() => {
            Err("unsafe_path")
        }
        Ok(_) => {
            crate::fs_privacy::set_private_permissions(directory).map_err(|error| io_code(&error))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            create_private_directory(directory)
        }
        Err(error) => Err(io_code(&error)),
    }
}

fn create_private_directory(directory: &Path) -> Result<(), &'static str> {
    if let Some(parent) = directory
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        match fs::symlink_metadata(parent) {
            Ok(meta) if crate::fs_privacy::metadata_is_reparse(&meta) || !meta.is_dir() => {
                return Err("unsafe_path");
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                fs::create_dir_all(parent).map_err(|error| io_code(&error))?;
                refuse_reparse_ancestors(directory)?;
            }
            Err(error) => return Err(io_code(&error)),
        }
    }
    match fs::create_dir(directory) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            return match fs::symlink_metadata(directory) {
                Ok(meta) if !crate::fs_privacy::metadata_is_reparse(&meta) && meta.is_dir() => {
                    crate::fs_privacy::set_private_permissions(directory)
                        .map_err(|error| io_code(&error))
                }
                _ => Err("unsafe_path"),
            };
        }
        Err(error) => return Err(io_code(&error)),
    }
    refuse_reparse_ancestors(directory)?;
    match fs::symlink_metadata(directory) {
        Ok(meta) if !crate::fs_privacy::metadata_is_reparse(&meta) && meta.is_dir() => {}
        _ => return Err("unsafe_path"),
    }
    if let Err(error) = crate::fs_privacy::set_private_permissions(directory) {
        let _ = fs::remove_dir(directory);
        return Err(io_code(&error));
    }
    Ok(())
}

fn refuse_reparse_ancestors(directory: &Path) -> Result<(), &'static str> {
    let mut current = if directory.is_absolute() {
        directory.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|_| "unsafe_path")?
            .join(directory)
    };
    loop {
        match fs::symlink_metadata(&current) {
            Ok(meta) if crate::fs_privacy::metadata_is_reparse(&meta) => return Err("unsafe_path"),
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(io_code(&error)),
        }
        if !current.pop() {
            break;
        }
    }
    Ok(())
}

struct OwnedFile {
    modified: SystemTime,
    len: u64,
    path: PathBuf,
}

fn capture_lock(directory: &Path) -> Result<std::fs::File, &'static str> {
    let path = directory.join(".debug-capture.lock");
    if fs::symlink_metadata(&path).is_ok() {
        reject_regular_file(&path)?;
    }
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    nofollow(&mut options);
    let file = options.open(&path).map_err(|error| io_code(&error))?;
    let meta = file.metadata().map_err(|error| io_code(&error))?;
    if crate::fs_privacy::metadata_is_reparse(&meta) || !meta.is_file() {
        return Err("unsafe_path");
    }
    reject_regular_file(&path)?;
    crate::fs_privacy::set_private_permissions(&path).map_err(|error| io_code(&error))?;
    fs2::FileExt::try_lock_exclusive(&file).map_err(|_| "capture_busy")?;
    Ok(file)
}

fn prune_owned(directory: &Path, incoming: Option<u64>) -> Result<(), &'static str> {
    let incoming_bytes = incoming.unwrap_or(0);
    if incoming_bytes > MAX_ENCODED_BYTES as u64 || incoming_bytes > MAX_TOTAL_BYTES {
        return Err("capture_too_large");
    }
    let mut kept = Vec::new();
    let now = SystemTime::now();
    for file in list_owned(directory)? {
        let expired = now
            .duration_since(file.modified)
            .is_ok_and(|age| age >= MAX_AGE);
        if expired
            || file.len > MAX_ENCODED_BYTES as u64
            || file
                .path
                .extension()
                .is_some_and(|extension| extension == "partial")
        {
            remove_owned(&file.path)?;
        } else {
            crate::fs_privacy::set_private_permissions(&file.path)
                .map_err(|error| io_code(&error))?;
            kept.push(file);
        }
    }
    kept.sort_by(|left, right| {
        left.modified
            .cmp(&right.modified)
            .then_with(|| left.path.cmp(&right.path))
    });
    let slots = usize::from(incoming.is_some());
    while kept.len().saturating_add(slots) > MAX_FILES {
        let oldest = kept.remove(0);
        remove_owned(&oldest.path)?;
    }
    let mut total = kept
        .iter()
        .fold(0u64, |sum, file| sum.saturating_add(file.len));
    while total.saturating_add(incoming_bytes) > MAX_TOTAL_BYTES && !kept.is_empty() {
        let oldest = kept.remove(0);
        total = total.saturating_sub(oldest.len);
        remove_owned(&oldest.path)?;
    }
    if total.saturating_add(incoming_bytes) > MAX_TOTAL_BYTES {
        Err("capture_too_large")
    } else {
        Ok(())
    }
}

fn list_owned(directory: &Path) -> Result<Vec<OwnedFile>, &'static str> {
    let mut owned = Vec::new();
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(owned),
        Err(error) => return Err(io_code(&error)),
    };
    for entry in entries {
        let entry = entry.map_err(|error| io_code(&error))?;
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        if !owned_filename(&name) {
            continue;
        }
        let path = directory.join(&name);
        if path.file_name().and_then(|item| item.to_str()) != Some(name.as_str()) {
            continue;
        }
        let meta = match fs::symlink_metadata(&path) {
            Ok(meta) => meta,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(io_code(&error)),
        };
        if crate::fs_privacy::metadata_is_reparse(&meta) || !meta.is_file() {
            continue;
        }
        let Ok(modified) = meta.modified() else {
            continue;
        };
        owned.push(OwnedFile {
            modified,
            len: meta.len(),
            path,
        });
    }
    Ok(owned)
}

fn remove_owned(path: &Path) -> Result<(), &'static str> {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return Err("unsafe_name");
    };
    if !owned_filename(name) {
        return Err("unsafe_name");
    }
    reject_regular_file(path)?;
    fs::remove_file(path).map_err(|error| io_code(&error))
}

fn remove_regular_file(path: &Path) {
    let Ok(meta) = fs::symlink_metadata(path) else {
        return;
    };
    if crate::fs_privacy::metadata_is_reparse(&meta) || !meta.is_file() {
        return;
    }
    let _ = fs::remove_file(path);
}

fn reject_regular_file(path: &Path) -> Result<(), &'static str> {
    match fs::symlink_metadata(path) {
        Ok(meta) if !crate::fs_privacy::metadata_is_reparse(&meta) && meta.is_file() => Ok(()),
        Ok(_) => Err("unsafe_path"),
        Err(error) => Err(io_code(&error)),
    }
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

fn io_code(error: &std::io::Error) -> &'static str {
    match error.kind() {
        std::io::ErrorKind::PermissionDenied => "permission_denied",
        std::io::ErrorKind::AlreadyExists => "already_exists",
        std::io::ErrorKind::NotFound => "not_found",
        std::io::ErrorKind::InvalidInput => "unsafe_path",
        _ => "file_write_failed",
    }
}

fn owned_filename(name: &str) -> bool {
    if name.len() > 160
        || name.bytes().any(|byte| {
            matches!(byte, b'/' | b'\\' | b':' | 0)
                || byte.is_ascii_whitespace()
                || byte.is_ascii_control()
        })
    {
        return false;
    }
    // The earlier writer used an internal UUID without the `ocg-` prefix.
    // Retention includes those owned files and interrupted private writes.
    let rest = name.strip_prefix("ocg-").unwrap_or(name);
    let Some(id) = rest.get(..36) else {
        return false;
    };
    if uuid::Uuid::parse_str(id).is_err() {
        return false;
    }
    let Some(tail) = rest
        .get(36..)
        .and_then(|tail| tail.strip_prefix('-'))
        .and_then(|tail| {
            tail.strip_suffix(".json")
                .or_else(|| tail.strip_suffix(".partial"))
        })
    else {
        return false;
    };
    let mut parts = tail.split('-');
    let Some(stamp) = parts.next() else {
        return false;
    };
    let Some(stage) = parts.next() else {
        return false;
    };
    let Some(attempt) = parts.next() else {
        return false;
    };
    parts.next().is_none()
        && !stamp.is_empty()
        && stamp.bytes().all(|byte| byte.is_ascii_digit())
        && stamp.parse::<u64>().is_ok()
        && matches!(stage, "client" | "upstream")
        && !attempt.is_empty()
        && attempt.bytes().all(|byte| byte.is_ascii_digit())
        && attempt.parse::<u32>().is_ok()
}

#[cfg(test)]
mod tests;

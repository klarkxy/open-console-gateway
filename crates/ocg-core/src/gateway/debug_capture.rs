//! Opt-in development captures, separate from bounded operational diagnostics.
use super::diagnostics::RequestTrace;
use crate::state::CoreState;
use axum::http::HeaderMap;
use bytes::Bytes;
use serde_json::{Value, json};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;

const MAX_FILES: usize = 1000;

pub(crate) struct DebugCapture {
    directory: Option<PathBuf>,
    writer: Arc<parking_lot::Mutex<()>>,
}

impl DebugCapture {
    pub(crate) fn from_env(data_dir: &std::path::Path) -> Self {
        let enabled = std::env::var("OCG_DEBUG_REQUESTS").is_ok_and(|v| v.trim() == "1");
        Self {
            directory: enabled.then(|| {
                std::env::var_os("OCG_DEBUG_DIR")
                    .filter(|v| !v.is_empty())
                    .map(PathBuf::from)
                    .unwrap_or_else(|| data_dir.join("debug-requests"))
            }),
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
        // Generated IDs only, never use a caller's request-id or model as a filename.
        let filename = format!(
            "{}-{}-{stage}-{attempt}.json",
            trace.request_id,
            chrono::Utc::now().timestamp_micros()
        );
        let id = trace.request_id.clone();
        let uri = uri.to_string();
        let stage = stage.to_string();
        let headers = headers.clone();
        let secrets = known_secrets.to_vec();
        let writer = self.writer.clone();
        tokio::task::spawn_blocking(move || {
            let record = capture_record(&id, &stage, attempt, &uri, &headers, &body, &secrets);
            let encoded =
                serde_json::to_vec_pretty(&record).map_err(|_| "encode_failed".to_string())?;
            let _guard = writer.lock();
            write_capture(&directory, &filename, &encoded)
                .map_err(|error| format!("file_write_failed:{:?}", error.kind()))
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
    if state.db.lock().log_level <= crate::runtime_log::Level::Trace {
        let diagnostic = super::diagnostics::ErrorDiagnostic::new(
            trace,
            0,
            "client",
            "received",
            super::protocol::ApiFormat::ChatCompletions,
        )
        .with_request_summary(&body);
        super::diagnostics::log_event(
            &state.db.lock(),
            trace,
            "trace",
            "request",
            "request_shape",
            None,
            json!({"summary": diagnostic.request_summary, "fingerprint": diagnostic.request_fingerprint}),
        );
    }
    if let Err(code) = state
        .debug_capture
        .save(trace, "client", 0, &trace.path, headers, body, &[])
        .await
    {
        super::diagnostics::log_event(
            &state.db.lock(),
            trace,
            "warn",
            "debug_capture",
            "capture_failed",
            None,
            json!({"stage": "client", "reason": code}),
        );
    }
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
        .filter_map(|(_, v)| v.to_str().ok())
        .map(|v| {
            let v = v.trim();
            v.strip_prefix("Bearer ").unwrap_or(v).trim().to_string()
        })
        .filter(|v| !v.is_empty())
        .collect()
}

fn capture_record(
    id: &str,
    stage: &str,
    attempt: u32,
    uri: &str,
    headers: &HeaderMap,
    body: &[u8],
    known_secrets: &[String],
) -> Value {
    let mut secrets = authentication_secrets(headers);
    secrets.extend(known_secrets.iter().filter(|s| !s.is_empty()).cloned());
    let clean = |text: &str| {
        secrets
            .iter()
            .filter(|s| !s.is_empty())
            .fold(text.to_string(), |text, secret| {
                crate::redaction::redact_known_secret(&text, secret)
            })
    };
    let mut url = reqwest::Url::parse(uri)
        .or_else(|_| reqwest::Url::parse(&format!("http://localhost{uri}")))
        .ok();
    let safe_uri = if let Some(url) = url.as_mut() {
        let pairs: Vec<_> = url
            .query_pairs()
            .map(|(k, v)| {
                let v = if sensitive(&k) {
                    "<redacted>".into()
                } else {
                    clean(&v)
                };
                (k.into_owned(), v)
            })
            .collect();
        if url.query().is_some() {
            url.query_pairs_mut().clear().extend_pairs(pairs);
        }
        let _ = url.set_username("");
        let _ = url.set_password(None);
        url.set_fragment(None);
        if uri.starts_with('/') {
            format!(
                "{}{}",
                url.path(),
                url.query().map(|q| format!("?{q}")).unwrap_or_default()
            )
        } else {
            url.to_string()
        }
    } else {
        "<invalid-uri>".into()
    };
    let safe_headers: Vec<Value> = headers
        .iter()
        .map(|(name, value)| {
            json!({
                "name": name.as_str(), "value": if sensitive(name.as_str()) { "<redacted>".into() }
                    else { clean(value.to_str().unwrap_or("<non-utf8>")) }
            })
        })
        .collect();
    let (body, capture_status) = match serde_json::from_slice::<Value>(body) {
        Ok(mut body) => {
            redact_fields(&mut body);
            let safe = clean(&body.to_string());
            (
                serde_json::from_str::<Value>(&safe).unwrap_or(Value::String(safe)),
                "complete_redacted",
            )
        }
        // No safe schema for malformed/binary input. Explicitly record the gap, not a raw secret dump.
        Err(_) => (
            json!({"bytes": body.len(), "sha256": crate::redaction::sha256_hex(body)}),
            "invalid_json_omitted",
        ),
    };
    json!({"version": 1, "timestamp": chrono::Utc::now(), "request_id": id,
        "stage": stage, "attempt": attempt, "method": "POST", "uri": clean(&safe_uri),
        "headers": safe_headers, "capture_status": capture_status, "body": body})
}

fn write_capture(directory: &std::path::Path, filename: &str, bytes: &[u8]) -> std::io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(directory)?;
    let path = directory.join(filename);
    let temp = path.with_extension("partial");
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let result = (|| {
        let mut file = options.open(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temp, &path)?;
        // Delete only our own regular capture files. Never follow directories or symlinks.
        let mut captures: Vec<_> = std::fs::read_dir(directory)?
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_ok_and(|t| t.is_file()))
            .filter(|entry| {
                let name = entry.file_name().to_string_lossy().into_owned();
                owned_filename(&name)
            })
            .filter_map(|entry| Some((entry.metadata().ok()?.modified().ok()?, entry.path())))
            .collect();
        captures.sort_by_key(|entry| entry.0);
        let excess = captures.len().saturating_sub(MAX_FILES);
        for (_, path) in captures.into_iter().take(excess) {
            std::fs::remove_file(path)?;
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(temp);
    }
    result
}

fn owned_filename(name: &str) -> bool {
    let Some(rest) = name.strip_prefix("ocg-") else {
        return false;
    };
    let Some(id) = rest.get(..36) else {
        return false;
    };
    if uuid::Uuid::parse_str(id).is_err() {
        return false;
    }
    let Some(tail) = rest
        .get(36..)
        .and_then(|tail| tail.strip_prefix('-'))
        .and_then(|tail| tail.strip_suffix(".json"))
    else {
        return false;
    };
    let parts: Vec<_> = tail.split('-').collect();
    parts.len() == 3
        && parts[0].parse::<u64>().is_ok()
        && matches!(parts[1], "client" | "upstream")
        && parts[2].parse::<u32>().is_ok()
}

#[cfg(test)]
mod tests;

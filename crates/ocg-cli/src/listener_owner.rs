//! Projection of the bound address for the serve process in this data directory.
//!
//! The pid in this file is not an exclusive owner and is not a lock. `serve`
//! holds `serve_lock` for that. Key writes read the endpoint once. A dead pid
//! does not authorize a second writer. Cleanup removes the file only when its
//! pid is this process.

use super::process_alive::process_is_running;
use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};

const FILE_NAME: &str = "cli-listener.json";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Owner {
    Absent,
    Live { endpoint: String },
    Unreadable { message: String },
}

struct Record {
    pid: u32,
    endpoint: String,
}

pub fn path_for(data_dir: &Path) -> PathBuf {
    data_dir.join(FILE_NAME)
}

pub fn write(data_dir: &Path, endpoint: &str) -> Result<()> {
    std::fs::create_dir_all(data_dir)
        .with_context(|| format!("failed to create {}", data_dir.display()))?;
    let record = Record {
        pid: std::process::id(),
        endpoint: endpoint.to_string(),
    };
    let bytes = serde_json::to_vec(&serde_json::json!({
        "pid": record.pid,
        "endpoint": record.endpoint,
    }))?;
    let path = path_for(data_dir);
    let temporary = data_dir.join(format!("{FILE_NAME}.tmp"));
    std::fs::write(&temporary, &bytes)
        .with_context(|| format!("failed to write {}", temporary.display()))?;
    std::fs::rename(&temporary, &path)
        .with_context(|| format!("failed to publish {}", path.display()))?;
    Ok(())
}

pub fn update_endpoint_if_ours(data_dir: &Path, endpoint: &str) -> Result<bool> {
    let Some(record) = read_record(data_dir)? else {
        return Ok(false);
    };
    if record.pid != std::process::id() || record.endpoint == endpoint {
        return Ok(false);
    }
    write(data_dir, endpoint)?;
    Ok(true)
}

pub fn remove(data_dir: &Path) {
    let _ = std::fs::remove_file(path_for(data_dir));
    let _ = std::fs::remove_file(data_dir.join(format!("{FILE_NAME}.tmp")));
}

pub fn remove_if_ours(data_dir: &Path) {
    let Ok(Some(record)) = read_record(data_dir) else {
        return;
    };
    if record.pid == std::process::id() {
        remove(data_dir);
    }
}

/// Refuse to start over a marker this process does not own.
///
/// A dead pid is absent and may be replaced after this process binds. A live
/// foreign pid, or a marker that cannot be parsed, is left untouched.
pub fn refuse_foreign_writer(data_dir: &Path) -> Result<()> {
    match inspect(data_dir) {
        Owner::Absent => Ok(()),
        Owner::Live { endpoint } => match read_record(data_dir) {
            Ok(Some(record)) if record.pid == std::process::id() => Ok(()),
            _ => bail!(
                "cli-listener.json records a live process at {endpoint}. The pid marker is not an exclusive owner and this process will not replace it or open the database. Stop that process, or use api --endpoint."
            ),
        },
        Owner::Unreadable { message } => bail!(
            "{message} Refusing to start serve over an unreadable marker. The marker was not replaced."
        ),
    }
}

pub fn inspect(data_dir: &Path) -> Owner {
    let path = path_for(data_dir);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Owner::Absent,
        Err(error) => {
            return Owner::Unreadable {
                message: format!("listener marker {} is unreadable: {error}", path.display()),
            };
        }
    };
    let record = match parse_record(&bytes) {
        Ok(record) => record,
        Err(error) => {
            return Owner::Unreadable {
                message: format!("listener marker {} is invalid: {error}", path.display()),
            };
        }
    };
    if record.endpoint.is_empty() || record.pid == 0 {
        return Owner::Unreadable {
            message: format!("listener marker {} is incomplete", path.display()),
        };
    }
    if process_is_running(record.pid) {
        Owner::Live {
            endpoint: record.endpoint,
        }
    } else {
        Owner::Absent
    }
}

fn read_record(data_dir: &Path) -> Result<Option<Record>> {
    let bytes = match std::fs::read(path_for(data_dir)) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).context(format!(
                "listener marker {} is unreadable",
                path_for(data_dir).display()
            ));
        }
    };
    Ok(Some(parse_record(&bytes)?))
}

fn parse_record(bytes: &[u8]) -> Result<Record> {
    let value: serde_json::Value = serde_json::from_slice(bytes)?;
    let pid = value
        .get("pid")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| anyhow::anyhow!("pid is missing"))?;
    let endpoint = value
        .get("endpoint")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("endpoint is missing"))?;
    Ok(Record {
        pid: u32::try_from(pid)?,
        endpoint: endpoint.to_string(),
    })
}

#[cfg(test)]
#[path = "listener_owner/tests.rs"]
mod tests;

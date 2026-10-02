//! Session cookies bound to one exact endpoint origin.
//!
//! The file is the only place a cookie is stored. Responses and errors never
//! include it. A deletion `Set-Cookie` removes the matching name.

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::path::Path;

const VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
struct StoredCookie {
    name: String,
    value: String,
    path: String,
    secure: bool,
}

pub struct SessionStore {
    path: std::path::PathBuf,
    endpoint: String,
    cookies: Vec<StoredCookie>,
}

impl SessionStore {
    pub fn load(path: &Path, endpoint: &str) -> Result<Self> {
        reject_symlink(path)?;
        if !path.exists() {
            return Ok(Self {
                path: path.to_path_buf(),
                endpoint: endpoint.to_string(),
                cookies: Vec::new(),
            });
        }
        let bytes = std::fs::read(path)
            .with_context(|| format!("failed to read session file {}", path.display()))?;
        let (stored_endpoint, cookies) = parse_file(path, &bytes)?;
        if stored_endpoint != endpoint {
            bail!("session file is bound to {stored_endpoint} and cannot be sent to {endpoint}");
        }
        Ok(Self {
            path: path.to_path_buf(),
            endpoint: endpoint.to_string(),
            cookies,
        })
    }

    pub fn header_for(&self, request_path: &str, https: bool) -> Option<String> {
        let path = request_path.split('?').next().unwrap_or(request_path);
        let pairs = self
            .cookies
            .iter()
            .filter(|cookie| path_matches(&cookie.path, path) && (!cookie.secure || https))
            .map(|cookie| format!("{}={}", cookie.name, cookie.value))
            .collect::<Vec<_>>();
        if pairs.is_empty() {
            None
        } else {
            Some(pairs.join("; "))
        }
    }

    pub fn secret_values(&self) -> Vec<String> {
        self.cookies
            .iter()
            .map(|cookie| cookie.value.clone())
            .filter(|value| !value.is_empty())
            .collect()
    }

    pub fn apply_set_cookie(&mut self, headers: &reqwest::header::HeaderMap) -> Result<()> {
        let mut changed = false;
        for value in headers.get_all(reqwest::header::SET_COOKIE) {
            let Ok(text) = value.to_str() else {
                bail!("host sent a non-UTF-8 Set-Cookie header");
            };
            let Some(parsed) = parse_set_cookie(text) else {
                continue;
            };
            changed = true;
            self.cookies.retain(|cookie| cookie.name != parsed.name);
            if !parsed.delete {
                self.cookies.push(StoredCookie {
                    name: parsed.name,
                    value: parsed.value,
                    path: parsed.path,
                    secure: parsed.secure,
                });
            }
        }
        if changed {
            self.save()?;
        }
        Ok(())
    }

    fn save(&self) -> Result<()> {
        reject_symlink(&self.path)?;
        if let Some(parent) = self.path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)?;
        }
        let cookies = self
            .cookies
            .iter()
            .map(|cookie| {
                json!({
                    "name": cookie.name,
                    "value": cookie.value,
                    "path": cookie.path,
                    "secure": cookie.secure,
                })
            })
            .collect::<Vec<_>>();
        let bytes = serde_json::to_vec(&json!({
            "version": VERSION,
            "endpoint": self.endpoint,
            "cookies": cookies,
        }))?;
        write_private(&self.path, &bytes)
    }
}

fn parse_file(path: &Path, bytes: &[u8]) -> Result<(String, Vec<StoredCookie>)> {
    let value: Value = serde_json::from_slice(bytes)
        .with_context(|| format!("session file {} is invalid", path.display()))?;
    let version = value
        .get("version")
        .and_then(Value::as_u64)
        .ok_or_else(|| anyhow::anyhow!("session file {} is invalid", path.display()))?;
    if version != u64::from(VERSION) {
        bail!("session file {} has an unsupported version", path.display());
    }
    let endpoint = value
        .get("endpoint")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("session file {} is invalid", path.display()))?
        .to_string();
    let entries = value
        .get("cookies")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("session file {} is invalid", path.display()))?;
    let mut cookies = Vec::new();
    for entry in entries {
        let name = entry
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("session file {} is invalid", path.display()))?;
        let value = entry
            .get("value")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("session file {} is invalid", path.display()))?;
        let path_attr = entry
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("session file {} is invalid", path.display()))?;
        let secure = entry
            .get("secure")
            .and_then(Value::as_bool)
            .ok_or_else(|| anyhow::anyhow!("session file {} is invalid", path.display()))?;
        cookies.push(StoredCookie {
            name: name.to_string(),
            value: value.to_string(),
            path: path_attr.to_string(),
            secure,
        });
    }
    Ok((endpoint, cookies))
}

struct ParsedCookie {
    name: String,
    value: String,
    path: String,
    secure: bool,
    delete: bool,
}

fn parse_set_cookie(header: &str) -> Option<ParsedCookie> {
    let (pair, attributes) = header
        .split_once(';')
        .map(|(pair, rest)| (pair, Some(rest)))
        .unwrap_or((header, None));
    let (name, value) = pair.trim().split_once('=')?;
    let name = name.trim();
    let value = value.trim();
    if name.is_empty()
        || name
            .bytes()
            .any(|byte| !byte.is_ascii_graphic() || byte == b';')
        || value
            .chars()
            .any(|character| character.is_control() || character == ';')
    {
        return None;
    }
    let mut path = "/".to_string();
    let mut secure = false;
    let mut delete = value.is_empty();
    if let Some(attributes) = attributes {
        for attribute in attributes.split(';') {
            let attribute = attribute.trim();
            if attribute.eq_ignore_ascii_case("secure") {
                secure = true;
                continue;
            }
            let Some((key, raw)) = attribute.split_once('=') else {
                continue;
            };
            if key.trim().eq_ignore_ascii_case("path") {
                let candidate = raw.trim();
                if candidate.starts_with('/') && !candidate.contains("..") {
                    path = candidate.to_string();
                }
            } else if key.trim().eq_ignore_ascii_case("max-age") && raw.trim() == "0" {
                delete = true;
            }
        }
    }
    Some(ParsedCookie {
        name: name.to_string(),
        value: value.to_string(),
        path,
        secure,
        delete,
    })
}

fn path_matches(cookie_path: &str, request_path: &str) -> bool {
    request_path == cookie_path
        || request_path.starts_with(cookie_path)
            && (cookie_path.ends_with('/') || request_path[cookie_path.len()..].starts_with('/'))
}

fn reject_symlink(path: &Path) -> Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            bail!("refusing to use symlink {}", path.display());
        }
        Ok(_) | Err(_) => Ok(()),
    }
}

pub fn open_private(path: &Path) -> Result<std::fs::File> {
    reject_symlink(path)?;
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options
        .open(path)
        .with_context(|| format!("failed to create {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    // Restrict the DACL before returning the handle so callers cannot write
    // secret bytes into an inherited ACL.
    #[cfg(windows)]
    restrict_private_permissions(path)?;
    Ok(file)
}

/// Current-user and SYSTEM access only. Call again after a replacement:
/// Windows can keep the destination DACL.
#[cfg(windows)]
pub(crate) fn restrict_private_permissions(path: &Path) -> Result<()> {
    ocg_core::private_file::set_private_permissions(path)
        .with_context(|| format!("failed to restrict permissions on {}", path.display()))
}

pub fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    let temporary = path.with_extension("tmp");
    {
        let mut file = open_private(&temporary)?;
        use std::io::Write;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(windows)]
    if path.exists() {
        restrict_private_permissions(path)?;
    }
    std::fs::rename(&temporary, path)
        .with_context(|| format!("failed to replace {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(windows)]
    restrict_private_permissions(path)?;
    Ok(())
}

#[cfg(test)]
#[path = "session_file/tests.rs"]
mod tests;

#[cfg(all(test, windows))]
pub(crate) use tests::{assert_private_dacl, dacl_report};

//! Explicit, one-time local CLI credential import. Discovery reads metadata only;
//! no arbitrary paths, token refresh, source writes, or background synchronization.

use crate::cpa::CpaOAuthProvider;
use base64::Engine;
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use zeroize::Zeroizing;

const MAX_FILE_BYTES: u64 = 128 * 1024;
const MAX_TOKEN_BYTES: usize = 32 * 1024;
const GROK_CLIENT_ID: &str = "b1a00492-073a-47ea-816f-4c329264a828";
const GROK_AUTH_ENTRY: &str = "https://auth.x.ai::b1a00492-073a-47ea-816f-4c329264a828";
type ImportResult<T> = Result<T, &'static str>;

pub(crate) struct CliRoots {
    #[cfg(test)]
    home: PathBuf,
    codex: PathBuf,
    claude: PathBuf,
    kimi: PathBuf,
    grok: PathBuf,
}

pub(crate) struct ImportSource {
    pub provider: CpaOAuthProvider,
    pub source: &'static str,
    pub supported: bool,
    pub available: bool,
    pub reason: Option<&'static str>,
}

// Intentionally no Debug or Serialize implementation: only the typed CPA client
// can transmit the allowlisted payload, never a Dashboard response.
pub(crate) struct ImportedCredential {
    pub name: String,
    pub cpa_provider: &'static str,
    pub payload: Value,
}

impl CliRoots {
    pub(crate) fn from_env() -> ImportResult<Self> {
        #[cfg(windows)]
        let home = std::env::var_os("USERPROFILE");
        #[cfg(not(windows))]
        let home = std::env::var_os("HOME");
        let home = PathBuf::from(home.ok_or("Local user profile is unavailable.")?);
        Ok(Self {
            codex: std::env::var_os("CODEX_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".codex")),
            claude: std::env::var_os("CLAUDE_CONFIG_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".claude")),
            kimi: std::env::var_os("KIMI_CODE_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".kimi-code")),
            grok: std::env::var_os("GROK_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".grok")),
            #[cfg(test)]
            home,
        })
    }

    /// Fresh directory for tests. Production import keeps `from_env`.
    #[cfg(test)]
    pub(crate) fn synthetic(root: PathBuf) -> Self {
        Self {
            home: root.clone(),
            codex: root.join(".codex"),
            claude: root.join(".claude"),
            kimi: root.join(".kimi-code"),
            grok: root.join(".grok"),
        }
    }

    fn source(&self, provider: CpaOAuthProvider) -> (&'static str, ImportResult<PathBuf>) {
        match provider {
            CpaOAuthProvider::Codex => ("Codex CLI", Ok(self.codex.join("auth.json"))),
            CpaOAuthProvider::Anthropic => {
                ("Claude Code", Ok(self.claude.join(".credentials.json")))
            }
            CpaOAuthProvider::Kimi => (
                "Kimi Code",
                Ok(self.kimi.join("credentials").join("kimi-code.json")),
            ),
            CpaOAuthProvider::Xai => ("Grok CLI", Ok(self.grok.join("auth.json"))),
            CpaOAuthProvider::Antigravity => (
                "Antigravity",
                Err(
                    "Antigravity local credential storage is not compatible with this import; use login.",
                ),
            ),
        }
    }

    pub(crate) fn discover(&self) -> Vec<ImportSource> {
        CpaOAuthProvider::ALL
            .into_iter()
            .map(|provider| {
                let (source, path) = self.source(provider);
                match path {
                    Err(reason) => ImportSource {
                        provider,
                        source,
                        supported: false,
                        available: false,
                        reason: Some(reason),
                    },
                    Ok(path) => {
                        let checked = checked_metadata(&path);
                        ImportSource {
                            provider,
                            source,
                            supported: true,
                            available: checked.is_ok(),
                            reason: checked.err(),
                        }
                    }
                }
            })
            .collect()
    }

    pub(crate) fn read(&self, provider: CpaOAuthProvider) -> ImportResult<ImportedCredential> {
        let (_, path) = self.source(provider);
        let path = path?;
        checked_metadata(&path)?;
        let file = open_source(&path)?;
        let metadata = file
            .metadata()
            .map_err(|_| "Cannot inspect the local CLI credential file.")?;
        if !metadata.is_file() || metadata.len() > MAX_FILE_BYTES {
            return Err("CLI credential file is not a bounded regular file.");
        }
        let mut bytes = Zeroizing::new(Vec::new());
        file.take(MAX_FILE_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| "Cannot read the local CLI credential file.")?;
        if bytes.len() as u64 > MAX_FILE_BYTES {
            return Err("CLI credential file exceeds 128 KiB.");
        }
        let source: Value = serde_json::from_slice(&bytes)
            .map_err(|_| "Local CLI credential file is not valid JSON.")?;
        normalize(provider, &source)
    }
}

fn checked_metadata(path: &Path) -> ImportResult<fs::Metadata> {
    if !path.is_absolute() {
        return Err("CLI credential directory must be an absolute local path.");
    }
    if path
        .components()
        .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err("CLI credential directory must not contain parent traversal.");
    }
    #[cfg(windows)]
    if matches!(path.components().next(), Some(std::path::Component::Prefix(prefix)) if matches!(prefix.kind(), std::path::Prefix::UNC(..) | std::path::Prefix::VerbatimUNC(..) | std::path::Prefix::DeviceNS(..)))
    {
        return Err("Network credential paths are not supported.");
    }
    for ancestor in path.ancestors() {
        let metadata = fs::symlink_metadata(ancestor).map_err(
            |_| "No readable CLI credential file was found. Keychain-only logins need a new login.",
        )?;
        #[cfg(windows)]
        let linked = {
            use std::os::windows::fs::MetadataExt;
            metadata.file_attributes() & 0x400 != 0
        };
        #[cfg(not(windows))]
        let linked = metadata.file_type().is_symlink();
        if linked {
            return Err("Linked CLI credential paths are not supported.");
        }
    }
    let metadata =
        fs::symlink_metadata(path).map_err(|_| "Cannot inspect the local CLI credential file.")?;
    if !metadata.is_file() {
        return Err("CLI credential path is not a regular file.");
    }
    if metadata.len() > MAX_FILE_BYTES {
        return Err("CLI credential file exceeds 128 KiB.");
    }
    Ok(metadata)
}

#[cfg(windows)]
fn open_source(path: &Path) -> ImportResult<fs::File> {
    use std::os::windows::{
        fs::{MetadataExt, OpenOptionsExt},
        io::AsRawHandle,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_OPEN_REPARSE_POINT, GetFinalPathNameByHandleW,
    };
    let expected = windows_source_name(path)?;
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
        .map_err(|_| "Cannot read the local CLI credential file.")?;
    let metadata = file
        .metadata()
        .map_err(|_| "Cannot inspect the opened CLI credential file.")?;
    if metadata.file_attributes() & 0x400 != 0 || !metadata.is_file() {
        return Err("Linked CLI credential paths are not supported.");
    }
    // OPEN_REPARSE_POINT protects the leaf. Verify the opened handle's name to
    // reject an ancestor junction replaced after metadata discovery as well.
    let mut name = vec![0u16; 32768];
    let length = unsafe {
        GetFinalPathNameByHandleW(
            file.as_raw_handle(),
            name.as_mut_ptr(),
            name.len() as u32,
            0,
        )
    } as usize;
    if length == 0 || length >= name.len() {
        return Err("Cannot verify the opened CLI credential path.");
    }
    let resolved = String::from_utf16(&name[..length])
        .map_err(|_| "CLI credential path is not valid Unicode.")?;
    if windows_source_name(Path::new(&resolved))? != expected {
        return Err("CLI credential path changed during import; detect it again.");
    }
    Ok(file)
}

#[cfg(windows)]
fn windows_source_name(path: &Path) -> ImportResult<String> {
    use std::path::{Component, Prefix};
    if !path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, Component::ParentDir))
        || !matches!(path.components().next(), Some(Component::Prefix(prefix)) if matches!(prefix.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_)))
    {
        return Err("CLI credential directory must be an absolute local disk path.");
    }
    let normalized: PathBuf = path.components().collect();
    let text = normalized
        .to_str()
        .ok_or("CLI credential path is not valid Unicode.")?;
    Ok(text
        .strip_prefix(r"\\?\")
        .unwrap_or(text)
        .replace('/', "\\")
        .to_lowercase())
}

#[cfg(unix)]
fn open_source(path: &Path) -> ImportResult<fs::File> {
    use std::ffi::CString;
    use std::os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::ffi::OsStrExt,
    };
    use std::path::Component;
    if !path.is_absolute() {
        return Err("CLI credential directory must be absolute.");
    }
    let parts = path
        .components()
        .filter(|part| !matches!(part, Component::RootDir))
        .collect::<Vec<_>>();
    if parts.is_empty()
        || parts
            .iter()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err("CLI credential path must not contain parent traversal.");
    }
    let root = unsafe {
        nix::libc::open(
            c"/".as_ptr(),
            nix::libc::O_RDONLY | nix::libc::O_DIRECTORY | nix::libc::O_CLOEXEC,
        )
    };
    if root < 0 {
        return Err("Cannot open the local credential root.");
    }
    let mut directory = unsafe { OwnedFd::from_raw_fd(root) };
    for (index, component) in parts.iter().enumerate() {
        let name = CString::new(component.as_os_str().as_bytes())
            .map_err(|_| "CLI credential path is invalid.")?;
        let last = index + 1 == parts.len();
        let flags = nix::libc::O_RDONLY
            | nix::libc::O_CLOEXEC
            | nix::libc::O_NOFOLLOW
            | if last {
                nix::libc::O_NONBLOCK
            } else {
                nix::libc::O_DIRECTORY
            };
        let fd = unsafe { nix::libc::openat(directory.as_raw_fd(), name.as_ptr(), flags) };
        if fd < 0 {
            return Err("Cannot open a non-linked local CLI credential file.");
        }
        if last {
            return Ok(unsafe { fs::File::from_raw_fd(fd) });
        }
        directory = unsafe { OwnedFd::from_raw_fd(fd) };
    }
    Err("CLI credential file is missing.")
}

#[cfg(not(any(windows, unix)))]
fn open_source(_: &Path) -> ImportResult<fs::File> {
    Err("Local CLI credential import is not supported on this platform.")
}

fn token<'a>(value: &'a Value, key: &str) -> ImportResult<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| {
            !value.is_empty()
                && value.len() <= MAX_TOKEN_BYTES
                && !value
                    .chars()
                    .any(|ch| ch.is_control() || ch.is_whitespace())
        })
        .ok_or("CLI login is missing a usable OAuth token. Log in again with the CLI or CPA.")
}

fn jwt_claims(token: &str) -> ImportResult<Value> {
    let parts = token.split('.').collect::<Vec<_>>();
    if parts.len() != 3 {
        return Err("CLI OAuth identity token is malformed.");
    }
    let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(parts[1])
        .map_err(|_| "CLI OAuth identity token is malformed.")?;
    serde_json::from_slice(&decoded).map_err(|_| "CLI OAuth identity token is malformed.")
}

fn timestamp(seconds: i64) -> ImportResult<String> {
    DateTime::<Utc>::from_timestamp(seconds, 0)
        .map(|time| time.to_rfc3339())
        .ok_or("CLI OAuth expiry is invalid.")
}

fn normalize(provider: CpaOAuthProvider, source: &Value) -> ImportResult<ImportedCredential> {
    let (cpa_provider, payload) = match provider {
        CpaOAuthProvider::Codex => {
            if source
                .get("auth_mode")
                .and_then(Value::as_str)
                .is_some_and(|mode| mode != "chatgpt")
            {
                return Err(
                    "Codex is not using a ChatGPT OAuth login; API keys and other login modes cannot be imported.",
                );
            }
            let tokens = source
                .get("tokens")
                .ok_or("No Codex ChatGPT OAuth login was found.")?;
            let id_token = token(tokens, "id_token")?;
            let access = token(tokens, "access_token")?;
            let refresh = token(tokens, "refresh_token")?;
            let identity = jwt_claims(id_token)?;
            let access_claims = jwt_claims(access)?;
            let account_id = tokens
                .get("account_id")
                .and_then(Value::as_str)
                .or_else(|| {
                    identity
                        .pointer("/https:~1~1api.openai.com~1auth/chatgpt_account_id")
                        .and_then(Value::as_str)
                })
                .filter(|id| !id.is_empty() && id.len() <= 512)
                .ok_or("Codex OAuth account identity is missing.")?;
            let expiry = timestamp(
                access_claims
                    .get("exp")
                    .and_then(Value::as_i64)
                    .ok_or("Codex OAuth expiry is missing.")?,
            )?;
            (
                "codex",
                json!({"type":"codex", "id_token":id_token, "access_token":access,
                "refresh_token":refresh, "account_id":account_id, "expired":expiry,
                "email":identity.get("email").and_then(Value::as_str).unwrap_or("")}),
            )
        }
        CpaOAuthProvider::Anthropic => {
            let oauth = source
                .get("claudeAiOauth")
                .ok_or("No Claude Code OAuth login was found.")?;
            if !oauth
                .get("scopes")
                .and_then(Value::as_array)
                .is_some_and(|scopes| scopes.iter().any(|s| s.as_str() == Some("user:inference")))
            {
                return Err(
                    "Claude Code login does not include the inference OAuth scope; use a new login.",
                );
            }
            let expiry_ms = oauth
                .get("expiresAt")
                .and_then(Value::as_i64)
                .ok_or("Claude Code OAuth expiry is missing.")?;
            (
                "claude",
                json!({"type":"claude", "access_token":token(oauth,"accessToken")?,
                "refresh_token":token(oauth,"refreshToken")?, "expired":timestamp(expiry_ms / 1000)?}),
            )
        }
        CpaOAuthProvider::Kimi => {
            // Official Kimi Code's file wire format; CPA uses the same OAuth
            // host/client ID. CPA owns its own refresh device identity.
            let seconds = source
                .get("expires_at")
                .and_then(Value::as_f64)
                .filter(|n| n.is_finite() && *n > 0.0 && *n < 253402300800.0)
                .ok_or("Kimi Code OAuth expiry is missing or invalid.")?;
            let scope = source
                .get("scope")
                .and_then(Value::as_str)
                .filter(|s| s.len() <= 2048 && !s.chars().any(char::is_control))
                .ok_or("Kimi Code OAuth scope is missing or invalid.")?;
            let token_type = token(source, "token_type")?;
            if !token_type.eq_ignore_ascii_case("bearer") {
                return Err("Kimi Code login is not a supported Bearer OAuth login.");
            }
            (
                "kimi",
                json!({"type":"kimi", "access_token":token(source,"access_token")?,
                "refresh_token":token(source,"refresh_token")?, "token_type":token_type,
                "scope":scope,"expired":timestamp(seconds.floor() as i64)?}),
            )
        }
        CpaOAuthProvider::Xai => {
            // Grok CLI 1.0.13 caches entries by issuer::client_id. Do not accept
            // API keys, other issuers, or grants CPA cannot refresh.
            let oauth = source
                .get(GROK_AUTH_ENTRY)
                .ok_or("No CPA-compatible Grok OAuth login was found.")?;
            if oauth.get("auth_mode").and_then(Value::as_str) != Some("oidc")
                || oauth.get("oidc_issuer").and_then(Value::as_str) != Some("https://auth.x.ai")
                || oauth.get("oidc_client_id").and_then(Value::as_str) != Some(GROK_CLIENT_ID)
            {
                return Err(
                    "Grok login uses an incompatible issuer or OAuth client; use CPA login.",
                );
            }
            let expiry = oauth
                .get("expires_at")
                .and_then(Value::as_str)
                .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                .ok_or("Grok OAuth expiry is missing or invalid.")?
                .to_rfc3339();
            let mut payload = json!({"type":"xai", "auth_kind":"oauth", "token_type":"Bearer",
                "access_token":token(oauth,"key")?, "refresh_token":token(oauth,"refresh_token")?, "expired":expiry});
            for (target, key) in [("email", "email"), ("sub", "principal_id")] {
                if let Some(value) = oauth
                    .get(key)
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty() && s.len() <= 512 && !s.chars().any(char::is_control))
                {
                    payload[target] = json!(value);
                }
            }
            ("xai", payload)
        }
        _ => return Err("This CLI credential format is not supported; use login."),
    };
    // Stable names make retries reconcilable without keeping an import database.
    // An account ID is preferable; opaque providers fall back to the same grant.
    let identity = payload
        .get("account_id")
        .and_then(Value::as_str)
        .or_else(|| payload.get("sub").and_then(Value::as_str))
        .or_else(|| payload.get("refresh_token").and_then(Value::as_str))
        .ok_or("OAuth identity is missing.")?;
    let digest = format!(
        "{:x}",
        Sha256::digest(format!("{cpa_provider}:{identity}").as_bytes())
    );
    Ok(ImportedCredential {
        name: format!("ocg-cli-{cpa_provider}-{}.json", &digest[..24]),
        cpa_provider,
        payload,
    })
}

#[cfg(test)]
mod tests;

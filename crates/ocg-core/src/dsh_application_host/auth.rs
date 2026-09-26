//! Local DSH browser-session grant reader.
//!
//! Reads only `records["client-connection/browser-session"]` from a selected
//! same-user `DSH_HOME/.credentials.yaml`. The secret is kept in memory and
//! never written back. This is native local compatibility, not a public
//! external-auth API.

use super::is_link_or_reparse;
use super::runtime::{
    DshRuntimeCookie, DshRuntimeError, DshRuntimeOrigin, mint_browser_session_cookie,
};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::de::{self, IgnoredAny, MapAccess, Visitor};
use serde::{Deserialize, Deserializer};
use sha2::{Digest, Sha256};
use std::fmt;
use std::fs::File;
use std::io::{ErrorKind, Read};
use std::path::Path;
use zeroize::Zeroize;

const GRANT_KEY: &str = "client-connection/browser-session";
const SECRET_BYTES: usize = 32;
const MAX_CREDENTIALS_BYTES: u64 = 64 * 1024;
const DOCUMENT_VERSION: u64 = 1;
const BASE64URL_PATTERN: &[u8] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BrowserGrantError {
    Missing,
    Unsupported,
}

impl BrowserGrantError {
    pub fn message(self) -> &'static str {
        match self {
            Self::Missing => {
                "DSH local session is unavailable; start the displayed DSH address on this machine"
            }
            Self::Unsupported => "DSH local session format is unsupported",
        }
    }
}

pub(crate) struct BrowserSessionSecret([u8; SECRET_BYTES]);

impl Drop for BrowserSessionSecret {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl BrowserSessionSecret {
    pub fn digest(&self) -> [u8; 32] {
        Sha256::digest(self.0).into()
    }

    pub fn mint_cookie(
        &self,
        origin: &DshRuntimeOrigin,
    ) -> Result<DshRuntimeCookie, DshRuntimeError> {
        mint_browser_session_cookie(origin, &self.0, None)
    }
}

impl fmt::Debug for BrowserSessionSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("BrowserSessionSecret([redacted])")
    }
}

pub(crate) fn read_browser_session_grant(
    home: &Path,
) -> Result<BrowserSessionSecret, BrowserGrantError> {
    let path = home.join(".credentials.yaml");
    match std::fs::symlink_metadata(&path) {
        Err(error) if error.kind() == ErrorKind::NotFound => {
            return Err(BrowserGrantError::Missing);
        }
        Err(_) => return Err(BrowserGrantError::Unsupported),
        Ok(metadata) => {
            if is_link_or_reparse(&path) || !metadata.file_type().is_file() {
                return Err(BrowserGrantError::Unsupported);
            }
        }
    }
    let file = File::open(&path).map_err(|_| BrowserGrantError::Unsupported)?;
    let mut bytes = Vec::new();
    file.take(MAX_CREDENTIALS_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| BrowserGrantError::Unsupported)?;
    if bytes.len() as u64 > MAX_CREDENTIALS_BYTES {
        return Err(BrowserGrantError::Unsupported);
    }
    if bytes.is_empty() {
        return Err(BrowserGrantError::Missing);
    }
    if bytes
        .windows(GRANT_KEY.len())
        .filter(|window| *window == GRANT_KEY.as_bytes())
        .count()
        > 1
    {
        return Err(BrowserGrantError::Unsupported);
    }
    let document: CredentialsDocument =
        serde_yaml_ng::from_slice(&bytes).map_err(|_| BrowserGrantError::Unsupported)?;
    if document.version != Some(DOCUMENT_VERSION) {
        if document.version.is_none() && document.records.browser.is_none() {
            return Err(BrowserGrantError::Missing);
        }
        return Err(BrowserGrantError::Unsupported);
    }
    let Some(record) = document.records.browser else {
        return Err(BrowserGrantError::Missing);
    };
    parse_grant(record)
}

#[derive(Deserialize)]
struct CredentialsDocument {
    #[serde(default)]
    version: Option<u64>,
    #[serde(default)]
    records: StrictRecords,
}

#[derive(Default)]
struct StrictRecords {
    browser: Option<GrantRecord>,
}

impl<'de> Deserialize<'de> for StrictRecords {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct RecordsVisitor;
        impl<'de> Visitor<'de> for RecordsVisitor {
            type Value = StrictRecords;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("a credentials records mapping")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<StrictRecords, A::Error> {
                let mut browser = None;
                while let Some(key) = map.next_key::<String>()? {
                    if key == GRANT_KEY {
                        if browser.is_some() {
                            return Err(de::Error::custom("duplicate browser-session grant"));
                        }
                        browser = Some(map.next_value()?);
                    } else {
                        let _: IgnoredAny = map.next_value()?;
                    }
                }
                Ok(StrictRecords { browser })
            }
        }
        deserializer.deserialize_map(RecordsVisitor)
    }
}

#[derive(Deserialize)]
struct GrantRecord {
    kind: String,
    payload: GrantPayload,
}

#[derive(Deserialize)]
struct GrantPayload {
    version: u64,
    secret: String,
}

fn parse_grant(record: GrantRecord) -> Result<BrowserSessionSecret, BrowserGrantError> {
    if record.kind != "grant" || record.payload.version != 1 {
        return Err(BrowserGrantError::Unsupported);
    }
    let secret = decode_secret(&record.payload.secret).ok_or(BrowserGrantError::Unsupported)?;
    Ok(BrowserSessionSecret(secret))
}

fn decode_secret(value: &str) -> Option<[u8; SECRET_BYTES]> {
    if value.is_empty()
        || value.len() % 4 == 1
        || !value.bytes().all(|byte| BASE64URL_PATTERN.contains(&byte))
    {
        return None;
    }
    let decoded = URL_SAFE_NO_PAD.decode(value).ok()?;
    if decoded.len() != SECRET_BYTES {
        return None;
    }
    let encoded = URL_SAFE_NO_PAD.encode(&decoded);
    if encoded != value {
        return None;
    }
    let mut secret = [0u8; SECRET_BYTES];
    secret.copy_from_slice(&decoded);
    Some(secret)
}

#[cfg(test)]
#[path = "auth/tests.rs"]
mod tests;

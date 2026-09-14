//! OCG-owned CPA runtime: desktop/CLI install/lifecycle, bounded logs,
//! and managed client inference keys.
//!
//! External user-operated CPA remains a connect-only integration. This module
//! never stops, replaces, or deletes a process OCG did not start.

mod device;
mod extract;
pub mod host;

use crate::cpa::{CpaClient, CpaError};
use crate::db::{CpaCatalogModel, CpaCatalogRecord, CpaIntegrationRecord};
use crate::http_client;
use crate::models::{Account as ModelAccount, AccountSetupStep, AccountType, AppConfig};
use crate::provider::{
    CPA_ACCOUNT_ID, CPA_ACCOUNT_NAME, CPA_PROVIDER_ID, CredentialKind, QuotaScope,
};
use crate::state::CoreStateInner;
use chrono::Utc;
use futures_util::StreamExt;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

pub const CPA_RUNTIME_DIR: &str = "cpa";
pub const CPA_GITHUB_LATEST_API: &str =
    "https://api.github.com/repos/router-for-me/CLIProxyAPI/releases/latest";
pub const CPA_GITHUB_RELEASES_URL: &str = "https://github.com/router-for-me/CLIProxyAPI/releases";
pub const WINDOWS_AMD64_ASSET_MARKER: &str = "_windows_amd64.zip";
pub const DARWIN_AMD64_ASSET_MARKER: &str = "_darwin_amd64.tar.gz";
pub const DARWIN_AARCH64_ASSET_MARKER: &str = "_darwin_aarch64.tar.gz";
pub const LINUX_AMD64_ASSET_MARKER: &str = "_linux_amd64.tar.gz";
pub const UNAVAILABLE_REASON: &str = "CPA runtime management needs an official CLIProxyAPI build for this OS/CPU (supported: Windows x64, macOS, Linux x64)";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CpaReleaseAsset {
    WindowsAmd64Zip,
    DarwinAmd64TarGz,
    DarwinAarch64TarGz,
    LinuxAmd64TarGz,
}

impl CpaReleaseAsset {
    pub fn file_name(self, version: &str) -> String {
        let marker = match self {
            Self::WindowsAmd64Zip => WINDOWS_AMD64_ASSET_MARKER,
            Self::DarwinAmd64TarGz => DARWIN_AMD64_ASSET_MARKER,
            Self::DarwinAarch64TarGz => DARWIN_AARCH64_ASSET_MARKER,
            Self::LinuxAmd64TarGz => LINUX_AMD64_ASSET_MARKER,
        };
        format!("CLIProxyAPI_{version}{marker}")
    }

    pub fn archive_kind(self) -> extract::CpaArchiveKind {
        match self {
            Self::WindowsAmd64Zip => extract::CpaArchiveKind::Zip,
            Self::DarwinAmd64TarGz | Self::DarwinAarch64TarGz | Self::LinuxAmd64TarGz => {
                extract::CpaArchiveKind::TarGz
            }
        }
    }
}

pub fn current_cpa_release_asset() -> Option<CpaReleaseAsset> {
    #[cfg(all(windows, target_arch = "x86_64"))]
    {
        Some(CpaReleaseAsset::WindowsAmd64Zip)
    }
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    {
        Some(CpaReleaseAsset::DarwinAmd64TarGz)
    }
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        Some(CpaReleaseAsset::DarwinAarch64TarGz)
    }
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        Some(CpaReleaseAsset::LinuxAmd64TarGz)
    }
    #[cfg(not(any(
        all(windows, target_arch = "x86_64"),
        all(target_os = "macos", target_arch = "x86_64"),
        all(target_os = "macos", target_arch = "aarch64"),
        all(target_os = "linux", target_arch = "x86_64"),
    )))]
    {
        None
    }
}
const CHECKSUMS_NAME: &str = "checksums.txt";
const MANAGED_NAME: &str = "managed.json";
const CONFIG_NAME: &str = "config.yaml";
const PREVIOUS_CONFIG_NAME: &str = "config.yaml.previous";
const ASSET_SHA_NAME: &str = ".asset-sha256";
const DEFAULT_PORT: u16 = 8317;
const MAX_CHECKSUM_BYTES: usize = 64 * 1024;
const MAX_ARCHIVE_BYTES: usize = 64 * 1024 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(5 * 60);
#[cfg(not(test))]
const PROBE_ATTEMPTS: usize = 30;
#[cfg(test)]
const PROBE_ATTEMPTS: usize = 2;
#[cfg(not(test))]
const PROBE_DELAY: Duration = Duration::from_secs(1);
#[cfg(test)]
const PROBE_DELAY: Duration = Duration::from_millis(1);
pub const MAX_LOG_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CpaRuntimeError {
    Unavailable(String),
    Invalid(String),
    Conflict(String),
    Unreachable(String),
    Failed(String),
}

impl std::fmt::Display for CpaRuntimeError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable(message)
            | Self::Invalid(message)
            | Self::Conflict(message)
            | Self::Unreachable(message)
            | Self::Failed(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for CpaRuntimeError {}

impl From<CpaError> for CpaRuntimeError {
    fn from(error: CpaError) -> Self {
        match error {
            CpaError::Invalid(message) => Self::Invalid(message),
            CpaError::Unreachable(message) => Self::Unreachable(message),
            CpaError::Http { status, message } => {
                Self::Failed(format!("CPA returned HTTP {status}: {message}"))
            }
            CpaError::Response(message) | CpaError::Incompatible(message) => Self::Failed(message),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CpaRuntimePhase {
    Idle,
    Checking,
    Downloading,
    Installing,
    Starting,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagedCpa {
    pub current_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_version: Option<String>,
    pub asset_sha256: String,
    pub port: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CpaRuntimeSnapshot {
    pub supported: bool,
    pub unavailable_reason: Option<String>,
    pub installed: bool,
    pub running: bool,
    pub owned: bool,
    pub current_version: Option<String>,
    pub previous_version: Option<String>,
    pub asset_sha256: Option<String>,
    pub port: Option<u16>,
    pub base_url: Option<String>,
    pub phase: CpaRuntimePhase,
    pub error: Option<String>,
    pub latest_version: Option<String>,
    pub update_available: bool,
    pub current_operation: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CpaRuntimeCheck {
    pub current_version: Option<String>,
    pub latest_version: String,
    pub update_available: bool,
    pub release_url: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CpaRuntimeLogTail {
    pub stdout: String,
    pub stderr: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CpaRuntimeKeyView {
    pub fingerprint: String,
    pub hint: String,
    pub protected: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CpaRuntimeKeyCreated {
    pub fingerprint: String,
    pub hint: String,
    pub secret: String,
}

#[derive(Clone)]
pub struct CpaRuntimeSecret(String);

impl CpaRuntimeSecret {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn expose_to_host(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for CpaRuntimeSecret {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("[redacted]")
    }
}

pub struct CpaRuntimeProcessSpec {
    pub codex_device_login: bool,
    pub executable: PathBuf,
    pub config_path: PathBuf,
    pub working_dir: PathBuf,
    pub management_password: CpaRuntimeSecret,
    pub log_secrets: Vec<CpaRuntimeSecret>,
}

pub trait CpaRuntimeProcessHost: Send + Sync {
    fn start_owned(&self, spec: &CpaRuntimeProcessSpec) -> Result<(), CpaRuntimeError>;
    fn stop_owned(&self) -> Result<(), CpaRuntimeError>;
    fn owned_running(&self) -> bool;
    fn logs(&self) -> CpaRuntimeLogTail;
    fn add_log_secret(&self, secret: &CpaRuntimeSecret);
}

pub type CpaRuntimeHost = Arc<dyn CpaRuntimeProcessHost>;

pub struct CpaRuntimeCapabilities {
    host: OnceLock<CpaRuntimeHost>,
    status: Mutex<RuntimeStatus>,
    device: Mutex<Option<Arc<device::DeviceSession>>>,
}

struct RuntimeStatus {
    phase: CpaRuntimePhase,
    error: Option<String>,
    latest_version: Option<String>,
    current_operation: Option<String>,
    failure_logs: Option<CpaRuntimeLogTail>,
}

impl CpaRuntimeCapabilities {
    pub fn new() -> Self {
        Self {
            host: OnceLock::new(),
            device: Mutex::new(None),
            status: Mutex::new(RuntimeStatus {
                phase: CpaRuntimePhase::Idle,
                error: None,
                latest_version: None,
                current_operation: None,
                failure_logs: None,
            }),
        }
    }

    pub fn set_host(&self, host: CpaRuntimeHost) {
        assert!(
            self.host.set(host).is_ok(),
            "CPA runtime Host is already configured"
        );
    }

    pub fn supported(&self) -> bool {
        self.host.get().is_some()
    }

    fn host(&self) -> Result<&CpaRuntimeHost, CpaRuntimeError> {
        self.host
            .get()
            .ok_or_else(|| CpaRuntimeError::Unavailable(UNAVAILABLE_REASON.into()))
    }

    fn set_phase(&self, phase: CpaRuntimePhase, error: Option<String>) {
        let mut status = self.status.lock();
        status.phase = phase;
        status.error = error;
    }

    fn set_operation(&self, operation: Option<&str>) {
        self.status.lock().current_operation = operation.map(ToOwned::to_owned);
    }

    fn begin_operation(&self, operation: &str) -> RuntimeOperationGuard<'_> {
        self.set_operation(Some(operation));
        RuntimeOperationGuard(self)
    }

    fn begin_lifecycle_operation(&self, operation: &str) -> RuntimeOperationGuard<'_> {
        self.cancel_device_login();
        let mut status = self.status.lock();
        status.current_operation = Some(operation.to_string());
        status.failure_logs = None;
        RuntimeOperationGuard(self)
    }

    fn cache_failure_logs(&self, logs: CpaRuntimeLogTail) {
        self.status.lock().failure_logs = Some(logs);
    }

    fn failure_logs(&self) -> Option<CpaRuntimeLogTail> {
        self.status.lock().failure_logs.clone()
    }

    fn set_latest_version(&self, latest_version: String) {
        self.status.lock().latest_version = Some(latest_version);
    }

    fn snapshot_machine(
        &self,
    ) -> (
        CpaRuntimePhase,
        Option<String>,
        Option<String>,
        Option<String>,
    ) {
        let status = self.status.lock();
        (
            status.phase,
            status.error.clone(),
            status.latest_version.clone(),
            status.current_operation.clone(),
        )
    }
}

struct RuntimeOperationGuard<'a>(&'a CpaRuntimeCapabilities);

impl Drop for RuntimeOperationGuard<'_> {
    fn drop(&mut self) {
        self.0.set_operation(None);
    }
}

impl Default for CpaRuntimeCapabilities {
    fn default() -> Self {
        Self::new()
    }
}

pub fn runtime_dir(data_dir: &Path) -> PathBuf {
    data_dir.join(CPA_RUNTIME_DIR)
}

pub fn managed_path(data_dir: &Path) -> PathBuf {
    runtime_dir(data_dir).join(MANAGED_NAME)
}

pub fn windows_amd64_asset_name(version: &str) -> String {
    CpaReleaseAsset::WindowsAmd64Zip.file_name(version)
}

pub fn normalize_release_version(tag: &str) -> Result<String, CpaRuntimeError> {
    let version = tag.trim().trim_start_matches('v').trim();
    if version.is_empty()
        || version.len() > 64
        || !version.as_bytes().first().is_some_and(u8::is_ascii_digit)
        || !version
            .as_bytes()
            .last()
            .is_some_and(u8::is_ascii_alphanumeric)
        || version.split('.').any(str::is_empty)
        || !version
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-'))
    {
        return Err(CpaRuntimeError::Invalid(
            "CPA release version is invalid".into(),
        ));
    }
    Ok(version.to_string())
}

pub fn load_managed(data_dir: &Path) -> Result<Option<ManagedCpa>, CpaRuntimeError> {
    let path = managed_path(data_dir);
    reject_reparse_ancestors(parent_path(&path)?)?;
    match fs::symlink_metadata(&path) {
        Ok(_) if is_reparse_path(&path) => {
            return Err(CpaRuntimeError::Invalid(
                "CPA managed.json must not be a reparse point".into(),
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(fs_error(error)),
    }
    let text = fs::read_to_string(&path).map_err(|error| {
        CpaRuntimeError::Failed(format!("failed to read CPA managed.json: {error}"))
    })?;
    let managed: ManagedCpa = serde_json::from_str(&text).map_err(|error| {
        CpaRuntimeError::Failed(format!("CPA managed.json is invalid: {error}"))
    })?;
    let current = normalize_release_version(&managed.current_version).map_err(|_| {
        CpaRuntimeError::Failed("CPA managed.json has an invalid current version".into())
    })?;
    if current != managed.current_version {
        return Err(CpaRuntimeError::Failed(
            "CPA managed.json current version is not canonical".into(),
        ));
    }
    if let Some(previous) = managed.previous_version.as_deref() {
        let normalized = normalize_release_version(previous).map_err(|_| {
            CpaRuntimeError::Failed("CPA managed.json has an invalid previous version".into())
        })?;
        if normalized != previous || previous.eq_ignore_ascii_case(&managed.current_version) {
            return Err(CpaRuntimeError::Failed(
                "CPA managed.json previous version is invalid".into(),
            ));
        }
    }
    if managed.port == 0 {
        return Err(CpaRuntimeError::Failed(
            "CPA managed.json is missing a loopback port".into(),
        ));
    }
    if managed.asset_sha256.len() != 64
        || !managed
            .asset_sha256
            .chars()
            .all(|ch| ch.is_ascii_hexdigit())
    {
        return Err(CpaRuntimeError::Failed(
            "CPA managed.json has an invalid asset SHA-256".into(),
        ));
    }
    Ok(Some(managed))
}

fn require_managed(data_dir: &Path) -> Result<ManagedCpa, CpaRuntimeError> {
    load_managed(data_dir)?
        .ok_or_else(|| CpaRuntimeError::Invalid("CPA runtime is not installed by OCG".into()))
}

fn require_fresh_install(data_dir: &Path) -> Result<(), CpaRuntimeError> {
    if load_managed(data_dir)?.is_some() {
        return Err(CpaRuntimeError::Invalid(
            "CPA runtime is already installed; use update".into(),
        ));
    }
    Ok(())
}

fn version_dir(data_dir: &Path, version: &str) -> Result<PathBuf, CpaRuntimeError> {
    let normalized = normalize_release_version(version)?;
    if normalized != version {
        return Err(CpaRuntimeError::Invalid(
            "CPA runtime version is not canonical".into(),
        ));
    }
    let versions = runtime_dir(data_dir).join("versions");
    reject_reparse_ancestors(&versions)?;
    if versions.is_dir() {
        for entry in fs::read_dir(&versions).map_err(fs_error)? {
            let name = entry.map_err(fs_error)?.file_name();
            let name = name.to_string_lossy();
            if name.eq_ignore_ascii_case(&normalized) && name != normalized {
                return Err(CpaRuntimeError::Invalid(
                    "CPA version directory has ambiguous Windows casing".into(),
                ));
            }
        }
    }
    Ok(versions.join(normalized))
}

pub fn save_managed(data_dir: &Path, managed: &ManagedCpa) -> Result<(), CpaRuntimeError> {
    let encoded = serde_json::to_vec_pretty(managed).map_err(|error| {
        CpaRuntimeError::Failed(format!("failed to encode CPA managed.json: {error}"))
    })?;
    atomic_write(&managed_path(data_dir), &encoded)
}

pub fn fingerprint_key(secret: &str) -> String {
    format!("{:x}", Sha256::digest(secret.as_bytes()))
}

pub fn key_hint(secret: &str) -> String {
    let tail = secret
        .chars()
        .rev()
        .take(4)
        .collect::<String>()
        .chars()
        .rev()
        .collect::<String>();
    format!("••••{tail}")
}

pub fn parse_checksum(text: &str, filename: &str) -> Result<String, CpaRuntimeError> {
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut parts = line.split_whitespace();
        let (hash, name) = match (parts.next(), parts.next()) {
            (Some(hash), Some(name)) => (hash, name.trim_start_matches('*')),
            _ => continue,
        };
        if name == filename && hash.len() == 64 && hash.chars().all(|ch| ch.is_ascii_hexdigit()) {
            return Ok(hash.to_ascii_lowercase());
        }
    }
    Err(CpaRuntimeError::Invalid(format!(
        "checksums.txt does not contain {filename}"
    )))
}

pub fn append_log_tail(buffer: &mut String, chunk: &str, max_bytes: usize) {
    buffer.push_str(chunk);
    if buffer.len() <= max_bytes {
        return;
    }
    let extra = buffer.len() - max_bytes;
    let trim_at = buffer
        .char_indices()
        .find(|(index, _)| *index >= extra)
        .map(|(index, _)| index)
        .unwrap_or(extra);
    buffer.drain(..trim_at);
    if let Some(newline) = buffer.find('\n') {
        buffer.drain(..=newline);
    }
}

impl CoreStateInner {
    pub fn set_cpa_runtime_host(&self, host: CpaRuntimeHost) {
        self.cpa_runtime.set_host(host);
    }

    pub fn cpa_runtime_supported(&self) -> bool {
        self.cpa_runtime.supported()
    }

    pub fn cpa_runtime_snapshot(&self) -> CpaRuntimeSnapshot {
        let (phase, status_error, latest_version, current_operation) =
            self.cpa_runtime.snapshot_machine();
        let managed_result = load_managed(&self.data_dir);
        let managed = managed_result.as_ref().ok().and_then(|item| item.clone());
        let error = managed_result
            .err()
            .map(|error| error.to_string())
            .or(status_error);
        let running = self
            .cpa_runtime
            .host
            .get()
            .is_some_and(|host| host.owned_running());
        let port = managed.as_ref().map(|item| item.port);
        let unavailable_reason = if !self.cpa_runtime.supported() {
            Some(UNAVAILABLE_REASON.to_string())
        } else if managed.is_some() && std::env::var_os(crate::cpa::CPA_BASE_URL_ENV).is_some() {
            Some(
                "OCG_CPA_BASE_URL selects an external CPA; unset it to manage the installed runtime"
                    .into(),
            )
        } else {
            None
        };
        CpaRuntimeSnapshot {
            supported: self.cpa_runtime.supported(),
            unavailable_reason,
            installed: managed.is_some(),
            running,
            owned: managed.is_some(),
            current_version: managed.as_ref().map(|item| item.current_version.clone()),
            previous_version: managed
                .as_ref()
                .and_then(|item| item.previous_version.clone()),
            asset_sha256: managed.as_ref().map(|item| item.asset_sha256.clone()),
            port,
            base_url: port.map(|port| format!("http://127.0.0.1:{port}")),
            phase,
            error,
            update_available: managed
                .as_ref()
                .zip(latest_version.as_ref())
                .is_some_and(|(managed, latest)| managed.current_version != *latest),
            latest_version,
            current_operation,
        }
    }

    pub fn cpa_runtime_logs(&self) -> Result<CpaRuntimeLogTail, CpaRuntimeError> {
        if let Some(logs) = self.cpa_runtime.failure_logs() {
            return Ok(logs);
        }
        let host = self.cpa_runtime.host()?;
        if load_managed(&self.data_dir)?.is_none()
            && self.cpa_runtime.snapshot_machine().0 != CpaRuntimePhase::Failed
        {
            return Err(CpaRuntimeError::Invalid(
                "CPA managed runtime is not installed".into(),
            ));
        }
        Ok(host.logs())
    }

    pub fn stop_owned_cpa_runtime(&self) {
        self.cpa_runtime.cancel_device_login();
        if let Some(host) = self.cpa_runtime.host.get() {
            let _ = host.stop_owned();
        }
    }

    pub async fn check_cpa_runtime_update(
        &self,
        expected_revision: u64,
        expected_generation: u64,
    ) -> Result<CpaRuntimeCheck, CpaRuntimeError> {
        self.require_supported()?;
        self.ensure_cas(expected_revision, expected_generation)?;
        let _runtime_operation = self.cpa_runtime.begin_operation("check-update");
        self.cpa_runtime.set_phase(CpaRuntimePhase::Checking, None);
        let result = self.check_cpa_runtime_update_inner().await;
        let result = result.and_then(|check| {
            self.ensure_cas(expected_revision, expected_generation)?;
            self.cpa_runtime
                .set_latest_version(check.latest_version.clone());
            Ok(check)
        });
        match &result {
            Ok(_) => self.cpa_runtime.set_phase(CpaRuntimePhase::Idle, None),
            Err(error) => self
                .cpa_runtime
                .set_phase(CpaRuntimePhase::Failed, Some(error.to_string())),
        }
        result
    }

    async fn check_cpa_runtime_update_inner(&self) -> Result<CpaRuntimeCheck, CpaRuntimeError> {
        let release = self.fetch_latest_release().await?;
        let current = load_managed(&self.data_dir)?
            .map(|item| item.current_version)
            .filter(|item| !item.is_empty());
        let latest = release.version.clone();
        Ok(CpaRuntimeCheck {
            update_available: current.as_deref() != Some(latest.as_str()),
            current_version: current,
            latest_version: latest,
            release_url: CPA_GITHUB_RELEASES_URL.to_string(),
        })
    }

    pub async fn install_cpa_runtime(
        &self,
        expected_revision: u64,
        expected_generation: u64,
        expected_version: Option<&str>,
    ) -> Result<CpaRuntimeSnapshot, CpaRuntimeError> {
        self.install_or_update_cpa_runtime(
            InstallMode::Fresh,
            expected_revision,
            expected_generation,
            expected_version,
        )
        .await
    }

    async fn install_or_update_cpa_runtime(
        &self,
        mode: InstallMode,
        expected_revision: u64,
        expected_generation: u64,
        expected_version: Option<&str>,
    ) -> Result<CpaRuntimeSnapshot, CpaRuntimeError> {
        self.require_supported()?;
        if std::env::var_os(crate::cpa::CPA_BASE_URL_ENV).is_some() {
            return Err(CpaRuntimeError::Conflict(
                "OCG_CPA_BASE_URL selects an external CPA; unset it before managing a runtime"
                    .into(),
            ));
        }
        self.ensure_cas(expected_revision, expected_generation)?;
        let previous = match mode {
            InstallMode::Fresh => {
                require_fresh_install(&self.data_dir)?;
                if self.cpa_runtime.host()?.owned_running() {
                    return Err(CpaRuntimeError::Conflict(
                        "an OCG-owned CPA process is running without a valid owner manifest".into(),
                    ));
                }
                None
            }
            InstallMode::Update => Some(require_managed(&self.data_dir)?),
        };
        let operation = if mode == InstallMode::Fresh {
            "install"
        } else {
            "update"
        };
        let _runtime_operation = self.cpa_runtime.begin_lifecycle_operation(operation);
        self.cpa_runtime
            .set_phase(CpaRuntimePhase::Downloading, None);
        let config_path = runtime_dir(&self.data_dir).join(CONFIG_NAME);
        let outcome = match self.managed_secrets(mode, &config_path) {
            Ok(secrets) => {
                self.install_or_update_cpa_runtime_inner(
                    mode,
                    previous,
                    expected_revision,
                    expected_generation,
                    expected_version,
                    secrets,
                )
                .await
            }
            Err(error) => Err(error),
        };
        match &outcome {
            Ok(_) => self.cpa_runtime.set_phase(CpaRuntimePhase::Idle, None),
            Err(error) => self
                .cpa_runtime
                .set_phase(CpaRuntimePhase::Failed, Some(error.to_string())),
        }
        outcome
    }

    async fn install_or_update_cpa_runtime_inner(
        &self,
        mode: InstallMode,
        previous: Option<ManagedCpa>,
        expected_revision: u64,
        expected_generation: u64,
        expected_version: Option<&str>,
        secrets: ManagedSecrets,
    ) -> Result<CpaRuntimeSnapshot, CpaRuntimeError> {
        let release = self.fetch_latest_release().await?;
        self.cpa_runtime.set_latest_version(release.version.clone());
        if let Some(expected) = expected_version {
            let expected = normalize_release_version(expected)?;
            if expected != release.version {
                return Err(CpaRuntimeError::Invalid(
                    "CPA expectedVersion does not match the latest official release".into(),
                ));
            }
        }
        if previous
            .as_ref()
            .is_some_and(|managed| managed.current_version == release.version)
        {
            return Err(CpaRuntimeError::Invalid(format!(
                "CPA {} is already installed",
                release.version
            )));
        }
        let (archive, sha256) = self.download_verified_asset(&release).await?;
        self.ensure_cas(expected_revision, expected_generation)?;
        self.cpa_runtime
            .set_phase(CpaRuntimePhase::Installing, None);
        let root = runtime_dir(&self.data_dir);
        reject_reparse_ancestors(&root)?;
        fs::create_dir_all(root.join("auth")).map_err(fs_error)?;
        fs::create_dir_all(root.join("logs")).map_err(fs_error)?;
        fs::create_dir_all(root.join("versions")).map_err(fs_error)?;
        reject_reparse_ancestors(&root)?;
        let candidate_dir = version_dir(&self.data_dir, &release.version)?;
        match fs::symlink_metadata(&candidate_dir) {
            Ok(_) => {
                return Err(CpaRuntimeError::Conflict(
                    "the target CPA version directory already exists".into(),
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(fs_error(error)),
        }
        let token = format!("{}-{}", release.version, uuid::Uuid::new_v4().simple());
        let staging = root.join("versions").join(format!(".staging-{token}"));
        let archive_path = root.join("versions").join(format!(
            ".staging-{token}.{}",
            release.archive_kind.extension()
        ));
        fs::write(&archive_path, &archive).map_err(fs_error)?;
        let prepared = (|| {
            extract::extract_release(&archive_path, &staging, release.archive_kind)?;
            find_managed_executable(&staging)?;
            atomic_write(&staging.join(ASSET_SHA_NAME), sha256.as_bytes())?;
            fs::rename(&staging, &candidate_dir).map_err(fs_error)
        })();
        let _ = fs::remove_file(&archive_path);
        if let Err(error) = prepared {
            let _ = remove_known_path(&staging);
            return Err(error);
        }
        let mut candidate_guard = CandidateDirGuard::new(candidate_dir.clone());

        let port = select_port(previous.as_ref().map(|item| item.port))?;
        let config_path = root.join(CONFIG_NAME);
        let config_before = match mode {
            InstallMode::Fresh => match fs::symlink_metadata(&config_path) {
                Ok(_) => {
                    return Err(CpaRuntimeError::Conflict(
                        "a CPA config already exists without a managed owner manifest".into(),
                    ));
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(error) => return Err(fs_error(error)),
            },
            InstallMode::Update => Some(fs::read(&config_path).map_err(fs_error)?),
        };
        let mut config_guard = FileRestoreGuard::new(config_path.clone(), config_before.clone());
        let previous_config_path = root.join(PREVIOUS_CONFIG_NAME);
        let previous_config_before = fs::read(&previous_config_path).ok();
        let mut previous_config_guard =
            FileRestoreGuard::new(previous_config_path.clone(), previous_config_before.clone());
        let persistence_before = self.capture_persistence_backup()?;
        if let Some(previous) = previous.as_ref() {
            let current_dir = version_dir(&self.data_dir, &previous.current_version)?;
            reject_reparse_tree(&current_dir)?;
            let current_sha = current_dir.join(ASSET_SHA_NAME);
            if !current_sha.exists() {
                atomic_write(&current_sha, previous.asset_sha256.as_bytes())?;
            }
        }
        let ManagedSecrets {
            management_key,
            inference_key,
            mut extra_keys,
        } = secrets;
        if let Some(config_before) = config_before.as_deref() {
            extra_keys = config_extras(config_before, &inference_key)?;
        }
        write_config_yaml(
            &config_path,
            port,
            &root.join("auth"),
            &inference_key,
            &extra_keys,
        )?;

        let host = self.cpa_runtime.host()?.clone();
        let was_running = host.owned_running();
        if was_running {
            host.stop_owned()?;
        }
        if tcp_open(port) {
            let error = CpaRuntimeError::Conflict(format!(
                "loopback port {port} is already in use; OCG will not stop an external CPA"
            ));
            let compensation = self.restore_candidate_failure(
                &host,
                previous.as_ref(),
                config_before.as_deref(),
                was_running,
                &management_key,
            );
            return Err(with_compensation_error(error, compensation));
        }

        self.cpa_runtime.set_phase(CpaRuntimePhase::Starting, None);
        if let Err(error) =
            self.start_version(&host, &release.version, &config_path, &management_key)
        {
            let compensation = self
                .restore_candidate_failure_verified(
                    &host,
                    previous.as_ref(),
                    config_before.as_deref(),
                    was_running,
                    &management_key,
                    &inference_key,
                )
                .await;
            return Err(with_compensation_error(error, compensation));
        }
        let models = match self
            .probe_candidate(port, &management_key, &inference_key)
            .await
        {
            Ok(models) => models,
            Err(error) => {
                let compensation = self
                    .restore_candidate_failure_verified(
                        &host,
                        previous.as_ref(),
                        config_before.as_deref(),
                        was_running,
                        &management_key,
                        &inference_key,
                    )
                    .await;
                return Err(with_compensation_error(error, compensation));
            }
        };
        if let Err(error) = self.ensure_cas(expected_revision, expected_generation) {
            let compensation = self
                .restore_candidate_failure_verified(
                    &host,
                    previous.as_ref(),
                    config_before.as_deref(),
                    was_running,
                    &management_key,
                    &inference_key,
                )
                .await;
            return Err(with_compensation_error(error, compensation));
        }

        if mode == InstallMode::Update && !was_running {
            host.stop_owned()?;
        }

        let previous_version = previous.as_ref().and_then(|item| {
            (item.current_version != release.version).then(|| item.current_version.clone())
        });
        let next_managed = ManagedCpa {
            current_version: release.version,
            previous_version,
            asset_sha256: sha256,
            port,
        };
        let committed = {
            let _settings = self.settings_update.lock();
            self.ensure_cas(expected_revision, expected_generation)
                .and_then(|_| {
                    restore_optional_file(&previous_config_path, config_before.as_deref())?;
                    self.persist_managed_connection(port, &management_key, &inference_key, models)?;
                    save_managed(&self.data_dir, &next_managed)?;
                    self.bump_settings_revision();
                    Ok(())
                })
        };
        let committed = prune_versions_after_commit(
            committed,
            &root.join("versions"),
            &next_managed.current_version,
            next_managed.previous_version.as_deref(),
        );
        if let Err(error) = committed {
            let persistence_restore = self.restore_persistence_backup(persistence_before);
            let previous_config_restore =
                restore_optional_file(&previous_config_path, previous_config_before.as_deref());
            let runtime_restore = self
                .restore_candidate_failure_verified(
                    &host,
                    previous.as_ref(),
                    config_before.as_deref(),
                    was_running,
                    &management_key,
                    &inference_key,
                )
                .await;
            if let Err(compensation) = persistence_restore
                .and(previous_config_restore)
                .and(runtime_restore)
            {
                return Err(CpaRuntimeError::Failed(format!(
                    "{error}; restoring the previous CPA state also failed: {compensation}"
                )));
            }
            return Err(error);
        }
        candidate_guard.keep();
        config_guard.keep();
        previous_config_guard.keep();
        Ok(self.cpa_runtime_snapshot())
    }

    pub async fn start_cpa_runtime(
        &self,
        expected_revision: u64,
        expected_generation: u64,
    ) -> Result<CpaRuntimeSnapshot, CpaRuntimeError> {
        self.require_supported()?;
        if std::env::var_os(crate::cpa::CPA_BASE_URL_ENV).is_some() {
            return Err(CpaRuntimeError::Conflict(
                "OCG_CPA_BASE_URL selects an external CPA; unset it before starting the managed runtime"
                    .into(),
            ));
        }
        self.ensure_cas(expected_revision, expected_generation)?;
        let managed = require_managed(&self.data_dir)?;
        let _runtime_operation = self.cpa_runtime.begin_lifecycle_operation("start");
        let host = self.cpa_runtime.host()?.clone();
        if host.owned_running() {
            return Ok(self.cpa_runtime_snapshot());
        }
        if tcp_open(managed.port) {
            return Err(CpaRuntimeError::Conflict(format!(
                "loopback port {} is already in use; OCG will not stop an external CPA",
                managed.port
            )));
        }
        self.cpa_runtime.set_phase(CpaRuntimePhase::Starting, None);
        let config_path = runtime_dir(&self.data_dir).join(CONFIG_NAME);
        let secrets = match self.load_saved_secrets() {
            Ok(secrets) => secrets,
            Err(error) => {
                self.cpa_runtime
                    .set_phase(CpaRuntimePhase::Failed, Some(error.to_string()));
                return Err(error);
            }
        };
        let started = self.start_version(
            &host,
            &managed.current_version,
            &config_path,
            &secrets.management_key,
        );
        if let Err(error) = started {
            self.cpa_runtime
                .set_phase(CpaRuntimePhase::Failed, Some(error.to_string()));
            return Err(error);
        }
        let probe = self
            .probe_candidate(
                managed.port,
                &secrets.management_key,
                &secrets.inference_key,
            )
            .await;
        match probe {
            Ok(_) => {
                if let Err(error) = self.ensure_cas(expected_revision, expected_generation) {
                    let _ = host.stop_owned();
                    self.cpa_runtime.set_phase(CpaRuntimePhase::Idle, None);
                    return Err(error);
                }
                self.bump_settings_revision();
                self.cpa_runtime.set_phase(CpaRuntimePhase::Idle, None);
                Ok(self.cpa_runtime_snapshot())
            }
            Err(error) => {
                let _ = host.stop_owned();
                self.cpa_runtime
                    .set_phase(CpaRuntimePhase::Failed, Some(error.to_string()));
                Err(error)
            }
        }
    }

    pub fn stop_cpa_runtime(
        &self,
        expected_revision: u64,
        expected_generation: u64,
    ) -> Result<CpaRuntimeSnapshot, CpaRuntimeError> {
        self.require_supported()?;
        self.ensure_cas(expected_revision, expected_generation)?;
        let _ = require_managed(&self.data_dir)?;
        let _runtime_operation = self.cpa_runtime.begin_lifecycle_operation("stop");
        let host = self.cpa_runtime.host()?;
        if !host.owned_running() {
            return Err(CpaRuntimeError::Invalid(
                "no OCG-owned CPA process is running".into(),
            ));
        }
        host.stop_owned()?;
        self.bump_settings_revision();
        self.cpa_runtime.set_phase(CpaRuntimePhase::Idle, None);
        Ok(self.cpa_runtime_snapshot())
    }

    pub async fn rollback_cpa_runtime(
        &self,
        expected_revision: u64,
        expected_generation: u64,
    ) -> Result<CpaRuntimeSnapshot, CpaRuntimeError> {
        self.require_supported()?;
        if std::env::var_os(crate::cpa::CPA_BASE_URL_ENV).is_some() {
            return Err(CpaRuntimeError::Conflict(
                "OCG_CPA_BASE_URL selects an external CPA; unset it before rolling back the managed runtime"
                    .into(),
            ));
        }
        self.ensure_cas(expected_revision, expected_generation)?;
        let managed = require_managed(&self.data_dir)?;
        let _runtime_operation = self.cpa_runtime.begin_lifecycle_operation("rollback");
        let outcome = self
            .rollback_cpa_runtime_inner(managed, expected_revision, expected_generation)
            .await;
        match &outcome {
            Ok(_) => self.cpa_runtime.set_phase(CpaRuntimePhase::Idle, None),
            Err(error) => self
                .cpa_runtime
                .set_phase(CpaRuntimePhase::Failed, Some(error.to_string())),
        }
        outcome
    }

    async fn rollback_cpa_runtime_inner(
        &self,
        managed: ManagedCpa,
        expected_revision: u64,
        expected_generation: u64,
    ) -> Result<CpaRuntimeSnapshot, CpaRuntimeError> {
        let previous_version = managed.previous_version.clone().ok_or_else(|| {
            CpaRuntimeError::Invalid("no previous CPA version is available to roll back".into())
        })?;
        let root = runtime_dir(&self.data_dir);
        let config_path = root.join(CONFIG_NAME);
        let previous_config = root.join(PREVIOUS_CONFIG_NAME);
        let current_config_bytes = fs::read(&config_path).map_err(fs_error)?;
        let previous_config_bytes = fs::read(&previous_config)
            .map_err(|_| CpaRuntimeError::Invalid("previous CPA config.yaml is missing".into()))?;
        let previous_dir = version_dir(&self.data_dir, &previous_version)?;
        reject_reparse_tree(&previous_dir)?;
        find_managed_executable(&previous_dir)?;
        let previous_sha = read_asset_sha(&previous_dir)?;
        let secrets = self.load_saved_secrets()?;
        let _ = config_extras(&current_config_bytes, &secrets.inference_key)?;
        let _ = config_extras(&previous_config_bytes, &secrets.inference_key)?;
        let host = self.cpa_runtime.host()?.clone();
        let was_running = host.owned_running();
        if was_running {
            host.stop_owned()?;
        }
        atomic_write(&config_path, &previous_config_bytes)?;
        self.cpa_runtime.set_phase(CpaRuntimePhase::Starting, None);
        let restore = RollbackRestore {
            host: &host,
            managed: &managed,
            config_path: &config_path,
            current_config: &current_config_bytes,
            was_running,
            management_key: &secrets.management_key,
            inference_key: &secrets.inference_key,
        };
        if let Err(error) = self.start_version(
            &host,
            &previous_version,
            &config_path,
            &secrets.management_key,
        ) {
            return self.rollback_failed(error, restore).await;
        }
        let models = match self
            .probe_candidate(
                managed.port,
                &secrets.management_key,
                &secrets.inference_key,
            )
            .await
        {
            Ok(models) => models,
            Err(error) => {
                return self.rollback_failed(error, restore).await;
            }
        };
        if let Err(error) = self.ensure_cas(expected_revision, expected_generation) {
            return self.rollback_failed(error, restore).await;
        }
        if !was_running && let Err(error) = host.stop_owned() {
            return self.rollback_failed(error, restore).await;
        }
        if let Err(error) = atomic_write(&previous_config, &current_config_bytes) {
            return self.rollback_failed(error, restore).await;
        }
        let next_managed = ManagedCpa {
            current_version: previous_version,
            previous_version: Some(managed.current_version.clone()),
            asset_sha256: previous_sha,
            port: managed.port,
        };
        let mut persistence_before = Some(self.capture_persistence_backup()?);
        let committed = {
            let _settings = self.settings_update.lock();
            self.ensure_cas(expected_revision, expected_generation)
                .and_then(|_| {
                    self.activate_cpa_model_catalog(
                        models,
                        &format!("http://127.0.0.1:{}", managed.port),
                        Utc::now(),
                    )
                    .map_err(|error| CpaRuntimeError::Failed(error.to_string()))?;
                    if let Err(error) = save_managed(&self.data_dir, &next_managed) {
                        let restore = self.restore_persistence_backup(
                            persistence_before
                                .take()
                                .expect("rollback persistence backup must be available"),
                        );
                        return Err(with_compensation_error(error, restore));
                    }
                    self.bump_settings_revision();
                    Ok(())
                })
        };
        if let Err(error) = committed {
            let _ = restore_optional_file(&previous_config, Some(&previous_config_bytes));
            return self.rollback_failed(error, restore).await;
        }
        Ok(self.cpa_runtime_snapshot())
    }

    pub async fn update_cpa_runtime(
        &self,
        expected_revision: u64,
        expected_generation: u64,
        expected_version: Option<&str>,
    ) -> Result<CpaRuntimeSnapshot, CpaRuntimeError> {
        self.install_or_update_cpa_runtime(
            InstallMode::Update,
            expected_revision,
            expected_generation,
            expected_version,
        )
        .await
    }

    pub async fn remove_cpa_runtime(
        &self,
        expected_revision: u64,
        expected_generation: u64,
    ) -> Result<CpaRuntimeSnapshot, CpaRuntimeError> {
        self.require_supported()?;
        self.ensure_cas(expected_revision, expected_generation)?;
        let managed = require_managed(&self.data_dir)?;
        let _ = managed;
        let _runtime_operation = self.cpa_runtime.begin_lifecycle_operation("remove");
        let host = self.cpa_runtime.host()?.clone();
        if host.owned_running() {
            host.stop_owned()?;
        }
        let root = runtime_dir(&self.data_dir);
        let mut persistence_before = Some(self.capture_persistence_backup()?);
        {
            let _settings = self.settings_update.lock();
            self.ensure_cas(expected_revision, expected_generation)?;
            // Keep the owner marker until every canonical owned artifact is gone
            // and the database has disconnected. A failed earlier removal is
            // therefore safe to retry as an owned removal.
            remove_known_path(&root.join(CONFIG_NAME))?;
            remove_known_path(&root.join(PREVIOUS_CONFIG_NAME))?;
            remove_known_path(&root.join("logs"))?;
            remove_known_path(&root.join("versions"))?;
            remove_known_path(&root.join("auth"))?;
            if let Err(error) = self
                .disconnect_cpa_integration()
                .map_err(|error| CpaRuntimeError::Failed(error.to_string()))
            {
                let restore = self.restore_persistence_backup(
                    persistence_before
                        .take()
                        .expect("remove persistence backup must be available"),
                );
                return Err(with_compensation_error(error, restore));
            }
            if let Err(error) = remove_known_path(&root.join(MANAGED_NAME)) {
                let restore = self.restore_persistence_backup(
                    persistence_before
                        .take()
                        .expect("remove persistence backup must be available"),
                );
                return Err(with_compensation_error(error, restore));
            }
            self.bump_settings_revision();
        }
        self.cpa_runtime.set_phase(CpaRuntimePhase::Idle, None);
        Ok(self.cpa_runtime_snapshot())
    }

    pub async fn list_cpa_runtime_keys(&self) -> Result<Vec<CpaRuntimeKeyView>, CpaRuntimeError> {
        self.require_supported()?;
        let _ = require_managed(&self.data_dir)?;
        let protected = self.load_saved_secrets()?.inference_key;
        let protected_fingerprint = fingerprint_key(&protected);
        let keys = self.managed_config_keys()?;
        if !keys.iter().any(|key| key == &protected) {
            return Err(CpaRuntimeError::Failed(
                "managed CPA config does not contain the protected OCG key".into(),
            ));
        }
        Ok(keys
            .into_iter()
            .map(|secret| {
                let fingerprint = fingerprint_key(&secret);
                CpaRuntimeKeyView {
                    protected: fingerprint == protected_fingerprint,
                    hint: key_hint(&secret),
                    fingerprint,
                }
            })
            .collect())
    }

    pub async fn create_cpa_runtime_key(
        &self,
        expected_revision: u64,
        expected_generation: u64,
    ) -> Result<CpaRuntimeKeyCreated, CpaRuntimeError> {
        self.require_supported()?;
        self.ensure_cas(expected_revision, expected_generation)?;
        let _runtime_operation = self.cpa_runtime.begin_operation("create-client-key");
        let _ = require_managed(&self.data_dir)?;
        let mut keys = self.managed_config_keys()?;
        let secret = generate_secret()?;
        keys.push(secret.clone());
        self.commit_client_keys(expected_revision, expected_generation, keys, None)
            .await?;
        Ok(CpaRuntimeKeyCreated {
            fingerprint: fingerprint_key(&secret),
            hint: key_hint(&secret),
            secret,
        })
    }

    pub async fn delete_cpa_runtime_key(
        &self,
        expected_revision: u64,
        expected_generation: u64,
        fingerprint: &str,
    ) -> Result<(), CpaRuntimeError> {
        self.require_supported()?;
        self.ensure_cas(expected_revision, expected_generation)?;
        let _runtime_operation = self.cpa_runtime.begin_operation("delete-client-key");
        let _ = require_managed(&self.data_dir)?;
        validate_fingerprint(fingerprint)?;
        let protected = fingerprint_key(&self.load_saved_secrets()?.inference_key);
        if fingerprint == protected {
            return Err(CpaRuntimeError::Invalid(
                "the OCG-protected CPA Inference Key cannot be deleted".into(),
            ));
        }
        let original = self.managed_config_keys()?;
        if original
            .iter()
            .all(|secret| fingerprint_key(secret) != fingerprint)
        {
            return Err(CpaRuntimeError::Invalid(
                "CPA client key was not found".into(),
            ));
        }
        let remaining: Vec<String> = original
            .into_iter()
            .filter(|secret| fingerprint_key(secret) != fingerprint)
            .collect();
        self.commit_client_keys(expected_revision, expected_generation, remaining, None)
            .await
    }

    pub async fn rotate_cpa_runtime_key(
        &self,
        expected_revision: u64,
        expected_generation: u64,
        fingerprint: &str,
    ) -> Result<CpaRuntimeKeyCreated, CpaRuntimeError> {
        self.require_supported()?;
        self.ensure_cas(expected_revision, expected_generation)?;
        let _runtime_operation = self.cpa_runtime.begin_operation("rotate-client-key");
        let _ = require_managed(&self.data_dir)?;
        validate_fingerprint(fingerprint)?;
        let saved = self.load_saved_secrets()?;
        let protected_fingerprint = fingerprint_key(&saved.inference_key);
        let mut keys = self.managed_config_keys()?;
        let index = keys
            .iter()
            .position(|secret| fingerprint_key(secret) == fingerprint)
            .ok_or_else(|| CpaRuntimeError::Invalid("CPA client key was not found".into()))?;
        let secret = generate_secret()?;
        keys[index] = secret.clone();
        let new_protected = (fingerprint == protected_fingerprint).then(|| secret.clone());
        self.commit_client_keys(expected_revision, expected_generation, keys, new_protected)
            .await?;
        Ok(CpaRuntimeKeyCreated {
            fingerprint: fingerprint_key(&secret),
            hint: key_hint(&secret),
            secret,
        })
    }

    fn require_supported(&self) -> Result<(), CpaRuntimeError> {
        if self.cpa_runtime.supported() {
            Ok(())
        } else {
            Err(CpaRuntimeError::Unavailable(UNAVAILABLE_REASON.into()))
        }
    }

    fn ensure_cas(
        &self,
        expected_revision: u64,
        expected_generation: u64,
    ) -> Result<(), CpaRuntimeError> {
        if expected_revision != self.settings_revision()
            || expected_generation != self.process_generation()
        {
            Err(CpaRuntimeError::Conflict("revisionConflict".into()))
        } else {
            Ok(())
        }
    }

    fn start_version(
        &self,
        host: &CpaRuntimeHost,
        version: &str,
        config_path: &Path,
        management_password: &str,
    ) -> Result<PathBuf, CpaRuntimeError> {
        validate_managed_secret(management_password)?;
        let working_dir = version_dir(&self.data_dir, version)?;
        reject_reparse_tree(&working_dir)?;
        let executable = find_managed_executable(&working_dir)?;
        ensure_unix_executable(&executable)?;
        let config = fs::read_to_string(config_path).map_err(fs_error)?;
        let log_secrets = parse_api_keys_from_yaml(&config)?
            .into_iter()
            .chain(std::iter::once(management_password.to_string()))
            .map(CpaRuntimeSecret::new)
            .collect();
        host.start_owned(&CpaRuntimeProcessSpec {
            codex_device_login: false,
            executable: executable.clone(),
            config_path: config_path.to_path_buf(),
            working_dir,
            management_password: CpaRuntimeSecret::new(management_password),
            log_secrets,
        })?;
        Ok(executable)
    }

    fn restore_candidate_failure(
        &self,
        host: &CpaRuntimeHost,
        previous: Option<&ManagedCpa>,
        config_before: Option<&[u8]>,
        was_running: bool,
        management_key: &str,
    ) -> Result<(), CpaRuntimeError> {
        host.stop_owned()?;
        self.cpa_runtime.cache_failure_logs(host.logs());
        let config_path = runtime_dir(&self.data_dir).join(CONFIG_NAME);
        restore_optional_file(&config_path, config_before)?;
        if was_running {
            let previous = previous.ok_or_else(|| {
                CpaRuntimeError::Failed(
                    "cannot restore the previously running CPA without an owner manifest".into(),
                )
            })?;
            self.start_version(
                host,
                &previous.current_version,
                &config_path,
                management_key,
            )?;
        }
        Ok(())
    }

    async fn restore_candidate_failure_verified(
        &self,
        host: &CpaRuntimeHost,
        previous: Option<&ManagedCpa>,
        config_before: Option<&[u8]>,
        was_running: bool,
        management_key: &str,
        inference_key: &str,
    ) -> Result<(), CpaRuntimeError> {
        self.restore_candidate_failure(host, previous, config_before, was_running, management_key)?;
        if let Some(previous) = previous.filter(|_| was_running) {
            self.probe_candidate(previous.port, management_key, inference_key)
                .await?;
        }
        Ok(())
    }

    async fn rollback_failed(
        &self,
        original: CpaRuntimeError,
        restore: RollbackRestore<'_>,
    ) -> Result<CpaRuntimeSnapshot, CpaRuntimeError> {
        let compensation = (|| {
            restore.host.stop_owned()?;
            self.cpa_runtime.cache_failure_logs(restore.host.logs());
            atomic_write(restore.config_path, restore.current_config)?;
            save_managed(&self.data_dir, restore.managed)?;
            if restore.was_running {
                self.start_version(
                    restore.host,
                    &restore.managed.current_version,
                    restore.config_path,
                    restore.management_key,
                )?;
            }
            Ok::<(), CpaRuntimeError>(())
        })();
        let compensation = match compensation {
            Ok(()) if restore.was_running => self
                .probe_candidate(
                    restore.managed.port,
                    restore.management_key,
                    restore.inference_key,
                )
                .await
                .map(|_| ()),
            other => other,
        };
        match compensation {
            Ok(()) => Err(original),
            Err(compensation) => Err(CpaRuntimeError::Failed(format!(
                "{original}; restoring the previous CPA runtime also failed: {compensation}"
            ))),
        }
    }

    fn capture_persistence_backup(&self) -> Result<CpaPersistenceBackup, CpaRuntimeError> {
        let db = self.db.lock();
        Ok(CpaPersistenceBackup {
            record: db
                .cpa_integration()
                .map_err(|error| CpaRuntimeError::Failed(error.to_string()))?,
            account: db
                .get_account(CPA_ACCOUNT_ID)
                .map_err(|error| CpaRuntimeError::Failed(error.to_string()))?,
            catalog: db
                .cpa_model_catalog()
                .map_err(|error| CpaRuntimeError::Failed(error.to_string()))?,
        })
    }

    fn restore_persistence_backup(
        &self,
        backup: CpaPersistenceBackup,
    ) -> Result<(), CpaRuntimeError> {
        self.disconnect_cpa_integration()
            .map_err(|error| CpaRuntimeError::Failed(error.to_string()))?;
        match (backup.record, backup.account) {
            (Some(record), Some(account)) => {
                self.db
                    .lock()
                    .upsert_cpa_integration(
                        &account,
                        &record.base_url,
                        &record.management_key_cipher,
                    )
                    .map_err(|error| CpaRuntimeError::Failed(error.to_string()))?;
                if let Some(catalog) = backup.catalog {
                    self.activate_cpa_model_catalog(
                        catalog.models,
                        &catalog.source_url,
                        catalog.refreshed_at.unwrap_or_else(Utc::now),
                    )
                    .map_err(|error| CpaRuntimeError::Failed(error.to_string()))?;
                }
                self.routing.reset();
                Ok(())
            }
            (None, None) => Ok(()),
            _ => Err(CpaRuntimeError::Failed(
                "the previous CPA persistence snapshot was inconsistent".into(),
            )),
        }
    }

    async fn probe_candidate(
        &self,
        port: u16,
        management_key: &str,
        inference_key: &str,
    ) -> Result<Vec<CpaCatalogModel>, CpaRuntimeError> {
        // `/v1/models` is CPA's strongest non-billable Inference-Key check.
        // A real completion would prove provider usability but could consume a
        // subscription, so installation separately proves health, Management
        // authentication/version via `accounts`, and authenticated catalog access.
        let client = CpaClient::new(
            &self.config(),
            &format!("http://127.0.0.1:{port}"),
            management_key.to_string(),
            inference_key.to_string(),
            false,
        )?;
        let mut last = CpaRuntimeError::Unreachable("CPA candidate did not become ready".into());
        for _ in 0..PROBE_ATTEMPTS {
            match client.health().await {
                Ok(()) => match client.accounts().await {
                    Ok(_) => match client.models().await {
                        Ok(models) => return Ok(models),
                        Err(error) => {
                            last =
                                redact_runtime_error(error.into(), &[management_key, inference_key])
                        }
                    },
                    Err(error) => {
                        last = redact_runtime_error(error.into(), &[management_key, inference_key])
                    }
                },
                Err(error) => {
                    last = redact_runtime_error(error.into(), &[management_key, inference_key])
                }
            }
            tokio::time::sleep(PROBE_DELAY).await;
        }
        Err(last)
    }

    fn managed_secrets(
        &self,
        mode: InstallMode,
        config_path: &Path,
    ) -> Result<ManagedSecrets, CpaRuntimeError> {
        match mode {
            InstallMode::Update => {
                let saved = self.load_saved_secrets()?;
                let config = fs::read(config_path).map_err(fs_error)?;
                let extra_keys = config_extras(&config, &saved.inference_key)?;
                Ok(ManagedSecrets {
                    management_key: saved.management_key,
                    inference_key: saved.inference_key,
                    extra_keys,
                })
            }
            InstallMode::Fresh => {
                if load_managed(&self.data_dir)?.is_some() {
                    return Err(CpaRuntimeError::Conflict(
                        "CPA managed runtime is already installed".into(),
                    ));
                }
                match fs::symlink_metadata(config_path) {
                    Ok(_) => {
                        return Err(CpaRuntimeError::Conflict(
                            "a CPA config already exists without a managed owner manifest".into(),
                        ));
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(fs_error(error)),
                }
                let persistence = self.capture_persistence_backup()?;
                let saved = match (&persistence.record, &persistence.account) {
                    (None, None) => None,
                    (Some(_), Some(_)) => Some(self.load_saved_secrets()?),
                    _ => {
                        return Err(CpaRuntimeError::Failed(
                            "the existing CPA persistence state is inconsistent".into(),
                        ));
                    }
                };
                let (management_key, inference_key) = match saved {
                    Some(saved) => (saved.management_key, saved.inference_key),
                    None => (generate_secret()?, generate_secret()?),
                };
                Ok(ManagedSecrets {
                    management_key,
                    inference_key,
                    extra_keys: Vec::new(),
                })
            }
        }
    }

    fn managed_config_keys(&self) -> Result<Vec<String>, CpaRuntimeError> {
        let _ = require_managed(&self.data_dir)?;
        let config_path = runtime_dir(&self.data_dir).join(CONFIG_NAME);
        let text = fs::read_to_string(&config_path).map_err(fs_error)?;
        parse_api_keys_from_yaml(&text)
    }

    async fn commit_client_keys(
        &self,
        expected_revision: u64,
        expected_generation: u64,
        next_keys: Vec<String>,
        new_protected: Option<String>,
    ) -> Result<(), CpaRuntimeError> {
        let managed = require_managed(&self.data_dir)?;
        let saved = self.load_saved_secrets()?;
        let protected = new_protected
            .as_deref()
            .unwrap_or(&saved.inference_key)
            .to_string();
        if !next_keys.iter().any(|key| key == &protected) {
            return Err(CpaRuntimeError::Invalid(
                "the protected OCG CPA key must remain present".into(),
            ));
        }
        let config_path = runtime_dir(&self.data_dir).join(CONFIG_NAME);
        let config_before = fs::read(&config_path).map_err(fs_error)?;
        let previous_config_path = runtime_dir(&self.data_dir).join(PREVIOUS_CONFIG_NAME);
        let previous_config_before = fs::read(&previous_config_path).ok();
        let client = self.saved_cpa_client()?;
        let running = self
            .cpa_runtime
            .host
            .get()
            .is_some_and(|host| host.owned_running());
        let upstream_before = if running {
            if let Some(host) = self.cpa_runtime.host.get() {
                for secret in &next_keys {
                    host.add_log_secret(&CpaRuntimeSecret::new(secret.clone()));
                }
            }
            let known_secrets = next_keys
                .iter()
                .map(String::as_str)
                .chain(std::iter::once(saved.management_key.as_str()))
                .collect::<Vec<_>>();
            let keys = client
                .api_keys()
                .await
                .map_err(|error| redact_runtime_error(error.into(), &known_secrets))?;
            self.ensure_cas(expected_revision, expected_generation)?;
            client
                .replace_api_keys(&next_keys)
                .await
                .map_err(|error| redact_runtime_error(error.into(), &known_secrets))?;
            if let Err(error) = self.ensure_cas(expected_revision, expected_generation) {
                let compensation = client.replace_api_keys(&keys).await;
                return match compensation {
                    Ok(()) => Err(error),
                    Err(compensation) => {
                        let compensation = redact_text(&compensation.to_string(), &known_secrets);
                        Err(CpaRuntimeError::Failed(format!(
                            "{error}; restoring CPA client keys also failed: {compensation}"
                        )))
                    }
                };
            }
            Some(keys)
        } else {
            None
        };

        let extras = next_keys
            .iter()
            .filter(|key| *key != &protected)
            .cloned()
            .collect::<Vec<_>>();
        let local_result = {
            let _settings = self.settings_update.lock();
            self.ensure_cas(expected_revision, expected_generation)
                .and_then(|_| {
                    write_config_yaml(
                        &config_path,
                        managed.port,
                        &runtime_dir(&self.data_dir).join("auth"),
                        &protected,
                        &extras,
                    )?;
                    if previous_config_path.exists()
                        && let Err(error) = write_config_yaml(
                            &previous_config_path,
                            managed.port,
                            &runtime_dir(&self.data_dir).join("auth"),
                            &protected,
                            &extras,
                        )
                    {
                        let _ = atomic_write(&config_path, &config_before);
                        return Err(error);
                    }
                    if let Some(new_protected) = new_protected.as_deref()
                        && let Err(error) = self.persist_inference_key(new_protected)
                    {
                        let restore = atomic_write(&config_path, &config_before);
                        let previous_restore = restore_optional_file(
                            &previous_config_path,
                            previous_config_before.as_deref(),
                        );
                        return match restore {
                            Ok(()) if previous_restore.is_ok() => Err(error),
                            Err(restore) => Err(CpaRuntimeError::Failed(format!(
                                "{error}; restoring managed CPA config also failed: {restore}"
                            ))),
                            Ok(()) => Err(CpaRuntimeError::Failed(format!(
                                "{error}; restoring previous CPA config also failed"
                            ))),
                        };
                    }
                    self.bump_settings_revision();
                    Ok(())
                })
        };
        if let Err(error) = local_result {
            let _ = atomic_write(&config_path, &config_before);
            let _ = restore_optional_file(&previous_config_path, previous_config_before.as_deref());
            if let Some(upstream_before) = upstream_before
                && let Err(compensation) = client.replace_api_keys(&upstream_before).await
            {
                let mut secrets = next_keys.iter().map(String::as_str).collect::<Vec<_>>();
                secrets.extend(upstream_before.iter().map(String::as_str));
                secrets.push(saved.management_key.as_str());
                let compensation = redact_text(&compensation.to_string(), &secrets);
                return Err(CpaRuntimeError::Failed(format!(
                    "{error}; restoring CPA client keys also failed: {compensation}"
                )));
            }
            return Err(error);
        }
        Ok(())
    }

    fn persist_managed_connection(
        &self,
        port: u16,
        management_key: &str,
        inference_key: &str,
        models: Vec<CpaCatalogModel>,
    ) -> Result<(), CpaRuntimeError> {
        let base_url = format!("http://127.0.0.1:{port}");
        let management_key_cipher = self
            .encrypt_key(management_key)
            .map_err(|error| CpaRuntimeError::Failed(error.to_string()))?;
        let inference_key_cipher = self
            .encrypt_key(inference_key)
            .map_err(|error| CpaRuntimeError::Failed(error.to_string()))?;
        let now = Utc::now();
        let existing = self
            .db
            .lock()
            .get_account(CPA_ACCOUNT_ID)
            .map_err(|error| CpaRuntimeError::Failed(error.to_string()))?;
        let account = ModelAccount {
            id: CPA_ACCOUNT_ID.to_string(),
            provider_id: CPA_PROVIDER_ID.to_string(),
            credential_kind: CredentialKind::ApiKey,
            quota_scope: QuotaScope::Key,
            name: CPA_ACCOUNT_NAME.to_string(),
            username: None,
            password_cipher: None,
            key_cipher: inference_key_cipher,
            enabled: existing.as_ref().is_some_and(|item| item.enabled),
            account_type: AccountType::Key,
            setup_step: AccountSetupStep::Ready,
            referral_code: None,
            purchase_date: String::new(),
            expires_on: String::new(),
            cooldown_until: None,
            cooldown_generic_until: None,
            cooldown_5h_until: None,
            cooldown_week_until: None,
            cooldown_month_until: None,
            cooldown_free_until: None,
            last_error: None,
            auth_error: None,
            notes: None,
            created_at: existing.as_ref().map_or(now, |item| item.created_at),
            updated_at: now,
        };
        let previous = self
            .db
            .lock()
            .cpa_model_catalog()
            .map_err(|error| CpaRuntimeError::Failed(error.to_string()))?;
        let models = CpaCatalogModel::merge_refresh(
            models,
            previous
                .as_ref()
                .map(|item| item.models.as_slice())
                .unwrap_or(&[]),
        );
        self.db
            .lock()
            .upsert_cpa_integration(&account, &base_url, &management_key_cipher)
            .map_err(|error| CpaRuntimeError::Failed(error.to_string()))?;
        self.activate_cpa_model_catalog(models, &base_url, now)
            .map_err(|error| CpaRuntimeError::Failed(error.to_string()))?;
        self.routing.reset();
        Ok(())
    }

    fn persist_inference_key(&self, inference_key: &str) -> Result<(), CpaRuntimeError> {
        let (record, account) = {
            let db = self.db.lock();
            (
                db.cpa_integration()
                    .map_err(|error| CpaRuntimeError::Failed(error.to_string()))?,
                db.get_account(CPA_ACCOUNT_ID)
                    .map_err(|error| CpaRuntimeError::Failed(error.to_string()))?,
            )
        };
        let record =
            record.ok_or_else(|| CpaRuntimeError::Invalid("CPA is not configured".into()))?;
        let mut account = account
            .ok_or_else(|| CpaRuntimeError::Invalid("CPA singleton account is missing".into()))?;
        account.key_cipher = self
            .encrypt_key(inference_key)
            .map_err(|error| CpaRuntimeError::Failed(error.to_string()))?;
        account.updated_at = Utc::now();
        self.db
            .lock()
            .upsert_cpa_integration(&account, &record.base_url, &record.management_key_cipher)
            .map_err(|error| CpaRuntimeError::Failed(error.to_string()))?;
        self.routing.reset();
        Ok(())
    }

    fn load_saved_secrets(&self) -> Result<SavedSecrets, CpaRuntimeError> {
        let (record, account) = {
            let db = self.db.lock();
            (
                db.cpa_integration()
                    .map_err(|error| CpaRuntimeError::Failed(error.to_string()))?,
                db.get_account(CPA_ACCOUNT_ID)
                    .map_err(|error| CpaRuntimeError::Failed(error.to_string()))?,
            )
        };
        let record =
            record.ok_or_else(|| CpaRuntimeError::Invalid("CPA is not configured".into()))?;
        let account = account
            .ok_or_else(|| CpaRuntimeError::Invalid("CPA singleton account is missing".into()))?;
        let saved = SavedSecrets {
            management_key: self
                .decrypt_key(&record.management_key_cipher)
                .map_err(|error| CpaRuntimeError::Failed(error.to_string()))?,
            inference_key: self
                .decrypt_key(&account.key_cipher)
                .map_err(|error| CpaRuntimeError::Failed(error.to_string()))?,
        };
        validate_managed_secret(&saved.management_key)?;
        validate_managed_secret(&saved.inference_key)?;
        Ok(saved)
    }

    fn saved_cpa_client(&self) -> Result<CpaClient, CpaRuntimeError> {
        let managed = require_managed(&self.data_dir)?;
        let saved = self.load_saved_secrets()?;
        let base_url = format!("http://127.0.0.1:{}", managed.port);
        Ok(CpaClient::new(
            &self.config(),
            &base_url,
            saved.management_key,
            saved.inference_key,
            false,
        )?)
    }

    async fn fetch_latest_release(&self) -> Result<ResolvedRelease, CpaRuntimeError> {
        let config = self.config();
        let client = github_client(&config)?;
        let release: GithubRelease = client
            .get(CPA_GITHUB_LATEST_API)
            .header(reqwest::header::ACCEPT, "application/vnd.github+json")
            .header(
                reqwest::header::USER_AGENT,
                concat!("ocg-manager/", env!("CARGO_PKG_VERSION")),
            )
            .timeout(REQUEST_TIMEOUT)
            .send()
            .await
            .map_err(|error| CpaRuntimeError::Unreachable(error.to_string()))?
            .error_for_status()
            .map_err(|error| CpaRuntimeError::Unreachable(error.to_string()))?
            .json()
            .await
            .map_err(|error| {
                CpaRuntimeError::Failed(format!("CPA GitHub release JSON is invalid: {error}"))
            })?;
        let version = normalize_release_version(&release.tag_name)?;
        let selected = current_cpa_release_asset()
            .ok_or_else(|| CpaRuntimeError::Unavailable(UNAVAILABLE_REASON.into()))?;
        let asset_name = selected.file_name(&version);
        let asset = release
            .assets
            .iter()
            .find(|asset| asset.name == asset_name)
            .ok_or_else(|| {
                CpaRuntimeError::Invalid(format!(
                    "latest CPA release does not contain {asset_name}"
                ))
            })?;
        let checksums = release
            .assets
            .iter()
            .find(|asset| asset.name == CHECKSUMS_NAME)
            .ok_or_else(|| {
                CpaRuntimeError::Invalid("latest CPA release is missing checksums.txt".into())
            })?;
        Ok(ResolvedRelease {
            version,
            asset_name,
            asset_url: asset.browser_download_url.clone(),
            checksums_url: checksums.browser_download_url.clone(),
            archive_kind: selected.archive_kind(),
        })
    }

    async fn download_verified_asset(
        &self,
        release: &ResolvedRelease,
    ) -> Result<(Vec<u8>, String), CpaRuntimeError> {
        let config = self.config();
        let client = github_client(&config)?;
        let checksums = download_bytes(&client, &release.checksums_url, MAX_CHECKSUM_BYTES).await?;
        let checksum_text = String::from_utf8(checksums)
            .map_err(|_| CpaRuntimeError::Invalid("checksums.txt is not valid UTF-8".into()))?;
        let expected = parse_checksum(&checksum_text, &release.asset_name)?;
        let archive = download_bytes(&client, &release.asset_url, MAX_ARCHIVE_BYTES).await?;
        let actual = format!("{:x}", Sha256::digest(&archive));
        if actual != expected {
            return Err(CpaRuntimeError::Invalid(
                "CPA release SHA-256 does not match checksums.txt".into(),
            ));
        }
        Ok((archive, actual))
    }
}

struct SavedSecrets {
    management_key: String,
    inference_key: String,
}

struct ManagedSecrets {
    management_key: String,
    inference_key: String,
    extra_keys: Vec<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum InstallMode {
    Fresh,
    Update,
}

#[derive(Clone, Copy)]
struct RollbackRestore<'a> {
    host: &'a CpaRuntimeHost,
    managed: &'a ManagedCpa,
    config_path: &'a Path,
    current_config: &'a [u8],
    was_running: bool,
    management_key: &'a str,
    inference_key: &'a str,
}

struct CpaPersistenceBackup {
    record: Option<CpaIntegrationRecord>,
    account: Option<ModelAccount>,
    catalog: Option<CpaCatalogRecord>,
}

struct CandidateDirGuard {
    path: PathBuf,
    keep: bool,
}

struct FileRestoreGuard {
    path: PathBuf,
    before: Option<Vec<u8>>,
    keep: bool,
}

impl FileRestoreGuard {
    fn new(path: PathBuf, before: Option<Vec<u8>>) -> Self {
        Self {
            path,
            before,
            keep: false,
        }
    }

    fn keep(&mut self) {
        self.keep = true;
    }
}

impl Drop for FileRestoreGuard {
    fn drop(&mut self) {
        if !self.keep {
            let _ = restore_optional_file(&self.path, self.before.as_deref());
        }
    }
}

impl CandidateDirGuard {
    fn new(path: PathBuf) -> Self {
        Self { path, keep: false }
    }

    fn keep(&mut self) {
        self.keep = true;
    }
}

impl Drop for CandidateDirGuard {
    fn drop(&mut self) {
        if !self.keep {
            let _ = remove_known_path(&self.path);
        }
    }
}

struct ResolvedRelease {
    version: String,
    asset_name: String,
    asset_url: String,
    checksums_url: String,
    archive_kind: extract::CpaArchiveKind,
}

#[derive(Debug, Deserialize)]
struct GithubRelease {
    tag_name: String,
    #[serde(default)]
    assets: Vec<GithubAsset>,
}

#[derive(Debug, Deserialize)]
struct GithubAsset {
    name: String,
    browser_download_url: String,
}

fn github_client(config: &AppConfig) -> Result<reqwest::Client, CpaRuntimeError> {
    http_client::configured_builder(config)
        .and_then(|builder| {
            builder
                .timeout(DOWNLOAD_TIMEOUT)
                .build()
                .map_err(Into::into)
        })
        .map_err(|error| CpaRuntimeError::Failed(error.to_string()))
}

async fn download_bytes(
    client: &reqwest::Client,
    url: &str,
    max_bytes: usize,
) -> Result<Vec<u8>, CpaRuntimeError> {
    let response = client
        .get(url)
        .header(
            reqwest::header::USER_AGENT,
            concat!("ocg-manager/", env!("CARGO_PKG_VERSION")),
        )
        .timeout(DOWNLOAD_TIMEOUT)
        .send()
        .await
        .map_err(|error| CpaRuntimeError::Unreachable(error.to_string()))?
        .error_for_status()
        .map_err(|error| CpaRuntimeError::Unreachable(error.to_string()))?;
    if response
        .content_length()
        .is_some_and(|size| size > max_bytes as u64)
    {
        return Err(CpaRuntimeError::Invalid(
            "CPA download exceeds the size limit".into(),
        ));
    }
    // Content-Length can be missing or lie; never buffer more than max+1 bytes.
    read_limited_body(response, max_bytes).await
}

async fn read_limited_body(
    response: reqwest::Response,
    max_bytes: usize,
) -> Result<Vec<u8>, CpaRuntimeError> {
    let limit = max_bytes.saturating_add(1);
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| CpaRuntimeError::Unreachable(error.to_string()))?;
        let remaining = limit.saturating_sub(body.len());
        if remaining == 0 {
            return Err(CpaRuntimeError::Invalid(
                "CPA download exceeds the size limit".into(),
            ));
        }
        let take = remaining.min(chunk.len());
        body.extend_from_slice(&chunk[..take]);
        if body.len() > max_bytes {
            return Err(CpaRuntimeError::Invalid(
                "CPA download exceeds the size limit".into(),
            ));
        }
    }
    Ok(body)
}

fn write_config_yaml(
    path: &Path,
    port: u16,
    auth_dir: &Path,
    inference_key: &str,
    extra_keys: &[String],
) -> Result<(), CpaRuntimeError> {
    validate_managed_secret(inference_key)?;
    let mut seen = HashSet::new();
    if !seen.insert(inference_key) {
        return Err(CpaRuntimeError::Invalid(
            "managed CPA client keys must be unique".into(),
        ));
    }
    for key in extra_keys {
        validate_managed_secret(key)?;
        if !seen.insert(key) {
            return Err(CpaRuntimeError::Invalid(
                "managed CPA client keys must be unique".into(),
            ));
        }
    }
    let auth_dir = auth_dir.to_string_lossy().replace('\\', "/");
    let mut keys = String::new();
    for key in std::iter::once(inference_key).chain(extra_keys.iter().map(String::as_str)) {
        keys.push_str("  - \"");
        keys.push_str(&yaml_escape(key));
        keys.push_str("\"\n");
    }
    let body = format!(
        "host: \"127.0.0.1\"\nport: {port}\nauth-dir: \"{auth_dir}\"\ndebug: false\nlogging-to-file: false\nremote-management:\n  allow-remote: false\n  secret-key: \"\"\n  disable-control-panel: true\n  disable-auto-update-panel: true\napi-keys:\n{keys}"
    );
    atomic_write(path, body.as_bytes())
}

fn remove_known_path(path: &Path) -> Result<(), CpaRuntimeError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(fs_error(error)),
    };
    reject_reparse_tree(path)?;
    if metadata.is_dir() {
        fs::remove_dir_all(path).map_err(fs_error)
    } else {
        fs::remove_file(path).map_err(fs_error)
    }
}

fn is_reparse_path(path: &Path) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        fs::symlink_metadata(path)
            .map(|metadata| metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0)
            .unwrap_or(false)
    }
    #[cfg(not(windows))]
    {
        fs::symlink_metadata(path)
            .map(|metadata| metadata.file_type().is_symlink())
            .unwrap_or(false)
    }
}

fn yaml_escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), CpaRuntimeError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(fs_error)?;
    }
    reject_reparse_ancestors(parent_path(path)?)?;
    if is_reparse_path(path) {
        return Err(CpaRuntimeError::Invalid(
            "refusing to replace a CPA file that is a reparse point".into(),
        ));
    }
    let parent = parent_path(path)?;
    let tmp = parent.join(format!(".ocg-cpa-{}.tmp", uuid::Uuid::new_v4().simple()));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp)
        .map_err(fs_error)?;
    use std::io::Write as _;
    if let Err(error) = file.write_all(bytes).and_then(|_| file.sync_all()) {
        let _ = fs::remove_file(&tmp);
        return Err(fs_error(error));
    }
    drop(file);
    let result = replace_file(&tmp, path);
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

fn restore_optional_file(path: &Path, bytes: Option<&[u8]>) -> Result<(), CpaRuntimeError> {
    if let Some(bytes) = bytes {
        atomic_write(path, bytes)
    } else {
        remove_known_path(path)
    }
}

fn prune_old_versions(
    versions: &Path,
    current: &str,
    previous: &str,
) -> Result<(), CpaRuntimeError> {
    let current = normalize_release_version(current)?;
    let previous = normalize_release_version(previous)?;
    if current.eq_ignore_ascii_case(&previous) {
        return Err(CpaRuntimeError::Invalid(
            "current and previous CPA versions must be distinct".into(),
        ));
    }
    if !versions.exists() {
        return Ok(());
    }
    reject_reparse_tree(versions)?;
    for entry in fs::read_dir(versions).map_err(fs_error)? {
        let entry = entry.map_err(fs_error)?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.eq_ignore_ascii_case(&current) || name.eq_ignore_ascii_case(&previous) {
            if name != current && name != previous {
                return Err(CpaRuntimeError::Invalid(
                    "CPA version directory has ambiguous Windows casing".into(),
                ));
            }
            continue;
        }
        if name.starts_with('.') {
            continue;
        }
        let path = entry.path();
        remove_known_path(&path)?;
    }
    Ok(())
}

fn prune_versions_after_commit(
    committed: Result<(), CpaRuntimeError>,
    versions: &Path,
    current: &str,
    previous: Option<&str>,
) -> Result<(), CpaRuntimeError> {
    committed?;
    if let Some(previous) = previous {
        // Cleanup cannot invalidate an update whose connection and owner
        // manifest have already committed. A later update/remove can retry it.
        let _ = prune_old_versions(versions, current, previous);
    }
    Ok(())
}

fn read_asset_sha(version_dir: &Path) -> Result<String, CpaRuntimeError> {
    let value = fs::read_to_string(version_dir.join(ASSET_SHA_NAME)).map_err(fs_error)?;
    let value = value.trim();
    if value.len() != 64 || !value.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return Err(CpaRuntimeError::Invalid(
            "CPA version asset SHA-256 sidecar is invalid".into(),
        ));
    }
    Ok(value.to_ascii_lowercase())
}

fn find_managed_executable(dir: &Path) -> Result<PathBuf, CpaRuntimeError> {
    for name in [
        "cli-proxy-api.exe",
        "CLIProxyAPI.exe",
        "cli-proxy-api",
        "CLIProxyAPI",
    ] {
        let path = dir.join(name);
        if path.is_file() {
            return Ok(path);
        }
    }
    let mut found = None;
    for entry in fs::read_dir(dir).map_err(fs_error)? {
        let path = entry.map_err(fs_error)?.path();
        if !path.is_file() {
            continue;
        }
        let name = path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("");
        if name == ASSET_SHA_NAME || name.starts_with('.') {
            continue;
        }
        let is_windows_exe = path.extension().and_then(|value| value.to_str()) == Some("exe");
        let is_unix_binary = path.extension().is_none();
        if !is_windows_exe && !is_unix_binary {
            continue;
        }
        if found.is_some() {
            return Err(CpaRuntimeError::Invalid(
                "CPA release contains more than one executable".into(),
            ));
        }
        found = Some(path);
    }
    found.ok_or_else(|| {
        CpaRuntimeError::Invalid("CPA release archive does not contain an executable".into())
    })
}

fn ensure_unix_executable(path: &Path) -> Result<(), CpaRuntimeError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(path).map_err(fs_error)?.permissions().mode();
        fs::set_permissions(path, fs::Permissions::from_mode(mode | 0o111)).map_err(fs_error)?;
    }
    let _ = path;
    Ok(())
}

fn select_port(preferred: Option<u16>) -> Result<u16, CpaRuntimeError> {
    if let Some(port) = preferred.filter(|port| *port > 0) {
        return Ok(port);
    }
    bind_loopback(DEFAULT_PORT).or_else(|_| bind_loopback(0))
}

fn bind_loopback(port: u16) -> Result<u16, CpaRuntimeError> {
    let listener = std::net::TcpListener::bind(("127.0.0.1", port)).map_err(|error| {
        CpaRuntimeError::Failed(format!("failed to allocate a CPA loopback port: {error}"))
    })?;
    let port = listener
        .local_addr()
        .map_err(|error| {
            CpaRuntimeError::Failed(format!("failed to read CPA loopback port: {error}"))
        })?
        .port();
    drop(listener);
    Ok(port)
}

fn tcp_open(port: u16) -> bool {
    std::net::TcpStream::connect(("127.0.0.1", port)).is_ok()
}

fn parse_api_keys_from_yaml(text: &str) -> Result<Vec<String>, CpaRuntimeError> {
    let mut keys = Vec::new();
    let mut in_keys = false;
    let mut found_keys = false;
    for line in text.lines() {
        if line == "api-keys:" {
            if found_keys {
                return Err(CpaRuntimeError::Failed(
                    "managed CPA config contains more than one api-keys block".into(),
                ));
            }
            found_keys = true;
            in_keys = true;
            continue;
        }
        if in_keys {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            if !line.starts_with(' ') && !line.starts_with('\t') {
                break;
            }
            let encoded = trimmed.strip_prefix("- ").ok_or_else(|| {
                CpaRuntimeError::Failed("managed CPA api-keys block is malformed".into())
            })?;
            keys.push(parse_yaml_quoted_scalar(encoded)?);
        }
    }
    if !found_keys || keys.is_empty() {
        return Err(CpaRuntimeError::Failed(
            "managed CPA config has no client inference keys".into(),
        ));
    }
    let mut seen = HashSet::new();
    if keys
        .iter()
        .any(|key| validate_managed_secret(key).is_err() || !seen.insert(key.clone()))
    {
        return Err(CpaRuntimeError::Failed(
            "managed CPA config contains an invalid or duplicate client key".into(),
        ));
    }
    Ok(keys)
}

fn parse_yaml_quoted_scalar(encoded: &str) -> Result<String, CpaRuntimeError> {
    let body = encoded
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .ok_or_else(|| {
            CpaRuntimeError::Failed("managed CPA api-keys entries must be double quoted".into())
        })?;
    let mut value = String::with_capacity(body.len());
    let mut chars = body.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            value.push(ch);
            continue;
        }
        match chars.next() {
            Some('\\') => value.push('\\'),
            Some('"') => value.push('"'),
            _ => {
                return Err(CpaRuntimeError::Failed(
                    "managed CPA api-keys entry has an invalid escape".into(),
                ));
            }
        }
    }
    Ok(value)
}

fn config_extras(config: &[u8], protected: &str) -> Result<Vec<String>, CpaRuntimeError> {
    let text = std::str::from_utf8(config)
        .map_err(|_| CpaRuntimeError::Failed("managed CPA config is not valid UTF-8".into()))?;
    let keys = parse_api_keys_from_yaml(text)?;
    if !keys.iter().any(|key| key == protected) {
        return Err(CpaRuntimeError::Failed(
            "managed CPA config does not contain the protected OCG key".into(),
        ));
    }
    Ok(keys.into_iter().filter(|key| key != protected).collect())
}

fn generate_secret() -> Result<String, CpaRuntimeError> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes)
        .map_err(|error| CpaRuntimeError::Failed(format!("failed to generate CPA key: {error}")))?;
    Ok(format!("cpa-{:x}", Sha256::digest(bytes)))
}

fn validate_fingerprint(value: &str) -> Result<(), CpaRuntimeError> {
    if value.len() == 64 && value.chars().all(|ch| ch.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err(CpaRuntimeError::Invalid(
            "CPA client key fingerprint is invalid".into(),
        ))
    }
}

fn validate_managed_secret(value: &str) -> Result<(), CpaRuntimeError> {
    if value.is_empty()
        || value.len() > 4096
        || value
            .chars()
            .any(|ch| ch == '\0' || ch == '\r' || ch == '\n')
    {
        Err(CpaRuntimeError::Invalid(
            "managed CPA secret has an invalid format".into(),
        ))
    } else {
        Ok(())
    }
}

fn redact_text(value: &str, secrets: &[&str]) -> String {
    secrets
        .iter()
        .filter(|secret| !secret.is_empty())
        .fold(value.to_string(), |text, secret| {
            text.replace(secret, "[REDACTED]")
        })
}

fn redact_runtime_error(error: CpaRuntimeError, secrets: &[&str]) -> CpaRuntimeError {
    match error {
        CpaRuntimeError::Unavailable(message) => {
            CpaRuntimeError::Unavailable(redact_text(&message, secrets))
        }
        CpaRuntimeError::Invalid(message) => {
            CpaRuntimeError::Invalid(redact_text(&message, secrets))
        }
        CpaRuntimeError::Conflict(message) => {
            CpaRuntimeError::Conflict(redact_text(&message, secrets))
        }
        CpaRuntimeError::Unreachable(message) => {
            CpaRuntimeError::Unreachable(redact_text(&message, secrets))
        }
        CpaRuntimeError::Failed(message) => CpaRuntimeError::Failed(redact_text(&message, secrets)),
    }
}

fn with_compensation_error(
    original: CpaRuntimeError,
    compensation: Result<(), CpaRuntimeError>,
) -> CpaRuntimeError {
    match compensation {
        Ok(()) => original,
        Err(compensation) => CpaRuntimeError::Failed(format!(
            "{original}; restoring the previous CPA runtime also failed: {compensation}"
        )),
    }
}

fn fs_error(error: std::io::Error) -> CpaRuntimeError {
    CpaRuntimeError::Failed(format!("CPA runtime file error: {error}"))
}

fn parent_path(path: &Path) -> Result<&Path, CpaRuntimeError> {
    path.parent()
        .ok_or_else(|| CpaRuntimeError::Invalid("CPA runtime path has no parent".into()))
}

fn reject_reparse_ancestors(path: &Path) -> Result<(), CpaRuntimeError> {
    let mut current = Some(path);
    while let Some(item) = current {
        if is_reparse_path(item) {
            return Err(CpaRuntimeError::Invalid(format!(
                "CPA runtime path must not cross a reparse point: {}",
                item.display()
            )));
        }
        current = item.parent();
    }
    Ok(())
}

fn reject_reparse_tree(path: &Path) -> Result<(), CpaRuntimeError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(fs_error(error)),
    };
    if is_reparse_path(path) {
        return Err(CpaRuntimeError::Invalid(format!(
            "CPA runtime path must not be a reparse point: {}",
            path.display()
        )));
    }
    if metadata.is_dir() {
        for entry in fs::read_dir(path).map_err(fs_error)? {
            reject_reparse_tree(&entry.map_err(fs_error)?.path())?;
        }
    }
    Ok(())
}

#[cfg(windows)]
fn replace_file(source: &Path, destination: &Path) -> Result<(), CpaRuntimeError> {
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
        Err(fs_error(std::io::Error::last_os_error()))
    } else {
        Ok(())
    }
}

#[cfg(not(windows))]
fn replace_file(source: &Path, destination: &Path) -> Result<(), CpaRuntimeError> {
    fs::rename(source, destination).map_err(fs_error)
}

#[cfg(test)]
mod tests;

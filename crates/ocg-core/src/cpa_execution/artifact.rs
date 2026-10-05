//! Pinned CPA host selection. Stock CLIProxyAPI health is not an artifact.
//!
//! Product trust is the compile-time embed of `runtime/cpa/artifact-lock.json`.
//! A directory or `OCG_CPA_HOST_DIR` only locates bytes.

use super::ExecutionError;
use super::io::{self, copy_verified};
use std::fs;
use std::path::{Component, Path, PathBuf};

/// Historical placeholder. Not an accepted digest and not a fallback.
/// Unedited root `write_managed` and device launch still name this constant.
pub(super) const PINNED_SHA256: &str =
    "baff0e76f37b32f8e16618b6533f9a435dc36b4bc737302745b4b9353ed45542";
pub(super) const PINNED_COMMIT: &str = "6fecc6e5567912661654a4eaf9b8f5436facd1c2";
pub(super) const PINNED_VERSION: &str = "v8.0.10";
pub(super) const PINNED_VERSION_CANONICAL: &str = "8.0.10";
const PINNED_PROTOCOL: u64 = 1;
const FIXTURE_BUILD_TAG: &str = "ocg_native_loopback_fixture";

pub(super) const REQUIRED_CAPABILITIES: [&str; 16] = [
    "attempt-boundary",
    "selective-no-replay",
    "explicit-429-failover",
    "policy-ipc-v1",
    "readiness-v1",
    "compat-protocol-routes",
    "go-zen-identity",
    "ollama-reasoning",
    "oauth-redacted-references",
    "routing-preserve",
    "identity-fence-epoch-version-material",
    "validated-protocol-pin-v1",
    "validation-only-routes-v1",
    "absolute-request-deadline-v1",
    "native-refresh-registration-fence-v1",
    "native-final-endpoint-pin-v1",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Variant {
    Production,
    NativeLoopbackFixture,
}

impl Variant {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "production" => Some(Self::Production),
            "native-loopback-fixture" => Some(Self::NativeLoopbackFixture),
            _ => None,
        }
    }
}

/// Already-normalized compile platform. `os` is `windows`, `linux`, or `macos`.
/// `arch` is `x86_64` or `aarch64`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Platform {
    pub os: &'static str,
    pub arch: &'static str,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Artifact {
    pub dir: PathBuf,
    pub executable: PathBuf,
    pub sha256: String,
    pub source_commit: String,
    pub source_version: String,
    pub protocol_version: u64,
    pub capabilities: Vec<String>,
}

#[derive(Debug)]
struct SelectedRecord {
    sha256: String,
    source_commit: String,
    source_version: String,
    protocol_version: u64,
    capabilities: Vec<String>,
    executable: String,
}

struct LockRecord {
    os: &'static str,
    arch: &'static str,
    variant: Variant,
    executable: String,
    sha256: String,
}

pub(super) fn selected_variant() -> Variant {
    if cfg!(feature = "ollama-cloud-loopback-test") {
        Variant::NativeLoopbackFixture
    } else {
        Variant::Production
    }
}

pub(super) fn current_platform() -> Result<Platform, ExecutionError> {
    let os = if cfg!(windows) {
        "windows"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else {
        return Err(invalid("pinned CPA artifact lock platform is unsupported"));
    };
    let arch = if cfg!(target_arch = "x86_64") {
        "x86_64"
    } else if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else {
        return Err(invalid("pinned CPA artifact lock platform is unsupported"));
    };
    Ok(Platform { os, arch })
}

pub(super) fn resolve_dir(explicit: Option<&Path>) -> Result<PathBuf, ExecutionError> {
    if let Some(dir) = explicit {
        return Ok(dir.to_path_buf());
    }
    if let Some(dir) = std::env::var_os("OCG_CPA_HOST_DIR") {
        if !dir.is_empty() {
            return Ok(PathBuf::from(dir));
        }
    }
    let exe =
        std::env::current_exe().map_err(|_| invalid("the current executable is unavailable"))?;
    let mut cursor = exe.parent();
    for _ in 0..8 {
        let Some(dir) = cursor else {
            break;
        };
        if regular_manifest(dir) {
            return Ok(dir.to_path_buf());
        }
        cursor = dir.parent();
    }
    Err(invalid("pinned CPA host directory was not provided"))
}

pub(super) fn verify(dir: &Path) -> Result<Artifact, ExecutionError> {
    verify_directory_with_lock(
        embedded_root_lock()?.as_bytes(),
        dir,
        current_platform()?,
        selected_variant(),
    )
}

fn verify_directory_with_lock(
    lock_bytes: &[u8],
    dir: &Path,
    platform: Platform,
    variant: Variant,
) -> Result<Artifact, ExecutionError> {
    let manifest = io::read_manifest_bytes(&dir.join("manifest.json"))?;
    let selected = select_record(lock_bytes, &manifest, platform, variant)?;
    let executable = executable_in_dir(dir, &selected.executable)?;
    let actual = io::hash_regular_executable(&executable)?;
    if actual != selected.sha256 {
        return Err(invalid(
            "pinned CPA executable hash does not match the selected record",
        ));
    }
    Ok(Artifact {
        dir: dir.to_path_buf(),
        executable,
        sha256: selected.sha256,
        source_commit: selected.source_commit,
        source_version: selected.source_version,
        protocol_version: selected.protocol_version,
        capabilities: selected.capabilities,
    })
}

/// Selected digest for this compile platform and variant.
/// Same embed and parser as `verify`. No manifest, directory, or caller digest.
pub(super) fn selected_trusted_sha() -> Result<String, ExecutionError> {
    trusted_sha_in_lock(
        embedded_root_lock()?.as_bytes(),
        current_platform()?,
        selected_variant(),
    )
}

pub(super) fn install(data_dir: &Path, artifact: &Artifact) -> Result<PathBuf, ExecutionError> {
    install_with_trust(
        data_dir,
        artifact,
        embedded_root_lock()?.as_bytes(),
        current_platform()?,
        selected_variant(),
    )
}

fn install_with_trust(
    data_dir: &Path,
    artifact: &Artifact,
    lock_bytes: &[u8],
    platform: Platform,
    variant: Variant,
) -> Result<PathBuf, ExecutionError> {
    let trusted = trusted_sha_in_lock(lock_bytes, platform, variant)?;
    if artifact.sha256 == PINNED_SHA256 {
        return Err(invalid("pinned CPA placeholder digest is not authority"));
    }
    if artifact.sha256 != trusted {
        return Err(invalid(
            "pinned CPA executable hash does not match the selected record",
        ));
    }
    let destination_dir = io::version_dir(data_dir, &trusted);
    io::ensure_dir(&destination_dir)?;
    let destination = destination_dir.join(io::executable_name());
    copy_verified(&artifact.executable, &destination, &trusted)?;
    io::atomic_write_private(
        &destination_dir.join(".asset-sha256"),
        format!("{trusted}\n").as_bytes(),
    )?;
    installed_with_trust(data_dir, &trusted, lock_bytes, platform, variant)
}

pub(super) fn installed_executable(data_dir: &Path, sha: &str) -> Option<PathBuf> {
    let text = embedded_root_lock().ok()?;
    let platform = current_platform().ok()?;
    installed_with_trust(data_dir, sha, text.as_bytes(), platform, selected_variant()).ok()
}

fn installed_with_trust(
    data_dir: &Path,
    sha: &str,
    lock_bytes: &[u8],
    platform: Platform,
    variant: Variant,
) -> Result<PathBuf, ExecutionError> {
    let trusted = trusted_sha_in_lock(lock_bytes, platform, variant)?;
    if sha == PINNED_SHA256 {
        return Err(invalid("pinned CPA placeholder digest is not authority"));
    }
    if sha != trusted {
        return Err(invalid(
            "pinned CPA executable hash does not match the selected record",
        ));
    }
    let directory = io::version_dir(data_dir, &trusted);
    if !marker_matches(&directory.join(".asset-sha256"), &trusted) {
        return Err(invalid(
            "pinned CPA executable hash does not match the selected record",
        ));
    }
    let path = directory.join(io::executable_name());
    io::confirm_installed_executable(&path, &trusted)?;
    Ok(path)
}

pub(super) fn version_accepted(expected: Option<&str>) -> Result<(), ExecutionError> {
    match expected.map(str::trim).filter(|value| !value.is_empty()) {
        None => Ok(()),
        Some(PINNED_VERSION | PINNED_VERSION_CANONICAL) => Ok(()),
        Some(_) => Err(invalid("pinned CPA install only accepts v8.0.10")),
    }
}

/// The only product read of trust material. The path is the repository root
/// lock via this crate's manifest directory. A missing file fails compilation.
fn embedded_root_lock() -> Result<&'static str, ExecutionError> {
    const TEXT: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../runtime/cpa/artifact-lock.json"
    ));
    if TEXT.is_empty() || TEXT.trim().is_empty() {
        return Err(invalid("pinned CPA artifact lock is empty"));
    }
    if TEXT.len() > io::MAX_CONFIG_BYTES {
        return Err(invalid("pinned CPA artifact lock is too large"));
    }
    Ok(TEXT)
}

fn select_record(
    lock_bytes: &[u8],
    manifest_bytes: &[u8],
    platform: Platform,
    variant: Variant,
) -> Result<SelectedRecord, ExecutionError> {
    let lock = parse_lock(lock_bytes)?;
    let selected = selected_lock_record(&lock, platform, variant)?;
    let manifest = parse_manifest(manifest_bytes)?;
    if manifest_text(&manifest, "sourceCommit")? != PINNED_COMMIT
        || manifest_text(&manifest, "sourceVersion")? != PINNED_VERSION
        || manifest_u64(&manifest, "protocolVersion")? != PINNED_PROTOCOL
        || manifest_text(&manifest, "buildIdentity")? != lock.build_identity
        || manifest_text(&manifest, "hostSHA256")? != lock.host_sha256
        || manifest_text(&manifest, "overlaySHA256")? != lock.overlay_sha256
    {
        return Err(invalid(
            "pinned CPA manifest does not match the required artifact",
        ));
    }
    if manifest
        .get("variant")
        .and_then(serde_json::Value::as_str)
        .and_then(Variant::parse)
        != Some(variant)
    {
        return Err(invalid(
            "pinned CPA manifest variant does not match the selected record",
        ));
    }
    if !build_tags_match(&manifest, variant) {
        return Err(invalid(
            "pinned CPA manifest build tags do not match the selected record",
        ));
    }
    let os = manifest
        .get("os")
        .and_then(serde_json::Value::as_str)
        .and_then(normalize_os);
    let arch = manifest
        .get("arch")
        .and_then(serde_json::Value::as_str)
        .and_then(normalize_arch);
    if os != Some(platform.os) || arch != Some(platform.arch) {
        return Err(invalid(
            "pinned CPA manifest os/arch does not match the selected record",
        ));
    }
    let executable = manifest
        .get("executable")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| invalid("pinned CPA manifest is incomplete"))?;
    if executable != selected.executable {
        return Err(invalid(
            "pinned CPA manifest does not match the required artifact",
        ));
    }
    let capabilities = manifest_capabilities(&manifest)?;
    if let Some(claimed) = manifest.get("executableSHA256") {
        let claimed = claimed
            .as_str()
            .ok_or_else(|| invalid("pinned CPA manifest does not match the required artifact"))?;
        if claimed != selected.sha256 {
            return Err(invalid(
                "pinned CPA manifest does not match the required artifact",
            ));
        }
    }
    Ok(SelectedRecord {
        sha256: selected.sha256,
        source_commit: PINNED_COMMIT.to_string(),
        source_version: PINNED_VERSION.to_string(),
        protocol_version: PINNED_PROTOCOL,
        capabilities,
        executable: selected.executable,
    })
}

struct ParsedLock {
    build_identity: String,
    host_sha256: String,
    overlay_sha256: String,
    records: Vec<LockRecord>,
}

struct ChosenLockRecord {
    sha256: String,
    executable: String,
}

fn trusted_sha_in_lock(
    lock_bytes: &[u8],
    platform: Platform,
    variant: Variant,
) -> Result<String, ExecutionError> {
    let lock = parse_lock(lock_bytes)?;
    Ok(selected_lock_record(&lock, platform, variant)?.sha256)
}

fn selected_lock_record(
    lock: &ParsedLock,
    platform: Platform,
    variant: Variant,
) -> Result<ChosenLockRecord, ExecutionError> {
    let record = lock
        .records
        .iter()
        .find(|record| {
            record.os == platform.os && record.arch == platform.arch && record.variant == variant
        })
        .ok_or_else(|| invalid("pinned CPA artifact lock record is missing"))?;
    Ok(ChosenLockRecord {
        sha256: record.sha256.clone(),
        executable: record.executable.clone(),
    })
}

fn parse_lock(bytes: &[u8]) -> Result<ParsedLock, ExecutionError> {
    if bytes.is_empty() || bytes.iter().all(u8::is_ascii_whitespace) {
        return Err(invalid("pinned CPA artifact lock is empty"));
    }
    if bytes.len() > io::MAX_CONFIG_BYTES {
        return Err(invalid("pinned CPA artifact lock is too large"));
    }
    let text =
        std::str::from_utf8(bytes).map_err(|_| invalid("pinned CPA artifact lock is invalid"))?;
    let value: serde_json::Value =
        serde_json::from_str(text).map_err(|_| invalid("pinned CPA artifact lock is invalid"))?;
    if !value.is_object() {
        return Err(invalid("pinned CPA artifact lock is invalid"));
    }
    if value
        .get("schemaVersion")
        .and_then(serde_json::Value::as_u64)
        != Some(1)
    {
        return Err(invalid("pinned CPA artifact lock schema is not 1"));
    }
    if value
        .get("sourceCommit")
        .and_then(serde_json::Value::as_str)
        != Some(PINNED_COMMIT)
        || value
            .get("sourceVersion")
            .and_then(serde_json::Value::as_str)
            != Some(PINNED_VERSION)
        || value
            .get("protocolVersion")
            .and_then(serde_json::Value::as_u64)
            != Some(PINNED_PROTOCOL)
    {
        return Err(invalid(
            "pinned CPA artifact lock does not match the required source",
        ));
    }
    let build_identity = lowercase_hex_field(&value, "buildIdentity")?;
    let host_sha256 = lowercase_hex_field(&value, "hostSHA256")?;
    let overlay_sha256 = lowercase_hex_field(&value, "overlaySHA256")?;
    let capabilities = match value.get("requiredCapabilities") {
        Some(serde_json::Value::Array(items)) => capability_names(items)?,
        Some(_) => return Err(invalid("pinned CPA artifact lock is invalid")),
        None => {
            return Err(invalid(
                "pinned CPA artifact lock capability set is incomplete",
            ));
        }
    };
    if !covers_required(&capabilities) {
        return Err(invalid(
            "pinned CPA artifact lock capability set is incomplete",
        ));
    }
    let artifacts = match value.get("artifacts") {
        Some(serde_json::Value::Array(items)) if items.len() <= 32 => items,
        _ => return Err(invalid("pinned CPA artifact lock is invalid")),
    };
    let mut records = Vec::with_capacity(artifacts.len());
    let mut seen = Vec::<(&str, &str, Variant)>::new();
    for item in artifacts {
        let record = parse_lock_record(item)?;
        let key = (record.os, record.arch, record.variant);
        if seen.contains(&key) {
            return Err(invalid("pinned CPA artifact lock record is duplicated"));
        }
        seen.push(key);
        records.push(record);
    }
    Ok(ParsedLock {
        build_identity,
        host_sha256,
        overlay_sha256,
        records,
    })
}

fn parse_lock_record(value: &serde_json::Value) -> Result<LockRecord, ExecutionError> {
    let object = value
        .as_object()
        .ok_or_else(|| invalid("pinned CPA artifact lock record is invalid"))?;
    let os = object
        .get("os")
        .and_then(serde_json::Value::as_str)
        .and_then(normalize_os)
        .ok_or_else(|| invalid("pinned CPA artifact lock platform is unsupported"))?;
    let arch = object
        .get("arch")
        .and_then(serde_json::Value::as_str)
        .and_then(normalize_arch)
        .ok_or_else(|| invalid("pinned CPA artifact lock platform is unsupported"))?;
    let variant = object
        .get("variant")
        .and_then(serde_json::Value::as_str)
        .and_then(Variant::parse)
        .ok_or_else(|| invalid("pinned CPA artifact lock record is invalid"))?;
    let executable = object
        .get("executable")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| invalid("pinned CPA artifact lock record is invalid"))?;
    if !io::safe_executable_name(executable) {
        return Err(invalid("pinned CPA executable name is unsafe"));
    }
    let sha256 = object
        .get("sha256")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| invalid("pinned CPA artifact lock record is invalid"))?;
    if sha256 == PINNED_SHA256 {
        return Err(invalid("pinned CPA placeholder digest is not authority"));
    }
    if !super::lowercase_hex_64(sha256) {
        return Err(invalid("pinned CPA artifact lock record is invalid"));
    }
    match object
        .get("verification")
        .and_then(serde_json::Value::as_str)
    {
        Some("host-suite" | "compiled-only") => {}
        _ => return Err(invalid("pinned CPA artifact lock record is invalid")),
    }
    Ok(LockRecord {
        os,
        arch,
        variant,
        executable: executable.to_string(),
        sha256: sha256.to_string(),
    })
}

fn parse_manifest(bytes: &[u8]) -> Result<serde_json::Value, ExecutionError> {
    if bytes.len() > io::MAX_CONFIG_BYTES {
        return Err(invalid("pinned CPA manifest is too large"));
    }
    let text = std::str::from_utf8(bytes).map_err(|_| invalid("pinned CPA manifest is invalid"))?;
    let value: serde_json::Value =
        serde_json::from_str(text).map_err(|_| invalid("pinned CPA manifest is invalid"))?;
    if !value.is_object() {
        return Err(invalid("pinned CPA manifest is invalid"));
    }
    Ok(value)
}

fn manifest_text<'a>(
    manifest: &'a serde_json::Value,
    key: &str,
) -> Result<&'a str, ExecutionError> {
    manifest
        .get(key)
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| invalid("pinned CPA manifest is incomplete"))
}

fn manifest_u64(manifest: &serde_json::Value, key: &str) -> Result<u64, ExecutionError> {
    manifest
        .get(key)
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| invalid("pinned CPA manifest is incomplete"))
}

fn manifest_capabilities(manifest: &serde_json::Value) -> Result<Vec<String>, ExecutionError> {
    let items = match manifest.get("capabilities") {
        Some(serde_json::Value::Array(items)) => items,
        Some(_) => return Err(invalid("pinned CPA manifest is invalid")),
        None => {
            return Err(invalid(
                "pinned CPA manifest is missing a required capability",
            ));
        }
    };
    let names = capability_names(items).map_err(|_| invalid("pinned CPA manifest is invalid"))?;
    if !covers_required(&names) {
        return Err(invalid(
            "pinned CPA manifest is missing a required capability",
        ));
    }
    Ok(names)
}

fn build_tags_match(manifest: &serde_json::Value, variant: Variant) -> bool {
    let Some(tags) = manifest
        .get("buildTags")
        .and_then(serde_json::Value::as_array)
    else {
        return false;
    };
    let expected: &[&str] = match variant {
        Variant::Production => &[],
        Variant::NativeLoopbackFixture => &[FIXTURE_BUILD_TAG],
    };
    tags.len() == expected.len()
        && tags.len() <= 8
        && tags
            .iter()
            .zip(expected)
            .all(|(tag, expected)| tag.as_str() == Some(*expected))
}

fn lowercase_hex_field(value: &serde_json::Value, key: &str) -> Result<String, ExecutionError> {
    let text = value
        .get(key)
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| invalid("pinned CPA artifact lock does not match the required source"))?;
    if !super::lowercase_hex_64(text) {
        return Err(invalid(
            "pinned CPA artifact lock does not match the required source",
        ));
    }
    Ok(text.to_string())
}

fn capability_names(values: &[serde_json::Value]) -> Result<Vec<String>, ExecutionError> {
    if values.len() > 64 {
        return Err(invalid("pinned CPA artifact lock is invalid"));
    }
    let mut names = Vec::with_capacity(values.len());
    for value in values {
        let name = value
            .as_str()
            .ok_or_else(|| invalid("pinned CPA artifact lock is invalid"))?;
        if name.is_empty()
            || name.len() > 128
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        {
            return Err(invalid("pinned CPA artifact lock is invalid"));
        }
        names.push(name.to_string());
    }
    Ok(names)
}

fn covers_required(names: &[String]) -> bool {
    REQUIRED_CAPABILITIES
        .iter()
        .all(|required| names.iter().any(|item| item == required))
}

fn normalize_os(value: &str) -> Option<&'static str> {
    match value.to_ascii_lowercase().as_str() {
        "windows" | "win32" => Some("windows"),
        "linux" => Some("linux"),
        "darwin" | "macos" => Some("macos"),
        _ => None,
    }
}

fn normalize_arch(value: &str) -> Option<&'static str> {
    match value.to_ascii_lowercase().as_str() {
        "x86_64" | "amd64" | "x64" => Some("x86_64"),
        "aarch64" | "arm64" => Some("aarch64"),
        _ => None,
    }
}

fn regular_manifest(dir: &Path) -> bool {
    let path = dir.join("manifest.json");
    match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata.is_file() && !io::is_reparse_metadata(&metadata),
        Err(_) => false,
    }
}

fn executable_in_dir(dir: &Path, name: &str) -> Result<PathBuf, ExecutionError> {
    if !io::safe_executable_name(name) {
        return Err(invalid("pinned CPA executable name is unsafe"));
    }
    if dir
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(invalid("pinned CPA executable path is unsafe"));
    }
    let path = dir.join(name);
    if path.file_name().and_then(|item| item.to_str()) != Some(name)
        || path.as_os_str().len() > 4096
    {
        return Err(invalid("pinned CPA executable path is unsafe"));
    }
    Ok(path)
}

fn marker_matches(path: &Path, sha: &str) -> bool {
    let Ok(Some(bytes)) = io::read_bounded(path) else {
        return false;
    };
    bytes.as_slice() == sha.as_bytes() || bytes.as_slice() == format!("{sha}\n").as_bytes()
}

fn invalid(message: &str) -> ExecutionError {
    ExecutionError::Invalid(message.to_string())
}

#[cfg(test)]
pub(super) fn trusted_sha_for_lock(
    lock_bytes: &[u8],
    platform: Platform,
    variant: Variant,
) -> Result<String, ExecutionError> {
    trusted_sha_in_lock(lock_bytes, platform, variant)
}

#[cfg(test)]
pub(super) fn install_for_lock(
    data_dir: &Path,
    artifact: &Artifact,
    lock_bytes: &[u8],
    platform: Platform,
    variant: Variant,
) -> Result<PathBuf, ExecutionError> {
    install_with_trust(data_dir, artifact, lock_bytes, platform, variant)
}

#[cfg(test)]
pub(super) fn installed_for_lock(
    data_dir: &Path,
    sha: &str,
    lock_bytes: &[u8],
    platform: Platform,
    variant: Variant,
) -> Result<PathBuf, ExecutionError> {
    installed_with_trust(data_dir, sha, lock_bytes, platform, variant)
}

#[cfg(test)]
const SYNTHETIC_PRODUCTION: &[u8] = b"synthetic-production-host";
#[cfg(test)]
const SYNTHETIC_FIXTURE: &[u8] = b"synthetic-fixture-host";
#[cfg(test)]
const SYNTHETIC_OTHER: &[u8] = b"synthetic-other-platform-host";

#[cfg(test)]
pub(super) struct SyntheticSelection {
    pub lock: Vec<u8>,
    pub platform: Platform,
    pub variant: Variant,
    pub selected_sha: String,
    pub alternate_sha: String,
    pub other_platform_sha: String,
    pub selected_bytes: &'static [u8],
    pub alternate_bytes: &'static [u8],
    pub other_bytes: &'static [u8],
}

#[cfg(test)]
pub(super) fn synthetic_selection() -> Result<SyntheticSelection, ExecutionError> {
    synthetic_selection_for(current_platform()?, selected_variant())
}

#[cfg(test)]
fn synthetic_selection_for(
    platform: Platform,
    variant: Variant,
) -> Result<SyntheticSelection, ExecutionError> {
    let other = if platform.os == "linux" && platform.arch == "x86_64" {
        Platform {
            os: "windows",
            arch: "x86_64",
        }
    } else {
        Platform {
            os: "linux",
            arch: "x86_64",
        }
    };
    let (selected_bytes, alternate_bytes, alternate) = match variant {
        Variant::Production => (
            SYNTHETIC_PRODUCTION,
            SYNTHETIC_FIXTURE,
            Variant::NativeLoopbackFixture,
        ),
        Variant::NativeLoopbackFixture => {
            (SYNTHETIC_FIXTURE, SYNTHETIC_PRODUCTION, Variant::Production)
        }
    };
    let selected_sha = synthetic_digest(selected_bytes);
    let alternate_sha = synthetic_digest(alternate_bytes);
    let other_platform_sha = synthetic_digest(SYNTHETIC_OTHER);
    let lock = serde_json::json!({
        "schemaVersion": 1,
        "sourceCommit": PINNED_COMMIT,
        "sourceVersion": PINNED_VERSION,
        "protocolVersion": PINNED_PROTOCOL,
        "buildIdentity": "11".repeat(32),
        "hostSHA256": "22".repeat(32),
        "overlaySHA256": "33".repeat(32),
        "requiredCapabilities": REQUIRED_CAPABILITIES,
        "artifacts": [
            synthetic_lock_record(platform, variant, &selected_sha),
            synthetic_lock_record(other, Variant::Production, &other_platform_sha),
            synthetic_lock_record(platform, alternate, &alternate_sha)
        ]
    });
    Ok(SyntheticSelection {
        lock: serde_json::to_vec(&lock)
            .map_err(|_| invalid("pinned CPA artifact lock is invalid"))?,
        platform,
        variant,
        selected_sha,
        alternate_sha,
        other_platform_sha,
        selected_bytes,
        alternate_bytes,
        other_bytes: SYNTHETIC_OTHER,
    })
}

#[cfg(test)]
fn synthetic_digest(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(bytes))
}

#[cfg(test)]
fn synthetic_lock_record(platform: Platform, variant: Variant, sha: &str) -> serde_json::Value {
    let variant = match variant {
        Variant::Production => "production",
        Variant::NativeLoopbackFixture => "native-loopback-fixture",
    };
    let executable = match (platform.os, platform.arch) {
        ("windows", "x86_64") => "ocg-cpa-host.exe",
        ("linux", "x86_64") => "ocg-cpa-host-linux-amd64",
        ("macos", "aarch64") => "ocg-cpa-host-darwin-arm64",
        ("linux", "aarch64") => "ocg-cpa-host-linux-arm64",
        ("macos", "x86_64") => "ocg-cpa-host-darwin-amd64",
        ("windows", "aarch64") => "ocg-cpa-host-windows-arm64.exe",
        _ => "ocg-cpa-host",
    };
    serde_json::json!({
        "os": platform.os,
        "arch": platform.arch,
        "variant": variant,
        "executable": executable,
        "sha256": sha,
        "verification": "host-suite"
    })
}

#[cfg(test)]
fn verify_material(
    lock_bytes: &[u8],
    manifest_bytes: &[u8],
    executable_bytes: &[u8],
    platform: Platform,
    variant: Variant,
) -> Result<SelectedRecord, ExecutionError> {
    use sha2::{Digest, Sha256};
    if executable_bytes.len() as u64 > io::MAX_EXECUTABLE_BYTES {
        return Err(invalid("pinned CPA executable is too large"));
    }
    let selected = select_record(lock_bytes, manifest_bytes, platform, variant)?;
    let actual = hex::encode(Sha256::digest(executable_bytes));
    if actual != selected.sha256 {
        return Err(invalid(
            "pinned CPA executable hash does not match the selected record",
        ));
    }
    Ok(selected)
}

#[cfg(test)]
mod tests;

//! Readiness is the private ready document, never a stock `/health` body.

use super::ExecutionError;
use super::artifact::{self, PINNED_COMMIT, REQUIRED_CAPABILITIES};
use super::native::{self, DiscoverySnapshot};
use super::store::{OAuthStamp, Record};
use serde_json::Value;
use std::time::Duration;

const READY_TIMEOUT: Duration = Duration::from_secs(25);

#[derive(Clone, Debug)]
pub(super) struct AcceptedReady {
    pub oauth: Vec<OAuthStamp>,
    /// Required capability names verified by this acceptance. Extra body names are not stored.
    pub capabilities: Vec<String>,
}

pub(super) fn accept_ready(
    body: &str,
    record: &Record,
    secrets: &super::Secrets,
) -> Result<AcceptedReady, ExecutionError> {
    let trusted = artifact::selected_trusted_sha()?;
    accept_ready_checked(body, record, secrets, &trusted)
}

fn accept_ready_checked(
    body: &str,
    record: &Record,
    secrets: &super::Secrets,
    trusted: &str,
) -> Result<AcceptedReady, ExecutionError> {
    if body.contains(secrets.hop.expose())
        || body.contains(secrets.policy.expose())
        || body.contains(secrets.ready.expose())
    {
        return Err(ExecutionError::ApplyFailed(
            "cpa_apply_failed: ready document contains a secret".into(),
        ));
    }
    let value: Value = serde_json::from_str(body).map_err(|_| {
        ExecutionError::ApplyFailed("cpa_apply_failed: ready document is invalid".into())
    })?;
    if value.get("status").and_then(Value::as_str) == Some("ok") && value.get("artifact").is_none()
    {
        return Err(ExecutionError::ApplyFailed(
            "cpa_apply_failed: stock health is not readiness".into(),
        ));
    }
    if value.get("protocolVersion").and_then(Value::as_u64) != Some(1) {
        return Err(ExecutionError::ApplyFailed(
            "cpa_apply_failed: ready protocol is not 1".into(),
        ));
    }
    let artifact = value.get("artifact").ok_or_else(|| {
        ExecutionError::ApplyFailed("cpa_apply_failed: ready artifact is missing".into())
    })?;
    let sha = artifact
        .get("executableSHA256")
        .and_then(Value::as_str)
        .unwrap_or("");
    let commit = artifact
        .get("sourceCommit")
        .and_then(Value::as_str)
        .unwrap_or("");
    let protocol = artifact.get("protocolVersion").and_then(Value::as_u64);
    if sha != trusted
        || sha == artifact::PINNED_SHA256
        || !super::lowercase_hex_64(sha)
        || sha != record.artifact_sha256
        || commit != PINNED_COMMIT
        || protocol != Some(1)
    {
        return Err(ExecutionError::ApplyFailed(
            "cpa_apply_failed: ready artifact does not match the selected digest".into(),
        ));
    }
    if let Some(version) = artifact.get("sourceVersion").and_then(Value::as_str) {
        if version != artifact::PINNED_VERSION {
            return Err(ExecutionError::ApplyFailed(
                "cpa_apply_failed: ready source version does not match the pin".into(),
            ));
        }
    }
    let capabilities = artifact
        .get("capabilities")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            ExecutionError::ApplyFailed("cpa_apply_failed: ready capabilities are missing".into())
        })?;
    let names = capabilities
        .iter()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>();
    for required in REQUIRED_CAPABILITIES {
        if !names.contains(&required) {
            return Err(ExecutionError::ApplyFailed(
                "cpa_apply_failed: ready capability set is incomplete".into(),
            ));
        }
    }
    let generation = decimal_field(value.get("processGeneration"))?;
    let applied_revision = decimal_field(value.get("appliedProjectionRevision"))?;
    let desired_revision = decimal_field(value.get("desiredProjectionRevision"))?;
    let applied_digest = digest_field(value.get("appliedProjectionDigest"))?;
    let desired_digest = digest_field(value.get("desiredProjectionDigest"))?;
    if generation != record.child_generation
        || applied_revision != record.desired_revision
        || desired_revision != record.desired_revision
        || applied_digest != record.desired_digest
        || desired_digest != record.desired_digest
    {
        return Err(ExecutionError::ApplyFailed(
            "cpa_apply_failed: ready generation does not match the desired projection".into(),
        ));
    }
    if value.get("policyReady").and_then(Value::as_bool) != Some(true) {
        return Err(ExecutionError::ApplyFailed(
            "cpa_apply_failed: ready policy is not ready".into(),
        ));
    }
    if value.get("applyStatus").and_then(Value::as_str) != Some("applied") {
        return Err(ExecutionError::ApplyFailed(
            "cpa_apply_failed: ready apply status is not applied".into(),
        ));
    }
    Ok(AcceptedReady {
        oauth: oauth_refs(&value)?,
        capabilities: verified_capabilities(),
    })
}

fn verified_capabilities() -> Vec<String> {
    REQUIRED_CAPABILITIES
        .iter()
        .map(|name| (*name).to_string())
        .collect()
}

pub(super) fn accept_restored(
    body: &str,
    record: &Record,
    secrets: &super::Secrets,
) -> Result<AcceptedReady, ExecutionError> {
    let restored = restored_record(record)?;
    let accepted = accept_ready(body, &restored, secrets)?;
    reject_failed_projection(&restored, record)?;
    Ok(accepted)
}

fn restored_record(record: &Record) -> Result<Record, ExecutionError> {
    if record.applied_revision == 0
        || record.applied_generation == 0
        || !super::lowercase_hex_64(&record.applied_digest)
    {
        return Err(ExecutionError::ApplyFailed(
            "cpa_apply_failed: restored projection is not applied".into(),
        ));
    }
    let mut restored = record.clone();
    restored.child_generation = record.applied_generation;
    restored.desired_revision = record.applied_revision;
    restored.desired_digest = record.applied_digest.clone();
    Ok(restored)
}

fn reject_failed_projection(restored: &Record, record: &Record) -> Result<(), ExecutionError> {
    if restored.desired_revision == record.desired_revision
        && record.desired_revision != record.applied_revision
    {
        return Err(ExecutionError::ApplyFailed(
            "cpa_apply_failed: restored ready still names the failed projection".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
pub(super) fn accept_ready_for_lock(
    body: &str,
    record: &Record,
    secrets: &super::Secrets,
    lock_bytes: &[u8],
    platform: artifact::Platform,
    variant: artifact::Variant,
) -> Result<AcceptedReady, ExecutionError> {
    let trusted = artifact::trusted_sha_for_lock(lock_bytes, platform, variant)?;
    accept_ready_checked(body, record, secrets, &trusted)
}

#[cfg(test)]
pub(super) fn accept_restored_for_lock(
    body: &str,
    record: &Record,
    secrets: &super::Secrets,
    lock_bytes: &[u8],
    platform: artifact::Platform,
    variant: artifact::Variant,
) -> Result<AcceptedReady, ExecutionError> {
    let restored = restored_record(record)?;
    let accepted = accept_ready_for_lock(body, &restored, secrets, lock_bytes, platform, variant)?;
    reject_failed_projection(&restored, record)?;
    Ok(accepted)
}

pub(super) fn restored_projection_matches(body: &str, record: &Record) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(body) else {
        return false;
    };
    let generation = decimal_field(value.get("processGeneration")).ok();
    let applied_revision = decimal_field(value.get("appliedProjectionRevision")).ok();
    let desired_revision = decimal_field(value.get("desiredProjectionRevision")).ok();
    let applied_digest = digest_field(value.get("appliedProjectionDigest")).ok();
    let desired_digest = digest_field(value.get("desiredProjectionDigest")).ok();
    generation == Some(record.applied_generation)
        && applied_revision == Some(record.applied_revision)
        && desired_revision == Some(record.applied_revision)
        && applied_digest.as_deref() == Some(record.applied_digest.as_str())
        && desired_digest.as_deref() == Some(record.applied_digest.as_str())
        && record.applied_revision != record.desired_revision
}

pub(super) fn synthetic_ready(record: &Record) -> String {
    serde_json::json!({
        "protocolVersion": 1,
        "artifact": {
            "sourceCommit": PINNED_COMMIT,
            "sourceVersion": artifact::PINNED_VERSION,
            "protocolVersion": 1,
            "executableSHA256": record.artifact_sha256,
            "capabilities": REQUIRED_CAPABILITIES
        },
        "processGeneration": record.child_generation.to_string(),
        "appliedProjectionRevision": record.desired_revision.to_string(),
        "appliedProjectionDigest": record.desired_digest,
        "desiredProjectionRevision": record.desired_revision.to_string(),
        "desiredProjectionDigest": record.desired_digest,
        "policyReady": true,
        "applyStatus": "applied",
        "authRefs": []
    })
    .to_string()
}

pub(super) async fn poll_ready(
    port: u16,
    ready_token: &str,
    origin: &str,
) -> Result<String, ExecutionError> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .map_err(|_| ExecutionError::Unavailable("ready client could not be built".into()))?;
    let url = format!("http://127.0.0.1:{port}/_internal/ocg/ready");
    let deadline = std::time::Instant::now() + READY_TIMEOUT;
    let mut last = ExecutionError::ApplyFailed("cpa_apply_failed: ready timed out".into());
    while std::time::Instant::now() < deadline {
        match client
            .get(&url)
            .header("X-OCG-Ready-Token", ready_token)
            .header("Origin", origin)
            .send()
            .await
        {
            Ok(response) if response.status().is_success() => {
                let body = response.text().await.map_err(|_| {
                    ExecutionError::ApplyFailed(
                        "cpa_apply_failed: ready body could not be read".into(),
                    )
                })?;
                return Ok(body);
            }
            Ok(_) => {
                last = ExecutionError::ApplyFailed(
                    "cpa_apply_failed: ready request was rejected".into(),
                );
            }
            Err(_) => {
                last = ExecutionError::ApplyFailed(
                    "cpa_apply_failed: ready endpoint is not listening".into(),
                );
            }
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    Err(last)
}

fn oauth_refs(value: &Value) -> Result<Vec<OAuthStamp>, ExecutionError> {
    match native::discovery_from_ready_value(value) {
        DiscoverySnapshot::Incomplete => Ok(Vec::new()),
        DiscoverySnapshot::Malformed => Err(ExecutionError::ApplyFailed(
            "cpa_apply_failed: ready auth refs are invalid".into(),
        )),
        DiscoverySnapshot::Complete(refs) => {
            Ok(refs.iter().map(native::stamp_from_discovered).collect())
        }
    }
}

/// Copy the host registration epoch onto in-memory API-key stamps.
///
/// The SDK increments `RegistrationEpoch` when it registers an auth. File-backed
/// native refs stay on the OAuth merge and do not change these stamps. An absent
/// or empty `relativePath` is the compat auth synthesized from the projection.
/// Malformed, duplicate, or missing in-memory evidence invalidates a retained
/// nonzero epoch. Epoch 0 stays the unset value and is not a wildcard.
pub(super) fn note_keyed_registration_epochs(
    stamps: &mut [super::store::AuthStamp],
    body: &str,
) -> Result<(), ExecutionError> {
    let Ok(value) = serde_json::from_str::<Value>(body) else {
        return invalidate_retained_epochs(stamps, "keyed registration evidence is missing");
    };
    let Some(items) = value.get("authRefs").and_then(Value::as_array) else {
        return invalidate_retained_epochs(stamps, "keyed registration evidence is missing");
    };
    let mut invalid_evidence = false;
    for stamp in stamps.iter_mut() {
        let mut matched = None;
        let mut invalid = false;
        for item in items {
            let Some(object) = item.as_object() else {
                continue;
            };
            match keyed_relative_path(object.get("relativePath")) {
                KeyedPath::Native => continue,
                KeyedPath::Invalid => {
                    if keyed_identity_matches(object, stamp) {
                        invalid = true;
                    }
                    continue;
                }
                KeyedPath::InMemory => {}
            }
            if !keyed_identity_matches(object, stamp) {
                continue;
            }
            match optional_decimal(object.get("registrationEpoch")) {
                Some(epoch) if matched.is_none() => matched = Some(epoch),
                _ => invalid = true,
            }
        }
        if invalid {
            if stamp.registration_epoch != 0 {
                stamp.registration_epoch = 0;
            }
            invalid_evidence = true;
            continue;
        }
        if let Some(epoch) = matched {
            stamp.registration_epoch = epoch;
            continue;
        }
        if stamp.registration_epoch != 0 {
            stamp.registration_epoch = 0;
            invalid_evidence = true;
        }
    }
    if invalid_evidence {
        return Err(ExecutionError::ApplyFailed(
            "cpa_apply_failed: keyed registration evidence is invalid".into(),
        ));
    }
    Ok(())
}

/// A retained nonzero epoch is authority. Missing evidence must not keep it.
fn invalidate_retained_epochs(
    stamps: &mut [super::store::AuthStamp],
    reason: &str,
) -> Result<(), ExecutionError> {
    let mut retained = false;
    for stamp in stamps {
        if stamp.registration_epoch != 0 {
            stamp.registration_epoch = 0;
            retained = true;
        }
    }
    if retained {
        Err(ExecutionError::ApplyFailed(format!(
            "cpa_apply_failed: {reason}"
        )))
    } else {
        Ok(())
    }
}

enum KeyedPath {
    InMemory,
    Native,
    Invalid,
}

/// Go omits an empty `relativePath`. A present non-string is not that omission.
fn keyed_relative_path(value: Option<&Value>) -> KeyedPath {
    match value {
        None => KeyedPath::InMemory,
        Some(Value::String(text)) if text.trim().is_empty() => KeyedPath::InMemory,
        Some(Value::String(_)) => KeyedPath::Native,
        Some(_) => KeyedPath::Invalid,
    }
}

fn keyed_identity_matches(
    object: &serde_json::Map<String, Value>,
    stamp: &super::store::AuthStamp,
) -> bool {
    let auth_id = object.get("authId").and_then(Value::as_str).unwrap_or("");
    let credential_id = object
        .get("credentialId")
        .and_then(Value::as_str)
        .unwrap_or("");
    let version = optional_decimal(object.get("credentialVersion"));
    let material = object
        .get("materialRevision")
        .and_then(Value::as_str)
        .unwrap_or("");
    let provider = object
        .get("providerId")
        .and_then(Value::as_str)
        .unwrap_or("");
    auth_id == stamp.auth_id
        && credential_id == stamp.credential_id
        && version == Some(stamp.credential_version)
        && material == stamp.material_revision
        && (provider.is_empty() || provider == stamp.provider_id)
}

/// Restoration publishes the retained applied plane. The failed desired plane
/// does not receive the restored host's counters.
pub(super) fn note_applied_keyed_epochs(
    record: &mut super::store::Record,
    body: &str,
) -> Result<(), ExecutionError> {
    note_keyed_registration_epochs(&mut record.applied_auth, body)
}

/// Rollback adopts counters only on the applied plane when that plane is the
/// restored projection. Any other plane, including desired, is left unchanged.
pub(super) fn note_restored_keyed_epochs(
    record: &mut super::store::Record,
    body: &str,
    revision: u64,
    digest: &str,
) -> Result<(), ExecutionError> {
    if record.applied_revision == revision && record.applied_digest == digest {
        note_keyed_registration_epochs(&mut record.applied_auth, body)
    } else {
        Err(ExecutionError::RollbackUnavailable)
    }
}

fn optional_decimal(value: Option<&Value>) -> Option<u64> {
    match value? {
        Value::String(text) => parse_epoch(text),
        Value::Number(number) => number.as_u64(),
        _ => None,
    }
}

fn parse_epoch(text: &str) -> Option<u64> {
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    if text.len() > 1 && text.starts_with('0') {
        return None;
    }
    text.parse().ok()
}

pub(super) async fn fetch_ready_once(
    port: u16,
    ready_token: &str,
    origin: &str,
) -> Result<String, ExecutionError> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .map_err(|_| ExecutionError::Unavailable("ready client could not be built".into()))?;
    let url = format!("http://127.0.0.1:{port}/_internal/ocg/ready");
    let response = client
        .get(&url)
        .header("X-OCG-Ready-Token", ready_token)
        .header("Origin", origin)
        .send()
        .await
        .map_err(|_| {
            ExecutionError::ApplyFailed("cpa_apply_failed: ready endpoint is not listening".into())
        })?;
    if !response.status().is_success() {
        return Err(ExecutionError::ApplyFailed(
            "cpa_apply_failed: ready request was rejected".into(),
        ));
    }
    response.text().await.map_err(|_| {
        ExecutionError::ApplyFailed("cpa_apply_failed: ready body could not be read".into())
    })
}

fn decimal_field(value: Option<&Value>) -> Result<u64, ExecutionError> {
    let text = match value {
        Some(Value::String(text)) => text.as_str(),
        Some(Value::Number(number)) => {
            return number.as_u64().ok_or_else(|| {
                ExecutionError::ApplyFailed("cpa_apply_failed: ready number is invalid".into())
            });
        }
        _ => {
            return Err(ExecutionError::ApplyFailed(
                "cpa_apply_failed: ready field is missing".into(),
            ));
        }
    };
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(ExecutionError::ApplyFailed(
            "cpa_apply_failed: ready field is not a decimal".into(),
        ));
    }
    if text.len() > 1 && text.starts_with('0') {
        return Err(ExecutionError::ApplyFailed(
            "cpa_apply_failed: ready field is not a decimal".into(),
        ));
    }
    text.parse().map_err(|_| {
        ExecutionError::ApplyFailed("cpa_apply_failed: ready field is not a decimal".into())
    })
}

fn digest_field(value: Option<&Value>) -> Result<String, ExecutionError> {
    let text = value.and_then(Value::as_str).unwrap_or("");
    if text.len() != 64 || !text.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(ExecutionError::ApplyFailed(
            "cpa_apply_failed: ready digest is invalid".into(),
        ));
    }
    Ok(text.to_ascii_lowercase())
}

#[cfg(test)]
mod tests;

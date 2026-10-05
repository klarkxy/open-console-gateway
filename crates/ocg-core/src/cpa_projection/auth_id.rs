//! Stable auth id. Rotation, rebind, and version each change the digest input.

use super::types::fingerprint;
use sha2::{Digest, Sha256};

pub(crate) fn auth_id(
    credential_id: &str,
    credential_version: u64,
    binding_id: &str,
    material_fingerprint: Option<&str>,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"ocg-cpa-auth-v1");
    hasher.update([0]);
    hasher.update(credential_id.as_bytes());
    hasher.update([0]);
    hasher.update(credential_version.to_string().as_bytes());
    hasher.update([0]);
    hasher.update(binding_id.as_bytes());
    hasher.update([0]);
    match material_fingerprint {
        Some(value) => hasher.update(value.as_bytes()),
        None => hasher.update(b"no-material"),
    }
    hex::encode(hasher.finalize())
}

pub(crate) fn material_fingerprint(secret: &str) -> String {
    let mut prefixed = Vec::with_capacity(secret.len() + 16);
    prefixed.extend_from_slice(b"ocg-material-v1\0");
    prefixed.extend_from_slice(secret.as_bytes());
    let digest = fingerprint(&prefixed);
    prefixed.fill(0);
    digest
}

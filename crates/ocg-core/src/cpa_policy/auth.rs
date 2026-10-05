//! Constant-time comparison for the private policy token.
//!
//! The token is not a client credential and must not be forwarded upstream.

use sha2::{Digest, Sha256};

use super::wire::MAX_TOKEN_BYTES;

/// Compare the configured token with the presented token.
///
/// Both sides are hashed to a fixed digest before the byte walk, so a mismatch
/// does not return on the first differing byte. Empty tokens fail closed.
pub fn authorize_token(expected: &[u8], provided: &[u8]) -> bool {
    let presented = if provided.len() > MAX_TOKEN_BYTES {
        &provided[..MAX_TOKEN_BYTES]
    } else {
        provided
    };
    let mut diff = 0u8;
    let left = fingerprint(expected);
    let right = fingerprint(presented);
    for i in 0..left.len() {
        diff |= left[i] ^ right[i];
    }
    diff == 0 && !expected.is_empty() && !provided.is_empty() && provided.len() <= MAX_TOKEN_BYTES
}

fn fingerprint(token: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(b"ocg-cpa-policy-token-v1");
    hasher.update((token.len() as u64).to_le_bytes());
    hasher.update(token);
    hasher.finalize().into()
}

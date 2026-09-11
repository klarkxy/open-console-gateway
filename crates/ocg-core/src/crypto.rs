//! Local Key storage facade (`ocg-infra::crypto`).
//!
//! New writes are AES-256-GCM (`v2:`). Legacy XOR still decrypts so backups
//! restore. This is a local-disk bound, not a KMS.
//!
//! Two cipher implementations are provided:
//! - `MachineBoundCipher`: derives a key from Windows environment variables
//!   (USERNAME, COMPUTERNAME, APPDATA) for backward compatibility with the
//!   original GUI app.
//! - `StaticKeyCipher`: derives a key from an arbitrary user-supplied secret,
//!   suitable for headless / cross-platform / Docker deployments.

#[doc(inline)]
pub use ocg_infra::crypto::{
    KeyCipher, LOCAL_CIPHER_V2_PREFIX, MachineBoundCipher, StaticKeyCipher,
    is_legacy_local_ciphertext, load_or_create_static_cipher,
};

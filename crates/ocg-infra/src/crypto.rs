//! Local storage for API keys and saved login passwords.
//!
//! New writes use authenticated AES-256-GCM (`v2:` + base64(nonce || ciphertext || tag)).
//! The 32-byte AEAD key is SHA-256-derived from the Host MachineBound / StaticKey seed;
//! the legacy FNV mixer is not used for v2. Decrypt accepts `v2:` only via AEAD
//! (authentication failure is an explicit error, never a UTF-8 guess) and still
//! reads unprefixed XOR of the old 16-byte-nonce shape so directory backups restore.
//! Callers that persist should rewrite v2 after a successful legacy decrypt.
//!
//! This is still a local-disk bound, not a KMS: anyone with the matching Host seed
//! can recover stored values. Errors never include plaintext Keys.
//!
//! Two cipher implementations are provided:
//! - `MachineBoundCipher`: derives a key from Windows environment variables
//!   (USERNAME, COMPUTERNAME, APPDATA) for backward compatibility with the
//!   original GUI app.
//! - `StaticKeyCipher`: derives a key from an arbitrary user-supplied secret,
//!   suitable for headless / cross-platform / Docker deployments.

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use anyhow::Context;
use base64::{Engine, engine::general_purpose::STANDARD};
use sha2::{Digest, Sha256};
use std::env;
use std::fs::{self, OpenOptions};
use std::io::{ErrorKind, Write};
use std::path::Path;

const XOR_NONCE_LEN: usize = 16;
const AEAD_NONCE_LEN: usize = 12;
const AEAD_TAG_LEN: usize = 16;
const AEAD_KEY_LEN: usize = 32;
const AEAD_KDF_DOMAIN: &[u8] = b"ocg-local-key-cipher-v2";

/// Prefix for AES-256-GCM local ciphertext (`v2:` + standard base64).
pub const LOCAL_CIPHER_V2_PREFIX: &str = "v2:";

/// Trait for pluggable key obfuscation.
pub trait KeyCipher: Send + Sync {
    fn encrypt(&self, plaintext: &str) -> anyhow::Result<String>;
    fn decrypt(&self, ciphertext: &str) -> anyhow::Result<String>;
}

/// Errors from local Key encrypt/decrypt. Display and Debug never include
/// plaintext, ciphertext, or host-seed material.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CryptoError {
    InvalidCiphertext,
    AuthenticationFailed,
    LegacyDecryptFailed,
    Internal,
}

impl std::fmt::Display for CryptoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidCiphertext => write!(f, "invalid local key ciphertext"),
            Self::AuthenticationFailed => write!(f, "local key authentication failed"),
            Self::LegacyDecryptFailed => write!(f, "legacy local key ciphertext rejected"),
            Self::Internal => write!(f, "local key cipher failed"),
        }
    }
}

impl std::error::Error for CryptoError {}

/// Returns true when `ciphertext` is a non-empty pre-v2 XOR blob (no `v2:` prefix).
pub fn is_legacy_local_ciphertext(ciphertext: &str) -> bool {
    !ciphertext.is_empty() && !ciphertext.starts_with(LOCAL_CIPHER_V2_PREFIX)
}

fn derive_xor_key(seed: &[u8], len: usize) -> Vec<u8> {
    // Legacy FNV-like mixer. Used only to read pre-v2 XOR rows.
    let mut key = Vec::with_capacity(len);
    let mut state: u64 = 0xcbf29ce484222325;
    let mut idx = 0;
    while key.len() < len {
        let b = seed.get(idx % seed.len().max(1)).copied().unwrap_or(0);
        state ^= b as u64;
        state = state.wrapping_mul(0x100000001b3);
        key.push((state >> 24) as u8);
        key.push((state >> 16) as u8);
        key.push((state >> 8) as u8);
        key.push(state as u8);
        idx += 1;
    }
    key.truncate(len);
    key
}

fn derive_aead_key(seed: &[u8]) -> [u8; AEAD_KEY_LEN] {
    let mut hasher = Sha256::new();
    hasher.update(AEAD_KDF_DOMAIN);
    hasher.update(seed);
    hasher.finalize().into()
}

fn xor_encrypt(plaintext: &str, key_seed: &[u8]) -> Result<String, CryptoError> {
    if plaintext.is_empty() {
        return Ok(String::new());
    }
    let bytes = plaintext.as_bytes();
    let nonce: Vec<u8> = uuid::Uuid::new_v4().as_bytes().to_vec();
    let key = derive_xor_key(key_seed, bytes.len());
    let mut cipher = Vec::with_capacity(XOR_NONCE_LEN + bytes.len());
    cipher.extend_from_slice(&nonce);
    for (i, b) in bytes.iter().enumerate() {
        cipher.push(b ^ key[i] ^ nonce[i % XOR_NONCE_LEN]);
    }
    Ok(STANDARD.encode(&cipher))
}

fn xor_decrypt(ciphertext: &str, key_seed: &[u8]) -> Result<String, CryptoError> {
    if ciphertext.is_empty() {
        return Ok(String::new());
    }
    let cipher = STANDARD
        .decode(ciphertext)
        .map_err(|_| CryptoError::InvalidCiphertext)?;
    if cipher.len() < XOR_NONCE_LEN {
        return Err(CryptoError::InvalidCiphertext);
    }
    let (nonce, body) = cipher.split_at(XOR_NONCE_LEN);
    let key = derive_xor_key(key_seed, body.len());
    let mut plain = Vec::with_capacity(body.len());
    for (i, b) in body.iter().enumerate() {
        plain.push(b ^ key[i] ^ nonce[i % XOR_NONCE_LEN]);
    }
    String::from_utf8(plain).map_err(|_| CryptoError::LegacyDecryptFailed)
}

fn aead_encrypt(plaintext: &str, key_seed: &[u8]) -> Result<String, CryptoError> {
    if plaintext.is_empty() {
        return Ok(String::new());
    }
    let key = derive_aead_key(key_seed);
    let cipher = Aes256Gcm::new_from_slice(&key).map_err(|_| CryptoError::Internal)?;
    let uuid = uuid::Uuid::new_v4();
    let mut nonce_bytes = [0_u8; AEAD_NONCE_LEN];
    nonce_bytes.copy_from_slice(&uuid.as_bytes()[..AEAD_NONCE_LEN]);
    let nonce = Nonce::from_slice(&nonce_bytes);
    let sealed = cipher
        .encrypt(nonce, plaintext.as_bytes())
        .map_err(|_| CryptoError::Internal)?;
    let mut packed = Vec::with_capacity(AEAD_NONCE_LEN + sealed.len());
    packed.extend_from_slice(&nonce_bytes);
    packed.extend_from_slice(&sealed);
    Ok(format!(
        "{LOCAL_CIPHER_V2_PREFIX}{}",
        STANDARD.encode(packed)
    ))
}

fn aead_decrypt(encoded: &str, key_seed: &[u8]) -> Result<String, CryptoError> {
    let packed = STANDARD
        .decode(encoded)
        .map_err(|_| CryptoError::InvalidCiphertext)?;
    if packed.len() < AEAD_NONCE_LEN + AEAD_TAG_LEN {
        return Err(CryptoError::InvalidCiphertext);
    }
    let (nonce_bytes, sealed) = packed.split_at(AEAD_NONCE_LEN);
    let key = derive_aead_key(key_seed);
    let cipher = Aes256Gcm::new_from_slice(&key).map_err(|_| CryptoError::Internal)?;
    let nonce = Nonce::from_slice(nonce_bytes);
    let plain = cipher
        .decrypt(nonce, sealed)
        .map_err(|_| CryptoError::AuthenticationFailed)?;
    String::from_utf8(plain).map_err(|_| CryptoError::InvalidCiphertext)
}

fn encrypt_local(plaintext: &str, key_seed: &[u8]) -> Result<String, CryptoError> {
    aead_encrypt(plaintext, key_seed)
}

fn decrypt_local(ciphertext: &str, key_seed: &[u8]) -> Result<String, CryptoError> {
    if ciphertext.is_empty() {
        return Ok(String::new());
    }
    if let Some(encoded) = ciphertext.strip_prefix(LOCAL_CIPHER_V2_PREFIX) {
        return aead_decrypt(encoded, key_seed);
    }
    xor_decrypt(ciphertext, key_seed)
}

/// Original Windows machine-bound cipher.
#[derive(Debug, Clone, Default)]
pub struct MachineBoundCipher;

impl MachineBoundCipher {
    pub fn new() -> Self {
        Self
    }

    fn seed(&self) -> Vec<u8> {
        let mut parts = Vec::new();
        if let Ok(user) = env::var("USERNAME") {
            parts.push(user);
        }
        if let Ok(computer) = env::var("COMPUTERNAME") {
            parts.push(computer);
        }
        if let Ok(appdata) = env::var("APPDATA") {
            parts.push(appdata);
        }
        parts.join("|").into_bytes()
    }
}

impl KeyCipher for MachineBoundCipher {
    fn encrypt(&self, plaintext: &str) -> anyhow::Result<String> {
        Ok(encrypt_local(plaintext, &self.seed())?)
    }

    fn decrypt(&self, ciphertext: &str) -> anyhow::Result<String> {
        Ok(decrypt_local(ciphertext, &self.seed())?)
    }
}

/// Cross-platform cipher based on a user-provided secret.
#[derive(Debug, Clone)]
pub struct StaticKeyCipher {
    seed: Vec<u8>,
}

impl StaticKeyCipher {
    pub fn new(secret: &str) -> Self {
        Self {
            seed: secret.as_bytes().to_vec(),
        }
    }

    /// Write the pre-v2 XOR local format. Used to plant backup-shaped rows in tests.
    pub fn encrypt_legacy(&self, plaintext: &str) -> anyhow::Result<String> {
        Ok(xor_encrypt(plaintext, &self.seed)?)
    }
}

/// Loads the static encryption key from `.encryption-key`, creating it when absent.
pub fn load_or_create_static_cipher(data_dir: &Path) -> anyhow::Result<StaticKeyCipher> {
    fs::create_dir_all(data_dir)
        .with_context(|| format!("failed to create data directory {data_dir:?}"))?;
    let key_path = data_dir.join(".encryption-key");
    let secret = match fs::read_to_string(&key_path) {
        Ok(secret) => secret,
        Err(error) if error.kind() == ErrorKind::NotFound => {
            let generated = uuid::Uuid::new_v4().simple().to_string();
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            match options.open(&key_path) {
                Ok(mut file) => {
                    file.write_all(generated.as_bytes()).with_context(|| {
                        format!("failed to write encryption key to {key_path:?}")
                    })?;
                    generated
                }
                Err(error) if error.kind() == ErrorKind::AlreadyExists => {
                    fs::read_to_string(&key_path).with_context(|| {
                        format!("failed to read encryption key from {key_path:?}")
                    })?
                }
                Err(error) => {
                    return Err(error).with_context(|| {
                        format!("failed to create encryption key at {key_path:?}")
                    });
                }
            }
        }
        Err(error) => {
            return Err(error)
                .with_context(|| format!("failed to read encryption key from {key_path:?}"));
        }
    };

    if secret.is_empty() {
        anyhow::bail!("encryption key at {key_path:?} is empty");
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&key_path, fs::Permissions::from_mode(0o600))
            .with_context(|| format!("failed to secure encryption key at {key_path:?}"))?;
    }

    Ok(StaticKeyCipher::new(&secret))
}

impl KeyCipher for StaticKeyCipher {
    fn encrypt(&self, plaintext: &str) -> anyhow::Result<String> {
        Ok(encrypt_local(plaintext, &self.seed)?)
    }

    fn decrypt(&self, ciphertext: &str) -> anyhow::Result<String> {
        Ok(decrypt_local(ciphertext, &self.seed)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_dir(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("ocg-crypto-{name}-{}", uuid::Uuid::new_v4()))
    }

    fn assert_error_hides_secret(error: &anyhow::Error, secret: &str) {
        let display = format!("{error}");
        let debug = format!("{error:?}");
        let alt = format!("{error:#}");
        assert!(
            !display.contains(secret) && !debug.contains(secret) && !alt.contains(secret),
            "error must not include plaintext: display={display} debug={debug}"
        );
    }

    #[test]
    fn machine_bound_roundtrip() {
        let original = "sk-ocg-test-key-12345";
        let cipher = MachineBoundCipher::new();
        let encrypted = cipher.encrypt(original).unwrap();
        assert_ne!(encrypted, original);
        assert!(encrypted.starts_with(LOCAL_CIPHER_V2_PREFIX));
        let decrypted = cipher.decrypt(&encrypted).unwrap();
        assert_eq!(decrypted, original);
    }

    #[test]
    fn static_key_roundtrip() {
        let original = "sk-ocg-test-key-12345";
        let cipher = StaticKeyCipher::new("my-secret-key");
        let encrypted = cipher.encrypt(original).unwrap();
        assert_ne!(encrypted, original);
        assert!(encrypted.starts_with(LOCAL_CIPHER_V2_PREFIX));
        let decrypted = cipher.decrypt(&encrypted).unwrap();
        assert_eq!(decrypted, original);
        assert!(!encrypted.contains(original));

        let secret = "sk-ocg-secret";
        let secret_enc = cipher.encrypt(secret).unwrap();
        assert!(!secret_enc.is_empty());
        assert_ne!(secret_enc, secret);
        assert!(!secret_enc.contains(secret));
        assert!(secret_enc.starts_with(LOCAL_CIPHER_V2_PREFIX));
    }

    // --- negative cases ---

    /// Same plaintext encrypted twice yields different ciphertext (nonce randomness).
    #[test]
    fn static_key_encrypt_is_nondeterministic() {
        let cipher = StaticKeyCipher::new("k");
        let a = cipher.encrypt("hello").unwrap();
        let b = cipher.encrypt("hello").unwrap();
        assert_ne!(a, b, "ciphertext must differ across calls (random nonce)");
    }

    /// Empty plaintext must round-trip to empty string — no panic, no base64 garbage.
    #[test]
    fn static_key_empty_string_roundtrip() {
        let cipher = StaticKeyCipher::new("k");
        let enc = cipher.encrypt("").unwrap();
        assert_eq!(enc, "");
        let dec = cipher.decrypt("").unwrap();
        assert_eq!(dec, "");
    }

    /// Different secrets cannot decrypt each other's v2 ciphertext.
    #[test]
    fn s05_wrong_key_fails_closed() {
        let secret = "payload";
        let enc = StaticKeyCipher::new("right-key").encrypt(secret).unwrap();
        let result = StaticKeyCipher::new("wrong-key").decrypt(&enc);
        let error = result.expect_err("wrong AEAD key must fail closed");
        assert_error_hides_secret(&error, secret);
        assert_error_hides_secret(&error, "right-key");
        assert_error_hides_secret(&error, "wrong-key");
    }

    #[test]
    fn s05_truncated_v2_fails() {
        let host = "truncated-host-secret";
        let cipher = StaticKeyCipher::new(host);
        let error = cipher
            .decrypt(&format!("{LOCAL_CIPHER_V2_PREFIX}AAAA"))
            .expect_err("truncated v2 must fail");
        assert_error_hides_secret(&error, host);
    }

    #[test]
    fn s05_corrupt_v2_fails() {
        let secret = "sk-corrupt-probe";
        let cipher = StaticKeyCipher::new("k");
        let enc = cipher.encrypt(secret).unwrap();
        let mut packed = STANDARD
            .decode(enc.strip_prefix(LOCAL_CIPHER_V2_PREFIX).unwrap())
            .unwrap();
        let last = packed.len() - 1;
        packed[last] ^= 0x5a;
        let corrupt = format!("{LOCAL_CIPHER_V2_PREFIX}{}", STANDARD.encode(packed));
        let error = cipher
            .decrypt(&corrupt)
            .expect_err("corrupt v2 must fail AEAD");
        assert_error_hides_secret(&error, secret);
        assert!(
            !format!("{error}").contains(&enc) && !format!("{error:?}").contains(&enc),
            "error must not echo ciphertext"
        );
    }

    #[test]
    fn v2_prefix_never_falls_back_to_xor_utf8() {
        let cipher = StaticKeyCipher::new("k");
        let legacy = xor_encrypt("hello", b"k").unwrap();
        let disguised = format!("{LOCAL_CIPHER_V2_PREFIX}{legacy}");
        let error = cipher
            .decrypt(&disguised)
            .expect_err("v2: must not succeed via XOR UTF-8");
        assert_error_hides_secret(&error, "hello");
    }

    #[test]
    fn s05_legacy_xor_written_by_xor_encrypt_still_decrypts() {
        let seed = b"legacy-seed";
        let original = "sk-legacy-xor-key";
        let enc = xor_encrypt(original, seed).unwrap();
        assert!(!enc.starts_with(LOCAL_CIPHER_V2_PREFIX));
        assert!(is_legacy_local_ciphertext(&enc));
        let cipher = StaticKeyCipher::new("legacy-seed");
        assert_eq!(cipher.decrypt(&enc).unwrap(), original);
        let planted = cipher.encrypt_legacy(original).unwrap();
        assert!(!planted.is_empty());
        assert!(is_legacy_local_ciphertext(&planted));
        assert_eq!(cipher.decrypt(&planted).unwrap(), original);
    }

    #[test]
    fn s05_reencrypt_roundtrip_is_v2() {
        let cipher = StaticKeyCipher::new("legacy-seed");
        let original = "sk-reencrypt";
        let legacy = cipher.encrypt_legacy(original).unwrap();
        let plain = cipher.decrypt(&legacy).unwrap();
        let rewritten = cipher.encrypt(&plain).unwrap();
        assert!(rewritten.starts_with(LOCAL_CIPHER_V2_PREFIX));
        assert!(!is_legacy_local_ciphertext(&rewritten));
        assert_eq!(cipher.decrypt(&rewritten).unwrap(), original);
    }

    /// Garbage base64 must error rather than panic.
    #[test]
    fn static_key_rejects_garbage_ciphertext() {
        let cipher = StaticKeyCipher::new("k");
        assert!(cipher.decrypt("!!!not-base64!!!").is_err());
    }

    /// Valid base64 but too short (< XOR nonce bytes) must error.
    #[test]
    fn static_key_rejects_short_ciphertext() {
        let cipher = StaticKeyCipher::new("k");
        // 4 bytes of valid base64 = "AAAA"
        assert!(cipher.decrypt("AAAA").is_err());
    }

    #[test]
    fn static_key_file_is_created_and_reused() {
        let dir = test_dir("reuse");
        let first = load_or_create_static_cipher(&dir).unwrap();
        let key_path = dir.join(".encryption-key");
        let original = fs::read_to_string(&key_path).unwrap();
        let second = load_or_create_static_cipher(&dir).unwrap();

        assert!(!original.is_empty());
        assert_eq!(fs::read_to_string(&key_path).unwrap(), original);
        assert_eq!(
            second.decrypt(&first.encrypt("payload").unwrap()).unwrap(),
            "payload"
        );

        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn static_key_file_contents_are_preserved_exactly() {
        let dir = test_dir("preserve");
        fs::create_dir_all(&dir).unwrap();
        let key_path = dir.join(".encryption-key");
        fs::write(&key_path, " existing-secret\n").unwrap();

        let loaded = load_or_create_static_cipher(&dir).unwrap();
        let expected = StaticKeyCipher::new(" existing-secret\n");

        assert_eq!(fs::read_to_string(&key_path).unwrap(), " existing-secret\n");
        assert_eq!(
            loaded
                .decrypt(&expected.encrypt("payload").unwrap())
                .unwrap(),
            "payload"
        );

        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn static_key_file_rejects_empty_secret() {
        let dir = test_dir("empty");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(".encryption-key"), "").unwrap();

        load_or_create_static_cipher(&dir).expect_err("empty secret must fail");
        fs::remove_dir_all(dir).unwrap();
    }
}

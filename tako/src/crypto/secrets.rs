use aes_gcm::{
    Aes256Gcm, Nonce,
    aead::{Aead, KeyInit},
};
use argon2::Argon2;
use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use std::fs;
use std::path::{Path, PathBuf};

use crate::config::{ConfigError, Result};

/// AES-256 key size in bytes
const KEY_SIZE: usize = 32;

/// AES-GCM nonce size in bytes
const NONCE_SIZE: usize = 12;

/// Argon2id salt size in bytes
const SALT_SIZE: usize = 16;

/// Encryption key for secrets
#[derive(Clone)]
pub struct EncryptionKey {
    key: [u8; KEY_SIZE],
}

impl EncryptionKey {
    /// Derive a key from a passphrase and salt using Argon2id
    pub fn derive(passphrase: &str, salt: &[u8]) -> Result<Self> {
        let mut key = [0u8; KEY_SIZE];
        // OWASP-recommended Argon2id parameters:
        // m=64MiB, t=3, p=4
        let params = argon2::Params::new(64 * 1024, 3, 4, Some(KEY_SIZE))
            .map_err(|e| ConfigError::Encryption(format!("Invalid Argon2id params: {}", e)))?;
        let argon2 = Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params);
        argon2
            .hash_password_into(passphrase.as_bytes(), salt, &mut key)
            .map_err(|e| ConfigError::Encryption(format!("Argon2id derivation failed: {}", e)))?;
        Ok(Self { key })
    }

    /// Create from raw bytes
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        if bytes.len() != KEY_SIZE {
            return Err(ConfigError::Encryption(format!(
                "Key must be {} bytes, got {}",
                KEY_SIZE,
                bytes.len()
            )));
        }
        let mut key = [0u8; KEY_SIZE];
        key.copy_from_slice(bytes);
        Ok(Self { key })
    }

    /// Create from base64-encoded string
    pub fn from_base64(encoded: &str) -> Result<Self> {
        let bytes = BASE64
            .decode(encoded)
            .map_err(|e| ConfigError::Encryption(format!("Invalid base64 key: {}", e)))?;
        Self::from_bytes(&bytes)
    }

    /// Export as base64-encoded string
    pub fn to_base64(&self) -> String {
        BASE64.encode(self.key)
    }

    /// Get the raw key bytes
    pub fn as_bytes(&self) -> &[u8; KEY_SIZE] {
        &self.key
    }
}

/// Generate a random salt for Argon2id
pub fn generate_salt() -> [u8; SALT_SIZE] {
    let mut salt = [0u8; SALT_SIZE];
    getrandom::fill(&mut salt).expect("operating system RNG unavailable");
    salt
}

/// Encode salt as base64
pub fn encode_salt(salt: &[u8]) -> String {
    BASE64.encode(salt)
}

/// Decode salt from base64
pub fn decode_salt(encoded: &str) -> Result<Vec<u8>> {
    BASE64
        .decode(encoded)
        .map_err(|e| ConfigError::Encryption(format!("Invalid base64 salt: {}", e)))
}

/// Encrypt a plaintext string using AES-256-GCM
///
/// Returns a base64-encoded string containing: nonce (12 bytes) + ciphertext
pub fn encrypt(plaintext: &str, key: &EncryptionKey) -> Result<String> {
    let cipher = Aes256Gcm::new_from_slice(&key.key)
        .map_err(|e| ConfigError::Encryption(format!("Failed to create cipher: {}", e)))?;

    // Generate random nonce
    let mut nonce_bytes = [0u8; NONCE_SIZE];
    getrandom::fill(&mut nonce_bytes)
        .map_err(|e| ConfigError::Encryption(format!("Failed to generate nonce: {}", e)))?;
    let nonce = Nonce::from_slice(&nonce_bytes);

    // Encrypt
    let ciphertext = cipher
        .encrypt(nonce, plaintext.as_bytes())
        .map_err(|e| ConfigError::Encryption(format!("Encryption failed: {}", e)))?;

    // Combine nonce + ciphertext
    let mut combined = Vec::with_capacity(NONCE_SIZE + ciphertext.len());
    combined.extend_from_slice(&nonce_bytes);
    combined.extend_from_slice(&ciphertext);

    Ok(BASE64.encode(combined))
}

/// Decrypt a base64-encoded ciphertext using AES-256-GCM
///
/// Expects format: base64(nonce (12 bytes) + ciphertext)
pub fn decrypt(encrypted: &str, key: &EncryptionKey) -> Result<String> {
    let combined = BASE64
        .decode(encrypted)
        .map_err(|e| ConfigError::Decryption(format!("Invalid base64: {}", e)))?;

    if combined.len() < NONCE_SIZE {
        return Err(ConfigError::Decryption("Ciphertext too short".to_string()));
    }

    let (nonce_bytes, ciphertext) = combined.split_at(NONCE_SIZE);
    let nonce = Nonce::from_slice(nonce_bytes);

    let cipher = Aes256Gcm::new_from_slice(&key.key)
        .map_err(|e| ConfigError::Decryption(format!("Failed to create cipher: {}", e)))?;

    let plaintext = cipher.decrypt(nonce, ciphertext).map_err(|_| {
        ConfigError::Decryption("Decryption failed (wrong key or corrupted data)".to_string())
    })?;

    String::from_utf8(plaintext)
        .map_err(|e| ConfigError::Decryption(format!("Invalid UTF-8: {}", e)))
}

/// Key storage manager
///
/// Caches derived encryption keys locally, keyed by a truncated hash of the
/// Argon2id salt. Since each app-environment gets a unique random salt (stored
/// in `secrets.json`), this gives stable, collision-free paths without needing
/// a separate project ID.
///
/// Path: `$TAKO_HOME/keys/{sha256(salt)[:16]}`
pub struct KeyStore {
    /// Path to the key file
    key_path: PathBuf,
}

impl KeyStore {
    /// Create a key store keyed by the environment's salt.
    ///
    /// The cache path is `$TAKO_HOME/keys/{hex(sha256(salt))[:16]}` — derived
    /// deterministically from the salt in `secrets.json`, so it's stable across
    /// app renames, `--name` overrides, and git worktrees.
    pub fn for_salt(salt_b64: &str) -> Result<Self> {
        use sha2::{Digest, Sha256};

        let data_dir = crate::paths::tako_data_dir().map_err(|e| {
            ConfigError::Validation(format!("Could not determine tako data directory: {}", e))
        })?;

        let hash = Sha256::digest(salt_b64.as_bytes());
        let dir_name = hex::encode(&hash[..8]); // 16 hex chars

        Ok(Self {
            key_path: data_dir.join("keys").join(dir_name),
        })
    }

    /// Create a key store with a custom path
    pub fn with_path(path: PathBuf) -> Self {
        Self { key_path: path }
    }

    /// Get key file path
    pub fn key_path(&self) -> &Path {
        &self.key_path
    }

    /// Load the encryption key from storage
    pub fn load_key(&self) -> Result<EncryptionKey> {
        let encoded = fs::read_to_string(&self.key_path)
            .map_err(|e| ConfigError::FileRead(self.key_path.clone(), e))?;
        EncryptionKey::from_base64(encoded.trim())
    }

    /// Save the encryption key to storage
    pub fn save_key(&self, key: &EncryptionKey) -> Result<()> {
        // Ensure parent directory exists
        if let Some(parent) = self.key_path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| ConfigError::FileWrite(parent.to_path_buf(), e))?;
        }

        // Write key with restrictive permissions.
        // On Unix, create with 0600 from the start to avoid a window where the
        // key is world-readable (TOCTOU between write and chmod).
        let encoded = key.to_base64();
        #[cfg(unix)]
        {
            use std::io::Write;
            use std::os::unix::fs::OpenOptionsExt;
            let mut f = fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&self.key_path)
                .map_err(|e| ConfigError::FileWrite(self.key_path.clone(), e))?;
            f.write_all(encoded.as_bytes())
                .map_err(|e| ConfigError::FileWrite(self.key_path.clone(), e))?;
        }
        #[cfg(not(unix))]
        {
            fs::write(&self.key_path, &encoded)
                .map_err(|e| ConfigError::FileWrite(self.key_path.clone(), e))?;
        }

        Ok(())
    }

    /// Check if a key exists
    pub fn key_exists(&self) -> bool {
        self.key_path.exists()
    }

    /// Delete the key
    pub fn delete_key(&self) -> Result<()> {
        if self.key_path.exists() {
            fs::remove_file(&self.key_path)
                .map_err(|e| ConfigError::FileWrite(self.key_path.clone(), e))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;
    use tempfile::TempDir;

    fn with_temp_tako_home<T>(f: impl FnOnce(&std::path::Path) -> T) -> T {
        let _lock = crate::paths::test_tako_home_env_lock();

        let temp = TempDir::new().unwrap();
        let previous = std::env::var_os("TAKO_HOME");
        unsafe {
            std::env::set_var("TAKO_HOME", temp.path());
        }

        struct ResetEnv(Option<OsString>);
        impl Drop for ResetEnv {
            fn drop(&mut self) {
                match self.0.take() {
                    Some(value) => unsafe { std::env::set_var("TAKO_HOME", value) },
                    None => unsafe { std::env::remove_var("TAKO_HOME") },
                }
            }
        }
        let _reset = ResetEnv(previous);

        f(temp.path())
    }

    // ==================== Key Derivation Tests ====================

    #[test]
    fn test_derive_produces_correct_length_key() {
        let salt = generate_salt();
        let key = EncryptionKey::derive("my-passphrase", &salt).unwrap();
        assert_eq!(key.as_bytes().len(), KEY_SIZE);
    }

    #[test]
    fn test_derive_is_deterministic() {
        let salt = generate_salt();
        let key1 = EncryptionKey::derive("my-passphrase", &salt).unwrap();
        let key2 = EncryptionKey::derive("my-passphrase", &salt).unwrap();
        assert_eq!(key1.as_bytes(), key2.as_bytes());
    }

    #[test]
    fn test_derive_different_passphrases_produce_different_keys() {
        let salt = generate_salt();
        let key1 = EncryptionKey::derive("passphrase-one", &salt).unwrap();
        let key2 = EncryptionKey::derive("passphrase-two", &salt).unwrap();
        assert_ne!(key1.as_bytes(), key2.as_bytes());
    }

    #[test]
    fn test_derive_different_salts_produce_different_keys() {
        let salt1 = generate_salt();
        let salt2 = generate_salt();
        let key1 = EncryptionKey::derive("same-passphrase", &salt1).unwrap();
        let key2 = EncryptionKey::derive("same-passphrase", &salt2).unwrap();
        assert_ne!(key1.as_bytes(), key2.as_bytes());
    }

    #[test]
    fn test_derive_and_encrypt_decrypt_round_trip() {
        let salt = generate_salt();
        let key = EncryptionKey::derive("test-passphrase", &salt).unwrap();
        let plaintext = "Hello, World!";

        let encrypted = encrypt(plaintext, &key).unwrap();
        let decrypted = decrypt(&encrypted, &key).unwrap();
        assert_eq!(decrypted, plaintext);
    }

    // ==================== Salt Tests ====================

    #[test]
    fn test_generate_salt_is_random() {
        let salt1 = generate_salt();
        let salt2 = generate_salt();
        assert_ne!(salt1, salt2);
    }

    #[test]
    fn test_salt_base64_round_trip() {
        let salt = generate_salt();
        let encoded = encode_salt(&salt);
        let decoded = decode_salt(&encoded).unwrap();
        assert_eq!(decoded, salt);
    }

    // ==================== Encryption Tests ====================

    #[test]
    fn test_key_from_bytes() {
        let bytes = [0u8; KEY_SIZE];
        let key = EncryptionKey::from_bytes(&bytes).unwrap();
        assert_eq!(key.as_bytes(), &bytes);
    }

    #[test]
    fn test_key_from_bytes_wrong_size() {
        let bytes = [0u8; 16];
        let result = EncryptionKey::from_bytes(&bytes);
        assert!(result.is_err());
    }

    #[test]
    fn test_key_base64_round_trip() {
        let salt = generate_salt();
        let key = EncryptionKey::derive("test", &salt).unwrap();
        let encoded = key.to_base64();
        let decoded = EncryptionKey::from_base64(&encoded).unwrap();
        assert_eq!(key.as_bytes(), decoded.as_bytes());
    }

    #[test]
    fn test_encrypt_decrypt_round_trip() {
        let salt = generate_salt();
        let key = EncryptionKey::derive("test", &salt).unwrap();
        let plaintext = "Hello, World!";

        let encrypted = encrypt(plaintext, &key).unwrap();
        let decrypted = decrypt(&encrypted, &key).unwrap();

        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn test_encrypt_produces_different_ciphertext() {
        let salt = generate_salt();
        let key = EncryptionKey::derive("test", &salt).unwrap();
        let plaintext = "Hello, World!";

        let encrypted1 = encrypt(plaintext, &key).unwrap();
        let encrypted2 = encrypt(plaintext, &key).unwrap();

        // Different ciphertexts due to random nonce
        assert_ne!(encrypted1, encrypted2);

        // But both decrypt to the same plaintext
        assert_eq!(decrypt(&encrypted1, &key).unwrap(), plaintext);
        assert_eq!(decrypt(&encrypted2, &key).unwrap(), plaintext);
    }

    #[test]
    fn test_decrypt_with_wrong_key_fails() {
        let salt = generate_salt();
        let key1 = EncryptionKey::derive("passphrase-1", &salt).unwrap();
        let key2 = EncryptionKey::derive("passphrase-2", &salt).unwrap();
        let plaintext = "Hello, World!";

        let encrypted = encrypt(plaintext, &key1).unwrap();
        let result = decrypt(&encrypted, &key2);

        assert!(result.is_err());
    }

    #[test]
    fn test_encrypt_unicode() {
        let salt = generate_salt();
        let key = EncryptionKey::derive("test", &salt).unwrap();
        let plaintext = "Hello, 世界! 🔐";

        let encrypted = encrypt(plaintext, &key).unwrap();
        let decrypted = decrypt(&encrypted, &key).unwrap();

        assert_eq!(decrypted, plaintext);
    }

    // ==================== KeyStore Tests ====================

    #[test]
    fn test_key_store_for_salt_uses_hash_dir() {
        with_temp_tako_home(|temp_home| {
            let salt_b64 = encode_salt(&generate_salt());
            let store = KeyStore::for_salt(&salt_b64).unwrap();

            // Path should be under keys/ with a hex hash directory name
            let key_path = store.key_path();
            assert!(key_path.starts_with(temp_home.join("keys")));
            let dir_name = key_path.file_name().unwrap().to_str().unwrap();
            assert_eq!(dir_name.len(), 16, "hash dir should be 16 hex chars");
            assert!(
                dir_name.chars().all(|c| c.is_ascii_hexdigit()),
                "dir name should be hex: {}",
                dir_name
            );
        });
    }

    #[test]
    fn test_key_store_for_salt_is_deterministic() {
        with_temp_tako_home(|_| {
            let salt_b64 = encode_salt(&generate_salt());
            let store1 = KeyStore::for_salt(&salt_b64).unwrap();
            let store2 = KeyStore::for_salt(&salt_b64).unwrap();
            assert_eq!(store1.key_path(), store2.key_path());
        });
    }

    #[test]
    fn test_key_store_different_salts_different_paths() {
        with_temp_tako_home(|_| {
            let salt_a = encode_salt(&generate_salt());
            let salt_b = encode_salt(&generate_salt());
            let store_a = KeyStore::for_salt(&salt_a).unwrap();
            let store_b = KeyStore::for_salt(&salt_b).unwrap();
            assert_ne!(store_a.key_path(), store_b.key_path());
        });
    }

    #[test]
    fn test_key_store_save_and_load() {
        with_temp_tako_home(|_| {
            let salt = generate_salt();
            let salt_b64 = encode_salt(&salt);
            let store = KeyStore::for_salt(&salt_b64).unwrap();
            let key = EncryptionKey::derive("test-passphrase", &salt).unwrap();

            store.save_key(&key).unwrap();
            assert!(store.key_exists());

            let loaded = store.load_key().unwrap();
            assert_eq!(key.as_bytes(), loaded.as_bytes());
        });
    }

    #[test]
    fn test_key_store_delete() {
        with_temp_tako_home(|_| {
            let salt = generate_salt();
            let salt_b64 = encode_salt(&salt);
            let store = KeyStore::for_salt(&salt_b64).unwrap();
            let key = EncryptionKey::derive("test", &salt).unwrap();

            store.save_key(&key).unwrap();
            assert!(store.key_exists());

            store.delete_key().unwrap();
            assert!(!store.key_exists());
        });
    }

    #[test]
    fn test_separate_salts_have_separate_keys() {
        with_temp_tako_home(|_| {
            let salt_a = generate_salt();
            let salt_b = generate_salt();
            let store_a = KeyStore::for_salt(&encode_salt(&salt_a)).unwrap();
            let store_b = KeyStore::for_salt(&encode_salt(&salt_b)).unwrap();

            let key_a = EncryptionKey::derive("passphrase-a", &salt_a).unwrap();
            let key_b = EncryptionKey::derive("passphrase-b", &salt_b).unwrap();

            store_a.save_key(&key_a).unwrap();
            store_b.save_key(&key_b).unwrap();

            let loaded_a = store_a.load_key().unwrap();
            let loaded_b = store_b.load_key().unwrap();
            assert_ne!(loaded_a.as_bytes(), loaded_b.as_bytes());
        });
    }

    /// One-off helper to generate real encrypted secrets.json for Go examples.
    /// Run with: cargo test -p tako -- generate_go_example_secrets --ignored --nocapture
    #[test]
    #[ignore]
    fn generate_go_example_secrets() {
        let passphrase = "tako-example";
        let repo_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap();
        let example_dirs = [
            repo_root.join("examples/go/basic"),
            repo_root.join("examples/go/gin"),
            repo_root.join("examples/go/echo"),
            repo_root.join("examples/go/chi"),
        ];
        let secrets_data = [
            ("API_KEY", "sk-example-key-12345"),
            ("DATABASE_URL", "postgres://localhost:5432/myapp"),
            ("EXAMPLE_SECRET", "hello-from-tako"),
        ];

        for dir in &example_dirs {
            let dir = dir.as_path();
            if !dir.exists() {
                eprintln!("skipping {} (not found)", dir.display());
                continue;
            }
            let tako_dir = dir.join(".tako");
            std::fs::create_dir_all(&tako_dir).unwrap();

            let salt = generate_salt();
            let salt_b64 = encode_salt(&salt);
            let key = EncryptionKey::derive(passphrase, &salt).unwrap();

            let mut secrets_map = serde_json::Map::new();
            for (name, value) in &secrets_data {
                let encrypted = encrypt(value, &key).unwrap();
                secrets_map.insert(name.to_string(), serde_json::Value::String(encrypted));
            }

            let json = serde_json::json!({
                "development": {
                    "salt": salt_b64,
                    "secrets": secrets_map
                }
            });

            let path = tako_dir.join("secrets.json");
            std::fs::write(&path, serde_json::to_string_pretty(&json).unwrap()).unwrap();
            eprintln!("wrote {}", path.display());
        }
    }
}

use aes_gcm::{
    Aes256Gcm, Nonce,
    aead::{Aead, KeyInit},
};
use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use std::fs;
use std::path::{Path, PathBuf};

use crate::config::{ConfigError, Result};

/// AES-256 key size in bytes
const KEY_SIZE: usize = 32;

/// AES-GCM nonce size in bytes
const NONCE_SIZE: usize = 12;

/// Encryption key for secrets
#[derive(Clone)]
pub struct EncryptionKey {
    key: [u8; KEY_SIZE],
}

impl EncryptionKey {
    /// Create a new random encryption key
    pub fn generate() -> Self {
        let mut key = [0u8; KEY_SIZE];
        getrandom::fill(&mut key).expect("operating system RNG unavailable");
        Self { key }
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
/// Handles storing and retrieving encryption keys.
/// Currently uses file-based storage; can be extended to use OS keychain.
pub struct KeyStore {
    /// Path to the key file
    key_path: PathBuf,
}

impl KeyStore {
    /// Create an environment-scoped key store (~/.tako/keys/{env})
    pub fn for_env(env: &str) -> Result<Self> {
        validate_key_scope(env)?;

        let home = crate::paths::tako_home_dir().map_err(|e| {
            ConfigError::Validation(format!("Could not determine tako home directory: {}", e))
        })?;

        Ok(Self {
            key_path: home.join("keys").join(env),
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

    /// Get or create the encryption key
    pub fn get_or_create_key(&self) -> Result<EncryptionKey> {
        if self.key_path.exists() {
            self.load_key()
        } else {
            let key = EncryptionKey::generate();
            self.save_key(&key)?;
            Ok(key)
        }
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

        // Write key with restrictive permissions
        let encoded = key.to_base64();
        fs::write(&self.key_path, &encoded)
            .map_err(|e| ConfigError::FileWrite(self.key_path.clone(), e))?;

        // Set file permissions to 600 (owner read/write only) on Unix
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let permissions = std::fs::Permissions::from_mode(0o600);
            fs::set_permissions(&self.key_path, permissions)
                .map_err(|e| ConfigError::FileWrite(self.key_path.clone(), e))?;
        }

        Ok(())
    }

    /// Check if a key exists
    pub fn key_exists(&self) -> bool {
        self.key_path.exists()
    }

    /// Delete the key (use with caution!)
    pub fn delete_key(&self) -> Result<()> {
        if self.key_path.exists() {
            fs::remove_file(&self.key_path)
                .map_err(|e| ConfigError::FileWrite(self.key_path.clone(), e))?;
        }
        Ok(())
    }
}

fn validate_key_scope(scope: &str) -> Result<()> {
    if scope.is_empty() {
        return Err(ConfigError::Validation(
            "Environment name cannot be empty".to_string(),
        ));
    }

    for c in scope.chars() {
        if !c.is_ascii_lowercase() && !c.is_ascii_digit() && c != '-' {
            return Err(ConfigError::Validation(format!(
                "Environment name can only contain lowercase letters, numbers, and hyphens. Found: '{}'",
                c
            )));
        }
    }

    Ok(())
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

    #[test]
    fn test_generate_key() {
        let key = EncryptionKey::generate();
        assert_eq!(key.as_bytes().len(), KEY_SIZE);
    }

    #[test]
    fn test_generate_key_is_random() {
        let key1 = EncryptionKey::generate();
        let key2 = EncryptionKey::generate();
        assert_ne!(key1.as_bytes(), key2.as_bytes());
    }

    #[test]
    fn test_key_from_bytes() {
        let bytes = [0u8; KEY_SIZE];
        let key = EncryptionKey::from_bytes(&bytes).unwrap();
        assert_eq!(key.as_bytes(), &bytes);
    }

    #[test]
    fn test_key_from_bytes_wrong_size() {
        let bytes = [0u8; 16]; // Wrong size
        let result = EncryptionKey::from_bytes(&bytes);
        assert!(result.is_err());
    }

    #[test]
    fn test_key_base64_round_trip() {
        let key = EncryptionKey::generate();
        let encoded = key.to_base64();
        let decoded = EncryptionKey::from_base64(&encoded).unwrap();
        assert_eq!(key.as_bytes(), decoded.as_bytes());
    }

    #[test]
    fn test_encrypt_decrypt_round_trip() {
        let key = EncryptionKey::generate();
        let plaintext = "Hello, World!";

        let encrypted = encrypt(plaintext, &key).unwrap();
        let decrypted = decrypt(&encrypted, &key).unwrap();

        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn test_encrypt_produces_different_ciphertext() {
        let key = EncryptionKey::generate();
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
        let key1 = EncryptionKey::generate();
        let key2 = EncryptionKey::generate();
        let plaintext = "Hello, World!";

        let encrypted = encrypt(plaintext, &key1).unwrap();
        let result = decrypt(&encrypted, &key2);

        assert!(result.is_err());
    }

    #[test]
    fn test_encrypt_unicode() {
        let key = EncryptionKey::generate();
        let plaintext = "Hello, 世界! 🔐";

        let encrypted = encrypt(plaintext, &key).unwrap();
        let decrypted = decrypt(&encrypted, &key).unwrap();

        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn test_key_store_for_env_uses_keys_subdir() {
        with_temp_tako_home(|temp_home| {
            let store = KeyStore::for_env("production").unwrap();
            assert_eq!(store.key_path(), temp_home.join("keys").join("production"));
        });
    }

    #[test]
    fn test_key_store_for_env_rejects_invalid_name() {
        with_temp_tako_home(|_| {
            let result = KeyStore::for_env("../production");
            assert!(result.is_err());
        });
    }
}

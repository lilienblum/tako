use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use rcgen::{
    BasicConstraints, CertificateParams, DistinguishedName, DnType, ExtendedKeyUsagePurpose, IsCa,
    Issuer, KeyPair, KeyUsagePurpose, SanType,
};
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use thiserror::Error;
use time::{Duration, OffsetDateTime};

use crate::paths::tako_home_dir;

/// Root CA certificate validity period (10 years)
const CA_VALIDITY_DAYS: i64 = 3650;
/// Leaf certificate validity period (1 year)
const LEAF_VALIDITY_DAYS: i64 = 365;

const CA_COMMON_NAME: &str = "Tako Local Development CA";
const CA_ORGANIZATION: &str = "Tako";
const LOCAL_CA_CERT_FILENAME: &str = "ca.crt";
const LEGACY_LOCAL_CA_CERT_FILENAME: &str = "tako-ca.crt";

fn keychain_account_for_home(home: &std::path::Path) -> String {
    // Namespace keychain entries by TAKO_HOME so switching homes cannot pair a
    // cert from one home with a private key from another.
    let mut hash: u64 = 0xcbf29ce484222325; // FNV-1a offset basis
    for b in home.to_string_lossy().as_bytes() {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x100000001b3); // FNV-1a prime
    }
    format!("tako-{hash:016x}")
}

#[derive(Debug, Error)]
pub enum CaError {
    #[error("Failed to generate keypair: {0}")]
    KeypairGeneration(String),
    #[error("Failed to generate certificate: {0}")]
    CertificateGeneration(String),
    #[error("Failed to parse certificate/key: {0}")]
    Parse(String),
    #[error("Failed to read file {0}: {1}")]
    FileRead(PathBuf, std::io::Error),
    #[error("Failed to write file {0}: {1}")]
    FileWrite(PathBuf, std::io::Error),
    #[error("Validation error: {0}")]
    Validation(String),
}

pub type Result<T> = std::result::Result<T, CaError>;

pub struct Certificate {
    pub cert_pem: String,
    pub key_pem: String,
}

#[derive(Clone)]
pub struct LocalCA {
    ca_cert_pem: String,
    ca_key_pem: String,
}

impl LocalCA {
    pub fn new(ca_cert_pem: String, ca_key_pem: String) -> Self {
        Self {
            ca_cert_pem,
            ca_key_pem,
        }
    }

    pub fn generate() -> Result<Self> {
        let mut params = CertificateParams::default();

        let mut dn = DistinguishedName::new();
        dn.push(DnType::CommonName, CA_COMMON_NAME);
        dn.push(DnType::OrganizationName, CA_ORGANIZATION);
        params.distinguished_name = dn;
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        params.key_usages = vec![
            KeyUsagePurpose::KeyCertSign,
            KeyUsagePurpose::CrlSign,
            KeyUsagePurpose::DigitalSignature,
        ];

        let now = OffsetDateTime::now_utc();
        params.not_before = now;
        params.not_after = now + Duration::days(CA_VALIDITY_DAYS);

        let key_pair =
            KeyPair::generate().map_err(|e| CaError::KeypairGeneration(e.to_string()))?;
        let cert = params
            .self_signed(&key_pair)
            .map_err(|e| CaError::CertificateGeneration(e.to_string()))?;

        Ok(Self {
            ca_cert_pem: cert.pem(),
            ca_key_pem: key_pair.serialize_pem(),
        })
    }

    pub fn generate_leaf_cert_for_names(&self, names: &[&str]) -> Result<Certificate> {
        let primary = names
            .first()
            .ok_or_else(|| CaError::Validation("At least one name is required".to_string()))?;

        let ca_key = KeyPair::from_pem(&self.ca_key_pem)
            .map_err(|e| CaError::Parse(format!("Failed to parse CA private key: {}", e)))?;

        // Recreate CA cert params to sign leaf.
        let mut ca_params = CertificateParams::default();
        let mut ca_dn = DistinguishedName::new();
        ca_dn.push(DnType::CommonName, CA_COMMON_NAME);
        ca_dn.push(DnType::OrganizationName, CA_ORGANIZATION);
        ca_params.distinguished_name = ca_dn;
        ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        ca_params.key_usages = vec![
            KeyUsagePurpose::KeyCertSign,
            KeyUsagePurpose::CrlSign,
            KeyUsagePurpose::DigitalSignature,
        ];
        let now = OffsetDateTime::now_utc();
        ca_params.not_before = now - Duration::days(1);
        ca_params.not_after = now + Duration::days(CA_VALIDITY_DAYS);

        let issuer = Issuer::new(ca_params, ca_key);

        let mut params = CertificateParams::default();
        let mut dn = DistinguishedName::new();
        dn.push(DnType::CommonName, *primary);
        dn.push(DnType::OrganizationName, CA_ORGANIZATION);
        params.distinguished_name = dn;
        params.is_ca = IsCa::NoCa;
        params.key_usages = vec![
            KeyUsagePurpose::DigitalSignature,
            KeyUsagePurpose::KeyEncipherment,
        ];
        params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];

        let mut sans = Vec::new();
        for name in names {
            if let Ok(ip) = name.parse::<std::net::IpAddr>() {
                sans.push(SanType::IpAddress(ip));
            } else {
                let dns = (*name).try_into().map_err(|e| {
                    CaError::Validation(format!("Invalid DNS name '{}': {:?}", name, e))
                })?;
                sans.push(SanType::DnsName(dns));
            }
        }
        params.subject_alt_names = sans;
        params.not_before = now;
        params.not_after = now + Duration::days(LEAF_VALIDITY_DAYS);

        let leaf_key =
            KeyPair::generate().map_err(|e| CaError::KeypairGeneration(e.to_string()))?;
        let leaf_cert = params.signed_by(&leaf_key, &issuer).map_err(|e| {
            CaError::CertificateGeneration(format!("Failed to sign leaf certificate: {}", e))
        })?;

        Ok(Certificate {
            cert_pem: leaf_cert.pem(),
            key_pem: leaf_key.serialize_pem(),
        })
    }
}

pub struct LocalCAStore {
    ca_cert_path: PathBuf,
    keychain_service: String,
    keychain_account: String,
}

impl LocalCAStore {
    pub fn new() -> Result<Self> {
        let home = tako_home_dir().map_err(|e| {
            CaError::Validation(format!("Could not determine tako home directory: {}", e))
        })?;

        let ca_dir = home.join("ca");
        let ca_cert_path = ca_dir.join(LOCAL_CA_CERT_FILENAME);

        Ok(Self {
            ca_cert_path,
            keychain_service: "tako-local-ca".to_string(),
            keychain_account: keychain_account_for_home(&home),
        })
    }

    fn ca_key_path(&self) -> PathBuf {
        self.ca_cert_path.with_extension("key")
    }

    fn legacy_ca_cert_path(&self) -> PathBuf {
        self.ca_cert_path
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."))
            .join(LEGACY_LOCAL_CA_CERT_FILENAME)
    }

    fn legacy_ca_key_path(&self) -> PathBuf {
        self.legacy_ca_cert_path().with_extension("key")
    }

    fn existing_ca_cert_path(&self) -> PathBuf {
        if self.ca_cert_path.exists() {
            return self.ca_cert_path.clone();
        }
        let legacy = self.legacy_ca_cert_path();
        if legacy.exists() {
            return legacy;
        }
        self.ca_cert_path.clone()
    }

    fn save_ca_key_to_file(&self, key_pem: &str) -> Result<()> {
        let key_path = self.ca_key_path();
        fs::write(&key_path, key_pem).map_err(|e| CaError::FileWrite(key_path.clone(), e))?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let permissions = std::fs::Permissions::from_mode(0o600);
            fs::set_permissions(&key_path, permissions)
                .map_err(|e| CaError::FileWrite(key_path.clone(), e))?;
        }

        Ok(())
    }

    fn load_ca_key_from_file(&self) -> Result<String> {
        let primary = self.ca_key_path();
        if let Ok(key) = fs::read_to_string(&primary) {
            return Ok(key);
        }

        let legacy = self.legacy_ca_key_path();
        fs::read_to_string(&legacy).map_err(|e| CaError::FileRead(legacy.clone(), e))
    }

    pub fn ca_exists(&self) -> bool {
        (self.ca_cert_path.exists() || self.legacy_ca_cert_path().exists())
            && (self.load_ca_key_from_keychain().is_ok() || self.load_ca_key_from_file().is_ok())
    }

    pub fn load_ca(&self) -> Result<LocalCA> {
        let cert_path = self.existing_ca_cert_path();
        let ca_cert_pem =
            fs::read_to_string(&cert_path).map_err(|e| CaError::FileRead(cert_path.clone(), e))?;
        let ca_key_pem = self.load_ca_key_from_keychain()?;
        Ok(LocalCA::new(ca_cert_pem, ca_key_pem))
    }

    pub fn save_ca(&self, ca: &LocalCA) -> Result<()> {
        if let Some(parent) = self.ca_cert_path.parent() {
            fs::create_dir_all(parent).map_err(|e| CaError::FileWrite(parent.to_path_buf(), e))?;
        }
        fs::write(&self.ca_cert_path, ca.ca_cert_pem.as_bytes())
            .map_err(|e| CaError::FileWrite(self.ca_cert_path.clone(), e))?;
        self.save_ca_key_to_keychain(&ca.ca_key_pem)?;
        Ok(())
    }

    pub fn get_or_create_ca(&self) -> Result<LocalCA> {
        if self.ca_exists() {
            self.load_ca()
        } else {
            let ca = LocalCA::generate()?;
            self.save_ca(&ca)?;
            Ok(ca)
        }
    }

    #[cfg(target_os = "macos")]
    fn save_ca_key_to_keychain(&self, key_pem: &str) -> Result<()> {
        let encoded = BASE64.encode(key_pem.as_bytes());
        let output = Command::new("security")
            .args([
                "add-generic-password",
                "-U",
                "-s",
                &self.keychain_service,
                "-a",
                &self.keychain_account,
                "-w",
                &encoded,
            ])
            .output();
        match output {
            Ok(out) if out.status.success() => {
                let key_path = self.ca_key_path();
                if key_path.exists() {
                    let _ = fs::remove_file(key_path);
                }
                Ok(())
            }
            _ => self.save_ca_key_to_file(key_pem),
        }
    }

    #[cfg(not(target_os = "macos"))]
    fn save_ca_key_to_keychain(&self, key_pem: &str) -> Result<()> {
        self.save_ca_key_to_file(key_pem)
    }

    #[cfg(target_os = "macos")]
    fn load_ca_key_from_keychain(&self) -> Result<String> {
        let output = Command::new("security")
            .args([
                "find-generic-password",
                "-s",
                &self.keychain_service,
                "-a",
                &self.keychain_account,
                "-w",
            ])
            .output();
        if let Ok(out) = output
            && out.status.success()
            && let Ok(encoded) = String::from_utf8(out.stdout)
            && let Ok(decoded) = BASE64.decode(encoded.trim())
            && let Ok(key) = String::from_utf8(decoded)
        {
            return Ok(key);
        }
        self.load_ca_key_from_file()
    }

    #[cfg(not(target_os = "macos"))]
    fn load_ca_key_from_keychain(&self) -> Result<String> {
        self.load_ca_key_from_file()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn keychain_account_is_stable_and_home_scoped() {
        let a1 = keychain_account_for_home(std::path::Path::new("/tmp/a"));
        let a2 = keychain_account_for_home(std::path::Path::new("/tmp/a"));
        let b = keychain_account_for_home(std::path::Path::new("/tmp/b"));
        assert_eq!(a1, a2);
        assert_ne!(a1, b);
    }

    #[test]
    fn ca_store_loads_from_file_fallback() {
        let temp_dir = TempDir::new().unwrap();
        let ca_cert_path = temp_dir.path().join("ca").join("ca.crt");

        let store = LocalCAStore {
            ca_cert_path: ca_cert_path.clone(),
            keychain_service: "tako-dev-test-ca-fallback-load".to_string(),
            keychain_account: "tako-dev-test-fallback-load".to_string(),
        };

        let ca = LocalCA::generate().unwrap();
        std::fs::create_dir_all(ca_cert_path.parent().unwrap()).unwrap();
        std::fs::write(&ca_cert_path, &ca.ca_cert_pem).unwrap();
        std::fs::write(ca_cert_path.with_extension("key"), &ca.ca_key_pem).unwrap();

        let loaded = store.load_ca().unwrap();
        assert_eq!(loaded.ca_cert_pem, ca.ca_cert_pem);
        assert_eq!(loaded.ca_key_pem, ca.ca_key_pem);
    }

    #[test]
    fn ca_exists_with_file_fallback_key() {
        let temp_dir = TempDir::new().unwrap();
        let ca_cert_path = temp_dir.path().join("ca").join("ca.crt");

        let store = LocalCAStore {
            ca_cert_path: ca_cert_path.clone(),
            keychain_service: "tako-dev-test-ca-fallback-exists".to_string(),
            keychain_account: "tako-dev-test-fallback-exists".to_string(),
        };

        let ca = LocalCA::generate().unwrap();
        std::fs::create_dir_all(ca_cert_path.parent().unwrap()).unwrap();
        std::fs::write(&ca_cert_path, &ca.ca_cert_pem).unwrap();
        std::fs::write(ca_cert_path.with_extension("key"), &ca.ca_key_pem).unwrap();

        assert!(store.ca_exists());
    }

    #[test]
    fn ca_store_loads_legacy_filenames() {
        let temp_dir = TempDir::new().unwrap();
        let current_ca_cert_path = temp_dir.path().join("ca").join("ca.crt");
        let legacy_ca_cert_path = temp_dir.path().join("ca").join("tako-ca.crt");

        let store = LocalCAStore {
            ca_cert_path: current_ca_cert_path,
            keychain_service: "tako-dev-test-ca-legacy-load".to_string(),
            keychain_account: "tako-dev-test-legacy-load".to_string(),
        };

        let ca = LocalCA::generate().unwrap();
        std::fs::create_dir_all(legacy_ca_cert_path.parent().unwrap()).unwrap();
        std::fs::write(&legacy_ca_cert_path, &ca.ca_cert_pem).unwrap();
        std::fs::write(legacy_ca_cert_path.with_extension("key"), &ca.ca_key_pem).unwrap();

        let loaded = store.load_ca().unwrap();
        assert_eq!(loaded.ca_cert_pem, ca.ca_cert_pem);
        assert_eq!(loaded.ca_key_pem, ca.ca_key_pem);
    }
}

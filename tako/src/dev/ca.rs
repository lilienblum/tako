//! Local Certificate Authority for Development
//!
//! Generates and manages a local CA for trusted HTTPS in development.
//! Apps are accessible at `https://{app-name}.tako.local` with certificates
//! signed by the local CA.
//!
//! Security model:
//! - Root CA private key stored in system keychain (encrypted)
//! - Root CA public cert installed in system trust store
//! - Leaf certificates generated on-the-fly, stored in memory only
//! - No unencrypted key material on disk

#[cfg(target_os = "macos")]
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

use super::domain::TAKO_LOCAL_DOMAIN;

/// Root CA certificate validity period (10 years)
const CA_VALIDITY_DAYS: i64 = 3650;

/// Leaf certificate validity period (1 year)
const LEAF_VALIDITY_DAYS: i64 = 365;

/// Root CA common name
const CA_COMMON_NAME: &str = "Tako Local Development CA";

/// Root CA organization
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

/// Errors that can occur during CA operations
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

    #[error("Keychain operation failed: {0}")]
    Keychain(String),

    #[error("Validation error: {0}")]
    Validation(String),
}

pub type Result<T> = std::result::Result<T, CaError>;

/// A generated certificate with its private key
#[derive(Clone)]
pub struct Certificate {
    /// PEM-encoded certificate
    pub cert_pem: String,
    /// PEM-encoded private key
    pub key_pem: String,
}

/// Local Certificate Authority for development
pub struct LocalCA {
    /// Root CA certificate (PEM)
    ca_cert_pem: String,
    /// Root CA private key (PEM) - loaded from keychain
    ca_key_pem: String,
}

impl LocalCA {
    /// Create a new LocalCA from existing certificate and key
    pub fn new(ca_cert_pem: String, ca_key_pem: String) -> Self {
        Self {
            ca_cert_pem,
            ca_key_pem,
        }
    }

    /// Get the CA certificate PEM
    pub fn ca_cert_pem(&self) -> &str {
        &self.ca_cert_pem
    }

    /// Generate a new Root CA keypair
    pub fn generate() -> Result<Self> {
        let mut params = CertificateParams::default();

        // Set distinguished name
        let mut dn = DistinguishedName::new();
        dn.push(DnType::CommonName, CA_COMMON_NAME);
        dn.push(DnType::OrganizationName, CA_ORGANIZATION);
        params.distinguished_name = dn;

        // Set as CA certificate
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);

        // Set key usage for CA
        params.key_usages = vec![
            KeyUsagePurpose::KeyCertSign,
            KeyUsagePurpose::CrlSign,
            KeyUsagePurpose::DigitalSignature,
        ];

        // Set validity period
        let now = OffsetDateTime::now_utc();
        params.not_before = now;
        params.not_after = now + Duration::days(CA_VALIDITY_DAYS);

        // Generate keypair and certificate
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

    /// Generate a leaf certificate for a domain
    ///
    /// The domain should be in the format `{app-name}.tako.local`
    pub fn generate_leaf_cert(&self, domain: &str) -> Result<Certificate> {
        // Parse the CA key
        let ca_key = KeyPair::from_pem(&self.ca_key_pem)
            .map_err(|e| CaError::Parse(format!("Failed to parse CA private key: {}", e)))?;

        // Recreate CA cert params to get a signable certificate
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
        ca_params.not_before = now - Duration::days(1); // Allow for clock skew
        ca_params.not_after = now + Duration::days(CA_VALIDITY_DAYS);

        let issuer = Issuer::new(ca_params, ca_key);

        // Create leaf certificate parameters
        let mut params = CertificateParams::default();

        // Set distinguished name
        let mut dn = DistinguishedName::new();
        dn.push(DnType::CommonName, domain);
        dn.push(DnType::OrganizationName, CA_ORGANIZATION);
        params.distinguished_name = dn;

        // Not a CA
        params.is_ca = IsCa::NoCa;

        // Set key usage for server certificate
        params.key_usages = vec![
            KeyUsagePurpose::DigitalSignature,
            KeyUsagePurpose::KeyEncipherment,
        ];

        // Extended key usage for TLS server
        params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];

        // Subject Alternative Names
        params.subject_alt_names = vec![SanType::DnsName(domain.try_into().map_err(|e| {
            CaError::Validation(format!("Invalid domain name '{}': {:?}", domain, e))
        })?)];

        // Set validity period
        params.not_before = now;
        params.not_after = now + Duration::days(LEAF_VALIDITY_DAYS);

        // Generate keypair for leaf certificate
        let leaf_key =
            KeyPair::generate().map_err(|e| CaError::KeypairGeneration(e.to_string()))?;

        // Sign with CA
        let leaf_cert = params.signed_by(&leaf_key, &issuer).map_err(|e| {
            CaError::CertificateGeneration(format!("Failed to sign leaf certificate: {}", e))
        })?;

        Ok(Certificate {
            cert_pem: leaf_cert.pem(),
            key_pem: leaf_key.serialize_pem(),
        })
    }

    /// Get the domain for an app name
    pub fn app_domain(app_name: &str) -> String {
        format!("{}.{}", app_name, TAKO_LOCAL_DOMAIN)
    }

    /// Generate a leaf certificate with multiple SANs (DNS names and/or IPs).
    ///
    /// The first entry is used as the certificate's Common Name.
    pub fn generate_leaf_cert_for_names(&self, names: &[&str]) -> Result<Certificate> {
        let primary = names
            .first()
            .ok_or_else(|| CaError::Validation("At least one name is required".to_string()))?;

        // Parse the CA key
        let ca_key = KeyPair::from_pem(&self.ca_key_pem)
            .map_err(|e| CaError::Parse(format!("Failed to parse CA private key: {}", e)))?;

        // Recreate CA cert params to get a signable certificate
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
        ca_params.not_before = now - Duration::days(1); // Allow for clock skew
        ca_params.not_after = now + Duration::days(CA_VALIDITY_DAYS);

        let issuer = Issuer::new(ca_params, ca_key);

        // Create leaf certificate parameters
        let mut params = CertificateParams::default();

        // Set distinguished name
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

/// Manages the local CA storage and trust
pub struct LocalCAStore {
    /// Path to the CA certificate file
    ca_cert_path: PathBuf,
    /// Keychain service name for the CA private key
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    keychain_service: String,
    /// Keychain account name
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    keychain_account: String,
}

#[cfg(target_os = "macos")]
fn trim_ascii_leading_whitespace(input: &[u8]) -> &[u8] {
    let idx = input
        .iter()
        .position(|b| !b.is_ascii_whitespace())
        .unwrap_or(input.len());
    &input[idx..]
}

#[cfg(target_os = "macos")]
fn extract_pem_der_bundle(pem_bundle: &str) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let mut remaining = pem_bundle.as_bytes();

    loop {
        remaining = trim_ascii_leading_whitespace(remaining);
        if remaining.is_empty() {
            break;
        }

        match x509_parser::pem::parse_x509_pem(remaining) {
            Ok((next, pem)) => {
                out.push(pem.contents);
                if next.len() >= remaining.len() {
                    break;
                }
                remaining = next;
            }
            Err(_) => break,
        }
    }

    out
}

#[cfg(target_os = "macos")]
fn system_keychain_output_contains_cert(system_keychain_bundle: &str, cert_pem: &str) -> bool {
    let Some(target_der) = extract_pem_der_bundle(cert_pem).into_iter().next() else {
        return false;
    };

    extract_pem_der_bundle(system_keychain_bundle)
        .into_iter()
        .any(|der| der == target_der)
}

impl LocalCAStore {
    /// Create a new CA store with default paths
    pub fn new() -> Result<Self> {
        let home = crate::paths::tako_home_dir().map_err(|e| {
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

    /// Get path to CA certificate
    pub fn ca_cert_path(&self) -> &PathBuf {
        &self.ca_cert_path
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

    /// Check if the CA exists
    pub fn ca_exists(&self) -> bool {
        (self.ca_cert_path.exists() || self.legacy_ca_cert_path().exists())
            && (self.load_ca_key_from_keychain().is_ok() || self.load_ca_key_from_file().is_ok())
    }

    /// Get or create the local CA
    pub fn get_or_create_ca(&self) -> Result<LocalCA> {
        if self.ca_exists() {
            self.load_ca()
        } else {
            let ca = LocalCA::generate()?;
            self.save_ca(&ca)?;
            Ok(ca)
        }
    }

    /// Load existing CA
    pub fn load_ca(&self) -> Result<LocalCA> {
        let cert_path = self.existing_ca_cert_path();
        let ca_cert_pem =
            fs::read_to_string(&cert_path).map_err(|e| CaError::FileRead(cert_path.clone(), e))?;

        let ca_key_pem = self.load_ca_key_from_keychain()?;

        Ok(LocalCA::new(ca_cert_pem, ca_key_pem))
    }

    /// Save CA to storage
    pub fn save_ca(&self, ca: &LocalCA) -> Result<()> {
        // Ensure directory exists
        if let Some(parent) = self.ca_cert_path.parent() {
            fs::create_dir_all(parent).map_err(|e| CaError::FileWrite(parent.to_path_buf(), e))?;
        }

        // Save certificate (public, can be on disk)
        fs::write(&self.ca_cert_path, &ca.ca_cert_pem)
            .map_err(|e| CaError::FileWrite(self.ca_cert_path.clone(), e))?;

        // Save private key to keychain
        self.save_ca_key_to_keychain(&ca.ca_key_pem)?;

        Ok(())
    }

    /// Save CA private key to system keychain
    #[cfg(target_os = "macos")]
    fn save_ca_key_to_keychain(&self, key_pem: &str) -> Result<()> {
        // Encode the key as base64 for storage
        let encoded = BASE64.encode(key_pem.as_bytes());

        // Use security command to add to keychain
        let output = Command::new("security")
            .args([
                "add-generic-password",
                "-U", // Update if exists
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

    /// Save CA private key - Linux fallback (file-based with restricted permissions)
    #[cfg(not(target_os = "macos"))]
    fn save_ca_key_to_keychain(&self, key_pem: &str) -> Result<()> {
        self.save_ca_key_to_file(key_pem)
    }

    /// Load CA private key from system keychain
    #[cfg(target_os = "macos")]
    fn load_ca_key_from_keychain(&self) -> Result<String> {
        let output = Command::new("security")
            .args([
                "find-generic-password",
                "-s",
                &self.keychain_service,
                "-a",
                &self.keychain_account,
                "-w", // Output password only
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

    /// Load CA private key - Linux fallback
    #[cfg(not(target_os = "macos"))]
    fn load_ca_key_from_keychain(&self) -> Result<String> {
        self.load_ca_key_from_file()
    }

    /// Check if CA is installed in system trust store
    #[cfg(target_os = "macos")]
    pub fn is_ca_trusted(&self) -> bool {
        let cert_path = self.existing_ca_cert_path();
        if !cert_path.exists() {
            return false;
        }

        let local_cert = match fs::read_to_string(&cert_path) {
            Ok(cert) => cert,
            Err(_) => return false,
        };

        // Prefer exact cert match in the macOS System keychain.
        let in_system_keychain = Command::new("security")
            .args([
                "find-certificate",
                "-c",
                CA_COMMON_NAME,
                "-p",
                "/Library/Keychains/System.keychain",
            ])
            .output();

        if let Ok(out) = in_system_keychain
            && out.status.success()
        {
            let pem_bundle = String::from_utf8_lossy(&out.stdout);
            if system_keychain_output_contains_cert(&pem_bundle, &local_cert) {
                return true;
            }
        }

        // Fallback verifier check.
        let verify = Command::new("security")
            .args(["verify-cert", "-c", cert_path.to_str().unwrap_or("")])
            .output();

        match verify {
            Ok(out) => out.status.success(),
            Err(_) => false,
        }
    }

    /// Check if CA is trusted - Linux
    #[cfg(not(target_os = "macos"))]
    pub fn is_ca_trusted(&self) -> bool {
        // Check if the CA cert exists in the system CA store
        let system_ca_path = PathBuf::from("/usr/local/share/ca-certificates/tako-ca.crt");
        system_ca_path.exists()
    }

    /// Install CA in system trust store (requires sudo)
    #[cfg(target_os = "macos")]
    pub fn install_ca_trust(&self) -> Result<()> {
        let cert_path = self.existing_ca_cert_path();
        if !cert_path.exists() {
            return Err(CaError::Validation(
                "CA certificate not found. Run get_or_create_ca() first.".to_string(),
            ));
        }

        let output = Command::new("sudo")
            .args([
                "security",
                "add-trusted-cert",
                "-d",
                "-r",
                "trustRoot",
                "-k",
                "/Library/Keychains/System.keychain",
                cert_path.to_str().unwrap_or(""),
            ])
            .status()
            .map_err(|e| CaError::Keychain(format!("Failed to run security command: {}", e)))?;

        if !output.success() {
            return Err(CaError::Keychain(
                "Failed to install CA in trust store".to_string(),
            ));
        }
        Ok(())
    }

    /// Install CA in system trust store - Linux
    #[cfg(not(target_os = "macos"))]
    pub fn install_ca_trust(&self) -> Result<()> {
        let cert_path = self.existing_ca_cert_path();
        if !cert_path.exists() {
            return Err(CaError::Validation(
                "CA certificate not found. Run get_or_create_ca() first.".to_string(),
            ));
        }

        // Copy cert to system CA directory
        let dest = "/usr/local/share/ca-certificates/tako-ca.crt";
        let copy_status = Command::new("sudo")
            .args(["cp", cert_path.to_str().unwrap_or(""), dest])
            .status()
            .map_err(|e| CaError::Keychain(format!("Failed to copy CA cert: {}", e)))?;

        if !copy_status.success() {
            return Err(CaError::Keychain(
                "Failed to copy CA to system directory".to_string(),
            ));
        }

        // Update CA certificates
        let update_status = Command::new("sudo")
            .args(["update-ca-certificates"])
            .status()
            .map_err(|e| {
                CaError::Keychain(format!("Failed to run update-ca-certificates: {}", e))
            })?;

        if !update_status.success() {
            return Err(CaError::Keychain(
                "Failed to update system CA certificates".to_string(),
            ));
        }
        Ok(())
    }

    /// Delete the CA (removes from keychain and disk)
    pub fn delete_ca(&self) -> Result<()> {
        // Remove certificate file
        if self.ca_cert_path.exists() {
            fs::remove_file(&self.ca_cert_path)
                .map_err(|e| CaError::FileWrite(self.ca_cert_path.clone(), e))?;
        }
        let legacy_cert = self.legacy_ca_cert_path();
        if legacy_cert.exists() {
            fs::remove_file(&legacy_cert)
                .map_err(|e| CaError::FileWrite(legacy_cert.clone(), e))?;
        }

        // Remove from keychain
        #[cfg(target_os = "macos")]
        {
            let _ = Command::new("security")
                .args([
                    "delete-generic-password",
                    "-s",
                    &self.keychain_service,
                    "-a",
                    &self.keychain_account,
                ])
                .output();

            let key_path = self.ca_key_path();
            if key_path.exists() {
                fs::remove_file(&key_path).map_err(|e| CaError::FileWrite(key_path.clone(), e))?;
            }
            let legacy_key = self.legacy_ca_key_path();
            if legacy_key.exists() {
                fs::remove_file(&legacy_key)
                    .map_err(|e| CaError::FileWrite(legacy_key.clone(), e))?;
            }
        }

        #[cfg(not(target_os = "macos"))]
        {
            let key_path = self.ca_key_path();
            if key_path.exists() {
                fs::remove_file(&key_path).map_err(|e| CaError::FileWrite(key_path.clone(), e))?;
            }
            let legacy_key = self.legacy_ca_key_path();
            if legacy_key.exists() {
                fs::remove_file(&legacy_key)
                    .map_err(|e| CaError::FileWrite(legacy_key.clone(), e))?;
            }
        }

        Ok(())
    }
}

impl Default for LocalCAStore {
    fn default() -> Self {
        Self::new().expect("Failed to create LocalCAStore")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_generate_ca() {
        let ca = LocalCA::generate().unwrap();
        assert!(ca.ca_cert_pem.contains("BEGIN CERTIFICATE"));
        assert!(ca.ca_key_pem.contains("BEGIN PRIVATE KEY"));
    }

    #[test]
    fn test_generate_leaf_cert() {
        let ca = LocalCA::generate().unwrap();
        let domain = "my-app.tako.local";

        let leaf = ca.generate_leaf_cert(domain).unwrap();

        assert!(leaf.cert_pem.contains("BEGIN CERTIFICATE"));
        assert!(leaf.key_pem.contains("BEGIN PRIVATE KEY"));
    }

    #[test]
    fn test_generate_multiple_leaf_certs() {
        let ca = LocalCA::generate().unwrap();

        let leaf1 = ca.generate_leaf_cert("app1.tako.local").unwrap();
        let leaf2 = ca.generate_leaf_cert("app2.tako.local").unwrap();

        // Each leaf cert should be unique
        assert_ne!(leaf1.cert_pem, leaf2.cert_pem);
        assert_ne!(leaf1.key_pem, leaf2.key_pem);
    }

    #[test]
    fn test_app_domain() {
        assert_eq!(LocalCA::app_domain("my-app"), "my-app.tako.local");
        assert_eq!(LocalCA::app_domain("dashboard"), "dashboard.tako.local");
    }

    #[test]
    fn keychain_account_is_stable_and_home_scoped() {
        let a1 = keychain_account_for_home(std::path::Path::new("/tmp/a"));
        let a2 = keychain_account_for_home(std::path::Path::new("/tmp/a"));
        let b = keychain_account_for_home(std::path::Path::new("/tmp/b"));
        assert_eq!(a1, a2);
        assert_ne!(a1, b);
    }

    #[test]
    fn test_ca_store_save_and_load() {
        let temp_dir = TempDir::new().unwrap();
        let ca_cert_path = temp_dir.path().join("ca").join("ca.crt");

        let store = LocalCAStore {
            ca_cert_path,
            keychain_service: "tako-test-ca".to_string(),
            keychain_account: "tako-test".to_string(),
        };

        // Generate and save CA
        let ca = LocalCA::generate().unwrap();
        match store.save_ca(&ca) {
            Ok(()) => {}
            Err(CaError::Keychain(_)) => return,
            Err(e) => panic!("failed to save CA: {e}"),
        }

        // Load CA back
        let loaded = match store.load_ca() {
            Ok(loaded) => loaded,
            Err(CaError::Keychain(_)) => return,
            Err(e) => panic!("failed to load CA: {e}"),
        };

        assert_eq!(ca.ca_cert_pem, loaded.ca_cert_pem);
        assert_eq!(ca.ca_key_pem, loaded.ca_key_pem);
    }

    #[test]
    fn test_ca_store_get_or_create() {
        let temp_dir = TempDir::new().unwrap();
        let ca_cert_path = temp_dir.path().join("ca").join("ca.crt");

        let store = LocalCAStore {
            ca_cert_path: ca_cert_path.clone(),
            keychain_service: "tako-test-ca2".to_string(),
            keychain_account: "tako-test2".to_string(),
        };

        // First call creates CA
        let ca1 = match store.get_or_create_ca() {
            Ok(ca) => ca,
            Err(CaError::Keychain(_)) => return,
            Err(e) => panic!("failed to create CA: {e}"),
        };
        assert!(ca_cert_path.exists());

        // Second call loads existing CA
        let ca2 = match store.get_or_create_ca() {
            Ok(ca) => ca,
            Err(CaError::Keychain(_)) => return,
            Err(e) => panic!("failed to load existing CA: {e}"),
        };
        assert_eq!(ca1.ca_cert_pem, ca2.ca_cert_pem);
    }

    #[test]
    fn test_leaf_cert_has_correct_san() {
        let ca = LocalCA::generate().unwrap();
        let domain = "test-app.tako.local";
        let leaf = ca.generate_leaf_cert(domain).unwrap();

        // Parse the certificate to verify SAN
        let (_, cert) = x509_parser::pem::parse_x509_pem(leaf.cert_pem.as_bytes()).unwrap();
        let cert = cert.parse_x509().unwrap();

        // Check Subject Alternative Name extension includes our expected entry.
        let san_ext = cert
            .extensions()
            .iter()
            .find(|ext| ext.oid == x509_parser::oid_registry::OID_X509_EXT_SUBJECT_ALT_NAME)
            .expect("Certificate should have SAN extension");

        let san = match san_ext.parsed_extension() {
            x509_parser::extensions::ParsedExtension::SubjectAlternativeName(san) => san,
            other => panic!("Expected SubjectAlternativeName, got {:?}", other),
        };

        let mut has_domain = false;

        for name in san.general_names.iter() {
            if let x509_parser::extensions::GeneralName::DNSName(d) = name
                && *d == domain
            {
                has_domain = true;
            }
        }

        assert!(has_domain, "SAN should include {}", domain);
    }

    #[test]
    fn test_ca_cert_is_ca() {
        let ca = LocalCA::generate().unwrap();

        // Parse and verify it's a CA certificate
        let (_, cert) = x509_parser::pem::parse_x509_pem(ca.ca_cert_pem.as_bytes()).unwrap();
        let cert = cert.parse_x509().unwrap();

        // Check Basic Constraints
        let bc_ext = cert
            .extensions()
            .iter()
            .find(|ext| ext.oid == x509_parser::oid_registry::OID_X509_EXT_BASIC_CONSTRAINTS);

        assert!(
            bc_ext.is_some(),
            "CA certificate should have Basic Constraints"
        );
    }

    #[test]
    fn test_ca_store_loads_from_file_fallback() {
        let temp_dir = TempDir::new().unwrap();
        let ca_cert_path = temp_dir.path().join("ca").join("ca.crt");

        let store = LocalCAStore {
            ca_cert_path: ca_cert_path.clone(),
            keychain_service: "tako-test-ca-fallback-load".to_string(),
            keychain_account: "tako-test-fallback-load".to_string(),
        };

        let ca = LocalCA::generate().unwrap();
        std::fs::create_dir_all(ca_cert_path.parent().unwrap()).unwrap();
        std::fs::write(&ca_cert_path, ca.ca_cert_pem()).unwrap();
        std::fs::write(ca_cert_path.with_extension("key"), &ca.ca_key_pem).unwrap();

        let loaded = store.load_ca().unwrap();
        assert_eq!(loaded.ca_cert_pem(), ca.ca_cert_pem());
        assert_eq!(loaded.ca_key_pem, ca.ca_key_pem);
    }

    #[test]
    fn test_ca_exists_with_file_fallback_key() {
        let temp_dir = TempDir::new().unwrap();
        let ca_cert_path = temp_dir.path().join("ca").join("ca.crt");

        let store = LocalCAStore {
            ca_cert_path: ca_cert_path.clone(),
            keychain_service: "tako-test-ca-fallback-exists".to_string(),
            keychain_account: "tako-test-fallback-exists".to_string(),
        };

        let ca = LocalCA::generate().unwrap();
        std::fs::create_dir_all(ca_cert_path.parent().unwrap()).unwrap();
        std::fs::write(&ca_cert_path, ca.ca_cert_pem()).unwrap();
        std::fs::write(ca_cert_path.with_extension("key"), &ca.ca_key_pem).unwrap();

        assert!(store.ca_exists());
    }

    #[test]
    fn test_delete_ca_removes_file_fallback_key() {
        let temp_dir = TempDir::new().unwrap();
        let ca_cert_path = temp_dir.path().join("ca").join("ca.crt");
        let ca_key_path = ca_cert_path.with_extension("key");

        let store = LocalCAStore {
            ca_cert_path: ca_cert_path.clone(),
            keychain_service: "tako-test-ca-fallback-delete".to_string(),
            keychain_account: "tako-test-fallback-delete".to_string(),
        };

        let ca = LocalCA::generate().unwrap();
        std::fs::create_dir_all(ca_cert_path.parent().unwrap()).unwrap();
        std::fs::write(&ca_cert_path, ca.ca_cert_pem()).unwrap();
        std::fs::write(&ca_key_path, &ca.ca_key_pem).unwrap();

        store.delete_ca().unwrap();

        assert!(!ca_cert_path.exists());
        assert!(!ca_key_path.exists());
    }

    #[test]
    fn test_load_ca_reads_legacy_filenames() {
        let temp_dir = TempDir::new().unwrap();
        let current_ca_cert_path = temp_dir.path().join("ca").join("ca.crt");
        let legacy_ca_cert_path = temp_dir.path().join("ca").join("tako-ca.crt");

        let store = LocalCAStore {
            ca_cert_path: current_ca_cert_path,
            keychain_service: "tako-test-ca-legacy-load".to_string(),
            keychain_account: "tako-test-legacy-load".to_string(),
        };

        let ca = LocalCA::generate().unwrap();
        std::fs::create_dir_all(legacy_ca_cert_path.parent().unwrap()).unwrap();
        std::fs::write(&legacy_ca_cert_path, ca.ca_cert_pem()).unwrap();
        std::fs::write(legacy_ca_cert_path.with_extension("key"), &ca.ca_key_pem).unwrap();

        let loaded = store.load_ca().unwrap();
        assert_eq!(loaded.ca_cert_pem(), ca.ca_cert_pem());
        assert_eq!(loaded.ca_key_pem, ca.ca_key_pem);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn system_keychain_output_contains_matching_cert() {
        let ca = LocalCA::generate().unwrap();
        let other = LocalCA::generate().unwrap();
        let bundle = format!("{}\n{}", other.ca_cert_pem(), ca.ca_cert_pem());
        assert!(system_keychain_output_contains_cert(
            &bundle,
            ca.ca_cert_pem()
        ));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn system_keychain_output_rejects_non_matching_cert() {
        let ca = LocalCA::generate().unwrap();
        let other = LocalCA::generate().unwrap();
        assert!(!system_keychain_output_contains_cert(
            other.ca_cert_pem(),
            ca.ca_cert_pem()
        ));
    }
}

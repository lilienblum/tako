//! Self-signed certificate generation for development

use std::path::{Path, PathBuf};
use thiserror::Error;

/// Errors that can occur during self-signed cert generation
#[derive(Debug, Error)]
pub enum SelfSignedError {
    #[error("Failed to generate certificate: {0}")]
    GenerationError(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Certificate generation not available: {0}")]
    NotAvailable(String),
}

/// Self-signed certificate for development
#[derive(Debug, Clone)]
pub struct SelfSignedCert {
    /// Path to certificate file (PEM)
    pub cert_path: PathBuf,
    /// Path to private key file (PEM)
    pub key_path: PathBuf,
    /// Domain the cert was generated for
    pub domain: String,
}

impl SelfSignedCert {
    /// Check if certificate files exist
    pub fn exists(&self) -> bool {
        self.cert_path.exists() && self.key_path.exists()
    }
}

/// Generator for self-signed certificates
pub struct SelfSignedGenerator {
    /// Directory to store certificates
    cert_dir: PathBuf,
}

impl SelfSignedGenerator {
    pub fn new(cert_dir: impl Into<PathBuf>) -> Self {
        Self {
            cert_dir: cert_dir.into(),
        }
    }

    /// Get or create a self-signed certificate for localhost
    pub fn get_or_create_localhost(&self) -> Result<SelfSignedCert, SelfSignedError> {
        let cert = SelfSignedCert {
            cert_path: self.cert_dir.join("localhost.crt"),
            key_path: self.cert_dir.join("localhost.key"),
            domain: "localhost".to_string(),
        };

        if cert.exists() {
            return Ok(cert);
        }

        self.generate_localhost(&cert)?;
        Ok(cert)
    }

    /// Generate a self-signed certificate for localhost
    fn generate_localhost(&self, cert: &SelfSignedCert) -> Result<(), SelfSignedError> {
        std::fs::create_dir_all(&self.cert_dir)?;

        // Use rcgen to generate certificate
        use rcgen::{CertificateParams, DistinguishedName, DnType, KeyPair, SanType};

        let mut params = CertificateParams::default();

        // Set subject
        let mut dn = DistinguishedName::new();
        dn.push(DnType::CommonName, "Tako Development");
        dn.push(DnType::OrganizationName, "Tako");
        params.distinguished_name = dn;

        // Add SANs for localhost
        params.subject_alt_names = vec![
            SanType::DnsName("localhost".try_into().unwrap()),
            SanType::DnsName("*.localhost".try_into().unwrap()),
            SanType::IpAddress(std::net::IpAddr::V4(std::net::Ipv4Addr::new(127, 0, 0, 1))),
            SanType::IpAddress(std::net::IpAddr::V6(std::net::Ipv6Addr::LOCALHOST)),
        ];

        // Generate key pair
        let key_pair = KeyPair::generate().map_err(|e| {
            SelfSignedError::GenerationError(format!("Failed to generate key pair: {}", e))
        })?;

        // Generate certificate
        let cert_der = params.self_signed(&key_pair).map_err(|e| {
            SelfSignedError::GenerationError(format!("Failed to generate certificate: {}", e))
        })?;

        // Write certificate
        std::fs::write(&cert.cert_path, cert_der.pem())?;

        // Write private key
        std::fs::write(&cert.key_path, key_pair.serialize_pem())?;

        // Set restrictive permissions on key file
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&cert.key_path, std::fs::Permissions::from_mode(0o600))?;
        }

        tracing::info!(
            cert_path = %cert.cert_path.display(),
            key_path = %cert.key_path.display(),
            "Generated self-signed certificate for localhost"
        );

        Ok(())
    }

    /// Get path to certificate directory
    pub fn cert_dir(&self) -> &Path {
        &self.cert_dir
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_self_signed_cert_exists() {
        let cert = SelfSignedCert {
            cert_path: PathBuf::from("/nonexistent/cert.pem"),
            key_path: PathBuf::from("/nonexistent/key.pem"),
            domain: "localhost".to_string(),
        };
        assert!(!cert.exists());
    }

    #[test]
    fn test_generator_creation() {
        let temp = TempDir::new().unwrap();
        let generator = SelfSignedGenerator::new(temp.path());
        assert_eq!(generator.cert_dir(), temp.path());
    }

    #[test]
    fn test_generate_localhost_cert() {
        let temp = TempDir::new().unwrap();
        let generator = SelfSignedGenerator::new(temp.path());

        let cert = generator.get_or_create_localhost().unwrap();
        assert!(cert.exists());
        assert_eq!(cert.domain, "localhost");

        // Verify files have content
        let cert_content = std::fs::read_to_string(&cert.cert_path).unwrap();
        assert!(cert_content.contains("BEGIN CERTIFICATE"));

        let key_content = std::fs::read_to_string(&cert.key_path).unwrap();
        assert!(key_content.contains("BEGIN PRIVATE KEY"));
    }

    #[test]
    fn test_reuse_existing_cert() {
        let temp = TempDir::new().unwrap();
        let generator = SelfSignedGenerator::new(temp.path());

        // Generate first time
        let cert1 = generator.get_or_create_localhost().unwrap();
        let content1 = std::fs::read_to_string(&cert1.cert_path).unwrap();

        // Get second time (should reuse)
        let cert2 = generator.get_or_create_localhost().unwrap();
        let content2 = std::fs::read_to_string(&cert2.cert_path).unwrap();

        // Should be the same certificate
        assert_eq!(content1, content2);
    }
}

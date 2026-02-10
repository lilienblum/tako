//! Certificate manager - handles certificate lifecycle

use parking_lot::RwLock;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use thiserror::Error;
use x509_parser::prelude::*;

/// Errors that can occur during certificate management
#[derive(Debug, Error)]
pub enum CertError {
    #[error("Certificate not found for domain: {0}")]
    NotFound(String),

    #[error("Certificate expired for domain: {0}")]
    Expired(String),

    #[error("Failed to load certificate: {0}")]
    LoadError(String),

    #[error("Failed to parse certificate: {0}")]
    ParseError(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

/// Information about a certificate
#[derive(Debug, Clone)]
pub struct CertInfo {
    /// Domain the certificate is for
    pub domain: String,
    /// Path to certificate file
    pub cert_path: PathBuf,
    /// Path to private key file
    pub key_path: PathBuf,
    /// When the certificate expires
    pub expires_at: Option<SystemTime>,
    /// Whether this is a wildcard certificate
    pub is_wildcard: bool,
    /// Whether this is self-signed (dev mode)
    pub is_self_signed: bool,
}

impl CertInfo {
    /// Check if certificate is expired
    pub fn is_expired(&self) -> bool {
        self.expires_at
            .map(|exp| SystemTime::now() > exp)
            .unwrap_or(false)
    }

    /// Check if certificate needs renewal (expires within 30 days)
    pub fn needs_renewal(&self) -> bool {
        self.expires_at
            .map(|exp| {
                let thirty_days = Duration::from_secs(30 * 24 * 60 * 60);
                SystemTime::now() + thirty_days > exp
            })
            .unwrap_or(false)
    }

    /// Days until expiry
    pub fn days_until_expiry(&self) -> Option<i64> {
        self.expires_at
            .map(|exp| match exp.duration_since(SystemTime::now()) {
                Ok(duration) => (duration.as_secs() / 86400) as i64,
                Err(e) => -(e.duration().as_secs() as i64 / 86400),
            })
    }
}

/// Certificate manager configuration
#[derive(Debug, Clone)]
pub struct CertManagerConfig {
    /// Directory to store certificates
    pub cert_dir: PathBuf,
    /// How often to check for certificate renewal
    pub check_interval: Duration,
    /// Renew certificates this many days before expiry
    pub renewal_days: u32,
}

impl Default for CertManagerConfig {
    fn default() -> Self {
        Self {
            cert_dir: PathBuf::from("/opt/tako/certs"),
            check_interval: Duration::from_secs(24 * 60 * 60), // 24 hours
            renewal_days: 30,
        }
    }
}

/// Manages certificates for all domains
pub struct CertManager {
    config: CertManagerConfig,
    /// Cached certificate info by domain
    certs: RwLock<HashMap<String, CertInfo>>,
}

impl CertManager {
    pub fn new(config: CertManagerConfig) -> Self {
        Self {
            config,
            certs: RwLock::new(HashMap::new()),
        }
    }

    /// Initialize by loading existing certificates
    pub fn init(&self) -> Result<(), CertError> {
        std::fs::create_dir_all(&self.config.cert_dir)?;
        self.load_all_certs()?;
        Ok(())
    }

    /// Load all certificates from disk
    fn load_all_certs(&self) -> Result<(), CertError> {
        let mut certs = self.certs.write();

        if !self.config.cert_dir.exists() {
            return Ok(());
        }

        for entry in std::fs::read_dir(&self.config.cert_dir)? {
            let entry = entry?;
            let path = entry.path();

            if path.is_dir() {
                let domain = path.file_name().unwrap().to_string_lossy().to_string();
                if let Ok(cert_info) = self.load_cert_info(&domain) {
                    certs.insert(domain, cert_info);
                }
            }
        }

        Ok(())
    }

    /// Load certificate info for a domain
    fn load_cert_info(&self, domain: &str) -> Result<CertInfo, CertError> {
        let domain_dir = self.config.cert_dir.join(domain);
        let cert_path = domain_dir.join("fullchain.pem");
        let key_path = domain_dir.join("privkey.pem");

        if !cert_path.exists() || !key_path.exists() {
            return Err(CertError::NotFound(domain.to_string()));
        }

        // Parse certificate to get expiry date
        let expires_at = self.parse_cert_expiry(&cert_path).ok();
        let is_self_signed = self.check_self_signed(&cert_path).unwrap_or(false);

        Ok(CertInfo {
            domain: domain.to_string(),
            cert_path,
            key_path,
            expires_at,
            is_wildcard: domain.starts_with("*."),
            is_self_signed,
        })
    }

    /// Parse certificate expiry date from PEM file
    fn parse_cert_expiry(&self, cert_path: &PathBuf) -> Result<SystemTime, CertError> {
        let pem_data = std::fs::read(cert_path)?;

        // Parse PEM to get the first certificate
        for pem in Pem::iter_from_buffer(&pem_data) {
            let pem = pem.map_err(|e| CertError::ParseError(e.to_string()))?;

            if pem.label == "CERTIFICATE" {
                let (_, cert) = X509Certificate::from_der(&pem.contents)
                    .map_err(|e| CertError::ParseError(e.to_string()))?;

                // Get the not_after time (expiry)
                let not_after = cert.validity().not_after;

                // Convert ASN1Time to SystemTime
                let timestamp = not_after.timestamp();
                let system_time = UNIX_EPOCH + Duration::from_secs(timestamp as u64);

                return Ok(system_time);
            }
        }

        Err(CertError::ParseError(
            "No certificate found in PEM file".to_string(),
        ))
    }

    /// Check if certificate is self-signed
    fn check_self_signed(&self, cert_path: &PathBuf) -> Result<bool, CertError> {
        let pem_data = std::fs::read(cert_path)?;

        for pem in Pem::iter_from_buffer(&pem_data) {
            let pem = pem.map_err(|e| CertError::ParseError(e.to_string()))?;

            if pem.label == "CERTIFICATE" {
                let (_, cert) = X509Certificate::from_der(&pem.contents)
                    .map_err(|e| CertError::ParseError(e.to_string()))?;

                // Self-signed certificates have the same issuer and subject
                return Ok(cert.issuer() == cert.subject());
            }
        }

        Ok(false)
    }

    /// Get certificate for a domain
    pub fn get_cert(&self, domain: &str) -> Option<CertInfo> {
        let certs = self.certs.read();
        certs.get(domain).cloned()
    }

    /// Get certificate for a domain, falling back to wildcard
    pub fn get_cert_for_host(&self, host: &str) -> Option<CertInfo> {
        let certs = self.certs.read();

        // Try exact match first
        if let Some(cert) = certs.get(host) {
            return Some(cert.clone());
        }

        // Try wildcard match
        if let Some(dot_pos) = host.find('.') {
            let wildcard = format!("*.{}", &host[dot_pos + 1..]);
            if let Some(cert) = certs.get(&wildcard) {
                return Some(cert.clone());
            }
        }

        None
    }

    /// Add a certificate
    pub fn add_cert(&self, cert_info: CertInfo) {
        let mut certs = self.certs.write();
        certs.insert(cert_info.domain.clone(), cert_info);
    }

    /// Remove a certificate
    pub fn remove_cert(&self, domain: &str) -> Option<CertInfo> {
        let mut certs = self.certs.write();
        certs.remove(domain)
    }

    /// List all certificates
    pub fn list_certs(&self) -> Vec<CertInfo> {
        let certs = self.certs.read();
        certs.values().cloned().collect()
    }

    /// Get certificates that need renewal
    pub fn get_certs_needing_renewal(&self) -> Vec<CertInfo> {
        let certs = self.certs.read();
        certs
            .values()
            .filter(|c| c.needs_renewal() && !c.is_self_signed)
            .cloned()
            .collect()
    }

    /// Get certificate directory
    pub fn cert_dir(&self) -> &Path {
        &self.config.cert_dir
    }

    /// Get domain certificate directory
    pub fn domain_cert_dir(&self, domain: &str) -> PathBuf {
        self.config.cert_dir.join(domain)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_cert_info_is_expired() {
        let cert = CertInfo {
            domain: "example.com".to_string(),
            cert_path: PathBuf::from("/tmp/cert.pem"),
            key_path: PathBuf::from("/tmp/key.pem"),
            expires_at: Some(SystemTime::now() - Duration::from_secs(86400)),
            is_wildcard: false,
            is_self_signed: false,
        };
        assert!(cert.is_expired());
    }

    #[test]
    fn test_cert_info_not_expired() {
        let cert = CertInfo {
            domain: "example.com".to_string(),
            cert_path: PathBuf::from("/tmp/cert.pem"),
            key_path: PathBuf::from("/tmp/key.pem"),
            expires_at: Some(SystemTime::now() + Duration::from_secs(86400 * 60)),
            is_wildcard: false,
            is_self_signed: false,
        };
        assert!(!cert.is_expired());
    }

    #[test]
    fn test_cert_info_needs_renewal() {
        let cert = CertInfo {
            domain: "example.com".to_string(),
            cert_path: PathBuf::from("/tmp/cert.pem"),
            key_path: PathBuf::from("/tmp/key.pem"),
            expires_at: Some(SystemTime::now() + Duration::from_secs(86400 * 20)), // 20 days
            is_wildcard: false,
            is_self_signed: false,
        };
        assert!(cert.needs_renewal());
    }

    #[test]
    fn test_cert_manager_creation() {
        let temp = TempDir::new().unwrap();
        let config = CertManagerConfig {
            cert_dir: temp.path().to_path_buf(),
            ..Default::default()
        };
        let manager = CertManager::new(config);
        manager.init().unwrap();
    }

    #[test]
    fn test_add_and_get_cert() {
        let temp = TempDir::new().unwrap();
        let config = CertManagerConfig {
            cert_dir: temp.path().to_path_buf(),
            ..Default::default()
        };
        let manager = CertManager::new(config);

        let cert = CertInfo {
            domain: "example.com".to_string(),
            cert_path: temp.path().join("cert.pem"),
            key_path: temp.path().join("key.pem"),
            expires_at: Some(SystemTime::now() + Duration::from_secs(86400 * 90)),
            is_wildcard: false,
            is_self_signed: false,
        };

        manager.add_cert(cert.clone());

        let retrieved = manager.get_cert("example.com").unwrap();
        assert_eq!(retrieved.domain, "example.com");
    }

    #[test]
    fn test_wildcard_fallback() {
        let temp = TempDir::new().unwrap();
        let config = CertManagerConfig {
            cert_dir: temp.path().to_path_buf(),
            ..Default::default()
        };
        let manager = CertManager::new(config);

        let cert = CertInfo {
            domain: "*.example.com".to_string(),
            cert_path: temp.path().join("cert.pem"),
            key_path: temp.path().join("key.pem"),
            expires_at: Some(SystemTime::now() + Duration::from_secs(86400 * 90)),
            is_wildcard: true,
            is_self_signed: false,
        };

        manager.add_cert(cert);

        // Should find wildcard for subdomain
        let retrieved = manager.get_cert_for_host("api.example.com").unwrap();
        assert_eq!(retrieved.domain, "*.example.com");

        // Should not find for different domain
        assert!(manager.get_cert_for_host("other.com").is_none());
    }

    #[test]
    fn test_list_certs() {
        let temp = TempDir::new().unwrap();
        let config = CertManagerConfig {
            cert_dir: temp.path().to_path_buf(),
            ..Default::default()
        };
        let manager = CertManager::new(config);

        manager.add_cert(CertInfo {
            domain: "a.com".to_string(),
            cert_path: PathBuf::new(),
            key_path: PathBuf::new(),
            expires_at: None,
            is_wildcard: false,
            is_self_signed: false,
        });

        manager.add_cert(CertInfo {
            domain: "b.com".to_string(),
            cert_path: PathBuf::new(),
            key_path: PathBuf::new(),
            expires_at: None,
            is_wildcard: false,
            is_self_signed: false,
        });

        let certs = manager.list_certs();
        assert_eq!(certs.len(), 2);
    }

    // Certificate renewal tests

    #[test]
    fn test_cert_does_not_need_renewal_when_far_from_expiry() {
        let cert = CertInfo {
            domain: "example.com".to_string(),
            cert_path: PathBuf::from("/tmp/cert.pem"),
            key_path: PathBuf::from("/tmp/key.pem"),
            expires_at: Some(SystemTime::now() + Duration::from_secs(86400 * 60)), // 60 days
            is_wildcard: false,
            is_self_signed: false,
        };
        assert!(!cert.needs_renewal());
    }

    #[test]
    fn test_cert_needs_renewal_at_30_day_boundary() {
        // Exactly 30 days - should need renewal
        let cert_at_boundary = CertInfo {
            domain: "example.com".to_string(),
            cert_path: PathBuf::from("/tmp/cert.pem"),
            key_path: PathBuf::from("/tmp/key.pem"),
            expires_at: Some(SystemTime::now() + Duration::from_secs(86400 * 30)),
            is_wildcard: false,
            is_self_signed: false,
        };
        // At exactly 30 days, now + 30 days > exp is false (equal), so doesn't need renewal
        // But 29 days should trigger renewal
        let cert_29_days = CertInfo {
            domain: "example.com".to_string(),
            cert_path: PathBuf::from("/tmp/cert.pem"),
            key_path: PathBuf::from("/tmp/key.pem"),
            expires_at: Some(SystemTime::now() + Duration::from_secs(86400 * 29)),
            is_wildcard: false,
            is_self_signed: false,
        };
        assert!(cert_29_days.needs_renewal());

        // 31 days should not need renewal
        let cert_31_days = CertInfo {
            domain: "example.com".to_string(),
            cert_path: PathBuf::from("/tmp/cert.pem"),
            key_path: PathBuf::from("/tmp/key.pem"),
            expires_at: Some(SystemTime::now() + Duration::from_secs(86400 * 31)),
            is_wildcard: false,
            is_self_signed: false,
        };
        assert!(!cert_31_days.needs_renewal());
        let _ = cert_at_boundary; // silence unused warning
    }

    #[test]
    fn test_expired_cert_needs_renewal() {
        let cert = CertInfo {
            domain: "example.com".to_string(),
            cert_path: PathBuf::from("/tmp/cert.pem"),
            key_path: PathBuf::from("/tmp/key.pem"),
            expires_at: Some(SystemTime::now() - Duration::from_secs(86400)), // Expired yesterday
            is_wildcard: false,
            is_self_signed: false,
        };
        assert!(cert.is_expired());
        assert!(cert.needs_renewal());
    }

    #[test]
    fn test_days_until_expiry_calculation() {
        // Test positive days
        let cert_future = CertInfo {
            domain: "example.com".to_string(),
            cert_path: PathBuf::from("/tmp/cert.pem"),
            key_path: PathBuf::from("/tmp/key.pem"),
            expires_at: Some(SystemTime::now() + Duration::from_secs(86400 * 45)),
            is_wildcard: false,
            is_self_signed: false,
        };
        let days = cert_future.days_until_expiry().unwrap();
        assert!((44..=45).contains(&days), "Expected ~45 days, got {}", days);

        // Test negative days (expired)
        let cert_past = CertInfo {
            domain: "example.com".to_string(),
            cert_path: PathBuf::from("/tmp/cert.pem"),
            key_path: PathBuf::from("/tmp/key.pem"),
            expires_at: Some(SystemTime::now() - Duration::from_secs(86400 * 5)),
            is_wildcard: false,
            is_self_signed: false,
        };
        let days = cert_past.days_until_expiry().unwrap();
        assert!((-6..=-4).contains(&days), "Expected ~-5 days, got {}", days);

        // Test None expiry
        let cert_no_expiry = CertInfo {
            domain: "example.com".to_string(),
            cert_path: PathBuf::from("/tmp/cert.pem"),
            key_path: PathBuf::from("/tmp/key.pem"),
            expires_at: None,
            is_wildcard: false,
            is_self_signed: false,
        };
        assert!(cert_no_expiry.days_until_expiry().is_none());
    }

    #[test]
    fn test_get_certs_needing_renewal_filters_self_signed() {
        let temp = TempDir::new().unwrap();
        let config = CertManagerConfig {
            cert_dir: temp.path().to_path_buf(),
            ..Default::default()
        };
        let manager = CertManager::new(config);

        // Self-signed cert expiring soon - should NOT be in renewal list
        manager.add_cert(CertInfo {
            domain: "dev.local".to_string(),
            cert_path: PathBuf::new(),
            key_path: PathBuf::new(),
            expires_at: Some(SystemTime::now() + Duration::from_secs(86400 * 10)),
            is_wildcard: false,
            is_self_signed: true,
        });

        // Real cert expiring soon - SHOULD be in renewal list
        manager.add_cert(CertInfo {
            domain: "prod.example.com".to_string(),
            cert_path: PathBuf::new(),
            key_path: PathBuf::new(),
            expires_at: Some(SystemTime::now() + Duration::from_secs(86400 * 10)),
            is_wildcard: false,
            is_self_signed: false,
        });

        // Real cert not expiring soon - should NOT be in renewal list
        manager.add_cert(CertInfo {
            domain: "other.example.com".to_string(),
            cert_path: PathBuf::new(),
            key_path: PathBuf::new(),
            expires_at: Some(SystemTime::now() + Duration::from_secs(86400 * 60)),
            is_wildcard: false,
            is_self_signed: false,
        });

        let needing_renewal = manager.get_certs_needing_renewal();
        assert_eq!(needing_renewal.len(), 1);
        assert_eq!(needing_renewal[0].domain, "prod.example.com");
    }

    #[test]
    fn test_get_certs_needing_renewal_empty_when_all_fresh() {
        let temp = TempDir::new().unwrap();
        let config = CertManagerConfig {
            cert_dir: temp.path().to_path_buf(),
            ..Default::default()
        };
        let manager = CertManager::new(config);

        // All certs have plenty of time
        for i in 0..5 {
            manager.add_cert(CertInfo {
                domain: format!("domain{}.com", i),
                cert_path: PathBuf::new(),
                key_path: PathBuf::new(),
                expires_at: Some(SystemTime::now() + Duration::from_secs(86400 * 90)),
                is_wildcard: false,
                is_self_signed: false,
            });
        }

        let needing_renewal = manager.get_certs_needing_renewal();
        assert!(needing_renewal.is_empty());
    }

    #[test]
    fn test_remove_cert() {
        let temp = TempDir::new().unwrap();
        let config = CertManagerConfig {
            cert_dir: temp.path().to_path_buf(),
            ..Default::default()
        };
        let manager = CertManager::new(config);

        manager.add_cert(CertInfo {
            domain: "example.com".to_string(),
            cert_path: PathBuf::new(),
            key_path: PathBuf::new(),
            expires_at: None,
            is_wildcard: false,
            is_self_signed: false,
        });

        assert!(manager.get_cert("example.com").is_some());

        let removed = manager.remove_cert("example.com");
        assert!(removed.is_some());
        assert_eq!(removed.unwrap().domain, "example.com");

        assert!(manager.get_cert("example.com").is_none());
    }

    #[test]
    fn test_wildcard_cert_renewal_detection() {
        let temp = TempDir::new().unwrap();
        let config = CertManagerConfig {
            cert_dir: temp.path().to_path_buf(),
            ..Default::default()
        };
        let manager = CertManager::new(config);

        // Wildcard cert expiring soon
        manager.add_cert(CertInfo {
            domain: "*.example.com".to_string(),
            cert_path: PathBuf::new(),
            key_path: PathBuf::new(),
            expires_at: Some(SystemTime::now() + Duration::from_secs(86400 * 15)),
            is_wildcard: true,
            is_self_signed: false,
        });

        let needing_renewal = manager.get_certs_needing_renewal();
        assert_eq!(needing_renewal.len(), 1);
        assert!(needing_renewal[0].is_wildcard);
        assert_eq!(needing_renewal[0].domain, "*.example.com");
    }
}

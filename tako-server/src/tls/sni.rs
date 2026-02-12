//! SNI-based certificate selection for TLS
//!
//! Implements dynamic certificate selection during TLS handshake
//! based on the SNI (Server Name Indication) hostname.

use super::{CertInfo, CertManager};
use async_trait::async_trait;
use openssl::pkey::PKey;
use openssl::ssl::SslRef;
use openssl::x509::X509;
use pingora_core::listeners::TlsAccept;
use std::sync::Arc;

/// SNI-based certificate resolver that selects certificates based on hostname
pub struct SniCertResolver {
    cert_manager: Arc<CertManager>,
}

impl SniCertResolver {
    /// Create a new SNI certificate resolver
    pub fn new(cert_manager: Arc<CertManager>) -> Self {
        Self { cert_manager }
    }

    /// Load certificate and key from files
    fn load_cert_and_key(
        cert_path: &std::path::Path,
        key_path: &std::path::Path,
    ) -> Result<(X509, PKey<openssl::pkey::Private>), openssl::error::ErrorStack> {
        let cert_pem = std::fs::read(cert_path).map_err(|e| {
            tracing::error!("Failed to read cert file {:?}: {}", cert_path, e);
            openssl::error::ErrorStack::get()
        })?;
        let key_pem = std::fs::read(key_path).map_err(|e| {
            tracing::error!("Failed to read key file {:?}: {}", key_path, e);
            openssl::error::ErrorStack::get()
        })?;

        let cert = X509::from_pem(&cert_pem)?;
        let key = PKey::private_key_from_pem(&key_pem)?;

        Ok((cert, key))
    }

    fn default_cert_info(&self) -> Option<CertInfo> {
        self.cert_manager
            .get_cert("default")
            .or_else(|| self.cert_manager.list_certs().into_iter().next())
    }

    fn set_default_cert(&self, ssl: &mut SslRef, reason: &str) {
        if let Some(cert_info) = self.default_cert_info() {
            match Self::load_cert_and_key(&cert_info.cert_path, &cert_info.key_path) {
                Ok((cert, key)) => {
                    if let Err(e) = ssl.set_certificate(&cert) {
                        tracing::error!("Failed to set default certificate ({reason}): {}", e);
                    }
                    if let Err(e) = ssl.set_private_key(&key) {
                        tracing::error!("Failed to set default private key ({reason}): {}", e);
                    }
                }
                Err(e) => {
                    tracing::error!("Failed to load default certificate ({reason}): {}", e);
                }
            }
        }
    }
}

impl std::fmt::Debug for SniCertResolver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SniCertResolver").finish()
    }
}

#[async_trait]
impl TlsAccept for SniCertResolver {
    async fn certificate_callback(&self, ssl: &mut SslRef) {
        // Get the SNI hostname from the TLS handshake
        let sni_hostname = match ssl.servername(openssl::ssl::NameType::HOST_NAME) {
            Some(name) => name.to_string(),
            None => {
                tracing::warn!("No SNI hostname in TLS handshake");
                if should_allow_default_cert_fallback_for_missing_sni() {
                    self.set_default_cert(ssl, "no-sni");
                }
                return;
            }
        };

        tracing::debug!(hostname = %sni_hostname, "SNI certificate lookup");

        // Look up certificate for this hostname (with wildcard fallback)
        match self.cert_manager.get_cert_for_host(&sni_hostname) {
            Some(cert_info) => {
                tracing::debug!(
                    hostname = %sni_hostname,
                    cert_domain = %cert_info.domain,
                    "Found certificate for hostname"
                );

                match Self::load_cert_and_key(&cert_info.cert_path, &cert_info.key_path) {
                    Ok((cert, key)) => {
                        if let Err(e) = ssl.set_certificate(&cert) {
                            tracing::error!(
                                hostname = %sni_hostname,
                                "Failed to set certificate: {}", e
                            );
                        }
                        if let Err(e) = ssl.set_private_key(&key) {
                            tracing::error!(
                                hostname = %sni_hostname,
                                "Failed to set private key: {}", e
                            );
                        }
                    }
                    Err(e) => {
                        tracing::error!(
                            hostname = %sni_hostname,
                            cert_path = ?cert_info.cert_path,
                            "Failed to load certificate: {}", e
                        );
                    }
                }
            }
            None => {
                if should_allow_default_cert_fallback_for_unknown_sni() {
                    tracing::warn!(
                        hostname = %sni_hostname,
                        "No certificate found for hostname, using default certificate fallback"
                    );
                    self.set_default_cert(ssl, "unknown-sni");
                } else {
                    tracing::warn!(
                        hostname = %sni_hostname,
                        "No certificate found for hostname, TLS handshake may fail"
                    );
                }
            }
        }
    }
}

/// Create TLS callbacks for SNI-based certificate selection
pub fn create_sni_callbacks(cert_manager: Arc<CertManager>) -> Box<dyn TlsAccept + Send + Sync> {
    Box::new(SniCertResolver::new(cert_manager))
}

fn should_allow_default_cert_fallback_for_unknown_sni() -> bool {
    false
}

fn should_allow_default_cert_fallback_for_missing_sni() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_cert_fallback_for_unknown_sni_is_disabled() {
        assert!(!should_allow_default_cert_fallback_for_unknown_sni());
    }

    #[test]
    fn default_cert_fallback_for_missing_sni_is_enabled() {
        assert!(should_allow_default_cert_fallback_for_missing_sni());
    }
}

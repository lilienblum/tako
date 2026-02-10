//! SNI-based certificate selection for TLS
//!
//! Implements dynamic certificate selection during TLS handshake
//! based on the SNI (Server Name Indication) hostname.

use super::CertManager;
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
                tracing::warn!("No SNI hostname in TLS handshake, using default cert");
                // Try to get any available certificate
                if let Some(cert_info) = self.cert_manager.list_certs().into_iter().next() {
                    match Self::load_cert_and_key(&cert_info.cert_path, &cert_info.key_path) {
                        Ok((cert, key)) => {
                            if let Err(e) = ssl.set_certificate(&cert) {
                                tracing::error!("Failed to set default certificate: {}", e);
                            }
                            if let Err(e) = ssl.set_private_key(&key) {
                                tracing::error!("Failed to set default private key: {}", e);
                            }
                        }
                        Err(e) => {
                            tracing::error!("Failed to load default certificate: {}", e);
                        }
                    }
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
                tracing::warn!(
                    hostname = %sni_hostname,
                    "No certificate found for hostname, TLS handshake may fail"
                );
                // The handshake will fail if no certificate is set
                // This is intentional - we don't want to serve the wrong cert
            }
        }
    }
}

/// Create TLS callbacks for SNI-based certificate selection
pub fn create_sni_callbacks(cert_manager: Arc<CertManager>) -> Box<dyn TlsAccept + Send + Sync> {
    Box::new(SniCertResolver::new(cert_manager))
}

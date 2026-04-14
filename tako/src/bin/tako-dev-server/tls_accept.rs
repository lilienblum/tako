use async_trait::async_trait;
use openssl::pkey::PKey;
use openssl::ssl::SslRef;
use openssl::x509::X509;
use pingora_core::listeners::TlsAccept;
use std::collections::HashMap;
use std::sync::Mutex;
use tako::dev::{LocalCA, LocalCAStore};

pub(super) fn load_or_create_ca() -> Result<LocalCA, Box<dyn std::error::Error>> {
    let store = LocalCAStore::new()?;
    Ok(store.get_or_create_ca()?)
}

/// Dynamic TLS certificate resolver for development.
pub(crate) struct DevCertResolver {
    ca: LocalCA,
    cache: Mutex<HashMap<String, (X509, PKey<openssl::pkey::Private>)>>,
}

impl DevCertResolver {
    pub(crate) fn new(ca: LocalCA) -> Self {
        Self {
            ca,
            cache: Mutex::new(HashMap::new()),
        }
    }

    pub(crate) fn get_or_create_cert(
        &self,
        hostname: &str,
    ) -> Option<(X509, PKey<openssl::pkey::Private>)> {
        {
            let cache = self.cache.lock().unwrap();
            if let Some(cached) = cache.get(hostname) {
                return Some(cached.clone());
            }
        }

        let cert = self
            .ca
            .generate_leaf_cert_for_names(&[hostname])
            .map_err(|e| tracing::warn!(hostname, error = %e, "failed to generate dev cert"))
            .ok()?;

        let x509 = X509::from_pem(cert.cert_pem.as_bytes())
            .map_err(|e| tracing::warn!(hostname, error = %e, "failed to parse generated cert"))
            .ok()?;
        let pkey = PKey::private_key_from_pem(cert.key_pem.as_bytes())
            .map_err(|e| tracing::warn!(hostname, error = %e, "failed to parse generated key"))
            .ok()?;

        self.cache
            .lock()
            .unwrap()
            .insert(hostname.to_string(), (x509.clone(), pkey.clone()));
        Some((x509, pkey))
    }
}

#[async_trait]
impl TlsAccept for DevCertResolver {
    async fn certificate_callback(&self, ssl: &mut SslRef) {
        let sni = match ssl.servername(openssl::ssl::NameType::HOST_NAME) {
            Some(name) => name.to_string(),
            None => return,
        };

        if let Some((cert, key)) = self.get_or_create_cert(&sni) {
            let _ = ssl.set_certificate(&cert);
            let _ = ssl.set_private_key(&key);
        }
    }
}

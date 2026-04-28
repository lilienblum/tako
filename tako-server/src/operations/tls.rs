use crate::release::should_use_self_signed_route_cert;
use crate::socket::Response;
use crate::tls::CertInfo;

impl crate::ServerState {
    pub async fn request_certificate(&self, domain: &str) -> Response {
        let acme_guard = self.acme_client.read().await;
        let acme = match acme_guard.as_ref() {
            Some(acme) => acme,
            None => return Response::error("ACME is disabled".to_string()),
        };

        match acme.request_certificate(domain).await {
            Ok(cert) => Response::ok(serde_json::json!({
                "status": "issued",
                "domain": domain,
                "expires_in_days": cert.days_until_expiry(),
                "cert_path": cert.cert_path.to_string_lossy(),
            })),
            Err(e) => Response::error(format!("Certificate request failed: {}", e)),
        }
    }

    pub(crate) async fn ensure_route_certificate(
        &self,
        app_name: &str,
        domain: &str,
    ) -> Option<CertInfo> {
        if let Some(existing) = self.cert_manager.get_cert_for_host(domain) {
            tracing::debug!(domain = %domain, "Certificate already exists");
            return Some(existing);
        }

        if should_use_self_signed_route_cert(domain) {
            match self.cert_manager.get_or_create_self_signed_cert(domain) {
                Ok(cert) => {
                    tracing::info!(
                        domain = %domain,
                        app = app_name,
                        cert_path = %cert.cert_path.display(),
                        "Generated self-signed certificate for private route domain"
                    );
                    return Some(cert);
                }
                Err(e) => {
                    tracing::warn!(
                        domain = %domain,
                        app = app_name,
                        error = %e,
                        "Failed to generate self-signed certificate for private route domain"
                    );
                    return None;
                }
            }
        }

        let acme_guard = self.acme_client.read().await;
        let acme = acme_guard.as_ref()?;

        tracing::info!(domain = %domain, app = app_name, "Requesting certificate for route");
        match acme.request_certificate(domain).await {
            Ok(cert) => {
                tracing::info!(
                    domain = %domain,
                    expires_in_days = cert.days_until_expiry(),
                    "Certificate issued successfully"
                );
                Some(cert)
            }
            Err(e) => {
                tracing::warn!(
                    domain = %domain,
                    error = %e,
                    "Failed to request certificate (HTTPS may not work for this domain)"
                );
                None
            }
        }
    }
}

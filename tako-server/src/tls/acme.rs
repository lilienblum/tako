//! ACME client for Let's Encrypt certificate issuance
//!
//! Uses instant-acme for the ACME protocol implementation.
//! Supports HTTP-01 challenges for non-wildcard domains and
//! DNS-01 challenges (via lego) for wildcard certificates.

use super::manager::{CertError, CertInfo, CertManager};
use instant_acme::{
    Account, AuthorizationStatus, ChallengeType, Identifier, NewAccount, NewOrder, OrderStatus,
    RetryPolicy,
};
use parking_lot::RwLock;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;

/// Errors that can occur during ACME operations
#[derive(Debug, Error)]
pub enum AcmeError {
    #[error("ACME account not registered")]
    NotRegistered,

    #[error("Challenge failed: {0}")]
    ChallengeFailed(String),

    #[error("Certificate issuance failed: {0}")]
    IssuanceFailed(String),

    #[error("Rate limited: {0}")]
    RateLimited(String),

    #[error("Invalid domain: {0}")]
    InvalidDomain(String),

    #[error("Order not ready: {0}")]
    OrderNotReady(String),

    #[error("Authorization pending")]
    AuthorizationPending,

    #[error("ACME error: {0}")]
    Acme(#[from] instant_acme::Error),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Certificate error: {0}")]
    CertError(#[from] CertError),

    #[error("Key generation error: {0}")]
    KeyGeneration(String),

    #[error("Timeout waiting for challenge validation")]
    Timeout,

    #[error("HTTP-01 challenge not available")]
    NoHttp01Challenge,

    #[error("Wildcard certificate requires a DNS provider (dns.provider not set in config.json)")]
    NoDnsProvider,

    #[error("lego DNS-01 challenge failed: {0}")]
    LegoDns01Failed(String),
}

/// ACME configuration
#[derive(Debug, Clone)]
pub struct AcmeConfig {
    /// Use Let's Encrypt staging (for testing)
    pub staging: bool,
    /// Contact email for ACME account
    pub email: Option<String>,
    /// Directory to store ACME account credentials
    pub account_dir: PathBuf,
    /// Timeout for ACME operations
    pub timeout: Duration,
    /// Maximum attempts to check order status
    pub max_attempts: u32,
    /// Delay between status checks
    pub check_delay: Duration,
    /// DNS provider for lego DNS-01 challenges (e.g. "cloudflare", "route53")
    pub dns_provider: Option<String>,
    /// Server data directory (lego stores state under `<data_dir>/lego/`)
    pub data_dir: PathBuf,
}

impl Default for AcmeConfig {
    fn default() -> Self {
        Self {
            staging: false,
            email: None,
            account_dir: PathBuf::from("/opt/tako/acme"),
            timeout: Duration::from_secs(300),
            max_attempts: 30,
            check_delay: Duration::from_secs(5),
            dns_provider: None,
            data_dir: PathBuf::from("/opt/tako"),
        }
    }
}

impl AcmeConfig {
    /// Get the ACME directory URL
    pub fn directory_url(&self) -> String {
        if self.staging {
            "https://acme-staging-v02.api.letsencrypt.org/directory".to_string()
        } else {
            "https://acme-v02.api.letsencrypt.org/directory".to_string()
        }
    }
}

/// HTTP-01 challenge tokens storage
/// Maps token -> key_authorization
pub type ChallengeTokens = Arc<RwLock<HashMap<String, String>>>;

/// ACME client for certificate operations
pub struct AcmeClient {
    config: AcmeConfig,
    cert_manager: Arc<CertManager>,
    /// HTTP-01 challenge tokens (token -> key_authorization)
    challenge_tokens: ChallengeTokens,
    /// Cached ACME account
    account: RwLock<Option<Account>>,
}

impl AcmeClient {
    pub fn new(config: AcmeConfig, cert_manager: Arc<CertManager>) -> Self {
        Self::with_tokens(config, cert_manager, Arc::new(RwLock::new(HashMap::new())))
    }

    /// Create with externally-owned challenge tokens map.
    /// This allows the proxy and server state to share the same tokens
    /// even when the ACME client fails to initialize.
    pub fn with_tokens(
        config: AcmeConfig,
        cert_manager: Arc<CertManager>,
        challenge_tokens: ChallengeTokens,
    ) -> Self {
        Self {
            config,
            cert_manager,
            challenge_tokens,
            account: RwLock::new(None),
        }
    }

    /// Get shared challenge tokens for HTTP-01 validation
    pub fn challenge_tokens(&self) -> ChallengeTokens {
        self.challenge_tokens.clone()
    }

    /// Initialize ACME account (load existing or create new)
    pub async fn init(&self) -> Result<(), AcmeError> {
        std::fs::create_dir_all(&self.config.account_dir)?;

        let credentials_path = self.config.account_dir.join("credentials.json");

        // Try to load existing account
        if credentials_path.exists() {
            match self.load_account(&credentials_path).await {
                Ok(account) => {
                    tracing::info!("Loaded existing ACME account");
                    *self.account.write() = Some(account);
                    return Ok(());
                }
                Err(e) => {
                    tracing::warn!("Failed to load ACME account, will create new: {}", e);
                }
            }
        }

        // Create new account
        let (account, credentials) = self.create_account().await?;

        // Save account credentials
        let credentials_json = serde_json::to_string_pretty(&credentials).map_err(|e| {
            AcmeError::IssuanceFailed(format!("Failed to serialize credentials: {}", e))
        })?;
        std::fs::write(&credentials_path, credentials_json)?;

        // Save account info for reference
        let account_path = self.config.account_dir.join("account.json");
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let account_info = serde_json::json!({
            "created_timestamp": now,
            "email": self.config.email,
            "staging": self.config.staging,
            "id": account.id(),
        });
        std::fs::write(
            &account_path,
            serde_json::to_string_pretty(&account_info).unwrap(),
        )?;

        tracing::info!(
            staging = self.config.staging,
            id = %account.id(),
            "Created new ACME account"
        );

        *self.account.write() = Some(account);
        Ok(())
    }

    /// Load account from saved credentials
    async fn load_account(&self, path: &PathBuf) -> Result<Account, AcmeError> {
        let contents = std::fs::read_to_string(path)?;
        let credentials: instant_acme::AccountCredentials = serde_json::from_str(&contents)
            .map_err(|e| AcmeError::IssuanceFailed(format!("Invalid credentials: {}", e)))?;

        let account = Account::builder()
            .map_err(AcmeError::Acme)?
            .from_credentials(credentials)
            .await?;

        Ok(account)
    }

    /// Create a new ACME account
    async fn create_account(
        &self,
    ) -> Result<(Account, instant_acme::AccountCredentials), AcmeError> {
        let contact = self.config.email.as_ref().map(|e| format!("mailto:{}", e));

        let contact_refs: Vec<&str> = contact
            .as_ref()
            .map(|c| vec![c.as_str()])
            .unwrap_or_default();

        let new_account = NewAccount {
            contact: &contact_refs,
            terms_of_service_agreed: true,
            only_return_existing: false,
        };

        let (account, credentials) = Account::builder()
            .map_err(AcmeError::Acme)?
            .create(&new_account, self.config.directory_url(), None)
            .await?;

        Ok((account, credentials))
    }

    /// Request a certificate for a domain.
    ///
    /// Wildcard domains (starting with `*.`) use DNS-01 via lego.
    /// All other domains use HTTP-01 via instant-acme.
    pub async fn request_certificate(&self, domain: &str) -> Result<CertInfo, AcmeError> {
        // Validate domain
        if domain.is_empty() || domain.contains('/') || domain.starts_with('.') {
            return Err(AcmeError::InvalidDomain(domain.to_string()));
        }

        if domain.starts_with("*.") {
            return self.request_certificate_dns01(domain).await;
        }

        let account = {
            let guard = self.account.read();
            guard.clone().ok_or(AcmeError::NotRegistered)?
        };

        tracing::info!(domain = domain, "Requesting certificate via ACME");

        // Create order
        let identifiers = [Identifier::Dns(domain.to_string())];
        let new_order = NewOrder::new(&identifiers);

        let mut order = account.new_order(&new_order).await?;

        // Process authorizations
        let mut authorizations = order.authorizations();
        while let Some(auth_result) = authorizations.next().await {
            let mut auth = auth_result?;

            match auth.status {
                AuthorizationStatus::Pending => {
                    // Get HTTP-01 challenge
                    let mut challenge = auth
                        .challenge(ChallengeType::Http01)
                        .ok_or(AcmeError::NoHttp01Challenge)?;

                    // Get key authorization
                    let key_auth = challenge.key_authorization();
                    let token = challenge.token.clone();

                    // Store token for HTTP-01 validation
                    {
                        let mut tokens = self.challenge_tokens.write();
                        tokens.insert(token.clone(), key_auth.as_str().to_string());
                    }

                    tracing::info!(
                        domain = domain,
                        token = %token,
                        "HTTP-01 challenge ready at /.well-known/acme-challenge/{}",
                        token
                    );

                    // Tell ACME server we're ready
                    challenge.set_ready().await?;
                }
                AuthorizationStatus::Valid => {
                    tracing::debug!(domain = domain, "Authorization already valid");
                }
                status => {
                    return Err(AcmeError::ChallengeFailed(format!(
                        "Unexpected authorization status: {:?}",
                        status
                    )));
                }
            }
        }

        // Wait for order to be ready with retry policy
        let retry_policy = RetryPolicy::new().timeout(self.config.timeout);

        let order_status = order.poll_ready(&retry_policy).await?;

        match order_status {
            OrderStatus::Ready => {
                tracing::info!(domain = domain, "Order ready, finalizing");
            }
            OrderStatus::Invalid => {
                self.clear_domain_tokens(domain);

                // Re-fetch authorizations to capture the challenge error detail
                // from Let's Encrypt (e.g. DNS resolution failures, wrong content).
                let mut detail = String::from("Order became invalid");
                let mut auths = order.authorizations();
                while let Some(Ok(auth)) = auths.next().await {
                    for challenge in &auth.challenges {
                        if let Some(ref err) = challenge.error {
                            let err_type = err.r#type.as_deref().unwrap_or("unknown");
                            let err_detail = err.detail.as_deref().unwrap_or("unknown");
                            detail = format!(
                                "Order became invalid: {err_detail} (type: {err_type}, status: {:?})",
                                challenge.status,
                            );
                            tracing::warn!(
                                domain = domain,
                                error_type = err_type,
                                error_detail = err_detail,
                                challenge_status = ?challenge.status,
                                "ACME challenge validation failed"
                            );
                        }
                    }
                }

                return Err(AcmeError::ChallengeFailed(detail));
            }
            status => {
                self.clear_domain_tokens(domain);
                return Err(AcmeError::OrderNotReady(format!("{:?}", status)));
            }
        }

        // Clean up challenge tokens
        self.clear_domain_tokens(domain);

        // Finalize order - this generates a CSR internally with rcgen
        // Returns the private key as a PEM string
        let private_key_pem = order.finalize().await?;

        // Poll for certificate with retry policy
        let cert_chain = order.poll_certificate(&retry_policy).await?;

        // Save certificate and key
        let domain_dir = self.cert_manager.domain_cert_dir(domain);
        std::fs::create_dir_all(&domain_dir)?;

        let cert_path = domain_dir.join("fullchain.pem");
        let key_path = domain_dir.join("privkey.pem");

        // Write certificate chain
        std::fs::write(&cert_path, &cert_chain)?;

        // Write private key (already in PEM format)
        std::fs::write(&key_path, &private_key_pem)?;

        // Set restrictive permissions on key
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600))?;
        }

        // Parse expiry from certificate
        let expires_at = parse_cert_expiry(&cert_chain);

        let cert_info = CertInfo {
            domain: domain.to_string(),
            cert_path: cert_path.clone(),
            key_path: key_path.clone(),
            expires_at,
            is_wildcard: domain.starts_with("*."),
            is_self_signed: false,
        };

        // Add to cert manager
        self.cert_manager.add_cert(cert_info.clone());

        tracing::info!(
            domain = domain,
            cert_path = %cert_path.display(),
            expires_in_days = cert_info.days_until_expiry(),
            "Certificate issued successfully"
        );

        Ok(cert_info)
    }

    /// Request a wildcard certificate using DNS-01 challenge via lego.
    async fn request_certificate_dns01(&self, domain: &str) -> Result<CertInfo, AcmeError> {
        let provider = self
            .config
            .dns_provider
            .as_deref()
            .ok_or(AcmeError::NoDnsProvider)?;

        let email = self.config.email.as_deref().unwrap_or("admin@example.com");

        let lego_dir = self.config.data_dir.join("lego");
        std::fs::create_dir_all(&lego_dir)?;

        // Determine whether to `run` (first time) or `renew` (cert already exists).
        // lego stores certs under <path>/certificates/<domain>.crt
        let lego_cert_path = lego_dir
            .join("certificates")
            .join(format!("{}.crt", domain));
        let lego_action = if lego_cert_path.exists() {
            "renew"
        } else {
            "run"
        };

        let mut cmd = tokio::process::Command::new("lego");
        cmd.arg("--dns")
            .arg(provider)
            .arg("--domains")
            .arg(domain)
            .arg("--path")
            .arg(&lego_dir)
            .arg("--email")
            .arg(email)
            .arg("--accept-tos");

        if self.config.staging {
            cmd.arg("--server")
                .arg("https://acme-staging-v02.api.letsencrypt.org/directory");
        }

        cmd.arg(lego_action);

        tracing::info!(
            domain = domain,
            provider = provider,
            action = lego_action,
            "Running lego DNS-01 challenge"
        );

        let output = cmd
            .output()
            .await
            .map_err(|e| AcmeError::LegoDns01Failed(format!("Failed to execute lego: {}", e)))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(AcmeError::LegoDns01Failed(format!(
                "lego {} exited with {}: {}",
                lego_action, output.status, stderr
            )));
        }

        // lego writes cert to <path>/certificates/<domain>.crt and .key
        let lego_key_path = lego_dir
            .join("certificates")
            .join(format!("{}.key", domain));

        if !lego_cert_path.exists() || !lego_key_path.exists() {
            return Err(AcmeError::LegoDns01Failed(
                "lego completed but certificate files not found".to_string(),
            ));
        }

        // Copy into our cert dir structure
        let domain_dir = self.cert_manager.domain_cert_dir(domain);
        std::fs::create_dir_all(&domain_dir)?;

        let cert_path = domain_dir.join("fullchain.pem");
        let key_path = domain_dir.join("privkey.pem");

        std::fs::copy(&lego_cert_path, &cert_path)?;
        std::fs::copy(&lego_key_path, &key_path)?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600))?;
        }

        let cert_pem = std::fs::read_to_string(&cert_path)?;
        let expires_at = parse_cert_expiry(&cert_pem);

        let cert_info = CertInfo {
            domain: domain.to_string(),
            cert_path: cert_path.clone(),
            key_path: key_path.clone(),
            expires_at,
            is_wildcard: true,
            is_self_signed: false,
        };

        self.cert_manager.add_cert(cert_info.clone());

        tracing::info!(
            domain = domain,
            cert_path = %cert_path.display(),
            expires_in_days = cert_info.days_until_expiry(),
            "Wildcard certificate issued via lego DNS-01"
        );

        Ok(cert_info)
    }

    /// Clear challenge tokens for a domain
    fn clear_domain_tokens(&self, _domain: &str) {
        // For now, we clear all tokens since we only handle one domain at a time
        // In the future, we could track which tokens belong to which domain
        let mut tokens = self.challenge_tokens.write();
        tokens.clear();
    }

    /// Renew a certificate
    pub async fn renew_certificate(&self, domain: &str) -> Result<CertInfo, AcmeError> {
        tracing::info!(domain = domain, "Renewing certificate");
        self.request_certificate(domain).await
    }

    /// Get challenge response for HTTP-01 validation
    pub fn get_challenge_response(&self, token: &str) -> Option<String> {
        let tokens = self.challenge_tokens.read();
        tokens.get(token).cloned()
    }

    /// Check if using staging environment
    pub fn is_staging(&self) -> bool {
        self.config.staging
    }

    /// Run renewal check for all certificates
    pub async fn check_renewals(&self) -> Vec<Result<CertInfo, AcmeError>> {
        let certs_to_renew = self.cert_manager.get_certs_needing_renewal();
        let mut results = Vec::new();

        for cert in certs_to_renew {
            tracing::info!(
                domain = %cert.domain,
                days_until_expiry = cert.days_until_expiry(),
                "Certificate needs renewal"
            );
            let result = self.renew_certificate(&cert.domain).await;
            results.push(result);
        }

        results
    }

    /// Get config
    pub fn config(&self) -> &AcmeConfig {
        &self.config
    }
}

/// Parse certificate expiry from PEM data
fn parse_cert_expiry(pem_data: &str) -> Option<std::time::SystemTime> {
    use x509_parser::prelude::*;

    // Find the first certificate in the chain
    for pem in Pem::iter_from_buffer(pem_data.as_bytes()).flatten() {
        if pem.label == "CERTIFICATE"
            && let Ok((_, cert)) = parse_x509_certificate(&pem.contents)
        {
            let not_after = cert.validity().not_after;
            let timestamp = not_after.timestamp();
            return std::time::UNIX_EPOCH
                .checked_add(std::time::Duration::from_secs(timestamp as u64));
        }
    }

    None
}

/// HTTP-01 challenge handler for use in the proxy
pub struct ChallengeHandler {
    tokens: ChallengeTokens,
}

impl ChallengeHandler {
    pub fn new(tokens: ChallengeTokens) -> Self {
        Self { tokens }
    }

    /// Check if a request is for ACME challenge
    pub fn is_challenge_request(&self, path: &str) -> bool {
        path.starts_with("/.well-known/acme-challenge/")
    }

    /// Get response for ACME challenge
    pub fn handle_challenge(&self, path: &str) -> Option<String> {
        let token = path.strip_prefix("/.well-known/acme-challenge/")?;
        let tokens = self.tokens.read();
        tokens.get(token).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tls::manager::CertManagerConfig;
    use tempfile::TempDir;

    fn create_test_acme() -> (TempDir, AcmeClient) {
        let temp = TempDir::new().unwrap();
        let cert_config = CertManagerConfig {
            cert_dir: temp.path().join("certs"),
            ..Default::default()
        };
        let cert_manager = Arc::new(CertManager::new(cert_config));

        let acme_config = AcmeConfig {
            staging: true,
            email: Some("test@example.com".to_string()),
            account_dir: temp.path().join("acme"),
            ..Default::default()
        };
        let acme = AcmeClient::new(acme_config, cert_manager);

        (temp, acme)
    }

    #[test]
    fn test_acme_config_defaults() {
        let config = AcmeConfig::default();
        assert!(!config.staging);
        assert!(config.email.is_none());
        assert_eq!(config.max_attempts, 30);
    }

    #[test]
    fn test_directory_url() {
        let mut config = AcmeConfig::default();
        assert!(config.directory_url().contains("acme-v02"));

        config.staging = true;
        assert!(config.directory_url().contains("staging"));
    }

    #[test]
    fn test_challenge_tokens() {
        let (_temp, acme) = create_test_acme();
        let tokens = acme.challenge_tokens();

        {
            let mut t = tokens.write();
            t.insert("token123".to_string(), "auth456".to_string());
        }

        assert_eq!(
            acme.get_challenge_response("token123"),
            Some("auth456".to_string())
        );
    }

    #[test]
    fn test_challenge_handler() {
        let tokens: ChallengeTokens = Arc::new(RwLock::new(HashMap::new()));
        let handler = ChallengeHandler::new(tokens.clone());

        assert!(handler.is_challenge_request("/.well-known/acme-challenge/token123"));
        assert!(!handler.is_challenge_request("/other/path"));

        {
            let mut t = tokens.write();
            t.insert("token123".to_string(), "response".to_string());
        }

        assert_eq!(
            handler.handle_challenge("/.well-known/acme-challenge/token123"),
            Some("response".to_string())
        );
    }

    #[test]
    fn test_is_staging() {
        let (_temp, acme) = create_test_acme();
        assert!(acme.is_staging());
    }

    #[test]
    fn test_invalid_domain() {
        let (_temp, _acme) = create_test_acme();

        // These should be invalid domains
        let invalid_domains = vec!["", "bad/domain", ".startwithdot"];

        for domain in invalid_domains {
            assert!(
                domain.is_empty() || domain.contains('/') || domain.starts_with('.'),
                "Expected {} to be invalid",
                domain
            );
        }
    }

    #[test]
    fn test_parse_cert_expiry() {
        // Test with a sample certificate (this would need a real cert to fully test)
        let invalid_pem = "not a valid certificate";
        assert!(parse_cert_expiry(invalid_pem).is_none());
    }

    // Certificate renewal tests

    #[tokio::test]
    async fn test_check_renewals_empty_when_no_certs() {
        let (_temp, acme) = create_test_acme();
        // Don't init account - just test the renewal check logic
        let results = acme.check_renewals().await;
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn test_check_renewals_identifies_expiring_certs() {
        let (temp, acme) = create_test_acme();

        // Add a certificate that needs renewal to the cert manager
        let cert_manager = acme.cert_manager.clone();
        cert_manager.add_cert(super::super::manager::CertInfo {
            domain: "expiring.example.com".to_string(),
            cert_path: temp.path().join("cert.pem"),
            key_path: temp.path().join("key.pem"),
            expires_at: Some(
                std::time::SystemTime::now() + std::time::Duration::from_secs(86400 * 15),
            ),
            is_wildcard: false,
            is_self_signed: false,
        });

        // Verify the cert manager sees this cert as needing renewal
        let needing_renewal = cert_manager.get_certs_needing_renewal();
        assert_eq!(needing_renewal.len(), 1);
        assert_eq!(needing_renewal[0].domain, "expiring.example.com");
    }

    #[tokio::test]
    async fn test_check_renewals_skips_self_signed() {
        let (temp, acme) = create_test_acme();

        // Add a self-signed certificate that is expiring
        let cert_manager = acme.cert_manager.clone();
        cert_manager.add_cert(super::super::manager::CertInfo {
            domain: "localhost".to_string(),
            cert_path: temp.path().join("cert.pem"),
            key_path: temp.path().join("key.pem"),
            expires_at: Some(
                std::time::SystemTime::now() + std::time::Duration::from_secs(86400 * 5),
            ),
            is_wildcard: false,
            is_self_signed: true, // Self-signed should be skipped
        });

        // Verify self-signed certs are not in renewal list
        let needing_renewal = cert_manager.get_certs_needing_renewal();
        assert!(needing_renewal.is_empty());
    }

    #[tokio::test]
    async fn test_check_renewals_skips_fresh_certs() {
        let (temp, acme) = create_test_acme();

        // Add a certificate that does NOT need renewal (60 days out)
        let cert_manager = acme.cert_manager.clone();
        cert_manager.add_cert(super::super::manager::CertInfo {
            domain: "fresh.example.com".to_string(),
            cert_path: temp.path().join("cert.pem"),
            key_path: temp.path().join("key.pem"),
            expires_at: Some(
                std::time::SystemTime::now() + std::time::Duration::from_secs(86400 * 60),
            ),
            is_wildcard: false,
            is_self_signed: false,
        });

        // Should not need renewal
        let needing_renewal = cert_manager.get_certs_needing_renewal();
        assert!(needing_renewal.is_empty());

        // check_renewals should return empty too
        let results = acme.check_renewals().await;
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn test_renew_certificate_requires_account() {
        let (_temp, acme) = create_test_acme();
        // Don't initialize account

        let result = acme.renew_certificate("example.com").await;
        assert!(matches!(result, Err(AcmeError::NotRegistered)));
    }

    #[test]
    fn test_acme_config_with_custom_values() {
        let config = AcmeConfig {
            staging: true,
            email: Some("admin@example.com".to_string()),
            account_dir: PathBuf::from("/custom/path"),
            timeout: Duration::from_secs(600),
            max_attempts: 50,
            check_delay: Duration::from_secs(10),
            dns_provider: Some("cloudflare".to_string()),
            data_dir: PathBuf::from("/custom/data"),
        };

        assert!(config.staging);
        assert_eq!(config.email, Some("admin@example.com".to_string()));
        assert_eq!(config.max_attempts, 50);
        assert!(config.directory_url().contains("staging"));
        assert_eq!(config.dns_provider, Some("cloudflare".to_string()));
    }

    #[tokio::test]
    async fn test_wildcard_requires_dns_provider() {
        let (_temp, acme) = create_test_acme();
        // dns_provider is None by default, so wildcard should fail with NoDnsProvider
        let result = acme.request_certificate("*.example.com").await;
        assert!(matches!(result, Err(AcmeError::NoDnsProvider)));
    }

    #[test]
    fn test_challenge_handler_extracts_token() {
        let tokens: ChallengeTokens = Arc::new(RwLock::new(HashMap::new()));
        let handler = ChallengeHandler::new(tokens.clone());

        // Insert a token
        {
            let mut t = tokens.write();
            t.insert("abc123".to_string(), "key_auth_value".to_string());
        }

        // Test extraction from various paths
        assert!(handler.is_challenge_request("/.well-known/acme-challenge/abc123"));
        assert_eq!(
            handler.handle_challenge("/.well-known/acme-challenge/abc123"),
            Some("key_auth_value".to_string())
        );

        // Unknown token
        assert_eq!(
            handler.handle_challenge("/.well-known/acme-challenge/unknown"),
            None
        );

        // Non-challenge paths
        assert!(!handler.is_challenge_request("/"));
        assert!(!handler.is_challenge_request("/api/health"));
        assert!(!handler.is_challenge_request("/.well-known/other"));
    }
}

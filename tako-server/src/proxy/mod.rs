//! HTTP/HTTPS Proxy using Pingora
//!
//! Routes incoming HTTP requests to app instances based on Host header.
//! Supports TLS termination with automatic certificate management.
//! Handles ACME HTTP-01 challenges for Let's Encrypt certificate issuance.
//! Exposes `/_tako/status` endpoint for health monitoring.

mod static_files;

#[allow(unused_imports)]
pub use static_files::*;

use crate::lb::{Backend, LoadBalancer};
use crate::routing::RouteTable;
use crate::scaling::ColdStartManager;
use crate::socket::{AppState, AppStatus, BuildStatus, InstanceState, InstanceStatus};
use crate::tls::{
    CertInfo, CertManager, ChallengeHandler, ChallengeTokens, SelfSignedGenerator,
    create_sni_callbacks,
};
use async_trait::async_trait;
use pingora_core::listeners::tls::TlsSettings;
use pingora_core::prelude::*;
use pingora_core::upstreams::peer::HttpPeer;
use pingora_http::{RequestHeader, ResponseHeader};
use pingora_proxy::{ProxyHttp, Session};
use serde::Serialize;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Server status response for /_tako/status endpoint
#[derive(Debug, Clone, Serialize)]
pub struct ServerStatus {
    /// Overall server health (healthy if at least one app has healthy instances)
    pub healthy: bool,
    /// List of all apps and their statuses
    pub apps: Vec<AppStatus>,
    /// Uptime in seconds (if available)
    pub uptime_secs: Option<u64>,
}

/// Proxy configuration
#[derive(Debug, Clone)]
pub struct ProxyConfig {
    /// HTTP port to listen on
    pub http_port: u16,
    /// HTTPS port to listen on
    pub https_port: u16,
    /// Whether to enable HTTPS
    pub enable_https: bool,
    /// Whether to use self-signed certs for development
    pub dev_mode: bool,
    /// Directory for certificates
    pub cert_dir: PathBuf,
    /// Whether to redirect HTTP to HTTPS
    pub redirect_http_to_https: bool,
}

impl Default for ProxyConfig {
    fn default() -> Self {
        Self {
            http_port: 80,
            https_port: 443,
            enable_https: true,
            dev_mode: false,
            cert_dir: PathBuf::from("/opt/tako/certs"),
            redirect_http_to_https: true,
        }
    }
}

impl ProxyConfig {
    /// Create development configuration
    pub fn development() -> Self {
        Self {
            http_port: 8080,
            https_port: 8443,
            enable_https: true,
            dev_mode: true,
            cert_dir: PathBuf::from("./data/certs"),
            redirect_http_to_https: true,
        }
    }
}

/// Tako HTTP proxy service
pub struct TakoProxy {
    /// Load balancer
    lb: Arc<LoadBalancer>,
    /// Route table (app_name -> route patterns)
    routes: Arc<RwLock<RouteTable>>,
    /// Configuration
    config: ProxyConfig,
    /// ACME challenge handler (optional)
    challenge_handler: Option<ChallengeHandler>,

    /// Cold start coordinator for on-demand apps
    cold_start: Arc<ColdStartManager>,
}

impl TakoProxy {
    pub fn new(
        lb: Arc<LoadBalancer>,
        routes: Arc<RwLock<RouteTable>>,
        config: ProxyConfig,
        cold_start: Arc<ColdStartManager>,
    ) -> Self {
        Self {
            lb,
            routes,
            config,
            challenge_handler: None,
            cold_start,
        }
    }

    /// Create proxy with ACME challenge handling
    pub fn with_acme(
        lb: Arc<LoadBalancer>,
        routes: Arc<RwLock<RouteTable>>,
        config: ProxyConfig,
        tokens: ChallengeTokens,
        cold_start: Arc<ColdStartManager>,
    ) -> Self {
        Self {
            lb,
            routes,
            config,
            challenge_handler: Some(ChallengeHandler::new(tokens)),
            cold_start,
        }
    }

    /// Get config
    pub fn config(&self) -> &ProxyConfig {
        &self.config
    }

    /// Generate server status for /_tako/status endpoint
    fn get_server_status(&self) -> ServerStatus {
        let app_manager = self.lb.app_manager();
        let app_names = app_manager.list_apps();

        let mut apps = Vec::new();
        let mut has_healthy = false;

        for name in app_names {
            if let Some(app) = app_manager.get_app(&name) {
                let instances: Vec<InstanceStatus> =
                    app.get_instances().iter().map(|i| i.status()).collect();

                let healthy_count = instances
                    .iter()
                    .filter(|i| i.state == crate::socket::InstanceState::Healthy)
                    .count();

                if healthy_count > 0 {
                    has_healthy = true;
                }

                apps.push(AppStatus {
                    name: app.name(),
                    version: app.version(),
                    state: app.state(),
                    instances,
                    builds: collect_build_statuses(&app),
                    last_error: app.last_error(),
                });
            }
        }

        ServerStatus {
            healthy: has_healthy || apps.is_empty(), // Empty server is considered healthy
            apps,
            uptime_secs: None, // Could track server start time if needed
        }
    }
}

fn collect_build_statuses(app: &crate::instances::App) -> Vec<BuildStatus> {
    let mut instances_by_build: std::collections::HashMap<String, Vec<InstanceStatus>> =
        std::collections::HashMap::new();
    for instance in app.get_instances() {
        instances_by_build
            .entry(instance.build_version().to_string())
            .or_default()
            .push(instance.status());
    }

    let mut builds: Vec<BuildStatus> = instances_by_build
        .into_iter()
        .map(|(version, instances)| BuildStatus {
            state: derive_build_state(&instances),
            version,
            instances,
        })
        .collect();

    let current_version = app.version();
    builds.sort_by(|a, b| a.version.cmp(&b.version));
    if let Some(index) = builds.iter().position(|b| b.version == current_version) {
        let current = builds.remove(index);
        builds.insert(0, current);
    }

    builds
}

fn derive_build_state(instances: &[InstanceStatus]) -> AppState {
    if instances
        .iter()
        .any(|i| i.state == InstanceState::Healthy || i.state == InstanceState::Ready)
    {
        return AppState::Running;
    }
    if instances
        .iter()
        .any(|i| i.state == InstanceState::Starting || i.state == InstanceState::Draining)
    {
        return AppState::Deploying;
    }
    if instances
        .iter()
        .any(|i| i.state == InstanceState::Unhealthy)
    {
        return AppState::Error;
    }
    AppState::Stopped
}

/// Request context for tracking which backend is serving
pub struct RequestCtx {
    backend: Option<Backend>,
    is_https: bool,
    /// Set if this is an ACME challenge response
    acme_response: Option<String>,
}

#[async_trait]
impl ProxyHttp for TakoProxy {
    type CTX = RequestCtx;

    fn new_ctx(&self) -> Self::CTX {
        RequestCtx {
            backend: None,
            is_https: false,
            acme_response: None,
        }
    }

    async fn request_filter(&self, session: &mut Session, ctx: &mut Self::CTX) -> Result<bool> {
        let path = session.req_header().uri.path();

        // Handle ACME HTTP-01 challenges
        if let Some(ref handler) = self.challenge_handler
            && handler.is_challenge_request(path)
        {
            if let Some(response) = handler.handle_challenge(path) {
                tracing::info!(path = path, "Serving ACME challenge response");
                ctx.acme_response = Some(response);
                return Ok(true); // Skip upstream, we'll handle in response
            } else {
                tracing::warn!(path = path, "ACME challenge token not found");
                // Return 404 for unknown challenge tokens
                let mut header = ResponseHeader::build(404, None)?;
                header.insert_header("Content-Type", "text/plain")?;
                session
                    .write_response_header(Box::new(header), false)
                    .await?;
                session
                    .write_response_body(Some("Token not found".into()), true)
                    .await?;
                return Ok(true);
            }
        }

        // Handle /_tako/status endpoint for health monitoring
        if path == "/_tako/status" {
            let status = self.get_server_status();
            let status_code = if status.healthy { 200 } else { 503 };

            let body = serde_json::to_string_pretty(&status).unwrap_or_else(|_| {
                r#"{"healthy":false,"apps":[],"error":"serialization failed"}"#.to_string()
            });

            let mut header = ResponseHeader::build(status_code, None)?;
            header.insert_header("Content-Type", "application/json")?;
            header.insert_header("Cache-Control", "no-cache, no-store")?;
            session
                .write_response_header(Box::new(header), false)
                .await?;
            session.write_response_body(Some(body.into()), true).await?;

            tracing::debug!(
                healthy = status.healthy,
                app_count = status.apps.len(),
                "Served /_tako/status"
            );
            return Ok(true);
        }

        // Handle HTTP to HTTPS redirect
        let host = session
            .req_header()
            .headers
            .get("host")
            .and_then(|h| h.to_str().ok())
            .unwrap_or("");
        let hostname = host.split(':').next().unwrap_or(host);

        // Handle HTTP to HTTPS redirect.
        // Allow ACME challenges and status endpoint on HTTP.
        if !path.starts_with("/.well-known/acme-challenge/") && path != "/_tako/status" {
            let is_https = session
                .digest()
                .map(|d| d.ssl_digest.is_some())
                .unwrap_or(false);

            if should_redirect_http_request(is_https, self.config.redirect_http_to_https) {
                let redirect_url = format!("https://{}{}", host, path);

                let mut header = ResponseHeader::build(301, None)?;
                header.insert_header("Location", &redirect_url)?;
                header.insert_header("Content-Type", "text/plain")?;
                session
                    .write_response_header(Box::new(header), false)
                    .await?;
                session
                    .write_response_body(Some("Redirecting to HTTPS".into()), true)
                    .await?;
                return Ok(true);
            }
        }

        // Route request to app based on host/path
        let app_name = match self.routes.read().await.select(hostname, path) {
            Some(app) => app,
            None => {
                let mut header = ResponseHeader::build(404, None)?;
                header.insert_header("Content-Type", "text/plain")?;
                session
                    .write_response_header(Box::new(header), false)
                    .await?;
                session
                    .write_response_body(Some("No route".into()), true)
                    .await?;
                return Ok(true);
            }
        };

        // Try to get a healthy backend. If none exists and the app is on-demand
        // (instances=0), attempt a cold start and wait for a healthy instance.
        let backend = self.lb.get_backend(&app_name);

        if backend.is_none()
            && let Some(app) = self.lb.app_manager().get_app(&app_name)
        {
            let min_instances = app.config.read().min_instances;
            if min_instances == 0 {
                let begin = self.cold_start.begin(&app_name);
                if begin.leader {
                    app.set_state(crate::socket::AppState::Running);

                    let app_name = app_name.clone();
                    let app = app.clone();
                    let spawner = self.lb.app_manager().spawner();
                    let cold_start = self.cold_start.clone();

                    tokio::spawn(async move {
                        let instance = app.allocate_instance();
                        if let Err(e) = spawner.spawn(&app, instance.clone()).await {
                            tracing::error!(app = %app_name, "cold start spawn failed: {}", e);
                            app.set_state(crate::socket::AppState::Error);
                            app.set_last_error(format!("Cold start failed: {}", e));
                            app.remove_instance(instance.id);
                            cold_start.mark_failed(&app_name);
                        }
                    });
                }

                let mut header = ResponseHeader::build(503, None)?;
                header.insert_header("Content-Type", "text/plain")?;
                header.insert_header("Retry-After", "1")?;
                session
                    .write_response_header(Box::new(header), false)
                    .await?;
                session
                    .write_response_body(Some("App is starting".into()), true)
                    .await?;
                return Ok(true);
            }
        }

        let backend = backend.ok_or_else(|| Error::new(ErrorType::ConnectNoRoute))?;
        ctx.backend = Some(backend);

        Ok(false)
    }

    async fn upstream_peer(
        &self,
        session: &mut Session,
        ctx: &mut Self::CTX,
    ) -> Result<Box<HttpPeer>> {
        // If we have an ACME response, we need to handle it specially
        // This shouldn't be called if request_filter returned true
        if ctx.acme_response.is_some() {
            return Err(Error::new(ErrorType::InternalError));
        }

        // Check if this is an HTTPS connection
        ctx.is_https = session
            .digest()
            .map(|d| d.ssl_digest.is_some())
            .unwrap_or(false);

        let backend = ctx
            .backend
            .clone()
            .ok_or_else(|| Error::new(ErrorType::ConnectNoRoute))?;

        let (host, port) = backend.host_port();
        let peer = HttpPeer::new((host.to_string(), port), false, String::new());
        Ok(Box::new(peer))
    }

    async fn response_filter(
        &self,
        session: &mut Session,
        _upstream_response: &mut ResponseHeader,
        ctx: &mut Self::CTX,
    ) -> Result<()> {
        // Handle ACME response if we have one
        if let Some(ref response) = ctx.acme_response {
            let mut header = ResponseHeader::build(200, None)?;
            header.insert_header("Content-Type", "text/plain")?;
            header.insert_header("Content-Length", response.len().to_string())?;
            session
                .write_response_header(Box::new(header), false)
                .await?;
            session
                .write_response_body(Some(response.clone().into()), true)
                .await?;
        }
        Ok(())
    }

    async fn upstream_request_filter(
        &self,
        _session: &mut Session,
        upstream_request: &mut RequestHeader,
        ctx: &mut Self::CTX,
    ) -> Result<()> {
        // Add X-Forwarded headers
        let proto = if ctx.is_https { "https" } else { "http" };
        upstream_request
            .insert_header("X-Forwarded-Proto", proto)
            .unwrap();

        // Track the request on the instance
        if let Some(ref backend) = ctx.backend
            && let Some(app) = self.lb.app_manager().get_app(&backend.app_name)
            && let Some(instance) = app.get_instance(backend.instance_id)
        {
            instance.request_started();
        }

        Ok(())
    }

    async fn logging(&self, session: &mut Session, _e: Option<&Error>, ctx: &mut Self::CTX) {
        // Mark connection completed in load balancer
        if let Some(ref backend) = ctx.backend {
            self.lb
                .request_completed(&backend.app_name, backend.instance_id);

            if let Some(app) = self.lb.app_manager().get_app(&backend.app_name)
                && let Some(instance) = app.get_instance(backend.instance_id)
            {
                instance.request_finished();
            }
        }

        // Log request
        let status = session
            .response_written()
            .map(|r| r.status.as_u16())
            .unwrap_or(0);

        let host = session
            .req_header()
            .headers
            .get("host")
            .and_then(|h| h.to_str().ok())
            .unwrap_or("-");

        let path = session.req_header().uri.path();
        let method = session.req_header().method.as_str();

        tracing::info!(
            host = host,
            method = method,
            path = path,
            status = status,
            https = ctx.is_https,
            "Request completed"
        );
    }
}

fn should_redirect_http_request(is_https: bool, redirect_http_to_https: bool) -> bool {
    redirect_http_to_https && !is_https
}

/// TLS configuration for the proxy
pub struct TlsConfig {
    /// Certificate manager
    cert_manager: Arc<CertManager>,
    /// Self-signed generator for dev mode
    self_signed: Option<SelfSignedGenerator>,
}

impl TlsConfig {
    /// Create TLS config with certificate manager
    pub fn new(cert_manager: Arc<CertManager>) -> Self {
        Self {
            cert_manager,
            self_signed: None,
        }
    }

    /// Create TLS config for development with self-signed certs
    pub fn development(cert_dir: PathBuf) -> Self {
        Self {
            cert_manager: Arc::new(CertManager::new(crate::tls::CertManagerConfig {
                cert_dir: cert_dir.clone(),
                ..Default::default()
            })),
            self_signed: Some(SelfSignedGenerator::new(cert_dir)),
        }
    }

    /// Get or create certificate for a domain
    pub fn get_cert(&self, domain: &str) -> Option<CertInfo> {
        // Try to get from manager first
        if let Some(cert) = self.cert_manager.get_cert_for_host(domain) {
            return Some(cert);
        }

        // In dev mode, generate self-signed if not found
        if let Some(ref generator) = self.self_signed
            && (domain == "localhost" || domain.ends_with(".localhost"))
            && let Ok(self_signed) = generator.get_or_create_localhost()
        {
            return Some(CertInfo {
                domain: domain.to_string(),
                cert_path: self_signed.cert_path,
                key_path: self_signed.key_path,
                expires_at: None,
                is_wildcard: false,
                is_self_signed: true,
            });
        }

        None
    }

    /// Get default certificate (for SNI fallback)
    pub fn get_default_cert(&self) -> Option<CertInfo> {
        // Try localhost first for dev mode
        if let Some(cert) = self.get_cert("localhost") {
            return Some(cert);
        }

        // Return first available cert
        self.cert_manager.list_certs().into_iter().next()
    }
}

/// Build and start the Pingora server
pub fn build_server(
    lb: Arc<LoadBalancer>,
    config: ProxyConfig,
    cold_start: Arc<ColdStartManager>,
) -> Result<Server> {
    build_server_with_acme(
        lb,
        Arc::new(RwLock::new(RouteTable::default())),
        config,
        None,
        None,
        cold_start,
    )
}

/// Build and start the Pingora server with ACME and SNI support
pub fn build_server_with_acme(
    lb: Arc<LoadBalancer>,
    routes: Arc<RwLock<RouteTable>>,
    config: ProxyConfig,
    acme_tokens: Option<ChallengeTokens>,
    cert_manager: Option<Arc<CertManager>>,
    cold_start: Arc<ColdStartManager>,
) -> Result<Server> {
    let mut server = Server::new(None)?;
    server.bootstrap();

    let proxy = if let Some(tokens) = acme_tokens {
        TakoProxy::with_acme(lb, routes.clone(), config.clone(), tokens, cold_start)
    } else {
        TakoProxy::new(lb, routes.clone(), config.clone(), cold_start)
    };

    let mut proxy_service = pingora_proxy::http_proxy_service(&server.configuration, proxy);

    // Add HTTP listener
    proxy_service.add_tcp(&format!("0.0.0.0:{}", config.http_port));

    // Add HTTPS listener if enabled
    if config.enable_https {
        if let Some(tls_settings) = create_tls_settings(&config, cert_manager)? {
            proxy_service.add_tls_with_settings(
                &format!("0.0.0.0:{}", config.https_port),
                None,
                tls_settings,
            );
            tracing::info!(port = config.https_port, "HTTPS listener enabled");
        } else {
            tracing::warn!("HTTPS enabled but no certificates available");
        }
    }

    server.add_service(proxy_service);
    Ok(server)
}

/// Create TLS settings from configuration
fn create_tls_settings(
    config: &ProxyConfig,
    cert_manager: Option<Arc<CertManager>>,
) -> Result<Option<TlsSettings>> {
    // Ensure cert directory exists
    std::fs::create_dir_all(&config.cert_dir).map_err(|e| {
        Error::explain(
            ErrorType::InternalError,
            format!("Failed to create cert directory: {}", e),
        )
    })?;

    if config.dev_mode {
        // Dev mode: use static self-signed certificate
        let generator = SelfSignedGenerator::new(&config.cert_dir);
        let cert = generator.get_or_create_localhost().map_err(|e| {
            Error::explain(
                ErrorType::InternalError,
                format!("Failed to generate self-signed cert: {}", e),
            )
        })?;

        let cert_path_str = cert.cert_path.to_string_lossy().to_string();
        let key_path_str = cert.key_path.to_string_lossy().to_string();

        let mut tls_settings =
            TlsSettings::intermediate(&cert_path_str, &key_path_str).map_err(|e| {
                Error::explain(
                    ErrorType::InternalError,
                    format!("Failed to create TLS settings: {}", e),
                )
            })?;

        tls_settings.enable_h2();

        tracing::info!(
            cert_path = %cert.cert_path.display(),
            "Loaded self-signed TLS certificate (dev mode)"
        );

        Ok(Some(tls_settings))
    } else if let Some(cm) = cert_manager {
        // Production mode: use SNI-based certificate selection
        let callbacks = create_sni_callbacks(cm);

        let mut tls_settings = TlsSettings::with_callbacks(callbacks).map_err(|e| {
            Error::explain(
                ErrorType::InternalError,
                format!("Failed to create TLS settings with SNI callbacks: {}", e),
            )
        })?;

        tls_settings.enable_h2();

        tracing::info!("TLS enabled with SNI-based certificate selection");

        Ok(Some(tls_settings))
    } else {
        // No cert manager, try fallback to default certificate
        let default_cert = config.cert_dir.join("default/fullchain.pem");
        let default_key = config.cert_dir.join("default/privkey.pem");

        if !default_cert.exists() || !default_key.exists() {
            tracing::warn!(
                "No certificate manager and no default certificate found. HTTPS disabled."
            );
            return Ok(None);
        }

        let cert_path_str = default_cert.to_string_lossy().to_string();
        let key_path_str = default_key.to_string_lossy().to_string();

        let mut tls_settings =
            TlsSettings::intermediate(&cert_path_str, &key_path_str).map_err(|e| {
                Error::explain(
                    ErrorType::InternalError,
                    format!("Failed to create TLS settings: {}", e),
                )
            })?;

        tls_settings.enable_h2();

        tracing::info!(
            cert_path = %default_cert.display(),
            "Loaded default TLS certificate"
        );

        Ok(Some(tls_settings))
    }
}

/// Builder for configuring the proxy server
pub struct ProxyBuilder {
    lb: Arc<LoadBalancer>,
    routes: Arc<RwLock<RouteTable>>,
    config: ProxyConfig,
    tls_config: Option<TlsConfig>,
    acme_tokens: Option<ChallengeTokens>,
    cert_manager: Option<Arc<CertManager>>,
}

impl ProxyBuilder {
    pub fn new(lb: Arc<LoadBalancer>) -> Self {
        Self {
            lb,
            routes: Arc::new(RwLock::new(RouteTable::default())),
            config: ProxyConfig::default(),
            tls_config: None,
            acme_tokens: None,
            cert_manager: None,
        }
    }

    /// Set route table shared with the proxy (app_name -> patterns)
    pub fn routes(mut self, routes: Arc<RwLock<RouteTable>>) -> Self {
        self.routes = routes;
        self
    }

    /// Set proxy configuration
    pub fn config(mut self, config: ProxyConfig) -> Self {
        self.config = config;
        self
    }

    /// Set HTTP port
    pub fn http_port(mut self, port: u16) -> Self {
        self.config.http_port = port;
        self
    }

    /// Set HTTPS port
    pub fn https_port(mut self, port: u16) -> Self {
        self.config.https_port = port;
        self
    }

    /// Enable development mode with self-signed certificates
    pub fn dev_mode(mut self) -> Self {
        self.config.dev_mode = true;
        self
    }

    /// Set certificate directory
    pub fn cert_dir(mut self, dir: PathBuf) -> Self {
        self.config.cert_dir = dir;
        self
    }

    /// Set TLS configuration
    pub fn tls(mut self, tls_config: TlsConfig) -> Self {
        self.tls_config = Some(tls_config);
        self
    }

    /// Enable ACME challenge handling
    pub fn acme_tokens(mut self, tokens: ChallengeTokens) -> Self {
        self.acme_tokens = Some(tokens);
        self
    }

    /// Set certificate manager for SNI-based cert selection
    pub fn cert_manager(mut self, cm: Arc<CertManager>) -> Self {
        self.cert_manager = Some(cm);
        self
    }

    /// Build the proxy server
    pub fn build(self) -> Result<Server> {
        build_server_with_acme(
            self.lb,
            self.routes,
            self.config,
            self.acme_tokens,
            self.cert_manager,
            Arc::new(ColdStartManager::new(
                crate::scaling::ColdStartConfig::default(),
            )),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::instances::AppManager;
    use parking_lot::RwLock;
    use std::collections::HashMap;
    use tempfile::TempDir;

    #[test]
    fn test_tako_proxy_creation() {
        let manager = Arc::new(AppManager::new());
        let lb = Arc::new(LoadBalancer::new(manager));
        let routes = Arc::new(tokio::sync::RwLock::new(RouteTable::default()));
        let cold_start = Arc::new(ColdStartManager::new(
            crate::scaling::ColdStartConfig::default(),
        ));
        let proxy = TakoProxy::new(lb, routes, ProxyConfig::default(), cold_start);

        // Just verify creation works
        let ctx = proxy.new_ctx();
        assert!(ctx.backend.is_none());
        assert!(!ctx.is_https);
        assert!(ctx.acme_response.is_none());
    }

    #[test]
    fn test_tako_proxy_with_acme() {
        let manager = Arc::new(AppManager::new());
        let lb = Arc::new(LoadBalancer::new(manager));
        let tokens: ChallengeTokens = Arc::new(RwLock::new(HashMap::new()));

        let routes = Arc::new(tokio::sync::RwLock::new(RouteTable::default()));
        let cold_start = Arc::new(ColdStartManager::new(
            crate::scaling::ColdStartConfig::default(),
        ));
        let proxy = TakoProxy::with_acme(lb, routes, ProxyConfig::default(), tokens, cold_start);
        assert!(proxy.challenge_handler.is_some());
    }

    #[test]
    fn test_proxy_config_default() {
        let config = ProxyConfig::default();
        assert_eq!(config.http_port, 80);
        assert_eq!(config.https_port, 443);
        assert!(config.enable_https);
        assert!(!config.dev_mode);
        assert!(config.redirect_http_to_https);
    }

    #[test]
    fn test_proxy_config_development() {
        let config = ProxyConfig::development();
        assert_eq!(config.http_port, 8080);
        assert_eq!(config.https_port, 8443);
        assert!(config.enable_https);
        assert!(config.dev_mode);
        assert!(config.redirect_http_to_https);
    }

    #[test]
    fn test_tls_config_development() {
        let temp = TempDir::new().unwrap();
        let tls_config = TlsConfig::development(temp.path().to_path_buf());

        // Should be able to get localhost cert
        let cert = tls_config.get_cert("localhost");
        assert!(cert.is_some());

        let cert = cert.unwrap();
        assert!(cert.is_self_signed);
        assert!(cert.cert_path.exists());
        assert!(cert.key_path.exists());
    }

    #[test]
    fn test_tls_config_wildcard_localhost() {
        let temp = TempDir::new().unwrap();
        let tls_config = TlsConfig::development(temp.path().to_path_buf());

        // Should get localhost cert for subdomains too
        let cert = tls_config.get_cert("app.localhost");
        assert!(cert.is_some());
    }

    #[test]
    fn test_create_tls_settings_dev_mode() {
        let temp = TempDir::new().unwrap();
        let config = ProxyConfig {
            cert_dir: temp.path().to_path_buf(),
            dev_mode: true,
            ..Default::default()
        };

        let settings = create_tls_settings(&config, None).unwrap();
        assert!(settings.is_some());
    }

    #[test]
    fn test_create_tls_settings_no_cert() {
        let temp = TempDir::new().unwrap();
        let config = ProxyConfig {
            cert_dir: temp.path().to_path_buf(),
            dev_mode: false, // Not dev mode, requires real certs
            ..Default::default()
        };

        let settings = create_tls_settings(&config, None).unwrap();
        assert!(settings.is_none()); // No default cert exists
    }

    #[test]
    fn test_should_redirect_http_request_when_http_and_enabled() {
        assert!(should_redirect_http_request(false, true));
    }

    #[test]
    fn test_should_not_redirect_http_request_when_already_https() {
        assert!(!should_redirect_http_request(true, true));
    }

    #[test]
    fn test_should_not_redirect_http_request_when_disabled() {
        assert!(!should_redirect_http_request(false, false));
    }

    #[test]
    fn test_proxy_builder() {
        let manager = Arc::new(AppManager::new());
        let lb = Arc::new(LoadBalancer::new(manager));
        let temp = TempDir::new().unwrap();

        let builder = ProxyBuilder::new(lb)
            .http_port(3000)
            .https_port(3443)
            .dev_mode()
            .cert_dir(temp.path().to_path_buf());

        assert_eq!(builder.config.http_port, 3000);
        assert_eq!(builder.config.https_port, 3443);
        assert!(builder.config.dev_mode);
        assert!(builder.config.redirect_http_to_https);
    }

    #[test]
    fn test_proxy_builder_with_acme() {
        let manager = Arc::new(AppManager::new());
        let lb = Arc::new(LoadBalancer::new(manager));
        let tokens: ChallengeTokens = Arc::new(RwLock::new(HashMap::new()));

        let builder = ProxyBuilder::new(lb).acme_tokens(tokens);
        assert!(builder.acme_tokens.is_some());
    }
}

//! HTTP/HTTPS Proxy using Pingora
//!
//! Routes incoming HTTP requests to app instances based on Host header.
//! Supports TLS termination with automatic certificate management.
//! Handles ACME HTTP-01 challenges for Let's Encrypt certificate issuance.
//! Exposes internal host status endpoint (`tako.internal/status`) for health monitoring.

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
use pingora_core::listeners::TcpSocketOptions;
use pingora_core::listeners::tls::TlsSettings;
use pingora_core::prelude::*;
use pingora_core::upstreams::peer::HttpPeer;
use pingora_http::{RequestHeader, ResponseHeader};
use pingora_proxy::{ProxyHttp, Session};
use serde::Serialize;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Server status response for internal `tako.internal/status` endpoint
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

    async fn load_balancer_cleanup(&self, app_name: &str) {
        self.lb.unregister_app(app_name);
        self.routes.write().await.remove_app_routes(app_name);
    }

    /// Generate server status for internal `tako.internal/status` endpoint
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

enum BackendResolution {
    Ready(Backend),
    StartupTimeout,
    StartupFailed,
    Unavailable,
    AppMissing,
}

impl TakoProxy {
    async fn resolve_backend(&self, app_name: &str) -> BackendResolution {
        if let Some(backend) = self.lb.get_backend(app_name) {
            return BackendResolution::Ready(backend);
        }

        let Some(app) = self.lb.app_manager().get_app(app_name) else {
            return BackendResolution::AppMissing;
        };

        if app.config.read().min_instances != 0 {
            return BackendResolution::Unavailable;
        }

        let begin = self.cold_start.begin(app_name);
        if begin.leader {
            app.set_state(crate::socket::AppState::Running);

            let app_name = app_name.to_string();
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

        let ready = self.cold_start.wait_for_ready(app_name).await;
        if ready && let Some(backend) = self.lb.get_backend(app_name) {
            return BackendResolution::Ready(backend);
        }

        if self.cold_start.is_cold_starting(app_name) {
            BackendResolution::StartupTimeout
        } else {
            BackendResolution::StartupFailed
        }
    }
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
        let host = session
            .req_header()
            .headers
            .get("host")
            .and_then(|h| h.to_str().ok())
            .unwrap_or("");
        let hostname = host.split(':').next().unwrap_or(host);
        let internal_status_request = is_internal_status_request(hostname, path);

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
                let body = "Token not found";
                let mut header = ResponseHeader::build(404, None)?;
                insert_body_headers(&mut header, "text/plain", body)?;
                session
                    .write_response_header(Box::new(header), false)
                    .await?;
                session.write_response_body(Some(body.into()), true).await?;
                return Ok(true);
            }
        }

        // Handle internal status endpoint for health monitoring
        if internal_status_request {
            let status = self.get_server_status();
            let status_code = if status.healthy { 200 } else { 503 };

            let body = serde_json::to_string_pretty(&status).unwrap_or_else(|_| {
                r#"{"healthy":false,"apps":[],"error":"serialization failed"}"#.to_string()
            });

            let mut header = ResponseHeader::build(status_code, None)?;
            insert_body_headers(&mut header, "application/json", &body)?;
            header.insert_header("Cache-Control", "no-cache, no-store")?;
            session
                .write_response_header(Box::new(header), false)
                .await?;
            session.write_response_body(Some(body.into()), true).await?;

            tracing::debug!(
                healthy = status.healthy,
                app_count = status.apps.len(),
                "Served internal status endpoint"
            );
            return Ok(true);
        }

        // Handle HTTP to HTTPS redirect.
        // Allow ACME challenges and internal status endpoint on HTTP.
        if !path.starts_with("/.well-known/acme-challenge/") && !internal_status_request {
            let transport_https = session
                .digest()
                .map(|d| d.ssl_digest.is_some())
                .unwrap_or(false);
            let request_headers = &session.req_header().headers;
            let x_forwarded_for = request_headers
                .get("x-forwarded-for")
                .and_then(|h| h.to_str().ok());
            let x_forwarded_proto = request_headers
                .get("x-forwarded-proto")
                .and_then(|h| h.to_str().ok());
            let forwarded = request_headers
                .get("forwarded")
                .and_then(|h| h.to_str().ok());
            let is_effective_https =
                is_effective_request_https(transport_https, x_forwarded_proto, forwarded)
                    || should_assume_forwarded_private_request_https(
                        hostname,
                        x_forwarded_for,
                        x_forwarded_proto,
                        forwarded,
                    );
            ctx.is_https = is_effective_https;

            if should_redirect_http_request(is_effective_https, self.config.redirect_http_to_https)
            {
                let redirect_url = format!("https://{}{}", host, path);
                let body = "Redirecting to HTTPS";

                let mut header = ResponseHeader::build(307, None)?;
                header.insert_header("Location", &redirect_url)?;
                header.insert_header("Cache-Control", "no-store")?;
                insert_body_headers(&mut header, "text/plain", body)?;
                session
                    .write_response_header(Box::new(header), false)
                    .await?;
                session.write_response_body(Some(body.into()), true).await?;
                return Ok(true);
            }
        }

        // Route request to app based on host/path
        let app_name = match self.routes.read().await.select(hostname, path) {
            Some(app) => app,
            None => {
                let body = "Not Found";
                let mut header = ResponseHeader::build(404, None)?;
                insert_body_headers(&mut header, "text/plain", body)?;
                session
                    .write_response_header(Box::new(header), false)
                    .await?;
                session.write_response_body(Some(body.into()), true).await?;
                return Ok(true);
            }
        };

        // Try to get a healthy backend. For on-demand apps (instances=0), this
        // waits for cold start readiness (up to the configured startup timeout).
        let backend = match self.resolve_backend(&app_name).await {
            BackendResolution::Ready(backend) => backend,
            BackendResolution::StartupTimeout => {
                let body = "App startup timed out";
                let mut header = ResponseHeader::build(504, None)?;
                insert_body_headers(&mut header, "text/plain", body)?;
                session
                    .write_response_header(Box::new(header), false)
                    .await?;
                session.write_response_body(Some(body.into()), true).await?;
                return Ok(true);
            }
            BackendResolution::StartupFailed => {
                let body = "App failed to start";
                let mut header = ResponseHeader::build(502, None)?;
                insert_body_headers(&mut header, "text/plain", body)?;
                session
                    .write_response_header(Box::new(header), false)
                    .await?;
                session.write_response_body(Some(body.into()), true).await?;
                return Ok(true);
            }
            BackendResolution::Unavailable => {
                let body = "No healthy backend";
                let mut header = ResponseHeader::build(503, None)?;
                insert_body_headers(&mut header, "text/plain", body)?;
                session
                    .write_response_header(Box::new(header), false)
                    .await?;
                session.write_response_body(Some(body.into()), true).await?;
                return Ok(true);
            }
            BackendResolution::AppMissing => {
                // Route existed but app no longer exists (stale in-memory routing).
                // Clean it up and return a normal 404 to callers.
                self.load_balancer_cleanup(&app_name).await;
                let body = "Not Found";
                let mut header = ResponseHeader::build(404, None)?;
                insert_body_headers(&mut header, "text/plain", body)?;
                session
                    .write_response_header(Box::new(header), false)
                    .await?;
                session.write_response_body(Some(body.into()), true).await?;
                return Ok(true);
            }
        };

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
        let transport_https = session
            .digest()
            .map(|d| d.ssl_digest.is_some())
            .unwrap_or(false);
        let request_headers = &session.req_header().headers;
        let host = request_headers
            .get("host")
            .and_then(|h| h.to_str().ok())
            .unwrap_or("");
        let hostname = host.split(':').next().unwrap_or(host);
        let x_forwarded_for = request_headers
            .get("x-forwarded-for")
            .and_then(|h| h.to_str().ok());
        let x_forwarded_proto = request_headers
            .get("x-forwarded-proto")
            .and_then(|h| h.to_str().ok());
        let forwarded = request_headers
            .get("forwarded")
            .and_then(|h| h.to_str().ok());
        ctx.is_https = is_effective_request_https(transport_https, x_forwarded_proto, forwarded)
            || should_assume_forwarded_private_request_https(
                hostname,
                x_forwarded_for,
                x_forwarded_proto,
                forwarded,
            );

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
            insert_body_headers(&mut header, "text/plain", response)?;
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

fn should_redirect_http_request(is_effective_https: bool, redirect_http_to_https: bool) -> bool {
    redirect_http_to_https && !is_effective_https
}

fn is_request_forwarded_https(x_forwarded_proto: Option<&str>, forwarded: Option<&str>) -> bool {
    x_forwarded_proto.is_some_and(x_forwarded_proto_is_https)
        || forwarded.is_some_and(forwarded_header_proto_is_https)
}

fn is_effective_request_https(
    transport_https: bool,
    x_forwarded_proto: Option<&str>,
    forwarded: Option<&str>,
) -> bool {
    transport_https || is_request_forwarded_https(x_forwarded_proto, forwarded)
}

fn should_assume_forwarded_private_request_https(
    hostname: &str,
    x_forwarded_for: Option<&str>,
    x_forwarded_proto: Option<&str>,
    forwarded: Option<&str>,
) -> bool {
    crate::is_private_local_hostname(hostname)
        && has_nonempty_header_value(x_forwarded_for)
        && !has_forwarded_proto(x_forwarded_proto, forwarded)
}

fn has_forwarded_proto(x_forwarded_proto: Option<&str>, forwarded: Option<&str>) -> bool {
    has_nonempty_header_value(x_forwarded_proto)
        || forwarded.is_some_and(forwarded_header_has_proto)
}

fn has_nonempty_header_value(value: Option<&str>) -> bool {
    value.is_some_and(|raw| !raw.trim().is_empty())
}

fn x_forwarded_proto_is_https(value: &str) -> bool {
    // Keep only the first forwarded hop; proxies append as comma-separated values.
    value
        .split(',')
        .next()
        .map(str::trim)
        .is_some_and(|proto| proto.eq_ignore_ascii_case("https"))
}

fn forwarded_header_proto_is_https(value: &str) -> bool {
    // RFC 7239: Forwarded: for=...,proto=https,by=...
    value.split(',').any(|entry| {
        entry.split(';').any(|param| {
            let mut parts = param.splitn(2, '=');
            let key = parts.next().map(str::trim).unwrap_or("");
            let raw_value = parts.next().map(str::trim).unwrap_or("");
            let parsed = raw_value.trim_matches('"');
            key.eq_ignore_ascii_case("proto") && parsed.eq_ignore_ascii_case("https")
        })
    })
}

fn forwarded_header_has_proto(value: &str) -> bool {
    value.split(',').any(|entry| {
        entry.split(';').any(|param| {
            let mut parts = param.splitn(2, '=');
            let key = parts.next().map(str::trim).unwrap_or("");
            let raw_value = parts.next().map(str::trim).unwrap_or("");
            let parsed = raw_value.trim_matches('"');
            key.eq_ignore_ascii_case("proto") && !parsed.is_empty()
        })
    })
}

fn is_internal_status_request(hostname: &str, path: &str) -> bool {
    hostname.eq_ignore_ascii_case(crate::instances::INTERNAL_STATUS_HOST) && path == "/status"
}

fn insert_body_headers(header: &mut ResponseHeader, content_type: &str, body: &str) -> Result<()> {
    header.insert_header("Content-Type", content_type)?;
    header.insert_header("Content-Length", body.as_bytes().len().to_string())?;
    Ok(())
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
    let listener_options = listener_socket_options();

    // Add HTTP listener
    proxy_service.add_tcp_with_settings(
        &format!("0.0.0.0:{}", config.http_port),
        listener_options.clone(),
    );

    // Add HTTPS listener if enabled
    if config.enable_https {
        if let Some(tls_settings) = create_tls_settings(&config, cert_manager)? {
            proxy_service.add_tls_with_settings(
                &format!("0.0.0.0:{}", config.https_port),
                Some(listener_options),
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

fn listener_socket_options() -> TcpSocketOptions {
    let mut options = TcpSocketOptions::default();
    options.so_reuseport = Some(true);
    options
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
    use crate::instances::{AppConfig, AppManager};
    use crate::scaling::ColdStartConfig;
    use crate::socket::InstanceState;
    use parking_lot::RwLock;
    use std::collections::HashMap;
    use std::time::Duration;
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
    fn listener_socket_options_enable_reuseport() {
        let options = listener_socket_options();
        assert_eq!(options.so_reuseport, Some(true));
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
    fn test_should_not_redirect_http_request_when_forwarded_proto_is_https() {
        assert!(is_request_forwarded_https(Some("https"), None));
        assert!(!should_redirect_http_request(true, true));
    }

    #[test]
    fn test_should_not_redirect_http_request_when_forwarded_header_proto_is_https() {
        assert!(is_request_forwarded_https(
            None,
            Some("for=192.0.2.60;proto=https;by=203.0.113.43")
        ));
        assert!(!should_redirect_http_request(true, true));
    }

    #[test]
    fn test_effective_request_https_prefers_transport_tls() {
        assert!(is_effective_request_https(true, None, None));
    }

    #[test]
    fn test_effective_request_https_uses_forwarded_https_when_transport_is_http() {
        assert!(is_effective_request_https(false, Some("https"), None));
        assert!(is_effective_request_https(
            false,
            None,
            Some("for=192.0.2.60;proto=https")
        ));
        assert!(!is_effective_request_https(false, Some("http"), None));
    }

    #[test]
    fn test_private_local_forwarded_request_without_proto_is_treated_as_https() {
        let inferred_https = should_assume_forwarded_private_request_https(
            "test-app.orb.local",
            Some("127.0.0.1"),
            None,
            None,
        );
        assert!(inferred_https);
    }

    #[test]
    fn test_private_local_forwarded_request_with_proto_is_not_inferred() {
        assert!(!should_assume_forwarded_private_request_https(
            "test-app.orb.local",
            Some("127.0.0.1"),
            Some("http"),
            None,
        ));
        assert!(!should_assume_forwarded_private_request_https(
            "test-app.orb.local",
            None,
            None,
            Some("for=127.0.0.1;proto=https"),
        ));
    }

    #[test]
    fn test_public_forwarded_request_without_proto_is_not_inferred() {
        assert!(!should_assume_forwarded_private_request_https(
            "api.example.com",
            Some("127.0.0.1"),
            None,
            None,
        ));
    }

    #[test]
    fn test_forwarded_header_has_proto_detects_presence() {
        assert!(forwarded_header_has_proto("for=192.0.2.60;proto=https"));
        assert!(forwarded_header_has_proto(
            r#"for=192.0.2.60;proto="http";by=203.0.113.43"#
        ));
        assert!(!forwarded_header_has_proto(
            "for=192.0.2.60;by=203.0.113.43"
        ));
        assert!(!forwarded_header_has_proto(r#"for=192.0.2.60;proto="""#));
    }

    #[test]
    fn test_x_forwarded_proto_parsing_handles_case_and_commas() {
        assert!(x_forwarded_proto_is_https("HTTPS"));
        assert!(x_forwarded_proto_is_https("https, http"));
        assert!(!x_forwarded_proto_is_https("http, https"));
    }

    #[test]
    fn test_forwarded_header_parsing_handles_quotes_and_multiple_entries() {
        assert!(forwarded_header_proto_is_https(
            r#"for=192.0.2.60;proto="https";by=203.0.113.43"#
        ));
        assert!(forwarded_header_proto_is_https(
            "for=192.0.2.60;proto=http,for=198.51.100.17;proto=https"
        ));
        assert!(!forwarded_header_proto_is_https(
            "for=192.0.2.60;proto=http"
        ));
    }

    #[test]
    fn test_is_internal_status_request_matches_expected_host_and_path() {
        assert!(is_internal_status_request("tako.internal", "/status"));
        assert!(is_internal_status_request("TAKO.INTERNAL", "/status"));
    }

    #[test]
    fn test_is_internal_status_request_rejects_non_internal_targets() {
        assert!(!is_internal_status_request("example.com", "/status"));
        assert!(!is_internal_status_request("tako.internal", "/"));
        assert!(!is_internal_status_request(
            "tako.internal",
            "/_tako/status"
        ));
    }

    #[test]
    fn body_headers_include_content_type_and_length() {
        let mut header = ResponseHeader::build(404, None).expect("build header");
        insert_body_headers(&mut header, "text/plain", "Not Found").expect("insert headers");

        assert_eq!(
            header
                .headers
                .get("Content-Type")
                .and_then(|v| v.to_str().ok()),
            Some("text/plain")
        );
        assert_eq!(
            header
                .headers
                .get("Content-Length")
                .and_then(|v| v.to_str().ok()),
            Some("9")
        );
    }

    #[test]
    fn body_headers_use_utf8_byte_length() {
        let mut header = ResponseHeader::build(200, None).expect("build header");
        insert_body_headers(&mut header, "text/plain", "✓").expect("insert headers");

        assert_eq!(
            header
                .headers
                .get("Content-Length")
                .and_then(|v| v.to_str().ok()),
            Some("3")
        );
    }

    #[tokio::test]
    async fn resolve_backend_waits_for_ready_on_on_demand_apps() {
        let manager = Arc::new(AppManager::new());
        let lb = Arc::new(LoadBalancer::new(manager.clone()));
        let app = manager.register_app(AppConfig {
            name: "test-app".to_string(),
            version: "v1".to_string(),
            min_instances: 0,
            base_port: 3010,
            ..Default::default()
        });
        lb.register_app(app.clone());

        let routes = Arc::new(tokio::sync::RwLock::new(RouteTable::default()));
        let cold_start = Arc::new(ColdStartManager::new(ColdStartConfig {
            startup_timeout: Duration::from_secs(1),
            max_queued_requests: 100,
        }));
        let proxy = TakoProxy::new(lb, routes, ProxyConfig::default(), cold_start.clone());

        let instance = app.allocate_instance();
        cold_start.begin("test-app");

        let ready_cold_start = cold_start.clone();
        let ready_instance = instance.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            ready_instance.set_state(InstanceState::Healthy);
            ready_cold_start.mark_ready("test-app");
        });

        let resolution = proxy.resolve_backend("test-app").await;
        assert!(matches!(resolution, BackendResolution::Ready(_)));
    }

    #[tokio::test]
    async fn resolve_backend_returns_startup_timeout_after_wait_timeout() {
        let manager = Arc::new(AppManager::new());
        let lb = Arc::new(LoadBalancer::new(manager.clone()));
        let app = manager.register_app(AppConfig {
            name: "test-app".to_string(),
            version: "v1".to_string(),
            min_instances: 0,
            ..Default::default()
        });
        lb.register_app(app);

        let routes = Arc::new(tokio::sync::RwLock::new(RouteTable::default()));
        let cold_start = Arc::new(ColdStartManager::new(ColdStartConfig {
            startup_timeout: Duration::from_millis(25),
            max_queued_requests: 100,
        }));
        let proxy = TakoProxy::new(lb, routes, ProxyConfig::default(), cold_start.clone());

        cold_start.begin("test-app");

        let resolution = proxy.resolve_backend("test-app").await;
        assert!(matches!(resolution, BackendResolution::StartupTimeout));
    }

    #[tokio::test]
    async fn resolve_backend_returns_startup_failed_when_cold_start_fails() {
        let manager = Arc::new(AppManager::new());
        let lb = Arc::new(LoadBalancer::new(manager.clone()));
        let app = manager.register_app(AppConfig {
            name: "test-app".to_string(),
            version: "v1".to_string(),
            min_instances: 0,
            ..Default::default()
        });
        lb.register_app(app);

        let routes = Arc::new(tokio::sync::RwLock::new(RouteTable::default()));
        let cold_start = Arc::new(ColdStartManager::new(ColdStartConfig {
            startup_timeout: Duration::from_secs(1),
            max_queued_requests: 100,
        }));
        let proxy = TakoProxy::new(lb, routes, ProxyConfig::default(), cold_start.clone());

        cold_start.begin("test-app");
        let failed_cold_start = cold_start.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(25)).await;
            failed_cold_start.mark_failed("test-app");
        });

        let resolution = proxy.resolve_backend("test-app").await;
        assert!(matches!(resolution, BackendResolution::StartupFailed));
    }

    #[tokio::test]
    async fn resolve_backend_returns_unavailable_for_non_on_demand_apps_without_backend() {
        let manager = Arc::new(AppManager::new());
        let lb = Arc::new(LoadBalancer::new(manager.clone()));
        let app = manager.register_app(AppConfig {
            name: "test-app".to_string(),
            version: "v1".to_string(),
            min_instances: 1,
            ..Default::default()
        });
        lb.register_app(app);

        let routes = Arc::new(tokio::sync::RwLock::new(RouteTable::default()));
        let cold_start = Arc::new(ColdStartManager::new(ColdStartConfig::default()));
        let proxy = TakoProxy::new(lb, routes, ProxyConfig::default(), cold_start);

        let resolution = proxy.resolve_backend("test-app").await;
        assert!(matches!(resolution, BackendResolution::Unavailable));
    }

    #[tokio::test]
    async fn resolve_backend_returns_app_missing_when_app_not_registered() {
        let manager = Arc::new(AppManager::new());
        let lb = Arc::new(LoadBalancer::new(manager));

        let routes = Arc::new(tokio::sync::RwLock::new(RouteTable::default()));
        let cold_start = Arc::new(ColdStartManager::new(ColdStartConfig::default()));
        let proxy = TakoProxy::new(lb, routes, ProxyConfig::default(), cold_start);

        let resolution = proxy.resolve_backend("missing-app").await;
        assert!(matches!(resolution, BackendResolution::AppMissing));
    }

    #[tokio::test]
    async fn load_balancer_cleanup_removes_stale_routes_for_app() {
        let manager = Arc::new(AppManager::new());
        let lb = Arc::new(LoadBalancer::new(manager));
        let routes = Arc::new(tokio::sync::RwLock::new(RouteTable::default()));
        {
            let mut table = routes.write().await;
            table.set_app_routes("test-app".to_string(), vec!["test.example.com".to_string()]);
            assert_eq!(
                table.select("test.example.com", "/"),
                Some("test-app".to_string())
            );
        }
        let cold_start = Arc::new(ColdStartManager::new(ColdStartConfig::default()));
        let proxy = TakoProxy::new(lb, routes.clone(), ProxyConfig::default(), cold_start);

        proxy.load_balancer_cleanup("test-app").await;

        let table = routes.read().await;
        assert!(table.routes_for_app("test-app").is_empty());
        assert_eq!(table.select("test.example.com", "/"), None);
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

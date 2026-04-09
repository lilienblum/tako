use super::request::{
    build_proxy_cache_key, client_ip_from_session, insert_body_headers, is_effective_request_https,
    path_looks_like_static_asset, request_host, request_is_proxy_cacheable, response_cacheability,
    should_assume_forwarded_private_request_https, should_redirect_http_request,
    static_lookup_paths, stream_static_file,
};
use super::{AppStaticServer, StaticConfig, StaticFileError, TakoProxy};
use crate::lb::Backend;
use crate::metrics::RequestTimer;
use crate::scaling::WaitForReadyOutcome;
use async_trait::async_trait;
use pingora_cache::{CacheKey, RespCacheable};
use pingora_core::prelude::*;
use pingora_core::upstreams::peer::HttpPeer;
use pingora_http::{RequestHeader, ResponseHeader};
use pingora_proxy::{ProxyHttp, Session};
use std::net::IpAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

impl TakoProxy {
    pub(super) async fn load_balancer_cleanup(&self, app_name: &str) {
        self.lb.unregister_app(app_name);
        self.routes.write().await.remove_app_routes(app_name);
        self.static_servers.write().remove(app_name);
    }

    pub(super) fn static_server_for_app(
        &self,
        app_name: &str,
        app_root: &Path,
    ) -> Arc<AppStaticServer> {
        let desired_root = app_root.join(&default_static_config().public_dir);
        if let Some(existing) = self.static_servers.read().get(app_name)
            && existing.root() == desired_root.as_path()
        {
            return existing.clone();
        }

        let mut servers = self.static_servers.write();
        if let Some(existing) = servers.get(app_name)
            && existing.root() == desired_root.as_path()
        {
            return existing.clone();
        }

        let server = Arc::new(AppStaticServer::new(
            app_name.to_string(),
            app_root.to_path_buf(),
            default_static_config().clone(),
        ));
        servers.insert(app_name.to_string(), server.clone());
        server
    }

    async fn try_serve_static_asset(
        &self,
        session: &mut Session,
        app_name: &str,
        request_path: &str,
        matched_route_path: Option<&str>,
    ) -> Result<bool> {
        let method = session.req_header().method.as_str().to_string();
        if method != "GET" && method != "HEAD" {
            return Ok(false);
        }

        let Some(app) = self.lb.app_manager().get_app(app_name) else {
            return Ok(false);
        };
        let app_root = app.config.read().path.clone();
        let static_server = self.static_server_for_app(app_name, &app_root);
        if !static_server.is_available() {
            return Ok(false);
        }

        for lookup_path in static_lookup_paths(request_path, matched_route_path) {
            match static_server.resolve(&lookup_path) {
                Ok(file) => {
                    let mut file_handle = if method == "HEAD" {
                        None
                    } else {
                        match tokio::fs::File::open(&file.path).await {
                            Ok(opened) => Some(opened),
                            Err(_) => {
                                let body = "Static asset read failed";
                                let mut header = ResponseHeader::build(500, None)?;
                                insert_body_headers(&mut header, "text/plain", body)?;
                                session
                                    .write_response_header(Box::new(header), false)
                                    .await?;
                                session.write_response_body(Some(body.into()), true).await?;
                                return Ok(true);
                            }
                        }
                    };

                    let mut header = ResponseHeader::build(200, None)?;
                    header.insert_header("Content-Type", &file.content_type)?;
                    header.insert_header("Content-Length", file.size.to_string())?;
                    header.insert_header("Cache-Control", &file.cache_control)?;
                    header.insert_header("ETag", &file.etag)?;
                    session
                        .write_response_header(Box::new(header), false)
                        .await?;

                    if method == "HEAD" {
                        session.write_response_body(None, true).await?;
                        return Ok(true);
                    }

                    let mut file_handle = file_handle
                        .take()
                        .expect("file handle is always present for non-HEAD static responses");
                    stream_static_file(session, &mut file_handle, &file.path).await?;
                    return Ok(true);
                }
                Err(StaticFileError::NotFound(_)) => {}
                Err(StaticFileError::PathTraversal(_)) | Err(StaticFileError::InvalidPath(_)) => {
                    let body = "Bad Request";
                    let mut header = ResponseHeader::build(400, None)?;
                    insert_body_headers(&mut header, "text/plain", body)?;
                    session
                        .write_response_header(Box::new(header), false)
                        .await?;
                    session.write_response_body(Some(body.into()), true).await?;
                    return Ok(true);
                }
                Err(StaticFileError::Io(_)) => {}
            }
        }

        Ok(false)
    }
}

/// Request context for tracking which backend is serving
pub struct RequestCtx {
    pub(super) backend: Option<Backend>,
    pub(super) is_https: bool,
    pub(super) matched_route_path: Option<String>,
    pub(super) request_timer: Option<RequestTimer>,
    /// Client IP for per-IP rate limit tracking (released in logging phase)
    pub(super) client_ip: Option<IpAddr>,
    /// Accumulated request body bytes (for chunked transfer size enforcement)
    pub(super) body_bytes_received: u64,
}

pub(crate) enum BackendResolution {
    Ready(Backend),
    StartupTimeout,
    StartupFailed,
    QueueFull,
    Unavailable,
    AppMissing,
}

impl TakoProxy {
    pub(super) async fn resolve_backend(&self, app_name: &str) -> BackendResolution {
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
                    app.remove_instance(&instance.id);
                    cold_start.mark_failed(&app_name);
                }
            });
        }

        match self.cold_start.wait_for_ready_outcome(app_name).await {
            WaitForReadyOutcome::Ready => self
                .lb
                .get_backend(app_name)
                .map(BackendResolution::Ready)
                .unwrap_or(BackendResolution::StartupFailed),
            WaitForReadyOutcome::Timeout => BackendResolution::StartupTimeout,
            WaitForReadyOutcome::Failed => BackendResolution::StartupFailed,
            WaitForReadyOutcome::QueueFull => BackendResolution::QueueFull,
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
            matched_route_path: None,
            request_timer: None,
            client_ip: None,
            body_bytes_received: 0,
        }
    }

    async fn request_filter(&self, session: &mut Session, ctx: &mut Self::CTX) -> Result<bool> {
        if let Some(ip) = client_ip_from_session(session) {
            if !self.ip_tracker.try_acquire(ip) {
                let body = "Too Many Requests";
                let mut header = ResponseHeader::build(429, None)?;
                header.insert_header("Retry-After", "1")?;
                insert_body_headers(&mut header, "text/plain", body)?;
                session
                    .write_response_header(Box::new(header), false)
                    .await?;
                session.write_response_body(Some(body.into()), true).await?;
                return Ok(true);
            }
            ctx.client_ip = Some(ip);
        }

        if let Some(cl) = session
            .req_header()
            .headers
            .get("content-length")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<u64>().ok())
            && cl > super::MAX_REQUEST_BODY_BYTES
        {
            let body = "Payload Too Large";
            let mut header = ResponseHeader::build(413, None)?;
            insert_body_headers(&mut header, "text/plain", body)?;
            session
                .write_response_header(Box::new(header), false)
                .await?;
            session.write_response_body(Some(body.into()), true).await?;
            return Ok(true);
        }

        let path = session.req_header().uri.path().to_string();
        let host = request_host(session.req_header());
        let hostname = host.split(':').next().unwrap_or(host);

        if let Some(ref handler) = self.challenge_handler
            && handler.is_challenge_request(&path)
        {
            if let Some(response) = handler.handle_challenge(&path) {
                tracing::info!(path = %path, "Serving ACME challenge response");
                let mut header = ResponseHeader::build(200, None)?;
                insert_body_headers(&mut header, "text/plain", &response)?;
                session
                    .write_response_header(Box::new(header), false)
                    .await?;
                session
                    .write_response_body(Some(response.into()), true)
                    .await?;
                return Ok(true);
            } else {
                tracing::warn!(path = %path, "ACME challenge token not found");
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

        if !path.starts_with("/.well-known/acme-challenge/") {
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
                let path_and_query = session
                    .req_header()
                    .uri
                    .path_and_query()
                    .map(|pq| pq.as_str())
                    .unwrap_or(&path);
                let redirect_url = format!("https://{}{}", host, path_and_query);
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

        let route_match = match self.routes.read().await.select_with_route(hostname, &path) {
            Some(route_match) => route_match,
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
        let app_name = route_match.app;
        ctx.matched_route_path = route_match.path;

        if path_looks_like_static_asset(&path)
            && self
                .try_serve_static_asset(
                    session,
                    &app_name,
                    &path,
                    ctx.matched_route_path.as_deref(),
                )
                .await?
        {
            return Ok(true);
        }

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
            BackendResolution::QueueFull => {
                let body = "App startup queue is full";
                let mut header = ResponseHeader::build(503, None)?;
                header.insert_header("Retry-After", "1")?;
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

        ctx.request_timer = Some(RequestTimer::start(app_name));
        ctx.backend = Some(backend);

        Ok(false)
    }

    async fn request_body_filter(
        &self,
        _session: &mut Session,
        body: &mut Option<bytes::Bytes>,
        _end_of_stream: bool,
        ctx: &mut Self::CTX,
    ) -> Result<()> {
        if let Some(data) = body {
            ctx.body_bytes_received += data.len() as u64;
            if ctx.body_bytes_received > super::MAX_REQUEST_BODY_BYTES {
                return Err(Error::explain(
                    ErrorType::InvalidHTTPHeader,
                    "Request body exceeds maximum allowed size",
                ));
            }
        }
        Ok(())
    }

    fn request_cache_filter(&self, session: &mut Session, _ctx: &mut Self::CTX) -> Result<()> {
        let Some(cache) = self.response_cache else {
            return Ok(());
        };

        if !request_is_proxy_cacheable(session.req_header()) {
            return Ok(());
        }

        session.cache.enable(
            cache.storage,
            Some(cache.eviction),
            None,
            Some(cache.cache_lock),
            None,
        );
        session
            .cache
            .set_max_file_size_bytes(cache.max_file_size_bytes);

        Ok(())
    }

    fn cache_key_callback(&self, session: &Session, _ctx: &mut Self::CTX) -> Result<CacheKey> {
        let host = request_host(session.req_header());
        Ok(build_proxy_cache_key(
            host,
            &session.req_header().uri.to_string(),
        ))
    }

    fn response_cache_filter(
        &self,
        session: &Session,
        resp: &ResponseHeader,
        _ctx: &mut Self::CTX,
    ) -> Result<RespCacheable> {
        if self.response_cache.is_none() {
            return Ok(RespCacheable::Uncacheable(
                pingora_cache::NoCacheReason::Custom("proxy_cache_disabled"),
            ));
        }

        let authorization_present = session.req_header().headers.contains_key("authorization");
        Ok(response_cacheability(resp, authorization_present))
    }

    async fn upstream_peer(
        &self,
        _session: &mut Session,
        ctx: &mut Self::CTX,
    ) -> Result<Box<HttpPeer>> {
        // ctx.is_https was already computed in request_filter

        let backend = ctx
            .backend
            .clone()
            .ok_or_else(|| Error::new(ErrorType::ConnectNoRoute))?;

        let mut peer = if let Some(endpoint) = backend.endpoint() {
            HttpPeer::new(endpoint, false, String::new())
        } else {
            return Err(Error::explain(
                ErrorType::ConnectNoRoute,
                format!(
                    "Missing upstream endpoint for app '{}' instance {}",
                    backend.app_name, backend.instance_id
                ),
            ));
        };

        peer.options.connection_timeout = Some(Duration::from_secs(5));
        peer.options.read_timeout = Some(Duration::from_secs(60));
        peer.options.write_timeout = Some(Duration::from_secs(30));

        Ok(Box::new(peer))
    }

    async fn upstream_request_filter(
        &self,
        session: &mut Session,
        upstream_request: &mut RequestHeader,
        ctx: &mut Self::CTX,
    ) -> Result<()> {
        let proto = if ctx.is_https { "https" } else { "http" };
        upstream_request
            .insert_header("X-Forwarded-Proto", proto)
            .unwrap();

        if let Some(ip) = client_ip_from_session(session) {
            upstream_request
                .insert_header("X-Forwarded-For", ip.to_string())
                .unwrap();
        } else {
            let _ = upstream_request.remove_header("X-Forwarded-For");
        }

        let _ = upstream_request.remove_header("Forwarded");
        let _ = upstream_request.remove_header("X-Tako-Internal-Token");

        if let Some(ref backend) = ctx.backend
            && let Some(app) = self.lb.app_manager().get_app(&backend.app_name)
            && let Some(instance) = app.get_instance(&backend.instance_id)
        {
            instance.request_started();
        }

        Ok(())
    }

    async fn logging(&self, session: &mut Session, _e: Option<&Error>, ctx: &mut Self::CTX) {
        if let Some(ip) = ctx.client_ip.take() {
            self.ip_tracker.release(ip);
        }

        if let Some(ref backend) = ctx.backend {
            self.lb
                .request_completed(&backend.app_name, &backend.instance_id);

            if let Some(app) = self.lb.app_manager().get_app(&backend.app_name)
                && let Some(instance) = app.get_instance(&backend.instance_id)
            {
                instance.request_finished();
            }
        }

        let status = session
            .response_written()
            .map(|r| r.status.as_u16())
            .unwrap_or(0);

        if let Some(timer) = ctx.request_timer.take() {
            timer.finish(status);
        }

        let host = request_host(session.req_header());
        let host = if host.is_empty() { "-" } else { host };

        let path = session.req_header().uri.path();
        let method = session.req_header().method.as_str();

        tracing::debug!(
            host = host,
            method = method,
            path = path,
            status = status,
            https = ctx.is_https,
            "Request completed"
        );
    }
}

fn default_static_config() -> &'static StaticConfig {
    static CONFIG: std::sync::OnceLock<StaticConfig> = std::sync::OnceLock::new();
    CONFIG.get_or_init(StaticConfig::default)
}

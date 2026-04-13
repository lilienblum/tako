use super::request::{
    build_proxy_cache_key, client_ip_from_session, insert_body_headers, is_effective_request_https,
    path_looks_like_static_asset, request_host, request_is_proxy_cacheable, response_cacheability,
    should_assume_forwarded_private_request_https, should_redirect_http_request,
    static_lookup_paths, stream_static_file,
};
use super::{AppStaticServer, StaticConfig, StaticFileError, TakoProxy};
use crate::channels::{
    ChannelAuthResponse, ChannelEndpoint, ChannelError, ChannelOperation, ChannelPublishPayload,
    ChannelStore, ChannelTransport, app_channels_db_path, authorize_channel_request,
    parse_channel_route, parse_message_id_cursor, parse_ws_last_message_id, read_request_body,
};
use crate::channels_ws::{
    WebSocketFrameReader, build_websocket_upgrade_response, parse_publish_payload,
    websocket_close_frame, websocket_ping_frame, websocket_pong_frame, websocket_text_frame,
};
use crate::lb::Backend;
use crate::metrics::RequestTimer;
use crate::scaling::WaitForReadyOutcome;
use async_trait::async_trait;
use bytes::Bytes;
use pingora_cache::{CacheKey, RespCacheable};
use pingora_core::prelude::*;
use pingora_core::upstreams::peer::HttpPeer;
use pingora_http::{RequestHeader, ResponseHeader};
use pingora_proxy::{ProxyHttp, Session};
use std::collections::HashMap;
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

    async fn write_json_response(
        &self,
        session: &mut Session,
        status: u16,
        value: &serde_json::Value,
    ) -> Result<bool> {
        let body = serde_json::to_string(value).map_err(|e| {
            Error::explain(
                ErrorType::InternalError,
                format!("Failed to serialize channel response: {e}"),
            )
        })?;
        let mut header = ResponseHeader::build(status, None)?;
        insert_body_headers(&mut header, "application/json", &body)?;
        session
            .write_response_header(Box::new(header), false)
            .await?;
        session
            .write_response_body(Some(Bytes::from(body)), true)
            .await?;
        Ok(true)
    }

    async fn write_channel_error(
        &self,
        session: &mut Session,
        error: ChannelError,
    ) -> Result<bool> {
        let (status, body) = match error {
            ChannelError::Forbidden => (403, serde_json::json!({ "error": "Forbidden" })),
            ChannelError::NotDefined => {
                (404, serde_json::json!({ "error": "Channel not defined" }))
            }
            ChannelError::StaleCursor => (
                410,
                serde_json::json!({ "error": "Channel cursor is outside the replay window" }),
            ),
            ChannelError::BadRequest(message) => (400, serde_json::json!({ "error": message })),
            ChannelError::Unsupported => (
                400,
                serde_json::json!({ "error": "Channel transport is not enabled" }),
            ),
            ChannelError::InvalidPath => (404, serde_json::json!({ "error": "Not found" })),
            ChannelError::AuthUnavailable => (
                503,
                serde_json::json!({ "error": "Channel auth unavailable" }),
            ),
            ChannelError::Storage(message) => {
                tracing::error!("Channel storage error: {message}");
                (500, serde_json::json!({ "error": "Internal Server Error" }))
            }
        };
        self.write_json_response(session, status, &body).await
    }

    async fn write_channel_events(
        &self,
        session: &mut Session,
        store: &ChannelStore,
        channel: &str,
        mut after: Option<i64>,
        auth: &ChannelAuthResponse,
    ) -> Result<bool> {
        let mut header = ResponseHeader::build(200, None)?;
        header.insert_header("Content-Type", "text/event-stream")?;
        header.insert_header("Cache-Control", "no-store")?;
        header.insert_header("Connection", "keep-alive")?;
        session
            .write_response_header(Box::new(header), false)
            .await?;

        let keepalive_interval = Duration::from_millis(auth.keepalive_interval_ms.max(1));
        let max_connection_lifetime = Duration::from_millis(auth.max_connection_lifetime_ms.max(1));
        let started_at = tokio::time::Instant::now();
        let mut next_keepalive = started_at + keepalive_interval;

        loop {
            let messages = store.read_after(channel, after, 100).map_err(|error| {
                Error::explain(
                    ErrorType::InternalError,
                    format!("Failed to read channel replay: {error}"),
                )
            })?;

            if !messages.is_empty() {
                for message in messages {
                    let encoded = serde_json::to_string(&message).map_err(|error| {
                        Error::explain(
                            ErrorType::InternalError,
                            format!("Failed to encode SSE payload: {error}"),
                        )
                    })?;
                    let frame = format!(
                        "id: {}\nevent: {}\ndata: {}\n\n",
                        message.id, message.r#type, encoded
                    );
                    session
                        .write_response_body(Some(Bytes::from(frame)), false)
                        .await?;
                    after = Some(
                        message
                            .id
                            .parse::<i64>()
                            .expect("channel ids are always numeric"),
                    );
                }
                next_keepalive = tokio::time::Instant::now() + keepalive_interval;
            }

            if started_at.elapsed() >= max_connection_lifetime {
                session.write_response_body(None, true).await?;
                return Ok(true);
            }

            let now = tokio::time::Instant::now();
            if now >= next_keepalive {
                session
                    .write_response_body(Some(Bytes::from_static(b": keepalive\n\n")), false)
                    .await?;
                next_keepalive = now + keepalive_interval;
            }

            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    async fn write_channel_websocket(
        &self,
        session: &mut Session,
        store: &ChannelStore,
        channel: &str,
        mut after: Option<i64>,
        auth: &ChannelAuthResponse,
    ) -> Result<bool> {
        if !session.as_downstream().is_upgrade_req() {
            return self
                .write_json_response(
                    session,
                    400,
                    &serde_json::json!({ "error": "WebSocket upgrade required" }),
                )
                .await;
        }

        let header = match build_websocket_upgrade_response(session.req_header()) {
            Ok(header) => header,
            Err(error) => return self.write_channel_error(session, error).await,
        };
        session
            .write_response_header(Box::new(header), false)
            .await?;

        let keepalive_interval = Duration::from_millis(auth.keepalive_interval_ms.max(1));
        let max_connection_lifetime = Duration::from_millis(auth.max_connection_lifetime_ms.max(1));
        let started_at = tokio::time::Instant::now();
        let mut next_ping = started_at + keepalive_interval;
        let mut reader = WebSocketFrameReader::default();

        loop {
            let messages = store.read_after(channel, after, 100).map_err(|error| {
                Error::explain(
                    ErrorType::InternalError,
                    format!("Failed to read channel replay: {error}"),
                )
            })?;

            if !messages.is_empty() {
                for message in messages {
                    let encoded = serde_json::to_string(&message).map_err(|error| {
                        Error::explain(
                            ErrorType::InternalError,
                            format!("Failed to encode websocket payload: {error}"),
                        )
                    })?;
                    session
                        .write_response_body(
                            Some(Bytes::from(websocket_text_frame(&encoded))),
                            false,
                        )
                        .await?;
                    after = Some(
                        message
                            .id
                            .parse::<i64>()
                            .expect("channel ids are always numeric"),
                    );
                }
                next_ping = tokio::time::Instant::now() + keepalive_interval;
            }

            if started_at.elapsed() >= max_connection_lifetime {
                session
                    .write_response_body(
                        Some(Bytes::from(websocket_close_frame(
                            1000,
                            "connection expired",
                        ))),
                        true,
                    )
                    .await?;
                return Ok(true);
            }

            let ping_deadline = next_ping;
            let sleep_until = std::cmp::min(ping_deadline, started_at + max_connection_lifetime);
            enum WebSocketAction {
                Read(Option<Bytes>),
                Tick,
            }

            let action = {
                let mut sleep = std::pin::pin!(tokio::time::sleep_until(sleep_until));
                let mut read = std::pin::pin!(session.as_downstream_mut().read_body_or_idle(true));
                tokio::select! {
                    body = &mut read => WebSocketAction::Read(body?),
                    _ = &mut sleep => WebSocketAction::Tick,
                }
            };

            match action {
                WebSocketAction::Read(Some(chunk)) => {
                    reader.extend(&chunk);
                    while let Some(frame) = reader.next_frame().map_err(|error| {
                        Error::explain(
                            ErrorType::InvalidHTTPHeader,
                            format!("Invalid websocket frame: {error}"),
                        )
                    })? {
                        match frame.opcode {
                            0x1 => {
                                let payload =
                                    parse_publish_payload(&frame.payload).map_err(|error| {
                                        Error::explain(
                                            ErrorType::InvalidHTTPHeader,
                                            format!("Invalid websocket publish payload: {error}"),
                                        )
                                    })?;
                                store.append(channel, &payload).map_err(|error| {
                                    Error::explain(
                                        ErrorType::InternalError,
                                        format!(
                                            "Failed to append websocket channel payload: {error}"
                                        ),
                                    )
                                })?;
                            }
                            0x8 => {
                                session
                                    .write_response_body(
                                        Some(Bytes::from(websocket_close_frame(1000, "closing"))),
                                        true,
                                    )
                                    .await?;
                                return Ok(true);
                            }
                            0x9 => {
                                session
                                    .write_response_body(
                                        Some(Bytes::from(websocket_pong_frame(&frame.payload))),
                                        false,
                                    )
                                    .await?;
                            }
                            0xA => {}
                            _ => {
                                session
                                    .write_response_body(
                                        Some(Bytes::from(websocket_close_frame(
                                            1003,
                                            "unsupported frame",
                                        ))),
                                        true,
                                    )
                                    .await?;
                                return Ok(true);
                            }
                        }
                    }
                    next_ping = tokio::time::Instant::now() + keepalive_interval;
                }
                WebSocketAction::Read(None) => return Ok(true),
                WebSocketAction::Tick => {
                    if tokio::time::Instant::now() >= next_ping {
                        session
                            .write_response_body(
                                Some(Bytes::from(websocket_ping_frame(b""))),
                                false,
                            )
                            .await?;
                        next_ping = tokio::time::Instant::now() + keepalive_interval;
                    }
                }
            }
        }
    }

    async fn try_handle_channel_request(
        &self,
        session: &mut Session,
        ctx: &mut RequestCtx,
        app_name: &str,
        path: &str,
        host: &str,
    ) -> Result<bool> {
        let route = match parse_channel_route(path) {
            Ok(Some(route)) => route,
            Ok(None) => {
                return Ok(false);
            }
            Err(error) => {
                return self.write_channel_error(session, error).await;
            }
        };

        let backend = match self.resolve_backend(app_name).await {
            BackendResolution::Ready(backend) => backend,
            BackendResolution::StartupTimeout => {
                return self
                    .write_channel_error(session, ChannelError::AuthUnavailable)
                    .await;
            }
            BackendResolution::StartupFailed
            | BackendResolution::QueueFull
            | BackendResolution::Unavailable
            | BackendResolution::AppMissing => {
                return self
                    .write_channel_error(session, ChannelError::AuthUnavailable)
                    .await;
            }
        };

        let app = match self.lb.app_manager().get_app(&backend.app_name) {
            Some(app) => app,
            None => {
                self.lb
                    .request_completed(&backend.app_name, &backend.instance_id);
                return self
                    .write_channel_error(session, ChannelError::AuthUnavailable)
                    .await;
            }
        };
        let instance = match app.get_instance(&backend.instance_id) {
            Some(instance) => instance,
            None => {
                self.lb
                    .request_completed(&backend.app_name, &backend.instance_id);
                return self
                    .write_channel_error(session, ChannelError::AuthUnavailable)
                    .await;
            }
        };

        let operation = match route.endpoint {
            ChannelEndpoint::Messages => ChannelOperation::Publish,
            ChannelEndpoint::Read if session.as_downstream().is_upgrade_req() => {
                ChannelOperation::Connect
            }
            ChannelEndpoint::Read => ChannelOperation::Subscribe,
        };

        let request_headers = request_headers_to_map(session.req_header());
        let request_url = format!(
            "{}://{}{}",
            if ctx.is_https { "https" } else { "http" },
            host,
            session.req_header().uri
        );

        let auth_result = authorize_channel_request(
            &instance,
            operation.clone(),
            &route.channel,
            request_url,
            session.req_header().method.as_str(),
            request_headers,
        )
        .await;

        self.lb
            .request_completed(&backend.app_name, &backend.instance_id);

        let auth_result = match auth_result {
            Ok(result) => result,
            Err(error) => return self.write_channel_error(session, error).await,
        };

        let store = ChannelStore::new(app_channels_db_path(
            self.lb.app_manager().data_dir(),
            app_name,
        ));
        if let Err(error) = store.sync_channel(&route.channel, &auth_result) {
            return self.write_channel_error(session, error).await;
        }

        match route.endpoint {
            ChannelEndpoint::Messages => {
                if session.req_header().method.as_str() != "POST" {
                    return self
                        .write_json_response(
                            session,
                            405,
                            &serde_json::json!({ "error": "Method not allowed" }),
                        )
                        .await;
                }
                let body = read_request_body(session).await?;
                let payload: ChannelPublishPayload =
                    serde_json::from_slice(&body).map_err(|e| {
                        Error::explain(
                            ErrorType::InvalidHTTPHeader,
                            format!("Invalid JSON body: {e}"),
                        )
                    })?;
                match store.append(&route.channel, &payload) {
                    Ok(message) => {
                        self.write_json_response(session, 200, &serde_json::json!(message))
                            .await
                    }
                    Err(error) => self.write_channel_error(session, error).await,
                }
            }
            ChannelEndpoint::Read if session.as_downstream().is_upgrade_req() => {
                if session.req_header().method.as_str() != "GET" {
                    return self
                        .write_json_response(
                            session,
                            405,
                            &serde_json::json!({ "error": "Method not allowed" }),
                        )
                        .await;
                }
                if auth_result.transport != Some(ChannelTransport::Ws) {
                    return self
                        .write_channel_error(session, ChannelError::Unsupported)
                        .await;
                }
                let cursor = match parse_ws_last_message_id(session.req_header().uri.query())
                    .and_then(|cursor| store.replay_cursor(&route.channel, cursor))
                {
                    Ok(cursor) => cursor,
                    Err(error) => return self.write_channel_error(session, error).await,
                };
                self.write_channel_websocket(session, &store, &route.channel, cursor, &auth_result)
                    .await
            }
            ChannelEndpoint::Read => {
                if session.req_header().method.as_str() != "GET" {
                    return self
                        .write_json_response(
                            session,
                            405,
                            &serde_json::json!({ "error": "Method not allowed" }),
                        )
                        .await;
                }
                let cursor = match parse_message_id_cursor(
                    session
                        .req_header()
                        .headers
                        .get("last-event-id")
                        .and_then(|value| value.to_str().ok()),
                    "Last-Event-ID",
                )
                .and_then(|cursor| store.replay_cursor(&route.channel, cursor))
                {
                    Ok(cursor) => cursor,
                    Err(error) => return self.write_channel_error(session, error).await,
                };
                self.write_channel_events(session, &store, &route.channel, cursor, &auth_result)
                    .await
            }
        }
    }
}

fn request_headers_to_map(request: &RequestHeader) -> HashMap<String, String> {
    request
        .headers
        .iter()
        .filter_map(|(name, value)| {
            value
                .to_str()
                .ok()
                .map(|value| (name.as_str().to_ascii_lowercase(), value.to_string()))
        })
        .collect()
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
        let host = request_host(session.req_header()).to_string();
        let hostname = host.split(':').next().unwrap_or(&host);

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

        if self
            .try_handle_channel_request(session, ctx, &app_name, &path, &host)
            .await?
        {
            return Ok(true);
        }

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

    async fn upstream_response_filter(
        &self,
        _session: &mut Session,
        _upstream_response: &mut ResponseHeader,
        _ctx: &mut Self::CTX,
    ) -> Result<()> {
        Ok(())
    }

    async fn response_filter(
        &self,
        _session: &mut Session,
        _upstream_response: &mut ResponseHeader,
        _ctx: &mut Self::CTX,
    ) -> Result<()> {
        Ok(())
    }

    fn upstream_response_body_filter(
        &self,
        _session: &mut Session,
        _body: &mut Option<Bytes>,
        _end_of_stream: bool,
        _ctx: &mut Self::CTX,
    ) -> Result<Option<Duration>> {
        Ok(None)
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

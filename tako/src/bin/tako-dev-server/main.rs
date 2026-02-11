mod local_ca;
mod local_dns;
mod paths;
mod protocol;
mod proxy;

use std::collections::HashMap;
use std::net::Ipv4Addr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine;
use pingora_core::listeners::tls::TlsSettings;
use pingora_core::prelude::Server;
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream, UnixListener, UnixStream};
use tokio::sync::mpsc;
use tokio::sync::watch;

use tako_socket::{read_json_line, write_json_line};

use protocol::DevEvent;
use protocol::{AppInfo, Request, Response};
use tracing_subscriber::EnvFilter;

const IDLE_EXIT_DELAY: Duration = Duration::from_secs(2);
const TAKO_LOCAL_DOMAIN: &str = "tako.local";
const LOCAL_DNS_LISTEN_ADDR: &str = "127.0.0.1:53535";
const DEV_LOOPBACK_ADDR: &str = "127.77.0.1";
const HTTP_REDIRECT_LISTEN_ADDR: &str = "127.0.0.1:47830";
const DEV_TLS_CERT_FILENAME: &str = "fullchain.pem";
const DEV_TLS_KEY_FILENAME: &str = "privkey.pem";

#[derive(Debug, Clone)]
struct Args {
    listen_addr: String,
    dns_ip: String,
}

fn parse_args() -> Args {
    let mut listen_addr = "127.0.0.1:47831".to_string();
    let mut dns_ip = DEV_LOOPBACK_ADDR.to_string();

    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--listen" => {
                if let Some(v) = it.next()
                    && !v.trim().is_empty()
                {
                    listen_addr = v;
                }
            }
            "--dns-ip" => {
                if let Some(v) = it.next()
                    && !v.trim().is_empty()
                {
                    dns_ip = v;
                }
            }
            _ => {}
        }
    }

    Args {
        listen_addr,
        dns_ip,
    }
}

fn app_host(app_name: &str) -> String {
    format!("{}.{}", app_name, TAKO_LOCAL_DOMAIN)
}

fn default_hosts(app_name: &str) -> Vec<String> {
    vec![app_host(app_name)]
}

fn advertised_https_port(s: &State) -> u16 {
    s.listen_port
}

fn default_socket_path() -> PathBuf {
    paths::tako_home_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join("dev-server.sock")
}

fn port_from_listen(listen: &str, default_port: u16) -> u16 {
    listen
        .rsplit(':')
        .next()
        .and_then(|p| p.parse::<u16>().ok())
        .unwrap_or(default_port)
}

fn listen_port_from_addr(listen: &str) -> u16 {
    port_from_listen(listen, 47831)
}

fn dev_tls_dir_for_home(home: &std::path::Path) -> PathBuf {
    home.join("certs")
}

fn dev_tls_paths_for_dir(dir: &std::path::Path) -> (PathBuf, PathBuf) {
    (
        dir.join(DEV_TLS_CERT_FILENAME),
        dir.join(DEV_TLS_KEY_FILENAME),
    )
}

fn existing_dev_tls_paths(dir: &std::path::Path) -> Option<(PathBuf, PathBuf)> {
    let (cert_path, key_path) = dev_tls_paths_for_dir(dir);
    if cert_path.is_file() && key_path.is_file() {
        Some((cert_path, key_path))
    } else {
        None
    }
}

fn ensure_tcp_listener_can_bind(listen_addr: &str) -> Result<(), Box<dyn std::error::Error>> {
    match std::net::TcpListener::bind(listen_addr) {
        Ok(listener) => {
            drop(listener);
            Ok(())
        }
        Err(e) => Err(format!("dev proxy could not bind on {}: {}", listen_addr, e).into()),
    }
}

fn ensure_dev_tls_files() -> Result<(PathBuf, PathBuf), Box<dyn std::error::Error>> {
    let dir = dev_tls_dir_for_home(&paths::tako_home_dir()?);
    if let Some(existing) = existing_dev_tls_paths(&dir) {
        return Ok(existing);
    }

    let store = local_ca::LocalCAStore::new()?;
    let ca = store.get_or_create_ca()?;

    let cert = ca.generate_leaf_cert_for_names(&["*.tako.local", "tako.local"])?;

    std::fs::create_dir_all(&dir)?;
    let (cert_path, key_path) = dev_tls_paths_for_dir(&dir);
    std::fs::write(&cert_path, cert.cert_pem.as_bytes())?;
    std::fs::write(&key_path, cert.key_pem.as_bytes())?;
    Ok((cert_path, key_path))
}

fn sanitize_app_name(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if (c == '_' || c == '.' || c == '-') && !out.ends_with('-') {
            out.push('-');
        }
    }
    while out.starts_with('-') || out.starts_with(|c: char| c.is_ascii_digit()) {
        out.remove(0);
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        "app".to_string()
    } else {
        out
    }
}

#[derive(Debug, Clone)]
struct Lease {
    app_name: String,
    hosts: Vec<String>,
    upstream_port: u16,
    active: bool,
    expires_at: std::time::Instant,
}

#[derive(Clone, Default)]
struct EventsHub {
    subs: Arc<Mutex<Vec<mpsc::UnboundedSender<Response>>>>,
}

impl EventsHub {
    fn subscribe(&self) -> mpsc::UnboundedReceiver<Response> {
        let (tx, rx) = mpsc::unbounded_channel();
        self.subs.lock().unwrap().push(tx);
        rx
    }

    fn broadcast(&self, r: Response) {
        let mut subs = self.subs.lock().unwrap();
        subs.retain(|tx| tx.send(r.clone()).is_ok());
    }
}

struct State {
    leases: HashMap<String, Lease>,
    lease_by_app: HashMap<String, String>,
    server_token: String,

    events: EventsHub,

    shutdown_tx: watch::Sender<bool>,
    idle_generation: Arc<std::sync::atomic::AtomicU64>,

    routes: proxy::Routes,
    local_dns_enabled: bool,
    local_dns_port: u16,

    listen_port: u16,
    listen_addr: String,
    advertised_ip: String,
    control_clients: u32,
}

impl State {
    fn new(
        shutdown_tx: watch::Sender<bool>,
        routes: proxy::Routes,
        events: EventsHub,
        local_dns_enabled: bool,
        local_dns_port: u16,
        listen_port: u16,
        listen_addr: String,
        advertised_ip: String,
    ) -> Self {
        let mut tok_bytes = [0u8; 32];
        getrandom::fill(&mut tok_bytes).expect("operating system RNG unavailable");
        let server_token = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(tok_bytes);

        Self {
            leases: HashMap::new(),
            lease_by_app: HashMap::new(),
            server_token,

            events,

            shutdown_tx,
            idle_generation: Arc::new(std::sync::atomic::AtomicU64::new(0)),

            routes,
            local_dns_enabled,
            local_dns_port,

            listen_port,
            listen_addr,
            advertised_ip,
            control_clients: 0,
        }
    }

    fn cancel_idle_exit(&mut self) {
        let _ = self
            .idle_generation
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }

    fn schedule_idle_exit(&mut self) {
        let generation = self
            .idle_generation
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            + 1;
        let shutdown_tx = self.shutdown_tx.clone();
        let idle_generation = self.idle_generation.clone();
        tokio::spawn(async move {
            tokio::time::sleep(IDLE_EXIT_DELAY).await;
            if idle_generation.load(std::sync::atomic::Ordering::SeqCst) == generation {
                let _ = shutdown_tx.send(true);
            }
        });
    }
}

struct ControlClientSubscription {
    state: Arc<Mutex<State>>,
}

impl ControlClientSubscription {
    fn register(state: &Arc<Mutex<State>>) -> Self {
        if let Ok(mut s) = state.lock() {
            s.control_clients = s.control_clients.saturating_add(1);
        }
        Self {
            state: state.clone(),
        }
    }
}

impl Drop for ControlClientSubscription {
    fn drop(&mut self) {
        if let Ok(mut s) = self.state.lock() {
            s.control_clients = s.control_clients.saturating_sub(1);
        }
    }
}

fn set_routes_for_hosts(
    s: &mut State,
    hosts: &[String],
    lease_id: &str,
    upstream_port: u16,
    active: bool,
) {
    for host in hosts {
        s.routes
            .set(host.clone(), lease_id.to_string(), upstream_port, active);
    }
}

fn remove_routes_for_hosts(s: &mut State, hosts: &[String]) {
    for host in hosts {
        s.routes.remove(host);
    }
}

fn new_lease_id() -> String {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).expect("operating system RNG unavailable");
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

async fn expire_leases_loop(state: Arc<Mutex<State>>) {
    let mut ticker = tokio::time::interval(Duration::from_millis(200));
    loop {
        ticker.tick().await;

        // Phase 1: collect expired leases.
        let expired = {
            let mut s = match state.lock() {
                Ok(s) => s,
                Err(_) => continue,
            };
            let now = std::time::Instant::now();
            let mut expired: Vec<(String, String)> = Vec::new();
            for (lease_id, lease) in &s.leases {
                if now >= lease.expires_at {
                    expired.push((lease_id.clone(), lease.app_name.clone()));
                }
            }

            if !expired.is_empty() {
                for (lease_id, app_name) in &expired {
                    if let Some(lease) = s.leases.remove(lease_id) {
                        s.lease_by_app.remove(app_name);
                        remove_routes_for_hosts(&mut s, &lease.hosts);
                    }
                }

                let empty = s.leases.is_empty();
                if empty {
                    s.schedule_idle_exit();
                }
            }
            expired
        };

        if expired.is_empty() {
            continue;
        }
    }
}

async fn handle_client(
    stream: UnixStream,
    state: Arc<Mutex<State>>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let (r, mut w) = stream.into_split();
    let mut r = BufReader::new(r);

    loop {
        let Some(req) = (match read_json_line::<_, Request>(&mut r).await {
            Ok(v) => v,
            Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {
                write_resp(
                    &mut w,
                    &Response::Error {
                        message: format!("invalid request: {}", e),
                    },
                )
                .await?;
                continue;
            }
            Err(e) => return Err(e.into()),
        }) else {
            break;
        };

        let resp = match req {
            Request::Ping => Response::Pong,
            Request::GetToken => {
                let s = state.lock().unwrap();
                Response::Token {
                    token: s.server_token.clone(),
                }
            }
            Request::RegisterLease {
                token,
                app_name,
                hosts,
                upstream_port,
                active,
                ttl_ms,
            } => {
                if ttl_ms == 0 {
                    Response::Error {
                        message: "ttl_ms must be > 0".to_string(),
                    }
                } else {
                    let app_name = sanitize_app_name(&app_name);
                    let out = {
                        let mut s = state.lock().unwrap();
                        s.cancel_idle_exit();

                        if token != s.server_token {
                            None
                        } else {
                            let hosts = if hosts.is_empty() {
                                default_hosts(&app_name)
                            } else {
                                hosts
                            };

                            let lease_id = s
                                .lease_by_app
                                .get(&app_name)
                                .cloned()
                                .unwrap_or_else(new_lease_id);

                            let expires_at =
                                std::time::Instant::now() + Duration::from_millis(ttl_ms);

                            // If we're replacing an existing lease for this app, remove old routes first.
                            if let Some(existing) = s.leases.get(&lease_id) {
                                let old_hosts = existing.hosts.clone();
                                remove_routes_for_hosts(&mut s, &old_hosts);
                            }

                            s.leases.insert(
                                lease_id.clone(),
                                Lease {
                                    app_name: app_name.clone(),
                                    hosts: hosts.clone(),
                                    upstream_port,
                                    active,
                                    expires_at,
                                },
                            );
                            s.lease_by_app.insert(app_name.clone(), lease_id.clone());

                            set_routes_for_hosts(&mut s, &hosts, &lease_id, upstream_port, active);

                            let host = hosts
                                .first()
                                .cloned()
                                .unwrap_or_else(|| app_host(&app_name));
                            let public_port = advertised_https_port(&s);
                            Some((lease_id, host, public_port))
                        }
                    };

                    match out {
                        None => Response::Error {
                            message: "invalid token".to_string(),
                        },
                        Some((lease_id, host, public_port)) => {
                            let url = if public_port == 443 {
                                format!("https://{}/", host)
                            } else {
                                format!("https://{}:{}/", host, public_port)
                            };

                            Response::LeaseRegistered {
                                app_name,
                                lease_id,
                                expires_in_ms: ttl_ms,
                                url,
                            }
                        }
                    }
                }
            }
            Request::RenewLease {
                token,
                lease_id,
                ttl_ms,
            } => {
                if ttl_ms == 0 {
                    Response::Error {
                        message: "ttl_ms must be > 0".to_string(),
                    }
                } else {
                    let mut s = state.lock().unwrap();
                    if token != s.server_token {
                        Response::Error {
                            message: "invalid token".to_string(),
                        }
                    } else if let Some(lease) = s.leases.get_mut(&lease_id) {
                        lease.expires_at =
                            std::time::Instant::now() + Duration::from_millis(ttl_ms);
                        Response::LeaseRenewed {
                            lease_id,
                            expires_in_ms: ttl_ms,
                        }
                    } else {
                        Response::Error {
                            message: "unknown lease".to_string(),
                        }
                    }
                }
            }
            Request::SetLeaseActive {
                token,
                lease_id,
                active,
            } => {
                let mut s = state.lock().unwrap();
                if token != s.server_token {
                    Response::Error {
                        message: "invalid token".to_string(),
                    }
                } else if let Some(lease) = s.leases.get_mut(&lease_id) {
                    lease.active = active;
                    let expires_in_ms = lease
                        .expires_at
                        .saturating_duration_since(std::time::Instant::now())
                        .as_millis() as u64;
                    let _ = lease;
                    s.routes.set_active(&lease_id, active);
                    Response::LeaseRenewed {
                        lease_id,
                        expires_in_ms,
                    }
                } else {
                    Response::Error {
                        message: "unknown lease".to_string(),
                    }
                }
            }
            Request::UnregisterLease { token, lease_id } => {
                let mut s = state.lock().unwrap();
                if token != s.server_token {
                    Response::Error {
                        message: "invalid token".to_string(),
                    }
                } else {
                    if let Some(lease) = s.leases.remove(&lease_id) {
                        s.lease_by_app.remove(&lease.app_name);
                        remove_routes_for_hosts(&mut s, &lease.hosts);
                    }

                    if s.leases.is_empty() {
                        s.schedule_idle_exit();
                    }
                    Response::LeaseUnregistered { lease_id }
                }
            }
            Request::SubscribeEvents { token } => {
                let (ok, rx) = {
                    let s = state.lock().unwrap();
                    if token == s.server_token {
                        (true, Some(s.events.subscribe()))
                    } else {
                        (false, None)
                    }
                };
                if !ok {
                    write_resp(
                        &mut w,
                        &Response::Error {
                            message: "invalid token".to_string(),
                        },
                    )
                    .await?;
                    return Ok(());
                }

                let _control_client = ControlClientSubscription::register(&state);
                let mut rx = rx.expect("subscription exists after token check");
                if write_resp(&mut w, &Response::Subscribed).await.is_err() {
                    return Ok(());
                }
                let mut disconnect_probe = [0_u8; 1];
                loop {
                    tokio::select! {
                        maybe_resp = rx.recv() => {
                            let Some(resp) = maybe_resp else {
                                break;
                            };
                            if write_resp(&mut w, &resp).await.is_err() {
                                break;
                            }
                        }
                        read_result = r.read(&mut disconnect_probe) => {
                            match read_result {
                                Ok(0) | Err(_) => break,
                                Ok(_) => {
                                    // SubscribeEvents is a one-way stream.
                                    // Ignore additional client input and keep streaming.
                                }
                            }
                        }
                    }
                }
                return Ok(());
            }
            Request::ListApps => {
                let s = state.lock().unwrap();
                let apps = s
                    .lease_by_app
                    .iter()
                    .filter_map(|(app_name, lease_id)| {
                        let lease = s.leases.get(lease_id)?;
                        Some(AppInfo {
                            lease_id: lease_id.clone(),
                            app_name: app_name.clone(),
                            hosts: lease.hosts.clone(),
                            upstream_port: lease.upstream_port,
                            pid: None,
                        })
                    })
                    .collect::<Vec<_>>();
                Response::Apps { apps }
            }
            Request::Info => {
                let s = state.lock().unwrap();
                Response::Info {
                    info: protocol::DevInfo {
                        listen: s.listen_addr.clone(),
                        port: advertised_https_port(&s),
                        advertised_ip: s.advertised_ip.clone(),
                        local_dns_enabled: s.local_dns_enabled,
                        local_dns_port: s.local_dns_port,
                        control_clients: s.control_clients,
                    },
                }
            }
            Request::StopServer => {
                let mut s = state.lock().unwrap();
                // Remove all leases and clean up routes/forwarders.
                let lease_ids: Vec<String> = s.leases.keys().cloned().collect();
                for lease_id in lease_ids {
                    if let Some(lease) = s.leases.remove(&lease_id) {
                        remove_routes_for_hosts(&mut s, &lease.hosts);
                    }
                }
                s.lease_by_app.clear();
                let _ = s.shutdown_tx.send(true);
                Response::Stopping
            }
        };

        write_resp(&mut w, &resp).await?;
    }

    Ok(())
}

async fn write_resp(
    w: &mut tokio::net::unix::OwnedWriteHalf,
    resp: &Response,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    write_json_line(w, resp).await?;
    Ok(())
}

fn normalize_redirect_host(host_header: &str) -> String {
    let host = host_header.trim();
    if host.is_empty() {
        return "localhost".to_string();
    }
    if let Some(stripped) = host.strip_suffix(":80") {
        return stripped.to_string();
    }
    host.to_string()
}

fn redirect_location(host_header: &str, path: &str) -> String {
    let host = normalize_redirect_host(host_header);
    let path = if path.starts_with('/') { path } else { "/" };
    format!("https://{}{}", host, path)
}

fn parse_http_redirect_target(request: &str) -> (String, String) {
    let mut lines = request.lines();
    let request_line = lines.next().unwrap_or_default();
    let path = request_line.split_whitespace().nth(1).unwrap_or("/");
    let mut host = "";
    for line in lines {
        if line.trim().is_empty() {
            break;
        }
        if let Some(value) = line.strip_prefix("Host:") {
            host = value.trim();
            break;
        }
        if let Some(value) = line.strip_prefix("host:") {
            host = value.trim();
            break;
        }
    }
    (host.to_string(), path.to_string())
}

async fn handle_http_redirect_connection(
    mut stream: TcpStream,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut buf = vec![0u8; 4096];
    let n = tokio::time::timeout(Duration::from_secs(2), stream.read(&mut buf)).await??;
    let req = String::from_utf8_lossy(&buf[..n]).to_string();
    let (host, path) = parse_http_redirect_target(&req);
    let location = redirect_location(&host, &path);
    let response = format!(
        "HTTP/1.1 308 Permanent Redirect\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    );
    stream.write_all(response.as_bytes()).await?;
    let _ = stream.shutdown().await;
    Ok(())
}

async fn start_http_redirect_server(
    listen_addr: &str,
    mut shutdown_rx: watch::Receiver<bool>,
) -> Result<(), Box<dyn std::error::Error>> {
    let listener = TcpListener::bind(listen_addr).await?;
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = shutdown_rx.changed() => {
                    if *shutdown_rx.borrow() {
                        break;
                    }
                }
                accepted = listener.accept() => {
                    match accepted {
                        Ok((stream, _)) => {
                            tokio::spawn(async move {
                                if let Err(e) = handle_http_redirect_connection(stream).await {
                                    tracing::warn!(error = %e, "http redirect handler failed");
                                }
                            });
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, "http redirect accept failed");
                        }
                    }
                }
            }
        }
    });
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        // Default to quiet. `RUST_LOG` can be used to enable info/debug output.
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn")),
        )
        .with_target(false)
        .init();

    let args = parse_args();

    // Shared route table between the unix-socket control plane and the proxy.
    let routes = proxy::Routes::default();
    let events = EventsHub::default();

    // Events channel from Pingora runtime -> control-plane subscribers.
    let (ev_tx, mut ev_rx) = mpsc::unbounded_channel::<DevEvent>();
    {
        let events = events.clone();
        tokio::spawn(async move {
            while let Some(ev) = ev_rx.recv().await {
                events.broadcast(Response::Event { event: ev });
            }
        });
    }

    // Start the Pingora proxy in a dedicated thread.
    // We exit the whole process when the control-plane tells us to shut down.
    {
        let listen = args.listen_addr.clone();
        ensure_tcp_listener_can_bind(&listen)?;

        let proxy = proxy::DevProxy {
            routes: routes.clone(),
            events: ev_tx.clone(),
        };

        let mut server = Server::new(None)?;
        server.bootstrap();
        let mut svc = pingora_proxy::http_proxy_service(&server.configuration, proxy);

        let (cert_path, key_path) = ensure_dev_tls_files()?;
        let cert_path_str = cert_path.to_string_lossy().to_string();
        let key_path_str = key_path.to_string_lossy().to_string();
        let tls = TlsSettings::intermediate(&cert_path_str, &key_path_str)?;
        svc.add_tls_with_settings(&listen, None, tls);

        server.add_service(svc);

        std::thread::spawn(move || {
            server.run_forever();
        });
    }
    let listen_addr = args.listen_addr;
    let listen_port = listen_port_from_addr(&listen_addr);

    let loopback_ip = args.dns_ip.parse::<Ipv4Addr>()?;
    let local_dns = local_dns::start(routes.clone(), LOCAL_DNS_LISTEN_ADDR, loopback_ip).await?;
    tracing::info!(listen = %local_dns.listen_addr(), "local DNS server listening");

    let sock = default_socket_path();
    if let Some(parent) = sock.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    // Avoid clobbering an existing running dev server.
    // Unconditionally unlinking the socket is dangerous: a second instance can remove the socket
    // file, breaking clients of the first instance, and then fail to start.
    if tokio::fs::try_exists(&sock).await.unwrap_or(false) {
        match tokio::net::UnixStream::connect(&sock).await {
            Ok(_) => {
                return Err(format!(
                    "dev server already running (socket exists at {})",
                    sock.display()
                )
                .into());
            }
            Err(e) => {
                // If the socket file exists but nothing is listening, remove it.
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::ConnectionRefused
                        | std::io::ErrorKind::NotConnected
                        | std::io::ErrorKind::ConnectionReset
                ) {
                    let _ = tokio::fs::remove_file(&sock).await;
                }
            }
        }
    }
    let listener = UnixListener::bind(&sock)?;
    tracing::info!(sock = %sock.display(), "tako-dev-server listening");

    let (shutdown_tx, mut shutdown_rx) = watch::channel(false);
    start_http_redirect_server(HTTP_REDIRECT_LISTEN_ADDR, shutdown_rx.clone()).await?;
    tracing::info!(listen = %HTTP_REDIRECT_LISTEN_ADDR, "http redirect server listening");
    let st = State::new(
        shutdown_tx,
        routes,
        events,
        true,
        local_dns.port(),
        listen_port,
        listen_addr,
        args.dns_ip,
    );

    let state = Arc::new(Mutex::new(st));

    // Lease cleanup loop.
    {
        let state = state.clone();
        tokio::spawn(async move { expire_leases_loop(state).await });
    }

    loop {
        tokio::select! {
            _ = shutdown_rx.changed() => {
                if *shutdown_rx.borrow() {
                    tracing::info!("tako-dev-server shutting down");
                    std::process::exit(0);
                }
            }
            accepted = listener.accept() => {
                let (stream, _) = accepted?;
                let state = state.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle_client(stream, state).await {
                        tracing::warn!(err = %e, "client handler error");
                    }
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use tokio::io::AsyncBufReadExt;
    use tokio::io::AsyncWriteExt;
    use tokio::time::Duration;

    async fn query_control_clients(state: Arc<Mutex<State>>) -> u32 {
        let (a, b) = tokio::net::UnixStream::pair().unwrap();
        let h = tokio::spawn(async move { handle_client(a, state).await });

        let (r, mut w) = b.into_split();
        w.write_all(b"{\"type\":\"Info\"}\n").await.unwrap();
        let mut lines = BufReader::new(r).lines();
        let line = lines.next_line().await.unwrap().unwrap();
        let resp: Response = serde_json::from_str(&line).unwrap();

        drop(w);
        h.await.unwrap().unwrap();

        match resp {
            Response::Info { info } => info.control_clients,
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn token_then_register_lease_roundtrip() {
        let (a, b) = tokio::net::UnixStream::pair().unwrap();
        let (shutdown_tx, _shutdown_rx) = watch::channel(false);
        let st = State::new(
            shutdown_tx,
            proxy::Routes::default(),
            EventsHub::default(),
            true,
            53535,
            8443,
            "127.0.0.1:8443".to_string(),
            "127.0.0.1".to_string(),
        );
        let state = Arc::new(Mutex::new(st));
        let h = tokio::spawn(async move { handle_client(a, state).await });

        let (r, mut w) = b.into_split();
        w.write_all(b"{\"type\":\"GetToken\"}\n").await.unwrap();
        let mut lines = BufReader::new(r).lines();
        let tok_line = lines.next_line().await.unwrap().unwrap();
        let tok_resp: Response = serde_json::from_str(&tok_line).unwrap();
        let token = match tok_resp {
            Response::Token { token } => token,
            other => panic!("unexpected: {other:?}"),
        };

        let req = serde_json::json!({
            "type": "RegisterLease",
            "token": token,
            "app_name": "my-app",
            "hosts": ["my-app.tako.local"],
            "upstream_port": 1234,
            "ttl_ms": 1000
        });
        w.write_all(req.to_string().as_bytes()).await.unwrap();
        w.write_all(b"\n").await.unwrap();

        let reg_line = lines.next_line().await.unwrap().unwrap();
        let reg: Response = serde_json::from_str(&reg_line).unwrap();
        match reg {
            Response::LeaseRegistered {
                app_name,
                lease_id,
                expires_in_ms,
                url,
            } => {
                assert_eq!(app_name, "my-app");
                assert!(!lease_id.is_empty());
                assert_eq!(expires_in_ms, 1000);
                assert!(url.contains("my-app.tako.local"));
            }
            other => panic!("unexpected: {other:?}"),
        }

        drop(w);
        h.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn register_lease_rejects_invalid_token() {
        let (a, b) = tokio::net::UnixStream::pair().unwrap();
        let (shutdown_tx, _shutdown_rx) = watch::channel(false);
        let st = State::new(
            shutdown_tx,
            proxy::Routes::default(),
            EventsHub::default(),
            true,
            53535,
            8443,
            "127.0.0.1:8443".to_string(),
            "127.0.0.1".to_string(),
        );
        let state = Arc::new(Mutex::new(st));
        let h = tokio::spawn(async move { handle_client(a, state).await });

        let (r, mut w) = b.into_split();
        let req = serde_json::json!({
            "type": "RegisterLease",
            "token": "bad",
            "app_name": "a",
            "hosts": ["a.tako.local"],
            "upstream_port": 1234,
            "ttl_ms": 1000
        });
        w.write_all(req.to_string().as_bytes()).await.unwrap();
        w.write_all(b"\n").await.unwrap();

        let mut lines = BufReader::new(r).lines();
        let line = lines.next_line().await.unwrap().unwrap();
        let resp: Response = serde_json::from_str(&line).unwrap();
        match resp {
            Response::Error { message } => assert!(message.contains("token")),
            other => panic!("unexpected: {other:?}"),
        }

        drop(w);
        h.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn info_reports_connected_control_clients() {
        let (a, b) = tokio::net::UnixStream::pair().unwrap();
        let (shutdown_tx, _shutdown_rx) = watch::channel(false);
        let st = State::new(
            shutdown_tx,
            proxy::Routes::default(),
            EventsHub::default(),
            true,
            53535,
            8443,
            "127.0.0.1:8443".to_string(),
            "127.0.0.1".to_string(),
        );
        let state = Arc::new(Mutex::new(st));
        let h = tokio::spawn({
            let state = state.clone();
            async move { handle_client(a, state).await }
        });

        let (r, mut w) = b.into_split();
        let mut lines = BufReader::new(r).lines();
        w.write_all(b"{\"type\":\"GetToken\"}\n").await.unwrap();
        let tok_line = lines.next_line().await.unwrap().unwrap();
        let tok_resp: Response = serde_json::from_str(&tok_line).unwrap();
        let token = match tok_resp {
            Response::Token { token } => token,
            other => panic!("unexpected: {other:?}"),
        };

        let req = serde_json::json!({
            "type": "SubscribeEvents",
            "token": token,
        });
        w.write_all(req.to_string().as_bytes()).await.unwrap();
        w.write_all(b"\n").await.unwrap();

        let sub_line = lines.next_line().await.unwrap().unwrap();
        let sub_resp: Response = serde_json::from_str(&sub_line).unwrap();
        assert!(matches!(sub_resp, Response::Subscribed));

        let clients = query_control_clients(state.clone()).await;
        assert_eq!(clients, 1);

        drop(lines);
        drop(w);

        tokio::time::timeout(Duration::from_secs(1), h)
            .await
            .expect("subscribe handler should exit")
            .unwrap()
            .unwrap();

        let clients = query_control_clients(state).await;
        assert_eq!(clients, 0);
    }

    #[test]
    fn redirect_location_strips_default_http_port() {
        let location = redirect_location("bun-example.tako.local:80", "/hello");
        assert_eq!(location, "https://bun-example.tako.local/hello");
    }

    #[test]
    fn redirect_location_keeps_non_default_port() {
        let location = redirect_location("bun-example.tako.local:8080", "/");
        assert_eq!(location, "https://bun-example.tako.local:8080/");
    }

    #[test]
    fn dev_tls_dir_uses_certs_root() {
        let home = PathBuf::from("/tmp/tako-home");
        assert_eq!(
            dev_tls_dir_for_home(&home),
            PathBuf::from("/tmp/tako-home/certs")
        );
    }

    #[test]
    fn dev_tls_paths_use_expected_filenames() {
        let dir = PathBuf::from("/tmp/tako-home/certs");
        let (cert, key) = dev_tls_paths_for_dir(&dir);
        assert_eq!(cert, PathBuf::from("/tmp/tako-home/certs/fullchain.pem"));
        assert_eq!(key, PathBuf::from("/tmp/tako-home/certs/privkey.pem"));
    }

    #[test]
    fn existing_dev_tls_paths_returns_pair_when_both_exist() {
        let temp = tempfile::TempDir::new().unwrap();
        let certs_dir = temp.path().join("certs");
        std::fs::create_dir_all(&certs_dir).unwrap();
        let (cert, key) = dev_tls_paths_for_dir(&certs_dir);
        std::fs::write(&cert, "cert").unwrap();
        std::fs::write(&key, "key").unwrap();

        assert_eq!(existing_dev_tls_paths(&certs_dir), Some((cert, key)));
    }

    #[test]
    fn existing_dev_tls_paths_returns_none_when_any_file_missing() {
        let temp = tempfile::TempDir::new().unwrap();
        let certs_dir = temp.path().join("certs");
        std::fs::create_dir_all(&certs_dir).unwrap();
        let (cert, _key) = dev_tls_paths_for_dir(&certs_dir);
        std::fs::write(&cert, "cert").unwrap();
        assert!(existing_dev_tls_paths(&certs_dir).is_none());
    }

    #[test]
    fn ensure_tcp_listener_can_bind_succeeds_when_port_is_available() {
        let Ok(listener) = std::net::TcpListener::bind(("127.0.0.1", 0)) else {
            return;
        };
        let addr = listener.local_addr().unwrap();
        drop(listener);
        assert!(ensure_tcp_listener_can_bind(&addr.to_string()).is_ok());
    }

    #[test]
    fn ensure_tcp_listener_can_bind_reports_error_when_port_in_use() {
        let Ok(listener) = std::net::TcpListener::bind(("127.0.0.1", 0)) else {
            return;
        };
        let addr = listener.local_addr().unwrap();
        let err = ensure_tcp_listener_can_bind(&addr.to_string())
            .unwrap_err()
            .to_string();
        assert!(err.contains("dev proxy could not bind on"));
        assert!(err.contains(&addr.to_string()));
        drop(listener);
    }
}

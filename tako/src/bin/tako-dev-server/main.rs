mod local_ca;
mod local_dns;
mod paths;
mod protocol;
mod proxy;
mod state;

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read as _, Seek, Write as _};
use std::net::Ipv4Addr;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use openssl::pkey::PKey;
use openssl::ssl::SslRef;
use openssl::x509::X509;
use pingora_core::listeners::TlsAccept;
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
const TAKO_DEV_DOMAIN: &str = "tako";
const LOCAL_DNS_LISTEN_ADDR: &str = "127.0.0.1:53535";
const DEV_LOOPBACK_ADDR: &str = "127.77.0.1";
const HTTP_REDIRECT_LISTEN_ADDR: &str = "127.0.0.1:47830";

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

/// Acquire an exclusive PID file lock using `flock(LOCK_EX | LOCK_NB)`.
///
/// If another instance holds the lock, sends SIGTERM to that PID and retries
/// for up to 2 seconds before giving up.
///
/// The caller must keep the returned `File` alive for the lifetime of the
/// process — the kernel releases the advisory lock when the fd is closed
/// (including on crash / SIGKILL).
fn acquire_pid_lock(pid_path: &Path) -> Result<File, Box<dyn std::error::Error>> {
    let mut file = File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(pid_path)?;

    // Try non-blocking exclusive lock.
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc == 0 {
        // Lock acquired immediately.
        write_pid(&mut file)?;
        return Ok(file);
    }

    let err = std::io::Error::last_os_error();
    if err.raw_os_error() != Some(libc::EWOULDBLOCK) {
        return Err(format!("flock({}) failed: {}", pid_path.display(), err).into());
    }

    // Another instance holds the lock. Read its PID and send SIGTERM.
    let mut contents = String::new();
    file.read_to_string(&mut contents)?;
    if let Ok(old_pid) = contents.trim().parse::<i32>() {
        if old_pid > 0 {
            unsafe {
                libc::kill(old_pid, libc::SIGTERM);
            }
        }
    }

    // Retry with a short sleep loop (up to 2 seconds).
    const MAX_RETRIES: u32 = 20;
    for _ in 0..MAX_RETRIES {
        std::thread::sleep(Duration::from_millis(100));
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if rc == 0 {
            write_pid(&mut file)?;
            return Ok(file);
        }
    }

    Err(format!(
        "could not acquire dev-server lock at {} after sending SIGTERM (another instance may be stuck)",
        pid_path.display()
    )
    .into())
}

fn write_pid(file: &mut File) -> std::io::Result<()> {
    file.set_len(0)?;
    file.seek(std::io::SeekFrom::Start(0))?;
    write!(file, "{}", std::process::id())?;
    file.sync_all()?;
    Ok(())
}

fn app_host(app_name: &str) -> String {
    format!("{}.{}", app_name, TAKO_DEV_DOMAIN)
}

fn default_hosts(app_name: &str) -> Vec<String> {
    vec![app_host(app_name)]
}

fn advertised_https_port(s: &State) -> u16 {
    if s.advertised_ip == DEV_LOOPBACK_ADDR {
        443
    } else {
        s.listen_port
    }
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

fn ensure_tcp_listener_can_bind(listen_addr: &str) -> Result<(), Box<dyn std::error::Error>> {
    match std::net::TcpListener::bind(listen_addr) {
        Ok(listener) => {
            drop(listener);
            Ok(())
        }
        Err(e) => Err(format!("dev proxy could not bind on {}: {}", listen_addr, e).into()),
    }
}

fn load_or_create_ca() -> Result<local_ca::LocalCA, Box<dyn std::error::Error>> {
    let store = local_ca::LocalCAStore::new()?;
    Ok(store.get_or_create_ca()?)
}

/// Dynamic TLS certificate resolver for development.
///
/// OpenSSL rejects `*.tako` wildcards because single-label TLDs are treated
/// like `*.com` — "too broad". So we generate a per-hostname cert on the fly
/// during the TLS handshake using the local CA, and cache it for reuse.
struct DevCertResolver {
    ca: local_ca::LocalCA,
    cache: Mutex<HashMap<String, (X509, PKey<openssl::pkey::Private>)>>,
}

impl DevCertResolver {
    fn new(ca: local_ca::LocalCA) -> Self {
        Self {
            ca,
            cache: Mutex::new(HashMap::new()),
        }
    }

    fn get_or_create_cert(&self, hostname: &str) -> Option<(X509, PKey<openssl::pkey::Private>)> {
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

/// Split a route pattern like "app.tako/api" into ("app.tako", Some("/api")).
fn split_route_pattern(route: &str) -> (&str, Option<&str>) {
    match route.find('/') {
        Some(idx) => (&route[..idx], Some(&route[idx..])),
        None => (route, None),
    }
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

    db: Option<state::DevStateStore>,
}

impl State {
    #[allow(clippy::too_many_arguments)]
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
        Self {
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

            db: None,
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
            Request::SubscribeEvents => {
                let rx = {
                    let s = state.lock().unwrap();
                    s.events.subscribe()
                };

                let _control_client = ControlClientSubscription::register(&state);
                let mut rx = rx;
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
            Request::RegisterApp {
                project_dir,
                app_name,
                hosts,
                upstream_port,
                command,
                env,
                log_path,
                client_pid,
            } => {
                let app_name = sanitize_app_name(&app_name);
                let mut s = state.lock().unwrap();
                s.cancel_idle_exit();

                let hosts = if hosts.is_empty() {
                    default_hosts(&app_name)
                } else {
                    hosts
                };

                if let Some(db) = &s.db {
                    let _ = db.register_app(
                        &project_dir,
                        &app_name,
                        &hosts,
                        upstream_port,
                        &state::AppStatus::Running,
                        &command,
                        &env,
                        &log_path,
                        client_pid,
                    );
                }

                let route_id = format!("reg:{}", project_dir);
                s.routes
                    .set_routes(route_id, hosts.clone(), upstream_port, true);

                let host = hosts
                    .first()
                    .cloned()
                    .unwrap_or_else(|| app_host(&app_name));
                let public_port = advertised_https_port(&s);
                let url = if public_port == 443 {
                    format!("https://{}/", host)
                } else {
                    format!("https://{}:{}/", host, public_port)
                };

                s.events.broadcast(Response::Event {
                    event: protocol::DevEvent::AppStatusChanged {
                        project_dir: project_dir.clone(),
                        app_name: app_name.clone(),
                        status: "running".to_string(),
                    },
                });

                Response::AppRegistered {
                    app_name,
                    project_dir,
                    url,
                }
            }
            Request::UnregisterApp { project_dir } => {
                let mut s = state.lock().unwrap();
                let mut app_name = String::new();

                if let Some(db) = &s.db {
                    if let Ok(Some(app)) = db.get_app(&project_dir) {
                        app_name = app.app_name.clone();
                        let _ = db.set_status(&project_dir, &state::AppStatus::Stopped);
                        let _ = db.set_pid(&project_dir, None);
                    }
                }

                let route_id = format!("reg:{}", project_dir);
                s.routes.remove_app(&route_id);

                if !app_name.is_empty() {
                    s.events.broadcast(Response::Event {
                        event: protocol::DevEvent::AppStatusChanged {
                            project_dir: project_dir.clone(),
                            app_name: app_name.clone(),
                            status: "stopped".to_string(),
                        },
                    });
                }

                // Check if daemon should idle-exit.
                if let Some(db) = &s.db {
                    if db.list_active_apps().ok().is_none_or(|a| a.is_empty()) {
                        s.schedule_idle_exit();
                    }
                }

                Response::AppUnregistered { project_dir }
            }
            Request::RestartApp { project_dir } => {
                let s = state.lock().unwrap();
                let app_name =
                    s.db.as_ref()
                        .and_then(|db| db.get_app(&project_dir).ok().flatten())
                        .map(|a| a.app_name.clone())
                        .unwrap_or_default();
                s.events.broadcast(Response::Event {
                    event: protocol::DevEvent::RestartRequested {
                        project_dir: project_dir.clone(),
                        app_name,
                    },
                });
                Response::AppRestarting { project_dir }
            }
            Request::SetAppStatus {
                project_dir,
                status,
            } => {
                let s = state.lock().unwrap();
                let parsed = state::AppStatus::from_str(&status);
                match parsed {
                    None => Response::Error {
                        message: format!("unknown status: {status}"),
                    },
                    Some(app_status) => {
                        if let Some(db) = &s.db {
                            let _ = db.set_status(&project_dir, &app_status);
                        }

                        let route_id = format!("reg:{}", project_dir);
                        let active = app_status == state::AppStatus::Running;
                        s.routes.set_active(&route_id, active);

                        let mut app_name = String::new();
                        if let Some(db) = &s.db {
                            if let Ok(Some(app)) = db.get_app(&project_dir) {
                                app_name = app.app_name.clone();
                            }
                        }

                        if !app_name.is_empty() {
                            s.events.broadcast(Response::Event {
                                event: protocol::DevEvent::AppStatusChanged {
                                    project_dir: project_dir.clone(),
                                    app_name,
                                    status: status.clone(),
                                },
                            });
                        }

                        Response::AppStatusUpdated {
                            project_dir,
                            status,
                        }
                    }
                }
            }
            Request::HandoffApp { project_dir, pid } => {
                let s = state.lock().unwrap();
                if let Some(db) = &s.db {
                    let _ = db.set_pid(&project_dir, Some(pid));
                    let _ = db.set_status(&project_dir, &state::AppStatus::Running);
                }

                // Spawn a PID monitor: if the process dies, mark as idle.
                let state_for_monitor = state.clone();
                let dir_for_monitor = project_dir.clone();
                tokio::spawn(async move {
                    monitor_handoff_pid(state_for_monitor, dir_for_monitor, pid).await;
                });

                Response::AppHandedOff { project_dir }
            }
            Request::ListRegisteredApps => {
                let s = state.lock().unwrap();
                let apps = if let Some(db) = &s.db {
                    db.list_apps()
                        .unwrap_or_default()
                        .into_iter()
                        .map(|a| protocol::RegisteredAppInfo {
                            project_dir: a.project_dir,
                            app_name: a.app_name,
                            hosts: a.hosts,
                            upstream_port: a.upstream_port,
                            status: match a.status {
                                state::AppStatus::Running => "running".to_string(),
                                state::AppStatus::Idle => "idle".to_string(),
                                state::AppStatus::Stopped => "stopped".to_string(),
                            },
                            pid: a.pid,
                            client_pid: a.client_pid,
                        })
                        .collect()
                } else {
                    Vec::new()
                };
                Response::RegisteredApps { apps }
            }
            Request::ListApps => {
                let s = state.lock().unwrap();
                let apps = if let Some(db) = &s.db {
                    db.list_active_apps()
                        .unwrap_or_default()
                        .into_iter()
                        .map(|a| AppInfo {
                            app_name: a.app_name,
                            hosts: a.hosts,
                            upstream_port: a.upstream_port,
                            pid: a.pid,
                        })
                        .collect()
                } else {
                    Vec::new()
                };
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
                let s = state.lock().unwrap();
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

/// Monitor a handed-off process by PID. When the process exits, mark the app as idle.
async fn monitor_handoff_pid(state: Arc<Mutex<State>>, project_dir: String, pid: u32) {
    let sysinfo_pid = sysinfo::Pid::from_u32(pid);
    let mut sys = sysinfo::System::new();
    loop {
        tokio::time::sleep(Duration::from_secs(2)).await;

        sys.refresh_processes(sysinfo::ProcessesToUpdate::Some(&[sysinfo_pid]), false);
        if sys.process(sysinfo_pid).is_none() {
            let s = state.lock().unwrap();
            if let Some(db) = &s.db {
                let _ = db.set_status(&project_dir, &state::AppStatus::Idle);
                let _ = db.set_pid(&project_dir, None);
            }
            let route_id = format!("reg:{}", project_dir);
            s.routes.set_active(&route_id, false);
            tracing::info!(project_dir = %project_dir, pid = pid, "handoff'd process exited, marking idle");
            break;
        }
    }
}

/// Spawn an app process for wake-on-request. Returns the child PID on success.
async fn spawn_app_for_wake(
    app: &state::DevApp,
) -> Result<tokio::process::Child, Box<dyn std::error::Error + Send + Sync>> {
    if app.command.is_empty() {
        return Err("app has empty command".into());
    }

    let mut cmd = tokio::process::Command::new(&app.command[0]);
    if app.command.len() > 1 {
        cmd.args(&app.command[1..]);
    }
    cmd.current_dir(&app.project_dir)
        .env("PORT", app.upstream_port.to_string())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    for (k, v) in &app.env {
        cmd.env(k, v);
    }

    let mut child = cmd.spawn()?;

    // Drain stdout/stderr to the app's JSONL log store.
    let log_path = std::path::PathBuf::from(&app.log_path);
    if let Some(stdout) = child.stdout.take() {
        let log_path = log_path.clone();
        tokio::spawn(async move {
            drain_pipe_to_log(stdout, &log_path).await;
        });
    }
    if let Some(stderr) = child.stderr.take() {
        let log_path = log_path.clone();
        tokio::spawn(async move {
            drain_pipe_to_log(stderr, &log_path).await;
        });
    }

    Ok(child)
}

async fn drain_pipe_to_log(pipe: impl tokio::io::AsyncRead + Unpin, log_path: &std::path::Path) {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
    let Ok(mut file) = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
        .await
    else {
        return;
    };
    let reader = tokio::io::BufReader::new(pipe);
    let mut lines = reader.lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let ts = time::OffsetDateTime::now_utc()
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap_or_default();
        let json = serde_json::json!({
            "timestamp": ts,
            "level": "Info",
            "scope": "app",
            "message": line,
        });
        let mut encoded = json.to_string();
        encoded.push('\n');
        let _ = file.write_all(encoded.as_bytes()).await;
    }
}

/// Handle wake-on-request: when a RequestStarted event arrives for an idle app,
/// spawn the process and mark it as running.
async fn handle_wake_on_request(state: Arc<Mutex<State>>, host: String, path: String) {
    // Quick check under lock: is the route already active?
    // Also grab the DB path so we can query outside the lock.
    let (route_active, db_path) = {
        let s = state.lock().unwrap();
        let active = s.routes.lookup(&host, &path).is_some_and(|(_, _, a, _)| a);
        let db_p = s.db.as_ref().map(|db| db.path());
        (active, db_p)
    };

    if route_active {
        return;
    }
    let Some(db_path) = db_path else { return };

    // Query SQLite outside the mutex to avoid holding the lock during I/O.
    let app_info = {
        let Ok(db) = state::DevStateStore::open(&db_path) else {
            return;
        };
        let apps = db.list_active_apps().unwrap_or_default();
        apps.into_iter().find(|a| {
            if a.status != state::AppStatus::Idle {
                return false;
            }
            // Check if any of the app's stored route patterns match the request.
            a.hosts.iter().any(|route_pattern| {
                let (pat_host, pat_path) = split_route_pattern(route_pattern);
                let host_ok = pat_host == host
                    || (pat_host.starts_with("*.")
                        && host
                            .split_once('.')
                            .is_some_and(|(_, rest)| format!("*.{rest}") == pat_host));
                if !host_ok {
                    return false;
                }
                match pat_path {
                    None => true,
                    Some(_) => {
                        // If the route has a path component, it will match
                        // at the proxy layer. For wake purposes, hostname
                        // match is sufficient — the app handles all its paths.
                        true
                    }
                }
            })
        })
    };

    let Some(app) = app_info else { return };

    tracing::info!(
        app_name = %app.app_name,
        host = %host,
        "waking idle app on request"
    );

    match spawn_app_for_wake(&app).await {
        Ok(mut child) => {
            let pid = child.id();
            let project_dir = app.project_dir.clone();

            {
                let s = state.lock().unwrap();
                if let Some(db) = &s.db {
                    let _ = db.set_status(&project_dir, &state::AppStatus::Running);
                    if let Some(pid) = pid {
                        let _ = db.set_pid(&project_dir, Some(pid));
                    }
                }
                let route_id = format!("reg:{}", project_dir);
                s.routes.set_active(&route_id, true);
            }

            // Monitor the spawned process.
            if let Some(pid) = pid {
                let state = state.clone();
                let project_dir = project_dir.clone();
                tokio::spawn(async move {
                    let _ = child.wait().await;
                    let s = state.lock().unwrap();
                    if let Some(db) = &s.db {
                        let _ = db.set_status(&project_dir, &state::AppStatus::Idle);
                        let _ = db.set_pid(&project_dir, None);
                    }
                    let route_id = format!("reg:{}", project_dir);
                    s.routes.set_active(&route_id, false);
                    tracing::info!(project_dir = %project_dir, pid = pid, "wake-spawned process exited, marking idle");
                });
            }
        }
        Err(e) => {
            tracing::warn!(
                app_name = %app.app_name,
                error = %e,
                "failed to spawn app for wake-on-request"
            );
        }
    }
}

/// Periodic cleanup: remove apps whose project_dir no longer has a tako.toml.
async fn stale_app_cleanup_loop(state: Arc<Mutex<State>>) {
    let mut ticker = tokio::time::interval(Duration::from_secs(60));
    loop {
        ticker.tick().await;
        // Get the DB path under lock, then do the (potentially slow) FS cleanup outside it.
        let db_path = {
            let s = state.lock().unwrap();
            s.db.as_ref().map(|db| db.path())
        };
        let Some(db_path) = db_path else { continue };
        let Ok(db) = state::DevStateStore::open(&db_path) else {
            continue;
        };
        if let Ok(removed) = db.cleanup_stale() {
            if !removed.is_empty() {
                let s = state.lock().unwrap();
                for dir in &removed {
                    let route_id = format!("reg:{}", dir);
                    s.routes.set_active(&route_id, false);
                }
                tracing::info!(count = removed.len(), "cleaned up stale app registrations");
            }
        }
    }
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

    // Acquire an exclusive PID lock. If another instance is running, SIGTERM it.
    let pid_path = paths::tako_home_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join("dev-server.pid");
    if let Some(parent) = pid_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let _pid_lock = acquire_pid_lock(&pid_path)?;

    // Shared route table between the unix-socket control plane and the proxy.
    let routes = proxy::Routes::default();
    let events = EventsHub::default();

    // Events channel from Pingora runtime -> control-plane subscribers.
    // Also triggers wake-on-request for idle registered apps.
    let (ev_tx, mut ev_rx) = mpsc::unbounded_channel::<DevEvent>();

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

        // Dynamic per-SNI cert generation: OpenSSL rejects `*.tako` wildcards
        // (single-label TLD), so we generate a cert per hostname on the fly.
        let ca = load_or_create_ca()?;
        let resolver = DevCertResolver::new(ca);
        let callbacks: Box<dyn TlsAccept + Send + Sync> = Box::new(resolver);
        let mut tls = TlsSettings::with_callbacks(callbacks)?;
        tls.enable_h2();
        svc.add_tls_with_settings(&listen, None, tls);

        server.add_service(svc);

        std::thread::spawn(move || {
            server.run_forever();
        });
    }
    let listen_addr = args.listen_addr;
    let listen_port = listen_port_from_addr(&listen_addr);

    let loopback_ip = args.dns_ip.parse::<Ipv4Addr>()?;
    let local_dns = local_dns::start(LOCAL_DNS_LISTEN_ADDR, loopback_ip).await?;
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
    let events_for_relay = events.clone();
    let mut st = State::new(
        shutdown_tx,
        routes,
        events,
        true,
        local_dns.port(),
        listen_port,
        listen_addr,
        args.dns_ip,
    );

    // Open the SQLite state store and populate routes from persisted apps.
    let db_path = paths::tako_home_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join("dev-server.db");
    match state::DevStateStore::open(db_path) {
        Ok(db) => {
            if let Ok(active_apps) = db.list_active_apps() {
                for app in &active_apps {
                    let id = format!("reg:{}", app.project_dir);
                    let active = app.status == state::AppStatus::Running;
                    st.routes
                        .set_routes(id, app.hosts.clone(), app.upstream_port, active);
                }
                if !active_apps.is_empty() {
                    st.cancel_idle_exit();
                }
            }
            st.db = Some(db);
        }
        Err(e) => {
            tracing::warn!(error = %e, "failed to open dev state store; registration disabled");
        }
    }

    let state = Arc::new(Mutex::new(st));

    // Events relay: broadcast to subscribers + trigger wake-on-request.
    {
        let events = events_for_relay;
        let state = state.clone();
        tokio::spawn(async move {
            while let Some(ev) = ev_rx.recv().await {
                if let DevEvent::RequestStarted { ref host, ref path } = ev {
                    let state = state.clone();
                    let host = host.clone();
                    let path = path.clone();
                    tokio::spawn(async move {
                        handle_wake_on_request(state, host, path).await;
                    });
                }
                events.broadcast(Response::Event { event: ev });
            }
        });
    }

    // Stale app cleanup loop.
    {
        let state = state.clone();
        tokio::spawn(async move { stale_app_cleanup_loop(state).await });
    }

    loop {
        tokio::select! {
            _ = shutdown_rx.changed() => {
                if *shutdown_rx.borrow() {
                    tracing::info!("tako-dev-server shutting down");
                    let _ = std::fs::remove_file(&sock);
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
    async fn register_app_roundtrip() {
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
        let mut lines = BufReader::new(r).lines();

        let req = serde_json::json!({
            "type": "RegisterApp",
            "project_dir": "/tmp/test-proj",
            "app_name": "my-app",
            "hosts": ["my-app.tako"],
            "upstream_port": 1234,
            "command": ["node", "index.js"],
            "env": {},
            "log_path": "/tmp/test.jsonl"
        });
        w.write_all(req.to_string().as_bytes()).await.unwrap();
        w.write_all(b"\n").await.unwrap();

        let reg_line = lines.next_line().await.unwrap().unwrap();
        let reg: Response = serde_json::from_str(&reg_line).unwrap();
        match reg {
            Response::AppRegistered {
                app_name,
                project_dir,
                url,
            } => {
                assert_eq!(app_name, "my-app");
                assert_eq!(project_dir, "/tmp/test-proj");
                assert!(url.contains("my-app.tako"));
            }
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
        w.write_all(b"{\"type\":\"SubscribeEvents\"}\n")
            .await
            .unwrap();

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

    /// Helper: create a test State with a temp SQLite DB and return (state, _tmpdir).
    fn test_state() -> (Arc<Mutex<State>>, tempfile::TempDir) {
        let tmp = tempfile::TempDir::new().unwrap();
        let db_path = tmp.path().join("dev-server.db");
        let db = state::DevStateStore::open(db_path).unwrap();

        let (shutdown_tx, _shutdown_rx) = watch::channel(false);
        let mut st = State::new(
            shutdown_tx,
            proxy::Routes::default(),
            EventsHub::default(),
            true,
            53535,
            8443,
            "127.0.0.1:8443".to_string(),
            "127.0.0.1".to_string(),
        );
        st.db = Some(db);
        (Arc::new(Mutex::new(st)), tmp)
    }

    #[tokio::test]
    async fn unregister_app_broadcasts_stopped_event_to_subscriber() {
        let (state, _tmp) = test_state();

        // Register an app first.
        {
            let s = state.lock().unwrap();
            if let Some(db) = &s.db {
                db.register_app(
                    "/proj",
                    "my-app",
                    &["my-app.tako".to_string()],
                    3000,
                    &state::AppStatus::Running,
                    &["bun".to_string()],
                    &std::collections::HashMap::new(),
                    "/log",
                    None,
                )
                .unwrap();
            }
            s.routes.set_routes(
                "reg:/proj".to_string(),
                vec!["my-app.tako".to_string()],
                3000,
                true,
            );
        }

        // Subscribe to events.
        let mut ev_rx = {
            let s = state.lock().unwrap();
            s.events.subscribe()
        };

        // Unregister the app on a client connection.
        let (a, b) = tokio::net::UnixStream::pair().unwrap();
        let state_for_handler = state.clone();
        let h = tokio::spawn(async move { handle_client(a, state_for_handler).await });

        let (r, mut w) = b.into_split();
        let req = serde_json::json!({
            "type": "UnregisterApp",

            "project_dir": "/proj",
        });
        w.write_all(req.to_string().as_bytes()).await.unwrap();
        w.write_all(b"\n").await.unwrap();

        let mut lines = BufReader::new(r).lines();
        let line = lines.next_line().await.unwrap().unwrap();
        let resp: Response = serde_json::from_str(&line).unwrap();
        assert!(matches!(resp, Response::AppUnregistered { .. }));

        drop(w);
        drop(lines);
        h.await.unwrap().unwrap();

        // The subscriber should have received an AppStatusChanged event.
        let event = tokio::time::timeout(Duration::from_millis(100), ev_rx.recv())
            .await
            .expect("should not time out")
            .unwrap();

        match event {
            Response::Event {
                event:
                    protocol::DevEvent::AppStatusChanged {
                        project_dir,
                        app_name,
                        status,
                    },
            } => {
                assert_eq!(project_dir, "/proj");
                assert_eq!(app_name, "my-app");
                assert_eq!(status, "stopped");
            }
            other => panic!("expected AppStatusChanged, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn restart_app_broadcasts_restart_requested_event() {
        let (state, _tmp) = test_state();

        // Register an app.
        {
            let s = state.lock().unwrap();
            if let Some(db) = &s.db {
                db.register_app(
                    "/proj",
                    "my-app",
                    &["my-app.tako".to_string()],
                    3000,
                    &state::AppStatus::Running,
                    &["bun".to_string()],
                    &std::collections::HashMap::new(),
                    "/log",
                    None,
                )
                .unwrap();
            }
        }

        // Subscribe to events.
        let mut ev_rx = {
            let s = state.lock().unwrap();
            s.events.subscribe()
        };

        // Send RestartApp.
        let (a, b) = tokio::net::UnixStream::pair().unwrap();
        let state_for_handler = state.clone();
        let h = tokio::spawn(async move { handle_client(a, state_for_handler).await });

        let (r, mut w) = b.into_split();
        let req = serde_json::json!({
            "type": "RestartApp",

            "project_dir": "/proj",
        });
        w.write_all(req.to_string().as_bytes()).await.unwrap();
        w.write_all(b"\n").await.unwrap();

        let mut lines = BufReader::new(r).lines();
        let line = lines.next_line().await.unwrap().unwrap();
        let resp: Response = serde_json::from_str(&line).unwrap();
        assert!(matches!(resp, Response::AppRestarting { .. }));

        drop(w);
        drop(lines);
        h.await.unwrap().unwrap();

        // Subscriber should have received RestartRequested.
        let event = tokio::time::timeout(Duration::from_millis(100), ev_rx.recv())
            .await
            .expect("should not time out")
            .unwrap();

        match event {
            Response::Event {
                event:
                    protocol::DevEvent::RestartRequested {
                        project_dir,
                        app_name,
                    },
            } => {
                assert_eq!(project_dir, "/proj");
                assert_eq!(app_name, "my-app");
            }
            other => panic!("expected RestartRequested, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn set_app_status_broadcasts_status_changed_event() {
        let (state, _tmp) = test_state();

        // Register an app.
        {
            let s = state.lock().unwrap();
            if let Some(db) = &s.db {
                db.register_app(
                    "/proj",
                    "my-app",
                    &["my-app.tako".to_string()],
                    3000,
                    &state::AppStatus::Running,
                    &["bun".to_string()],
                    &std::collections::HashMap::new(),
                    "/log",
                    None,
                )
                .unwrap();
            }
        }

        let mut ev_rx = {
            let s = state.lock().unwrap();
            s.events.subscribe()
        };

        // Send SetAppStatus.
        let (a, b) = tokio::net::UnixStream::pair().unwrap();
        let state_for_handler = state.clone();
        let h = tokio::spawn(async move { handle_client(a, state_for_handler).await });

        let (r, mut w) = b.into_split();
        let req = serde_json::json!({
            "type": "SetAppStatus",

            "project_dir": "/proj",
            "status": "idle",
        });
        w.write_all(req.to_string().as_bytes()).await.unwrap();
        w.write_all(b"\n").await.unwrap();

        let mut lines = BufReader::new(r).lines();
        let line = lines.next_line().await.unwrap().unwrap();
        let resp: Response = serde_json::from_str(&line).unwrap();
        assert!(matches!(resp, Response::AppStatusUpdated { .. }));

        drop(w);
        drop(lines);
        h.await.unwrap().unwrap();

        let event = tokio::time::timeout(Duration::from_millis(100), ev_rx.recv())
            .await
            .expect("should not time out")
            .unwrap();

        match event {
            Response::Event {
                event:
                    protocol::DevEvent::AppStatusChanged {
                        project_dir,
                        app_name,
                        status,
                    },
            } => {
                assert_eq!(project_dir, "/proj");
                assert_eq!(app_name, "my-app");
                assert_eq!(status, "idle");
            }
            other => panic!("expected AppStatusChanged, got: {other:?}"),
        }
    }

    #[test]
    fn redirect_location_strips_default_http_port() {
        let location = redirect_location("bun-example.tako:80", "/hello");
        assert_eq!(location, "https://bun-example.tako/hello");
    }

    #[test]
    fn redirect_location_keeps_non_default_port() {
        let location = redirect_location("bun-example.tako:8080", "/");
        assert_eq!(location, "https://bun-example.tako:8080/");
    }

    #[test]
    fn ensure_tcp_listener_can_bind_succeeds_when_port_is_available() {
        // On busy CI hosts, another process can race us for a just-freed port.
        // Retry a few times with fresh ephemeral ports to keep this deterministic.
        for _ in 0..8 {
            let Ok(listener) = std::net::TcpListener::bind(("127.0.0.1", 0)) else {
                return;
            };
            let addr = listener.local_addr().unwrap();
            drop(listener);
            if ensure_tcp_listener_can_bind(&addr.to_string()).is_ok() {
                return;
            }
        }
        panic!("failed to find an available loopback port after retries");
    }

    /// End-to-end test: client B subscribes to events via a real socket
    /// handler, client A unregisters an app via a separate socket handler,
    /// and client B must receive the AppStatusChanged{stopped} event over
    /// the wire. This exercises the exact codepath that the attached dev
    /// client uses to detect when the owner stops the app.
    #[tokio::test]
    async fn subscriber_receives_stopped_event_over_socket_when_app_unregistered() {
        let (state, _tmp) = test_state();

        // Register an app.
        {
            let s = state.lock().unwrap();
            if let Some(db) = &s.db {
                db.register_app(
                    "/proj",
                    "my-app",
                    &["my-app.tako".to_string()],
                    3000,
                    &state::AppStatus::Running,
                    &["bun".to_string()],
                    &std::collections::HashMap::new(),
                    "/log",
                    None,
                )
                .unwrap();
            }
            s.routes.set_routes(
                "reg:/proj".to_string(),
                vec!["my-app.tako".to_string()],
                3000,
                true,
            );
        }

        // Client B: subscribe to events via a real socket handler.
        let (sub_a, sub_b) = tokio::net::UnixStream::pair().unwrap();
        let sub_handler = tokio::spawn({
            let state = state.clone();
            async move { handle_client(sub_a, state).await }
        });
        let (sub_r, mut sub_w) = sub_b.into_split();
        let mut sub_lines = BufReader::new(sub_r).lines();

        sub_w
            .write_all(b"{\"type\":\"SubscribeEvents\"}\n")
            .await
            .unwrap();
        let resp_line = sub_lines.next_line().await.unwrap().unwrap();
        let resp: Response = serde_json::from_str(&resp_line).unwrap();
        assert!(matches!(resp, Response::Subscribed));

        // Client A: unregister the app via a separate socket handler.
        let (unreg_a, unreg_b) = tokio::net::UnixStream::pair().unwrap();
        let unreg_handler = tokio::spawn({
            let state = state.clone();
            async move { handle_client(unreg_a, state).await }
        });
        let (unreg_r, mut unreg_w) = unreg_b.into_split();

        let req = serde_json::json!({
            "type": "UnregisterApp",
            "project_dir": "/proj",
        });
        unreg_w
            .write_all(format!("{}\n", req).as_bytes())
            .await
            .unwrap();

        let mut unreg_lines = BufReader::new(unreg_r).lines();
        let unreg_resp_line = unreg_lines.next_line().await.unwrap().unwrap();
        let unreg_resp: Response = serde_json::from_str(&unreg_resp_line).unwrap();
        assert!(matches!(unreg_resp, Response::AppUnregistered { .. }));

        // Clean up unregister handler.
        drop(unreg_w);
        drop(unreg_lines);
        unreg_handler.await.unwrap().unwrap();

        // Client B should receive the AppStatusChanged event.
        let event_line = tokio::time::timeout(Duration::from_millis(500), sub_lines.next_line())
            .await
            .expect("subscriber should receive event within 500ms")
            .unwrap()
            .unwrap();
        let event_resp: Response = serde_json::from_str(&event_line).unwrap();
        match event_resp {
            Response::Event {
                event:
                    protocol::DevEvent::AppStatusChanged {
                        project_dir,
                        app_name,
                        status,
                    },
            } => {
                assert_eq!(project_dir, "/proj");
                assert_eq!(app_name, "my-app");
                assert_eq!(status, "stopped");
            }
            other => panic!("expected AppStatusChanged stopped, got: {other:?}"),
        }

        // Clean up subscriber.
        drop(sub_w);
        drop(sub_lines);
        let _ = tokio::time::timeout(Duration::from_secs(1), sub_handler).await;
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

    /// Verify that the dynamic cert resolver generates a cert whose SAN
    /// exactly matches the requested hostname — this is how we sidestep
    /// OpenSSL rejecting `*.tako` wildcards (single-label TLD).
    #[test]
    fn dev_cert_resolver_generates_cert_matching_hostname() {
        let ca = local_ca::LocalCA::generate().unwrap();
        let resolver = DevCertResolver::new(ca);

        let (x509, _pkey) = resolver
            .get_or_create_cert("foo.tako")
            .expect("should generate cert");

        // Verify the SAN contains the exact hostname.
        let pem = x509.to_pem().unwrap();
        let (_, parsed_pem) = x509_parser::pem::parse_x509_pem(&pem).unwrap();
        let parsed = parsed_pem.parse_x509().unwrap();

        let san_ext = parsed
            .extensions()
            .iter()
            .find(|ext| ext.oid == x509_parser::oid_registry::OID_X509_EXT_SUBJECT_ALT_NAME)
            .expect("cert must have SAN extension");

        let san = match san_ext.parsed_extension() {
            x509_parser::extensions::ParsedExtension::SubjectAlternativeName(san) => san,
            other => panic!("expected SubjectAlternativeName, got {:?}", other),
        };

        let dns_names: Vec<&str> = san
            .general_names
            .iter()
            .filter_map(|n| match n {
                x509_parser::extensions::GeneralName::DNSName(d) => Some(*d),
                _ => None,
            })
            .collect();

        assert!(
            dns_names.contains(&"foo.tako"),
            "cert must contain foo.tako SAN, got: {:?}",
            dns_names
        );
    }

    /// Verify that the dynamically generated cert chains back to the CA
    /// and that the SAN exactly matches — these are the two checks that
    /// Chrome/BoringSSL performs during the TLS handshake.
    #[test]
    fn dev_cert_resolver_cert_is_signed_by_ca() {
        let ca = local_ca::LocalCA::generate().unwrap();
        let ca_x509 = X509::from_pem(ca.ca_cert_pem().as_bytes()).unwrap();
        let resolver = DevCertResolver::new(ca);

        let (leaf_x509, _) = resolver
            .get_or_create_cert("foo.tako")
            .expect("should generate cert");

        // Verify the leaf cert is signed by the CA's public key.
        let ca_pubkey = ca_x509.public_key().unwrap();
        assert!(
            leaf_x509.verify(&ca_pubkey).unwrap(),
            "leaf cert must be signed by the local CA"
        );
    }

    #[test]
    fn dev_cert_resolver_caches_certs() {
        let ca = local_ca::LocalCA::generate().unwrap();
        let resolver = DevCertResolver::new(ca);

        let (first, _) = resolver.get_or_create_cert("bar.tako").unwrap();
        let (second, _) = resolver.get_or_create_cert("bar.tako").unwrap();

        // Same DER bytes → same cert object was returned from cache.
        assert_eq!(first.to_der().unwrap(), second.to_der().unwrap());
    }
}

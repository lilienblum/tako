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
use tokio::io::{AsyncBufReadExt as _, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream, UnixListener, UnixStream};
use tokio::sync::mpsc;
use tokio::sync::watch;

use tako_socket::{read_json_line, write_json_line};

use protocol::DevEvent;
use protocol::{AppInfo, Request, Response};
use tracing_subscriber::EnvFilter;

const IDLE_EXIT_DELAY: Duration = Duration::from_secs(2);
const TAKO_DEV_DOMAIN: &str = "tako.test";
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
    if let Ok(old_pid) = contents.trim().parse::<i32>()
        && old_pid > 0
    {
        unsafe {
            libc::kill(old_pid, libc::SIGTERM);
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
    paths::tako_data_dir()
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

/// Split a route pattern like "app.tako.test/api" into ("app.tako.test", Some("/api")).
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
    apps: std::collections::HashMap<String, state::RuntimeApp>,
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
            apps: std::collections::HashMap::new(),
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
            Request::SubscribeLogs { config_path, after } => {
                let log_buffer = {
                    let s = state.lock().unwrap();
                    s.apps.get(&config_path).map(|a| a.log_buffer.clone())
                };

                let Some(log_buffer) = log_buffer else {
                    write_resp(
                        &mut w,
                        &Response::Error {
                            message: format!("app not found: {config_path}"),
                        },
                    )
                    .await?;
                    continue;
                };

                let _control_client = ControlClientSubscription::register(&state);

                let (backlog, mut rx, truncated) = log_buffer.subscribe(after);

                if write_resp(&mut w, &Response::LogsSubscribed).await.is_err() {
                    return Ok(());
                }
                if truncated && write_resp(&mut w, &Response::LogsTruncated).await.is_err() {
                    return Ok(());
                }

                for entry in backlog {
                    if write_resp(
                        &mut w,
                        &Response::LogEntry {
                            id: entry.id,
                            line: entry.line,
                        },
                    )
                    .await
                    .is_err()
                    {
                        return Ok(());
                    }
                }

                let mut disconnect_probe = [0_u8; 1];
                loop {
                    tokio::select! {
                        maybe_entry = rx.recv() => {
                            let Some(entry) = maybe_entry else {
                                break;
                            };
                            if write_resp(
                                &mut w,
                                &Response::LogEntry {
                                    id: entry.id,
                                    line: entry.line,
                                },
                            )
                            .await
                            .is_err()
                            {
                                break;
                            }
                        }
                        read_result = r.read(&mut disconnect_probe) => {
                            match read_result {
                                Ok(0) | Err(_) => break,
                                Ok(_) => {}
                            }
                        }
                    }
                }
                return Ok(());
            }
            Request::RegisterApp {
                config_path,
                project_dir,
                app_name,
                variant,
                hosts,
                upstream_port,
                command,
                env,
                client_pid,
            } => {
                let app_name = sanitize_app_name(&app_name);

                // If an app is already registered with this config_path, kill it first.
                {
                    let s = state.lock().unwrap();
                    if let Some(existing) = s.apps.get(&config_path)
                        && let Some(pid) = existing.pid
                    {
                        kill_app_process(pid);
                    }
                }

                let mut s = state.lock().unwrap();
                s.cancel_idle_exit();

                let hosts = if hosts.is_empty() {
                    default_hosts(&app_name)
                } else {
                    hosts
                };

                if let Some(db) = &s.db {
                    let _ = db.register(&config_path, &project_dir, &app_name, variant.as_deref());
                }

                // Reuse existing buffer if re-registering, otherwise create new.
                let log_buffer = s
                    .apps
                    .get(&config_path)
                    .map(|a| {
                        a.log_buffer.clear();
                        a.log_buffer.clone()
                    })
                    .unwrap_or_else(state::LogBuffer::new);

                s.apps.insert(
                    config_path.clone(),
                    state::RuntimeApp {
                        project_dir: project_dir.clone(),
                        name: app_name.clone(),
                        variant: variant.clone(),
                        hosts: hosts.clone(),
                        upstream_port,
                        is_idle: false,
                        command,
                        env,
                        log_buffer,
                        pid: None,
                        client_pid,
                    },
                );

                let route_id = format!("reg:{}", config_path);
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
                drop(s);

                // Spawn the app process (daemon-owned).
                let spawn_state = state.clone();
                let spawn_config = config_path.clone();
                tokio::spawn(async move {
                    match spawn_and_monitor_app(spawn_state.clone(), &spawn_config).await {
                        Ok(pid) => {
                            tracing::info!(config_path = %spawn_config, pid = pid, "spawned app process");
                            broadcast_app_status(&spawn_state, &spawn_config, "running");
                        }
                        Err(e) => {
                            tracing::warn!(config_path = %spawn_config, error = %e, "failed to spawn app");
                            let log_buffer = {
                                let s = spawn_state.lock().unwrap();
                                s.apps.get(&spawn_config).map(|a| a.log_buffer.clone())
                            };
                            broadcast_app_status(&spawn_state, &spawn_config, "idle");
                            if let Some(buf) = log_buffer {
                                let msg = format!("failed to start app: {e}");
                                push_scoped_log(&buf, "Error", "tako", &msg);
                                push_app_event(
                                    &buf,
                                    "error",
                                    Some(("message", serde_json::json!(msg))),
                                );
                            }
                        }
                    }
                });

                Response::AppRegistered {
                    app_name,
                    config_path,
                    project_dir,
                    url,
                }
            }
            Request::UnregisterApp { config_path } => {
                let mut s = state.lock().unwrap();

                // Kill the child process before removing.
                if let Some(app) = s.apps.get(&config_path)
                    && let Some(pid) = app.pid
                {
                    kill_app_process(pid);
                    state::remove_pid_file(&app.project_dir, &config_path);
                }

                let app_name = s
                    .apps
                    .remove(&config_path)
                    .map(|a| a.name)
                    .unwrap_or_default();

                let route_id = format!("reg:{}", config_path);
                s.routes.remove_app(&route_id);

                if !app_name.is_empty() {
                    s.events.broadcast(Response::Event {
                        event: protocol::DevEvent::AppStatusChanged {
                            config_path: config_path.clone(),
                            app_name: app_name.clone(),
                            status: "stopped".to_string(),
                        },
                    });
                }

                if s.apps.is_empty() {
                    s.schedule_idle_exit();
                }

                Response::AppUnregistered { config_path }
            }
            Request::RestartApp { config_path } => {
                // Kill the current child if any.
                {
                    let mut s = state.lock().unwrap();
                    if let Some(app) = s.apps.get_mut(&config_path) {
                        if let Some(pid) = app.pid.take() {
                            kill_app_process(pid);
                            state::remove_pid_file(&app.project_dir, &config_path);
                        }
                        app.is_idle = true;
                    }
                }

                // Write divider to log buffer for attached clients.
                let log_buffer = {
                    let s = state.lock().unwrap();
                    s.apps.get(&config_path).map(|a| a.log_buffer.clone())
                };
                if let Some(ref buf) = log_buffer {
                    push_divider(buf, "restarted");
                }

                // Spawn a new process.
                let spawn_state = state.clone();
                let spawn_config = config_path.clone();
                tokio::spawn(async move {
                    match spawn_and_monitor_app(spawn_state.clone(), &spawn_config).await {
                        Ok(pid) => {
                            tracing::info!(config_path = %spawn_config, pid = pid, "restarted app process");
                            broadcast_app_status(&spawn_state, &spawn_config, "running");
                        }
                        Err(e) => {
                            tracing::warn!(config_path = %spawn_config, error = %e, "failed to restart app");
                            let log_buffer = {
                                let s = spawn_state.lock().unwrap();
                                s.apps.get(&spawn_config).map(|a| a.log_buffer.clone())
                            };
                            if let Some(buf) = log_buffer {
                                let msg = format!("restart failed: {e}");
                                push_scoped_log(&buf, "Error", "tako", &msg);
                                push_app_event(
                                    &buf,
                                    "error",
                                    Some(("message", serde_json::json!(msg))),
                                );
                            }
                        }
                    }
                });

                Response::AppRestarting { config_path }
            }
            Request::SetAppStatus {
                config_path,
                status,
            } => {
                let is_idle = match status.as_str() {
                    "idle" => true,
                    "running" => false,
                    _ => {
                        write_resp(
                            &mut w,
                            &Response::Error {
                                message: format!("unknown status: {status}"),
                            },
                        )
                        .await?;
                        continue;
                    }
                };

                let mut s = state.lock().unwrap();
                let route_id = format!("reg:{}", config_path);
                s.routes.set_active(&route_id, !is_idle);

                let app_name = if let Some(app) = s.apps.get_mut(&config_path) {
                    app.is_idle = is_idle;
                    app.name.clone()
                } else {
                    String::new()
                };

                if !app_name.is_empty() {
                    s.events.broadcast(Response::Event {
                        event: protocol::DevEvent::AppStatusChanged {
                            config_path: config_path.clone(),
                            app_name,
                            status: status.clone(),
                        },
                    });
                }

                Response::AppStatusUpdated {
                    config_path,
                    status,
                }
            }
            Request::HandoffApp { config_path, pid } => {
                let mut s = state.lock().unwrap();
                let project_dir = if let Some(app) = s.apps.get_mut(&config_path) {
                    app.pid = Some(pid);
                    app.is_idle = false;
                    app.project_dir.clone()
                } else {
                    String::new()
                };
                if !project_dir.is_empty() {
                    state::write_pid_file(&project_dir, &config_path, pid);
                }

                // Spawn a PID monitor: if the process dies, mark as idle.
                let state_for_monitor = state.clone();
                let config_for_monitor = config_path.clone();
                let dir_for_monitor = project_dir.clone();
                tokio::spawn(async move {
                    monitor_handoff_pid(
                        state_for_monitor,
                        config_for_monitor,
                        dir_for_monitor,
                        pid,
                    )
                    .await;
                });

                Response::AppHandedOff { config_path }
            }
            Request::OpenSession {
                config_path,
                session_id,
            } => {
                let s = state.lock().unwrap();
                let app_name = s
                    .apps
                    .get(&config_path)
                    .map(|a| a.name.clone())
                    .unwrap_or_default();
                s.events.broadcast(Response::Event {
                    event: protocol::DevEvent::SessionAttached {
                        config_path: config_path.clone(),
                        app_name,
                        session_id,
                    },
                });
                drop(s);
                Response::Pong
            }
            Request::ListRegisteredApps => {
                let s = state.lock().unwrap();
                let apps = s
                    .apps
                    .iter()
                    .map(|(config_path, a)| protocol::RegisteredAppInfo {
                        config_path: config_path.clone(),
                        project_dir: a.project_dir.clone(),
                        app_name: a.name.clone(),
                        variant: a.variant.clone(),
                        hosts: a.hosts.clone(),
                        upstream_port: a.upstream_port,
                        status: if a.is_idle { "idle" } else { "running" }.to_string(),
                        pid: a.pid,
                        client_pid: a.client_pid,
                    })
                    .collect();
                Response::RegisteredApps { apps }
            }
            Request::ListApps => {
                let s = state.lock().unwrap();
                let apps = s
                    .apps
                    .values()
                    .map(|a| AppInfo {
                        app_name: a.name.clone(),
                        variant: a.variant.clone(),
                        hosts: a.hosts.clone(),
                        upstream_port: a.upstream_port,
                        pid: a.pid,
                    })
                    .collect();
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
async fn monitor_handoff_pid(
    state: Arc<Mutex<State>>,
    config_path: String,
    project_dir: String,
    pid: u32,
) {
    let sysinfo_pid = sysinfo::Pid::from_u32(pid);
    let mut sys = sysinfo::System::new();
    loop {
        tokio::time::sleep(Duration::from_secs(2)).await;

        sys.refresh_processes(sysinfo::ProcessesToUpdate::Some(&[sysinfo_pid]), false);
        if sys.process(sysinfo_pid).is_none() {
            let mut s = state.lock().unwrap();
            // Only act if this PID is still the current one (a restart may
            // have spawned a new process since the handoff).
            let still_current = s
                .apps
                .get(&config_path)
                .and_then(|a| a.pid)
                .map(|p| p == pid)
                .unwrap_or(false);
            if still_current {
                if let Some(app) = s.apps.get_mut(&config_path) {
                    app.is_idle = true;
                    app.pid = None;
                }
                let route_id = format!("reg:{}", config_path);
                s.routes.set_active(&route_id, false);
                state::remove_pid_file(&project_dir, &config_path);
            }
            tracing::info!(config_path = %config_path, project_dir = %project_dir, pid = pid, still_current = still_current, "handoff'd process exited");
            break;
        }
    }
}

/// Broadcast an AppStatusChanged event for a config_path, reading the app_name from state.
fn broadcast_app_status(state: &Arc<Mutex<State>>, config_path: &str, status: &str) {
    let s = state.lock().unwrap();
    let app_name = s
        .apps
        .get(config_path)
        .map(|a| a.name.clone())
        .unwrap_or_default();
    s.events.broadcast(Response::Event {
        event: protocol::DevEvent::AppStatusChanged {
            config_path: config_path.to_string(),
            app_name,
            status: status.to_string(),
        },
    });
}

/// Kill an app process by PID using process group SIGKILL.
fn kill_app_process(pid: u32) {
    if pid == 0 {
        return;
    }
    unsafe {
        libc::kill(-(pid as i32), libc::SIGKILL);
        libc::kill(pid as i32, libc::SIGKILL);
    }
}

/// Push an app event marker to the log buffer (for subscribed clients).
fn push_app_event(
    buf: &state::LogBuffer,
    event_name: &str,
    extra: Option<(&str, serde_json::Value)>,
) {
    let mut payload = serde_json::json!({
        "type": "app_event",
        "event": event_name,
    });
    if let Some((key, value)) = extra {
        payload[key] = value;
    }
    buf.push(payload.to_string());
}

/// Push a divider marker to the log buffer (shows "restarted" separator in subscribed clients).
fn push_divider(buf: &state::LogBuffer, label: &str) {
    let payload = serde_json::json!({
        "timestamp": "",
        "level": "Info",
        "scope": "__divider__",
        "message": label,
    });
    buf.push(payload.to_string());
}

/// Push a scoped log line to the log buffer.
fn push_scoped_log(buf: &state::LogBuffer, level: &str, scope: &str, message: &str) {
    let now = time::OffsetDateTime::now_local().unwrap_or_else(|_| time::OffsetDateTime::now_utc());
    let timestamp = format!("{:02}:{:02}:{:02}", now.hour(), now.minute(), now.second());
    let payload = serde_json::json!({
        "timestamp": timestamp,
        "level": level,
        "scope": scope,
        "message": message,
    });
    buf.push(payload.to_string());
}

/// Spawn an app process and set up a monitor task. Returns the child PID.
/// On process exit, the monitor marks the app idle and broadcasts status.
async fn spawn_and_monitor_app(
    state: Arc<Mutex<State>>,
    config_path: &str,
) -> Result<u32, Box<dyn std::error::Error + Send + Sync>> {
    let (project_dir, app_clone, log_buffer) = {
        let s = state.lock().unwrap();
        let app = s.apps.get(config_path).ok_or("app not found")?;
        (app.project_dir.clone(), app.clone(), app.log_buffer.clone())
    };

    push_app_event(&log_buffer, "launching", None);

    let mut child = spawn_app(&project_dir, &app_clone).await?;
    let pid = child.id().ok_or("failed to get child PID")?;

    // Update app state with the new PID.
    {
        let mut s = state.lock().unwrap();
        if let Some(app) = s.apps.get_mut(config_path) {
            app.pid = Some(pid);
            app.is_idle = false;
        }
        let route_id = format!("reg:{}", config_path);
        s.routes.set_active(&route_id, true);
    }

    state::write_pid_file(&project_dir, config_path, pid);
    push_app_event(&log_buffer, "pid", Some(("pid", serde_json::json!(pid))));

    // Spawn monitor task: wait for child exit, then mark idle.
    let state_for_monitor = state.clone();
    let config_for_monitor = config_path.to_string();
    let dir_for_monitor = project_dir.clone();
    let buf_for_monitor = log_buffer.clone();
    tokio::spawn(async move {
        let exit_status = child.wait().await;
        let code_str = exit_status
            .as_ref()
            .ok()
            .and_then(|s| s.code())
            .map(|c| format!("exit code {c}"))
            .unwrap_or_else(|| {
                #[cfg(unix)]
                {
                    use std::os::unix::process::ExitStatusExt;
                    if let Some(sig) = exit_status.as_ref().ok().and_then(|s| s.signal()) {
                        return format!("killed by signal {sig}");
                    }
                }
                "signal".to_string()
            });

        let still_current = {
            let mut s = state_for_monitor.lock().unwrap();
            let current = s
                .apps
                .get(&config_for_monitor)
                .and_then(|a| a.pid)
                .map(|p| p == pid)
                .unwrap_or(false);

            if current {
                if let Some(app) = s.apps.get_mut(&config_for_monitor) {
                    app.is_idle = true;
                    app.pid = None;
                }
                let route_id = format!("reg:{}", config_for_monitor);
                s.routes.set_active(&route_id, false);
                state::remove_pid_file(&dir_for_monitor, &config_for_monitor);
                let app_name = s
                    .apps
                    .get(&config_for_monitor)
                    .map(|a| a.name.clone())
                    .unwrap_or_default();
                s.events.broadcast(Response::Event {
                    event: protocol::DevEvent::AppStatusChanged {
                        config_path: config_for_monitor.clone(),
                        app_name,
                        status: "idle".to_string(),
                    },
                });
            }
            current
        };

        if still_current {
            let msg = format!("app exited ({code_str})");
            push_scoped_log(&buf_for_monitor, "Fatal", "tako", &msg);
            push_app_event(
                &buf_for_monitor,
                "exited",
                Some(("message", serde_json::json!(msg))),
            );

            tracing::info!(config_path = %config_for_monitor, pid = pid, "app process exited, marking idle");
        }
    });

    push_app_event(&log_buffer, "started", None);

    Ok(pid)
}

/// Spawn an app process. Returns the child on success.
///
/// This is the universal spawner used by RegisterApp, RestartApp, and
/// wake-on-request. It captures stdout/stderr to the app's JSONL log file.
async fn spawn_app(
    project_dir: &str,
    app: &state::RuntimeApp,
) -> Result<tokio::process::Child, Box<dyn std::error::Error + Send + Sync>> {
    if app.command.is_empty() {
        return Err("app has empty command".into());
    }

    let mut cmd = tokio::process::Command::new(&app.command[0]);
    if app.command.len() > 1 {
        cmd.args(&app.command[1..]);
    }
    cmd.current_dir(project_dir)
        .env("PORT", app.upstream_port.to_string())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    #[cfg(unix)]
    cmd.process_group(0);

    // On Linux, ask the kernel to send SIGTERM to the child when the daemon
    // dies (including SIGKILL). Belt-and-suspenders with process-group kill.
    #[cfg(target_os = "linux")]
    unsafe {
        cmd.pre_exec(|| {
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
            Ok(())
        });
    }

    // Prepend node_modules/.bin to PATH so preset commands like `next`, `vite`
    // are found — same as what npm/yarn/bun do when running package scripts.
    let bin_dir = std::path::Path::new(project_dir).join("node_modules/.bin");
    if bin_dir.is_dir() {
        let current_path = std::env::var("PATH").unwrap_or_default();
        cmd.env("PATH", format!("{}:{current_path}", bin_dir.display()));
    }

    for (k, v) in &app.env {
        cmd.env(k, v);
    }

    let mut child = cmd.spawn()?;

    // Drain stdout/stderr to the app's in-memory log buffer.
    let log_buffer = app.log_buffer.clone();
    if let Some(stdout) = child.stdout.take() {
        let buf = log_buffer.clone();
        tokio::spawn(async move {
            drain_pipe_to_buffer(stdout, buf).await;
        });
    }
    if let Some(stderr) = child.stderr.take() {
        let buf = log_buffer.clone();
        tokio::spawn(async move {
            drain_pipe_to_buffer(stderr, buf).await;
        });
    }

    Ok(child)
}

async fn drain_pipe_to_buffer(pipe: impl tokio::io::AsyncRead + Unpin, buf: state::LogBuffer) {
    let reader = tokio::io::BufReader::new(pipe);
    let mut lines = reader.lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let now =
            time::OffsetDateTime::now_local().unwrap_or_else(|_| time::OffsetDateTime::now_utc());
        let ts = format!("{:02}:{:02}:{:02}", now.hour(), now.minute(), now.second());
        let json = serde_json::json!({
            "timestamp": ts,
            "level": "Info",
            "scope": "app",
            "message": line,
        });
        buf.push(json.to_string());
    }
}

/// Handle wake-on-request: when a RequestStarted event arrives for an idle app,
/// spawn the process and mark it as running.
async fn handle_wake_on_request(state: Arc<Mutex<State>>, host: String, path: String) {
    // Check under lock: is the route already active? Find an idle app matching this host.
    let app_info: Option<(String, state::RuntimeApp)> = {
        let s = state.lock().unwrap();
        if s.routes.lookup(&host, &path).is_some_and(|(_, _, a, _)| a) {
            return;
        }
        s.apps
            .iter()
            .find(|(_, a)| {
                if !a.is_idle {
                    return false;
                }
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
                        Some(_) => true,
                    }
                })
            })
            .map(|(config_path, a)| (config_path.clone(), a.clone()))
    };

    let Some((config_path, app)) = app_info else {
        return;
    };

    tracing::info!(
        app_name = %app.name,
        host = %host,
        "waking idle app on request"
    );

    match spawn_app(&app.project_dir, &app).await {
        Ok(mut child) => {
            let pid = child.id();

            {
                let mut s = state.lock().unwrap();
                if let Some(rt) = s.apps.get_mut(&config_path) {
                    rt.is_idle = false;
                    rt.pid = pid;
                }
                let route_id = format!("reg:{}", config_path);
                s.routes.set_active(&route_id, true);
            }

            // Monitor the spawned process.
            if let Some(pid) = pid {
                state::write_pid_file(&app.project_dir, &config_path, pid);
                let state = state.clone();
                let config_path = config_path.clone();
                let project_dir = app.project_dir.clone();
                let log_buffer = app.log_buffer.clone();
                tokio::spawn(async move {
                    let exit_status = child.wait().await;
                    let code_str = exit_status
                        .as_ref()
                        .ok()
                        .and_then(|s| s.code())
                        .map(|c| format!("exit code {c}"))
                        .unwrap_or_else(|| "signal".to_string());

                    let mut s = state.lock().unwrap();
                    let still_current = s
                        .apps
                        .get(&config_path)
                        .and_then(|a| a.pid)
                        .map(|p| p == pid)
                        .unwrap_or(false);
                    if still_current {
                        if let Some(rt) = s.apps.get_mut(&config_path) {
                            rt.is_idle = true;
                            rt.pid = None;
                        }
                        let route_id = format!("reg:{}", config_path);
                        s.routes.set_active(&route_id, false);
                        state::remove_pid_file(&project_dir, &config_path);
                    }
                    drop(s);

                    if still_current {
                        let msg = format!("app exited ({code_str})");
                        push_scoped_log(&log_buffer, "Fatal", "tako", &msg);
                        push_app_event(
                            &log_buffer,
                            "exited",
                            Some(("message", serde_json::json!(msg))),
                        );
                    }
                    tracing::info!(config_path = %config_path, project_dir = %project_dir, pid = pid, still_current = still_current, "wake-spawned process exited");
                });
            }
        }
        Err(e) => {
            tracing::warn!(
                app_name = %app.name,
                error = %e,
                "failed to spawn app for wake-on-request"
            );
        }
    }
}

/// Kill all app processes tracked in memory (both wake-spawned and handed-off).
fn kill_all_app_processes(state: &Arc<Mutex<State>>) {
    let s = state.lock().unwrap();
    for (config_path, app) in &s.apps {
        if let Some(pid) = app.pid
            && pid > 0
        {
            tracing::info!(app = %app.name, pid = pid, "killing app process group on shutdown");
            unsafe { libc::kill(-(pid as i32), libc::SIGKILL) };
            unsafe { libc::kill(pid as i32, libc::SIGKILL) };
            state::remove_pid_file(&app.project_dir, config_path);
        }
    }
}

/// Periodic cleanup: remove apps whose config file no longer exists.
async fn stale_app_cleanup_loop(state: Arc<Mutex<State>>) {
    let mut ticker = tokio::time::interval(Duration::from_secs(60));
    loop {
        ticker.tick().await;
        let mut s = state.lock().unwrap();
        if let Some(db) = &s.db
            && let Ok(removed) = db.cleanup_stale()
        {
            for config_path in &removed {
                s.apps.remove(config_path);
                let route_id = format!("reg:{}", config_path);
                s.routes.remove_app(&route_id);
            }
            if !removed.is_empty() {
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
    let pid_path = paths::tako_data_dir()
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

        if let Some(app) = svc.app_logic_mut() {
            let mut opts = pingora_core::apps::HttpServerOptions::default();
            opts.keepalive_request_limit = Some(4096);
            app.server_options = Some(opts);
        }

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

    // Open the SQLite state store (persistent registrations only; runtime state is in-memory).
    let db_path = paths::tako_data_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join("dev-server.db");
    match state::DevStateStore::open(db_path) {
        Ok(db) => {
            // Kill any orphaned app processes from a previous (crashed) server run.
            if let Ok(apps) = db.list() {
                for app in &apps {
                    state::kill_orphaned_process(&app.project_dir, &app.config_path);
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
                    kill_all_app_processes(&state);
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
            "config_path": "/tmp/test-proj/tako.toml",
            "project_dir": "/tmp/test-proj",
            "app_name": "my-app",
            "hosts": ["my-app.tako.test"],
            "upstream_port": 1234,
            "command": ["node", "index.js"],
            "env": {}
        });
        w.write_all(req.to_string().as_bytes()).await.unwrap();
        w.write_all(b"\n").await.unwrap();

        let reg_line = lines.next_line().await.unwrap().unwrap();
        let reg: Response = serde_json::from_str(&reg_line).unwrap();
        match reg {
            Response::AppRegistered {
                app_name,
                config_path,
                project_dir,
                url,
            } => {
                assert_eq!(app_name, "my-app");
                assert_eq!(config_path, "/tmp/test-proj/tako.toml");
                assert_eq!(project_dir, "/tmp/test-proj");
                assert!(url.contains("my-app.tako.test"));
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

    fn insert_test_app(state: &Arc<Mutex<State>>, project_dir: &str, name: &str) {
        let config_path = format!("{project_dir}/tako.toml");
        let mut s = state.lock().unwrap();
        s.apps.insert(
            config_path.clone(),
            state::RuntimeApp {
                project_dir: project_dir.to_string(),
                name: name.to_string(),
                variant: None,
                hosts: vec![format!("{name}.tako.test")],
                upstream_port: 3000,
                is_idle: false,
                command: vec!["bun".to_string()],
                env: std::collections::HashMap::new(),
                log_buffer: state::LogBuffer::new(),
                pid: None,
                client_pid: None,
            },
        );
        s.routes.set_routes(
            format!("reg:{config_path}"),
            vec![format!("{name}.tako.test")],
            3000,
            true,
        );
    }

    #[tokio::test]
    async fn unregister_app_broadcasts_stopped_event_to_subscriber() {
        let (state, _tmp) = test_state();
        insert_test_app(&state, "/proj", "my-app");

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
            "config_path": "/proj/tako.toml",
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
                        config_path,
                        app_name,
                        status,
                    },
            } => {
                assert_eq!(config_path, "/proj/tako.toml");
                assert_eq!(app_name, "my-app");
                assert_eq!(status, "stopped");
            }
            other => panic!("expected AppStatusChanged, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn restart_app_responds_with_app_restarting() {
        let (state, _tmp) = test_state();
        insert_test_app(&state, "/proj", "my-app");

        // Send RestartApp.
        let (a, b) = tokio::net::UnixStream::pair().unwrap();
        let state_for_handler = state.clone();
        let h = tokio::spawn(async move { handle_client(a, state_for_handler).await });

        let (r, mut w) = b.into_split();
        let req = serde_json::json!({
            "type": "RestartApp",
            "config_path": "/proj/tako.toml",
        });
        w.write_all(req.to_string().as_bytes()).await.unwrap();
        w.write_all(b"\n").await.unwrap();

        let mut lines = BufReader::new(r).lines();
        let line = lines.next_line().await.unwrap().unwrap();
        let resp: Response = serde_json::from_str(&line).unwrap();
        match resp {
            Response::AppRestarting { config_path } => {
                assert_eq!(config_path, "/proj/tako.toml");
            }
            other => panic!("expected AppRestarting, got: {other:?}"),
        }

        drop(w);
        drop(lines);
        h.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn subscribe_logs_streams_backlog_and_live_entries() {
        let (state, _tmp) = test_state();
        insert_test_app(&state, "/proj", "my-app");

        // Push some entries to the log buffer before subscribing.
        {
            let s = state.lock().unwrap();
            let app = s.apps.get("/proj/tako.toml").unwrap();
            app.log_buffer.push(
                r#"{"timestamp":"00:00:01","level":"Info","scope":"app","message":"line-1"}"#
                    .to_string(),
            );
            app.log_buffer.push(
                r#"{"timestamp":"00:00:02","level":"Info","scope":"app","message":"line-2"}"#
                    .to_string(),
            );
        }

        let (a, b) = tokio::net::UnixStream::pair().unwrap();
        let state_for_handler = state.clone();
        let h = tokio::spawn(async move { handle_client(a, state_for_handler).await });

        let (r, mut w) = b.into_split();
        let mut lines = BufReader::new(r).lines();

        let req = serde_json::json!({
            "type": "SubscribeLogs",
            "config_path": "/proj/tako.toml",
        });
        w.write_all(req.to_string().as_bytes()).await.unwrap();
        w.write_all(b"\n").await.unwrap();

        // First response: LogsSubscribed
        let line = lines.next_line().await.unwrap().unwrap();
        let resp: Response = serde_json::from_str(&line).unwrap();
        assert!(matches!(resp, Response::LogsSubscribed));

        // Next: two backlog entries
        let line = lines.next_line().await.unwrap().unwrap();
        let resp: Response = serde_json::from_str(&line).unwrap();
        match resp {
            Response::LogEntry { id, line } => {
                assert_eq!(id, 0);
                assert!(line.contains("line-1"));
            }
            other => panic!("expected LogEntry, got: {other:?}"),
        }

        let line = lines.next_line().await.unwrap().unwrap();
        let resp: Response = serde_json::from_str(&line).unwrap();
        match resp {
            Response::LogEntry { id, line } => {
                assert_eq!(id, 1);
                assert!(line.contains("line-2"));
            }
            other => panic!("expected LogEntry, got: {other:?}"),
        }

        // Push a live entry while subscribed.
        {
            let s = state.lock().unwrap();
            let app = s.apps.get("/proj/tako.toml").unwrap();
            app.log_buffer.push(
                r#"{"timestamp":"00:00:03","level":"Info","scope":"app","message":"line-3"}"#
                    .to_string(),
            );
        }

        let line = lines.next_line().await.unwrap().unwrap();
        let resp: Response = serde_json::from_str(&line).unwrap();
        match resp {
            Response::LogEntry { id, line } => {
                assert_eq!(id, 2);
                assert!(line.contains("line-3"));
            }
            other => panic!("expected LogEntry, got: {other:?}"),
        }

        drop(w);
        drop(lines);
        h.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn subscribe_logs_returns_error_for_unknown_app() {
        let (state, _tmp) = test_state();

        let (a, b) = tokio::net::UnixStream::pair().unwrap();
        let state_for_handler = state.clone();
        let h = tokio::spawn(async move { handle_client(a, state_for_handler).await });

        let (r, mut w) = b.into_split();
        let mut lines = BufReader::new(r).lines();

        let req = serde_json::json!({
            "type": "SubscribeLogs",
            "config_path": "/nonexistent/tako.toml",
        });
        w.write_all(req.to_string().as_bytes()).await.unwrap();
        w.write_all(b"\n").await.unwrap();

        let line = lines.next_line().await.unwrap().unwrap();
        let resp: Response = serde_json::from_str(&line).unwrap();
        assert!(matches!(resp, Response::Error { .. }));

        drop(w);
        h.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn subscribe_logs_counts_as_control_client() {
        let (state, _tmp) = test_state();
        insert_test_app(&state, "/proj", "my-app");

        let (a, b) = tokio::net::UnixStream::pair().unwrap();
        let state_for_handler = state.clone();
        let h = tokio::spawn(async move { handle_client(a, state_for_handler).await });

        let (r, mut w) = b.into_split();
        let mut lines = BufReader::new(r).lines();

        let req = serde_json::json!({
            "type": "SubscribeLogs",
            "config_path": "/proj/tako.toml",
        });
        w.write_all(req.to_string().as_bytes()).await.unwrap();
        w.write_all(b"\n").await.unwrap();

        let line = lines.next_line().await.unwrap().unwrap();
        assert!(line.contains("LogsSubscribed"));

        // While subscribed, control_clients should be 1.
        let clients = query_control_clients(state.clone()).await;
        assert_eq!(clients, 1);

        // Disconnect.
        drop(w);
        drop(lines);
        h.await.unwrap().unwrap();

        // After disconnect, control_clients should be 0.
        let clients = query_control_clients(state).await;
        assert_eq!(clients, 0);
    }

    #[tokio::test]
    async fn set_app_status_broadcasts_status_changed_event() {
        let (state, _tmp) = test_state();
        insert_test_app(&state, "/proj", "my-app");

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
            "config_path": "/proj/tako.toml",
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
                        config_path,
                        app_name,
                        status,
                    },
            } => {
                assert_eq!(config_path, "/proj/tako.toml");
                assert_eq!(app_name, "my-app");
                assert_eq!(status, "idle");
            }
            other => panic!("expected AppStatusChanged, got: {other:?}"),
        }
    }

    #[test]
    fn redirect_location_strips_default_http_port() {
        let location = redirect_location("bun-example.tako.test:80", "/hello");
        assert_eq!(location, "https://bun-example.tako.test/hello");
    }

    #[test]
    fn redirect_location_keeps_non_default_port() {
        let location = redirect_location("bun-example.tako.test:8080", "/");
        assert_eq!(location, "https://bun-example.tako.test:8080/");
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
        insert_test_app(&state, "/proj", "my-app");

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
            "config_path": "/proj/tako.toml",
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
                        config_path,
                        app_name,
                        status,
                    },
            } => {
                assert_eq!(config_path, "/proj/tako.toml");
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
            .get_or_create_cert("foo.tako.test")
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
            dns_names.contains(&"foo.tako.test"),
            "cert must contain foo.tako.test SAN, got: {:?}",
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
            .get_or_create_cert("foo.tako.test")
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

        let (first, _) = resolver.get_or_create_cert("bar.tako.test").unwrap();
        let (second, _) = resolver.get_or_create_cert("bar.tako.test").unwrap();

        // Same DER bytes → same cert object was returned from cache.
        assert_eq!(first.to_der().unwrap(), second.to_der().unwrap());
    }

    #[test]
    fn kill_all_app_processes_sends_sigterm_to_tracked_pids() {
        let (state, _tmp) = test_state();

        // Spawn a long-lived process we can check.
        let mut child = std::process::Command::new("sleep")
            .arg("60")
            .spawn()
            .unwrap();
        let pid = child.id();

        // Register it in memory with a PID.
        {
            let mut s = state.lock().unwrap();
            s.apps.insert(
                "/proj/tako.toml".to_string(),
                state::RuntimeApp {
                    project_dir: "/proj".to_string(),
                    name: "my-app".to_string(),
                    variant: None,
                    hosts: vec!["my-app.tako.test".to_string()],
                    upstream_port: 3000,
                    is_idle: false,
                    command: vec!["sleep".to_string(), "60".to_string()],
                    env: std::collections::HashMap::new(),
                    log_buffer: state::LogBuffer::new(),
                    pid: Some(pid),
                    client_pid: None,
                },
            );
        }

        // Verify the process is alive.
        assert_eq!(unsafe { libc::kill(pid as i32, 0) }, 0);

        kill_all_app_processes(&state);

        // wait() will return once the process has been terminated by SIGTERM.
        let status = child.wait().unwrap();
        assert!(!status.success());
    }

    // -----------------------------------------------------------------------
    // Variant support
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn register_app_with_variant_roundtrip() {
        let (a, b) = tokio::net::UnixStream::pair().unwrap();
        let (state, _tmp) = test_state();
        let h = tokio::spawn(async move { handle_client(a, state).await });

        let (r, mut w) = b.into_split();
        let mut lines = BufReader::new(r).lines();

        let req = serde_json::json!({
            "type": "RegisterApp",
            "config_path": "/proj/my-app/preview.toml",
            "project_dir": "/proj/my-app",
            "app_name": "my-app-staging",
            "variant": "staging",
            "hosts": ["my-app-staging.tako.test"],
            "upstream_port": 3000,
            "command": ["bun", "run", "index.ts"],
            "env": {},
            "log_path": "/tmp/log.jsonl"
        });
        w.write_all(req.to_string().as_bytes()).await.unwrap();
        w.write_all(b"\n").await.unwrap();

        let line = lines.next_line().await.unwrap().unwrap();
        let resp: Response = serde_json::from_str(&line).unwrap();
        match resp {
            Response::AppRegistered { app_name, .. } => {
                assert_eq!(app_name, "my-app-staging");
            }
            other => panic!("unexpected: {other:?}"),
        }

        // List and verify variant is present.
        let req = serde_json::json!({"type": "ListRegisteredApps"});
        w.write_all(req.to_string().as_bytes()).await.unwrap();
        w.write_all(b"\n").await.unwrap();

        let line = lines.next_line().await.unwrap().unwrap();
        let resp: Response = serde_json::from_str(&line).unwrap();
        match resp {
            Response::RegisteredApps { apps } => {
                assert_eq!(apps.len(), 1);
                assert_eq!(apps[0].app_name, "my-app-staging");
                assert_eq!(apps[0].variant, Some("staging".to_string()));
            }
            other => panic!("unexpected: {other:?}"),
        }

        drop(w);
        h.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn register_app_without_variant_has_none() {
        let (a, b) = tokio::net::UnixStream::pair().unwrap();
        let (state, _tmp) = test_state();
        let h = tokio::spawn(async move { handle_client(a, state).await });

        let (r, mut w) = b.into_split();
        let mut lines = BufReader::new(r).lines();

        let req = serde_json::json!({
            "type": "RegisterApp",
            "config_path": "/proj/my-app/tako.toml",
            "project_dir": "/proj/my-app",
            "app_name": "my-app",
            "hosts": ["my-app.tako.test"],
            "upstream_port": 3000,
            "command": ["bun", "run", "index.ts"],
            "env": {},
            "log_path": "/tmp/log.jsonl"
        });
        w.write_all(req.to_string().as_bytes()).await.unwrap();
        w.write_all(b"\n").await.unwrap();

        let _line = lines.next_line().await.unwrap().unwrap();

        let req = serde_json::json!({"type": "ListRegisteredApps"});
        w.write_all(req.to_string().as_bytes()).await.unwrap();
        w.write_all(b"\n").await.unwrap();

        let line = lines.next_line().await.unwrap().unwrap();
        let resp: Response = serde_json::from_str(&line).unwrap();
        match resp {
            Response::RegisteredApps { apps } => {
                assert_eq!(apps.len(), 1);
                assert_eq!(apps[0].app_name, "my-app");
                assert!(apps[0].variant.is_none());
            }
            other => panic!("unexpected: {other:?}"),
        }

        drop(w);
        h.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn variant_and_non_variant_coexist_in_list() {
        let (a, b) = tokio::net::UnixStream::pair().unwrap();
        let (state, _tmp) = test_state();
        let h = tokio::spawn(async move { handle_client(a, state).await });

        let (r, mut w) = b.into_split();
        let mut lines = BufReader::new(r).lines();

        // Register "app-foo" without variant from /proj1
        let req = serde_json::json!({
            "type": "RegisterApp",
            "config_path": "/proj1/tako.toml",
            "project_dir": "/proj1",
            "app_name": "app-foo",
            "hosts": ["app-foo.tako.test"],
            "upstream_port": 3000,
            "command": ["bun", "run", "index.ts"],
            "env": {},
            "log_path": "/tmp/log1.jsonl"
        });
        w.write_all(req.to_string().as_bytes()).await.unwrap();
        w.write_all(b"\n").await.unwrap();
        let _line = lines.next_line().await.unwrap().unwrap();

        // Register "app-foo" with variant "foo" from /proj2
        // (in practice the CLI would have disambiguated the name, but
        //  this tests that both can coexist with different project_dirs)
        let req = serde_json::json!({
            "type": "RegisterApp",
            "config_path": "/proj2/tako.toml",
            "project_dir": "/proj2",
            "app_name": "app-foo-proj2",
            "variant": "foo",
            "hosts": ["app-foo-proj2.tako.test"],
            "upstream_port": 3001,
            "command": ["bun", "run", "index.ts"],
            "env": {},
            "log_path": "/tmp/log2.jsonl"
        });
        w.write_all(req.to_string().as_bytes()).await.unwrap();
        w.write_all(b"\n").await.unwrap();
        let _line = lines.next_line().await.unwrap().unwrap();

        // List all
        let req = serde_json::json!({"type": "ListRegisteredApps"});
        w.write_all(req.to_string().as_bytes()).await.unwrap();
        w.write_all(b"\n").await.unwrap();

        let line = lines.next_line().await.unwrap().unwrap();
        let resp: Response = serde_json::from_str(&line).unwrap();
        match resp {
            Response::RegisteredApps { apps } => {
                assert_eq!(apps.len(), 2);
                let no_variant = apps.iter().find(|a| a.project_dir == "/proj1").unwrap();
                let with_variant = apps.iter().find(|a| a.project_dir == "/proj2").unwrap();
                assert_eq!(no_variant.app_name, "app-foo");
                assert!(no_variant.variant.is_none());
                assert_eq!(with_variant.app_name, "app-foo-proj2");
                assert_eq!(with_variant.variant, Some("foo".to_string()));
            }
            other => panic!("unexpected: {other:?}"),
        }

        drop(w);
        h.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn register_app_spawns_process_and_sets_pid() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (state, _tmp_db) = test_state();

        let (a, b) = tokio::net::UnixStream::pair().unwrap();
        let state_for_handler = state.clone();
        let h = tokio::spawn(async move { handle_client(a, state_for_handler).await });

        let (r, mut w) = b.into_split();
        let mut lines = BufReader::new(r).lines();

        let req = serde_json::json!({
            "type": "RegisterApp",
            "config_path": "/tmp/test-spawn/tako.toml",
            "project_dir": tmp.path().to_str().unwrap(),
            "app_name": "spawn-test",
            "hosts": ["spawn-test.tako.test"],
            "upstream_port": 19999,
            "command": ["sleep", "60"],
            "env": {},
        });
        w.write_all(format!("{}\n", req).as_bytes()).await.unwrap();

        let line = lines.next_line().await.unwrap().unwrap();
        let resp: Response = serde_json::from_str(&line).unwrap();
        assert!(matches!(resp, Response::AppRegistered { .. }));

        // Wait a moment for the background spawn task.
        tokio::time::sleep(Duration::from_millis(200)).await;

        // The app should now have a PID set by the daemon.
        let pid = {
            let s = state.lock().unwrap();
            s.apps.get("/tmp/test-spawn/tako.toml").and_then(|a| a.pid)
        };
        assert!(pid.is_some(), "daemon should have spawned the app process");

        // Clean up: kill the spawned process.
        if let Some(pid) = pid {
            kill_app_process(pid);
        }

        drop(w);
        drop(lines);
        h.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn unregister_app_kills_running_process() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (state, _tmp_db) = test_state();

        // Insert an app with a real running process.
        let mut child = std::process::Command::new("sleep")
            .arg("60")
            .spawn()
            .unwrap();
        let pid = child.id();

        {
            let mut s = state.lock().unwrap();
            s.apps.insert(
                "/tmp/test-kill/tako.toml".to_string(),
                state::RuntimeApp {
                    project_dir: tmp.path().to_string_lossy().to_string(),
                    name: "kill-test".to_string(),
                    variant: None,
                    hosts: vec!["kill-test.tako.test".to_string()],
                    upstream_port: 19998,
                    is_idle: false,
                    command: vec!["sleep".to_string(), "60".to_string()],
                    env: std::collections::HashMap::new(),
                    log_buffer: state::LogBuffer::new(),
                    pid: Some(pid),
                    client_pid: None,
                },
            );
        }

        // Unregister via socket.
        let (a, b) = tokio::net::UnixStream::pair().unwrap();
        let state_for_handler = state.clone();
        let h = tokio::spawn(async move { handle_client(a, state_for_handler).await });

        let (r, mut w) = b.into_split();
        let req = serde_json::json!({
            "type": "UnregisterApp",
            "config_path": "/tmp/test-kill/tako.toml",
        });
        w.write_all(format!("{}\n", req).as_bytes()).await.unwrap();

        let mut lines = BufReader::new(r).lines();
        let line = lines.next_line().await.unwrap().unwrap();
        let resp: Response = serde_json::from_str(&line).unwrap();
        assert!(matches!(resp, Response::AppUnregistered { .. }));

        drop(w);
        drop(lines);
        h.await.unwrap().unwrap();

        // The process should have been killed.
        let status = child.wait().unwrap();
        assert!(!status.success(), "process should have been killed");
    }

    #[test]
    fn variant_persisted_in_sqlite() {
        let (state, _tmp) = test_state();
        let s = state.lock().unwrap();
        let db = s.db.as_ref().unwrap();

        db.register("/proj/tako.toml", "/proj", "my-app", Some("staging"))
            .unwrap();
        let app = db.get("/proj/tako.toml").unwrap().unwrap();
        assert_eq!(app.name, "my-app");
        assert_eq!(app.variant.as_deref(), Some("staging"));

        // Re-register without variant clears it.
        db.register("/proj/tako.toml", "/proj", "my-app", None)
            .unwrap();
        let app = db.get("/proj/tako.toml").unwrap().unwrap();
        assert!(app.variant.is_none());
    }
}

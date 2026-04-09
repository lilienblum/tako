mod control;
mod local_ca;
mod local_dns;
mod paths;
mod process;
mod protocol;
mod proxy;
mod redirect;
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
#[cfg(test)]
use tokio::io::BufReader;
use tokio::net::UnixListener;
use tokio::sync::mpsc;
use tokio::sync::watch;

use control::{EventsHub, State, handle_client};
use process::{handle_wake_on_request, kill_all_app_processes, stale_app_cleanup_loop};
use redirect::start_http_redirect_server;

use protocol::DevEvent;
use protocol::Response;
use tracing_subscriber::EnvFilter;

const TAKO_DEV_DOMAIN: &str = "tako.test";
const SHORT_DEV_DOMAIN: &str = "test";
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

fn app_short_host(app_name: &str) -> String {
    format!("{}.{}", app_name, SHORT_DEV_DOMAIN)
}

fn app_host(app_name: &str) -> String {
    format!("{}.{}", app_name, TAKO_DEV_DOMAIN)
}

fn default_hosts(app_name: &str) -> Vec<String> {
    vec![app_short_host(app_name), app_host(app_name)]
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
mod tests;

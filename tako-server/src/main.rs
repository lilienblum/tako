// This crate contains runtime components that are exercised indirectly in integration tests.
#![allow(dead_code)]

#[cfg(not(unix))]
compile_error!("tako-server requires Unix (management commands use Unix sockets).");

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

mod app_command;
mod boot;
mod defaults;
mod instances;
mod lb;
mod metrics;
mod operations;
mod paths;
mod protocol;
mod proxy;
mod release;
mod routing;
mod runtime_events;
mod scaling;
mod socket;
mod state_store;
mod tls;
mod version_manager;

use crate::boot::{
    PrimaryStatus, certificate_renewal_task, install_rustls_crypto_provider, probe_primary_socket,
    read_server_config, sd_notify_ready,
};
use crate::instances::{AppManager, HealthChecker, HealthConfig};
use crate::lb::LoadBalancer;
use crate::release::{apply_release_runtime_to_config, release_app_path};
use crate::routing::RouteTable;
use crate::runtime_events::{handle_health_event, handle_idle_event, handle_instance_event};
use crate::scaling::{ColdStartConfig, ColdStartManager, IdleConfig, IdleMonitor};
use crate::socket::{AppState, Response, SocketServer};
use crate::state_store::{SqliteStateStore, StateStoreError, load_or_create_device_key};
use crate::tls::{AcmeClient, AcmeConfig, CertManager, CertManagerConfig, ChallengeTokens};
use clap::Parser;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tako_core::{ServerRuntimeInfo, UpgradeMode};
use tokio::sync::RwLock;
use tokio::sync::mpsc;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

pub(crate) use crate::release::is_private_local_hostname;

const DEFAULT_SERVER_LOG_FILTER: &str = "warn";
const SIGNAL_PARENT_ON_READY_ENV: &str = "TAKO_SIGNAL_PARENT_ON_READY";

fn server_version() -> &'static str {
    static VERSION: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
        let base = env!("CARGO_PKG_VERSION");
        match option_env!("TAKO_CANARY_SHA") {
            Some(sha) if !sha.trim().is_empty() => {
                let short = &sha.trim()[..sha.trim().len().min(7)];
                format!("{base}-canary-{short}")
            }
            _ => base.to_string(),
        }
    });
    &VERSION
}

/// Tako Server - Application runtime and proxy
#[derive(Parser)]
#[command(name = "tako-server")]
#[command(version = server_version())]
#[command(about = "Tako Server - Application runtime and proxy")]
pub struct Args {
    /// Unix socket path for management commands
    #[arg(long)]
    pub socket: Option<String>,

    /// HTTP port
    #[arg(long, default_value_t = 80)]
    pub port: u16,

    /// HTTPS port
    #[arg(long, default_value_t = 443)]
    pub tls_port: u16,

    /// Use Let's Encrypt staging environment
    #[arg(long)]
    pub acme_staging: bool,

    /// Data directory for apps and certificates
    #[arg(long)]
    pub data_dir: Option<String>,

    /// Disable ACME (use self-signed or manual certificates only)
    #[arg(long)]
    pub no_acme: bool,

    /// Certificate renewal check interval in hours (default: 12)
    #[arg(long, default_value_t = 12)]
    pub renewal_interval_hours: u64,

    /// Run as a hot standby: serve traffic with minimal scaling (max 1 instance
    /// per app), skip management socket and ACME. Monitors the primary
    /// server's socket — promotes to full mode if primary is unavailable,
    /// shuts down gracefully when primary comes back.
    #[arg(long)]
    pub standby: bool,

    /// Prometheus metrics port (default: 9898, set to 0 to disable)
    #[arg(long, default_value_t = 9898)]
    pub metrics_port: u16,

    /// Extract a `.tar.zst` archive into a destination directory and exit.
    #[arg(long, hide = true)]
    pub extract_zstd_archive: Option<String>,

    /// Destination directory used with `--extract-zstd-archive`.
    #[arg(long, hide = true)]
    pub extract_dest: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ServerRuntimeConfig {
    pid: u32,
    socket: String,
    data_dir: PathBuf,
    http_port: u16,
    https_port: u16,
    no_acme: bool,
    acme_staging: bool,
    renewal_interval_hours: u64,
    dns_provider: Option<String>,
    standby: bool,
    metrics_port: Option<u16>,
    server_name: Option<String>,
}

impl ServerRuntimeConfig {
    fn for_defaults(data_dir: PathBuf) -> Self {
        Self {
            pid: std::process::id(),
            socket: "/var/run/tako/tako.sock".to_string(),
            data_dir,
            http_port: 80,
            https_port: 443,
            no_acme: false,
            acme_staging: false,
            renewal_interval_hours: 12,
            dns_provider: None,
            standby: false,
            metrics_port: Some(9898),
            server_name: None,
        }
    }

    fn to_runtime_info(&self, mode: UpgradeMode) -> ServerRuntimeInfo {
        ServerRuntimeInfo {
            pid: self.pid,
            mode,
            socket: self.socket.clone(),
            data_dir: self.data_dir.to_string_lossy().to_string(),
            http_port: self.http_port,
            https_port: self.https_port,
            no_acme: self.no_acme,
            acme_staging: self.acme_staging,
            acme_email: None,
            renewal_interval_hours: self.renewal_interval_hours,
            dns_provider: self.dns_provider.clone(),
            standby: self.standby,
            metrics_port: self.metrics_port,
            server_name: self.server_name.clone(),
        }
    }
}

fn extract_zstd_archive(archive_path: &Path, dest_dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dest_dir)
        .map_err(|e| format!("create extraction dir {}: {}", dest_dir.display(), e))?;
    let file = std::fs::File::open(archive_path)
        .map_err(|e| format!("open archive {}: {}", archive_path.display(), e))?;
    let decoder = zstd::stream::read::Decoder::new(file).map_err(|e| {
        format!(
            "initialize zstd decoder for {}: {}",
            archive_path.display(),
            e
        )
    })?;
    let mut archive = tar::Archive::new(decoder);
    archive.unpack(dest_dir).map_err(|e| {
        format!(
            "extract archive {} into {}: {}",
            archive_path.display(),
            dest_dir.display(),
            e
        )
    })?;
    Ok(())
}

fn run_extract_archive_mode(args: &Args) -> Result<(), String> {
    let archive = args
        .extract_zstd_archive
        .as_deref()
        .ok_or_else(|| "Extraction mode requires --extract-zstd-archive <path>".to_string())?;
    let dest = args
        .extract_dest
        .as_deref()
        .ok_or_else(|| "Extraction mode requires --extract-dest <dir>".to_string())?;
    extract_zstd_archive(Path::new(archive), Path::new(dest))
}

/// Server state shared across components
pub struct ServerState {
    /// App manager
    app_manager: Arc<AppManager>,
    /// Load balancer
    load_balancer: Arc<LoadBalancer>,
    /// Certificate manager
    cert_manager: Arc<CertManager>,
    /// ACME client (optional, behind RwLock for runtime → full promotion)
    acme_client: RwLock<Option<Arc<AcmeClient>>>,
    /// HTTP-01 challenge tokens (always present for testing/proxy use)
    challenge_tokens: ChallengeTokens,
    /// Route table (app_name -> route patterns)
    routes: Arc<RwLock<RouteTable>>,

    /// Per-app deploy locks to prevent concurrent deploys
    deploy_locks: RwLock<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,

    /// Cold start coordinator for on-demand apps
    cold_start: Arc<ColdStartManager>,

    /// Durable runtime state store
    state_store: Arc<SqliteStateStore>,

    /// Current server upgrade mode
    server_mode: RwLock<UpgradeMode>,

    /// Runtime config used for upgrade orchestration/introspection.
    runtime: ServerRuntimeConfig,
}

impl ServerState {
    pub fn new(
        data_dir: PathBuf,
        cert_manager: Arc<CertManager>,
        acme_client: Option<Arc<AcmeClient>>,
        challenge_tokens: ChallengeTokens,
    ) -> Result<Self, StateStoreError> {
        let runtime = ServerRuntimeConfig::for_defaults(data_dir.clone());
        Self::new_with_runtime(
            data_dir,
            cert_manager,
            acme_client,
            challenge_tokens,
            runtime,
        )
    }

    pub fn new_with_runtime(
        data_dir: PathBuf,
        cert_manager: Arc<CertManager>,
        acme_client: Option<Arc<AcmeClient>>,
        challenge_tokens: ChallengeTokens,
        runtime: ServerRuntimeConfig,
    ) -> Result<Self, StateStoreError> {
        let app_manager = Arc::new(AppManager::new(data_dir.clone()));
        let load_balancer = Arc::new(LoadBalancer::new(app_manager.clone()));
        let device_key = load_or_create_device_key(&data_dir.join("secret.key"))?;
        let state_store = Arc::new(SqliteStateStore::new(data_dir.join("tako.db"), device_key));
        state_store.init()?;
        // Always start in Normal mode. If the server was previously in
        // Upgrading mode (e.g. Ctrl+C during upgrade), that upgrade is dead
        // now — the CLI drives the upgrade over SSH, so a fresh process
        // means no upgrade is in progress. Clear both the mode and the
        // upgrade lock so the next upgrade attempt isn't blocked.
        let server_mode = state_store.server_mode()?;
        if server_mode == UpgradeMode::Upgrading {
            state_store.set_server_mode(UpgradeMode::Normal)?;
            // Clear the orphaned upgrade lock so a new owner can acquire immediately.
            if let Some(owner) = state_store.upgrade_lock_owner()? {
                let _ = state_store.release_upgrade_lock(&owner);
            }
        }
        let server_mode = UpgradeMode::Normal;

        Ok(Self {
            app_manager,
            load_balancer,
            cert_manager,
            acme_client: RwLock::new(acme_client),
            challenge_tokens,
            routes: Arc::new(RwLock::new(RouteTable::default())),
            deploy_locks: RwLock::new(HashMap::new()),
            cold_start: Arc::new(ColdStartManager::new(ColdStartConfig::default())),
            state_store,
            server_mode: RwLock::new(server_mode),
            runtime,
        })
    }

    pub fn cold_start(&self) -> Arc<ColdStartManager> {
        self.cold_start.clone()
    }

    /// Set the ACME client (used during runtime → full promotion)
    pub async fn set_acme_client(&self, client: Arc<AcmeClient>) {
        *self.acme_client.write().await = Some(client);
    }

    /// Get or create a deploy lock for an app
    async fn get_deploy_lock(&self, app_name: &str) -> Arc<tokio::sync::Mutex<()>> {
        let locks = self.deploy_locks.read().await;
        if let Some(lock) = locks.get(app_name) {
            return lock.clone();
        }
        drop(locks);

        let mut locks = self.deploy_locks.write().await;
        locks
            .entry(app_name.to_string())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    }

    pub fn routes(&self) -> Arc<RwLock<RouteTable>> {
        self.routes.clone()
    }

    pub async fn set_server_mode(&self, mode: UpgradeMode) -> Result<(), StateStoreError> {
        self.state_store.set_server_mode(mode)?;
        *self.server_mode.write().await = mode;
        Ok(())
    }

    pub async fn try_enter_upgrading(&self, owner: &str) -> Result<bool, StateStoreError> {
        if !self.state_store.try_acquire_upgrade_lock(owner)? {
            return Ok(false);
        }
        self.set_server_mode(UpgradeMode::Upgrading).await?;
        Ok(true)
    }

    pub async fn exit_upgrading(&self, owner: &str) -> Result<bool, StateStoreError> {
        if !self.state_store.release_upgrade_lock(owner)? {
            return Ok(false);
        }
        self.set_server_mode(UpgradeMode::Normal).await?;
        Ok(true)
    }

    async fn reject_mutating_when_upgrading(&self, command: &str) -> Option<Response> {
        let mode = *self.server_mode.read().await;
        if mode == UpgradeMode::Upgrading {
            return Some(Response::error(format!(
                "Server is upgrading; '{}' is temporarily blocked. Please retry shortly.",
                command
            )));
        }
        None
    }

    async fn runtime_info(&self) -> ServerRuntimeInfo {
        let mode = *self.server_mode.read().await;
        self.runtime.to_runtime_info(mode)
    }

    pub async fn restore_from_state_store(&self) -> Result<(), StateStoreError> {
        let apps = self.state_store.load_apps()?;
        if apps.is_empty() {
            return Ok(());
        }

        tracing::info!(apps = apps.len(), "Restoring apps from durable state");

        for persisted in apps {
            let mut config = persisted.config.clone();
            let app_name = config.deployment_id();
            let routes = persisted.routes.clone();

            // In standby mode, cap scaling to 1 instance to minimize resources.
            if self.runtime.standby && config.min_instances > 1 {
                config.min_instances = 1;
                config.max_instances = config.max_instances.max(1);
            }

            let should_start = config.min_instances > 0;
            let release_path = release_app_path(&self.runtime.data_dir, &config);
            if let Err(error) = apply_release_runtime_to_config(&mut config, release_path, None) {
                tracing::error!(app = %app_name, "Failed to restore app config: {}", error);
                continue;
            }
            config.secrets = self.state_store.get_secrets(&app_name).unwrap_or_else(|e| {
                tracing::warn!(app = %app_name, "Failed to read secrets: {}", e);
                HashMap::new()
            });

            let app = self.app_manager.register_app(config.clone());
            self.load_balancer.register_app(app.clone());

            {
                let mut route_table = self.routes.write().await;
                route_table.set_app_routes(app_name.clone(), routes);
            }

            if should_start {
                match self.app_manager.start_app(&app_name).await {
                    Ok(()) => {
                        app.set_state(AppState::Running);
                        tracing::info!(app = %app_name, "Restored and started app");
                    }
                    Err(e) => {
                        app.set_state(AppState::Error);
                        app.set_last_error(format!("Restore startup failed: {}", e));
                        tracing::error!(app = %app_name, "Failed to start restored app: {}", e);
                    }
                }
            } else {
                app.set_state(AppState::Idle);
                self.cold_start.reset(&app_name);
                tracing::info!(app = %app_name, "Restored on-demand app in idle state");
            }
        }

        Ok(())
    }

    async fn persist_app_state(&self, app_name: &str) {
        let Some(app) = self.app_manager.get_app(app_name) else {
            return;
        };
        let config = app.config.read().clone();
        let routes = {
            let route_table = self.routes.read().await;
            route_table.routes_for_app(app_name)
        };
        if let Err(e) = self.state_store.upsert_app(&config, &routes) {
            tracing::warn!(app = app_name, "Failed to persist app state: {}", e);
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    install_rustls_crypto_provider();

    // Initialize tracing with a non-blocking writer so log I/O never stalls
    // Tokio worker threads (critical under high request volume / DDoS).
    let (non_blocking, _guard) = tracing_appender::non_blocking(std::io::stdout());
    tracing_subscriber::registry()
        .with(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new(DEFAULT_SERVER_LOG_FILTER)),
        )
        .with(
            tracing_subscriber::fmt::layer()
                .json()
                .with_target(false)
                .with_writer(non_blocking),
        )
        .init();

    let args = Args::parse();
    if args.extract_zstd_archive.is_some() || args.extract_dest.is_some() {
        run_extract_archive_mode(&args)?;
        return Ok(());
    }

    // Tokio runtime for all non-proxy async tasks (socket, health checks, ACME, etc).
    // Pingora manages its own runtime(s) internally.
    let rt = tokio::runtime::Runtime::new()?;

    let exe = std::env::current_exe().ok();

    let socket = args.socket.clone().unwrap_or_else(|| {
        if cfg!(debug_assertions)
            && let Some(exe) = &exe
            && let Some(p) = crate::paths::debug_default_socket_from_exe(exe)
        {
            return p.to_string_lossy().to_string();
        }
        "/var/run/tako/tako.sock".to_string()
    });

    let data_dir_str = args.data_dir.clone().unwrap_or_else(|| {
        if cfg!(debug_assertions)
            && let Some(exe) = &exe
            && let Some(p) = crate::paths::debug_default_data_dir_from_exe(exe)
        {
            return p.to_string_lossy().to_string();
        }
        "/var/lib/tako".to_string()
    });

    let standby = args.standby;

    tracing::info!("Tako Server v{}", env!("CARGO_PKG_VERSION"));
    if standby {
        tracing::info!("Mode: standby");
    }
    tracing::info!("Socket: {}", socket);
    tracing::info!("HTTP port: {}", args.port);
    tracing::info!("HTTPS port: {}", args.tls_port);
    tracing::info!("Data directory: {}", data_dir_str);

    // Create data directory
    let data_dir = PathBuf::from(&data_dir_str);
    std::fs::create_dir_all(&data_dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&data_dir, std::fs::Permissions::from_mode(0o700));
    }

    // Create socket parent directory (for debug/local usage)
    if let Some(parent) = PathBuf::from(&socket).parent() {
        std::fs::create_dir_all(parent)?;
    }

    // Setup certificate directories
    let cert_dir = data_dir.join("certs");
    let acme_dir = data_dir.join("acme");
    std::fs::create_dir_all(&cert_dir)?;
    std::fs::create_dir_all(&acme_dir)?;

    // Create certificate manager
    let cert_manager_config = CertManagerConfig {
        cert_dir: cert_dir.clone(),
        ..Default::default()
    };
    let cert_manager = Arc::new(CertManager::new(cert_manager_config));

    // Initialize cert manager (loads existing certs)
    if let Err(e) = cert_manager.init() {
        tracing::warn!("Failed to initialize certificate manager: {}", e);
    }

    // Bind management socket EARLY — before any potentially slow init (ACME, SQLite).
    // This ensures the new process takes over the symlink within milliseconds of
    // starting, so `tako servers upgrade` never times out waiting for the socket.
    // In standby mode, skip binding — let the primary own the socket.
    let (_socket_server, socket_listener) = if standby {
        (None, None)
    } else {
        let server = SocketServer::new(&socket);
        let listener = server
            .bind()
            .map_err(|e| format!("Failed to bind management socket: {e}"))?;
        (Some(server), Some(listener))
    };

    // Read server-level config (server name, DNS provider)
    let server_config = read_server_config(&data_dir);
    let config_dns_provider = server_config.dns.map(|d| d.provider);

    // Shared challenge tokens for HTTP-01 validation.
    // Always created so the proxy can serve challenge responses and tests can inject tokens.
    let challenge_tokens: ChallengeTokens = Arc::new(parking_lot::RwLock::new(HashMap::new()));

    // Create ACME client if enabled (skip in standby mode)
    let acme_client = if args.no_acme || standby {
        if standby {
            tracing::info!("ACME disabled (standby mode)");
        } else {
            tracing::info!("ACME disabled, using manual certificate management");
        }
        None
    } else {
        let acme_config = AcmeConfig {
            staging: args.acme_staging,
            email: server_config.acme_email.clone(),
            account_dir: acme_dir,
            dns_provider: config_dns_provider.clone(),
            data_dir: data_dir.clone(),
            ..Default::default()
        };

        let client = Arc::new(AcmeClient::with_tokens(
            acme_config,
            cert_manager.clone(),
            challenge_tokens.clone(),
        ));

        // Initialize ACME account
        if let Err(e) = rt.block_on(client.init()) {
            tracing::error!("Failed to initialize ACME client: {}", e);
            tracing::warn!("Continuing without ACME - certificates must be managed manually");
            None
        } else {
            if args.acme_staging {
                tracing::warn!(
                    "Using Let's Encrypt STAGING environment - certificates will NOT be trusted!"
                );
            } else {
                tracing::info!("ACME client initialized with Let's Encrypt production");
            }
            Some(client)
        }
    };

    // Always pass challenge tokens to proxy so HTTP-01 challenges are served
    let acme_tokens = Some(challenge_tokens.clone());

    // Create server state
    let runtime = ServerRuntimeConfig {
        pid: std::process::id(),
        socket: socket.clone(),
        data_dir: data_dir.clone(),
        http_port: args.port,
        https_port: args.tls_port,
        no_acme: args.no_acme,
        acme_staging: args.acme_staging,
        renewal_interval_hours: args.renewal_interval_hours,
        dns_provider: config_dns_provider.clone(),
        standby,
        metrics_port: if args.metrics_port == 0 {
            None
        } else {
            Some(args.metrics_port)
        },
        server_name: server_config.server_name.or_else(|| {
            hostname::get()
                .ok()
                .and_then(|h| h.into_string().ok())
                .filter(|h| !h.is_empty())
        }),
    };
    // Clone challenge_tokens before moving into ServerState (needed for runtime promotion)
    let challenge_tokens_for_promote = challenge_tokens.clone();
    let state = Arc::new(ServerState::new_with_runtime(
        data_dir.clone(),
        cert_manager.clone(),
        acme_client.clone(),
        challenge_tokens,
        runtime,
    )?);

    if let Err(e) = rt.block_on(state.restore_from_state_store()) {
        tracing::error!("Failed to restore server state from SQLite: {}", e);
        return Err(e.into());
    }

    // Take event receiver for monitoring
    let (health_event_tx, mut health_event_rx) = mpsc::channel(256);
    if let Some(mut event_rx) = state.app_manager.take_event_receiver() {
        let state_clone = state.clone();
        rt.spawn(async move {
            while let Some(event) = event_rx.recv().await {
                handle_instance_event(&state_clone, event).await;
            }
        });
    }

    // Start health checker for all apps
    let health_config = HealthConfig::default();
    let health_checker = Arc::new(HealthChecker::new(health_config, health_event_tx));
    let app_manager_clone = state.app_manager.clone();
    let health_checker_clone = health_checker.clone();
    rt.spawn(async move {
        // Track tasks for each app
        let mut app_tasks: std::collections::HashMap<String, tokio::task::JoinHandle<()>> =
            std::collections::HashMap::new();

        loop {
            // Get current app list
            let app_names = app_manager_clone.list_apps();
            let app_set: std::collections::HashSet<_> = app_names.into_iter().collect();

            // Start monitoring for new apps
            for app_name in &app_set {
                if !app_tasks.contains_key(app_name)
                    && let Some(app) = app_manager_clone.get_app(app_name)
                {
                    let checker = health_checker_clone.clone();
                    let task = tokio::spawn(async move {
                        checker.monitor_app(app).await;
                    });
                    app_tasks.insert(app_name.clone(), task);
                }
            }

            // Remove tasks for apps that no longer exist
            app_tasks.retain(|app_name, task| {
                if !app_set.contains(app_name) {
                    task.abort();
                    false
                } else {
                    true
                }
            });

            // Check every second
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    });

    // Handle health events
    let health_state = state.clone();
    rt.spawn(async move {
        while let Some(event) = health_event_rx.recv().await {
            handle_health_event(&health_state, event).await;
        }
    });

    // Start idle monitor for all apps
    let (idle_event_tx, mut idle_event_rx) = mpsc::channel(256);
    let idle_monitor = Arc::new(IdleMonitor::new(IdleConfig::default(), idle_event_tx));
    let app_manager_clone = state.app_manager.clone();
    let idle_monitor_clone = idle_monitor.clone();
    rt.spawn(async move {
        let mut app_tasks: std::collections::HashMap<String, tokio::task::JoinHandle<()>> =
            std::collections::HashMap::new();

        loop {
            let app_names = app_manager_clone.list_apps();
            let app_set: std::collections::HashSet<_> = app_names.into_iter().collect();

            for app_name in &app_set {
                if !app_tasks.contains_key(app_name)
                    && let Some(app) = app_manager_clone.get_app(app_name)
                {
                    let monitor = idle_monitor_clone.clone();
                    let task = tokio::spawn(async move {
                        monitor.monitor_app(app).await;
                    });
                    app_tasks.insert(app_name.clone(), task);
                }
            }

            app_tasks.retain(|app_name, task| {
                if !app_set.contains(app_name) {
                    task.abort();
                    false
                } else {
                    true
                }
            });

            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    });

    let idle_state = state.clone();
    rt.spawn(async move {
        while let Some(event) = idle_event_rx.recv().await {
            handle_idle_event(&idle_state, event).await;
        }
    });

    // Start certificate renewal background task
    if let Some(ref acme) = acme_client {
        let acme_clone = acme.clone();
        let interval = Duration::from_secs(args.renewal_interval_hours * 3600);
        rt.spawn(certificate_renewal_task(acme_clone, interval));
    }

    // Start management socket accept loop (listener was bound early, before ACME/SQLite init).
    // In standby mode, no socket is bound — the primary owns it.
    if let Some(socket_listener) = socket_listener {
        let socket_state = state.clone();
        rt.spawn(async move {
            if let Err(e) = SocketServer::serve(socket_listener, move |cmd| {
                let state = socket_state.clone();
                async move { state.handle_command(cmd).await }
            })
            .await
            {
                tracing::error!("Socket server error: {}", e);
            }
        });
    }

    // In standby mode, monitor the primary's management socket.
    // - If primary goes down: promote to full mode (bind socket, init ACME).
    // - If primary comes back (after we promoted): gracefully shut down.
    if standby {
        let socket_path = socket.clone();
        let promote_state = state.clone();
        let promote_cert_manager = cert_manager.clone();
        let promote_args_acme_staging = args.acme_staging;
        let promote_acme_email = server_config.acme_email.clone();
        let promote_dns_provider = config_dns_provider;
        let promote_args_no_acme = args.no_acme;
        let promote_renewal_hours = args.renewal_interval_hours;
        let promote_data_dir = data_dir.clone();
        let promote_challenge_tokens = challenge_tokens_for_promote;
        let our_pid = std::process::id();
        rt.spawn(async move {
            const PROBE_INTERVAL: Duration = Duration::from_secs(5);
            const FAILURE_THRESHOLD: u32 = 3;
            let mut consecutive_failures: u32 = 0;
            let mut promoted = false;

            loop {
                tokio::time::sleep(PROBE_INTERVAL).await;

                let probe_result = probe_primary_socket(&socket_path, our_pid).await;

                match probe_result {
                    PrimaryStatus::Alive => {
                        if promoted {
                            // Primary came back — we should shut down gracefully.
                            tracing::info!("Primary server is back — standby shutting down");
                            #[cfg(unix)]
                            unsafe {
                                libc::kill(libc::getpid(), libc::SIGTERM);
                            }
                            break;
                        }
                        if consecutive_failures > 0 {
                            tracing::debug!("Primary socket is alive again");
                        }
                        consecutive_failures = 0;
                    }
                    PrimaryStatus::IsUs => {
                        // Socket points to us (we promoted) — keep running.
                        consecutive_failures = 0;
                    }
                    PrimaryStatus::Down => {
                        consecutive_failures += 1;
                        tracing::warn!(
                            failures = consecutive_failures,
                            "Primary management socket not responding"
                        );

                        if consecutive_failures >= FAILURE_THRESHOLD && !promoted {
                            tracing::info!("Promoting standby to full mode");

                            let server = SocketServer::new(&socket_path);
                            match server.bind() {
                                Ok(listener) => {
                                    let socket_state = promote_state.clone();
                                    tokio::spawn(async move {
                                        if let Err(e) = SocketServer::serve(listener, move |cmd| {
                                            let state = socket_state.clone();
                                            async move { state.handle_command(cmd).await }
                                        })
                                        .await
                                        {
                                            tracing::error!(
                                                "Socket server error after promotion: {e}"
                                            );
                                        }
                                    });
                                    // Prevent Drop from removing socket file
                                    std::mem::forget(server);
                                    tracing::info!("Management socket bound after promotion");
                                }
                                Err(e) => {
                                    tracing::error!(
                                        "Failed to bind management socket on promotion: {e}"
                                    );
                                    consecutive_failures = 0;
                                    continue;
                                }
                            }

                            // Initialize ACME if enabled
                            if !promote_args_no_acme {
                                let acme_dir = promote_data_dir.join("acme");
                                let acme_config = AcmeConfig {
                                    staging: promote_args_acme_staging,
                                    email: promote_acme_email.clone(),
                                    account_dir: acme_dir,
                                    dns_provider: promote_dns_provider.clone(),
                                    data_dir: promote_data_dir.clone(),
                                    ..Default::default()
                                };
                                let client = Arc::new(AcmeClient::with_tokens(
                                    acme_config,
                                    promote_cert_manager.clone(),
                                    promote_challenge_tokens.clone(),
                                ));
                                match client.init().await {
                                    Ok(()) => {
                                        tracing::info!("ACME initialized after promotion");
                                        let interval =
                                            Duration::from_secs(promote_renewal_hours * 3600);
                                        tokio::spawn(certificate_renewal_task(
                                            client.clone(),
                                            interval,
                                        ));
                                        promote_state.set_acme_client(client).await;
                                    }
                                    Err(e) => {
                                        tracing::error!("Failed to init ACME after promotion: {e}");
                                    }
                                }
                            }

                            promoted = true;
                            consecutive_failures = 0;
                            tracing::info!(
                                "Promotion complete — standby now running as full server"
                            );
                        }
                    }
                }
            }
        });
    }

    // Configure proxy
    let dev_mode = cfg!(debug_assertions);
    let proxy_config = proxy::ProxyConfig {
        http_port: args.port,
        https_port: args.tls_port,
        enable_https: true,
        dev_mode,
        cert_dir,
        redirect_http_to_https: true,
        response_cache: Some(proxy::ResponseCacheConfig::default()),
        metrics_port: if args.metrics_port == 0 {
            None
        } else {
            Some(args.metrics_port)
        },
    };

    // Start Pingora proxy
    tracing::info!("Starting HTTP proxy on port {}", args.port);
    if proxy_config.enable_https {
        tracing::info!("HTTPS enabled on port {}", args.tls_port);
    }

    // Register SIGHUP and SIGUSR1 handlers before starting the proxy.
    // SIGHUP  → spawn a new server process for zero-downtime reload.
    // SIGUSR1 → new process is ready; send SIGTERM to self so Pingora drains gracefully.
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};

        // Resolve the exe path now (at startup) so SIGHUP can find it even if
        // the binary is replaced via rm + cp while the process is running.
        // /proc/self/exe would point to a deleted file in that case.
        let startup_exe = exe.clone();

        rt.spawn(async move {
            let mut sighup = match signal(SignalKind::hangup()) {
                Ok(signal) => signal,
                Err(err) => {
                    tracing::error!("Failed to register SIGHUP handler: {err}");
                    return;
                }
            };
            sighup.recv().await;
            tracing::info!(
                "SIGHUP received — spawning new server process for zero-downtime reload"
            );
            let exe = match &startup_exe {
                Some(p) => p.clone(),
                None => match std::env::current_exe() {
                    Ok(p) => p,
                    Err(e) => {
                        tracing::error!("Failed to get current exe: {e}");
                        return;
                    }
                },
            };
            let args: Vec<String> = std::env::args().skip(1).collect();
            match std::process::Command::new(&exe)
                .args(&args)
                .env(SIGNAL_PARENT_ON_READY_ENV, "1")
                .spawn()
            {
                Ok(child) => tracing::info!(pid = child.id(), "New server process spawned"),
                Err(e) => tracing::error!("Failed to spawn new server: {e}"),
            }
        });

        rt.spawn(async move {
            let mut sigusr1 = match signal(SignalKind::user_defined1()) {
                Ok(signal) => signal,
                Err(err) => {
                    tracing::error!("Failed to register SIGUSR1 handler: {err}");
                    return;
                }
            };
            sigusr1.recv().await;
            tracing::info!("SIGUSR1 received — new process ready, starting graceful drain");
            // Send SIGTERM to ourselves; Pingora's SIGTERM handler drains and exits.
            unsafe { libc::kill(libc::getpid(), libc::SIGTERM) };
        });
    }

    metrics::init(state.runtime.server_name.as_deref());

    let server = proxy::build_server_with_acme(
        state.load_balancer.clone(),
        state.routes(),
        proxy_config,
        acme_tokens,
        Some(cert_manager),
        state.cold_start(),
    )?;

    // Notify systemd that the server is ready and claim the main PID.
    // This also signals the parent process from the prior reload generation to start draining.
    sd_notify_ready();

    // Run the server (this blocks until SIGTERM triggers graceful shutdown)
    server.run_forever();

    #[allow(unreachable_code)]
    Ok(())
}

#[cfg(test)]
mod tests;

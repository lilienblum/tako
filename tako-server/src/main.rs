// This crate contains runtime components that are exercised indirectly in integration tests.
#![allow(dead_code)]

#[cfg(not(unix))]
compile_error!("tako-server requires Unix (management and app routing use Unix sockets).");

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

mod app_command;
mod app_socket_cleanup;
mod defaults;
mod instances;
mod lb;
mod metrics;
mod paths;
mod protocol;
mod proxy;
mod routing;
mod scaling;
mod socket;
mod state_store;
mod tls;
mod version_manager;

use crate::app_command::{
    command_from_manifest, env_vars_from_release_dir, load_release_manifest,
    runtime_from_release_dir, write_runtime_bin,
};
use crate::instances::{
    App, AppConfig, AppManager, HealthChecker, HealthConfig, InstanceEvent, RollingUpdateConfig,
    RollingUpdater, target_new_instances_for_build,
};
use crate::lb::LoadBalancer;
use crate::routing::RouteTable;
use crate::scaling::{ColdStartConfig, ColdStartManager, IdleConfig, IdleEvent, IdleMonitor};
use crate::socket::{
    AppState, AppStatus, BuildStatus, Command, InstanceState, InstanceStatus, Response,
    SocketServer,
};
use crate::state_store::{SqliteStateStore, StateStoreError, load_or_create_device_key};
use crate::tls::{
    AcmeClient, AcmeConfig, CertInfo, CertManager, CertManagerConfig, ChallengeTokens,
};
use clap::Parser;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::sync::Arc;
use std::time::Duration;
use tako_core::{
    HelloResponse, ListReleasesResponse, PROTOCOL_VERSION, ReleaseInfo, ServerRuntimeInfo,
    UpgradeMode,
};
use tokio::process::Command as TokioCommand;
use tokio::sync::RwLock;
use tokio::sync::mpsc;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

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

    /// ACME contact email for Let's Encrypt
    #[arg(long)]
    pub acme_email: Option<String>,

    /// Data directory for apps and certificates
    #[arg(long)]
    pub data_dir: Option<String>,

    /// Disable ACME (use self-signed or manual certificates only)
    #[arg(long)]
    pub no_acme: bool,

    /// Certificate renewal check interval in hours (default: 12)
    #[arg(long, default_value_t = 12)]
    pub renewal_interval_hours: u64,

    /// Run as a worker: serve traffic with minimal scaling (max 1 instance
    /// per app), skip management socket and ACME. Monitors the primary
    /// server's socket — promotes to full mode if primary is unavailable,
    /// shuts down gracefully when primary comes back.
    #[arg(long)]
    pub worker: bool,

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
    acme_email: Option<String>,
    renewal_interval_hours: u64,
    dns_provider: Option<String>,
    worker: bool,
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
            acme_email: None,
            renewal_interval_hours: 12,
            dns_provider: None,
            worker: false,
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
            acme_email: self.acme_email.clone(),
            renewal_interval_hours: self.renewal_interval_hours,
            dns_provider: self.dns_provider.clone(),
            worker: self.worker,
            metrics_port: self.metrics_port,
            server_name: self.server_name.clone(),
        }
    }

    fn app_socket_dir(&self) -> PathBuf {
        // Prefer /var/run/tako-app (production) for isolation; fall back to
        // a directory derived from the management socket path (dev/local runs).
        let preferred = PathBuf::from("/var/run/tako-app");
        if preferred.exists() {
            preferred
        } else {
            app_socket_dir_for_management_socket(&self.socket)
        }
    }
}

fn app_socket_dir_for_management_socket(socket_path: &str) -> PathBuf {
    let socket_path = Path::new(socket_path);
    let Some(parent) = socket_path.parent() else {
        return PathBuf::from("/var/run");
    };

    if parent.file_name().is_some_and(|name| name == "tako")
        && let Some(grandparent) = parent.parent()
    {
        return grandparent.to_path_buf();
    }

    parent.to_path_buf()
}

fn app_socket_cleanup_dirs(socket_path: &str) -> Vec<PathBuf> {
    let mut dirs = vec![
        app_socket_dir_for_management_socket(socket_path),
        PathBuf::from("/var/run"),
        PathBuf::from("/var/run/tako"),
    ];
    if let Some(parent) = Path::new(socket_path).parent() {
        dirs.push(parent.to_path_buf());
    }
    dirs.sort();
    dirs.dedup();
    dirs
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
        let app_manager = Arc::new(AppManager::new());
        let load_balancer = Arc::new(LoadBalancer::new(app_manager.clone()));
        let device_key = load_or_create_device_key(&data_dir.join("secret.key"))?;
        let state_store = Arc::new(SqliteStateStore::new(
            data_dir.join("runtime-state.sqlite3"),
            device_key,
        ));
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

            // In worker mode, cap scaling to 1 instance to minimize resources.
            if self.runtime.worker && config.min_instances > 1 {
                config.min_instances = 1;
                config.max_instances = config.max_instances.max(1);
            }

            let should_start = config.min_instances > 0;
            let release_path = release_app_path(&self.runtime.data_dir, &config);
            if let Err(error) = apply_release_runtime_to_config(
                &mut config,
                release_path,
                self.runtime.app_socket_dir(),
            ) {
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

    /// Handle a command from the management socket
    pub async fn handle_command(&self, cmd: Command) -> Response {
        match cmd {
            Command::Hello { protocol_version } => {
                let data = HelloResponse {
                    protocol_version: PROTOCOL_VERSION,
                    server_version: server_version().to_string(),
                    capabilities: vec![
                        "on_demand_cold_start".to_string(),
                        "idle_scale_to_zero".to_string(),
                        "scale".to_string(),
                        "upgrade_mode_control".to_string(),
                        "server_runtime_info".to_string(),
                        "release_history".to_string(),
                        "rollback".to_string(),
                    ],
                };

                if protocol_version != PROTOCOL_VERSION {
                    return Response::error(format!(
                        "Protocol version mismatch: client={} server={}",
                        protocol_version, PROTOCOL_VERSION
                    ));
                }

                Response::ok(data)
            }
            Command::Deploy {
                app,
                version,
                path,
                routes,
                secrets,
            } => {
                if let Err(msg) = validate_app_name(&app) {
                    return Response::error(msg);
                }
                if let Err(msg) = validate_release_version(&version) {
                    return Response::error(msg);
                }
                if let Some(resp) = self.reject_mutating_when_upgrading("deploy").await {
                    return resp;
                }
                self.deploy_app(&app, &version, &path, routes, secrets)
                    .await
            }
            Command::Scale { app, instances } => {
                if let Err(msg) = validate_app_name(&app) {
                    return Response::error(msg);
                }
                if let Some(resp) = self.reject_mutating_when_upgrading("scale").await {
                    return resp;
                }
                self.scale_app(&app, instances).await
            }
            Command::Stop { app } => {
                if let Err(msg) = validate_app_name(&app) {
                    return Response::error(msg);
                }
                if let Some(resp) = self.reject_mutating_when_upgrading("stop").await {
                    return resp;
                }
                self.stop_app(&app).await
            }
            Command::Delete { app } => {
                if let Err(msg) = validate_app_name(&app) {
                    return Response::error(msg);
                }
                if let Some(resp) = self.reject_mutating_when_upgrading("delete").await {
                    return resp;
                }
                self.delete_app(&app).await
            }
            Command::Status { app } => {
                if let Err(msg) = validate_app_name(&app) {
                    return Response::error(msg);
                }
                self.get_status(&app).await
            }
            Command::List => self.list_apps().await,
            Command::ListReleases { app } => {
                if let Err(msg) = validate_app_name(&app) {
                    return Response::error(msg);
                }
                self.list_releases(&app).await
            }
            Command::Routes => self.list_routes().await,
            Command::Rollback { app, version } => {
                if let Err(msg) = validate_app_name(&app) {
                    return Response::error(msg);
                }
                if let Err(msg) = validate_release_version(&version) {
                    return Response::error(msg);
                }
                if let Some(resp) = self.reject_mutating_when_upgrading("rollback").await {
                    return resp;
                }
                self.rollback_app(&app, &version).await
            }
            Command::UpdateSecrets { app, secrets } => {
                if let Err(msg) = validate_app_name(&app) {
                    return Response::error(msg);
                }
                if let Some(resp) = self.reject_mutating_when_upgrading("update-secrets").await {
                    return resp;
                }
                self.update_secrets(&app, secrets).await
            }
            Command::GetSecretsHash { app } => {
                if let Err(msg) = validate_app_name(&app) {
                    return Response::error(msg);
                }
                let secrets = self.state_store.get_secrets(&app).unwrap_or_default();
                let hash = tako_core::compute_secrets_hash(&secrets);
                Response::ok(serde_json::json!({ "hash": hash }))
            }
            Command::ServerInfo => Response::ok(self.runtime_info().await),
            Command::EnterUpgrading { owner } => match self.try_enter_upgrading(&owner).await {
                Ok(true) => Response::ok(serde_json::json!({
                    "status": "upgrading",
                    "owner": owner
                })),
                Ok(false) => {
                    let owner_msg = self
                        .state_store
                        .upgrade_lock_owner()
                        .ok()
                        .flatten()
                        .unwrap_or_else(|| "unknown".to_string());
                    Response::error(format!(
                        "Server is already upgrading (owner: {}).",
                        owner_msg
                    ))
                }
                Err(e) => Response::error(format!("Failed to enter upgrading mode: {}", e)),
            },
            Command::ExitUpgrading { owner } => match self.exit_upgrading(&owner).await {
                Ok(true) => Response::ok(serde_json::json!({
                    "status": "normal",
                    "owner": owner
                })),
                Ok(false) => Response::error(
                    "Failed to exit upgrading mode: owner does not hold the upgrade lock."
                        .to_string(),
                ),
                Err(e) => Response::error(format!("Failed to exit upgrading mode: {}", e)),
            },
            Command::InjectChallengeToken {
                token,
                key_authorization,
            } => {
                let mut tokens = self.challenge_tokens.write();
                tokens.insert(token.clone(), key_authorization);
                Response::ok(serde_json::json!({
                    "status": "injected",
                    "token": token
                }))
            }
        }
    }

    async fn deploy_app(
        &self,
        app_name: &str,
        version: &str,
        path: &str,
        routes: Vec<String>,
        secrets: Option<HashMap<String, String>>,
    ) -> Response {
        tracing::info!(app = app_name, version = version, "Deploying app");

        if let Err(msg) = validate_app_name(app_name) {
            return Response::error(msg);
        }
        if let Err(msg) = validate_release_version(version) {
            return Response::error(msg);
        }
        if let Err(msg) = validate_deploy_routes(&routes) {
            return Response::error(msg);
        }
        let release_path =
            match validate_release_path_for_app(&self.runtime.data_dir, app_name, path) {
                Ok(value) => value,
                Err(msg) => return Response::error(msg),
            };

        // Acquire deploy lock for this app (non-blocking check)
        let lock = self.get_deploy_lock(app_name).await;
        let _guard = match lock.try_lock() {
            Ok(guard) => guard,
            Err(_) => {
                tracing::warn!(
                    app = app_name,
                    "Deploy rejected: another deploy in progress"
                );
                return Response::error(format!(
                    "Deploy already in progress for app '{}'. Please wait and try again.",
                    app_name
                ));
            }
        };

        let env_vars = match env_vars_from_release_dir(&release_path) {
            Ok(vars) => vars,
            Err(error) => return Response::error(format!("Invalid app release: {}", error)),
        };

        // Resolve secrets: if provided, store in SQLite; otherwise keep existing.
        let secrets = if let Some(new_secrets) = secrets {
            if let Err(e) = self.state_store.set_secrets(app_name, &new_secrets) {
                return Response::error(format!("Failed to store secrets: {}", e));
            }
            new_secrets
        } else {
            self.state_store.get_secrets(app_name).unwrap_or_default()
        };
        let mut release_env = env_vars.clone();
        release_env.extend(secrets.clone());

        if let Err(error) = prepare_release_runtime(&release_path, &release_env).await {
            return Response::error(format!("Invalid app release: {}", error));
        }

        // Allow tako-app (in tako group) to read the release directory
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&release_path, std::fs::Permissions::from_mode(0o750));
        }

        let app_subdir = match app_subdir_from_release_path(
            &self.runtime.data_dir,
            app_name,
            version,
            &release_path,
        ) {
            Ok(value) => value,
            Err(error) => return Response::error(error),
        };

        // Get or create app
        let (app, deploy_config, is_new_app) =
            if let Some(existing) = self.app_manager.get_app(app_name) {
                // Update existing app config. We'll perform a rolling update if instances are running.
                let mut config = existing.config.read().clone();
                config.version = version.to_string();
                config.app_subdir = app_subdir.clone();
                config.secrets = secrets;
                if let Err(error) = apply_release_runtime_to_config(
                    &mut config,
                    release_path.clone(),
                    self.runtime.app_socket_dir(),
                ) {
                    return Response::error(format!("Invalid app release: {}", error));
                }
                existing.update_config(config.clone());
                (existing, config, false)
            } else {
                // Create new app
                let (name, environment) = requested_deployment_identity(app_name);
                let config = AppConfig {
                    name,
                    environment,
                    version: version.to_string(),
                    app_subdir,
                    secrets,
                    min_instances: 0,
                    max_instances: 4,
                    ..Default::default()
                };
                let mut config = config;
                if let Err(error) = apply_release_runtime_to_config(
                    &mut config,
                    release_path.clone(),
                    self.runtime.app_socket_dir(),
                ) {
                    return Response::error(format!("Invalid app release: {}", error));
                }

                let deploy_config = config.clone();
                let app = self.app_manager.register_app(config);
                self.load_balancer.register_app(app.clone());
                (app, deploy_config, true)
            };

        // Update route table for this app
        {
            let mut route_table = self.routes.write().await;
            route_table.set_app_routes(app_name.to_string(), routes.clone());
        }

        app.clear_last_error();

        // Ensure certificates for route domains.
        // Private/local domains get self-signed certs; public domains use ACME when enabled.
        for route in &routes {
            // Extract domain from route (e.g., "api.example.com/api/*" -> "api.example.com")
            let domain = route.split('/').next().unwrap_or(route);
            self.ensure_route_certificate(app_name, domain).await;
        }

        // Start the app
        if app.get_instances().is_empty() {
            // First deploy / currently stopped.
            if deploy_config.min_instances == 0 {
                match self.start_on_demand_warm_instance(&app).await {
                    Ok(()) => {
                        app.set_state(AppState::Running);
                        self.cold_start.reset(app_name);
                        self.persist_app_state(app_name).await;
                        Response::ok(serde_json::json!({
                            "status": "deployed",
                            "app": app_name,
                            "version": version,
                            "new_app": is_new_app,
                            "on_demand": true,
                            "startup_validated": true,
                            "warm_instance": true
                        }))
                    }
                    Err(e) => {
                        app.set_state(AppState::Error);
                        Response::error(format!("Deploy failed: {}", e))
                    }
                }
            } else {
                match self.app_manager.start_app(app_name).await {
                    Ok(()) => {
                        app.set_state(AppState::Running);
                        self.persist_app_state(app_name).await;
                        Response::ok(serde_json::json!({
                            "status": "deployed",
                            "app": app_name,
                            "version": version,
                            "new_app": is_new_app,
                            "on_demand": false
                        }))
                    }
                    Err(e) => {
                        app.set_state(AppState::Error);
                        Response::error(format!("Deploy failed: {}", e))
                    }
                }
            }
        } else {
            // Existing app: do a rolling update to apply new version/path.
            let previous_state = app.state();
            app.set_state(AppState::Deploying);

            let rolling_config = RollingUpdateConfig::default();
            let updater = RollingUpdater::new(self.app_manager.spawner().clone(), rolling_config);
            let target_new_instances = target_new_instances_for_build(
                deploy_config.min_instances,
                app.get_instances().len(),
            );

            match updater
                .update(&app, deploy_config.clone(), target_new_instances)
                .await
            {
                Ok(result) => {
                    if result.success {
                        if deploy_config.min_instances == 0 {
                            app.set_state(AppState::Running);
                            self.cold_start.reset(app_name);
                            self.persist_app_state(app_name).await;
                            Response::ok(serde_json::json!({
                                "status": "deployed",
                                "app": app_name,
                                "version": version,
                                "new_instances": result.new_instances,
                                "old_instances": result.old_instances,
                                "rolled_back": false,
                                "on_demand": true,
                                "startup_validated": true,
                                "warm_instance": true
                            }))
                        } else {
                            app.set_state(AppState::Running);
                            self.persist_app_state(app_name).await;
                            Response::ok(serde_json::json!({
                                "status": "deployed",
                                "app": app_name,
                                "version": version,
                                "new_instances": result.new_instances,
                                "old_instances": result.old_instances,
                                "rolled_back": false
                            }))
                        }
                    } else {
                        app.set_state(previous_state);
                        Response::error(
                            serde_json::json!({
                                "status": "rollback",
                                "app": app_name,
                                "error": result.error,
                                "rolled_back": true
                            })
                            .to_string(),
                        )
                    }
                }
                Err(e) => {
                    app.set_state(AppState::Error);
                    Response::error(format!("Rolling update failed: {}", e))
                }
            }
        }
    }

    async fn stop_app(&self, app_name: &str) -> Response {
        tracing::info!(app = app_name, "Stopping app");

        match self.app_manager.stop_app(app_name).await {
            Ok(()) => Response::ok(serde_json::json!({
                "status": "stopped",
                "app": app_name
            })),
            Err(e) => Response::error(format!("Stop failed: {}", e)),
        }
    }

    async fn scale_app(&self, app_name: &str, requested_instances: u8) -> Response {
        tracing::info!(app = app_name, requested_instances, "Scaling app");

        let app = match self.app_manager.get_app(app_name) {
            Some(app) => app,
            None => return Response::error(format!("App not found: {}", app_name)),
        };

        let previous_config = app.config.read().clone();
        let effective_instances = if self.runtime.worker {
            requested_instances.min(1)
        } else {
            requested_instances
        };

        let mut next_config = previous_config.clone();
        next_config.min_instances = effective_instances as u32;
        if next_config.max_instances < next_config.min_instances {
            next_config.max_instances = next_config.min_instances.max(4);
        }
        app.update_config(next_config.clone());

        let running_before = app
            .get_instances()
            .into_iter()
            .filter(|instance| {
                matches!(
                    instance.state(),
                    InstanceState::Starting | InstanceState::Ready | InstanceState::Healthy
                )
            })
            .count();

        if effective_instances as usize > running_before {
            let to_add = effective_instances as usize - running_before;
            let mut started_instances = Vec::with_capacity(to_add);

            for _ in 0..to_add {
                let instance = app.allocate_instance();
                match self
                    .app_manager
                    .spawner()
                    .spawn(&app, instance.clone())
                    .await
                {
                    Ok(()) => started_instances.push(instance),
                    Err(error) => {
                        for started in started_instances {
                            let _ = started.kill().await;
                            app.remove_instance(&started.id);
                        }
                        app.update_config(previous_config);
                        return Response::error(format!("Scale failed: {}", error));
                    }
                }
            }
        } else if (effective_instances as usize) < running_before {
            let mut candidates: Vec<_> = app
                .get_instances()
                .into_iter()
                .filter(|instance| {
                    matches!(
                        instance.state(),
                        InstanceState::Starting | InstanceState::Ready | InstanceState::Healthy
                    )
                })
                .collect();
            candidates.sort_by_key(|instance| std::cmp::Reverse(instance.idle_time()));

            let to_remove = running_before - effective_instances as usize;
            for instance in candidates.into_iter().take(to_remove) {
                if let Err(error) = self.drain_and_stop_instance(&app, &instance).await {
                    return Response::error(format!("Scale failed: {}", error));
                }
            }
        }

        update_instance_count_metric(app_name, &app);
        if app.get_instances().is_empty() && effective_instances == 0 {
            app.set_state(AppState::Idle);
            self.cold_start.reset(app_name);
        } else {
            app.set_state(AppState::Running);
        }

        self.persist_app_state(app_name).await;

        Response::ok(serde_json::json!({
            "status": "scaled",
            "app": app_name,
            "instances": effective_instances,
            "requested_instances": requested_instances,
            "worker_limited": self.runtime.worker && effective_instances != requested_instances
        }))
    }

    async fn drain_and_stop_instance(
        &self,
        app: &Arc<App>,
        instance: &Arc<crate::instances::Instance>,
    ) -> Result<(), String> {
        instance.set_state(InstanceState::Draining);
        let deadline = tokio::time::Instant::now() + RollingUpdateConfig::default().drain_timeout;
        while instance.in_flight() > 0 {
            if tokio::time::Instant::now() >= deadline {
                tracing::warn!(
                    app = %app.name(),
                    instance = %instance.id,
                    in_flight = instance.in_flight(),
                    "Scale drain timeout exceeded, forcing stop"
                );
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }

        instance
            .kill()
            .await
            .map_err(|error| format!("failed to stop instance '{}': {}", instance.id, error))?;
        app.remove_instance(&instance.id);
        metrics::remove_instance_metrics(&app.name(), &instance.id);
        Ok(())
    }

    async fn delete_app(&self, app_name: &str) -> Response {
        tracing::info!(app = app_name, "Deleting app");

        let mut existed = false;
        if self.app_manager.get_app(app_name).is_some() {
            existed = true;
            if let Err(e) = self.app_manager.stop_app(app_name).await {
                return Response::error(format!("Delete failed: {}", e));
            }
            self.app_manager.remove_app(app_name);
        }

        self.load_balancer.unregister_app(app_name);
        self.cold_start.reset(app_name);

        {
            let mut route_table = self.routes.write().await;
            route_table.remove_app_routes(app_name);
        }

        {
            let mut locks = self.deploy_locks.write().await;
            locks.remove(app_name);
        }

        let (name, environment) = requested_deployment_identity(app_name);
        if let Err(e) = self.state_store.delete_app(&name, &environment) {
            tracing::warn!(
                app = app_name,
                "Failed to delete persisted app state: {}",
                e
            );
        }

        Response::ok(serde_json::json!({
            "status": "deleted",
            "app": app_name,
            "existed": existed
        }))
    }

    async fn get_status(&self, app_name: &str) -> Response {
        let app = match self.app_manager.get_app(app_name) {
            Some(app) => app,
            None => return Response::error(format!("App not found: {}", app_name)),
        };

        let instances: Vec<InstanceStatus> =
            app.get_instances().iter().map(|i| i.status()).collect();
        let builds = collect_running_build_statuses(&app);

        let status = AppStatus {
            name: app.name(),
            version: app.version(),
            instances,
            builds,
            state: app.state(),
            last_error: app.last_error(),
        };

        Response::ok(status)
    }

    async fn list_apps(&self) -> Response {
        let apps: Vec<serde_json::Value> = self
            .app_manager
            .list_apps()
            .iter()
            .filter_map(|name| {
                self.app_manager.get_app(name).map(|app| {
                    serde_json::json!({
                        "name": app.name(),
                        "version": app.version(),
                        "state": app.state(),
                        "instances": app.get_instances().len()
                    })
                })
            })
            .collect();

        Response::ok(serde_json::json!({ "apps": apps }))
    }

    async fn list_releases(&self, app_name: &str) -> Response {
        let app = match self.app_manager.get_app(app_name) {
            Some(app) => app,
            None => return Response::error(format!("App not found: {}", app_name)),
        };
        let config = app.config.read().clone();

        let app_root = self.runtime.data_dir.join("apps").join(app_name);
        let releases_root = app_root.join("releases");
        let current_version = current_release_version(&app_root);

        let mut releases = Vec::new();
        let entries = match std::fs::read_dir(&releases_root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Response::ok(ListReleasesResponse {
                    app: app_name.to_string(),
                    releases,
                });
            }
            Err(error) => {
                return Response::error(format!(
                    "Failed to read releases directory '{}': {}",
                    releases_root.display(),
                    error
                ));
            }
        };

        for entry in entries.flatten() {
            let release_root = entry.path();
            if !release_root.is_dir() {
                continue;
            }

            let Some(version) = entry.file_name().to_str().map(|value| value.to_string()) else {
                continue;
            };

            let manifest_path = release_manifest_path(&release_root, &config.app_subdir);
            let (commit_message, git_dirty) = read_release_manifest_metadata(&manifest_path);
            releases.push(ReleaseInfo {
                current: current_version.as_deref() == Some(version.as_str()),
                deployed_at_unix_secs: directory_modified_unix_secs(&release_root),
                version,
                commit_message,
                git_dirty,
            });
        }

        releases.sort_by(|a, b| {
            b.deployed_at_unix_secs
                .cmp(&a.deployed_at_unix_secs)
                .then_with(|| b.version.cmp(&a.version))
        });

        Response::ok(ListReleasesResponse {
            app: app_name.to_string(),
            releases,
        })
    }

    async fn list_routes(&self) -> Response {
        let route_table = self.routes.read().await;
        let routes: Vec<serde_json::Value> = self
            .app_manager
            .list_apps()
            .iter()
            .map(|app| {
                let patterns = route_table.routes_for_app(app);
                serde_json::json!({ "app": app, "routes": patterns })
            })
            .collect();
        Response::ok(serde_json::json!({ "routes": routes }))
    }

    async fn rollback_app(&self, app_name: &str, version: &str) -> Response {
        let app = match self.app_manager.get_app(app_name) {
            Some(app) => app,
            None => return Response::error(format!("App not found: {}", app_name)),
        };
        let config = app.config.read().clone();

        let app_root = self.runtime.data_dir.join("apps").join(app_name);
        let target_path = rollback_release_path(&app_root, version, &config.app_subdir);

        if !target_path.is_dir() {
            return Response::error(format!(
                "Release '{}' not found for app '{}'",
                version, app_name
            ));
        }

        let routes = {
            let route_table = self.routes.read().await;
            route_table.routes_for_app(app_name)
        };
        if routes.is_empty() {
            return Response::error(format!(
                "Cannot rollback '{}': no routes are configured",
                app_name
            ));
        }

        self.deploy_app(
            app_name,
            version,
            &target_path.to_string_lossy(),
            routes,
            None, // keep existing secrets on rollback
        )
        .await
    }

    async fn update_secrets(
        &self,
        app_name: &str,
        new_secrets: HashMap<String, String>,
    ) -> Response {
        tracing::info!(app = app_name, "Updating secrets");

        if let Err(e) = self.state_store.set_secrets(app_name, &new_secrets) {
            return Response::error(format!("Failed to store secrets: {}", e));
        }

        // Update app config and auto-restart running instances to apply new secrets
        if let Some(app) = self.app_manager.get_app(app_name) {
            let mut config = app.config.read().clone();
            config.secrets = new_secrets;
            app.update_config(config.clone());
            self.persist_app_state(app_name).await;

            if !app.get_instances().is_empty() {
                let previous_state = app.state();
                app.set_state(AppState::Deploying);
                let rolling_config = RollingUpdateConfig::default();
                let updater =
                    RollingUpdater::new(self.app_manager.spawner().clone(), rolling_config);
                let target =
                    target_new_instances_for_build(config.min_instances, app.get_instances().len());
                match updater.update(&app, config, target).await {
                    Ok(result) if result.success => {
                        app.set_state(AppState::Running);
                        return Response::ok(serde_json::json!({
                            "status": "updated",
                            "app": app_name,
                            "restarted": true
                        }));
                    }
                    Ok(result) => {
                        app.set_state(previous_state);
                        return Response::error(format!(
                            "Rolling restart failed: {:?}",
                            result.error
                        ));
                    }
                    Err(e) => {
                        app.set_state(AppState::Error);
                        return Response::error(format!("Rolling restart failed: {}", e));
                    }
                }
            }
        }

        Response::ok(serde_json::json!({
            "status": "updated",
            "app": app_name,
            "restarted": false
        }))
    }

    /// Request a certificate for a domain via ACME
    pub async fn request_certificate(&self, domain: &str) -> Response {
        let acme_guard = self.acme_client.read().await;
        let acme = match acme_guard.as_ref() {
            Some(acme) => acme,
            None => return Response::error("ACME is disabled".to_string()),
        };

        match acme.request_certificate(domain).await {
            Ok(cert) => Response::ok(serde_json::json!({
                "status": "issued",
                "domain": domain,
                "expires_in_days": cert.days_until_expiry(),
                "cert_path": cert.cert_path.to_string_lossy(),
            })),
            Err(e) => Response::error(format!("Certificate request failed: {}", e)),
        }
    }

    async fn start_on_demand_warm_instance(&self, app: &Arc<App>) -> Result<(), String> {
        let instance = app.allocate_instance();
        let spawner = self.app_manager.spawner();

        match spawner.spawn(app, instance.clone()).await {
            Ok(()) => Ok(()),
            Err(e) => {
                app.remove_instance(&instance.id);
                Err(format!("Warm instance startup failed: {}", e))
            }
        }
    }

    async fn ensure_route_certificate(&self, app_name: &str, domain: &str) -> Option<CertInfo> {
        if let Some(existing) = self.cert_manager.get_cert_for_host(domain) {
            tracing::debug!(domain = %domain, "Certificate already exists");
            return Some(existing);
        }

        if should_use_self_signed_route_cert(domain) {
            match self.cert_manager.get_or_create_self_signed_cert(domain) {
                Ok(cert) => {
                    tracing::info!(
                        domain = %domain,
                        app = app_name,
                        cert_path = %cert.cert_path.display(),
                        "Generated self-signed certificate for private route domain"
                    );
                    return Some(cert);
                }
                Err(e) => {
                    tracing::warn!(
                        domain = %domain,
                        app = app_name,
                        error = %e,
                        "Failed to generate self-signed certificate for private route domain"
                    );
                    return None;
                }
            }
        }

        let acme_guard = self.acme_client.read().await;
        let Some(acme) = acme_guard.as_ref() else {
            return None;
        };

        tracing::info!(domain = %domain, app = app_name, "Requesting certificate for route");
        match acme.request_certificate(domain).await {
            Ok(cert) => {
                tracing::info!(
                    domain = %domain,
                    expires_in_days = cert.days_until_expiry(),
                    "Certificate issued successfully"
                );
                Some(cert)
            }
            Err(e) => {
                tracing::warn!(
                    domain = %domain,
                    error = %e,
                    "Failed to request certificate (HTTPS may not work for this domain)"
                );
                None
            }
        }
    }
}

fn collect_running_build_statuses(app: &App) -> Vec<BuildStatus> {
    let mut instances_by_build: HashMap<String, Vec<InstanceStatus>> = HashMap::new();
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

#[derive(Debug, serde::Deserialize)]
struct ReleaseManifestMetadata {
    #[serde(default)]
    commit_message: Option<String>,
    #[serde(default)]
    git_dirty: Option<bool>,
}

fn current_release_version(app_root: &Path) -> Option<String> {
    let current_link = app_root.join("current");
    let target = std::fs::read_link(current_link).ok()?;
    target.file_name()?.to_str().map(|value| value.to_string())
}

fn app_release_root(data_dir: &Path, app_name: &str, version: &str) -> PathBuf {
    data_dir
        .join("apps")
        .join(app_name)
        .join("releases")
        .join(version)
}

fn release_app_path(data_dir: &Path, config: &AppConfig) -> PathBuf {
    rollback_release_path(
        &data_dir.join("apps").join(config.deployment_id()),
        &config.version,
        &config.app_subdir,
    )
}

fn app_subdir_from_release_path(
    data_dir: &Path,
    app_name: &str,
    version: &str,
    release_path: &Path,
) -> Result<String, String> {
    let release_root = app_release_root(data_dir, app_name, version);
    let release_root = std::fs::canonicalize(&release_root).unwrap_or(release_root);
    let relative = release_path.strip_prefix(&release_root).map_err(|_| {
        format!(
            "Invalid release path: '{}' must stay under '{}'",
            release_path.display(),
            release_root.display()
        )
    })?;
    Ok(relative.to_string_lossy().to_string())
}

fn apply_release_runtime_to_config(
    config: &mut AppConfig,
    release_path: PathBuf,
    app_socket_dir: PathBuf,
) -> Result<(), String> {
    config.path = release_path;
    let manifest = load_release_manifest(&config.path)?;
    config.command = command_from_manifest(&manifest, &config.path)?;
    config.env_vars = manifest.env_vars;
    config.idle_timeout = Duration::from_secs(u64::from(manifest.idle_timeout));
    config.app_socket_dir = app_socket_dir;
    Ok(())
}

fn rollback_release_path(app_root: &Path, version: &str, app_subdir: &str) -> PathBuf {
    let release_root = app_root.join("releases").join(version);
    if app_subdir.is_empty() {
        release_root
    } else {
        release_root.join(app_subdir)
    }
}

fn release_manifest_path(release_root: &Path, app_subdir: &str) -> PathBuf {
    let app_dir = if app_subdir.is_empty() {
        release_root.to_path_buf()
    } else {
        release_root.join(app_subdir)
    };
    app_dir.join("app.json")
}

fn read_release_manifest_metadata(path: &Path) -> (Option<String>, Option<bool>) {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return (None, None);
    };
    let Ok(parsed) = serde_json::from_str::<ReleaseManifestMetadata>(&raw) else {
        return (None, None);
    };
    (parsed.commit_message, parsed.git_dirty)
}

fn directory_modified_unix_secs(path: &Path) -> Option<i64> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    let unix = modified.duration_since(std::time::UNIX_EPOCH).ok()?;
    i64::try_from(unix.as_secs()).ok()
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

/// Resolve the tako-app user for running deploy commands with dropped privileges.
#[cfg(unix)]
fn resolve_app_user_for_install() -> Option<(u32, u32)> {
    use std::ffi::CString;
    let name = CString::new("tako-app").ok()?;
    let pw = unsafe { libc::getpwnam(name.as_ptr()) };
    if pw.is_null() {
        return None;
    }
    Some(unsafe { ((*pw).pw_uid, (*pw).pw_gid) })
}

/// Drop privileges on a command to the tako-app user if running as root.
/// All deploy-time commands that execute user-controlled code (install scripts,
/// bun install, proto install) must use this to avoid running as root.
#[cfg(unix)]
fn drop_privileges_if_root(cmd: &mut TokioCommand) {
    if unsafe { libc::geteuid() } == 0 {
        if let Some((uid, gid)) = resolve_app_user_for_install() {
            cmd.uid(uid);
            cmd.gid(gid);
        }
    }
}

fn resolve_release_runtime(release_dir: &Path) -> Result<String, String> {
    runtime_from_release_dir(release_dir)
}

async fn run_release_install_command(
    release_dir: &Path,
    command: &str,
    env: &HashMap<String, String>,
) -> Result<(), String> {
    let mut cmd = TokioCommand::new("sh");
    cmd.args(["-lc", command])
        .current_dir(release_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .envs(
            env.iter()
                .map(|(key, value)| (key.as_str(), value.as_str())),
        );

    #[cfg(unix)]
    drop_privileges_if_root(&mut cmd);

    let output = cmd.output().await.map_err(|e| {
        format!(
            "Failed to run release install command '{}' in {}: {}",
            command,
            release_dir.display(),
            e
        )
    })?;

    if output.status.success() {
        return Ok(());
    }
    Err(format_process_failure(
        "Release dependency install failed",
        output.status,
        &output.stdout,
        &output.stderr,
    ))
}

fn bun_install_args_for_release(release_dir: &Path) -> Vec<String> {
    let mut args = vec!["install".to_string(), "--production".to_string()];
    if release_dir.join("bun.lockb").is_file() || release_dir.join("bun.lock").is_file() {
        args.push("--frozen-lockfile".to_string());
    }
    args
}

async fn prepare_release_runtime(
    release_dir: &Path,
    env: &HashMap<String, String>,
) -> Result<(), String> {
    let manifest = load_release_manifest(release_dir)?;
    let runtime = &manifest.runtime;
    if runtime.trim().is_empty() {
        return Err(format!(
            "deploy manifest {} has empty runtime field",
            release_dir.join("app.json").display()
        ));
    }

    // Install the pinned runtime version and cache the absolute binary path.
    if let Some(bin) =
        version_manager::install_and_resolve(runtime, manifest.runtime_version.as_deref()).await
    {
        if let Err(e) = write_runtime_bin(release_dir, &bin) {
            tracing::warn!(error = %e, "Failed to write runtime_bin to manifest (non-fatal)");
        }
    }

    if let Some(install_cmd) = manifest
        .install
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        run_release_install_command(release_dir, install_cmd, env).await?;
    } else if runtime == "bun" {
        install_bun_dependencies_for_release(release_dir, env).await?;
    }

    if let Some(rel_path) = crate::app_command::entrypoint_relative_path(runtime) {
        let entrypoint_path = release_dir.join(rel_path);
        if !entrypoint_path.is_file() {
            return Err(format!(
                "Dependency install completed but '{}' is missing. Ensure package 'tako.sh' is installed.",
                entrypoint_path.display()
            ));
        }
    }
    Ok(())
}

async fn install_bun_dependencies_for_release(
    release_dir: &Path,
    env: &HashMap<String, String>,
) -> Result<(), String> {
    let args = bun_install_args_for_release(release_dir);
    let mut cmd = TokioCommand::new("bun");
    cmd.args(args.iter().map(String::as_str))
        .current_dir(release_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .envs(
            env.iter()
                .map(|(key, value)| (key.as_str(), value.as_str())),
        );

    #[cfg(unix)]
    drop_privileges_if_root(&mut cmd);

    let output = cmd.output().await.map_err(|e| {
        format!(
            "Failed to run 'bun {}' in {}: {}",
            args.join(" "),
            release_dir.display(),
            e
        )
    })?;

    if !output.status.success() {
        return Err(format_process_failure(
            "Bun dependency install failed",
            output.status,
            &output.stdout,
            &output.stderr,
        ));
    }

    Ok(())
}

fn format_process_failure(
    context: &str,
    status: ExitStatus,
    stdout: &[u8],
    stderr: &[u8],
) -> String {
    let status_text = match status.code() {
        Some(code) => format!("exit code {code}"),
        None => "terminated by signal".to_string(),
    };

    let stderr_text = String::from_utf8_lossy(stderr).trim().to_string();
    let stdout_text = String::from_utf8_lossy(stdout).trim().to_string();
    let detail = if !stderr_text.is_empty() {
        stderr_text
    } else {
        stdout_text
    };

    if detail.is_empty() {
        return format!("{context} ({status_text})");
    }

    let preview: String = detail.chars().take(400).collect();
    if detail.chars().count() > 400 {
        format!("{context} ({status_text}): {preview}...")
    } else {
        format!("{context} ({status_text}): {preview}")
    }
}

pub(crate) fn is_private_local_hostname(domain: &str) -> bool {
    let host = domain
        .split(':')
        .next()
        .unwrap_or(domain)
        .trim()
        .trim_end_matches('.')
        .to_ascii_lowercase();

    if host.is_empty() {
        return false;
    }
    if host == "localhost" || host.ends_with(".localhost") {
        return true;
    }
    if !host.contains('.') {
        return true;
    }

    host.ends_with(".local")
        || host.ends_with(".test")
        || host.ends_with(".invalid")
        || host.ends_with(".example")
        || host.ends_with(".home.arpa")
}

fn should_use_self_signed_route_cert(domain: &str) -> bool {
    is_private_local_hostname(domain)
}

fn validate_app_name_segment(app_name: &str) -> Result<(), String> {
    if app_name.is_empty() {
        return Err("Invalid app name: must not be empty".to_string());
    }
    if app_name.len() > 63 {
        return Err("Invalid app name: must be 63 characters or fewer".to_string());
    }
    if !app_name
        .chars()
        .next()
        .map(|c| c.is_ascii_lowercase())
        .unwrap_or(false)
    {
        return Err("Invalid app name: must start with a lowercase letter".to_string());
    }
    if app_name.ends_with('-') {
        return Err("Invalid app name: must not end with '-'".to_string());
    }
    if !app_name
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        return Err(
            "Invalid app name: only lowercase letters, digits, and '-' are allowed".to_string(),
        );
    }
    Ok(())
}

fn validate_app_name(app_name: &str) -> Result<(), String> {
    if let Some((app, env)) = tako_core::split_deployment_app_id(app_name) {
        validate_app_name_segment(app)?;
        validate_app_name_segment(env)?;
        return Ok(());
    }

    validate_app_name_segment(app_name)
}

fn requested_deployment_identity(app_id: &str) -> (String, String) {
    if let Some((name, environment)) = tako_core::split_deployment_app_id(app_id) {
        return (name.to_string(), environment.to_string());
    }
    (app_id.to_string(), "production".to_string())
}

fn validate_release_version(version: &str) -> Result<(), String> {
    if version.is_empty() {
        return Err("Invalid release version: must not be empty".to_string());
    }
    if version.len() > 128 {
        return Err("Invalid release version: must be 128 characters or fewer".to_string());
    }
    if version == "." || version == ".." {
        return Err("Invalid release version: '.' and '..' are not allowed".to_string());
    }
    if version.contains('/') || version.contains('\\') {
        return Err("Invalid release version: path separators are not allowed".to_string());
    }
    if !version
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
    {
        return Err(
            "Invalid release version: only letters, digits, '.', '_' and '-' are allowed"
                .to_string(),
        );
    }
    Ok(())
}

fn validate_release_path_for_app(
    data_dir: &Path,
    app_name: &str,
    path: &str,
) -> Result<PathBuf, String> {
    let release_path = std::fs::canonicalize(Path::new(path))
        .map_err(|e| format!("Invalid release path: {} ({})", path, e))?;
    if !release_path.is_dir() {
        return Err(format!(
            "Invalid release path: '{}' must be an existing directory",
            release_path.display()
        ));
    }

    let expected_root = data_dir.join("apps").join(app_name).join("releases");
    let expected_root = std::fs::canonicalize(&expected_root).unwrap_or(expected_root);
    if !release_path.starts_with(&expected_root) {
        return Err(format!(
            "Invalid release path: '{}' must stay under '{}'",
            release_path.display(),
            expected_root.display()
        ));
    }

    Ok(release_path)
}

fn validate_deploy_routes(routes: &[String]) -> Result<(), String> {
    if routes.is_empty() {
        return Err("Deploy rejected: app must define at least one route".to_string());
    }
    if routes.iter().any(|r| r.trim().is_empty()) {
        return Err("Deploy rejected: routes must be non-empty values".to_string());
    }
    Ok(())
}

/// Background task for automatic certificate renewal
async fn certificate_renewal_task(acme_client: Arc<AcmeClient>, interval: Duration) {
    tracing::info!(
        interval_hours = interval.as_secs() / 3600,
        "Starting certificate renewal task"
    );

    loop {
        tokio::time::sleep(interval).await;

        tracing::info!("Checking for certificates needing renewal...");

        let results = acme_client.check_renewals().await;

        for result in results {
            match result {
                Ok(cert) => {
                    tracing::info!(
                        domain = %cert.domain,
                        expires_in_days = cert.days_until_expiry(),
                        "Certificate renewed successfully"
                    );
                }
                Err(e) => {
                    tracing::error!("Certificate renewal failed: {}", e);
                }
            }
        }
    }
}

fn install_rustls_crypto_provider() {
    if rustls::crypto::CryptoProvider::get_default().is_some() {
        return;
    }

    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
}

fn should_signal_parent_on_ready() -> bool {
    matches!(
        std::env::var(SIGNAL_PARENT_ON_READY_ENV).as_deref(),
        Ok("1")
    )
}

/// Server-level configuration stored in `{data_dir}/config.json`.
#[derive(Debug, Default, serde::Deserialize, serde::Serialize)]
struct ServerConfigFile {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    server_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    dns: Option<ServerConfigDns>,
}

#[derive(Debug, serde::Deserialize, serde::Serialize)]
struct ServerConfigDns {
    provider: String,
}

/// Read server configuration from `{data_dir}/config.json`.
fn read_server_config(data_dir: &Path) -> ServerConfigFile {
    let config_path = data_dir.join("config.json");
    if let Ok(contents) = std::fs::read_to_string(&config_path) {
        if let Ok(config) = serde_json::from_str::<ServerConfigFile>(&contents) {
            return config;
        }
    }
    ServerConfigFile::default()
}

/// Notify systemd that the server is ready (READY=1) and claim the main PID.
/// During a zero-downtime reload handoff, optionally sends SIGUSR1 to the parent process so the
/// previous server process starts draining.
fn sd_notify_ready() {
    #[cfg(unix)]
    {
        // sd_notify via NOTIFY_SOCKET
        if let Ok(socket_path) = std::env::var("NOTIFY_SOCKET") {
            use std::os::unix::net::UnixDatagram;
            if let Ok(sock) = UnixDatagram::unbound() {
                let pid = std::process::id();
                let msg = format!("READY=1\nMAINPID={pid}\n");
                // Abstract sockets have a leading '@' which maps to '\0'
                let path = if let Some(stripped) = socket_path.strip_prefix('@') {
                    format!("\0{}", stripped)
                } else {
                    socket_path
                };
                let _ = sock.send_to(msg.as_bytes(), path);
            }
        }

        // Signal parent to start draining (zero-downtime reload handoff)
        if !should_signal_parent_on_ready() {
            return;
        }
        let ppid = unsafe { libc::getppid() };
        if ppid > 1 {
            unsafe { libc::kill(ppid, libc::SIGUSR1) };
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

    let worker = args.worker;

    tracing::info!("Tako Server v{}", env!("CARGO_PKG_VERSION"));
    if worker {
        tracing::info!("Mode: worker");
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
    // In worker mode, skip binding — let the primary own the socket.
    let (_socket_server, socket_listener) = if worker {
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

    // Create ACME client if enabled (skip in worker mode)
    let acme_client = if args.no_acme || worker {
        if worker {
            tracing::info!("ACME disabled (worker mode)");
        } else {
            tracing::info!("ACME disabled, using manual certificate management");
        }
        None
    } else {
        let acme_config = AcmeConfig {
            staging: args.acme_staging,
            email: args.acme_email.clone(),
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
        acme_email: args.acme_email.clone(),
        renewal_interval_hours: args.renewal_interval_hours,
        dns_provider: config_dns_provider.clone(),
        worker,
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
    // In worker mode, no socket is bound — the primary owns it.
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

    // In worker mode, monitor the primary's management socket.
    // - If primary goes down: promote to full mode (bind socket, init ACME).
    // - If primary comes back (after we promoted): gracefully shut down.
    if worker {
        let socket_path = socket.clone();
        let promote_state = state.clone();
        let promote_cert_manager = cert_manager.clone();
        let promote_args_acme_staging = args.acme_staging;
        let promote_args_acme_email = args.acme_email.clone();
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
                            tracing::info!("Primary server is back — worker shutting down");
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
                            tracing::info!("Promoting worker to full mode");

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
                                    email: promote_args_acme_email.clone(),
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
                                "Promotion complete — worker now running as full server"
                            );
                        }
                    }
                }
            }
        });
    }

    // Best-effort cleanup for stale app unix socket files.
    // This matters if an app crashes and leaves behind a stale socket path.
    // During rolling updates multiple instances can be alive concurrently; we only remove sockets
    // whose PID no longer exists.
    {
        let dirs = app_socket_cleanup_dirs(&socket);
        for d in &dirs {
            app_socket_cleanup::cleanup_stale_app_sockets(d);
        }

        rt.spawn(async move {
            let mut ticker = tokio::time::interval(Duration::from_secs(30));
            loop {
                ticker.tick().await;
                for d in &dirs {
                    app_socket_cleanup::cleanup_stale_app_sockets(d);
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

async fn handle_instance_event(state: &ServerState, event: InstanceEvent) {
    match event {
        InstanceEvent::Started { app, instance_id } => {
            tracing::debug!(app = %app, instance = %instance_id, "Instance started");
        }
        InstanceEvent::Ready { app, instance_id } => {
            tracing::info!(app = %app, instance = %instance_id, "Instance ready");
            state.cold_start.mark_ready(&app);

            if let Some(app_ref) = state.app_manager.get_app(&app) {
                app_ref.clear_last_error();
            }
        }
        InstanceEvent::Unhealthy { app, instance_id } => {
            tracing::warn!(app = %app, instance = %instance_id, "Instance unhealthy");
            // Replace unhealthy instance after a grace period
            replace_instance_if_needed(state, &app, &instance_id, "unhealthy").await;
        }
        InstanceEvent::Stopped { app, instance_id } => {
            tracing::info!(app = %app, instance = %instance_id, "Instance stopped");
        }
    }
}

async fn handle_health_event(state: &ServerState, event: crate::instances::HealthEvent) {
    use crate::instances::HealthEvent;

    match event {
        HealthEvent::Healthy { app, instance_id } => {
            tracing::info!(app = %app, instance = %instance_id, "Instance is healthy");
            metrics::set_instance_health(&app, &instance_id, true);
            state.cold_start.mark_ready(&app);

            if let Some(app_ref) = state.app_manager.get_app(&app) {
                app_ref.clear_last_error();
                update_instance_count_metric(&app, &app_ref);
            }
        }
        HealthEvent::Unhealthy { app, instance_id } => {
            tracing::warn!(app = %app, instance = %instance_id, "Instance became unhealthy");
            metrics::set_instance_health(&app, &instance_id, false);
            // Don't immediately replace - wait for Dead event or recovery
        }
        HealthEvent::Dead { app, instance_id } => {
            tracing::error!(app = %app, instance = %instance_id, "Instance is dead (no heartbeat)");
            metrics::set_instance_health(&app, &instance_id, false);
            metrics::remove_instance_metrics(&app, &instance_id);
            state.cold_start.mark_failed(&app);
            if let Some(app_ref) = state.app_manager.get_app(&app) {
                app_ref.set_last_error("Instance marked dead");
                update_instance_count_metric(&app, &app_ref);
            }
            // Replace dead instance immediately
            replace_instance_if_needed(state, &app, &instance_id, "dead").await;
        }
        HealthEvent::Recovered { app, instance_id } => {
            tracing::info!(app = %app, instance = %instance_id, "Instance recovered from unhealthy");
            metrics::set_instance_health(&app, &instance_id, true);
        }
    }
}

fn update_instance_count_metric(app_name: &str, app: &App) {
    let count = app
        .get_instances()
        .iter()
        .filter(|i| {
            matches!(
                i.state(),
                InstanceState::Starting | InstanceState::Ready | InstanceState::Healthy
            )
        })
        .count();
    metrics::set_instances_running(app_name, count as i64);
}

async fn handle_idle_event(state: &ServerState, event: IdleEvent) {
    match event {
        IdleEvent::InstanceIdle { app, instance_id } => {
            if let Some(app_ref) = state.app_manager.get_app(&app)
                && let Some(instance) = app_ref.get_instance(&instance_id)
            {
                if let Err(e) = instance.kill().await {
                    tracing::warn!(app = %app, instance = %instance_id, "Failed to kill idle instance: {}", e);
                }
                app_ref.remove_instance(&instance_id);
                metrics::remove_instance_metrics(&app, &instance_id);

                let running_count = app_ref
                    .get_instances()
                    .iter()
                    .filter(|i| {
                        matches!(
                            i.state(),
                            InstanceState::Starting | InstanceState::Ready | InstanceState::Healthy
                        )
                    })
                    .count();
                metrics::set_instances_running(&app, running_count as i64);
                let min_instances = app_ref.config.read().min_instances;

                // When the final running instance is stopped, transition immediately so the
                // next request can trigger a fresh cold start without waiting for another
                // idle-monitor cycle.
                if running_count == 0 && min_instances == 0 {
                    app_ref.set_state(AppState::Idle);
                    state.cold_start.reset(&app);
                }
            }
        }
        IdleEvent::AppIdle { app } => {
            if let Some(app_ref) = state.app_manager.get_app(&app) {
                app_ref.set_state(AppState::Idle);
            }
            state.cold_start.reset(&app);
        }
    }
}

/// Replace an unhealthy or dead instance with a new one
async fn replace_instance_if_needed(
    state: &ServerState,
    app_name: &str,
    instance_id: &str,
    reason: &str,
) {
    let app = match state.app_manager.get_app(app_name) {
        Some(app) => app,
        None => {
            tracing::warn!(app = %app_name, "Cannot replace instance: app not found");
            return;
        }
    };

    // Find the instance
    let instance = match app.get_instance(instance_id) {
        Some(inst) => inst,
        None => {
            tracing::debug!(app = %app_name, instance = %instance_id, "Instance already removed");
            return;
        }
    };

    // Check if we should replace this instance.
    // Instance counts are evaluated per build/version, not across all builds.
    let failed_build = instance.build_version().to_string();
    let current_version = app.version();
    let current_count = app
        .get_instances()
        .into_iter()
        .filter(|i| i.build_version() == failed_build.as_str())
        .count() as u32;
    let min_instances = app.config.read().min_instances;
    let min_for_build = if failed_build == current_version {
        min_instances
    } else {
        0
    };

    // Only replace if we're at or below the per-build minimum
    // (don't want to scale up just because one instance is unhealthy)
    if current_count > min_for_build {
        tracing::info!(
            app = %app_name,
            instance = %instance_id,
            reason = reason,
            build = %failed_build,
            current = current_count,
            min = min_for_build,
            "Not replacing {} instance: have more than minimum instances",
            reason
        );
        // Just remove the bad instance
        if let Err(e) = instance.kill().await {
            tracing::error!(app = %app_name, instance = %instance_id, "Failed to kill instance: {}", e);
        }
        app.remove_instance(instance_id);
        return;
    }

    tracing::info!(
        app = %app_name,
        instance = %instance_id,
        reason = reason,
        "Replacing {} instance with a new one",
        reason
    );

    // Kill the old instance
    if let Err(e) = instance.kill().await {
        tracing::error!(app = %app_name, instance = %instance_id, "Failed to kill old instance: {}", e);
    }
    app.remove_instance(instance_id);

    // Allocate and spawn a new instance
    let new_instance = app.allocate_instance();
    let spawner = state.app_manager.spawner();

    match spawner.spawn(&app, new_instance.clone()).await {
        Ok(()) => {
            tracing::info!(
                app = %app_name,
                old_instance = %instance_id,
                new_instance = %new_instance.id,
                "Successfully spawned replacement instance"
            );
        }
        Err(e) => {
            tracing::error!(
                app = %app_name,
                instance = %new_instance.id,
                "Failed to spawn replacement instance: {}",
                e
            );
            // Clean up the failed instance
            app.remove_instance(&new_instance.id);
        }
    }
}

/// Result of probing the primary management socket.
enum PrimaryStatus {
    /// Primary is alive and it's a different process.
    Alive,
    /// Socket connects but points to our own PID (we promoted earlier).
    IsUs,
    /// Socket is unreachable or not responding.
    Down,
}

/// Probe the primary management socket. Sends a `server_info` command and
/// checks the PID in the response to distinguish "primary" from "us".
async fn probe_primary_socket(socket_path: &str, our_pid: u32) -> PrimaryStatus {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let mut stream = match tokio::net::UnixStream::connect(socket_path).await {
        Ok(s) => s,
        Err(_) => return PrimaryStatus::Down,
    };

    let cmd = "{\"command\":\"server_info\"}\n";
    if stream.write_all(cmd.as_bytes()).await.is_err() {
        return PrimaryStatus::Down;
    }
    let _ = stream.shutdown().await;

    let mut buf = Vec::new();
    let _ = stream.read_to_end(&mut buf).await;
    let response = String::from_utf8_lossy(&buf);

    if !response.contains("\"status\":\"ok\"") {
        return PrimaryStatus::Down;
    }

    // Parse PID from response to distinguish primary from ourselves
    if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&response) {
        if let Some(pid) = parsed
            .get("data")
            .and_then(|d| d.get("pid"))
            .and_then(|p| p.as_u64())
        {
            if pid as u32 == our_pid {
                return PrimaryStatus::IsUs;
            }
        }
    }

    PrimaryStatus::Alive
}

#[cfg(test)]
mod tests {
    use super::{
        SIGNAL_PARENT_ON_READY_ENV, ServerRuntimeConfig, ServerState, bun_install_args_for_release,
        extract_zstd_archive, handle_idle_event, install_rustls_crypto_provider,
        prepare_release_runtime, read_server_config, resolve_release_runtime,
        run_extract_archive_mode, should_signal_parent_on_ready, should_use_self_signed_route_cert,
        validate_app_name, validate_deploy_routes,
    };
    use crate::instances::AppConfig;
    use crate::socket::{AppState, Command, InstanceState, Response};
    use crate::tls::{CertManager, CertManagerConfig};
    use clap::Parser;
    use serde_json::Value;
    use std::collections::HashMap;
    use std::io::Cursor;
    use std::path::Path;
    use std::process::{Command as StdCommand, Stdio};
    use std::sync::{Arc, Mutex, OnceLock};
    use std::time::Duration;
    use tako_core::UpgradeMode;
    use tempfile::TempDir;

    fn empty_challenge_tokens() -> super::ChallengeTokens {
        Arc::new(parking_lot::RwLock::new(HashMap::new()))
    }

    fn write_release_manifest(
        release_dir: &Path,
        runtime: &str,
        main: &str,
        start: &[&str],
        install: Option<&str>,
        idle_timeout: u32,
    ) {
        let mut manifest = serde_json::json!({
            "runtime": runtime,
            "main": main,
            "idle_timeout": idle_timeout,
        });
        if !start.is_empty() {
            manifest["start"] =
                serde_json::Value::Array(start.iter().map(|value| (*value).into()).collect());
        }
        if let Some(install) = install {
            manifest["install"] = install.into();
        }
        std::fs::write(
            release_dir.join("app.json"),
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn default_server_log_filter_is_warn() {
        assert_eq!(super::DEFAULT_SERVER_LOG_FILTER, "warn");
    }

    #[test]
    fn extract_zstd_archive_unpacks_files() {
        let temp = TempDir::new().unwrap();
        let archive_path = temp.path().join("payload.tar.zst");
        let dest = temp.path().join("dest");

        let file = std::fs::File::create(&archive_path).unwrap();
        let encoder = zstd::stream::write::Encoder::new(file, 3).unwrap();
        let mut archive = tar::Builder::new(encoder);
        let mut header = tar::Header::new_gnu();
        let payload = b"hello";
        header.set_size(payload.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        archive
            .append_data(&mut header, "app/index.txt", &mut Cursor::new(payload))
            .unwrap();
        let encoder = archive.into_inner().unwrap();
        encoder.finish().unwrap();

        extract_zstd_archive(&archive_path, &dest).unwrap();
        assert_eq!(
            std::fs::read_to_string(dest.join("app/index.txt")).unwrap(),
            "hello"
        );
    }

    #[test]
    fn run_extract_archive_mode_requires_destination_flag() {
        let args = super::Args::try_parse_from([
            "tako-server",
            "--extract-zstd-archive",
            "/tmp/payload.tar.zst",
        ])
        .unwrap();
        let err = run_extract_archive_mode(&args).unwrap_err();
        assert!(err.contains("--extract-dest"));
    }

    #[test]
    fn install_rustls_crypto_provider_is_idempotent() {
        install_rustls_crypto_provider();
        assert!(rustls::crypto::CryptoProvider::get_default().is_some());

        install_rustls_crypto_provider();
        assert!(rustls::crypto::CryptoProvider::get_default().is_some());
    }

    fn signal_parent_env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    #[test]
    fn should_signal_parent_on_ready_defaults_to_false() {
        let _guard = signal_parent_env_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        unsafe {
            std::env::remove_var(SIGNAL_PARENT_ON_READY_ENV);
        }
        assert!(!should_signal_parent_on_ready());
    }

    #[test]
    fn should_signal_parent_on_ready_reads_env_toggle() {
        let _guard = signal_parent_env_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        unsafe {
            std::env::set_var(SIGNAL_PARENT_ON_READY_ENV, "1");
        }
        assert!(should_signal_parent_on_ready());

        unsafe {
            std::env::set_var(SIGNAL_PARENT_ON_READY_ENV, "0");
        }
        assert!(!should_signal_parent_on_ready());

        unsafe {
            std::env::remove_var(SIGNAL_PARENT_ON_READY_ENV);
        }
    }

    #[test]
    fn validate_deploy_routes_rejects_empty_routes() {
        let err = validate_deploy_routes(&[]).unwrap_err();
        assert!(err.contains("at least one route"));
    }

    #[test]
    fn validate_deploy_routes_rejects_empty_route_entry() {
        let err = validate_deploy_routes(&["".to_string()]).unwrap_err();
        assert!(err.contains("non-empty"));
    }

    #[test]
    fn validate_app_name_accepts_app_env_identifier() {
        assert!(validate_app_name("my-app/staging").is_ok());
    }

    #[tokio::test]
    async fn deploy_rejects_invalid_app_name() {
        let temp = TempDir::new().unwrap();
        let cert_manager = Arc::new(CertManager::new(CertManagerConfig {
            cert_dir: temp.path().join("certs"),
            ..Default::default()
        }));
        let state = ServerState::new(
            temp.path().to_path_buf(),
            cert_manager,
            None,
            empty_challenge_tokens(),
        )
        .unwrap();

        let response = state
            .handle_command(Command::Deploy {
                app: "../escape".to_string(),
                version: "v1".to_string(),
                path: temp.path().to_string_lossy().to_string(),
                routes: vec!["api.example.com".to_string()],
                secrets: Some(HashMap::new()),
            })
            .await;

        let Response::Error { message } = response else {
            panic!("expected invalid app name to be rejected");
        };
        assert!(message.contains("Invalid app name"), "got: {message}");
    }

    #[tokio::test]
    async fn deploy_rejects_release_path_outside_managed_root() {
        let temp = TempDir::new().unwrap();
        let cert_manager = Arc::new(CertManager::new(CertManagerConfig {
            cert_dir: temp.path().join("certs"),
            ..Default::default()
        }));
        let state = ServerState::new(
            temp.path().to_path_buf(),
            cert_manager,
            None,
            empty_challenge_tokens(),
        )
        .unwrap();

        let outside_release = temp.path().join("outside-release");
        std::fs::create_dir_all(&outside_release).unwrap();

        let response = state
            .handle_command(Command::Deploy {
                app: "demo-app".to_string(),
                version: "v1".to_string(),
                path: outside_release.to_string_lossy().to_string(),
                routes: vec!["api.example.com".to_string()],
                secrets: Some(HashMap::new()),
            })
            .await;

        let Response::Error { message } = response else {
            panic!("expected out-of-root deploy path to be rejected");
        };
        assert!(
            message.contains("Invalid release path"),
            "expected path validation error, got: {message}"
        );
    }

    #[tokio::test]
    async fn deploy_rejects_invalid_release_version() {
        let temp = TempDir::new().unwrap();
        let cert_manager = Arc::new(CertManager::new(CertManagerConfig {
            cert_dir: temp.path().join("certs"),
            ..Default::default()
        }));
        let state = ServerState::new(
            temp.path().to_path_buf(),
            cert_manager,
            None,
            empty_challenge_tokens(),
        )
        .unwrap();

        let release_dir = temp
            .path()
            .join("apps")
            .join("demo-app")
            .join("releases")
            .join("v1");
        std::fs::create_dir_all(&release_dir).unwrap();

        let response = state
            .handle_command(Command::Deploy {
                app: "demo-app".to_string(),
                version: "../v1".to_string(),
                path: release_dir.to_string_lossy().to_string(),
                routes: vec!["api.example.com".to_string()],
                secrets: Some(HashMap::new()),
            })
            .await;

        let Response::Error { message } = response else {
            panic!("expected invalid release version to be rejected");
        };
        assert!(
            message.contains("Invalid release version"),
            "got: {message}"
        );
    }

    #[test]
    fn private_route_domains_prefer_self_signed_certs() {
        assert!(should_use_self_signed_route_cert(
            "tako-bun-server.orb.local"
        ));
        assert!(should_use_self_signed_route_cert("localhost"));
        assert!(should_use_self_signed_route_cert("api.localhost"));
        assert!(should_use_self_signed_route_cert("my-service"));
    }

    #[test]
    fn public_route_domains_do_not_prefer_self_signed_certs() {
        assert!(!should_use_self_signed_route_cert("api.example.com"));
        assert!(!should_use_self_signed_route_cert("example.com"));
    }

    #[test]
    fn resolve_release_runtime_requires_manifest() {
        let temp = TempDir::new().unwrap();
        let err = resolve_release_runtime(temp.path()).unwrap_err();
        assert!(err.contains("failed to read deploy manifest"));
    }

    #[test]
    fn resolve_release_runtime_reads_manifest_runtime() {
        let temp = TempDir::new().unwrap();
        write_release_manifest(temp.path(), "bun", "index.ts", &[], None, 300);
        assert_eq!(
            resolve_release_runtime(temp.path()).unwrap(),
            "bun".to_string()
        );
    }

    #[test]
    fn bun_install_args_use_frozen_lockfile_when_lock_exists() {
        let temp = TempDir::new().unwrap();
        std::fs::write(temp.path().join("bun.lock"), "").unwrap();
        assert_eq!(
            bun_install_args_for_release(temp.path()),
            vec![
                "install".to_string(),
                "--production".to_string(),
                "--frozen-lockfile".to_string()
            ]
        );
    }

    #[test]
    fn bun_install_args_use_plain_install_without_lockfile() {
        let temp = TempDir::new().unwrap();
        assert_eq!(
            bun_install_args_for_release(temp.path()),
            vec!["install".to_string(), "--production".to_string()]
        );
    }

    #[tokio::test]
    async fn prepare_release_runtime_installs_bun_dependencies() {
        let temp = TempDir::new().unwrap();
        let release_dir = temp.path().join("release");
        std::fs::create_dir_all(&release_dir).unwrap();
        std::fs::write(
            release_dir.join("app.json"),
            r#"{"runtime":"bun","main":"index.ts","idle_timeout":300}"#,
        )
        .unwrap();
        std::fs::write(
            release_dir.join("package.json"),
            r#"{"name":"test-app","dependencies":{"tako.sh":"0.0.0"}} "#,
        )
        .unwrap();

        let fake_bin_dir = temp.path().join("bin");
        std::fs::create_dir_all(&fake_bin_dir).unwrap();
        let fake_bun = fake_bin_dir.join("bun");
        std::fs::write(
            &fake_bun,
            r#"#!/bin/sh
if [ "$1" = "install" ]; then
  mkdir -p node_modules/tako.sh/src/entrypoints
  printf "export {};\n" > node_modules/tako.sh/src/entrypoints/bun.ts
  exit 0
fi
echo "unexpected bun args: $*" >&2
exit 1
"#,
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = std::fs::metadata(&fake_bun).unwrap().permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(&fake_bun, permissions).unwrap();
        }

        let mut env = HashMap::new();
        let path = std::env::var("PATH").unwrap_or_default();
        env.insert(
            "PATH".to_string(),
            format!("{}:{}", fake_bin_dir.display(), path),
        );

        prepare_release_runtime(&release_dir, &env).await.unwrap();
        assert!(
            release_dir
                .join("node_modules/tako.sh/src/entrypoints/bun.ts")
                .is_file()
        );
    }

    #[tokio::test]
    async fn prepare_release_runtime_bun_install_does_not_require_package_json() {
        let temp = TempDir::new().unwrap();
        let release_dir = temp.path().join("release");
        std::fs::create_dir_all(&release_dir).unwrap();
        std::fs::write(
            release_dir.join("app.json"),
            r#"{"runtime":"bun","main":"index.ts","idle_timeout":300}"#,
        )
        .unwrap();

        let fake_bin_dir = temp.path().join("bin");
        std::fs::create_dir_all(&fake_bin_dir).unwrap();
        let fake_bun = fake_bin_dir.join("bun");
        std::fs::write(
            &fake_bun,
            r#"#!/bin/sh
if [ "$1" = "install" ]; then
  mkdir -p node_modules/tako.sh/src/entrypoints
  printf "export {};\n" > node_modules/tako.sh/src/entrypoints/bun.ts
  exit 0
fi
echo "unexpected bun args: $*" >&2
exit 1
"#,
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = std::fs::metadata(&fake_bun).unwrap().permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(&fake_bun, permissions).unwrap();
        }

        let mut env = HashMap::new();
        let path = std::env::var("PATH").unwrap_or_default();
        env.insert(
            "PATH".to_string(),
            format!("{}:{}", fake_bin_dir.display(), path),
        );

        prepare_release_runtime(&release_dir, &env).await.unwrap();
        assert!(
            release_dir
                .join("node_modules/tako.sh/src/entrypoints/bun.ts")
                .is_file()
        );
    }

    #[tokio::test]
    async fn prepare_release_runtime_runs_manifest_install_command_when_present() {
        let temp = TempDir::new().unwrap();
        let release_dir = temp.path().join("release");
        std::fs::create_dir_all(&release_dir).unwrap();
        std::fs::write(
            release_dir.join("app.json"),
            r#"{"runtime":"bun","main":"index.ts","idle_timeout":300,"install":"mkdir -p node_modules/tako.sh/src/entrypoints && printf 'export {};\n' > node_modules/tako.sh/src/entrypoints/bun.ts"}"#,
        )
        .unwrap();
        std::fs::write(
            release_dir.join("package.json"),
            r#"{"name":"test-app","dependencies":{"tako.sh":"0.0.0"}} "#,
        )
        .unwrap();

        prepare_release_runtime(&release_dir, &HashMap::new())
            .await
            .unwrap();
        assert!(
            release_dir
                .join("node_modules/tako.sh/src/entrypoints/bun.ts")
                .is_file()
        );
    }

    fn python3_ok() -> bool {
        StdCommand::new("python3")
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    fn python3_can_bind_unix_socket() -> bool {
        let temp = TempDir::new().unwrap();
        let socket_path = temp.path().join("probe.sock");
        StdCommand::new("python3")
            .args([
                "-c",
                "import socket, sys; s = socket.socket(socket.AF_UNIX); s.bind(sys.argv[1]); s.close()",
            ])
            .arg(&socket_path)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    }

    fn pick_free_port() -> Option<u16> {
        std::net::TcpListener::bind("127.0.0.1:0")
            .ok()
            .and_then(|l| l.local_addr().ok().map(|a| a.port()))
    }

    #[tokio::test]
    async fn ensure_route_certificate_generates_self_signed_for_private_domain() {
        let temp = TempDir::new().unwrap();
        let cert_manager = Arc::new(CertManager::new(CertManagerConfig {
            cert_dir: temp.path().join("certs"),
            ..Default::default()
        }));
        cert_manager.init().unwrap();
        let state = ServerState::new(
            temp.path().to_path_buf(),
            cert_manager.clone(),
            None,
            empty_challenge_tokens(),
        )
        .unwrap();

        let cert = state
            .ensure_route_certificate("my-app", "tako-bun-server.orb.local")
            .await
            .expect("private domain should get a generated cert");
        assert!(cert.is_self_signed);
        assert_eq!(cert.domain, "tako-bun-server.orb.local");

        let cached = cert_manager
            .get_cert_for_host("tako-bun-server.orb.local")
            .expect("generated cert should be cached");
        assert!(cached.is_self_signed);
    }

    #[tokio::test]
    async fn delete_command_removes_runtime_registration_and_routes() {
        let temp = TempDir::new().unwrap();
        let cert_manager = Arc::new(CertManager::new(CertManagerConfig {
            cert_dir: temp.path().join("certs"),
            ..Default::default()
        }));
        let state = ServerState::new(
            temp.path().to_path_buf(),
            cert_manager,
            None,
            empty_challenge_tokens(),
        )
        .unwrap();

        let release_dir = temp
            .path()
            .join("apps")
            .join("my-app")
            .join("releases")
            .join("v1");
        std::fs::create_dir_all(&release_dir).unwrap();

        let config = AppConfig {
            name: "my-app".to_string(),
            version: "v1".to_string(),
            path: release_dir.clone(),
            command: vec![
                "/bin/sh".to_string(),
                "-lc".to_string(),
                "exit 0".to_string(),
            ],
            min_instances: 0,
            ..Default::default()
        };

        let app = state.app_manager.register_app(config);
        state.load_balancer.register_app(app);
        {
            let mut route_table = state.routes.write().await;
            route_table.set_app_routes("my-app".to_string(), vec!["api.example.com".to_string()]);
        }

        let response = state
            .handle_command(Command::Delete {
                app: "my-app".to_string(),
            })
            .await;
        assert!(matches!(response, Response::Ok { .. }));
        assert!(state.app_manager.get_app("my-app").is_none());

        let route_table = state.routes.read().await;
        assert!(route_table.routes_for_app("my-app").is_empty());
        assert_eq!(route_table.select("api.example.com", "/"), None);
    }

    #[tokio::test]
    async fn delete_command_is_idempotent_for_missing_app() {
        let temp = TempDir::new().unwrap();
        let cert_manager = Arc::new(CertManager::new(CertManagerConfig {
            cert_dir: temp.path().join("certs"),
            ..Default::default()
        }));
        let state = ServerState::new(
            temp.path().to_path_buf(),
            cert_manager,
            None,
            empty_challenge_tokens(),
        )
        .unwrap();

        let response = state
            .handle_command(Command::Delete {
                app: "missing-app".to_string(),
            })
            .await;
        assert!(matches!(response, Response::Ok { .. }));
        assert!(state.app_manager.get_app("missing-app").is_none());
    }

    #[tokio::test]
    async fn delete_command_rejects_invalid_app_name() {
        let temp = TempDir::new().unwrap();
        let cert_manager = Arc::new(CertManager::new(CertManagerConfig {
            cert_dir: temp.path().join("certs"),
            ..Default::default()
        }));
        let state = ServerState::new(
            temp.path().to_path_buf(),
            cert_manager,
            None,
            empty_challenge_tokens(),
        )
        .unwrap();

        let response = state
            .handle_command(Command::Delete {
                app: "../bad".to_string(),
            })
            .await;

        let Response::Error { message } = response else {
            panic!("expected invalid app name to be rejected");
        };
        assert!(message.contains("Invalid app name"), "got: {message}");
    }

    #[tokio::test]
    async fn upgrading_mode_blocks_mutating_commands() {
        let temp = TempDir::new().unwrap();
        let cert_manager = Arc::new(CertManager::new(CertManagerConfig {
            cert_dir: temp.path().join("certs"),
            ..Default::default()
        }));
        let state = ServerState::new(
            temp.path().to_path_buf(),
            cert_manager,
            None,
            empty_challenge_tokens(),
        )
        .unwrap();
        state.set_server_mode(UpgradeMode::Upgrading).await.unwrap();

        let response = state
            .handle_command(Command::Delete {
                app: "my-app".to_string(),
            })
            .await;

        let Response::Error { message } = response else {
            panic!("expected blocked mutating command while upgrading");
        };
        assert!(message.contains("Server is upgrading"));
        assert!(message.contains("delete"));
    }

    #[tokio::test]
    async fn server_mode_resets_upgrading_on_boot() {
        let temp = TempDir::new().unwrap();
        let cert_manager = Arc::new(CertManager::new(CertManagerConfig {
            cert_dir: temp.path().join("certs"),
            ..Default::default()
        }));

        let state_a = ServerState::new(
            temp.path().to_path_buf(),
            cert_manager.clone(),
            None,
            empty_challenge_tokens(),
        )
        .unwrap();
        state_a
            .set_server_mode(UpgradeMode::Upgrading)
            .await
            .unwrap();
        // Simulate an upgrade lock left behind by a crashed CLI.
        assert!(state_a.try_enter_upgrading("crashed-cli").await.unwrap());
        drop(state_a);

        // On restart, stale Upgrading mode AND orphaned lock should be cleared.
        let state_b = ServerState::new(
            temp.path().to_path_buf(),
            cert_manager.clone(),
            None,
            empty_challenge_tokens(),
        )
        .unwrap();
        assert_eq!(*state_b.server_mode.read().await, UpgradeMode::Normal);
        // A new owner should be able to acquire immediately (no 10-min stale wait).
        assert!(state_b.try_enter_upgrading("new-cli").await.unwrap());
    }

    #[tokio::test]
    async fn upgrading_lock_allows_single_owner() {
        let temp = TempDir::new().unwrap();
        let cert_manager = Arc::new(CertManager::new(CertManagerConfig {
            cert_dir: temp.path().join("certs"),
            ..Default::default()
        }));
        let state_a = ServerState::new(
            temp.path().to_path_buf(),
            cert_manager.clone(),
            None,
            empty_challenge_tokens(),
        )
        .unwrap();
        let state_b = ServerState::new(
            temp.path().to_path_buf(),
            cert_manager,
            None,
            empty_challenge_tokens(),
        )
        .unwrap();

        assert!(state_a.try_enter_upgrading("controller-a").await.unwrap());
        assert!(!state_b.try_enter_upgrading("controller-b").await.unwrap());
        assert!(state_a.exit_upgrading("controller-a").await.unwrap());
        assert!(state_b.try_enter_upgrading("controller-b").await.unwrap());
    }

    #[tokio::test]
    async fn server_info_command_reports_runtime_config() {
        let temp = TempDir::new().unwrap();
        let cert_manager = Arc::new(CertManager::new(CertManagerConfig {
            cert_dir: temp.path().join("certs"),
            ..Default::default()
        }));
        let runtime = ServerRuntimeConfig {
            pid: std::process::id(),
            socket: "/var/run/tako/tako-custom.sock".to_string(),
            data_dir: temp.path().to_path_buf(),
            http_port: 8080,
            https_port: 8443,
            no_acme: true,
            acme_staging: false,
            acme_email: Some("ops@example.com".to_string()),
            renewal_interval_hours: 24,
            dns_provider: None,
            worker: false,
            metrics_port: Some(9898),
            server_name: Some("test-server".to_string()),
        };
        let state = ServerState::new_with_runtime(
            temp.path().to_path_buf(),
            cert_manager,
            None,
            empty_challenge_tokens(),
            runtime,
        )
        .unwrap();
        state
            .set_server_mode(UpgradeMode::Upgrading)
            .await
            .expect("mode set");

        let response = state.handle_command(Command::ServerInfo).await;
        let Response::Ok { data } = response else {
            panic!("expected server info response");
        };
        assert_eq!(
            data.get("pid").and_then(Value::as_u64),
            Some(std::process::id() as u64)
        );
        assert_eq!(data.get("mode").and_then(Value::as_str), Some("upgrading"));
        assert_eq!(
            data.get("socket").and_then(Value::as_str),
            Some("/var/run/tako/tako-custom.sock")
        );
        assert_eq!(data.get("http_port").and_then(Value::as_u64), Some(8080));
        assert_eq!(data.get("https_port").and_then(Value::as_u64), Some(8443));
        assert_eq!(data.get("no_acme").and_then(Value::as_bool), Some(true));
    }

    #[tokio::test]
    async fn enter_and_exit_upgrading_commands_use_owner_lock() {
        let temp = TempDir::new().unwrap();
        let cert_manager = Arc::new(CertManager::new(CertManagerConfig {
            cert_dir: temp.path().join("certs"),
            ..Default::default()
        }));
        let state = ServerState::new(
            temp.path().to_path_buf(),
            cert_manager,
            None,
            empty_challenge_tokens(),
        )
        .unwrap();

        let enter = state
            .handle_command(Command::EnterUpgrading {
                owner: "controller-a".to_string(),
            })
            .await;
        assert!(matches!(enter, Response::Ok { .. }));

        let reject = state
            .handle_command(Command::EnterUpgrading {
                owner: "controller-b".to_string(),
            })
            .await;
        let Response::Error { message } = reject else {
            panic!("expected lock owner rejection");
        };
        assert!(message.contains("already upgrading"));
        assert!(message.contains("controller-a"));

        let wrong_exit = state
            .handle_command(Command::ExitUpgrading {
                owner: "controller-b".to_string(),
            })
            .await;
        assert!(matches!(wrong_exit, Response::Error { .. }));

        let exit = state
            .handle_command(Command::ExitUpgrading {
                owner: "controller-a".to_string(),
            })
            .await;
        assert!(matches!(exit, Response::Ok { .. }));
    }

    #[tokio::test]
    async fn get_secrets_hash_returns_hash_of_app_secrets() {
        let temp = TempDir::new().unwrap();
        let cert_manager = Arc::new(CertManager::new(CertManagerConfig {
            cert_dir: temp.path().join("certs"),
            ..Default::default()
        }));
        let state = ServerState::new(
            temp.path().to_path_buf(),
            cert_manager,
            None,
            empty_challenge_tokens(),
        )
        .unwrap();

        // No secrets file → hash of empty map
        let response = state
            .handle_command(Command::GetSecretsHash {
                app: "my-app".to_string(),
            })
            .await;
        let Response::Ok { data } = &response else {
            panic!("expected ok response: {response:?}");
        };
        let empty_hash = data.get("hash").and_then(Value::as_str).unwrap();
        assert_eq!(empty_hash, tako_core::compute_secrets_hash(&HashMap::new()));

        // Store secrets and check hash changes
        let secrets: HashMap<String, String> = [("KEY".to_string(), "val".to_string())]
            .into_iter()
            .collect();
        state.state_store.set_secrets("my-app", &secrets).unwrap();

        let response = state
            .handle_command(Command::GetSecretsHash {
                app: "my-app".to_string(),
            })
            .await;
        let Response::Ok { data } = &response else {
            panic!("expected ok response");
        };
        let with_secrets_hash = data.get("hash").and_then(Value::as_str).unwrap();
        assert_ne!(with_secrets_hash, empty_hash);
        assert_eq!(with_secrets_hash, tako_core::compute_secrets_hash(&secrets));
    }

    #[tokio::test]
    async fn deploy_without_secrets_keeps_existing() {
        let temp = TempDir::new().unwrap();
        let cert_manager = Arc::new(CertManager::new(CertManagerConfig {
            cert_dir: temp.path().join("certs"),
            ..Default::default()
        }));
        let state = ServerState::new(
            temp.path().to_path_buf(),
            cert_manager,
            None,
            empty_challenge_tokens(),
        )
        .unwrap();

        // Pre-store secrets for the app
        let secrets: HashMap<String, String> = [("API_KEY".to_string(), "original".to_string())]
            .into_iter()
            .collect();
        state.state_store.set_secrets("keep-app", &secrets).unwrap();

        let release_dir = temp
            .path()
            .join("apps")
            .join("keep-app")
            .join("releases")
            .join("v1");
        std::fs::create_dir_all(&release_dir).unwrap();
        write_release_manifest(
            &release_dir,
            "node",
            "index.js",
            &["/bin/sh", "-lc", "sleep 600"],
            Some("true"),
            300,
        );

        // Deploy with secrets: None — should keep existing
        let _response = state
            .handle_command(Command::Deploy {
                app: "keep-app".to_string(),
                version: "v1".to_string(),
                path: release_dir.to_string_lossy().to_string(),
                routes: vec!["keep.localhost".to_string()],
                secrets: None,
            })
            .await;

        // Verify secrets still have original value
        let loaded = state.state_store.get_secrets("keep-app").unwrap();
        assert_eq!(loaded.get("API_KEY"), Some(&"original".to_string()));
    }

    #[tokio::test]
    async fn restore_from_state_store_rehydrates_apps_routes_and_secrets() {
        let temp = TempDir::new().unwrap();
        let app_id = "my-app/production";
        let cert_manager = Arc::new(CertManager::new(CertManagerConfig {
            cert_dir: temp.path().join("certs"),
            ..Default::default()
        }));

        let state_a = ServerState::new(
            temp.path().to_path_buf(),
            cert_manager.clone(),
            None,
            empty_challenge_tokens(),
        )
        .unwrap();
        let release_dir = temp
            .path()
            .join("apps")
            .join("my-app")
            .join("production")
            .join("releases")
            .join("v1");
        std::fs::create_dir_all(&release_dir).unwrap();
        write_release_manifest(
            &release_dir,
            "node",
            "index.js",
            &["/bin/sh", "-lc", "sleep 600"],
            Some("true"),
            300,
        );

        let app_secrets: HashMap<String, String> =
            [("DATABASE_URL".to_string(), "postgres://db".to_string())]
                .into_iter()
                .collect();
        state_a
            .state_store
            .set_secrets(app_id, &app_secrets)
            .unwrap();

        let app = state_a.app_manager.register_app(AppConfig {
            name: "my-app".to_string(),
            environment: "production".to_string(),
            version: "v1".to_string(),
            path: release_dir.clone(),
            command: vec![
                "/bin/sh".to_string(),
                "-lc".to_string(),
                "sleep 600".to_string(),
            ],
            min_instances: 0,
            max_instances: 4,
            idle_timeout: Duration::from_secs(300),
            ..Default::default()
        });
        state_a.load_balancer.register_app(app);
        {
            let mut route_table = state_a.routes.write().await;
            route_table.set_app_routes(
                app_id.to_string(),
                vec![
                    "api.example.com".to_string(),
                    "example.com/api/*".to_string(),
                ],
            );
        }
        state_a.persist_app_state(app_id).await;
        drop(state_a);

        let state_b = ServerState::new(
            temp.path().to_path_buf(),
            cert_manager,
            None,
            empty_challenge_tokens(),
        )
        .unwrap();
        state_b.restore_from_state_store().await.unwrap();

        let restored = state_b.app_manager.get_app(app_id).expect("app restored");
        assert_eq!(restored.version(), "v1");
        assert_eq!(restored.state(), crate::socket::AppState::Idle);
        let route_table = state_b.routes.read().await;
        assert_eq!(
            route_table.routes_for_app(app_id),
            vec![
                "api.example.com".to_string(),
                "example.com/api/*".to_string()
            ]
        );
        let restored_secrets = restored.config.read().secrets.clone();
        assert_eq!(
            restored_secrets.get("DATABASE_URL"),
            Some(&"postgres://db".to_string())
        );
    }

    #[tokio::test]
    async fn scale_command_persists_zero_instances_across_restore() {
        let temp = TempDir::new().unwrap();
        let cert_manager = Arc::new(CertManager::new(CertManagerConfig {
            cert_dir: temp.path().join("certs"),
            ..Default::default()
        }));

        let state_a = ServerState::new(
            temp.path().to_path_buf(),
            cert_manager.clone(),
            None,
            empty_challenge_tokens(),
        )
        .unwrap();
        let release_dir = temp
            .path()
            .join("apps")
            .join("my-app")
            .join("releases")
            .join("v1");
        std::fs::create_dir_all(&release_dir).unwrap();
        std::fs::write(
            release_dir.join("app.json"),
            r#"{"runtime":"node","main":"index.js","idle_timeout":300,"start":["/bin/sh","-lc","sleep 600"]}"#,
        )
        .unwrap();

        let app = state_a.app_manager.register_app(AppConfig {
            name: "my-app".to_string(),
            version: "v1".to_string(),
            path: release_dir.clone(),
            command: vec![
                "/bin/sh".to_string(),
                "-lc".to_string(),
                "sleep 600".to_string(),
            ],
            min_instances: 2,
            max_instances: 4,
            idle_timeout: Duration::from_secs(300),
            ..Default::default()
        });
        state_a.load_balancer.register_app(app.clone());
        {
            let mut route_table = state_a.routes.write().await;
            route_table.set_app_routes("my-app".to_string(), vec!["api.example.com".to_string()]);
        }

        let first = app.allocate_instance();
        first.set_state(InstanceState::Healthy);
        let second = app.allocate_instance();
        second.set_state(InstanceState::Healthy);

        let response = state_a
            .handle_command(Command::Scale {
                app: "my-app".to_string(),
                instances: 0,
            })
            .await;
        assert!(matches!(response, Response::Ok { .. }));
        assert_eq!(app.config.read().min_instances, 0);
        assert!(app.get_instances().is_empty());

        drop(state_a);

        let state_b = ServerState::new(
            temp.path().to_path_buf(),
            cert_manager,
            None,
            empty_challenge_tokens(),
        )
        .unwrap();
        state_b.restore_from_state_store().await.unwrap();

        let restored = state_b.app_manager.get_app("my-app").expect("app restored");
        assert_eq!(restored.config.read().min_instances, 0);
        assert_eq!(restored.state(), AppState::Idle);
    }

    #[tokio::test]
    async fn deploy_preserves_scaled_instance_count() {
        let temp = TempDir::new().unwrap();
        let cert_manager = Arc::new(CertManager::new(CertManagerConfig {
            cert_dir: temp.path().join("certs"),
            ..Default::default()
        }));
        let state = ServerState::new(
            temp.path().to_path_buf(),
            cert_manager,
            None,
            empty_challenge_tokens(),
        )
        .unwrap();

        let current_release = temp
            .path()
            .join("apps")
            .join("my-app")
            .join("releases")
            .join("v1");
        std::fs::create_dir_all(&current_release).unwrap();
        std::fs::write(
            current_release.join("app.json"),
            r#"{"runtime":"node","main":"index.js","idle_timeout":300,"start":["/bin/sh","-lc","sleep 600"]}"#,
        )
        .unwrap();

        let app = state.app_manager.register_app(AppConfig {
            name: "my-app".to_string(),
            version: "v1".to_string(),
            path: current_release.clone(),
            command: vec![
                "/bin/sh".to_string(),
                "-lc".to_string(),
                "sleep 600".to_string(),
            ],
            min_instances: 2,
            max_instances: 4,
            idle_timeout: Duration::from_secs(300),
            ..Default::default()
        });
        state.load_balancer.register_app(app.clone());
        {
            let mut route_table = state.routes.write().await;
            route_table.set_app_routes("my-app".to_string(), vec!["api.example.com".to_string()]);
        }

        let old_instance = app.allocate_instance();
        old_instance.set_state(InstanceState::Healthy);

        let broken_release = temp
            .path()
            .join("apps")
            .join("my-app")
            .join("releases")
            .join("v2");
        std::fs::create_dir_all(&broken_release).unwrap();
        std::fs::write(
            broken_release.join("app.json"),
            r#"{"runtime":"node","main":"index.js","idle_timeout":300,"start":["/bin/sh","-lc","exit 1"]}"#,
        )
        .unwrap();

        let response = state
            .handle_command(Command::Deploy {
                app: "my-app".to_string(),
                version: "v2".to_string(),
                path: broken_release.to_string_lossy().to_string(),
                routes: vec!["api.example.com".to_string()],
                secrets: Some(HashMap::new()),
            })
            .await;

        assert!(matches!(response, Response::Error { .. }));
        assert_eq!(app.config.read().min_instances, 2);
    }

    #[tokio::test]
    async fn restore_uses_persisted_app_subdir() {
        let temp = TempDir::new().unwrap();
        let cert_manager = Arc::new(CertManager::new(CertManagerConfig {
            cert_dir: temp.path().join("certs"),
            ..Default::default()
        }));
        let state_a = ServerState::new(
            temp.path().to_path_buf(),
            cert_manager.clone(),
            None,
            empty_challenge_tokens(),
        )
        .unwrap();
        let release_dir = temp
            .path()
            .join("apps")
            .join("my-app")
            .join("releases")
            .join("v1");
        let app_dir = release_dir.join("apps/web");
        std::fs::create_dir_all(&app_dir).unwrap();
        write_release_manifest(
            &app_dir,
            "node",
            "index.js",
            &["/bin/sh", "-lc", "sleep 600"],
            Some("true"),
            300,
        );

        let app = state_a.app_manager.register_app(AppConfig {
            name: "my-app".to_string(),
            version: "v1".to_string(),
            app_subdir: "apps/web".to_string(),
            path: app_dir.clone(),
            command: vec![
                "/bin/sh".to_string(),
                "-lc".to_string(),
                "sleep 600".to_string(),
            ],
            min_instances: 0,
            max_instances: 4,
            idle_timeout: Duration::from_secs(300),
            ..Default::default()
        });
        state_a.load_balancer.register_app(app);
        {
            let mut route_table = state_a.routes.write().await;
            route_table.set_app_routes("my-app".to_string(), vec!["api.example.com".to_string()]);
        }
        state_a.persist_app_state("my-app").await;
        drop(state_a);

        let runtime = ServerRuntimeConfig {
            pid: std::process::id(),
            socket: "/var/run/tako/tako.sock".to_string(),
            data_dir: temp.path().to_path_buf(),
            http_port: 80,
            https_port: 443,
            no_acme: true,
            acme_staging: false,
            acme_email: None,
            renewal_interval_hours: 12,
            dns_provider: None,
            worker: false,
            metrics_port: Some(9898),
            server_name: None,
        };
        let state_b = ServerState::new_with_runtime(
            temp.path().to_path_buf(),
            cert_manager,
            None,
            empty_challenge_tokens(),
            runtime,
        )
        .unwrap();
        state_b.restore_from_state_store().await.unwrap();
        let restored = state_b.app_manager.get_app("my-app").expect("app restored");
        assert_eq!(restored.config.read().app_subdir, "apps/web");
        assert_eq!(restored.config.read().path, app_dir);
    }

    #[tokio::test]
    async fn delete_command_removes_persisted_state_for_next_boot() {
        let temp = TempDir::new().unwrap();
        let cert_manager = Arc::new(CertManager::new(CertManagerConfig {
            cert_dir: temp.path().join("certs"),
            ..Default::default()
        }));
        let state_a = ServerState::new(
            temp.path().to_path_buf(),
            cert_manager.clone(),
            None,
            empty_challenge_tokens(),
        )
        .unwrap();

        let release_dir = temp
            .path()
            .join("apps")
            .join("my-app")
            .join("releases")
            .join("v1");
        std::fs::create_dir_all(&release_dir).unwrap();
        write_release_manifest(
            &release_dir,
            "node",
            "index.js",
            &["/bin/sh", "-lc", "sleep 600"],
            Some("true"),
            300,
        );
        let app = state_a.app_manager.register_app(AppConfig {
            name: "my-app".to_string(),
            version: "v1".to_string(),
            path: release_dir.clone(),
            command: vec![
                "/bin/sh".to_string(),
                "-lc".to_string(),
                "sleep 600".to_string(),
            ],
            min_instances: 0,
            ..Default::default()
        });
        state_a.load_balancer.register_app(app);
        {
            let mut route_table = state_a.routes.write().await;
            route_table.set_app_routes(
                "my-app/production".to_string(),
                vec!["api.example.com".to_string()],
            );
        }
        state_a.persist_app_state("my-app/production").await;

        let response = state_a
            .handle_command(Command::Delete {
                app: "my-app/production".to_string(),
            })
            .await;
        assert!(matches!(response, Response::Ok { .. }));

        let state_b = ServerState::new(
            temp.path().to_path_buf(),
            cert_manager,
            None,
            empty_challenge_tokens(),
        )
        .unwrap();
        state_b.restore_from_state_store().await.unwrap();
        assert!(state_b.app_manager.get_app("my-app/production").is_none());
    }

    #[tokio::test]
    async fn deploy_on_demand_validates_startup_and_fails_for_unhealthy_build() {
        let temp = TempDir::new().unwrap();
        let cert_manager = Arc::new(CertManager::new(CertManagerConfig {
            cert_dir: temp.path().join("certs"),
            ..Default::default()
        }));
        let state = ServerState::new(
            temp.path().to_path_buf(),
            cert_manager,
            None,
            empty_challenge_tokens(),
        )
        .unwrap();

        let release_dir = temp
            .path()
            .join("apps")
            .join("broken-app")
            .join("releases")
            .join("v1");
        std::fs::create_dir_all(&release_dir).unwrap();
        std::fs::write(
            release_dir.join("app.json"),
            r#"{"runtime":"node","main":"index.js","idle_timeout":300,"install":"true","start":["/bin/sh","-lc","exit 1"]}"#,
        )
        .unwrap();

        let response = state
            .handle_command(Command::Deploy {
                app: "broken-app".to_string(),
                version: "v1".to_string(),
                path: release_dir.to_string_lossy().to_string(),
                routes: vec!["broken.localhost".to_string()],
                secrets: Some(HashMap::new()),
            })
            .await;

        assert!(
            matches!(response, Response::Error { .. }),
            "expected startup validation failure for on-demand deploy: {response:?}"
        );
    }

    #[tokio::test]
    async fn deploy_on_demand_keeps_one_warm_instance_after_successful_deploy() {
        if !python3_ok() || !python3_can_bind_unix_socket() {
            return;
        }

        let temp = TempDir::new().unwrap();
        let cert_manager = Arc::new(CertManager::new(CertManagerConfig {
            cert_dir: temp.path().join("certs"),
            ..Default::default()
        }));
        // Use a runtime config with the socket inside temp dir so that
        // app_socket_dir resolves to a short writable location (not /var/run),
        // keeping the unix socket path under platform limits.
        let runtime = ServerRuntimeConfig {
            socket: "/tmp/tako-warm.sock".to_string(),
            ..ServerRuntimeConfig::for_defaults(temp.path().to_path_buf())
        };
        let state = ServerState::new_with_runtime(
            temp.path().to_path_buf(),
            cert_manager,
            None,
            empty_challenge_tokens(),
            runtime,
        )
        .unwrap();

        let fake_bin_dir = temp.path().join("bin");
        std::fs::create_dir_all(&fake_bin_dir).unwrap();
        let fake_bun = fake_bin_dir.join("bun");
        let fake_server_py = temp.path().join("server.py");
        std::fs::write(
            &fake_server_py,
            r#"import os
import socketserver

socket_path = (os.environ.get("TAKO_APP_SOCKET") or "").replace("{pid}", str(os.getpid()))
if not socket_path:
    raise SystemExit("TAKO_APP_SOCKET is required")
if os.path.exists(socket_path):
    os.remove(socket_path)

class Handler(socketserver.StreamRequestHandler):
    def handle(self):
        try:
            _ = self.rfile.readline()
            while True:
                line = self.rfile.readline()
                if not line or line in (b"\r\n", b"\n"):
                    break
            self.wfile.write(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
        except Exception:
            return

socketserver.UnixStreamServer(socket_path, Handler).serve_forever()
"#,
        )
        .unwrap();
        // Use exec so python3 replaces the shell — its PID matches what
        // the spawner records, so the health-check socket path resolves.
        std::fs::write(
            &fake_bun,
            format!("#!/bin/sh\nexec python3 {}\n", fake_server_py.display()),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = std::fs::metadata(&fake_bun).unwrap().permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(&fake_bun, permissions).unwrap();
        }

        let release_dir = temp
            .path()
            .join("apps")
            .join("warm-app")
            .join("releases")
            .join("v1");
        std::fs::create_dir_all(&release_dir).unwrap();
        std::fs::write(
            release_dir.join("package.json"),
            r#"{"name":"warm-app","scripts":{"dev":"bun run index.ts"}}"#,
        )
        .unwrap();
        std::fs::write(release_dir.join("index.ts"), "export default {};\n").unwrap();
        std::fs::create_dir_all(release_dir.join("node_modules/tako.sh/src/entrypoints")).unwrap();
        std::fs::write(
            release_dir.join("node_modules/tako.sh/src/entrypoints/bun.ts"),
            "export default {};",
        )
        .unwrap();
        // Include PATH in the manifest env_vars so that the spawned instance
        // can find the fake bun binary.  Also set runtime_bin to the absolute
        // path so resolve_runtime_binary picks it up directly.
        let path_with_fake = format!(
            "{}:{}",
            fake_bin_dir.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        std::fs::write(
            release_dir.join("app.json"),
            serde_json::json!({
                "runtime": "bun",
                "main": "index.ts",
                "idle_timeout": 300,
                "install": "true",
                "runtime_bin": fake_bun.to_string_lossy().to_string(),
                "env_vars": { "PATH": &path_with_fake }
            })
            .to_string(),
        )
        .unwrap();

        let app = state.app_manager.register_app(AppConfig {
            name: "warm-app".to_string(),
            version: "v0".to_string(),
            path: release_dir.clone(),
            command: vec![
                "/bin/sh".to_string(),
                "-lc".to_string(),
                "exit 0".to_string(),
            ],
            min_instances: 0,
            max_instances: 4,
            ..Default::default()
        });
        state.load_balancer.register_app(app);

        let response = state
            .handle_command(Command::Deploy {
                app: "warm-app".to_string(),
                version: "v1".to_string(),
                path: release_dir.to_string_lossy().to_string(),
                routes: vec!["warm.localhost".to_string()],
                secrets: Some(HashMap::new()),
            })
            .await;
        assert!(
            matches!(response, Response::Ok { .. }),
            "expected successful on-demand deploy: {response:?}"
        );

        let status = state
            .handle_command(Command::Status {
                app: "warm-app".to_string(),
            })
            .await;
        let Response::Ok { data } = status else {
            panic!("expected status response for warm-app");
        };

        assert_eq!(data.get("state").and_then(Value::as_str), Some("running"));
        let instances = data
            .get("instances")
            .and_then(Value::as_array)
            .expect("status should include instances");
        assert_eq!(instances.len(), 1);
    }

    #[tokio::test]
    async fn instance_idle_event_resets_cold_start_when_app_scales_to_zero() {
        let temp = TempDir::new().unwrap();
        let cert_manager = Arc::new(CertManager::new(CertManagerConfig {
            cert_dir: temp.path().join("certs"),
            ..Default::default()
        }));
        let state = ServerState::new(
            temp.path().to_path_buf(),
            cert_manager,
            None,
            empty_challenge_tokens(),
        )
        .unwrap();

        let app = state.app_manager.register_app(AppConfig {
            name: "idle-app".to_string(),
            version: "v1".to_string(),
            min_instances: 0,
            ..Default::default()
        });
        state.load_balancer.register_app(app.clone());
        app.set_state(AppState::Running);

        let instance = app.allocate_instance();
        instance.set_state(InstanceState::Healthy);

        // Simulate a prior successful cold start.
        state.cold_start.begin("idle-app");
        state.cold_start.mark_ready("idle-app");
        assert!(!state.cold_start.begin("idle-app").leader);

        handle_idle_event(
            &state,
            crate::scaling::IdleEvent::InstanceIdle {
                app: "idle-app".to_string(),
                instance_id: instance.id.clone(),
            },
        )
        .await;

        assert!(app.get_instances().is_empty());
        assert_eq!(app.state(), AppState::Idle);
        assert!(state.cold_start.begin("idle-app").leader);
    }

    #[tokio::test]
    async fn status_includes_running_builds_for_each_version() {
        let temp = TempDir::new().unwrap();
        let cert_manager = Arc::new(CertManager::new(CertManagerConfig {
            cert_dir: temp.path().join("certs"),
            ..Default::default()
        }));
        let state = ServerState::new(
            temp.path().to_path_buf(),
            cert_manager,
            None,
            empty_challenge_tokens(),
        )
        .unwrap();

        let app = state.app_manager.register_app(AppConfig {
            name: "my-app".to_string(),
            version: "v1".to_string(),
            min_instances: 0,
            ..Default::default()
        });

        let old = app.allocate_instance();
        old.set_state(InstanceState::Healthy);

        let mut cfg = app.config.read().clone();
        cfg.version = "v2".to_string();
        app.update_config(cfg);

        let new = app.allocate_instance();
        new.set_state(InstanceState::Healthy);

        let response = state
            .handle_command(Command::Status {
                app: "my-app".to_string(),
            })
            .await;

        let Response::Ok { data } = response else {
            panic!("expected ok status response");
        };

        let builds = data
            .get("builds")
            .and_then(Value::as_array)
            .expect("status should include builds");
        let versions: Vec<&str> = builds
            .iter()
            .filter_map(|b| b.get("version").and_then(Value::as_str))
            .collect();
        assert!(
            versions.contains(&"v1") && versions.contains(&"v2"),
            "expected status to include both running builds: {data}"
        );
    }

    #[test]
    fn read_server_config_from_json() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("config.json"),
            r#"{"server_name":"prod","dns":{"provider":"cloudflare"}}"#,
        )
        .unwrap();
        let config = read_server_config(dir.path());
        assert_eq!(config.server_name.as_deref(), Some("prod"));
        assert_eq!(config.dns.as_ref().unwrap().provider, "cloudflare");
    }

    #[test]
    fn read_server_config_returns_defaults_when_missing() {
        let dir = TempDir::new().unwrap();
        let config = read_server_config(dir.path());
        assert!(config.server_name.is_none());
        assert!(config.dns.is_none());
    }
}

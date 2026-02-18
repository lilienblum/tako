// This crate contains runtime components that are exercised indirectly in integration tests.
#![allow(dead_code)]

mod app_command;
mod app_socket_cleanup;
mod defaults;
mod instances;
mod lb;
mod paths;
mod protocol;
mod proxy;
mod routing;
mod scaling;
mod socket;
mod state_store;
mod tls;

use crate::app_command::command_for_release_dir;
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
use crate::state_store::{SqliteStateStore, StateStoreError};
use crate::tls::{AcmeClient, AcmeConfig, CertInfo, CertManager, CertManagerConfig};
use clap::Parser;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::sync::Arc;
use std::time::Duration;
use tako_core::{HelloResponse, PROTOCOL_VERSION, ServerRuntimeInfo, UpgradeMode};
use tokio::process::Command as TokioCommand;
use tokio::sync::RwLock;
use tokio::sync::mpsc;
use tracing_subscriber::EnvFilter;

/// Tako Server - Application runtime and proxy
#[derive(Parser)]
#[command(name = "tako-server")]
#[command(version)]
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

    /// Offset added to persisted app base ports (used for temporary upgrade candidates).
    #[arg(long, default_value_t = 0)]
    pub instance_port_offset: u16,
}

#[derive(Debug, Clone)]
pub struct ServerRuntimeConfig {
    socket: String,
    data_dir: PathBuf,
    http_port: u16,
    https_port: u16,
    no_acme: bool,
    acme_staging: bool,
    acme_email: Option<String>,
    renewal_interval_hours: u64,
    instance_port_offset: u16,
}

impl ServerRuntimeConfig {
    fn for_defaults(data_dir: PathBuf) -> Self {
        Self {
            socket: "/var/run/tako/tako.sock".to_string(),
            data_dir,
            http_port: 80,
            https_port: 443,
            no_acme: false,
            acme_staging: false,
            acme_email: None,
            renewal_interval_hours: 12,
            instance_port_offset: 0,
        }
    }

    fn to_runtime_info(&self, mode: UpgradeMode) -> ServerRuntimeInfo {
        ServerRuntimeInfo {
            mode,
            socket: self.socket.clone(),
            data_dir: self.data_dir.to_string_lossy().to_string(),
            http_port: self.http_port,
            https_port: self.https_port,
            no_acme: self.no_acme,
            acme_staging: self.acme_staging,
            acme_email: self.acme_email.clone(),
            renewal_interval_hours: self.renewal_interval_hours,
            instance_port_offset: self.instance_port_offset,
        }
    }
}

/// Server state shared across components
pub struct ServerState {
    /// App manager
    app_manager: Arc<AppManager>,
    /// Load balancer
    load_balancer: Arc<LoadBalancer>,
    /// Certificate manager
    cert_manager: Arc<CertManager>,
    /// ACME client (optional)
    acme_client: Option<Arc<AcmeClient>>,
    /// App launch environment (app_name -> env vars)
    secrets: RwLock<HashMap<String, HashMap<String, String>>>,

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
    ) -> Result<Self, StateStoreError> {
        let runtime = ServerRuntimeConfig::for_defaults(data_dir.clone());
        Self::new_with_runtime(data_dir, cert_manager, acme_client, runtime)
    }

    pub fn new_with_runtime(
        data_dir: PathBuf,
        cert_manager: Arc<CertManager>,
        acme_client: Option<Arc<AcmeClient>>,
        runtime: ServerRuntimeConfig,
    ) -> Result<Self, StateStoreError> {
        let app_manager = Arc::new(AppManager::new());
        let load_balancer = Arc::new(LoadBalancer::new(app_manager.clone()));
        let state_store = Arc::new(SqliteStateStore::new(
            data_dir.join("runtime-state.sqlite3"),
        ));
        state_store.init()?;
        let server_mode = state_store.server_mode()?;

        Ok(Self {
            app_manager,
            load_balancer,
            cert_manager,
            acme_client,
            secrets: RwLock::new(HashMap::new()),
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
            let app_name = config.name.clone();
            let routes = persisted.routes.clone();
            let should_start = config.min_instances > 0;
            config.base_port =
                self.select_runtime_base_port(config.base_port, config.max_instances);

            let app = self.app_manager.register_app(config.clone());
            self.load_balancer.register_app(app.clone());

            {
                let mut route_table = self.routes.write().await;
                route_table.set_app_routes(app_name.clone(), routes);
            }

            {
                let mut secrets = self.secrets.write().await;
                secrets.insert(app_name.clone(), config.env.clone());
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
                    server_version: env!("CARGO_PKG_VERSION").to_string(),
                    capabilities: vec![
                        "deploy_instances_idle_timeout".to_string(),
                        "on_demand_cold_start".to_string(),
                        "idle_scale_to_zero".to_string(),
                        "upgrade_mode_control".to_string(),
                        "server_runtime_info".to_string(),
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
                env,
                instances,
                idle_timeout,
            } => {
                if let Some(resp) = self.reject_mutating_when_upgrading("deploy").await {
                    return resp;
                }
                self.deploy_app(&app, &version, &path, routes, env, instances, idle_timeout)
                    .await
            }
            Command::Stop { app } => {
                if let Some(resp) = self.reject_mutating_when_upgrading("stop").await {
                    return resp;
                }
                self.stop_app(&app).await
            }
            Command::Delete { app } => {
                if let Some(resp) = self.reject_mutating_when_upgrading("delete").await {
                    return resp;
                }
                self.delete_app(&app).await
            }
            Command::Status { app } => self.get_status(&app).await,
            Command::List => self.list_apps().await,
            Command::Routes => self.list_routes().await,
            Command::Reload { app } => {
                if let Some(resp) = self.reject_mutating_when_upgrading("reload").await {
                    return resp;
                }
                self.reload_app(&app).await
            }
            Command::UpdateSecrets { app, secrets } => {
                if let Some(resp) = self.reject_mutating_when_upgrading("update-secrets").await {
                    return resp;
                }
                self.update_secrets(&app, secrets).await
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
        }
    }

    async fn deploy_app(
        &self,
        app_name: &str,
        version: &str,
        path: &str,
        routes: Vec<String>,
        env: HashMap<String, String>,
        instances: u8,
        idle_timeout: u32,
    ) -> Response {
        tracing::info!(app = app_name, version = version, "Deploying app");

        if let Err(msg) = validate_deploy_routes(&routes) {
            return Response::error(msg);
        }

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

        if let Err(error) = prepare_release_runtime(Path::new(path), &env).await {
            return Response::error(format!("Invalid app release: {}", error));
        }

        {
            // Deploy carries the full launch environment (vars + secrets) for this release.
            let mut secrets = self.secrets.write().await;
            secrets.insert(app_name.to_string(), env.clone());
        }

        // Get or create app
        let (app, deploy_config, is_new_app) =
            if let Some(existing) = self.app_manager.get_app(app_name) {
                // Update existing app config. We'll perform a rolling update if instances are running.
                let mut config = existing.config.read().clone();
                config.version = version.to_string();
                config.path = PathBuf::from(path);
                config.cwd = PathBuf::from(path);
                config.env = env.clone();
                config.min_instances = instances as u32;
                config.idle_timeout = Duration::from_secs(idle_timeout as u64);
                config.command = match command_for_release_dir(&config.cwd) {
                    Ok(cmd) => cmd,
                    Err(e) => {
                        return Response::error(format!("Invalid app release: {}", e));
                    }
                };
                existing.update_config(config.clone());
                (existing, config, false)
            } else {
                // Create new app
                let config = AppConfig {
                    name: app_name.to_string(),
                    version: version.to_string(),
                    path: PathBuf::from(path),
                    cwd: PathBuf::from(path),
                    command: match command_for_release_dir(Path::new(path)) {
                        Ok(cmd) => cmd,
                        Err(e) => {
                            return Response::error(format!("Invalid app release: {}", e));
                        }
                    },
                    env,
                    min_instances: instances as u32,
                    max_instances: 4,
                    base_port: self.allocate_port_range(),
                    idle_timeout: Duration::from_secs(idle_timeout as u64),
                    ..Default::default()
                };

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
            let mut secrets = self.secrets.write().await;
            secrets.remove(app_name);
        }

        {
            let mut locks = self.deploy_locks.write().await;
            locks.remove(app_name);
        }

        if let Err(e) = self.state_store.delete_app(app_name) {
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

    async fn reload_app(&self, app_name: &str) -> Response {
        tracing::info!(app = app_name, "Reloading app with rolling restart");

        let app = match self.app_manager.get_app(app_name) {
            Some(app) => app,
            None => return Response::error(format!("App not found: {}", app_name)),
        };

        // Check if app is running - if not, just start it
        if app.get_instances().is_empty() {
            tracing::info!(app = app_name, "App has no instances, starting fresh");
            match self.app_manager.start_app(app_name).await {
                Ok(()) => {
                    app.set_state(AppState::Running);
                    return Response::ok(serde_json::json!({
                        "status": "started",
                        "app": app_name,
                        "note": "App was not running, started fresh"
                    }));
                }
                Err(e) => {
                    app.set_state(AppState::Error);
                    return Response::error(format!("Start failed: {}", e));
                }
            }
        }

        // Mark app as deploying during rolling update
        let previous_state = app.state();
        app.set_state(AppState::Deploying);

        // Get current config for rolling update
        let config = app.config.read().clone();

        // Create rolling updater with default config
        let rolling_config = RollingUpdateConfig::default();
        let updater = RollingUpdater::new(self.app_manager.spawner().clone(), rolling_config);

        // Perform rolling update
        let target_new_instances =
            target_new_instances_for_build(config.min_instances, app.get_instances().len());
        match updater.update(&app, config, target_new_instances).await {
            Ok(result) => {
                if result.success {
                    app.set_state(AppState::Running);
                    Response::ok(serde_json::json!({
                        "status": "reloaded",
                        "app": app_name,
                        "new_instances": result.new_instances,
                        "old_instances": result.old_instances,
                        "rolled_back": false
                    }))
                } else {
                    // Rollback occurred
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

    async fn update_secrets(
        &self,
        app_name: &str,
        new_secrets: HashMap<String, String>,
    ) -> Response {
        tracing::info!(app = app_name, "Updating secrets");

        // Store secrets
        {
            let mut secrets = self.secrets.write().await;
            secrets.insert(app_name.to_string(), new_secrets.clone());
        }

        // If app exists, update its config
        if let Some(app) = self.app_manager.get_app(app_name) {
            let mut config = app.config.read().clone();
            config.env.extend(new_secrets);
            app.update_config(config);
            self.persist_app_state(app_name).await;

            // Note: Running instances still have old secrets
            // User should call reload to apply new secrets
        }

        Response::ok(serde_json::json!({
            "status": "updated",
            "app": app_name,
            "note": "Call reload to apply secrets to running instances"
        }))
    }

    /// Request a certificate for a domain via ACME
    pub async fn request_certificate(&self, domain: &str) -> Response {
        let acme = match &self.acme_client {
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

    /// Allocate a base port for a new app
    fn allocate_port_range(&self) -> u16 {
        // Pick an unused local port as the base.
        // This avoids hard-coding ports like 3000, which often collide in dev/test.
        std::net::TcpListener::bind("127.0.0.1:0")
            .ok()
            .and_then(|l| l.local_addr().ok().map(|a| a.port()))
            .unwrap_or(3000)
    }

    fn select_runtime_base_port(&self, persisted_base: u16, max_instances: u32) -> u16 {
        let width = max_instances.max(1).min(64);
        let preferred = persisted_base.saturating_add(self.runtime.instance_port_offset);

        if Self::port_range_is_available(preferred, width) {
            return preferred;
        }

        for _ in 0..128 {
            let candidate = self.allocate_port_range();
            if Self::port_range_is_available(candidate, width) {
                return candidate;
            }
        }

        preferred
    }

    fn port_range_is_available(base: u16, width: u32) -> bool {
        let Ok(width_u16) = u16::try_from(width) else {
            return false;
        };
        if base.checked_add(width_u16.saturating_sub(1)).is_none() {
            return false;
        }

        for offset in 0..width_u16 {
            let port = base + offset;
            if std::net::TcpListener::bind(("127.0.0.1", port)).is_err() {
                return false;
            }
        }
        true
    }

    async fn start_on_demand_warm_instance(&self, app: &Arc<App>) -> Result<(), String> {
        let instance = app.allocate_instance();
        let spawner = self.app_manager.spawner();

        match spawner.spawn(app, instance.clone()).await {
            Ok(()) => Ok(()),
            Err(e) => {
                app.remove_instance(instance.id);
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

        let Some(acme) = &self.acme_client else {
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

#[derive(Debug, serde::Deserialize)]
struct ReleaseRuntimeManifest {
    runtime: String,
}

fn resolve_release_runtime(release_dir: &Path) -> Result<Option<String>, String> {
    let manifest_path = release_dir.join("app.json");
    if !manifest_path.exists() {
        return Ok(None);
    }

    let raw = std::fs::read_to_string(&manifest_path).map_err(|e| {
        format!(
            "failed to read deploy manifest {}: {}",
            manifest_path.display(),
            e
        )
    })?;
    let manifest: ReleaseRuntimeManifest = serde_json::from_str(&raw).map_err(|e| {
        format!(
            "failed to parse deploy manifest {}: {}",
            manifest_path.display(),
            e
        )
    })?;
    if manifest.runtime.trim().is_empty() {
        return Err(format!(
            "deploy manifest {} has empty runtime field",
            manifest_path.display()
        ));
    }
    Ok(Some(manifest.runtime))
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
    let Some(runtime) = resolve_release_runtime(release_dir)? else {
        return Ok(());
    };
    if runtime == "bun" {
        install_bun_dependencies_for_release(release_dir, env).await?;
    }
    Ok(())
}

async fn install_bun_dependencies_for_release(
    release_dir: &Path,
    env: &HashMap<String, String>,
) -> Result<(), String> {
    if !release_dir.join("package.json").is_file() {
        return Err(format!(
            "Bun release '{}' is missing package.json required for server-side dependency install.",
            release_dir.display()
        ));
    }

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

    let wrapper_path = release_dir.join("node_modules/tako.sh/src/wrapper.ts");
    if !wrapper_path.is_file() {
        return Err(format!(
            "Bun dependency install completed but '{}' is missing. Ensure package 'tako.sh' is installed.",
            wrapper_path.display()
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

fn main() -> Result<(), Box<dyn std::error::Error>> {
    install_rustls_crypto_provider();

    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_target(false)
        .init();

    let args = Args::parse();

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

    tracing::info!("Tako Server v{}", env!("CARGO_PKG_VERSION"));
    tracing::info!("Socket: {}", socket);
    tracing::info!("HTTP port: {}", args.port);
    tracing::info!("HTTPS port: {}", args.tls_port);
    tracing::info!("Data directory: {}", data_dir_str);

    // Create data directory
    let data_dir = PathBuf::from(&data_dir_str);
    std::fs::create_dir_all(&data_dir)?;

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

    // Create ACME client if enabled
    let acme_client = if args.no_acme {
        tracing::info!("ACME disabled, using manual certificate management");
        None
    } else {
        let acme_config = AcmeConfig {
            staging: args.acme_staging,
            email: args.acme_email.clone(),
            account_dir: acme_dir,
            ..Default::default()
        };

        let client = Arc::new(AcmeClient::new(acme_config, cert_manager.clone()));

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

    // Get ACME challenge tokens for proxy
    let acme_tokens = acme_client.as_ref().map(|c| c.challenge_tokens());

    // Create server state
    let runtime = ServerRuntimeConfig {
        socket: socket.clone(),
        data_dir: data_dir.clone(),
        http_port: args.port,
        https_port: args.tls_port,
        no_acme: args.no_acme,
        acme_staging: args.acme_staging,
        acme_email: args.acme_email.clone(),
        renewal_interval_hours: args.renewal_interval_hours,
        instance_port_offset: args.instance_port_offset,
    };
    let state = Arc::new(ServerState::new_with_runtime(
        data_dir.clone(),
        cert_manager.clone(),
        acme_client.clone(),
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

    // Start management socket
    let socket_state = state.clone();
    let socket_path = socket.clone();
    rt.spawn(async move {
        let server = SocketServer::new(&socket_path);
        if let Err(e) = server
            .run(move |cmd| {
                let state = socket_state.clone();
                async move { state.handle_command(cmd).await }
            })
            .await
        {
            tracing::error!("Socket server error: {}", e);
        }
    });

    // Best-effort cleanup for stale app unix socket files.
    // This matters if an app crashes and leaves behind a stale socket path.
    // During rolling updates multiple instances can be alive concurrently; we only remove sockets
    // whose PID no longer exists.
    {
        use std::path::Path;
        let dirs = [Path::new("/var/run"), Path::new("/var/run/tako")];
        for d in &dirs {
            app_socket_cleanup::cleanup_stale_app_sockets(d);
        }

        rt.spawn(async move {
            let dirs = [Path::new("/var/run"), Path::new("/var/run/tako")];
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
    };

    // Start Pingora proxy
    tracing::info!("Starting HTTP proxy on port {}", args.port);
    if proxy_config.enable_https {
        tracing::info!("HTTPS enabled on port {}", args.tls_port);
    }

    let server = proxy::build_server_with_acme(
        state.load_balancer.clone(),
        state.routes(),
        proxy_config,
        acme_tokens,
        Some(cert_manager),
        state.cold_start(),
    )?;

    // Run the server (this blocks)
    server.run_forever();

    #[allow(unreachable_code)]
    Ok(())
}

async fn handle_instance_event(state: &ServerState, event: InstanceEvent) {
    match event {
        InstanceEvent::Started { app, instance_id } => {
            tracing::debug!(app = %app, instance = instance_id, "Instance started");
        }
        InstanceEvent::Ready { app, instance_id } => {
            tracing::info!(app = %app, instance = instance_id, "Instance ready");
            state.cold_start.mark_ready(&app);

            if let Some(app_ref) = state.app_manager.get_app(&app) {
                app_ref.clear_last_error();
            }
        }
        InstanceEvent::Unhealthy { app, instance_id } => {
            tracing::warn!(app = %app, instance = instance_id, "Instance unhealthy");
            // Replace unhealthy instance after a grace period
            replace_instance_if_needed(state, &app, instance_id, "unhealthy").await;
        }
        InstanceEvent::Stopped { app, instance_id } => {
            tracing::info!(app = %app, instance = instance_id, "Instance stopped");
        }
    }
}

async fn handle_health_event(state: &ServerState, event: crate::instances::HealthEvent) {
    use crate::instances::HealthEvent;

    match event {
        HealthEvent::Healthy { app, instance_id } => {
            tracing::info!(app = %app, instance = instance_id, "Instance is healthy");
            state.cold_start.mark_ready(&app);

            if let Some(app_ref) = state.app_manager.get_app(&app) {
                app_ref.clear_last_error();
            }
        }
        HealthEvent::Unhealthy { app, instance_id } => {
            tracing::warn!(app = %app, instance = instance_id, "Instance became unhealthy");
            // Don't immediately replace - wait for Dead event or recovery
        }
        HealthEvent::Dead { app, instance_id } => {
            tracing::error!(app = %app, instance = instance_id, "Instance is dead (no heartbeat)");
            state.cold_start.mark_failed(&app);
            if let Some(app_ref) = state.app_manager.get_app(&app) {
                app_ref.set_last_error("Instance marked dead");
            }
            // Replace dead instance immediately
            replace_instance_if_needed(state, &app, instance_id, "dead").await;
        }
        HealthEvent::Recovered { app, instance_id } => {
            tracing::info!(app = %app, instance = instance_id, "Instance recovered from unhealthy");
        }
    }
}

async fn handle_idle_event(state: &ServerState, event: IdleEvent) {
    match event {
        IdleEvent::InstanceIdle { app, instance_id } => {
            if let Some(app_ref) = state.app_manager.get_app(&app)
                && let Some(instance) = app_ref.get_instance(instance_id)
            {
                if let Err(e) = instance.kill().await {
                    tracing::warn!(app = %app, instance = instance_id, "Failed to kill idle instance: {}", e);
                }
                app_ref.remove_instance(instance_id);

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
    instance_id: u32,
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
            tracing::debug!(app = %app_name, instance = instance_id, "Instance already removed");
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
            instance = instance_id,
            reason = reason,
            build = %failed_build,
            current = current_count,
            min = min_for_build,
            "Not replacing {} instance: have more than minimum instances",
            reason
        );
        // Just remove the bad instance
        if let Err(e) = instance.kill().await {
            tracing::error!(app = %app_name, instance = instance_id, "Failed to kill instance: {}", e);
        }
        app.remove_instance(instance_id);
        return;
    }

    tracing::info!(
        app = %app_name,
        instance = instance_id,
        reason = reason,
        "Replacing {} instance with a new one",
        reason
    );

    // Kill the old instance
    if let Err(e) = instance.kill().await {
        tracing::error!(app = %app_name, instance = instance_id, "Failed to kill old instance: {}", e);
    }
    app.remove_instance(instance_id);

    // Allocate and spawn a new instance
    let new_instance = app.allocate_instance();
    let spawner = state.app_manager.spawner();

    match spawner.spawn(&app, new_instance.clone()).await {
        Ok(()) => {
            tracing::info!(
                app = %app_name,
                old_instance = instance_id,
                new_instance = new_instance.id,
                "Successfully spawned replacement instance"
            );
        }
        Err(e) => {
            tracing::error!(
                app = %app_name,
                instance = new_instance.id,
                "Failed to spawn replacement instance: {}",
                e
            );
            // Clean up the failed instance
            app.remove_instance(new_instance.id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ServerRuntimeConfig, ServerState, bun_install_args_for_release, handle_idle_event,
        install_rustls_crypto_provider, prepare_release_runtime, resolve_release_runtime,
        should_use_self_signed_route_cert, validate_deploy_routes,
    };
    use crate::instances::AppConfig;
    use crate::socket::{AppState, Command, InstanceState, Response};
    use crate::tls::{CertManager, CertManagerConfig};
    use serde_json::Value;
    use std::collections::HashMap;
    use std::process::{Command as StdCommand, Stdio};
    use std::sync::Arc;
    use std::time::Duration;
    use tako_core::UpgradeMode;
    use tempfile::TempDir;

    #[test]
    fn install_rustls_crypto_provider_is_idempotent() {
        install_rustls_crypto_provider();
        assert!(rustls::crypto::CryptoProvider::get_default().is_some());

        install_rustls_crypto_provider();
        assert!(rustls::crypto::CryptoProvider::get_default().is_some());
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
    fn resolve_release_runtime_returns_none_when_manifest_missing() {
        let temp = TempDir::new().unwrap();
        assert_eq!(resolve_release_runtime(temp.path()).unwrap(), None);
    }

    #[test]
    fn resolve_release_runtime_reads_manifest_runtime() {
        let temp = TempDir::new().unwrap();
        std::fs::write(
            temp.path().join("app.json"),
            r#"{"runtime":"bun","main":"index.ts"}"#,
        )
        .unwrap();
        assert_eq!(
            resolve_release_runtime(temp.path()).unwrap(),
            Some("bun".to_string())
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
            r#"{"runtime":"bun","main":"index.ts"}"#,
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
  mkdir -p node_modules/tako.sh/src
  printf "export {};\n" > node_modules/tako.sh/src/wrapper.ts
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
                .join("node_modules/tako.sh/src/wrapper.ts")
                .is_file()
        );
    }

    #[tokio::test]
    async fn prepare_release_runtime_bun_requires_package_json() {
        let temp = TempDir::new().unwrap();
        std::fs::write(
            temp.path().join("app.json"),
            r#"{"runtime":"bun","main":"index.ts"}"#,
        )
        .unwrap();

        let err = prepare_release_runtime(temp.path(), &HashMap::new())
            .await
            .unwrap_err();
        assert!(err.contains("missing package.json"));
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
        let state =
            ServerState::new(temp.path().to_path_buf(), cert_manager.clone(), None).unwrap();

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
        let state = ServerState::new(temp.path().to_path_buf(), cert_manager, None).unwrap();

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
            cwd: release_dir.clone(),
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
        let state = ServerState::new(temp.path().to_path_buf(), cert_manager, None).unwrap();

        let response = state
            .handle_command(Command::Delete {
                app: "missing-app".to_string(),
            })
            .await;
        assert!(matches!(response, Response::Ok { .. }));
        assert!(state.app_manager.get_app("missing-app").is_none());
    }

    #[tokio::test]
    async fn upgrading_mode_blocks_mutating_commands() {
        let temp = TempDir::new().unwrap();
        let cert_manager = Arc::new(CertManager::new(CertManagerConfig {
            cert_dir: temp.path().join("certs"),
            ..Default::default()
        }));
        let state = ServerState::new(temp.path().to_path_buf(), cert_manager, None).unwrap();
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
    async fn server_mode_restores_from_store_on_boot() {
        let temp = TempDir::new().unwrap();
        let cert_manager = Arc::new(CertManager::new(CertManagerConfig {
            cert_dir: temp.path().join("certs"),
            ..Default::default()
        }));

        let state_a =
            ServerState::new(temp.path().to_path_buf(), cert_manager.clone(), None).unwrap();
        state_a
            .set_server_mode(UpgradeMode::Upgrading)
            .await
            .unwrap();
        drop(state_a);

        let state_b = ServerState::new(temp.path().to_path_buf(), cert_manager, None).unwrap();
        assert_eq!(*state_b.server_mode.read().await, UpgradeMode::Upgrading);
    }

    #[tokio::test]
    async fn upgrading_lock_allows_single_owner() {
        let temp = TempDir::new().unwrap();
        let cert_manager = Arc::new(CertManager::new(CertManagerConfig {
            cert_dir: temp.path().join("certs"),
            ..Default::default()
        }));
        let state_a =
            ServerState::new(temp.path().to_path_buf(), cert_manager.clone(), None).unwrap();
        let state_b = ServerState::new(temp.path().to_path_buf(), cert_manager, None).unwrap();

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
            socket: "/var/run/tako/tako-custom.sock".to_string(),
            data_dir: temp.path().to_path_buf(),
            http_port: 8080,
            https_port: 8443,
            no_acme: true,
            acme_staging: false,
            acme_email: Some("ops@example.com".to_string()),
            renewal_interval_hours: 24,
            instance_port_offset: 10000,
        };
        let state =
            ServerState::new_with_runtime(temp.path().to_path_buf(), cert_manager, None, runtime)
                .unwrap();
        state
            .set_server_mode(UpgradeMode::Upgrading)
            .await
            .expect("mode set");

        let response = state.handle_command(Command::ServerInfo).await;
        let Response::Ok { data } = response else {
            panic!("expected server info response");
        };
        assert_eq!(data.get("mode").and_then(Value::as_str), Some("upgrading"));
        assert_eq!(
            data.get("socket").and_then(Value::as_str),
            Some("/var/run/tako/tako-custom.sock")
        );
        assert_eq!(data.get("http_port").and_then(Value::as_u64), Some(8080));
        assert_eq!(data.get("https_port").and_then(Value::as_u64), Some(8443));
        assert_eq!(data.get("no_acme").and_then(Value::as_bool), Some(true));
        assert_eq!(
            data.get("instance_port_offset").and_then(Value::as_u64),
            Some(10000)
        );
    }

    #[tokio::test]
    async fn enter_and_exit_upgrading_commands_use_owner_lock() {
        let temp = TempDir::new().unwrap();
        let cert_manager = Arc::new(CertManager::new(CertManagerConfig {
            cert_dir: temp.path().join("certs"),
            ..Default::default()
        }));
        let state = ServerState::new(temp.path().to_path_buf(), cert_manager, None).unwrap();

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
    async fn restore_from_state_store_rehydrates_apps_routes_and_secrets() {
        let temp = TempDir::new().unwrap();
        let cert_manager = Arc::new(CertManager::new(CertManagerConfig {
            cert_dir: temp.path().join("certs"),
            ..Default::default()
        }));

        let state_a =
            ServerState::new(temp.path().to_path_buf(), cert_manager.clone(), None).unwrap();
        let release_dir = temp
            .path()
            .join("apps")
            .join("my-app")
            .join("releases")
            .join("v1");
        std::fs::create_dir_all(&release_dir).unwrap();

        let mut env = HashMap::new();
        env.insert("DATABASE_URL".to_string(), "postgres://db".to_string());
        let app = state_a.app_manager.register_app(AppConfig {
            name: "my-app".to_string(),
            version: "v1".to_string(),
            path: release_dir.clone(),
            cwd: release_dir,
            command: vec![
                "/bin/sh".to_string(),
                "-lc".to_string(),
                "sleep 600".to_string(),
            ],
            env: env.clone(),
            min_instances: 0,
            max_instances: 4,
            base_port: 4200,
            idle_timeout: Duration::from_secs(300),
            ..Default::default()
        });
        state_a.load_balancer.register_app(app);
        {
            let mut route_table = state_a.routes.write().await;
            route_table.set_app_routes(
                "my-app".to_string(),
                vec![
                    "api.example.com".to_string(),
                    "example.com/api/*".to_string(),
                ],
            );
        }
        {
            let mut secrets = state_a.secrets.write().await;
            secrets.insert("my-app".to_string(), env);
        }
        state_a.persist_app_state("my-app").await;
        drop(state_a);

        let state_b = ServerState::new(temp.path().to_path_buf(), cert_manager, None).unwrap();
        state_b.restore_from_state_store().await.unwrap();

        let restored = state_b.app_manager.get_app("my-app").expect("app restored");
        assert_eq!(restored.version(), "v1");
        assert_eq!(restored.state(), crate::socket::AppState::Idle);
        let route_table = state_b.routes.read().await;
        assert_eq!(
            route_table.routes_for_app("my-app"),
            vec![
                "api.example.com".to_string(),
                "example.com/api/*".to_string()
            ]
        );
        let secrets = state_b.secrets.read().await;
        let restored_secrets = secrets.get("my-app").expect("secrets restored");
        assert_eq!(
            restored_secrets.get("DATABASE_URL"),
            Some(&"postgres://db".to_string())
        );
    }

    #[tokio::test]
    async fn restore_rebases_base_port_when_range_is_unavailable() {
        let temp = TempDir::new().unwrap();
        let cert_manager = Arc::new(CertManager::new(CertManagerConfig {
            cert_dir: temp.path().join("certs"),
            ..Default::default()
        }));
        let state_a =
            ServerState::new(temp.path().to_path_buf(), cert_manager.clone(), None).unwrap();
        let release_dir = temp
            .path()
            .join("apps")
            .join("my-app")
            .join("releases")
            .join("v1");
        std::fs::create_dir_all(&release_dir).unwrap();

        let app = state_a.app_manager.register_app(AppConfig {
            name: "my-app".to_string(),
            version: "v1".to_string(),
            path: release_dir.clone(),
            cwd: release_dir,
            command: vec![
                "/bin/sh".to_string(),
                "-lc".to_string(),
                "sleep 600".to_string(),
            ],
            min_instances: 0,
            max_instances: 4,
            base_port: 65_000,
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
            socket: "/var/run/tako/tako.sock".to_string(),
            data_dir: temp.path().to_path_buf(),
            http_port: 80,
            https_port: 443,
            no_acme: true,
            acme_staging: false,
            acme_email: None,
            renewal_interval_hours: 12,
            instance_port_offset: 10_000,
        };
        let state_b =
            ServerState::new_with_runtime(temp.path().to_path_buf(), cert_manager, None, runtime)
                .unwrap();
        state_b.restore_from_state_store().await.unwrap();
        let restored = state_b.app_manager.get_app("my-app").expect("app restored");
        assert_ne!(restored.config.read().base_port, 65_000);
    }

    #[tokio::test]
    async fn delete_command_removes_persisted_state_for_next_boot() {
        let temp = TempDir::new().unwrap();
        let cert_manager = Arc::new(CertManager::new(CertManagerConfig {
            cert_dir: temp.path().join("certs"),
            ..Default::default()
        }));
        let state_a =
            ServerState::new(temp.path().to_path_buf(), cert_manager.clone(), None).unwrap();

        let release_dir = temp
            .path()
            .join("apps")
            .join("my-app")
            .join("releases")
            .join("v1");
        std::fs::create_dir_all(&release_dir).unwrap();
        let app = state_a.app_manager.register_app(AppConfig {
            name: "my-app".to_string(),
            version: "v1".to_string(),
            path: release_dir.clone(),
            cwd: release_dir,
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
            route_table.set_app_routes("my-app".to_string(), vec!["api.example.com".to_string()]);
        }
        state_a.persist_app_state("my-app").await;

        let response = state_a
            .handle_command(Command::Delete {
                app: "my-app".to_string(),
            })
            .await;
        assert!(matches!(response, Response::Ok { .. }));

        let state_b = ServerState::new(temp.path().to_path_buf(), cert_manager, None).unwrap();
        state_b.restore_from_state_store().await.unwrap();
        assert!(state_b.app_manager.get_app("my-app").is_none());
    }

    #[tokio::test]
    async fn deploy_on_demand_validates_startup_and_fails_for_unhealthy_build() {
        let temp = TempDir::new().unwrap();
        let cert_manager = Arc::new(CertManager::new(CertManagerConfig {
            cert_dir: temp.path().join("certs"),
            ..Default::default()
        }));
        let state = ServerState::new(temp.path().to_path_buf(), cert_manager, None).unwrap();

        let release_dir = temp
            .path()
            .join("apps")
            .join("broken-app")
            .join("releases")
            .join("v1");
        std::fs::create_dir_all(&release_dir).unwrap();
        std::fs::write(
            release_dir.join("package.json"),
            r#"{"name":"broken-app","scripts":{"dev":"bun run index.ts"}}"#,
        )
        .unwrap();
        // This script exits immediately and never exposes the internal status endpoint.
        std::fs::write(
            release_dir.join("index.ts"),
            "console.log('boot then exit');",
        )
        .unwrap();

        let response = state
            .handle_command(Command::Deploy {
                app: "broken-app".to_string(),
                version: "v1".to_string(),
                path: release_dir.to_string_lossy().to_string(),
                routes: vec!["broken.localhost".to_string()],
                env: HashMap::new(),
                instances: 0,
                idle_timeout: 300,
            })
            .await;

        assert!(
            matches!(response, Response::Error { .. }),
            "expected startup validation failure for on-demand deploy: {response:?}"
        );
    }

    #[tokio::test]
    async fn deploy_on_demand_keeps_one_warm_instance_after_successful_deploy() {
        if !python3_ok() {
            return;
        }
        let Some(base_port) = pick_free_port() else {
            return;
        };

        let temp = TempDir::new().unwrap();
        let cert_manager = Arc::new(CertManager::new(CertManagerConfig {
            cert_dir: temp.path().join("certs"),
            ..Default::default()
        }));
        let state = ServerState::new(temp.path().to_path_buf(), cert_manager, None).unwrap();

        let fake_bin_dir = temp.path().join("bin");
        std::fs::create_dir_all(&fake_bin_dir).unwrap();
        let fake_bun = fake_bin_dir.join("bun");
        std::fs::write(
            &fake_bun,
            r#"#!/bin/sh
python3 - <<'PY'
import os
from http.server import BaseHTTPRequestHandler, HTTPServer

class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        self.send_response(200)
        self.send_header("Content-Type", "text/plain")
        self.end_headers()
        self.wfile.write(b"ok")
    def log_message(self, fmt, *args):
        return

HTTPServer(("127.0.0.1", int(os.environ.get("PORT", "3000"))), Handler).serve_forever()
PY
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

        let app = state.app_manager.register_app(AppConfig {
            name: "warm-app".to_string(),
            version: "v0".to_string(),
            path: release_dir.clone(),
            cwd: release_dir.clone(),
            command: vec![
                "/bin/sh".to_string(),
                "-lc".to_string(),
                "exit 0".to_string(),
            ],
            min_instances: 0,
            max_instances: 4,
            base_port,
            ..Default::default()
        });
        state.load_balancer.register_app(app);

        let mut env = HashMap::new();
        let path = std::env::var("PATH").unwrap_or_default();
        env.insert(
            "PATH".to_string(),
            format!("{}:{}", fake_bin_dir.display(), path),
        );

        let response = state
            .handle_command(Command::Deploy {
                app: "warm-app".to_string(),
                version: "v1".to_string(),
                path: release_dir.to_string_lossy().to_string(),
                routes: vec!["warm.localhost".to_string()],
                env,
                instances: 0,
                idle_timeout: 300,
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
        let state = ServerState::new(temp.path().to_path_buf(), cert_manager, None).unwrap();

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
                instance_id: instance.id,
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
        let state = ServerState::new(temp.path().to_path_buf(), cert_manager, None).unwrap();

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
}

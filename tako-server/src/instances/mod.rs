//! Instance lifecycle management
//!
//! Manages app instances - spawning, health checking, and cleanup.

mod health;
mod rolling;
mod spawner;

pub use health::*;
pub use rolling::*;
pub use spawner::*;

use crate::socket::{AppState, InstanceState, InstanceStatus};
use dashmap::DashMap;
use parking_lot::RwLock;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tokio::process::Child;
use tokio::sync::mpsc;

/// Configuration for an app
#[derive(Debug, Clone)]
pub struct AppConfig {
    /// App name
    pub name: String,
    /// Current version
    pub version: String,
    /// Path to app directory
    pub path: PathBuf,
    /// Runtime command (e.g., ["bun", "run", "src/index.ts"])
    pub command: Vec<String>,
    /// Working directory
    pub cwd: PathBuf,
    /// Environment variables
    pub env: HashMap<String, String>,
    /// Minimum instances (0 = on-demand)
    pub min_instances: u32,
    /// Maximum instances
    pub max_instances: u32,
    /// Port to bind (will be passed as PORT env var)
    pub base_port: u16,
    /// Health check path
    pub health_check_path: String,
    /// Health check interval
    pub health_check_interval: Duration,
    /// Startup timeout
    pub startup_timeout: Duration,
    /// Idle timeout (for on-demand scaling)
    pub idle_timeout: Duration,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            name: String::new(),
            version: String::new(),
            path: PathBuf::new(),
            command: vec![],
            cwd: PathBuf::new(),
            env: HashMap::new(),
            min_instances: 1,
            max_instances: 4,
            base_port: 3000,
            health_check_path: "/_tako/status".to_string(),
            health_check_interval: crate::defaults::HEALTH_CHECK_INTERVAL,
            startup_timeout: Duration::from_secs(30),
            idle_timeout: crate::defaults::DEFAULT_IDLE_TIMEOUT,
        }
    }
}

/// A running instance of an app
pub struct Instance {
    /// Unique instance ID
    pub id: u32,
    /// Port the instance is listening on
    pub port: u16,
    /// Build version this instance was launched from
    build_version: String,
    /// Process handle
    process: RwLock<Option<Child>>,
    /// Process ID
    pid: AtomicU32,
    /// Current state
    state: RwLock<InstanceState>,
    /// When the instance started
    started_at: RwLock<Option<Instant>>,
    /// Total requests handled
    requests_total: AtomicU64,

    /// In-flight requests (best-effort; used to avoid killing while serving)
    in_flight: AtomicU64,
    /// Last request time (for idle timeout)
    last_request: RwLock<Instant>,
    /// Last heartbeat time (for health checking)
    last_heartbeat: RwLock<Instant>,
}

impl Instance {
    pub fn new(id: u32, port: u16, build_version: String) -> Self {
        Self {
            id,
            port,
            build_version,
            process: RwLock::new(None),
            pid: AtomicU32::new(0),
            state: RwLock::new(InstanceState::Starting),
            started_at: RwLock::new(None),
            requests_total: AtomicU64::new(0),
            in_flight: AtomicU64::new(0),
            last_request: RwLock::new(Instant::now()),
            last_heartbeat: RwLock::new(Instant::now()),
        }
    }

    pub fn state(&self) -> InstanceState {
        *self.state.read()
    }

    pub fn set_state(&self, state: InstanceState) {
        *self.state.write() = state;
    }

    pub fn pid(&self) -> Option<u32> {
        let pid = self.pid.load(Ordering::Relaxed);
        if pid > 0 { Some(pid) } else { None }
    }

    pub fn build_version(&self) -> &str {
        &self.build_version
    }

    pub fn set_pid(&self, pid: u32) {
        self.pid.store(pid, Ordering::Relaxed);
    }

    pub fn set_process(&self, child: Child) {
        if let Some(pid) = child.id() {
            self.set_pid(pid);
        }
        *self.process.write() = Some(child);
        *self.started_at.write() = Some(Instant::now());
    }

    pub fn take_process(&self) -> Option<Child> {
        self.process.write().take()
    }

    pub fn request_started(&self) {
        self.requests_total.fetch_add(1, Ordering::Relaxed);
        self.in_flight.fetch_add(1, Ordering::Relaxed);
    }

    pub fn request_finished(&self) {
        self.in_flight.fetch_sub(1, Ordering::Relaxed);
        *self.last_request.write() = Instant::now();
    }

    pub fn in_flight(&self) -> u64 {
        self.in_flight.load(Ordering::Relaxed)
    }

    pub fn requests_total(&self) -> u64 {
        self.requests_total.load(Ordering::Relaxed)
    }

    pub fn uptime(&self) -> Duration {
        self.started_at
            .read()
            .map(|t| t.elapsed())
            .unwrap_or_default()
    }

    pub fn idle_time(&self) -> Duration {
        self.last_request.read().elapsed()
    }

    /// Get last heartbeat time
    pub fn last_heartbeat(&self) -> Instant {
        *self.last_heartbeat.read()
    }

    /// Record a heartbeat
    pub fn record_heartbeat(&self) {
        *self.last_heartbeat.write() = Instant::now();
    }

    pub fn status(&self) -> InstanceStatus {
        InstanceStatus {
            id: self.id,
            state: self.state(),
            port: self.port,
            pid: self.pid(),
            uptime_secs: self.uptime().as_secs(),
            requests_total: self.requests_total(),
        }
    }

    /// Check if process is still running
    pub async fn is_alive(&self) -> bool {
        let mut process = self.process.write();
        if let Some(ref mut child) = *process {
            match child.try_wait() {
                Ok(Some(_)) => false, // Process exited
                Ok(None) => true,     // Still running
                Err(_) => false,      // Error checking
            }
        } else {
            false
        }
    }

    /// Kill the process
    pub async fn kill(&self) -> Result<(), std::io::Error> {
        if let Some(mut child) = self.take_process() {
            child.kill().await?;
        }
        self.set_state(InstanceState::Stopped);
        Ok(())
    }
}

/// Manages all instances of an app
pub struct App {
    /// App configuration
    pub config: RwLock<AppConfig>,
    /// Running instances
    instances: DashMap<u32, Arc<Instance>>,
    /// Current app state
    state: RwLock<AppState>,

    /// Most recent error message (if any)
    last_error: RwLock<Option<String>>,
    /// Next instance ID
    next_instance_id: AtomicU32,
    /// Channel to notify about instance changes
    instance_tx: mpsc::Sender<InstanceEvent>,
}

/// Events for instance lifecycle
#[derive(Debug)]
pub enum InstanceEvent {
    Started { app: String, instance_id: u32 },
    Ready { app: String, instance_id: u32 },
    Unhealthy { app: String, instance_id: u32 },
    Stopped { app: String, instance_id: u32 },
}

impl App {
    pub fn new(config: AppConfig, instance_tx: mpsc::Sender<InstanceEvent>) -> Self {
        Self {
            config: RwLock::new(config),
            instances: DashMap::new(),
            state: RwLock::new(AppState::Stopped),
            last_error: RwLock::new(None),
            next_instance_id: AtomicU32::new(1),
            instance_tx,
        }
    }

    pub fn name(&self) -> String {
        self.config.read().name.clone()
    }

    pub fn version(&self) -> String {
        self.config.read().version.clone()
    }

    pub fn state(&self) -> AppState {
        *self.state.read()
    }

    pub fn set_state(&self, state: AppState) {
        *self.state.write() = state;
    }

    pub fn set_last_error(&self, message: impl Into<String>) {
        *self.last_error.write() = Some(message.into());
    }

    pub fn clear_last_error(&self) {
        *self.last_error.write() = None;
    }

    pub fn last_error(&self) -> Option<String> {
        self.last_error.read().clone()
    }

    /// Get a healthy instance for load balancing
    pub fn get_healthy_instance(&self) -> Option<Arc<Instance>> {
        self.instances
            .iter()
            .find(|entry| entry.value().state() == InstanceState::Healthy)
            .map(|entry| entry.value().clone())
    }

    /// Get all healthy instances
    pub fn get_healthy_instances(&self) -> Vec<Arc<Instance>> {
        self.instances
            .iter()
            .filter(|entry| entry.value().state() == InstanceState::Healthy)
            .map(|entry| entry.value().clone())
            .collect()
    }

    /// Get instance by ID
    pub fn get_instance(&self, id: u32) -> Option<Arc<Instance>> {
        self.instances.get(&id).map(|entry| entry.value().clone())
    }

    /// Get all instances
    pub fn get_instances(&self) -> Vec<Arc<Instance>> {
        self.instances
            .iter()
            .map(|entry| entry.value().clone())
            .collect()
    }

    /// Count instances by state
    pub fn count_by_state(&self, state: InstanceState) -> usize {
        self.instances
            .iter()
            .filter(|entry| entry.value().state() == state)
            .count()
    }

    /// Allocate a new instance (doesn't start it yet)
    pub fn allocate_instance(&self) -> Arc<Instance> {
        let id = self.next_instance_id.fetch_add(1, Ordering::Relaxed);
        let config = self.config.read();
        let port = config.base_port + (id - 1) as u16;
        let instance = Arc::new(Instance::new(id, port, config.version.clone()));
        self.instances.insert(id, instance.clone());
        instance
    }

    /// Remove an instance
    pub fn remove_instance(&self, id: u32) -> Option<Arc<Instance>> {
        self.instances.remove(&id).map(|(_, v)| v)
    }

    /// Update configuration (for reloads/deploys)
    pub fn update_config(&self, config: AppConfig) {
        *self.config.write() = config;
    }
}

/// Manages all apps
pub struct AppManager {
    /// All registered apps
    apps: DashMap<String, Arc<App>>,
    /// Instance spawner
    spawner: Arc<Spawner>,
    /// Event channel sender
    event_tx: mpsc::Sender<InstanceEvent>,
    /// Event channel receiver (for the manager loop)
    event_rx: RwLock<Option<mpsc::Receiver<InstanceEvent>>>,
}

impl AppManager {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::channel(1024);
        Self {
            apps: DashMap::new(),
            spawner: Arc::new(Spawner::new()),
            event_tx: tx,
            event_rx: RwLock::new(Some(rx)),
        }
    }

    /// Take the event receiver (can only be called once)
    pub fn take_event_receiver(&self) -> Option<mpsc::Receiver<InstanceEvent>> {
        self.event_rx.write().take()
    }

    /// Register a new app
    pub fn register_app(&self, config: AppConfig) -> Arc<App> {
        let name = config.name.clone();
        let app = Arc::new(App::new(config, self.event_tx.clone()));
        self.apps.insert(name, app.clone());
        app
    }

    /// Get an app by name
    pub fn get_app(&self, name: &str) -> Option<Arc<App>> {
        self.apps.get(name).map(|entry| entry.value().clone())
    }

    /// Remove an app
    pub fn remove_app(&self, name: &str) -> Option<Arc<App>> {
        self.apps.remove(name).map(|(_, v)| v)
    }

    /// List all app names
    pub fn list_apps(&self) -> Vec<String> {
        self.apps.iter().map(|entry| entry.key().clone()).collect()
    }

    /// Start an app (spawn minimum instances)
    pub async fn start_app(&self, name: &str) -> Result<(), InstanceError> {
        let app = self
            .get_app(name)
            .ok_or_else(|| InstanceError::AppNotFound(name.to_string()))?;

        let min_instances = app.config.read().min_instances;
        app.set_state(AppState::Running);

        for _ in 0..min_instances {
            let instance = app.allocate_instance();
            self.spawner.spawn(&app, instance).await?;
        }

        Ok(())
    }

    /// Stop an app (kill all instances)
    pub async fn stop_app(&self, name: &str) -> Result<(), InstanceError> {
        let app = self
            .get_app(name)
            .ok_or_else(|| InstanceError::AppNotFound(name.to_string()))?;

        app.set_state(AppState::Stopped);

        // Kill all instances
        let instances = app.get_instances();
        for instance in instances {
            instance.kill().await?;
            app.remove_instance(instance.id);
        }

        Ok(())
    }

    /// Get spawner for external use
    pub fn spawner(&self) -> Arc<Spawner> {
        self.spawner.clone()
    }
}

impl Default for AppManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Errors that can occur during instance management
#[derive(Debug, thiserror::Error)]
pub enum InstanceError {
    #[error("App not found: {0}")]
    AppNotFound(String),

    #[error("Failed to spawn instance: {0}")]
    SpawnError(#[from] std::io::Error),

    #[error("Instance startup timeout")]
    StartupTimeout,

    #[error("Health check failed: {0}")]
    HealthCheckFailed(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_instance_state_transitions() {
        let instance = Instance::new(1, 3000, "v1".to_string());
        assert_eq!(instance.state(), InstanceState::Starting);

        instance.set_state(InstanceState::Ready);
        assert_eq!(instance.state(), InstanceState::Ready);

        instance.set_state(InstanceState::Healthy);
        assert_eq!(instance.state(), InstanceState::Healthy);
    }

    #[test]
    fn test_instance_request_tracking() {
        let instance = Instance::new(1, 3000, "v1".to_string());
        assert_eq!(instance.requests_total(), 0);

        instance.request_started();
        instance.request_finished();
        instance.request_started();
        instance.request_finished();
        instance.request_started();
        instance.request_finished();

        assert_eq!(instance.requests_total(), 3);
    }

    #[test]
    fn test_app_allocate_instances() {
        let (tx, _rx) = mpsc::channel(16);
        let config = AppConfig {
            name: "test-app".to_string(),
            version: "v1".to_string(),
            base_port: 3000,
            ..Default::default()
        };
        let app = App::new(config, tx);

        let i1 = app.allocate_instance();
        assert_eq!(i1.id, 1);
        assert_eq!(i1.port, 3000);

        let i2 = app.allocate_instance();
        assert_eq!(i2.id, 2);
        assert_eq!(i2.port, 3001);

        let i3 = app.allocate_instance();
        assert_eq!(i3.id, 3);
        assert_eq!(i3.port, 3002);
    }

    #[test]
    fn test_allocate_instance_tracks_build_version() {
        let (tx, _rx) = mpsc::channel(16);
        let config = AppConfig {
            name: "test-app".to_string(),
            version: "v1".to_string(),
            base_port: 3000,
            ..Default::default()
        };
        let app = App::new(config, tx);

        let v1_instance = app.allocate_instance();
        assert_eq!(v1_instance.build_version(), "v1");

        let mut next = app.config.read().clone();
        next.version = "v2".to_string();
        app.update_config(next);

        let v2_instance = app.allocate_instance();
        assert_eq!(v2_instance.build_version(), "v2");
    }

    #[test]
    fn test_app_manager_register() {
        let manager = AppManager::new();

        let config = AppConfig {
            name: "my-app".to_string(),
            version: "1.0.0".to_string(),
            ..Default::default()
        };

        manager.register_app(config);

        let app = manager.get_app("my-app").unwrap();
        assert_eq!(app.name(), "my-app");
        assert_eq!(app.version(), "1.0.0");

        let apps = manager.list_apps();
        assert_eq!(apps.len(), 1);
        assert!(apps.contains(&"my-app".to_string()));
    }

    #[test]
    fn test_get_healthy_instances() {
        let (tx, _rx) = mpsc::channel(16);
        let config = AppConfig {
            name: "test-app".to_string(),
            base_port: 3000,
            ..Default::default()
        };
        let app = App::new(config, tx);

        let i1 = app.allocate_instance();
        let i2 = app.allocate_instance();
        let i3 = app.allocate_instance();

        i1.set_state(InstanceState::Healthy);
        i2.set_state(InstanceState::Starting);
        i3.set_state(InstanceState::Healthy);

        let healthy = app.get_healthy_instances();
        assert_eq!(healthy.len(), 2);
    }

    #[test]
    fn app_last_error_roundtrip() {
        let (tx, _rx) = mpsc::channel(1);
        let app = App::new(AppConfig::default(), tx);
        assert_eq!(app.last_error(), None);

        app.set_last_error("boom");
        assert_eq!(app.last_error(), Some("boom".to_string()));

        app.clear_last_error();
        assert_eq!(app.last_error(), None);
    }
}

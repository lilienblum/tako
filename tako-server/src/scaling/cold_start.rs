//! Cold start handler - manages request queuing during instance startup

use parking_lot::Mutex;
use std::collections::HashMap;
use std::time::{Duration, Instant};
use tokio::sync::broadcast;

/// Configuration for cold start handling
#[derive(Debug, Clone)]
pub struct ColdStartConfig {
    /// Maximum time to wait for an instance to start
    pub startup_timeout: Duration,
    /// Maximum number of requests to queue during cold start
    pub max_queued_requests: usize,
}

impl Default for ColdStartConfig {
    fn default() -> Self {
        Self {
            startup_timeout: Duration::from_secs(30),
            max_queued_requests: 100,
        }
    }
}

/// State of a cold start for an app
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColdStartState {
    /// No cold start in progress
    Idle,
    /// Instance is starting
    Starting,
    /// Instance is ready
    Ready,
    /// Cold start failed
    Failed,
}

/// Tracks cold start state for an app
struct AppColdStart {
    /// Current state
    state: ColdStartState,
    /// When the cold start began
    started_at: Option<Instant>,
    /// Channel to notify waiters when ready
    ready_tx: Option<broadcast::Sender<bool>>,
}

impl AppColdStart {
    fn new() -> Self {
        Self {
            state: ColdStartState::Idle,
            started_at: None,
            ready_tx: None,
        }
    }
}

/// Manages cold starts for all apps
pub struct ColdStartManager {
    config: ColdStartConfig,
    /// Per-app cold start state
    apps: Mutex<HashMap<String, AppColdStart>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ColdStartBegin {
    pub leader: bool,
}

impl ColdStartManager {
    pub fn new(config: ColdStartConfig) -> Self {
        Self {
            config,
            apps: Mutex::new(HashMap::new()),
        }
    }

    /// Check if an app is currently in cold start
    pub fn is_cold_starting(&self, app_name: &str) -> bool {
        let apps = self.apps.lock();
        apps.get(app_name)
            .map(|cs| cs.state == ColdStartState::Starting)
            .unwrap_or(false)
    }

    /// Begin a cold start for an app.
    ///
    /// Returns whether the caller is responsible for actually spawning the instance.
    pub fn begin(&self, app_name: &str) -> ColdStartBegin {
        let mut apps = self.apps.lock();
        let cold_start = apps
            .entry(app_name.to_string())
            .or_insert_with(AppColdStart::new);

        match cold_start.state {
            ColdStartState::Idle | ColdStartState::Failed => {
                let (tx, _rx) = broadcast::channel(1);
                cold_start.state = ColdStartState::Starting;
                cold_start.started_at = Some(Instant::now());
                cold_start.ready_tx = Some(tx);
                ColdStartBegin { leader: true }
            }
            ColdStartState::Starting | ColdStartState::Ready => ColdStartBegin { leader: false },
        }
    }

    /// Wait for a cold start to complete
    /// Returns true if instance is ready, false if failed/timeout
    pub async fn wait_for_ready(&self, app_name: &str) -> bool {
        let rx = {
            let apps = self.apps.lock();
            match apps.get(app_name) {
                Some(cs) if cs.state == ColdStartState::Ready => return true,
                Some(cs) if cs.state == ColdStartState::Starting => {
                    cs.ready_tx.as_ref().map(|tx| tx.subscribe())
                }
                _ => return false,
            }
        };

        if let Some(mut rx) = rx {
            matches!(
                tokio::time::timeout(self.config.startup_timeout, rx.recv()).await,
                Ok(Ok(true))
            )
        } else {
            false
        }
    }

    /// Mark cold start as complete (instance is ready)
    pub fn mark_ready(&self, app_name: &str) {
        let mut apps = self.apps.lock();
        if let Some(cold_start) = apps.get_mut(app_name) {
            cold_start.state = ColdStartState::Ready;
            if let Some(tx) = cold_start.ready_tx.take() {
                let _ = tx.send(true);
            }
        }
    }

    /// Mark cold start as failed
    pub fn mark_failed(&self, app_name: &str) {
        let mut apps = self.apps.lock();
        if let Some(cold_start) = apps.get_mut(app_name) {
            cold_start.state = ColdStartState::Failed;
            if let Some(tx) = cold_start.ready_tx.take() {
                let _ = tx.send(false);
            }
        }
    }

    /// Reset cold start state (e.g., when app is stopped)
    pub fn reset(&self, app_name: &str) {
        let mut apps = self.apps.lock();
        apps.remove(app_name);
    }

    /// Get elapsed time since cold start began
    pub fn elapsed(&self, app_name: &str) -> Option<Duration> {
        let apps = self.apps.lock();
        apps.get(app_name)
            .and_then(|cs| cs.started_at.map(|t| t.elapsed()))
    }
}

/// Result of trying to get an instance for a request
#[derive(Debug)]
pub enum InstanceResult {
    /// Instance is available immediately
    Available,
    /// Need to wait for cold start
    ColdStart,
    /// App not found
    NotFound,
    /// Cold start failed
    Failed,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn test_cold_start_config_defaults() {
        let config = ColdStartConfig::default();
        assert_eq!(config.startup_timeout, Duration::from_secs(30));
        assert_eq!(config.max_queued_requests, 100);
    }

    #[test]
    fn test_cold_start_manager_creation() {
        let manager = ColdStartManager::new(ColdStartConfig::default());
        assert!(!manager.is_cold_starting("my-app"));
    }

    #[test]
    fn test_begin_cold_start() {
        let manager = ColdStartManager::new(ColdStartConfig::default());

        // First call should be leader
        let r1 = manager.begin("my-app");
        assert!(r1.leader);
        assert!(manager.is_cold_starting("my-app"));

        // Second call should not be leader
        let r2 = manager.begin("my-app");
        assert!(!r2.leader);
    }

    #[test]
    fn test_mark_ready() {
        let manager = ColdStartManager::new(ColdStartConfig::default());

        manager.begin("my-app");
        assert!(manager.is_cold_starting("my-app"));

        manager.mark_ready("my-app");
        assert!(!manager.is_cold_starting("my-app"));
    }

    #[test]
    fn test_mark_failed() {
        let manager = ColdStartManager::new(ColdStartConfig::default());

        manager.begin("my-app");
        manager.mark_failed("my-app");

        assert!(!manager.is_cold_starting("my-app"));
    }

    #[test]
    fn test_reset() {
        let manager = ColdStartManager::new(ColdStartConfig::default());

        manager.begin("my-app");
        manager.reset("my-app");

        assert!(!manager.is_cold_starting("my-app"));
    }

    #[tokio::test]
    async fn test_wait_for_ready_immediate() {
        let manager = Arc::new(ColdStartManager::new(ColdStartConfig::default()));

        manager.begin("my-app");
        manager.mark_ready("my-app");

        // Should return immediately since already ready
        let result = manager.wait_for_ready("my-app").await;
        assert!(result);
    }

    #[tokio::test]
    async fn test_wait_for_ready_with_delay() {
        let manager = Arc::new(ColdStartManager::new(ColdStartConfig::default()));

        manager.begin("my-app");

        let manager_clone = manager.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            manager_clone.mark_ready("my-app");
        });

        let result = manager.wait_for_ready("my-app").await;
        assert!(result);
    }
}

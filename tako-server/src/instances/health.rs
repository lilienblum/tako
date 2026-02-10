//! Health checker - monitors instance health via HTTP probing
//!
//! Performs active HTTP health checks to `/_tako/status` endpoint on each instance.
//! This replaces passive heartbeat-only detection with active probing.

use super::{App, Instance, InstanceState};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::interval;

/// Health check configuration
#[derive(Debug, Clone)]
pub struct HealthConfig {
    /// Interval between health checks
    pub check_interval: Duration,
    /// Number of consecutive failures before marking unhealthy
    pub unhealthy_threshold: u32,
    /// Number of consecutive failures before marking dead
    pub dead_threshold: u32,
    /// Timeout for individual health check requests
    pub probe_timeout: Duration,
}

impl Default for HealthConfig {
    fn default() -> Self {
        Self {
            check_interval: crate::defaults::HEALTH_CHECK_INTERVAL,
            unhealthy_threshold: 2, // 2 failures = unhealthy
            dead_threshold: 5,      // 5 failures = dead
            probe_timeout: crate::defaults::HEALTH_PROBE_TIMEOUT,
        }
    }
}

/// Health check events
#[derive(Debug, Clone)]
pub enum HealthEvent {
    /// Instance became healthy
    Healthy { app: String, instance_id: u32 },
    /// Instance became unhealthy
    Unhealthy { app: String, instance_id: u32 },
    /// Instance is dead (no heartbeat for too long)
    Dead { app: String, instance_id: u32 },
    /// Instance recovered from unhealthy
    Recovered { app: String, instance_id: u32 },
}

/// Tracks consecutive health check failures per instance
use dashmap::DashMap;

/// Health checker for monitoring instance health via HTTP probing
pub struct HealthChecker {
    config: HealthConfig,
    event_tx: mpsc::Sender<HealthEvent>,
    /// HTTP client for health probes
    client: reqwest::Client,
    /// Consecutive failure counts per instance (app_name:instance_id -> count)
    failure_counts: DashMap<String, u32>,
}

impl HealthChecker {
    pub fn new(config: HealthConfig, event_tx: mpsc::Sender<HealthEvent>) -> Self {
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(config.probe_timeout)
            .build()
            .expect("Failed to build HTTP client for health checks");

        Self {
            config,
            event_tx,
            client,
            failure_counts: DashMap::new(),
        }
    }

    /// Start health check loop for an app
    pub async fn monitor_app(&self, app: Arc<App>) {
        let mut check_interval = interval(self.config.check_interval);

        loop {
            check_interval.tick().await;

            let instances = app.get_instances();
            for instance in instances {
                self.check_instance(&app, &instance).await;
            }
        }
    }

    /// Check health of a single instance via HTTP probe
    async fn check_instance(&self, app: &App, instance: &Instance) {
        let current_state = instance.state();

        // Skip instances that are starting, draining, or already stopped
        if matches!(
            current_state,
            InstanceState::Starting | InstanceState::Draining | InstanceState::Stopped
        ) {
            return;
        }

        // Build health check URL using app's configured health check path
        let health_path = app.config.read().health_check_path.clone();
        let health_url = format!("http://127.0.0.1:{}{}", instance.port, health_path);
        let instance_key = format!("{}:{}", app.name(), instance.id);

        // Perform HTTP probe
        let probe_success = match self.client.get(&health_url).send().await {
            Ok(resp) => resp.status().is_success(),
            Err(_) => false,
        };

        if probe_success {
            // Reset failure count and record heartbeat
            self.failure_counts.remove(&instance_key);
            instance.record_heartbeat();

            // Mark healthy on first successful probe.
            if current_state != InstanceState::Healthy {
                instance.set_state(InstanceState::Healthy);

                let event = if current_state == InstanceState::Unhealthy {
                    HealthEvent::Recovered {
                        app: app.name(),
                        instance_id: instance.id,
                    }
                } else {
                    HealthEvent::Healthy {
                        app: app.name(),
                        instance_id: instance.id,
                    }
                };
                let _ = self.event_tx.send(event).await;
            }
        } else {
            // Increment failure count
            let mut failures = self.failure_counts.entry(instance_key.clone()).or_insert(0);
            *failures += 1;
            let failure_count = *failures;

            tracing::debug!(
                app = %app.name(),
                instance = instance.id,
                failures = failure_count,
                url = %health_url,
                "Health check failed"
            );

            // Determine new state based on failure count
            let new_state = if failure_count >= self.config.dead_threshold {
                InstanceState::Stopped
            } else if failure_count >= self.config.unhealthy_threshold {
                InstanceState::Unhealthy
            } else {
                current_state
            };

            if new_state != current_state {
                instance.set_state(new_state);

                let event = match new_state {
                    InstanceState::Unhealthy => {
                        tracing::warn!(
                            app = %app.name(),
                            instance = instance.id,
                            failures = failure_count,
                            "Instance marked unhealthy"
                        );
                        Some(HealthEvent::Unhealthy {
                            app: app.name(),
                            instance_id: instance.id,
                        })
                    }
                    InstanceState::Stopped => {
                        tracing::error!(
                            app = %app.name(),
                            instance = instance.id,
                            failures = failure_count,
                            "Instance marked dead after {} consecutive failures",
                            failure_count
                        );
                        Some(HealthEvent::Dead {
                            app: app.name(),
                            instance_id: instance.id,
                        })
                    }
                    _ => None,
                };

                if let Some(event) = event {
                    let _ = self.event_tx.send(event).await;
                }
            }
        }
    }

    /// Get current failure count for an instance
    pub fn get_failure_count(&self, app_name: &str, instance_id: u32) -> u32 {
        let key = format!("{}:{}", app_name, instance_id);
        self.failure_counts.get(&key).map(|v| *v).unwrap_or(0)
    }

    /// Clear failure count for an instance (e.g., after restart)
    pub fn clear_failure_count(&self, app_name: &str, instance_id: u32) {
        let key = format!("{}:{}", app_name, instance_id);
        self.failure_counts.remove(&key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::instances::AppConfig;
    use tokio::sync::mpsc;
    use tokio::time::timeout;

    fn create_test_app() -> Arc<App> {
        let (tx, _rx) = mpsc::channel(16);
        let config = AppConfig {
            name: "test-app".to_string(),
            base_port: 3000,
            ..Default::default()
        };
        Arc::new(App::new(config, tx))
    }

    #[test]
    fn test_health_config_defaults() {
        let config = HealthConfig::default();
        assert_eq!(
            config.check_interval,
            crate::defaults::HEALTH_CHECK_INTERVAL
        );
        assert_eq!(config.unhealthy_threshold, 2);
        assert_eq!(config.dead_threshold, 5);
        assert_eq!(config.probe_timeout, crate::defaults::HEALTH_PROBE_TIMEOUT);
    }

    #[tokio::test]
    async fn test_health_checker_creation() {
        let (tx, _rx) = mpsc::channel(16);
        let config = HealthConfig::default();
        let checker = HealthChecker::new(config, tx);

        // Verify failure counts start empty
        assert_eq!(checker.get_failure_count("test-app", 1), 0);
    }

    #[tokio::test]
    async fn test_health_checker_failure_tracking() {
        let (tx, _rx) = mpsc::channel(16);
        let config = HealthConfig::default();
        let checker = HealthChecker::new(config, tx);

        // Simulate failure count increment (this would normally happen in check_instance)
        let key = "test-app:1".to_string();
        checker.failure_counts.insert(key.clone(), 3);

        assert_eq!(checker.get_failure_count("test-app", 1), 3);

        // Clear and verify
        checker.clear_failure_count("test-app", 1);
        assert_eq!(checker.get_failure_count("test-app", 1), 0);
    }

    #[tokio::test]
    async fn test_health_checker_skips_non_running_instances() {
        let (tx, mut rx) = mpsc::channel(16);
        let config = HealthConfig::default();
        let checker = HealthChecker::new(config, tx);

        let app = create_test_app();
        let instance = app.allocate_instance();

        // Instance in Starting state should be skipped
        instance.set_state(InstanceState::Starting);
        checker.check_instance(&app, &instance).await;

        // No events should be emitted
        assert!(rx.try_recv().is_err());

        // Instance in Draining state should be skipped
        instance.set_state(InstanceState::Draining);
        checker.check_instance(&app, &instance).await;
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn test_health_event_types() {
        let healthy = HealthEvent::Healthy {
            app: "test".to_string(),
            instance_id: 1,
        };
        let unhealthy = HealthEvent::Unhealthy {
            app: "test".to_string(),
            instance_id: 1,
        };
        let dead = HealthEvent::Dead {
            app: "test".to_string(),
            instance_id: 1,
        };
        let recovered = HealthEvent::Recovered {
            app: "test".to_string(),
            instance_id: 1,
        };

        // Just verify they can be created and formatted
        assert!(format!("{:?}", healthy).contains("Healthy"));
        assert!(format!("{:?}", unhealthy).contains("Unhealthy"));
        assert!(format!("{:?}", dead).contains("Dead"));
        assert!(format!("{:?}", recovered).contains("Recovered"));
    }

    #[tokio::test]
    async fn test_probe_marks_ready_instance_healthy() {
        let Ok(listener) = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await else {
            return;
        };
        let port = listener.local_addr().unwrap().port();

        tokio::spawn(async move {
            loop {
                let (mut sock, _) = match listener.accept().await {
                    Ok(v) => v,
                    Err(_) => break,
                };
                // Always return 200 OK.
                let _ = tokio::io::AsyncWriteExt::write_all(
                    &mut sock,
                    b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok",
                )
                .await;
            }
        });

        let (tx, _rx) = mpsc::channel(16);
        let mut config = AppConfig {
            name: "test-app".to_string(),
            base_port: port,
            min_instances: 1,
            ..Default::default()
        };
        config.health_check_path = "/_tako/status".to_string();

        let app = Arc::new(App::new(config, tx));
        let instance = app.allocate_instance();
        instance.set_state(InstanceState::Ready);

        let (ev_tx, _ev_rx) = mpsc::channel(16);
        let checker = HealthChecker::new(
            HealthConfig {
                check_interval: Duration::from_millis(25),
                probe_timeout: Duration::from_millis(200),
                ..Default::default()
            },
            ev_tx,
        );

        let app_task = tokio::spawn(async move {
            checker.monitor_app(app).await;
        });

        let wait = async {
            loop {
                if instance.state() == InstanceState::Healthy {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        };

        timeout(Duration::from_secs(1), wait)
            .await
            .expect("instance never became healthy");

        app_task.abort();
    }
}

//! Health checker - monitors instance health via HTTP probing
//!
//! Performs active HTTP health checks to internal host `tako` at `/status` on each
//! instance.
//! This replaces passive heartbeat-only detection with active probing.

use super::{App, INTERNAL_TOKEN_HEADER, Instance, InstanceState};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::{interval, timeout};

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
    /// Maximum concurrent probe tasks per app per cycle
    pub max_probe_concurrency: usize,
}

impl Default for HealthConfig {
    fn default() -> Self {
        Self {
            check_interval: crate::defaults::HEALTH_CHECK_INTERVAL,
            unhealthy_threshold: 2, // 2 failures = unhealthy
            dead_threshold: 5,      // 5 failures = dead
            probe_timeout: crate::defaults::HEALTH_PROBE_TIMEOUT,
            max_probe_concurrency: 16,
        }
    }
}

/// Health check events
#[derive(Debug, Clone)]
pub enum HealthEvent {
    /// Instance became healthy
    Healthy { app: String, instance_id: String },
    /// Instance became unhealthy
    Unhealthy { app: String, instance_id: String },
    /// Instance is dead (no heartbeat for too long)
    Dead { app: String, instance_id: String },
    /// Instance recovered from unhealthy
    Recovered { app: String, instance_id: String },
}

/// Tracks consecutive health check failures per instance
use dashmap::DashMap;

/// Health checker for monitoring instance health via HTTP probing
#[derive(Clone)]
pub struct HealthChecker {
    config: HealthConfig,
    event_tx: mpsc::Sender<HealthEvent>,
    /// Consecutive failure counts per instance (app_name:instance_id -> count)
    failure_counts: Arc<DashMap<String, u32>>,
}

impl HealthChecker {
    pub fn new(config: HealthConfig, event_tx: mpsc::Sender<HealthEvent>) -> Self {
        Self {
            config,
            event_tx,
            failure_counts: Arc::new(DashMap::new()),
        }
    }

    fn effective_probe_concurrency(value: usize) -> usize {
        value.max(1)
    }

    /// Start health check loop for an app
    pub async fn monitor_app(&self, app: Arc<App>) {
        let mut check_interval = interval(self.config.check_interval);
        let concurrency = Self::effective_probe_concurrency(self.config.max_probe_concurrency);
        let semaphore = Arc::new(tokio::sync::Semaphore::new(concurrency));

        loop {
            check_interval.tick().await;

            let instances = app.get_instances();
            let mut checks = tokio::task::JoinSet::new();

            for instance in instances {
                let permit = match semaphore.clone().acquire_owned().await {
                    Ok(permit) => permit,
                    Err(_) => break,
                };

                let checker = self.clone();
                let app = app.clone();
                checks.spawn(async move {
                    checker.check_instance(&app, &instance).await;
                    drop(permit);
                });
            }

            while checks.join_next().await.is_some() {}
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

        // Build health check target using app's configured path and internal host header
        let (health_host, health_path) = {
            let config = app.config.read();
            (
                config.health_check_host.clone(),
                config.health_check_path.clone(),
            )
        };
        let instance_key = format!("{}:{}", app.name(), instance.id);

        // Perform HTTP probe
        let probe_success = probe_instance_health(
            instance,
            &health_host,
            &health_path,
            self.config.probe_timeout,
        )
        .await;

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
                        instance_id: instance.id.clone(),
                    }
                } else {
                    HealthEvent::Healthy {
                        app: app.name(),
                        instance_id: instance.id.clone(),
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
                instance = %instance.id,
                failures = failure_count,
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
                            instance = %instance.id,
                            failures = failure_count,
                            "Instance marked unhealthy"
                        );
                        Some(HealthEvent::Unhealthy {
                            app: app.name(),
                            instance_id: instance.id.clone(),
                        })
                    }
                    InstanceState::Stopped => {
                        tracing::error!(
                            app = %app.name(),
                            instance = %instance.id,
                            failures = failure_count,
                            "Instance marked dead after {} consecutive failures",
                            failure_count
                        );
                        Some(HealthEvent::Dead {
                            app: app.name(),
                            instance_id: instance.id.clone(),
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
    pub fn get_failure_count(&self, app_name: &str, instance_id: &str) -> u32 {
        let key = format!("{}:{}", app_name, instance_id);
        self.failure_counts.get(&key).map(|v| *v).unwrap_or(0)
    }

    /// Clear failure count for an instance (e.g., after restart)
    pub fn clear_failure_count(&self, app_name: &str, instance_id: &str) {
        let key = format!("{}:{}", app_name, instance_id);
        self.failure_counts.remove(&key);
    }
}

async fn probe_instance_health(
    instance: &Instance,
    health_host: &str,
    health_path: &str,
    probe_timeout: Duration,
) -> bool {
    let Some(endpoint) = instance.endpoint() else {
        return false;
    };
    matches!(
        probe_endpoint_tcp(
            endpoint,
            health_host,
            health_path,
            instance.internal_token(),
            probe_timeout,
        )
        .await,
        Ok(true)
    )
}

async fn probe_endpoint_tcp(
    endpoint: std::net::SocketAddr,
    health_host: &str,
    health_path: &str,
    internal_token: &str,
    probe_timeout: Duration,
) -> Result<bool, std::io::Error> {
    use tokio::io::AsyncWriteExt;

    let mut socket = match timeout(probe_timeout, tokio::net::TcpStream::connect(endpoint)).await {
        Ok(result) => result?,
        Err(_) => return Ok(false),
    };
    let request = format!(
        "GET {health_path} HTTP/1.1\r\nHost: {health_host}\r\n{INTERNAL_TOKEN_HEADER}: {internal_token}\r\nConnection: close\r\n\r\n"
    );
    match timeout(probe_timeout, socket.write_all(request.as_bytes())).await {
        Ok(result) => result?,
        Err(_) => return Ok(false),
    }

    let Some(response) = read_http_response_headers(&mut socket, probe_timeout).await? else {
        return Ok(false);
    };
    Ok(http_response_is_internal_success(&response, internal_token))
}

async fn read_http_response_headers(
    socket: &mut tokio::net::TcpStream,
    io_timeout: Duration,
) -> Result<Option<String>, std::io::Error> {
    use tokio::io::AsyncReadExt;

    let mut response = Vec::with_capacity(1024);
    let mut chunk = [0_u8; 1024];

    loop {
        let bytes_read = match timeout(io_timeout, socket.read(&mut chunk)).await {
            Ok(result) => result?,
            Err(_) => return Ok(None),
        };

        if bytes_read == 0 {
            break;
        }

        response.extend_from_slice(&chunk[..bytes_read]);
        if response.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
    }

    if response.is_empty() {
        return Ok(None);
    }

    Ok(Some(String::from_utf8_lossy(&response).into_owned()))
}

fn http_status_is_success(status_line: &str) -> bool {
    let mut parts = status_line.split_whitespace();
    let Some(http_version) = parts.next() else {
        return false;
    };
    if !http_version.starts_with("HTTP/") {
        return false;
    }
    parts
        .next()
        .and_then(|code| code.parse::<u16>().ok())
        .map(|code| (200..300).contains(&code))
        .unwrap_or(false)
}

fn http_response_is_internal_success(response: &str, expected_token: &str) -> bool {
    let mut lines = response.lines();
    let status_line = lines.next().unwrap_or_default();
    if !http_status_is_success(status_line) {
        return false;
    }

    lines
        .take_while(|line| !line.is_empty())
        .filter_map(|line| line.split_once(':'))
        .any(|(name, value)| {
            name.eq_ignore_ascii_case(INTERNAL_TOKEN_HEADER) && value.trim() == expected_token
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::instances::AppConfig;
    use tokio::sync::mpsc;

    fn create_test_app() -> Arc<App> {
        let (tx, _rx) = mpsc::channel(16);
        let config = AppConfig {
            name: "test-app".to_string(),
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
        assert_eq!(config.max_probe_concurrency, 16);
    }

    #[test]
    fn test_effective_probe_concurrency_never_zero() {
        assert_eq!(HealthChecker::effective_probe_concurrency(0), 1);
        assert_eq!(HealthChecker::effective_probe_concurrency(7), 7);
    }

    #[tokio::test]
    async fn test_health_checker_creation() {
        let (tx, _rx) = mpsc::channel(16);
        let config = HealthConfig::default();
        let checker = HealthChecker::new(config, tx);

        // Verify failure counts start empty
        assert_eq!(checker.get_failure_count("test-app", "1"), 0);
    }

    #[tokio::test]
    async fn test_health_checker_failure_tracking() {
        let (tx, _rx) = mpsc::channel(16);
        let config = HealthConfig::default();
        let checker = HealthChecker::new(config, tx);

        // Simulate failure count increment (this would normally happen in check_instance)
        let key = "test-app:1".to_string();
        checker.failure_counts.insert(key.clone(), 3);

        assert_eq!(checker.get_failure_count("test-app", "1"), 3);

        // Clear and verify
        checker.clear_failure_count("test-app", "1");
        assert_eq!(checker.get_failure_count("test-app", "1"), 0);
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
            instance_id: "abc123".to_string(),
        };
        let unhealthy = HealthEvent::Unhealthy {
            app: "test".to_string(),
            instance_id: "abc123".to_string(),
        };
        let dead = HealthEvent::Dead {
            app: "test".to_string(),
            instance_id: "abc123".to_string(),
        };
        let recovered = HealthEvent::Recovered {
            app: "test".to_string(),
            instance_id: "abc123".to_string(),
        };

        // Just verify they can be created and formatted
        assert!(format!("{:?}", healthy).contains("Healthy"));
        assert!(format!("{:?}", unhealthy).contains("Unhealthy"));
        assert!(format!("{:?}", dead).contains("Dead"));
        assert!(format!("{:?}", recovered).contains("Recovered"));
    }

    #[tokio::test]
    async fn test_probe_uses_tcp_when_port_is_configured() {
        let Ok(listener) = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await else {
            return;
        };
        let port = listener.local_addr().expect("listener addr").port();

        let (tx, _rx) = mpsc::channel(16);
        let config = AppConfig {
            name: "test-app".to_string(),
            min_instances: 1,
            ..Default::default()
        };
        let app = App::new(config, tx);
        let instance = app.allocate_instance();
        instance.set_port(port);
        let token = instance.internal_token().to_string();

        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept");
            let mut request_buf = [0_u8; 2048];
            let n = tokio::io::AsyncReadExt::read(&mut socket, &mut request_buf)
                .await
                .expect("read request");
            let request = String::from_utf8_lossy(&request_buf[..n]);
            let is_internal_status = request.starts_with("GET /status ")
                && request
                    .lines()
                    .any(|line| line.eq_ignore_ascii_case("host: tako"));
            let has_token = request.lines().any(|line| {
                line.eq_ignore_ascii_case(&format!("{INTERNAL_TOKEN_HEADER}: {token}"))
            });

            let response = if is_internal_status && has_token {
                format!(
                    "HTTP/1.1 200 OK\r\n{INTERNAL_TOKEN_HEADER}: {token}\r\nContent-Length: 2\r\n\r\nok"
                )
            } else {
                "HTTP/1.1 404 Not Found\r\nContent-Length: 9\r\n\r\nnot found".to_string()
            };

            let _ = tokio::io::AsyncWriteExt::write_all(&mut socket, response.as_bytes()).await;
        });

        let healthy =
            probe_instance_health(&instance, "tako", "/status", Duration::from_millis(200)).await;
        assert!(healthy);
    }

    #[tokio::test]
    async fn test_probe_reads_split_response_headers() {
        let Ok(listener) = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await else {
            return;
        };
        let port = listener.local_addr().expect("listener addr").port();

        let (tx, _rx) = mpsc::channel(16);
        let config = AppConfig {
            name: "test-app".to_string(),
            min_instances: 1,
            ..Default::default()
        };
        let app = App::new(config, tx);
        let instance = app.allocate_instance();
        instance.set_port(port);
        let token = instance.internal_token().to_string();

        tokio::spawn(async move {
            use tokio::io::AsyncWriteExt;
            let (mut socket, _) = listener.accept().await.expect("accept");
            let mut request_buf = [0_u8; 2048];
            let n = tokio::io::AsyncReadExt::read(&mut socket, &mut request_buf)
                .await
                .expect("read request");
            let request = String::from_utf8_lossy(&request_buf[..n]);
            let is_internal_status = request.starts_with("GET /status ")
                && request
                    .lines()
                    .any(|line| line.eq_ignore_ascii_case("host: tako"));
            let has_token = request.lines().any(|line| {
                line.eq_ignore_ascii_case(&format!("{INTERNAL_TOKEN_HEADER}: {token}"))
            });

            if is_internal_status && has_token {
                socket
                    .write_all(b"HTTP/1.1 200 OK\r\nX-Tako-Internal-Token: ")
                    .await
                    .expect("write response prefix");
                tokio::time::sleep(Duration::from_millis(10)).await;
                socket
                    .write_all(format!("{token}\r\nContent-Length: 2\r\n\r\nok").as_bytes())
                    .await
                    .expect("write response suffix");
            } else {
                socket
                    .write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 9\r\n\r\nnot found")
                    .await
                    .expect("write not found");
            }
        });

        let healthy =
            probe_instance_health(&instance, "tako", "/status", Duration::from_millis(200)).await;
        assert!(healthy);
    }
}

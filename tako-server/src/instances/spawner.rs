//! Instance spawner - spawns and monitors app processes

use super::{App, Instance, InstanceError, InstanceEvent, InstanceState};
use std::process::ExitStatus;
use std::sync::Arc;
use std::time::Duration;
use tokio::process::Command;
use tokio::time::timeout;

/// Spawns and monitors app instances
pub struct Spawner {
    /// HTTP client for health checks
    client: reqwest::Client,
}

impl Spawner {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::builder()
                .no_proxy()
                .timeout(Duration::from_secs(5))
                .build()
                .expect("Failed to build HTTP client"),
        }
    }

    /// Spawn a new instance
    pub async fn spawn(&self, app: &App, instance: Arc<Instance>) -> Result<(), InstanceError> {
        let config = app.config.read().clone();
        let app_name = config.name.clone();
        let instance_id = instance.id;

        tracing::info!(
            app = %app_name,
            instance = instance_id,
            port = instance.port,
            "Spawning instance"
        );

        // Build environment
        let mut env = config.env.clone();
        env.insert("PORT".to_string(), instance.port.to_string());
        env.entry("NODE_ENV".to_string())
            .or_insert_with(|| "production".to_string());

        // Spawn process
        let child = Command::new(&config.command[0])
            .args(&config.command[1..])
            .current_dir(&config.cwd)
            .envs(env)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()?;

        instance.set_process(child);
        instance.set_state(InstanceState::Starting);

        // Notify about start
        let _ = app
            .instance_tx
            .send(InstanceEvent::Started {
                app: app_name.clone(),
                instance_id,
            })
            .await;

        // Wait for ready
        let health_url = format!(
            "http://127.0.0.1:{}{}",
            instance.port, config.health_check_path
        );
        let health_host = config.health_check_host.clone();

        match timeout(
            config.startup_timeout,
            self.wait_for_ready(&health_url, &health_host, instance.clone()),
        )
        .await
        {
            Ok(Ok(())) => {
                instance.set_state(InstanceState::Healthy);
                tracing::info!(
                    app = %app_name,
                    instance = instance_id,
                    "Instance is healthy"
                );

                let _ = app
                    .instance_tx
                    .send(InstanceEvent::Ready {
                        app: app_name,
                        instance_id,
                    })
                    .await;

                Ok(())
            }
            Ok(Err(e)) => {
                instance.set_state(InstanceState::Unhealthy);
                let _ = instance.kill().await;
                Err(e)
            }
            Err(_) => {
                instance.set_state(InstanceState::Unhealthy);
                let _ = instance.kill().await;
                Err(InstanceError::StartupTimeout)
            }
        }
    }

    /// Wait for instance to become ready
    async fn wait_for_ready(
        &self,
        health_url: &str,
        health_host: &str,
        instance: Arc<Instance>,
    ) -> Result<(), InstanceError> {
        let mut interval = tokio::time::interval(Duration::from_millis(100));
        let mut attempts = 0;

        loop {
            interval.tick().await;

            // Check if process is still alive
            if !instance.is_alive().await {
                let detail = startup_exit_detail(instance.clone()).await;
                return Err(InstanceError::HealthCheckFailed(detail));
            }

            // Try health check
            match self
                .client
                .get(health_url)
                .header(reqwest::header::HOST, health_host)
                .send()
                .await
            {
                Ok(resp) if resp.status().is_success() => {
                    instance.set_state(InstanceState::Ready);
                    return Ok(());
                }
                Ok(resp) => {
                    tracing::debug!(
                        attempt = attempts,
                        status = %resp.status(),
                        "Health check returned non-success status"
                    );
                }
                Err(e) => {
                    tracing::debug!(
                        attempt = attempts,
                        error = %e,
                        "Health check failed"
                    );
                }
            }

            attempts += 1;
            if attempts > 300 {
                // 30 seconds with 100ms intervals
                return Err(InstanceError::HealthCheckFailed(
                    "Too many failed health checks".to_string(),
                ));
            }
        }
    }

    /// Run health check on an instance
    pub async fn health_check(&self, app: &App, instance: &Instance) -> bool {
        let (health_check_path, health_check_host) = {
            let config = app.config.read();
            (
                config.health_check_path.clone(),
                config.health_check_host.clone(),
            )
        };
        let health_url = format!("http://127.0.0.1:{}{}", instance.port, health_check_path);

        match self
            .client
            .get(&health_url)
            .header(reqwest::header::HOST, health_check_host)
            .send()
            .await
        {
            Ok(resp) => resp.status().is_success(),
            Err(_) => false,
        }
    }
}

async fn startup_exit_detail(instance: Arc<Instance>) -> String {
    let Some(child) = instance.take_process() else {
        return "Process exited during startup".to_string();
    };

    match child.wait_with_output().await {
        Ok(output) => format_startup_exit_error(output.status, &output.stdout, &output.stderr),
        Err(error) => format!("Process exited during startup; failed to read output: {error}"),
    }
}

fn format_startup_exit_error(status: ExitStatus, stdout: &[u8], stderr: &[u8]) -> String {
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
        return format!("Process exited during startup ({status_text})");
    }

    let preview = truncate_chars(&detail, 400);
    format!("Process exited during startup ({status_text}): {preview}")
}

fn truncate_chars(value: &str, max_chars: usize) -> String {
    let mut chars = value.chars();
    let preview: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{preview}...")
    } else {
        preview
    }
}

impl Default for Spawner {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::super::AppConfig;
    use super::*;
    use tokio::sync::mpsc;

    #[test]
    fn test_spawner_creation() {
        let spawner = Spawner::new();
        // Just verify it creates without panic
        drop(spawner);
    }

    #[test]
    #[cfg(unix)]
    fn startup_exit_error_prefers_stderr_and_includes_status() {
        use std::os::unix::process::ExitStatusExt;

        let status = ExitStatus::from_raw(2 << 8);
        let message = format_startup_exit_error(status, b"", b"missing wrapper");
        assert!(message.contains("exit code 2"));
        assert!(message.contains("missing wrapper"));
    }

    #[test]
    #[cfg(unix)]
    fn startup_exit_error_uses_stdout_when_stderr_empty() {
        use std::os::unix::process::ExitStatusExt;

        let status = ExitStatus::from_raw(0);
        let message = format_startup_exit_error(status, b"hello", b"");
        assert!(message.contains("hello"));
    }

    #[test]
    fn truncate_chars_adds_ellipsis_when_over_limit() {
        let text = "a".repeat(405);
        let truncated = truncate_chars(&text, 400);
        assert_eq!(truncated.len(), 403);
        assert!(truncated.ends_with("..."));
    }

    #[tokio::test]
    async fn health_check_uses_internal_status_host_and_path() {
        let Ok(listener) = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await else {
            return;
        };
        let port = listener.local_addr().unwrap().port();

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
                    .any(|line| line.eq_ignore_ascii_case("host: tako.internal"));

            let response = if is_internal_status {
                b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok".as_slice()
            } else {
                b"HTTP/1.1 404 Not Found\r\nContent-Length: 9\r\n\r\nnot found".as_slice()
            };

            let _ = tokio::io::AsyncWriteExt::write_all(&mut socket, response).await;
        });

        let (instance_tx, _instance_rx) = mpsc::channel(4);
        let config = AppConfig {
            name: "test-app".to_string(),
            base_port: port,
            health_check_path: "/status".to_string(),
            health_check_host: "tako.internal".to_string(),
            ..Default::default()
        };
        let app = App::new(config, instance_tx);
        let instance = app.allocate_instance();

        let spawner = Spawner::new();
        assert!(spawner.health_check(&app, &instance).await);
    }
}

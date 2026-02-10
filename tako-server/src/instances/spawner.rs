//! Instance spawner - spawns and monitors app processes

use super::{App, Instance, InstanceError, InstanceEvent, InstanceState};
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
        env.insert("NODE_ENV".to_string(), "production".to_string());

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

        match timeout(
            config.startup_timeout,
            self.wait_for_ready(&health_url, instance.clone()),
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
        instance: Arc<Instance>,
    ) -> Result<(), InstanceError> {
        let mut interval = tokio::time::interval(Duration::from_millis(100));
        let mut attempts = 0;

        loop {
            interval.tick().await;

            // Check if process is still alive
            if !instance.is_alive().await {
                return Err(InstanceError::HealthCheckFailed(
                    "Process exited during startup".to_string(),
                ));
            }

            // Try health check
            match self.client.get(health_url).send().await {
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
        let health_check_path = {
            let config = app.config.read();
            config.health_check_path.clone()
        };
        let health_url = format!("http://127.0.0.1:{}{}", instance.port, health_check_path);

        match self.client.get(&health_url).send().await {
            Ok(resp) => resp.status().is_success(),
            Err(_) => false,
        }
    }
}

impl Default for Spawner {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_spawner_creation() {
        let spawner = Spawner::new();
        // Just verify it creates without panic
        drop(spawner);
    }
}

//! Instance spawner - spawns and monitors app processes

use super::{App, AppConfig, Instance, InstanceError, InstanceEvent, InstanceState};
use std::collections::HashMap;
use std::process::ExitStatus;
use std::sync::Arc;
use std::time::Duration;
use tokio::process::Command;
use tokio::time::timeout;

/// Spawns and monitors app instances
pub struct Spawner {
    /// UID/GID of the `tako-app` user for process isolation when running privileged (Unix only)
    #[cfg(unix)]
    app_user: Option<(u32, u32)>,
}

impl Spawner {
    pub fn new() -> Self {
        Self {
            #[cfg(unix)]
            app_user: resolve_app_user(),
        }
    }
}

#[cfg(unix)]
fn resolve_app_user() -> Option<(u32, u32)> {
    use std::ffi::CString;
    // Unprivileged service users cannot switch to another uid/gid without extra capabilities.
    // In that case, run app processes as the current service user.
    if unsafe { libc::geteuid() } != 0 {
        tracing::debug!(
            "Running as unprivileged user; app processes will run as current service user"
        );
        return None;
    }
    let name = CString::new("tako-app").ok()?;
    // SAFETY: getpwnam is thread-safe when not modifying the passwd db.
    // The pointer is valid until the next call to getpwnam on this thread.
    let pw = unsafe { libc::getpwnam(name.as_ptr()) };
    if pw.is_null() {
        tracing::debug!("tako-app user not found; app processes will run as current user");
        return None;
    }
    let uid = unsafe { (*pw).pw_uid };
    let gid = unsafe { (*pw).pw_gid };
    tracing::info!(uid, gid, "Resolved tako-app user for app process isolation");
    Some((uid, gid))
}

impl Spawner {
    /// Spawn a new instance
    pub async fn spawn(&self, app: &App, instance: Arc<Instance>) -> Result<(), InstanceError> {
        let config = app.config.read().clone();
        let app_name = config.name.clone();
        let instance_id = instance.id.clone();

        tracing::info!(
            app = %app_name,
            instance = %instance_id,
            port = instance.port,
            "Spawning instance"
        );

        let env = build_instance_env(&config, &instance);

        let app_user = self.app_user;

        let child = spawn_child_process(&config, &env, app_user)?;

        instance.set_process(child);
        instance.set_state(InstanceState::Starting);

        // Notify about start
        let _ = app
            .instance_tx
            .send(InstanceEvent::Started {
                app: app_name.clone(),
                instance_id: instance_id.clone(),
            })
            .await;

        // Wait for ready
        let health_check_path = config.health_check_path.clone();
        let health_host = config.health_check_host.clone();

        match timeout(
            config.startup_timeout,
            self.wait_for_ready(
                &health_check_path,
                &health_host,
                Duration::from_secs(5),
                instance.clone(),
            ),
        )
        .await
        {
            Ok(Ok(())) => {
                instance.set_state(InstanceState::Healthy);
                tracing::info!(
                    app = %app_name,
                    instance = %instance_id,
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
        health_path: &str,
        health_host: &str,
        probe_timeout: Duration,
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
            if self
                .probe_health(&instance, health_path, health_host, probe_timeout)
                .await
            {
                instance.set_state(InstanceState::Ready);
                return Ok(());
            }
            tracing::debug!(attempt = attempts, "Health check failed");

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

        self.probe_health(
            instance,
            &health_check_path,
            &health_check_host,
            Duration::from_secs(5),
        )
        .await
    }

    async fn probe_health(
        &self,
        instance: &Instance,
        health_check_path: &str,
        health_check_host: &str,
        probe_timeout: Duration,
    ) -> bool {
        let Some(socket_path) = instance.socket_path() else {
            return false;
        };
        matches!(
            probe_unix_socket(
                &socket_path,
                health_check_path,
                health_check_host,
                probe_timeout,
            )
            .await,
            Ok(true)
        )
    }
}

fn build_instance_env(config: &AppConfig, instance: &Instance) -> HashMap<String, String> {
    // Merge non-secret vars with secrets (secrets take precedence).
    let mut env = config.env_vars.clone();
    env.extend(config.secrets.iter().map(|(k, v)| (k.clone(), v.clone())));

    env.insert("TAKO_INSTANCE".to_string(), instance.id.clone());

    #[cfg(unix)]
    {
        let socket_template = instance
            .socket_template()
            .expect("unix instances must provide a unix socket template");
        env.insert("TAKO_APP_SOCKET".to_string(), socket_template.to_string());
    }

    env.entry("NODE_ENV".to_string())
        .or_insert_with(|| "production".to_string());

    env
}

fn build_child_command(
    config: &AppConfig,
    env: &HashMap<String, String>,
    app_user: Option<(u32, u32)>,
    use_app_user: bool,
) -> Command {
    let mut child_cmd = Command::new(&config.command[0]);
    child_cmd
        .args(&config.command[1..])
        .current_dir(&config.cwd)
        .envs(env)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);

    #[cfg(unix)]
    if use_app_user && let Some((uid, gid)) = app_user {
        child_cmd.uid(uid);
        child_cmd.gid(gid);
    }

    child_cmd
}

fn should_retry_spawn_without_app_user(
    error: &std::io::Error,
    app_user: Option<(u32, u32)>,
) -> bool {
    app_user.is_some() && error.kind() == std::io::ErrorKind::PermissionDenied
}

fn spawn_child_process(
    config: &AppConfig,
    env: &HashMap<String, String>,
    app_user: Option<(u32, u32)>,
) -> std::io::Result<tokio::process::Child> {
    let mut child_cmd = build_child_command(config, env, app_user, true);
    match child_cmd.spawn() {
        Ok(child) => Ok(child),
        Err(error) if should_retry_spawn_without_app_user(&error, app_user) => {
            tracing::warn!(
                error = %error,
                "Failed to switch to tako-app user; retrying spawn as service user"
            );
            let mut fallback = build_child_command(config, env, app_user, false);
            fallback.spawn()
        }
        Err(error) => Err(error),
    }
}

#[cfg(unix)]
async fn probe_unix_socket(
    socket_path: &str,
    health_check_path: &str,
    health_check_host: &str,
    probe_timeout: Duration,
) -> Result<bool, std::io::Error> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let mut socket =
        match timeout(probe_timeout, tokio::net::UnixStream::connect(socket_path)).await {
            Ok(result) => result?,
            Err(_) => return Ok(false),
        };
    let request = format!(
        "GET {health_check_path} HTTP/1.1\r\nHost: {health_check_host}\r\nConnection: close\r\n\r\n"
    );
    match timeout(probe_timeout, socket.write_all(request.as_bytes())).await {
        Ok(result) => result?,
        Err(_) => return Ok(false),
    }

    let mut response_buf = [0_u8; 2048];
    let bytes_read = match timeout(probe_timeout, socket.read(&mut response_buf)).await {
        Ok(result) => result?,
        Err(_) => return Ok(false),
    };
    if bytes_read == 0 {
        return Ok(false);
    }

    let response = String::from_utf8_lossy(&response_buf[..bytes_read]);
    let status_line = response.lines().next().unwrap_or_default();
    Ok(http_status_is_success(status_line))
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
        let _spawner = Spawner::new();
        // Just verify it creates without panic
    }

    #[test]
    #[cfg(unix)]
    fn resolve_app_user_returns_none_gracefully_for_missing_user() {
        use std::ffi::CString;
        let name = CString::new("this-user-definitely-does-not-exist-tako-test").unwrap();
        let pw = unsafe { libc::getpwnam(name.as_ptr()) };
        assert!(pw.is_null(), "expected nonexistent user to return null");
        // resolve_app_user looks up "tako-app"; on dev machines it won't exist.
        // Calling Spawner::new() must not panic regardless.
        let _spawner = Spawner::new();
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

    #[test]
    #[cfg(unix)]
    fn retries_spawn_without_app_user_only_for_permission_denied() {
        let denied = std::io::Error::from(std::io::ErrorKind::PermissionDenied);
        let other = std::io::Error::from(std::io::ErrorKind::NotFound);

        assert!(should_retry_spawn_without_app_user(
            &denied,
            Some((1001, 1001))
        ));
        assert!(!should_retry_spawn_without_app_user(&denied, None));
        assert!(!should_retry_spawn_without_app_user(
            &other,
            Some((1001, 1001))
        ));
    }

    #[test]
    #[cfg(unix)]
    fn build_instance_env_uses_unix_socket_without_port_when_socket_template_exists() {
        use std::collections::HashMap;

        let (instance_tx, _instance_rx) = mpsc::channel(4);
        let app = App::new(
            AppConfig {
                name: "test-app".to_string(),
                env_vars: HashMap::from([("FOO".to_string(), "bar".to_string())]),
                secrets: HashMap::from([("SECRET".to_string(), "shh".to_string())]),
                ..Default::default()
            },
            instance_tx,
        );
        let instance = app.allocate_instance();

        let env = build_instance_env(&app.config.read().clone(), &instance);
        assert_eq!(env.get("FOO").map(String::as_str), Some("bar"));
        assert_eq!(env.get("SECRET").map(String::as_str), Some("shh"));
        assert_eq!(env.get("TAKO_INSTANCE").map(String::as_str), Some(instance.id.as_str()));
        assert!(env.contains_key("TAKO_APP_SOCKET"));
        assert!(!env.contains_key("PORT"));
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn health_check_uses_unix_socket_when_available() {
        use std::os::unix::net::UnixListener as StdUnixListener;
        use tempfile::TempDir;

        let temp = TempDir::new().unwrap();
        let pid = std::process::id();
        let socket_path = temp.path().join(format!("tako-app-test-app-{pid}.sock"));
        let Ok(listener) = StdUnixListener::bind(&socket_path) else {
            return;
        };
        listener.set_nonblocking(true).unwrap();
        let listener = tokio::net::UnixListener::from_std(listener).unwrap();

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
            base_port: 31_000,
            app_socket_dir: temp.path().to_path_buf(),
            health_check_path: "/status".to_string(),
            health_check_host: "tako".to_string(),
            ..Default::default()
        };
        let app = App::new(config, instance_tx);
        let instance = app.allocate_instance();
        instance.set_pid(pid);

        let spawner = Spawner::new();
        assert!(spawner.health_check(&app, &instance).await);
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn health_check_does_not_fallback_to_tcp_when_unix_socket_path_is_configured() {
        use tempfile::TempDir;

        let Ok(listener) = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await else {
            return;
        };
        let port = listener.local_addr().unwrap().port();
        let (accepted_tx, accepted_rx) = tokio::sync::oneshot::channel();

        tokio::spawn(async move {
            let accepted = tokio::time::timeout(Duration::from_secs(1), listener.accept())
                .await
                .is_ok();
            let _ = accepted_tx.send(accepted);
        });

        let temp = TempDir::new().unwrap();
        let (instance_tx, _instance_rx) = mpsc::channel(4);
        let config = AppConfig {
            name: "test-app".to_string(),
            base_port: port,
            app_socket_dir: temp.path().to_path_buf(),
            health_check_path: "/status".to_string(),
            health_check_host: "tako".to_string(),
            ..Default::default()
        };
        let app = App::new(config, instance_tx);
        let instance = app.allocate_instance();
        assert_eq!(
            instance.port, port,
            "test precondition: expected listener port"
        );
        instance.set_pid(std::process::id());

        let socket_path = instance
            .socket_path()
            .expect("instance should resolve socket path with pid");
        assert!(
            !std::path::Path::new(&socket_path).exists(),
            "test precondition: unix socket should be absent"
        );

        let spawner = Spawner::new();
        assert!(
            !spawner
                .probe_health(
                    &instance,
                    "/status",
                    "tako",
                    Duration::from_millis(200),
                )
                .await
        );
        let accepted = accepted_rx
            .await
            .expect("listener should report acceptance result");
        assert!(
            !accepted,
            "tcp fallback should not be attempted when unix socket path is configured"
        );
    }
}

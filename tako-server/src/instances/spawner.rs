//! Instance spawner - spawns and monitors app processes

use super::{
    App, AppConfig, INTERNAL_TOKEN_ENV, INTERNAL_TOKEN_HEADER, Instance, InstanceError,
    InstanceEvent, InstanceState,
};
use std::collections::HashMap;
use std::net::SocketAddr;
#[cfg(unix)]
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::path::Path;
use std::process::ExitStatus;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncBufReadExt;
use tokio::process::Command;
use tokio::time::timeout;

const READY_PREFIX: &str = "TAKO:READY:";

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
        let app_name = config.deployment_id();
        let instance_id = instance.id.clone();

        tracing::info!(
            app = %app_name,
            instance = %instance_id,
            "Spawning instance"
        );

        let env = build_instance_env(&config, &instance);
        let extra_args = build_instance_args(&instance);

        let app_user = self.app_user;

        let child = spawn_child_process(&config, &env, &extra_args, app_user, &config.secrets)
            .map_err(InstanceError::from)?;

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

        // Wait for the SDK to signal readiness via stdout (TAKO:READY:<port>).
        match timeout(config.startup_timeout, wait_for_ready(instance.clone())).await {
            Ok(Ok(())) => {
                instance.set_state(InstanceState::Healthy);

                // Now that the instance is healthy, drain stdout/stderr so the
                // OS pipe buffer never fills (which would block the app process).
                // We keep pipes open during startup so startup_exit_detail can
                // read error output if the process crashes before becoming healthy.
                instance.drain_pipes();

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
        let Some(endpoint) = instance.endpoint() else {
            return false;
        };
        matches!(
            probe_endpoint_tcp(
                endpoint,
                health_check_path,
                health_check_host,
                instance.internal_token(),
                probe_timeout,
            )
            .await,
            Ok(true)
        )
    }
}

/// Wait for the SDK to signal readiness via a `TAKO:READY:<port>` line on stdout.
/// Sets the instance upstream once the port is learned.
async fn wait_for_ready(instance: Arc<Instance>) -> Result<(), InstanceError> {
    let stdout = instance
        .take_stdout()
        .ok_or_else(|| InstanceError::HealthCheckFailed("no stdout pipe available".to_string()))?;
    let mut lines = tokio::io::BufReader::new(stdout).lines();
    let mut startup_output = Vec::new();

    loop {
        tokio::select! {
            line = lines.next_line() => {
                match line {
                    Ok(Some(line)) => {
                        if let Some(port_str) = line.strip_prefix(READY_PREFIX) {
                            let port: u16 = port_str.trim().parse().map_err(|_| {
                                InstanceError::HealthCheckFailed(
                                    format!("invalid port in readiness signal: {line}"),
                                )
                            })?;
                            instance.set_port(port);
                            instance.set_state(InstanceState::Ready);
                            // Spawn a drain task for remaining stdout
                            instance.drain_remaining_stdout(lines);
                            return Ok(());
                        }
                        startup_output.push(line);
                    }
                    Ok(None) => {
                        // stdout closed — process exited
                        let detail = if startup_output.is_empty() {
                            startup_exit_detail(instance).await
                        } else {
                            format!(
                                "Process exited during startup: {}",
                                startup_output.join("\n"),
                            )
                        };
                        return Err(InstanceError::HealthCheckFailed(detail));
                    }
                    Err(error) => {
                        return Err(InstanceError::HealthCheckFailed(
                            format!("failed to read stdout: {error}"),
                        ));
                    }
                }
            }
            _ = check_process_alive(&instance) => {
                let detail = if startup_output.is_empty() {
                    startup_exit_detail(instance).await
                } else {
                    format!(
                        "Process exited during startup: {}",
                        startup_output.join("\n"),
                    )
                };
                return Err(InstanceError::HealthCheckFailed(detail));
            }
        }
    }
}

/// Resolves when the instance process is no longer alive.
async fn check_process_alive(instance: &Instance) {
    let mut interval = tokio::time::interval(Duration::from_millis(100));
    loop {
        interval.tick().await;
        if !instance.is_alive().await {
            return;
        }
    }
}

fn build_instance_env(config: &AppConfig, instance: &Instance) -> HashMap<String, String> {
    let mut env = config.env_vars.clone();

    // PORT=0 tells the SDK to bind to an OS-assigned port and report it back
    // via the TAKO:READY:<port> stdout protocol.
    env.insert("PORT".to_string(), "0".to_string());
    env.insert("HOST".to_string(), "127.0.0.1".to_string());
    env.insert(
        INTERNAL_TOKEN_ENV.to_string(),
        instance.internal_token().to_string(),
    );

    env.entry("NODE_ENV".to_string())
        .or_insert_with(|| "production".to_string());

    env
}

/// Build the extra CLI args for the entrypoint (internal protocol, not env vars).
fn build_instance_args(instance: &Instance) -> Vec<String> {
    vec![
        "--instance".to_string(),
        instance.id.clone(),
        "--version".to_string(),
        instance.build_version().to_string(),
    ]
}

/// Resolve a binary name against the app's PATH env, falling back to the bare name.
fn resolve_binary_from_env(binary: &str, env: &HashMap<String, String>) -> String {
    // Already absolute — use as-is
    if binary.starts_with('/') {
        return binary.to_string();
    }
    // Search the app's PATH
    if let Some(path_var) = env.get("PATH") {
        for dir in path_var.split(':') {
            let candidate = Path::new(dir).join(binary);
            if candidate.is_file() {
                return candidate.to_string_lossy().to_string();
            }
        }
    }
    // Fallback to bare name (Command::new will search process PATH)
    binary.to_string()
}

/// Create a pipe with secrets JSON on the read end.
///
/// The write end is closed after writing so the child gets EOF after reading.
/// Returns the read-end `OwnedFd`; the caller must keep it alive until after spawn.
#[cfg(unix)]
fn create_secrets_pipe(secrets: &HashMap<String, String>) -> std::io::Result<OwnedFd> {
    use std::io::Write;

    let json = serde_json::to_vec(secrets)
        .map_err(|e| std::io::Error::other(format!("failed to serialize secrets: {e}")))?;

    let mut fds = [0i32; 2];
    // SAFETY: pipe() is a standard POSIX call; fds is a valid 2-element array.
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error());
    }

    // SAFETY: pipe() just returned these file descriptors.
    let read_end = unsafe { OwnedFd::from_raw_fd(fds[0]) };
    let write_end = unsafe { OwnedFd::from_raw_fd(fds[1]) };

    // Write secrets JSON and close write end so child gets EOF after reading.
    let mut writer = std::fs::File::from(write_end);
    writer.write_all(&json)?;
    drop(writer);

    Ok(read_end)
}

fn build_child_command(
    config: &AppConfig,
    env: &HashMap<String, String>,
    extra_args: &[String],
    app_user: Option<(u32, u32)>,
    use_app_user: bool,
    secrets_fd: Option<RawFd>,
) -> std::io::Result<Command> {
    // Resolve the binary using the app's env PATH (not the server's PATH).
    let binary = resolve_binary_from_env(&config.command[0], env);
    let mut child_cmd = Command::new(&binary);
    child_cmd.args(&config.command[1..]).args(extra_args);

    #[cfg(unix)]
    if use_app_user && let Some((uid, gid)) = app_user {
        child_cmd.uid(uid);
        child_cmd.gid(gid);
    }

    child_cmd
        .current_dir(&config.path)
        .envs(env)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);

    // Pass secrets to the child via fd 3 (Tako runtime ABI).
    // pre_exec runs in the child after fork, before exec — dup2 the pipe
    // read end to fd 3 so it survives exec (no CLOEXEC).
    // When there are no secrets, close fd 3 so stray inherited fds
    // don't cause the entrypoint to block.
    #[cfg(unix)]
    unsafe {
        child_cmd.pre_exec(move || {
            if let Some(fd) = secrets_fd {
                if fd != 3 {
                    if libc::dup2(fd, 3) == -1 {
                        return Err(std::io::Error::last_os_error());
                    }
                    libc::close(fd);
                }
            } else {
                libc::close(3);
            }
            Ok(())
        });
    }

    Ok(child_cmd)
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
    extra_args: &[String],
    app_user: Option<(u32, u32)>,
    secrets: &HashMap<String, String>,
) -> std::io::Result<tokio::process::Child> {
    // Create a pipe with secrets JSON for the child to read on fd 3.
    // The OwnedFd must stay alive until after spawn (fork copies the fd table).
    #[cfg(unix)]
    let secrets_pipe = if !secrets.is_empty() {
        Some(create_secrets_pipe(secrets)?)
    } else {
        None
    };
    #[cfg(unix)]
    let raw_fd = secrets_pipe.as_ref().map(|fd| fd.as_raw_fd());
    #[cfg(not(unix))]
    let raw_fd = None;

    let mut child_cmd = build_child_command(config, env, extra_args, app_user, true, raw_fd)?;
    match child_cmd.spawn() {
        Ok(child) => Ok(child),
        Err(error) if should_retry_spawn_without_app_user(&error, app_user) => {
            tracing::warn!(
                error = %error,
                "Failed to switch to tako-app user; retrying spawn as service user"
            );
            // Pipe is still valid — fork either failed or the child exited before reading.
            let mut fallback =
                build_child_command(config, env, extra_args, app_user, false, raw_fd)?;
            fallback.spawn()
        }
        Err(error) => Err(error),
    }
    // secrets_pipe dropped here — parent's copy of the read end is closed.
    // The child already has its own copy from fork.
}

async fn probe_endpoint_tcp(
    endpoint: SocketAddr,
    health_check_path: &str,
    health_check_host: &str,
    internal_token: &str,
    probe_timeout: Duration,
) -> Result<bool, std::io::Error> {
    use tokio::io::AsyncWriteExt;

    let mut socket = match timeout(probe_timeout, tokio::net::TcpStream::connect(endpoint)).await {
        Ok(result) => result?,
        Err(_) => return Ok(false),
    };
    let request = format!(
        "GET {health_check_path} HTTP/1.1\r\nHost: {health_check_host}\r\n{INTERNAL_TOKEN_HEADER}: {internal_token}\r\nConnection: close\r\n\r\n"
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
    fn build_instance_env_only_has_app_vars() {
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
        instance.set_port(48_123);

        let env = build_instance_env(&app.config.read().clone(), &instance);
        assert_eq!(env.get("FOO").map(String::as_str), Some("bar"));
        // Secrets are NOT injected as env vars — they're passed via fd 3 at spawn time.
        assert!(!env.contains_key("SECRET"));
        assert_eq!(env.get("HOST").map(String::as_str), Some("127.0.0.1"));
        assert!(env.contains_key("PORT"));
        assert_eq!(
            env.get(INTERNAL_TOKEN_ENV).map(String::as_str),
            Some(instance.internal_token())
        );
        // TAKO_ runtime identity vars are passed as CLI args, not env vars.
        assert!(!env.contains_key("TAKO_INSTANCE"));
        assert!(!env.contains_key("TAKO_HAS_SECRETS"));
    }

    #[test]
    fn build_instance_args_has_instance_and_version() {
        let (instance_tx, _instance_rx) = mpsc::channel(4);
        let app = App::new(
            AppConfig {
                name: "test-app".to_string(),
                version: "v42".to_string(),
                ..Default::default()
            },
            instance_tx,
        );
        let instance = app.allocate_instance();

        let args = build_instance_args(&instance);
        assert!(args.contains(&"--instance".to_string()));
        assert!(args.contains(&instance.id));
        assert!(args.contains(&"--version".to_string()));
        assert!(args.contains(&"v42".to_string()));
    }

    #[test]
    fn build_instance_env_sets_port_zero_and_host_loopback() {
        let (instance_tx, _instance_rx) = mpsc::channel(4);
        let app = App::new(
            AppConfig {
                name: "test-app".to_string(),
                ..Default::default()
            },
            instance_tx,
        );
        let instance = app.allocate_instance();

        let env = build_instance_env(&app.config.read().clone(), &instance);
        assert_eq!(env.get("PORT").map(String::as_str), Some("0"));
        assert_eq!(env.get("HOST").map(String::as_str), Some("127.0.0.1"));
        assert_eq!(
            env.get(INTERNAL_TOKEN_ENV).map(String::as_str),
            Some(instance.internal_token())
        );
    }

    #[test]
    fn build_instance_env_overwrites_user_host_with_loopback() {
        let (instance_tx, _instance_rx) = mpsc::channel(4);
        let app = App::new(
            AppConfig {
                name: "test-app".to_string(),
                env_vars: HashMap::from([("HOST".to_string(), "0.0.0.0".to_string())]),
                ..Default::default()
            },
            instance_tx,
        );
        let instance = app.allocate_instance();

        let env = build_instance_env(&app.config.read().clone(), &instance);
        assert_eq!(env.get("HOST").map(String::as_str), Some("127.0.0.1"));
    }

    #[test]
    fn build_instance_args_never_includes_socket_flag() {
        let (instance_tx, _instance_rx) = mpsc::channel(4);
        let app = App::new(
            AppConfig {
                name: "test-app".to_string(),
                version: "v42".to_string(),
                ..Default::default()
            },
            instance_tx,
        );
        let instance = app.allocate_instance();

        let args = build_instance_args(&instance);
        assert!(!args.contains(&"--socket".to_string()));
        assert!(args.contains(&"--instance".to_string()));
        assert!(args.contains(&"--version".to_string()));
        assert!(args.contains(&"v42".to_string()));
    }

    #[tokio::test]
    async fn health_check_requires_matching_internal_token() {
        let Ok(listener) = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await else {
            return;
        };
        let port = listener.local_addr().expect("listener addr").port();
        let token = "spawner-health-token".to_string();
        let closure_token = token.clone();

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
                line.eq_ignore_ascii_case(&format!("{INTERNAL_TOKEN_HEADER}: {closure_token}"))
            });

            let response = if is_internal_status && has_token {
                format!(
                    "HTTP/1.1 200 OK\r\n{INTERNAL_TOKEN_HEADER}: {closure_token}\r\nContent-Length: 2\r\n\r\nok"
                )
            } else {
                "HTTP/1.1 404 Not Found\r\nContent-Length: 9\r\n\r\nnot found".to_string()
            };

            let _ = tokio::io::AsyncWriteExt::write_all(&mut socket, response.as_bytes()).await;
        });

        let (instance_tx, _instance_rx) = mpsc::channel(4);
        let config = AppConfig {
            name: "test-app".to_string(),
            health_check_path: "/status".to_string(),
            health_check_host: "tako".to_string(),
            ..Default::default()
        };
        let app = App::new(config, instance_tx);
        let instance = app.allocate_instance();
        instance.set_port(port);
        let token_field = instance.internal_token().to_string();
        assert_ne!(token_field, token, "test should use the instance token");

        let spawner = Spawner::new();
        assert!(
            !spawner.health_check(&app, &instance).await,
            "mismatched token must fail"
        );
    }

    #[tokio::test]
    async fn health_check_uses_loopback_tcp_with_matching_internal_token() {
        let Ok(listener) = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await else {
            return;
        };
        let port = listener.local_addr().expect("listener addr").port();

        let (instance_tx, _instance_rx) = mpsc::channel(4);
        let config = AppConfig {
            name: "test-app".to_string(),
            health_check_path: "/status".to_string(),
            health_check_host: "tako".to_string(),
            ..Default::default()
        };
        let app = App::new(config, instance_tx);
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

        let spawner = Spawner::new();
        assert!(spawner.health_check(&app, &instance).await);
    }

    #[tokio::test]
    async fn health_check_reads_response_headers_across_multiple_chunks() {
        let Ok(listener) = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await else {
            return;
        };
        let port = listener.local_addr().expect("listener addr").port();

        let (instance_tx, _instance_rx) = mpsc::channel(4);
        let config = AppConfig {
            name: "test-app".to_string(),
            health_check_path: "/status".to_string(),
            health_check_host: "tako".to_string(),
            ..Default::default()
        };
        let app = App::new(config, instance_tx);
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

        let spawner = Spawner::new();
        assert!(spawner.health_check(&app, &instance).await);
    }
}

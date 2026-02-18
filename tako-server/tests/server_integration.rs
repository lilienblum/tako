//! Server Integration Tests
//!
//! Tests the tako-server functionality including:
//! - Instance management
//! - Reload command handling
//! - Health endpoint availability

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::Duration;
use tempfile::TempDir;

fn workspace_root() -> PathBuf {
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .parent()
        .map(std::path::Path::to_path_buf)
        .unwrap_or_else(|| manifest_dir.to_path_buf())
}

fn apply_coverage_env(cmd: &mut Command) {
    let Some(profile) = std::env::var_os("LLVM_PROFILE_FILE") else {
        return;
    };
    let profile = PathBuf::from(profile);
    if profile.is_absolute() {
        return;
    }
    let absolute = workspace_root().join(profile);
    if let Some(parent) = absolute.parent() {
        let _ = fs::create_dir_all(parent);
    }
    cmd.env("LLVM_PROFILE_FILE", absolute);
}

fn pick_unused_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .expect("failed to bind to ephemeral port")
        .local_addr()
        .expect("failed to read local addr")
        .port()
}

fn can_bind_localhost() -> bool {
    TcpListener::bind("127.0.0.1:0").is_ok()
}

fn should_fail_when_localhost_bind_unavailable(ci_env: Option<&str>) -> bool {
    ci_env.is_some_and(|value| !value.trim().is_empty())
}

fn require_localhost_bind() -> bool {
    if can_bind_localhost() {
        return true;
    }
    if should_fail_when_localhost_bind_unavailable(std::env::var("CI").ok().as_deref()) {
        panic!("integration test requires localhost bind access in CI environment");
    }
    eprintln!("skipping integration test: localhost bind access unavailable");
    false
}

fn bun_available() -> bool {
    Command::new("bun")
        .arg("--version")
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

fn e2e_enabled() -> bool {
    std::env::var("TAKO_E2E").is_ok()
}

/// Helper to start tako-server in background
struct TestServer {
    child: Option<Child>,
    socket_path: PathBuf,
    data_dir: TempDir,
    http_port: u16,
}

const SERVER_START_RETRIES: usize = 5;
const SERVER_START_POLL_ATTEMPTS: usize = 100;
const SERVER_START_POLL_DELAY: Duration = Duration::from_millis(100);
const SERVER_START_RETRY_DELAY: Duration = Duration::from_millis(50);

impl TestServer {
    fn start() -> Self {
        let data_dir = TempDir::new().unwrap();
        let socket_path = data_dir.path().join("tako.sock");
        let mut last_error = None;

        for attempt in 1..=SERVER_START_RETRIES {
            let http_port = pick_unused_port();
            let tls_port = pick_unused_port();

            let _ = fs::remove_file(&socket_path);
            let mut child = spawn_test_server(&socket_path, data_dir.path(), http_port, tls_port);
            match wait_for_server_socket(&socket_path, &mut child) {
                Ok(()) => {
                    return TestServer {
                        child: Some(child),
                        socket_path,
                        data_dir,
                        http_port,
                    };
                }
                Err(error) => {
                    last_error = Some(format!(
                        "attempt {attempt}/{SERVER_START_RETRIES} failed (http={http_port}, tls={tls_port}): {error}"
                    ));
                    let _ = child.kill();
                    let _ = child.wait();
                    thread::sleep(SERVER_START_RETRY_DELAY);
                }
            }
        }

        panic!(
            "failed to start tako-server after {} attempts: {}",
            SERVER_START_RETRIES,
            last_error.unwrap_or_else(|| "unknown error".to_string())
        );
    }

    fn send_command(&self, command: &serde_json::Value) -> serde_json::Value {
        let mut stream =
            UnixStream::connect(&self.socket_path).expect("Failed to connect to server socket");

        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(5)))
            .unwrap();

        writeln!(stream, "{}", command).expect("Failed to send command");

        let mut reader = BufReader::new(stream);
        let mut response = String::new();
        reader
            .read_line(&mut response)
            .expect("Failed to read response");

        serde_json::from_str(&response).unwrap_or_else(|_| {
            serde_json::json!({
                "status": "error",
                "message": format!("Invalid JSON response: {}", response.trim()),
            })
        })
    }

    fn http_get(&self, path: &str) -> Result<String, String> {
        self.http_get_with_host("localhost", path)
    }

    fn http_get_with_host(&self, host: &str, path: &str) -> Result<String, String> {
        self.http_get_with_host_and_headers(host, path, &[])
    }

    fn http_get_with_host_and_headers(
        &self,
        host: &str,
        path: &str,
        headers: &[(&str, &str)],
    ) -> Result<String, String> {
        let addr = format!("127.0.0.1:{}", self.http_port);
        let mut stream =
            TcpStream::connect(&addr).map_err(|e| format!("Failed to connect: {}", e))?;

        stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
        let extra_headers = headers
            .iter()
            .map(|(name, value)| format!("{name}: {value}\r\n"))
            .collect::<String>();
        let request = format!(
            "GET {} HTTP/1.1\r\nHost: {}\r\n{}Connection: close\r\n\r\n",
            path, host, extra_headers
        );
        stream
            .write_all(request.as_bytes())
            .map_err(|e| format!("Failed to write: {}", e))?;

        let mut response = Vec::new();
        std::io::Read::read_to_end(&mut stream, &mut response)
            .map_err(|e| format!("Failed to read: {}", e))?;

        String::from_utf8(response).map_err(|e| format!("Invalid UTF-8: {}", e))
    }

    fn data_dir(&self) -> &std::path::Path {
        self.data_dir.path()
    }
}

fn spawn_test_server(
    socket_path: &std::path::Path,
    data_dir: &std::path::Path,
    http_port: u16,
    tls_port: u16,
) -> Child {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_tako-server"));
    cmd.arg("--socket")
        .arg(socket_path)
        .arg("--data-dir")
        .arg(data_dir)
        .arg("--port")
        .arg(http_port.to_string())
        .arg("--tls-port")
        .arg(tls_port.to_string())
        .arg("--no-acme")
        .env("RUST_LOG", "warn")
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    apply_coverage_env(&mut cmd);
    cmd.spawn().expect("Failed to start tako-server")
}

fn wait_for_server_socket(socket_path: &std::path::Path, child: &mut Child) -> Result<(), String> {
    for _ in 0..SERVER_START_POLL_ATTEMPTS {
        if socket_path.exists() && UnixStream::connect(socket_path).is_ok() {
            thread::sleep(SERVER_START_POLL_DELAY);
            return Ok(());
        }
        if let Ok(Some(status)) = child.try_wait() {
            return Err(format!(
                "tako-server exited before socket became available: {status}"
            ));
        }
        thread::sleep(SERVER_START_POLL_DELAY);
    }
    Err("server socket never became available".to_string())
}

impl Drop for TestServer {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

mod localhost_bind {
    use super::*;

    #[test]
    fn ci_env_requires_failure_when_bind_is_unavailable() {
        assert!(should_fail_when_localhost_bind_unavailable(Some("true")));
        assert!(!should_fail_when_localhost_bind_unavailable(None));
        assert!(!should_fail_when_localhost_bind_unavailable(Some("  ")));
    }
}

mod instance_management {
    use super::*;

    #[test]
    fn test_list_apps_empty() {
        if !require_localhost_bind() {
            return;
        }

        let server = TestServer::start();

        let response = server.send_command(&serde_json::json!({ "command": "list" }));
        assert_eq!(response.get("status").and_then(|s| s.as_str()), Some("ok"));

        let apps = response
            .get("data")
            .and_then(|d| d.get("apps"))
            .and_then(|a| a.as_array())
            .expect("response should include data.apps array");
        assert!(apps.is_empty(), "expected no apps, got: {response}");
    }

    #[test]
    fn test_deploy_and_list() {
        if !require_localhost_bind() || !e2e_enabled() || !bun_available() {
            return;
        }

        let server = TestServer::start();

        // Create a Bun app that serves requests on PORT.
        let app_dir = server.data_dir().join("releases").join("test-app-v1");
        fs::create_dir_all(&app_dir).unwrap();
        fs::create_dir_all(app_dir.join("node_modules/tako.sh/src")).unwrap();
        fs::write(
            app_dir.join("package.json"),
            r#"{"name":"test-app","scripts":{"dev":"bun run index.ts"}}"#,
        )
        .unwrap();
        fs::write(
            app_dir.join("node_modules/tako.sh/src/wrapper.ts"),
            "export default {};",
        )
        .unwrap();
        fs::write(
            app_dir.join("app.json"),
            r#"{"runtime":"bun","main":"index.ts","install":"true","start":["bun","{main}"]}"#,
        )
        .unwrap();
        fs::write(
            app_dir.join("index.ts"),
            r#"
Bun.serve({
  port: Number(process.env.PORT || 3000),
  fetch(request) {
    const url = new URL(request.url);
    const host = (request.headers.get("host") ?? url.host).split(":")[0]?.toLowerCase();
    if (host === "tako.internal" && url.pathname === "/status") {
      return new Response(JSON.stringify({ status: "ok" }), {
        headers: { "Content-Type": "application/json" },
      });
    }
    return new Response("test");
  },
});
"#,
        )
        .unwrap();

        let deploy_cmd = serde_json::json!({
            "command": "deploy",
            "app": "test-app",
            "version": "v1",
            "path": app_dir.to_string_lossy(),
            "routes": ["test-app.localhost"],
            "instances": 1,
            "idle_timeout": 300
        });

        let deploy_response = server.send_command(&deploy_cmd);
        assert_eq!(
            deploy_response.get("status").and_then(|s| s.as_str()),
            Some("ok"),
            "deploy should succeed: {deploy_response}"
        );

        // List should show the app.
        let list_response = server.send_command(&serde_json::json!({ "command": "list" }));
        let apps = list_response
            .get("data")
            .and_then(|d| d.get("apps"))
            .and_then(|a| a.as_array())
            .expect("response should include data.apps array");
        assert!(
            apps.iter()
                .any(|a| a.get("name").and_then(|n| n.as_str()) == Some("test-app")),
            "expected test-app in list response: {list_response}"
        );
    }
}

mod health_check {
    use super::*;

    #[test]
    fn test_http_redirects_to_https() {
        if !require_localhost_bind() {
            return;
        }

        let server = TestServer::start();
        let response = server
            .http_get("/")
            .expect("root endpoint request should succeed");

        assert!(
            response.starts_with("HTTP/1.1 307") || response.starts_with("HTTP/1.0 307"),
            "expected 307 response: {response}"
        );
        assert!(
            response.contains("Location: https://localhost/"),
            "expected https location header: {response}"
        );
        assert!(
            response.contains("Cache-Control: no-store"),
            "expected no-store cache control on redirect: {response}"
        );
    }

    #[test]
    fn test_health_endpoint() {
        if !require_localhost_bind() {
            return;
        }

        let server = TestServer::start();

        let response = server
            .http_get_with_host("tako.internal", "/status")
            .expect("status endpoint request should succeed");

        assert!(
            response.starts_with("HTTP/1.1 200") || response.starts_with("HTTP/1.0 200"),
            "expected 200 response: {response}"
        );
        assert!(
            response.contains("\"healthy\":true") || response.contains("\"healthy\": true"),
            "expected healthy payload: {response}"
        );
    }

    #[test]
    fn test_orbstack_host_does_not_redirect_when_proto_header_missing() {
        if !require_localhost_bind() {
            return;
        }

        let server = TestServer::start();

        let response = server
            .http_get_with_host_and_headers(
                "test-app.orb.local",
                "/",
                &[("X-Forwarded-For", "127.0.0.1")],
            )
            .expect("orb.local request should succeed");

        assert!(
            response.starts_with("HTTP/1.1 404") || response.starts_with("HTTP/1.0 404"),
            "expected 404 response without redirect loop: {response}"
        );
        assert!(
            !response.contains("Location: https://"),
            "did not expect https redirect for orb.local forwarded request: {response}"
        );
    }
}

mod rolling_update {
    use super::*;

    #[test]
    fn test_reload_missing_app_returns_error() {
        if !require_localhost_bind() {
            return;
        }

        let server = TestServer::start();

        let response =
            server.send_command(&serde_json::json!({ "command": "reload", "app": "missing" }));
        assert_eq!(
            response.get("status").and_then(|s| s.as_str()),
            Some("error")
        );

        let message = response
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or_default();
        assert!(
            message.contains("App not found"),
            "expected app not found error, got: {response}"
        );
    }
}

mod protocol {

    #[test]
    fn test_protocol_message_parsing() {
        // Test that protocol messages are correctly formatted
        let ready_msg = serde_json::json!({
            "type": "ready",
            "app": "test",
            "version": "v1",
            "instance_id": 1,
            "pid": 12345,
            "socket_path": "/tmp/test.sock",
            "timestamp": "2024-01-15T00:00:00Z"
        });

        let parsed: serde_json::Value = serde_json::from_str(&ready_msg.to_string()).unwrap();
        assert_eq!(parsed["type"], "ready");
        assert_eq!(parsed["app"], "test");
    }

    #[test]
    fn test_heartbeat_message() {
        let heartbeat = serde_json::json!({
            "type": "heartbeat",
            "app": "test",
            "instance_id": 1,
            "pid": 12345,
            "timestamp": "2024-01-15T00:00:00Z"
        });

        let parsed: serde_json::Value = serde_json::from_str(&heartbeat.to_string()).unwrap();
        assert_eq!(parsed["type"], "heartbeat");
    }

    #[test]
    fn test_shutdown_message() {
        let shutdown = serde_json::json!({
            "type": "shutdown",
            "reason": "deploy",
            "drain_timeout_seconds": 30
        });

        let parsed: serde_json::Value = serde_json::from_str(&shutdown.to_string()).unwrap();
        assert_eq!(parsed["type"], "shutdown");
        assert_eq!(parsed["reason"], "deploy");
    }
}

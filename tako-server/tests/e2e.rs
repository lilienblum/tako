//! End-to-End Tests
//!
//! Full integration tests that deploy real apps and verify:
//! - socket protocol error handling
//! - deploy flow (when `TAKO_E2E=1` and Bun is available)
//! - host/path routing (when `TAKO_E2E=1` and Bun is available)
//! - rolling deploy path (when `TAKO_E2E=1` and Bun is available)

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

fn pick_unused_port() -> Option<u16> {
    TcpListener::bind("127.0.0.1:0")
        .ok()
        .and_then(|listener| listener.local_addr().ok())
        .map(|addr| addr.port())
}

fn can_bind_localhost() -> bool {
    TcpListener::bind("127.0.0.1:0").is_ok()
}

fn bun_available() -> bool {
    Command::new("bun")
        .arg("--version")
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

fn e2e_enabled() -> bool {
    std::env::var("TAKO_E2E").is_ok() && bun_available() && can_bind_localhost()
}

/// E2E test environment with tako-server running.
struct E2EEnvironment {
    server_process: Option<Child>,
    server_socket: PathBuf,
    http_port: u16,
    data_dir: TempDir,
}

impl E2EEnvironment {
    fn new() -> Self {
        let data_dir = TempDir::new().unwrap();
        let server_socket = data_dir.path().join("tako.sock");

        let http_port = pick_unused_port().unwrap_or(18080);
        let https_port = pick_unused_port().unwrap_or(18443);

        let mut cmd = Command::new(env!("CARGO_BIN_EXE_tako-server"));
        cmd.arg("--socket")
            .arg(&server_socket)
            .arg("--data-dir")
            .arg(data_dir.path())
            .arg("--port")
            .arg(http_port.to_string())
            .arg("--tls-port")
            .arg(https_port.to_string())
            .arg("--no-acme")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        apply_coverage_env(&mut cmd);
        let server_process = cmd.spawn().expect("Failed to start tako-server");

        let env = E2EEnvironment {
            server_process: Some(server_process),
            server_socket,
            http_port,
            data_dir,
        };

        env.wait_for_ready();
        env
    }

    fn wait_for_ready(&self) {
        for _ in 0..100 {
            if self.server_socket.exists() {
                thread::sleep(Duration::from_millis(200));
                return;
            }
            thread::sleep(Duration::from_millis(100));
        }
        panic!("Server did not become ready in time");
    }

    fn send_command(&self, command: &serde_json::Value) -> serde_json::Value {
        let mut stream =
            UnixStream::connect(&self.server_socket).expect("Failed to connect to server");

        stream
            .set_read_timeout(Some(Duration::from_secs(30)))
            .unwrap();

        writeln!(stream, "{}", command).expect("Failed to write command");

        let mut reader = BufReader::new(stream);
        let mut response = String::new();
        reader
            .read_line(&mut response)
            .expect("Failed to read response");

        serde_json::from_str(&response).unwrap_or_else(
            |_| serde_json::json!({"status": "error", "message": "Failed to parse response", "raw": response}),
        )
    }

    fn send_raw_command(&self, line: &str) -> String {
        let mut stream =
            UnixStream::connect(&self.server_socket).expect("Failed to connect to server");

        stream
            .set_read_timeout(Some(Duration::from_secs(30)))
            .unwrap();

        writeln!(stream, "{}", line).expect("Failed to write command");

        let mut reader = BufReader::new(stream);
        let mut response = String::new();
        reader
            .read_line(&mut response)
            .expect("Failed to read response");
        response
    }

    fn create_test_app(&self, name: &str, code: &str) -> PathBuf {
        let app_dir = self.data_dir.path().join("apps").join(name);
        fs::create_dir_all(&app_dir).unwrap();
        fs::create_dir_all(app_dir.join("node_modules/tako.sh/src")).unwrap();

        fs::write(
            app_dir.join("package.json"),
            format!(
                r#"{{"name":"{}","scripts":{{"dev":"bun run index.ts"}}}}"#,
                name
            ),
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
        fs::write(app_dir.join("index.ts"), code).unwrap();
        app_dir
    }

    fn http_get_with_host(&self, path: &str, host: &str) -> Result<String, String> {
        let addr = format!("127.0.0.1:{}", self.http_port);
        let mut stream =
            TcpStream::connect(&addr).map_err(|e| format!("Failed to connect: {}", e))?;

        stream.set_read_timeout(Some(Duration::from_secs(5))).ok();

        let request = format!(
            "GET {} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
            path, host
        );
        stream
            .write_all(request.as_bytes())
            .map_err(|e| format!("Failed to write: {}", e))?;

        let mut response = Vec::new();
        std::io::Read::read_to_end(&mut stream, &mut response)
            .map_err(|e| format!("Failed to read: {}", e))?;

        String::from_utf8(response).map_err(|e| format!("Invalid UTF-8: {}", e))
    }
}

impl Drop for E2EEnvironment {
    fn drop(&mut self) {
        if let Some(mut child) = self.server_process.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn deploy_command(
    app: &str,
    version: &str,
    path: &std::path::Path,
    routes: &[&str],
    instances: u8,
) -> serde_json::Value {
    serde_json::json!({
        "command": "deploy",
        "app": app,
        "version": version,
        "path": path.to_string_lossy(),
        "routes": routes,
        "instances": instances,
        "idle_timeout": 300
    })
}

mod deploy_flow {
    use super::*;

    #[test]
    fn test_init_build_deploy_request() {
        if !e2e_enabled() {
            return;
        }

        let env = E2EEnvironment::new();

        let app_dir = env.create_test_app(
            "hello-world",
            r#"
Bun.serve({
  port: Number(process.env.PORT || 3000),
  fetch(request) {
    const url = new URL(request.url);
    const path = url.pathname;
    const host = (request.headers.get("host") ?? url.host).split(":")[0]?.toLowerCase();
    if (host === "tako.internal" && path === "/status") {
      return new Response(JSON.stringify({ status: "ok" }), {
        headers: { "Content-Type": "application/json" },
      });
    }
    return new Response("Hello from Tako!");
  },
});
"#,
        );

        let response = env.send_command(&deploy_command(
            "hello-world",
            "v1",
            &app_dir,
            &["hello-world.localhost"],
            1,
        ));

        assert_eq!(response.get("status").and_then(|s| s.as_str()), Some("ok"));

        thread::sleep(Duration::from_secs(2));

        let response = env
            .http_get_with_host("/", "hello-world.localhost")
            .expect("request should succeed");
        assert!(
            response.contains("Hello from Tako!"),
            "expected app response, got: {}",
            response
        );
    }
}

mod routing {
    use super::*;

    #[test]
    fn test_multiple_apps_routing() {
        if !e2e_enabled() {
            return;
        }

        let env = E2EEnvironment::new();

        let app_a = env.create_test_app(
            "app-a",
            r#"
Bun.serve({
  port: Number(process.env.PORT || 3000),
  fetch(request) {
    const url = new URL(request.url);
    const path = url.pathname;
    const host = (request.headers.get("host") ?? url.host).split(":")[0]?.toLowerCase();
    if (host === "tako.internal" && path === "/status") {
      return new Response(JSON.stringify({ status: "ok" }), {
        headers: { "Content-Type": "application/json" },
      });
    }
    return new Response("App A");
  },
});
"#,
        );
        let app_b = env.create_test_app(
            "app-b",
            r#"
Bun.serve({
  port: Number(process.env.PORT || 3000),
  fetch(request) {
    const url = new URL(request.url);
    const path = url.pathname;
    const host = (request.headers.get("host") ?? url.host).split(":")[0]?.toLowerCase();
    if (host === "tako.internal" && path === "/status") {
      return new Response(JSON.stringify({ status: "ok" }), {
        headers: { "Content-Type": "application/json" },
      });
    }
    return new Response("App B");
  },
});
"#,
        );

        let deploy_a = env.send_command(&deploy_command(
            "app-a",
            "v1",
            &app_a,
            &["router.localhost/a/*"],
            1,
        ));
        assert_eq!(deploy_a.get("status").and_then(|s| s.as_str()), Some("ok"));

        let deploy_b = env.send_command(&deploy_command(
            "app-b",
            "v1",
            &app_b,
            &["router.localhost/b/*"],
            1,
        ));
        assert_eq!(deploy_b.get("status").and_then(|s| s.as_str()), Some("ok"));

        thread::sleep(Duration::from_secs(2));

        let a_response = env
            .http_get_with_host("/a/one", "router.localhost")
            .expect("app-a request should succeed");
        assert!(
            a_response.contains("App A"),
            "expected App A response, got: {}",
            a_response
        );

        let b_response = env
            .http_get_with_host("/b/two", "router.localhost")
            .expect("app-b request should succeed");
        assert!(
            b_response.contains("App B"),
            "expected App B response, got: {}",
            b_response
        );
    }
}

mod rolling_updates {
    use super::*;

    #[test]
    fn test_rolling_update_deploys_new_version() {
        if !e2e_enabled() {
            return;
        }

        let env = E2EEnvironment::new();

        let v1_dir = env.create_test_app(
            "versioned-app-v1",
            r#"
Bun.serve({
  port: Number(process.env.PORT || 3000),
  fetch(request) {
    const url = new URL(request.url);
    const path = url.pathname;
    const host = (request.headers.get("host") ?? url.host).split(":")[0]?.toLowerCase();
    if (host === "tako.internal" && path === "/status") {
      return new Response(JSON.stringify({ status: "ok" }), {
        headers: { "Content-Type": "application/json" },
      });
    }
    return new Response("v1");
  },
});
"#,
        );

        let deploy_v1 = env.send_command(&deploy_command(
            "versioned-app",
            "v1",
            &v1_dir,
            &["rolling.localhost"],
            2,
        ));
        assert_eq!(deploy_v1.get("status").and_then(|s| s.as_str()), Some("ok"));

        thread::sleep(Duration::from_secs(2));
        let before = env
            .http_get_with_host("/", "rolling.localhost")
            .expect("v1 request should succeed");
        assert!(
            before.contains("v1"),
            "expected v1 response, got: {}",
            before
        );

        let v2_dir = env.create_test_app(
            "versioned-app-v2",
            r#"
Bun.serve({
  port: Number(process.env.PORT || 3000),
  fetch(request) {
    const url = new URL(request.url);
    const path = url.pathname;
    const host = (request.headers.get("host") ?? url.host).split(":")[0]?.toLowerCase();
    if (host === "tako.internal" && path === "/status") {
      return new Response(JSON.stringify({ status: "ok" }), {
        headers: { "Content-Type": "application/json" },
      });
    }
    return new Response("v2");
  },
});
"#,
        );

        let deploy_v2 = env.send_command(&deploy_command(
            "versioned-app",
            "v2",
            &v2_dir,
            &["rolling.localhost"],
            2,
        ));
        assert_eq!(deploy_v2.get("status").and_then(|s| s.as_str()), Some("ok"));

        thread::sleep(Duration::from_secs(2));
        let after = env
            .http_get_with_host("/", "rolling.localhost")
            .expect("v2 request should succeed");
        assert!(after.contains("v2"), "expected v2 response, got: {}", after);
    }
}

mod error_handling {
    use super::*;

    #[test]
    fn test_invalid_command_handling() {
        if !can_bind_localhost() {
            return;
        }

        let env = E2EEnvironment::new();
        let response = env.send_raw_command("not json at all");

        let parsed: serde_json::Value =
            serde_json::from_str(&response).expect("response should be valid JSON");
        assert_eq!(parsed.get("status").and_then(|s| s.as_str()), Some("error"));
    }

    #[test]
    fn test_unknown_command_type() {
        if !can_bind_localhost() {
            return;
        }

        let env = E2EEnvironment::new();
        let response = env.send_command(&serde_json::json!({
            "command": "unknown_command_xyz",
            "data": "test"
        }));

        assert_eq!(
            response.get("status").and_then(|s| s.as_str()),
            Some("error")
        );
    }
}

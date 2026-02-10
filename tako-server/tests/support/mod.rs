use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::OnceLock;
use std::thread;
use std::time::{Duration, Instant};

use tempfile::TempDir;

fn workspace_root() -> PathBuf {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .parent()
        .map(Path::to_path_buf)
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

pub fn bun_ok() -> bool {
    Command::new("bun")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

pub fn wait_for<F>(timeout: Duration, mut f: F) -> bool
where
    F: FnMut() -> bool,
{
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if f() {
            return true;
        }
        thread::sleep(Duration::from_millis(100));
    }
    false
}

pub fn can_bind_local_ports() -> bool {
    TcpListener::bind("127.0.0.1:0").is_ok()
}

fn pick_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

#[allow(dead_code)]
pub struct TestServer {
    child: Option<Child>,
    pub socket_path: PathBuf,
    pub http_port: u16,
    pub tls_port: u16,
    _data_dir: TempDir,
}

#[allow(dead_code)]
fn test_http_runtime() -> &'static tokio::runtime::Runtime {
    static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("create test http runtime")
    })
}

#[allow(dead_code)]
impl TestServer {
    pub fn start() -> Self {
        let data_dir = TempDir::new().unwrap();
        let socket_path = data_dir.path().join("tako.sock");
        let http_port = pick_port();
        let tls_port = pick_port();

        let mut cmd = Command::new(env!("CARGO_BIN_EXE_tako-server"));
        cmd.args([
            "--socket",
            socket_path.to_string_lossy().as_ref(),
            "--data-dir",
            data_dir.path().to_string_lossy().as_ref(),
            "--port",
            &http_port.to_string(),
            "--tls-port",
            &tls_port.to_string(),
            "--no-acme",
        ])
        .env("RUST_LOG", "warn")
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
        apply_coverage_env(&mut cmd);
        let mut child = cmd.spawn().expect("failed to start tako-server");

        let deadline = Instant::now() + Duration::from_secs(10);
        let mut startup_error: Option<String> = None;
        while Instant::now() < deadline {
            if let Ok(Some(status)) = child.try_wait() {
                startup_error = Some(format!("tako-server exited early: {}", status));
                break;
            }

            if socket_path.exists() && UnixStream::connect(&socket_path).is_ok() {
                thread::sleep(Duration::from_millis(100));
                return Self {
                    child: Some(child),
                    socket_path,
                    http_port,
                    tls_port,
                    _data_dir: data_dir,
                };
            }

            thread::sleep(Duration::from_millis(100));
        }

        let _ = child.kill();
        let _ = child.wait();
        panic!(
            "{}",
            startup_error
                .unwrap_or_else(|| "tako-server socket never became available".to_string())
        );
    }

    pub fn send_command(&self, cmd: &serde_json::Value) -> serde_json::Value {
        let mut stream = UnixStream::connect(&self.socket_path).expect("connect unix socket");
        stream
            .set_read_timeout(Some(Duration::from_secs(120)))
            .unwrap();
        writeln!(stream, "{}", cmd).expect("write command");

        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line).expect("read response");
        serde_json::from_str(&line).unwrap()
    }

    pub fn http_get(&self, host: &str, path: &str) -> String {
        let mut stream =
            std::net::TcpStream::connect(("127.0.0.1", self.http_port)).expect("connect http");
        stream.set_read_timeout(Some(Duration::from_secs(10))).ok();

        let request = format!(
            "GET {} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
            path, host
        );
        stream.write_all(request.as_bytes()).unwrap();

        let mut buf = Vec::new();
        std::io::Read::read_to_end(&mut stream, &mut buf).unwrap();
        String::from_utf8_lossy(&buf).to_string()
    }

    pub fn https_get(&self, host: &str, path: &str) -> String {
        let url = format!("https://{}:{}{}", host, self.tls_port, path);
        let resolve = SocketAddr::from(([127, 0, 0, 1], self.tls_port));

        test_http_runtime().block_on(async move {
            let client = match reqwest::Client::builder()
                .danger_accept_invalid_certs(true)
                .resolve(host, resolve)
                .connect_timeout(Duration::from_secs(10))
                .timeout(Duration::from_secs(10))
                .build()
            {
                Ok(c) => c,
                Err(e) => return format!("https client error: {e}"),
            };

            let response = match client.get(url).send().await {
                Ok(r) => r,
                Err(e) => return format!("https request error: {e}"),
            };

            match response.text().await {
                Ok(text) => text,
                Err(e) => format!("https response body error: {e}"),
            }
        })
    }

    pub fn https_status(&self, host: &str, path: &str) -> Result<u16, String> {
        let url = format!("https://{}:{}{}", host, self.tls_port, path);
        let resolve = SocketAddr::from(([127, 0, 0, 1], self.tls_port));

        test_http_runtime().block_on(async move {
            let client = reqwest::Client::builder()
                .danger_accept_invalid_certs(true)
                .resolve(host, resolve)
                .connect_timeout(Duration::from_secs(10))
                .timeout(Duration::from_secs(10))
                .build()
                .map_err(|e| format!("https client error: {e}"))?;

            let response = client
                .get(url)
                .send()
                .await
                .map_err(|e| format!("https request error: {e}"))?;

            Ok(response.status().as_u16())
        })
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

pub fn write_bun_app(app_dir: &Path, body: &str) {
    fs::create_dir_all(app_dir.join("src")).unwrap();
    fs::write(
        app_dir.join("package.json"),
        r#"{"name":"test-app","scripts":{"dev":"bun src/index.ts"}}"#,
    )
    .unwrap();
    fs::write(
        app_dir.join("src/index.ts"),
        format!(
            r#"Bun.serve({{
  port: Number(process.env.PORT),
  fetch(req) {{
    const url = new URL(req.url);
    if (url.pathname === "/_tako/status") {{
      return new Response(JSON.stringify({{ healthy: true }}), {{
        headers: {{ "content-type": "application/json" }},
      }});
    }}
    if (url.pathname === "/") {{
      return new Response({:?}, {{ headers: {{ "content-type": "text/plain" }} }});
    }}
    return new Response("not found", {{ status: 404 }});
  }},
}});
"#,
            body
        ),
    )
    .unwrap();
}

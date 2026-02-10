use std::path::PathBuf;
use std::time::Duration;
use std::time::SystemTime;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

// Keep this above the daemon-side proxy bind wait window (~12s) so we can
// report daemon exit/log details instead of a generic connect timeout.
const DEV_SERVER_STARTUP_WAIT_ATTEMPTS: usize = 300;
const DEV_SERVER_STARTUP_WAIT_INTERVAL_MS: u64 = 50;

fn socket_path() -> Result<PathBuf, Box<dyn std::error::Error>> {
    Ok(crate::paths::tako_home_dir()?.join("dev-server.sock"))
}

fn dev_server_log_path() -> Result<PathBuf, Box<dyn std::error::Error>> {
    Ok(crate::paths::tako_home_dir()?.join("dev-server.log"))
}

fn open_dev_server_log(log_path: &std::path::Path) -> Result<std::fs::File, std::io::Error> {
    if let Some(parent) = log_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(log_path)
}

fn read_dev_server_log_tail(log_path: &std::path::Path, max_lines: usize) -> String {
    let Ok(contents) = std::fs::read_to_string(log_path) else {
        return String::new();
    };
    let lines: Vec<&str> = contents.lines().collect();
    let keep = lines.len().saturating_sub(max_lines);
    let tail = lines[keep..].join("\n");
    tail.trim().to_string()
}

fn format_dev_server_connect_error(
    log_path: &std::path::Path,
    status: Option<std::process::ExitStatus>,
) -> String {
    let tail = read_dev_server_log_tail(log_path, 40);
    let status_hint = status
        .map(|s| format!(" (daemon exited: {s})"))
        .unwrap_or_default();
    if tail.is_empty() {
        format!("could not connect to tako-dev-server{status_hint}")
    } else {
        format!("could not connect to tako-dev-server{status_hint}\nlast daemon log lines:\n{tail}")
    }
}

struct LineClient {
    reader: BufReader<tokio::net::unix::OwnedReadHalf>,
    writer: tokio::net::unix::OwnedWriteHalf,
}

impl LineClient {
    fn new(stream: UnixStream) -> Self {
        let (r, w) = stream.into_split();
        Self {
            reader: BufReader::new(r),
            writer: w,
        }
    }

    async fn send_line(&mut self, s: &str) -> Result<(), Box<dyn std::error::Error>> {
        self.writer.write_all(s.as_bytes()).await?;
        self.writer.write_all(b"\n").await?;
        Ok(())
    }

    async fn read_line(&mut self) -> Result<String, Box<dyn std::error::Error>> {
        let mut line = String::new();
        self.reader.read_line(&mut line).await?;
        Ok(line)
    }
}

#[derive(Debug, Clone)]
pub struct LeaseInfo {
    pub lease_id: String,
    pub url: String,
}

#[derive(Debug, Clone)]
pub struct ListedApp {
    pub app_name: String,
    pub hosts: Vec<String>,
    pub upstream_port: u16,
    pub pid: Option<u32>,
}

pub async fn ensure_running(
    listen_addr: &str,
    dns_ip: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let sock = socket_path()?;
    let log_path = dev_server_log_path().unwrap_or_else(|_| PathBuf::from("dev-server.log"));

    if let Ok(stream) = UnixStream::connect(&sock).await {
        let mut c = LineClient::new(stream);
        ping(&mut c).await?;
        return Ok(());
    }

    // If we can't connect to the daemon, we're about to spawn one. Avoid noisy
    // daemon stderr output by checking bind errors ourselves.
    if let Err(e) = std::net::TcpListener::bind(listen_addr) {
        if e.kind() == std::io::ErrorKind::AddrInUse {
            return Err(format!("dev server listen {} is already in use", listen_addr).into());
        }
        return Err(format!("dev server listen {} is not available: {}", listen_addr, e).into());
    }

    let mut child = spawn_dev_server(listen_addr, dns_ip, &log_path)?;
    for _ in 0..DEV_SERVER_STARTUP_WAIT_ATTEMPTS {
        tokio::time::sleep(Duration::from_millis(DEV_SERVER_STARTUP_WAIT_INTERVAL_MS)).await;
        if let Ok(stream) = UnixStream::connect(&sock).await {
            let mut c = LineClient::new(stream);
            ping(&mut c).await?;
            return Ok(());
        }
        if let Some(status) = child.try_wait()? {
            return Err(format_dev_server_connect_error(&log_path, Some(status)).into());
        }
    }

    if let Some(status) = child.try_wait()? {
        return Err(format_dev_server_connect_error(&log_path, Some(status)).into());
    }

    Err(format_dev_server_connect_error(&log_path, None).into())
}

fn spawn_dev_server(
    listen_addr: &str,
    dns_ip: &str,
    log_path: &std::path::Path,
) -> Result<std::process::Child, Box<dyn std::error::Error>> {
    use std::process::Stdio;

    // Try repo-local target paths first when running from a source checkout.
    if let Ok(exe) = std::env::current_exe()
        && let Some(root) = crate::paths::repo_root_from_exe(&exe)
    {
        let candidates = repo_local_dev_server_candidates(&root);
        if repo_local_dev_server_build_needed(
            file_modified_time(&exe),
            file_modified_time(&candidates[0]),
        ) {
            let _ = maybe_build_repo_local_dev_server(&root);
        }

        for cand in candidates {
            if cand.exists() {
                let log_file = open_dev_server_log(log_path)?;
                let log_file_err = log_file.try_clone()?;
                let child = std::process::Command::new(cand)
                    .args(["--listen", listen_addr, "--dns-ip", dns_ip])
                    .stdin(Stdio::null())
                    .stdout(Stdio::from(log_file))
                    .stderr(Stdio::from(log_file_err))
                    .spawn()?;
                return Ok(child);
            }
        }
    }

    // Fall back to PATH.
    let log_file = open_dev_server_log(log_path)?;
    let log_file_err = log_file.try_clone()?;
    match std::process::Command::new("tako-dev-server")
        .args(["--listen", listen_addr, "--dns-ip", dns_ip])
        .stdin(Stdio::null())
        .stdout(Stdio::from(log_file))
        .stderr(Stdio::from(log_file_err))
        .spawn()
    {
        Ok(child) => Ok(child),
        Err(e) => Err(format!(
            "failed to spawn 'tako-dev-server' ({}). If you're running from a source checkout, build it with: cargo build -p tako-dev-server",
            e
        )
        .into()),
    }
}

fn repo_local_dev_server_candidates(root: &std::path::Path) -> [PathBuf; 2] {
    [
        root.join("target").join("debug").join("tako-dev-server"),
        root.join("target").join("release").join("tako-dev-server"),
    ]
}

fn file_modified_time(path: &std::path::Path) -> Option<SystemTime> {
    std::fs::metadata(path).ok()?.modified().ok()
}

fn repo_local_dev_server_build_needed(
    tako_modified: Option<SystemTime>,
    dev_server_modified: Option<SystemTime>,
) -> bool {
    match (tako_modified, dev_server_modified) {
        (_, None) => true,
        (Some(tako), Some(dev_server)) => dev_server < tako,
        (None, Some(_)) => false,
    }
}

fn maybe_build_repo_local_dev_server(root: &std::path::Path) -> std::io::Result<()> {
    std::process::Command::new("cargo")
        .args(["build", "-p", "tako-dev-server"])
        .current_dir(root)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|_| ())
}

async fn ping(c: &mut LineClient) -> Result<(), Box<dyn std::error::Error>> {
    c.send_line(r#"{"type":"Ping"}"#).await?;
    let line = c.read_line().await?;
    if line.trim() == r#"{"type":"Pong"}"# {
        return Ok(());
    }
    Err(format!("unexpected response: {}", line).into())
}

pub async fn get_token() -> Result<String, Box<dyn std::error::Error>> {
    let sock = socket_path()?;
    let stream = UnixStream::connect(&sock).await?;
    let mut c = LineClient::new(stream);
    c.send_line(r#"{"type":"GetToken"}"#).await?;
    let line = c.read_line().await?;
    let v: serde_json::Value = serde_json::from_str(&line)?;
    match v.get("type").and_then(|t| t.as_str()) {
        Some("Token") => Ok(v
            .get("token")
            .and_then(|t| t.as_str())
            .unwrap_or("")
            .to_string()),
        Some("Error") => Err(format!("dev-server error: {}", v).into()),
        _ => Err(format!("unexpected response: {}", line).into()),
    }
}

pub async fn register_lease(
    token: &str,
    app_name: &str,
    hosts: &[String],
    upstream_port: u16,
    active: bool,
    ttl_ms: u64,
) -> Result<LeaseInfo, Box<dyn std::error::Error>> {
    let sock = socket_path()?;
    let stream = UnixStream::connect(&sock).await?;
    let mut c = LineClient::new(stream);
    let req = serde_json::json!({
        "type": "RegisterLease",
        "token": token,
        "app_name": app_name,
        "hosts": hosts,
        "upstream_port": upstream_port,
        "active": active,
        "ttl_ms": ttl_ms,
    });
    c.send_line(&req.to_string()).await?;
    let line = c.read_line().await?;
    let v: serde_json::Value = serde_json::from_str(&line)?;
    match v.get("type").and_then(|t| t.as_str()) {
        Some("LeaseRegistered") => Ok(LeaseInfo {
            lease_id: v
                .get("lease_id")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string(),
            url: v
                .get("url")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string(),
        }),
        Some("Error") => Err(format!("dev-server error: {}", v).into()),
        _ => Err(format!("unexpected response: {}", line).into()),
    }
}

pub async fn set_lease_active(
    token: &str,
    lease_id: &str,
    active: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let sock = socket_path()?;
    let stream = UnixStream::connect(&sock).await?;
    let mut c = LineClient::new(stream);
    let req = serde_json::json!({
        "type": "SetLeaseActive",
        "token": token,
        "lease_id": lease_id,
        "active": active,
    });
    c.send_line(&req.to_string()).await?;
    let line = c.read_line().await?;
    let v: serde_json::Value = serde_json::from_str(&line)?;
    match v.get("type").and_then(|t| t.as_str()) {
        Some("LeaseRenewed") => Ok(()),
        Some("Error") => Err(format!("dev-server error: {}", v).into()),
        _ => Err(format!("unexpected response: {}", line).into()),
    }
}

#[derive(Debug, Clone)]
pub enum DevServerEvent {
    RequestStarted { host: String },
    RequestFinished { host: String },
}

pub async fn subscribe_events(
    token: &str,
) -> Result<tokio::sync::mpsc::UnboundedReceiver<DevServerEvent>, Box<dyn std::error::Error>> {
    let sock = socket_path()?;
    let stream = UnixStream::connect(&sock).await?;
    let mut c = LineClient::new(stream);
    let req = serde_json::json!({
        "type": "SubscribeEvents",
        "token": token,
    });
    c.send_line(&req.to_string()).await?;

    // Wait for Subscribed.
    let line = c.read_line().await?;
    let v: serde_json::Value = serde_json::from_str(&line)?;
    match v.get("type").and_then(|t| t.as_str()) {
        Some("Subscribed") => {}
        Some("Error") => return Err(format!("dev-server error: {}", v).into()),
        _ => return Err(format!("unexpected response: {}", line).into()),
    }

    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        loop {
            let line = match c.read_line().await {
                Ok(l) => l,
                Err(_) => break,
            };
            if line.trim().is_empty() {
                continue;
            }
            let v: serde_json::Value = match serde_json::from_str(&line) {
                Ok(v) => v,
                Err(_) => continue,
            };
            if v.get("type").and_then(|t| t.as_str()) != Some("Event") {
                continue;
            }
            let ev = match v
                .get("event")
                .and_then(|e| e.get("type"))
                .and_then(|t| t.as_str())
            {
                Some("RequestStarted") => {
                    let host = v
                        .get("event")
                        .and_then(|e| e.get("host"))
                        .and_then(|h| h.as_str())
                        .unwrap_or("")
                        .to_string();
                    DevServerEvent::RequestStarted { host }
                }
                Some("RequestFinished") => {
                    let host = v
                        .get("event")
                        .and_then(|e| e.get("host"))
                        .and_then(|h| h.as_str())
                        .unwrap_or("")
                        .to_string();
                    DevServerEvent::RequestFinished { host }
                }
                _ => continue,
            };
            let _ = tx.send(ev);
        }
    });

    Ok(rx)
}

pub async fn renew_lease(
    token: &str,
    lease_id: &str,
    ttl_ms: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    let sock = socket_path()?;
    let stream = UnixStream::connect(&sock).await?;
    let mut c = LineClient::new(stream);
    let req = serde_json::json!({
        "type": "RenewLease",
        "token": token,
        "lease_id": lease_id,
        "ttl_ms": ttl_ms,
    });
    c.send_line(&req.to_string()).await?;
    let line = c.read_line().await?;
    let v: serde_json::Value = serde_json::from_str(&line)?;
    match v.get("type").and_then(|t| t.as_str()) {
        Some("LeaseRenewed") => Ok(()),
        Some("Error") => Err(format!("dev-server error: {}", v).into()),
        _ => Err(format!("unexpected response: {}", line).into()),
    }
}

pub async fn unregister_lease(
    token: &str,
    lease_id: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let sock = socket_path()?;
    let stream = UnixStream::connect(&sock).await?;
    let mut c = LineClient::new(stream);
    let req = serde_json::json!({
        "type": "UnregisterLease",
        "token": token,
        "lease_id": lease_id,
    });
    c.send_line(&req.to_string()).await?;
    let line = c.read_line().await?;
    let v: serde_json::Value = serde_json::from_str(&line)?;
    match v.get("type").and_then(|t| t.as_str()) {
        Some("LeaseUnregistered") => Ok(()),
        Some("Error") => Err(format!("dev-server error: {}", v).into()),
        _ => Err(format!("unexpected response: {}", line).into()),
    }
}

pub async fn list_apps() -> Result<Vec<ListedApp>, Box<dyn std::error::Error>> {
    let sock = socket_path()?;
    let stream = UnixStream::connect(&sock).await?;
    let mut c = LineClient::new(stream);
    c.send_line(r#"{"type":"ListApps"}"#).await?;
    let line = c.read_line().await?;
    let v: serde_json::Value = serde_json::from_str(&line)?;
    if v.get("type").and_then(|t| t.as_str()) != Some("Apps") {
        return Err(format!("unexpected response: {}", line).into());
    }
    let apps = v
        .get("apps")
        .and_then(|a| a.as_array())
        .cloned()
        .unwrap_or_default();
    Ok(apps
        .into_iter()
        .filter_map(|a| {
            let hosts = a
                .get("hosts")
                .and_then(|h| h.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(|s| s.to_string()))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            Some(ListedApp {
                app_name: a.get("app_name")?.as_str()?.to_string(),
                hosts,
                upstream_port: a.get("upstream_port")?.as_u64()? as u16,
                pid: a.get("pid").and_then(|p| p.as_u64()).map(|p| p as u32),
            })
        })
        .collect())
}

pub async fn info() -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let sock = socket_path()?;
    let stream = UnixStream::connect(&sock).await?;
    let mut c = LineClient::new(stream);
    c.send_line(r#"{"type":"Info"}"#).await?;
    let line = c.read_line().await?;
    Ok(serde_json::from_str(&line)?)
}

pub async fn stop_server() -> Result<(), Box<dyn std::error::Error>> {
    let sock = socket_path()?;
    let stream = UnixStream::connect(&sock).await?;
    let mut c = LineClient::new(stream);
    c.send_line(r#"{"type":"StopServer"}"#).await?;
    let line = c.read_line().await?;
    let v: serde_json::Value = serde_json::from_str(&line)?;
    match v.get("type").and_then(|t| t.as_str()) {
        Some("Stopping") => Ok(()),
        Some("Error") => Err(format!("dev-server error: {}", v).into()),
        _ => Err(format!("unexpected response: {}", line).into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repo_local_dev_server_candidates_prefers_debug_then_release() {
        let root = std::path::Path::new("/tmp/tako");
        let candidates = repo_local_dev_server_candidates(root);
        assert_eq!(
            candidates[0],
            PathBuf::from("/tmp/tako/target/debug/tako-dev-server")
        );
        assert_eq!(
            candidates[1],
            PathBuf::from("/tmp/tako/target/release/tako-dev-server")
        );
    }

    #[test]
    fn repo_local_dev_server_build_needed_when_binary_missing() {
        assert!(repo_local_dev_server_build_needed(
            Some(SystemTime::now()),
            None
        ));
    }

    #[test]
    fn repo_local_dev_server_build_needed_when_daemon_is_older() {
        let now = SystemTime::now();
        let newer = now;
        let older = now.checked_sub(Duration::from_secs(1)).unwrap();
        assert!(repo_local_dev_server_build_needed(Some(newer), Some(older)));
    }

    #[test]
    fn repo_local_dev_server_build_not_needed_when_daemon_is_newer() {
        let now = SystemTime::now();
        let older = now.checked_sub(Duration::from_secs(1)).unwrap();
        let newer = now;
        assert!(!repo_local_dev_server_build_needed(
            Some(older),
            Some(newer)
        ));
    }

    #[test]
    fn daemon_startup_wait_is_15_seconds() {
        assert_eq!(
            (DEV_SERVER_STARTUP_WAIT_ATTEMPTS as u64) * DEV_SERVER_STARTUP_WAIT_INTERVAL_MS,
            15_000
        );
    }

    #[test]
    fn read_dev_server_log_tail_returns_last_lines_only() {
        let tmp = std::env::temp_dir().join(format!(
            "tako-dev-server-log-tail-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&tmp, "l1\nl2\nl3\nl4\n").unwrap();
        let tail = read_dev_server_log_tail(&tmp, 2);
        let _ = std::fs::remove_file(&tmp);
        assert_eq!(tail, "l3\nl4");
    }

    #[test]
    fn format_dev_server_connect_error_includes_log_tail_when_present() {
        let tmp = std::env::temp_dir().join(format!(
            "tako-dev-server-log-error-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&tmp, "boom\n").unwrap();
        let msg = format_dev_server_connect_error(&tmp, None);
        let _ = std::fs::remove_file(&tmp);
        assert!(msg.contains("could not connect to tako-dev-server"));
        assert!(msg.contains("boom"));
    }

    #[test]
    fn format_dev_server_connect_error_without_log_is_brief() {
        let tmp = std::env::temp_dir().join(format!(
            "tako-dev-server-log-missing-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let msg = format_dev_server_connect_error(&tmp, None);
        assert_eq!(msg, "could not connect to tako-dev-server");
    }

    #[test]
    fn format_dev_server_connect_error_without_log_includes_exit_status() {
        let tmp = std::env::temp_dir().join(format!(
            "tako-dev-server-log-status-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let status = std::process::Command::new("sh")
            .args(["-c", "exit 9"])
            .status()
            .unwrap();
        let msg = format_dev_server_connect_error(&tmp, Some(status));
        assert!(msg.contains("daemon exited"));
    }
}

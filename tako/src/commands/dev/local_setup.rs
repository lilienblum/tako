use std::time::Duration;

#[cfg(target_os = "macos")]
use std::path::Path;

#[cfg(test)]
use tokio::time::timeout;

use crate::dev::LocalCAStore;

#[cfg(any(target_os = "macos", test))]
pub(super) fn local_dns_resolver_contents(port: u16) -> String {
    format!("nameserver 127.0.0.1\nport {port}\n")
}

#[cfg(any(target_os = "macos", test))]
pub(super) fn parse_local_dns_resolver(contents: &str) -> (Option<String>, Option<u16>) {
    let mut nameserver = None;
    let mut port = None;

    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        let mut parts = trimmed.split_whitespace();
        let key = parts.next().unwrap_or_default();
        let value = parts.next().unwrap_or_default();
        match key {
            "nameserver" if !value.is_empty() => nameserver = Some(value.to_string()),
            "port" => {
                if let Ok(v) = value.parse::<u16>() {
                    port = Some(v);
                }
            }
            _ => {}
        }
    }

    (nameserver, port)
}

#[cfg(target_os = "macos")]
pub(super) fn sudo_run_checked(
    args: &[&str],
    context: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let status = std::process::Command::new("sudo").args(args).status()?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{context} failed").into())
    }
}

#[cfg(target_os = "macos")]
pub(super) fn write_system_file_with_sudo(
    path: &str,
    content: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let tmp = std::env::temp_dir().join(format!(
        "tako-dns-resolver-{}-{}",
        std::process::id(),
        unique
    ));
    std::fs::write(&tmp, content)?;
    let tmp_str = tmp.to_string_lossy().to_string();
    let install_args = ["install", "-m", "644", tmp_str.as_str(), path];
    let result = sudo_run_checked(&install_args, &format!("installing {path}"));
    let _ = std::fs::remove_file(&tmp);
    result
}

#[cfg(target_os = "macos")]
fn resolver_file_matches(path: &str, port: u16) -> bool {
    let Ok(contents) = std::fs::read_to_string(path) else {
        return false;
    };
    let (nameserver, configured_port) = parse_local_dns_resolver(&contents);
    nameserver.as_deref() == Some("127.0.0.1") && configured_port == Some(port)
}

#[cfg(target_os = "macos")]
fn local_dns_resolver_configured(port: u16) -> bool {
    resolver_file_matches(super::TAKO_RESOLVER_FILE, port)
}

#[cfg(target_os = "macos")]
fn short_dns_resolver_configured(port: u16) -> bool {
    resolver_file_matches(super::SHORT_RESOLVER_FILE, port)
}

#[cfg(target_os = "macos")]
pub(super) fn ensure_local_dns_resolver_configured(
    port: u16,
) -> Result<bool, Box<dyn std::error::Error>> {
    let tako_ok = local_dns_resolver_configured(port);
    let short_ok = short_dns_resolver_configured(port);

    if tako_ok && short_ok {
        return Ok(true);
    }

    if !tako_ok && !crate::output::is_interactive() && !crate::output::is_root() {
        return Err(format!(
            "local DNS resolver is not configured at {}; run `tako dev` interactively once to install it",
            super::TAKO_RESOLVER_FILE
        )
        .into());
    }

    crate::output::info("Configuring local DNS resolver (sudo)...");

    sudo_run_checked(
        &["install", "-d", "-m", "755", super::RESOLVER_DIR],
        "creating /etc/resolver",
    )?;

    if !tako_ok {
        write_system_file_with_sudo(
            super::TAKO_RESOLVER_FILE,
            &local_dns_resolver_contents(port),
        )?;

        if !local_dns_resolver_configured(port) {
            return Err("local DNS resolver setup verification failed".into());
        }
    }

    let short_active = if short_ok {
        true
    } else if !Path::new(super::SHORT_RESOLVER_FILE).exists() {
        write_system_file_with_sudo(
            super::SHORT_RESOLVER_FILE,
            &local_dns_resolver_contents(port),
        )?;
        short_dns_resolver_configured(port)
    } else if crate::output::is_interactive() {
        crate::output::warning(
            "Another tool owns /etc/resolver/test. Override it for shorter *.test URLs?",
        );
        if crate::output::confirm("Override /etc/resolver/test?", false).unwrap_or(false) {
            write_system_file_with_sudo(
                super::SHORT_RESOLVER_FILE,
                &local_dns_resolver_contents(port),
            )?;
            short_dns_resolver_configured(port)
        } else {
            crate::output::muted("Skipped — using *.tako.test URLs instead.");
            false
        }
    } else {
        false
    };

    if short_active {
        crate::output::success("Local DNS resolver configured for *.test and *.tako.test.");
    } else {
        crate::output::success("Local DNS resolver configured for *.tako.test.");
    }

    Ok(short_active)
}

#[cfg(not(target_os = "macos"))]
pub(super) fn ensure_local_dns_resolver_configured(
    _port: u16,
) -> Result<bool, Box<dyn std::error::Error>> {
    Ok(true)
}

#[cfg(target_os = "linux")]
pub(super) fn explain_pending_sudo_setup(_port: u16) -> Result<(), Box<dyn std::error::Error>> {
    if !crate::output::is_interactive() {
        return Ok(());
    }

    let ca_action = super::ca_setup::pending_sudo_action()?;
    let redirect_action = super::linux_setup::pending_sudo_action()?;

    let mut items = Vec::new();
    if let Some(action) = ca_action {
        items.push(action.to_string());
    }
    if let Some(action) = redirect_action {
        items.push(action.to_string());
    }
    if items.is_empty() {
        return Ok(());
    }

    crate::output::warning("One-time sudo is required for:");
    for item in &items {
        crate::output::bullet(item);
    }

    let status = std::process::Command::new("sudo")
        .arg("-v")
        .status()
        .map_err(|e| -> Box<dyn std::error::Error> { format!("failed to run sudo: {e}").into() })?;
    if !status.success() {
        return Err(crate::output::silent_exit_error().into());
    }

    Ok(())
}

#[cfg(any(target_os = "macos", test))]
pub(super) fn local_dns_sudo_action_line() -> &'static str {
    "Configure local DNS for *.test and *.tako.test"
}

#[cfg(any(target_os = "macos", test))]
pub(super) fn sudo_setup_action_items(
    ca_action: Option<&str>,
    local_dns_needed: bool,
    loopback_proxy_action: Option<&str>,
) -> Vec<String> {
    let mut items = Vec::new();
    if let Some(action) = ca_action {
        items.push(action.to_string());
    }
    if local_dns_needed {
        items.push(local_dns_sudo_action_line().to_string());
    }
    if let Some(action) = loopback_proxy_action {
        items.push(action.to_string());
    }
    items
}

#[cfg(target_os = "macos")]
pub(super) fn explain_pending_sudo_setup(port: u16) -> Result<(), Box<dyn std::error::Error>> {
    if !crate::output::is_interactive() {
        return Ok(());
    }

    let dns_needed = !local_dns_resolver_configured(port) || !short_dns_resolver_configured(port);
    let items = sudo_setup_action_items(
        super::ca_setup::pending_sudo_action()?,
        dns_needed,
        super::loopback_proxy::pending_sudo_action()?,
    );
    if items.is_empty() {
        return Ok(());
    }

    crate::output::warning("One-time sudo is required for:");
    for item in items {
        crate::output::bullet(&item);
    }

    let status = std::process::Command::new("sudo")
        .arg("-v")
        .status()
        .map_err(|e| -> Box<dyn std::error::Error> { format!("failed to run sudo: {e}").into() })?;
    if !status.success() {
        return Err(crate::output::silent_exit_error().into());
    }

    Ok(())
}

#[cfg(target_os = "macos")]
pub(crate) fn tcp_port_open(ip: &str, port: u16, timeout_ms: u64) -> bool {
    use std::net::{Ipv4Addr, SocketAddr};
    let Ok(ipv4) = ip.parse::<Ipv4Addr>() else {
        return false;
    };
    let addr = SocketAddr::from((ipv4, port));
    std::net::TcpStream::connect_timeout(&addr, Duration::from_millis(timeout_ms)).is_ok()
}

pub(super) async fn localhost_https_host_reachable_via_ip(
    host: &str,
    connect_ip: std::net::Ipv4Addr,
    port: u16,
    timeout_ms: u64,
) -> Result<(), String> {
    let store = match LocalCAStore::new() {
        Ok(store) => store,
        Err(e) => return Err(format!("failed to open local CA store: {e}")),
    };
    let cert_pem = match std::fs::read(store.ca_cert_path()) {
        Ok(bytes) => bytes,
        Err(e) => return Err(format!("failed to read local CA cert: {e}")),
    };
    let cert = match reqwest::Certificate::from_pem(&cert_pem) {
        Ok(cert) => cert,
        Err(e) => return Err(format!("failed to parse local CA cert: {e}")),
    };

    let mut base_url = format!("https://{host}");
    if port != 443 {
        base_url.push(':');
        base_url.push_str(&port.to_string());
    }
    base_url.push('/');

    let addr = std::net::SocketAddr::from((connect_ip, port));
    let client = match reqwest::Client::builder()
        .add_root_certificate(cert)
        .connect_timeout(Duration::from_millis(timeout_ms))
        .timeout(Duration::from_millis(timeout_ms))
        .resolve(host, addr)
        .build()
    {
        Ok(client) => client,
        Err(e) => return Err(format!("failed to build HTTPS probe client: {e}")),
    };

    client
        .get(base_url)
        .send()
        .await
        .map(|_| ())
        .map_err(|e| e.to_string())
}

pub(super) async fn wait_for_https_host_reachable_via_ip(
    host: &str,
    connect_ip: std::net::Ipv4Addr,
    port: u16,
    attempts: usize,
    timeout_ms: u64,
    retry_delay_ms: u64,
) -> Result<(), String> {
    let mut last_error = "probe did not return a response".to_string();
    for attempt in 0..attempts {
        match localhost_https_host_reachable_via_ip(host, connect_ip, port, timeout_ms).await {
            Ok(()) => return Ok(()),
            Err(e) => last_error = e,
        }
        if attempt + 1 < attempts {
            tokio::time::sleep(Duration::from_millis(retry_delay_ms)).await;
        }
    }
    Err(last_error)
}

#[cfg(target_os = "macos")]
pub(super) fn local_https_probe_error(
    host: &str,
    public_port: u16,
    loopback_error: &str,
    server_directly_reachable: bool,
) -> String {
    if server_directly_reachable {
        format!(
            "Local HTTPS endpoint is unreachable at https://{host}/ ({loopback_error}). \
             Tako dev server is reachable directly on 127.0.0.1:{public_port}, so the local launchd loopback proxy is not forwarding correctly. \
             Check `launchctl print system/{}`, then re-run `tako dev`.",
            super::LOOPBACK_PROXY_LABEL
        )
    } else {
        format!(
            "Local HTTPS endpoint is unreachable at https://{host}/ ({loopback_error}). \
             Check that the local loopback proxy is loaded (`tako doctor`) and try again."
        )
    }
}

pub(super) fn local_https_probe_host(primary_host: &str) -> &str {
    primary_host
}

#[cfg(test)]
pub(super) async fn tcp_probe(addr: (&str, u16), timeout_ms: u64) -> bool {
    timeout(
        std::time::Duration::from_millis(timeout_ms),
        tokio::net::TcpStream::connect(addr),
    )
    .await
    .is_ok_and(|r| r.is_ok())
}

pub(crate) fn is_dev_server_unavailable_error_message(message: &str) -> bool {
    let normalized = message.to_ascii_lowercase();
    normalized.contains("connection refused")
        || normalized.contains("no such file or directory")
        || normalized.contains("operation not permitted")
        || normalized.contains("permission denied")
}

#[cfg(target_os = "macos")]
pub(crate) fn local_dns_resolver_values() -> Option<(String, u16)> {
    let contents = std::fs::read_to_string(super::TAKO_RESOLVER_FILE).ok()?;
    let (nameserver, port) = parse_local_dns_resolver(&contents);
    Some((nameserver?, port?))
}

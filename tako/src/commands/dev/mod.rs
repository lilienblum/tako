//! Tako Dev Server
//!
//! Local development server with:
//! - HTTPS via local CA (`{app-name}.tako.local`)
//! - Local authoritative DNS for `*.tako.local`
//! - `tako.toml` watching for env/route updates
//! - TUI with logs, status, and resource monitoring
//! - Idle timeout (stops app process after inactivity)

mod ca_setup;
mod tui;
mod watcher;

use std::env::current_dir;
use std::io::IsTerminal;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
#[cfg(target_os = "macos")]
use std::process::Command;
use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use time::{OffsetDateTime, UtcOffset};

use sha2::Digest;
use std::fs::OpenOptions;
use std::io::Write;
use tokio::io::{AsyncBufReadExt, AsyncSeekExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio::sync::watch;
#[cfg(test)]
use tokio::time::timeout;

use crate::build::{BuildPreset, PresetReference, load_build_preset, parse_preset_reference};
use crate::config::TakoToml;
use crate::dev::LocalCA;
#[cfg(target_os = "macos")]
use crate::dev::LocalCAStore;
use crate::validation::validate_dev_route;

pub use ca_setup::setup_local_ca;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum LogLevel {
    Debug,
    Info,
    Warn,
    Error,
    Fatal,
}

impl std::fmt::Display for LogLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            LogLevel::Debug => "DEBUG",
            LogLevel::Info => "INFO",
            LogLevel::Warn => "WARN",
            LogLevel::Error => "ERROR",
            LogLevel::Fatal => "FATAL",
        };
        f.pad(s)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ScopedLog {
    pub timestamp: String,
    pub level: LogLevel,
    pub scope: String,
    pub message: String,
}

#[derive(Debug, Deserialize)]
struct ScopedLogSerde {
    #[serde(default)]
    timestamp: Option<String>,
    #[serde(default)]
    h: Option<u8>,
    #[serde(default)]
    m: Option<u8>,
    #[serde(default)]
    s: Option<u8>,
    level: LogLevel,
    scope: String,
    message: String,
}

fn hms_timestamp(h: u8, m: u8, s: u8) -> String {
    format!("{:02}:{:02}:{:02}", h, m, s)
}

impl<'de> Deserialize<'de> for ScopedLog {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = ScopedLogSerde::deserialize(deserializer)?;
        let timestamp = raw
            .timestamp
            .filter(|s| !s.trim().is_empty())
            .or_else(|| {
                raw.h
                    .zip(raw.m)
                    .zip(raw.s)
                    .map(|((h, m), s)| hms_timestamp(h, m, s))
            })
            .unwrap_or_else(|| "00:00:00".to_string());

        Ok(Self {
            timestamp,
            level: raw.level,
            scope: raw.scope,
            message: raw.message,
        })
    }
}

static LOCAL_OFFSET: OnceLock<UtcOffset> = OnceLock::new();

fn local_offset() -> UtcOffset {
    *LOCAL_OFFSET.get_or_init(|| UtcOffset::current_local_offset().unwrap_or(UtcOffset::UTC))
}

impl ScopedLog {
    pub fn at(level: LogLevel, scope: impl Into<String>, message: impl Into<String>) -> Self {
        let now = OffsetDateTime::now_utc().to_offset(local_offset());
        Self {
            timestamp: hms_timestamp(now.hour() as u8, now.minute() as u8, now.second() as u8),
            level,
            scope: scope.into(),
            message: message.into(),
        }
    }

    pub fn info(scope: impl Into<String>, message: impl Into<String>) -> Self {
        Self::at(LogLevel::Info, scope, message)
    }

    pub fn warn(scope: impl Into<String>, message: impl Into<String>) -> Self {
        Self::at(LogLevel::Warn, scope, message)
    }

    pub fn error(scope: impl Into<String>, message: impl Into<String>) -> Self {
        Self::at(LogLevel::Error, scope, message)
    }
}

const DEV_SERVER_SCOPE: &str = "tako";
const APP_SCOPE: &str = "app";

fn dev_server_starting_log() -> ScopedLog {
    ScopedLog::info(DEV_SERVER_SCOPE, "Starting dev server")
}

fn dev_server_ready_log(port: u16) -> ScopedLog {
    ScopedLog::info(
        DEV_SERVER_SCOPE,
        format!("Dev server listening on localhost:{}", port),
    )
}

fn app_log_scope() -> String {
    APP_SCOPE.to_string()
}

const LEASE_TTL_MS: u64 = 30_000;
const LEASE_HEARTBEAT_SECS: u64 = 5;
const DEV_INITIAL_INSTANCE_COUNT: usize = 1;
const DEV_IDLE_TIMEOUT_SECS: u64 = 30 * 60;
const DEV_DISCONNECT_GRACE_RELEASE_SECS: u64 = 10 * 60;
const DEV_DISCONNECT_GRACE_DEBUG_SECS: u64 = 10;
const LOCALHOST_443_HTTPS_PROBE_ATTEMPTS: usize = 12;
const LOCALHOST_443_HTTPS_PROBE_TIMEOUT_MS: u64 = 500;
const LOCALHOST_443_HTTPS_PROBE_RETRY_DELAY_MS: u64 = 150;
const LOOPBACK_PORT_PROBE_ATTEMPTS: usize = 10;
const LOOPBACK_PORT_PROBE_TIMEOUT_MS: u64 = 150;
const LOOPBACK_PORT_PROBE_RETRY_DELAY_MS: u64 = 50;
const DEV_LOG_TAIL_POLL_MS: u64 = 120;
const DEV_LOG_CLEAR_MARKER_TYPE: &str = "clear_logs";
const DEV_LOG_APP_EVENT_MARKER_TYPE: &str = "app_event";
const DEV_LOCK_ACQUIRE_TIMEOUT_MS: u64 = 1_200;
const DEV_LOCK_RETRY_POLL_MS: u64 = 20;
const DEV_LOCK_INCOMPLETE_STALE_AGE_MS: u64 = 2_000;
const LOCAL_DNS_PORT: u16 = 53535;
#[cfg(any(target_os = "macos", test))]
const RESOLVER_DIR: &str = "/etc/resolver";
#[cfg(any(target_os = "macos", test))]
const TAKO_RESOLVER_FILE: &str = "/etc/resolver/tako.local";
const DEV_LOOPBACK_ADDR: &str = "127.77.0.1";
#[cfg(any(target_os = "macos", test))]
const LOCAL_HTTP_REDIRECT_PORT: u16 = 47830;
const DEV_TLS_CERT_FILENAME: &str = "fullchain.pem";
const DEV_TLS_KEY_FILENAME: &str = "privkey.pem";

fn dev_initial_instance_count() -> usize {
    DEV_INITIAL_INSTANCE_COUNT
}

fn dev_idle_timeout() -> Duration {
    Duration::from_secs(DEV_IDLE_TIMEOUT_SECS)
}

fn dev_disconnect_grace_secs() -> u64 {
    if cfg!(debug_assertions) {
        DEV_DISCONNECT_GRACE_DEBUG_SECS
    } else {
        DEV_DISCONNECT_GRACE_RELEASE_SECS
    }
}

fn dev_disconnect_grace() -> Duration {
    Duration::from_secs(dev_disconnect_grace_secs())
}

fn dev_disconnect_grace_ttl_ms() -> u64 {
    dev_disconnect_grace_secs().saturating_mul(1000)
}

fn dev_disconnect_grace_label() -> &'static str {
    if cfg!(debug_assertions) {
        "10 seconds"
    } else {
        "10 minutes"
    }
}

fn should_linger_after_disconnect(terminate_requested: bool, app_was_running: bool) -> bool {
    !terminate_requested && app_was_running
}

fn load_dev_tako_toml(project_dir: &Path) -> crate::config::Result<TakoToml> {
    TakoToml::load_from_dir(project_dir)
}

fn compute_dev_hosts(
    app_name: &str,
    cfg: &TakoToml,
    default_host: &str,
) -> Result<Vec<String>, String> {
    let routes = match cfg.get_routes("development") {
        Some(routes) if !routes.is_empty() => routes,
        _ => return Ok(vec![default_host.to_string()]),
    };

    let mut out = Vec::new();
    for r in routes {
        validate_dev_route(&r, app_name).map_err(|e| e.to_string())?;
        let host = r.split('/').next().unwrap_or("");
        if host.starts_with("*.") || host.contains('*') {
            // The dev server currently does exact Host matching only.
            continue;
        }
        if !host.is_empty() {
            out.push(host.to_string());
        }
    }

    out.sort();
    out.dedup();
    if out.is_empty() {
        Err(
            "development routes are configured but none are exact hostnames; wildcard hosts are not supported in `tako dev` yet"
                .to_string(),
        )
    } else {
        Ok(out)
    }
}

fn compute_dev_env(cfg: &TakoToml) -> std::collections::HashMap<String, String> {
    let mut env = cfg.get_merged_vars("development");
    env.insert("ENV".to_string(), "development".to_string());
    env
}

fn resolve_dev_preset_ref(cfg: &TakoToml) -> Result<String, String> {
    cfg.build
        .preset
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.to_string())
        .ok_or_else(|| {
            "Missing [build].preset in tako.toml. Set an explicit preset before running `tako dev`."
                .to_string()
        })
}

fn resolve_dev_start_command(preset: &BuildPreset, main: &str) -> Result<Vec<String>, String> {
    if preset.dev.is_empty() {
        return Err(format!(
            "Preset '{}' does not define top-level `dev` command.",
            preset.name.as_str()
        ));
    }
    Ok(preset
        .dev
        .iter()
        .map(|arg| {
            if arg == "{main}" {
                main.to_string()
            } else {
                arg.clone()
            }
        })
        .collect())
}

fn infer_preset_name_from_ref(preset_ref: &str) -> String {
    match parse_preset_reference(preset_ref) {
        Ok(PresetReference::OfficialAlias { name, .. }) => name,
        Ok(PresetReference::Github { path, .. }) => Path::new(&path)
            .file_stem()
            .and_then(|stem| stem.to_str())
            .map(str::to_string)
            .unwrap_or_else(|| "preset".to_string()),
        Err(_) => "preset".to_string(),
    }
}

fn dev_startup_lines(
    verbose: bool,
    app_name: &str,
    runtime_name: &str,
    entry_point: &Path,
    url: &str,
) -> Vec<String> {
    let mut lines = Vec::new();

    if verbose {
        lines.push("Tako Dev Server".to_string());
        lines.push("───────────────────────────────────────".to_string());
        lines.push(format!("App:     {}", app_name));
        lines.push(format!("Runtime: {}", runtime_name));
        lines.push(format!("Entry:   {}", entry_point.display()));
        lines.push(format!("URL:     {}", url));
        lines.push("───────────────────────────────────────".to_string());
    } else {
        // Quiet default: just the URL (with a blank line after).
        lines.push(url.to_string());
    }

    lines
}

fn dev_url(domain: &str, public_port: u16) -> String {
    if public_port == 443 {
        format!("https://{}/", domain)
    } else {
        format!("https://{}:{}/", domain, public_port)
    }
}

fn preferred_public_url(
    domain: &str,
    daemon_url: &str,
    listen_port: u16,
    display_port: u16,
) -> String {
    if display_port != listen_port || daemon_url.is_empty() {
        dev_url(domain, display_port)
    } else {
        daemon_url.to_string()
    }
}

#[cfg(any(target_os = "macos", test))]
const PF_FORWARDING_ANCHOR_NAME: &str = "tako";
#[cfg(any(target_os = "macos", test))]
const PF_FORWARDING_ANCHOR_FILE: &str = "/etc/pf.anchors/tako";
#[cfg(any(target_os = "macos", test))]
const PF_CONF_FILE: &str = "/etc/pf.conf";
#[cfg(any(target_os = "macos", test))]
const PF_FORWARDING_MARK_BEGIN: &str = "# >>> tako >>>";
#[cfg(any(target_os = "macos", test))]
const PF_FORWARDING_MARK_END: &str = "# <<< tako <<<";

#[cfg(any(target_os = "macos", test))]
fn local_dns_resolver_contents(port: u16) -> String {
    format!("nameserver 127.0.0.1\nport {port}\n")
}

#[cfg(any(target_os = "macos", test))]
fn parse_local_dns_resolver(contents: &str) -> (Option<String>, Option<u16>) {
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

fn dev_server_tls_paths_for_home(home: &Path) -> (PathBuf, PathBuf) {
    let certs_dir = home.join("certs");
    (
        certs_dir.join(DEV_TLS_CERT_FILENAME),
        certs_dir.join(DEV_TLS_KEY_FILENAME),
    )
}

fn ensure_dev_server_tls_material_for_home(
    ca: &LocalCA,
    home: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let (cert_path, key_path) = dev_server_tls_paths_for_home(home);
    if cert_path.is_file() && key_path.is_file() {
        return Ok(());
    }

    if let Some(parent) = cert_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let cert = ca.generate_leaf_cert_for_names(&["*.tako.local", "tako.local"])?;
    std::fs::write(&cert_path, cert.cert_pem.as_bytes())?;
    std::fs::write(&key_path, cert.key_pem.as_bytes())?;
    Ok(())
}

fn ensure_dev_server_tls_material(ca: &LocalCA) -> Result<(), Box<dyn std::error::Error>> {
    let home = crate::paths::tako_home_dir()?;
    ensure_dev_server_tls_material_for_home(ca, &home)
}

#[cfg(any(target_os = "macos", test))]
fn pf_conf_forwarding_hook_block() -> String {
    format!(
        "{PF_FORWARDING_MARK_BEGIN}\nrdr-anchor \"{PF_FORWARDING_ANCHOR_NAME}\"\nload anchor \"{PF_FORWARDING_ANCHOR_NAME}\" from \"{PF_FORWARDING_ANCHOR_FILE}\"\n{PF_FORWARDING_MARK_END}\n"
    )
}

#[cfg(any(target_os = "macos", test))]
fn pf_conf_with_forwarding_hook(existing: &str) -> String {
    let block_lines: Vec<String> = pf_conf_forwarding_hook_block()
        .trim_end_matches('\n')
        .lines()
        .map(|line| line.to_string())
        .collect();

    // Replace any previously managed block (including legacy content) to keep
    // the hook definition up to date while staying idempotent.
    let base_without_managed_block = if let Some(start) = existing.find(PF_FORWARDING_MARK_BEGIN) {
        if let Some(end_rel) = existing[start..].find(PF_FORWARDING_MARK_END) {
            let end = start + end_rel + PF_FORWARDING_MARK_END.len();
            let after = if existing[end..].starts_with('\n') {
                end + 1
            } else {
                end
            };
            let mut rebuilt = String::new();
            let before = existing[..start].trim_end_matches('\n');
            let after_block = existing[after..].trim_start_matches('\n');
            if !before.is_empty() {
                rebuilt.push_str(before);
            }
            if !before.is_empty() && !after_block.is_empty() {
                rebuilt.push_str("\n\n");
            }
            if !after_block.is_empty() {
                rebuilt.push_str(after_block);
            }
            rebuilt
        } else {
            existing.to_string()
        }
    } else {
        existing.to_string()
    };

    let mut lines: Vec<String> = base_without_managed_block
        .trim_end_matches('\n')
        .lines()
        .map(|line| line.to_string())
        .collect();

    let insert_at = lines
        .iter()
        .rposition(|line| line.trim_start().starts_with("rdr-anchor "))
        .map(|idx| idx + 1)
        .or_else(|| {
            lines
                .iter()
                .position(|line| line.trim_start().starts_with("anchor "))
        })
        .unwrap_or(lines.len());

    let mut to_insert = block_lines;
    if !lines.is_empty() {
        if insert_at > 0 && !lines[insert_at - 1].trim().is_empty() {
            to_insert.insert(0, String::new());
        }
        if insert_at < lines.len() && !lines[insert_at].trim().is_empty() {
            to_insert.push(String::new());
        }
    }

    lines.splice(insert_at..insert_at, to_insert);

    let mut out = lines.join("\n");
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out
}

#[cfg(any(target_os = "macos", test))]
fn pf_anchor_maps_loopback_port(
    anchor_rules: &str,
    loopback_addr: &str,
    incoming_port: u16,
    target_port: u16,
) -> bool {
    let target_a = format!("port {}", target_port);
    let target_b = format!("port = {}", target_port);
    let loopback_a = format!("to {}", loopback_addr);
    let loopback_b = format!("to = {}", loopback_addr);
    anchor_rules.lines().any(|line| {
        let line = line.trim().to_ascii_lowercase();
        let incoming_port_match = match incoming_port {
            443 => {
                line.contains("port 443")
                    || line.contains("port = 443")
                    || line.contains("port https")
                    || line.contains("port = https")
            }
            80 => {
                line.contains("port 80")
                    || line.contains("port = 80")
                    || line.contains("port http")
                    || line.contains("port = http")
            }
            _ => line.contains(&format!("port {}", incoming_port)),
        };
        let target_match = line.contains(&target_a) || line.contains(&target_b);
        let loopback_match = line.contains(&loopback_a) || line.contains(&loopback_b);
        !line.is_empty()
            && !line.starts_with('#')
            && loopback_match
            && line.contains("-> 127.0.0.1")
            && incoming_port_match
            && target_match
    })
}

#[cfg(target_os = "macos")]
fn has_pf_https_forwarding(public_port: u16) -> bool {
    let Ok(anchor_rules) = std::fs::read_to_string(PF_FORWARDING_ANCHOR_FILE) else {
        return false;
    };
    pf_anchor_maps_loopback_port(&anchor_rules, DEV_LOOPBACK_ADDR, 443, public_port)
}

#[cfg(target_os = "macos")]
fn has_pf_http_redirect_forwarding() -> bool {
    let Ok(anchor_rules) = std::fs::read_to_string(PF_FORWARDING_ANCHOR_FILE) else {
        return false;
    };
    pf_anchor_maps_loopback_port(
        &anchor_rules,
        DEV_LOOPBACK_ADDR,
        80,
        LOCAL_HTTP_REDIRECT_PORT,
    )
}

#[cfg(target_os = "macos")]
fn sudo_run_checked(args: &[&str], context: &str) -> Result<(), Box<dyn std::error::Error>> {
    let status = Command::new("sudo").args(args).status()?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{context} failed").into())
    }
}

#[cfg(any(target_os = "macos", test))]
fn pfctl_args<'a>(args: &'a [&'a str]) -> Vec<&'a str> {
    let mut out = Vec::with_capacity(args.len() + 2);
    out.push("pfctl");
    out.push("-q");
    out.extend_from_slice(args);
    out
}

#[cfg(any(target_os = "macos", test))]
fn pfctl_shell_command(args: &[&str]) -> String {
    let argv = pfctl_args(args);
    format!("{} >/dev/null 2>&1", argv.join(" "))
}

#[cfg(target_os = "macos")]
fn write_system_file_with_sudo(
    path: &str,
    content: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let tmp = std::env::temp_dir().join(format!(
        "tako-local-forwarding-{}-{}",
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
fn local_dns_resolver_configured(port: u16) -> bool {
    let Ok(contents) = std::fs::read_to_string(TAKO_RESOLVER_FILE) else {
        return false;
    };
    let (nameserver, configured_port) = parse_local_dns_resolver(&contents);
    nameserver.as_deref() == Some("127.0.0.1") && configured_port == Some(port)
}

#[cfg(target_os = "macos")]
fn ensure_local_dns_resolver_configured(port: u16) -> Result<(), Box<dyn std::error::Error>> {
    if local_dns_resolver_configured(port) {
        return Ok(());
    }

    if !crate::output::is_interactive() {
        return Err(format!(
            "local DNS resolver is not configured at {TAKO_RESOLVER_FILE}; run `tako dev` interactively once to install it"
        )
        .into());
    }

    crate::output::warning("Sudo password required.");
    crate::output::muted("One-time sudo required to configure local DNS for *.tako.local.");
    crate::output::muted(&format!(
        "This writes {TAKO_RESOLVER_FILE} -> nameserver 127.0.0.1 port {port}."
    ));
    crate::output::step("Configuring local DNS resolver (sudo)...");

    sudo_run_checked(
        &["install", "-d", "-m", "755", RESOLVER_DIR],
        "creating /etc/resolver",
    )?;
    write_system_file_with_sudo(TAKO_RESOLVER_FILE, &local_dns_resolver_contents(port))?;

    if !local_dns_resolver_configured(port) {
        return Err("local DNS resolver setup verification failed".into());
    }

    crate::output::success("Local DNS resolver configured for *.tako.local.");
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn ensure_local_dns_resolver_configured(_port: u16) -> Result<(), Box<dyn std::error::Error>> {
    Ok(())
}

#[cfg(target_os = "macos")]
fn apply_local_80_443_forwarding(
    public_port: u16,
    repairing: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    if !repairing && has_local_80_443_forwarding(public_port) {
        return Ok(());
    }

    if !crate::output::is_interactive() {
        return Err(
            "local 80/443 forwarding is not configured; run `tako dev` interactively once to install pf rules"
                .into(),
        );
    }

    if repairing {
        crate::output::warning("Local 80/443 forwarding looks inactive.");
        crate::output::muted("Re-applying local pf rules (sudo)...");
    } else {
        crate::output::warning("Sudo password required.");
        crate::output::muted("One-time sudo required to enable local HTTPS forwarding (80/443).");
        crate::output::muted(&format!(
            "This redirects {DEV_LOOPBACK_ADDR}:443 to 127.0.0.1:{public_port} and {DEV_LOOPBACK_ADDR}:80 to 127.0.0.1:{LOCAL_HTTP_REDIRECT_PORT}."
        ));
        crate::output::step("Enabling local 80/443 forwarding (sudo)...");
    }

    let anchor_rule = format!(
        "rdr pass on lo0 inet proto tcp from any to {DEV_LOOPBACK_ADDR} port 443 -> 127.0.0.1 port {public_port}\nrdr pass on lo0 inet proto tcp from any to {DEV_LOOPBACK_ADDR} port 80 -> 127.0.0.1 port {LOCAL_HTTP_REDIRECT_PORT}\n"
    );
    write_system_file_with_sudo(PF_FORWARDING_ANCHOR_FILE, &anchor_rule)?;

    let current_pf_conf = std::fs::read_to_string(PF_CONF_FILE)
        .map_err(|e| format!("failed to read {PF_CONF_FILE}: {e}"))?;
    let updated_pf_conf = pf_conf_with_forwarding_hook(&current_pf_conf);
    if updated_pf_conf != current_pf_conf {
        write_system_file_with_sudo(PF_CONF_FILE, &updated_pf_conf)?;
    }

    let reload_cmd = pfctl_shell_command(&["-f", PF_CONF_FILE]);
    sudo_run_checked(&["sh", "-c", reload_cmd.as_str()], "reloading pf.conf")?;
    let enable_cmd = pfctl_shell_command(&["-e"]);
    let _ = sudo_run_checked(&["sh", "-c", enable_cmd.as_str()], "enabling pf");

    let show_args = pfctl_args(&["-a", PF_FORWARDING_ANCHOR_NAME, "-sn"]);
    let output = Command::new("sudo").args(&show_args).output()?;
    if !output.status.success() {
        return Err("reading pf anchor rules failed".into());
    }

    let rules = String::from_utf8_lossy(&output.stdout);
    let has_https = pf_anchor_maps_loopback_port(&rules, DEV_LOOPBACK_ADDR, 443, public_port);
    let has_http =
        pf_anchor_maps_loopback_port(&rules, DEV_LOOPBACK_ADDR, 80, LOCAL_HTTP_REDIRECT_PORT);
    if !has_https || !has_http {
        crate::output::warning(
            "Could not confirm pf anchor mappings from pfctl output; will verify using loopback 80/443 reachability.",
        );
    }

    crate::output::success(&format!(
        "Local 80/443 forwarding enabled ({DEV_LOOPBACK_ADDR}:443 -> 127.0.0.1:{public_port}, {DEV_LOOPBACK_ADDR}:80 -> 127.0.0.1:{LOCAL_HTTP_REDIRECT_PORT})."
    ));
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn ensure_local_80_443_forwarding(_public_port: u16) -> Result<(), Box<dyn std::error::Error>> {
    Ok(())
}

#[cfg(target_os = "macos")]
fn ensure_local_80_443_forwarding(public_port: u16) -> Result<(), Box<dyn std::error::Error>> {
    apply_local_80_443_forwarding(public_port, false)
}

#[cfg(target_os = "macos")]
fn repair_local_80_443_forwarding(public_port: u16) -> Result<(), Box<dyn std::error::Error>> {
    apply_local_80_443_forwarding(public_port, true)
}

#[cfg(not(target_os = "macos"))]
fn repair_local_80_443_forwarding(_public_port: u16) -> Result<(), Box<dyn std::error::Error>> {
    Ok(())
}

#[cfg(target_os = "macos")]
fn has_local_80_443_forwarding(public_port: u16) -> bool {
    has_pf_https_forwarding(public_port) && has_pf_http_redirect_forwarding()
}

#[cfg(not(target_os = "macos"))]
fn has_local_80_443_forwarding(_public_port: u16) -> bool {
    false
}

fn tcp_port_open(ip: &str, port: u16, timeout_ms: u64) -> bool {
    let Ok(ipv4) = ip.parse::<Ipv4Addr>() else {
        return false;
    };
    let addr = SocketAddr::from((ipv4, port));
    std::net::TcpStream::connect_timeout(&addr, Duration::from_millis(timeout_ms)).is_ok()
}

#[cfg(test)]
fn localhost_tcp_port_open(port: u16, timeout_ms: u64) -> bool {
    tcp_port_open("127.0.0.1", port, timeout_ms)
}

fn loopback_tcp_port_open(port: u16, timeout_ms: u64) -> bool {
    tcp_port_open(DEV_LOOPBACK_ADDR, port, timeout_ms)
}

fn selected_public_url_port(
    configured_public_port: u16,
    current_public_url_port: u16,
    forwarding_https_probe_ok: bool,
    require_loopback_https: bool,
) -> u16 {
    if require_loopback_https {
        return current_public_url_port;
    }
    if current_public_url_port == 443 && !forwarding_https_probe_ok {
        configured_public_port
    } else {
        current_public_url_port
    }
}

#[cfg(target_os = "macos")]
async fn localhost_https_host_reachable_via_ip(
    host: &str,
    connect_ip: Ipv4Addr,
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

    let addr = SocketAddr::from((connect_ip, port));
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

#[cfg(not(target_os = "macos"))]
async fn localhost_https_host_reachable_via_ip(
    _host: &str,
    _connect_ip: Ipv4Addr,
    _port: u16,
    _timeout_ms: u64,
) -> Result<(), String> {
    Err("HTTPS probe unsupported on this platform".to_string())
}

async fn wait_for_https_host_reachable_via_ip(
    host: &str,
    connect_ip: Ipv4Addr,
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

fn local_https_probe_error(
    host: &str,
    public_port: u16,
    loopback_error: &str,
    daemon_reachable_directly: bool,
) -> String {
    if daemon_reachable_directly {
        format!(
            "Local HTTPS endpoint is unreachable at https://{host}/ ({loopback_error}). Tako dev daemon is reachable on 127.0.0.1:{public_port}, so local :443 traffic is likely intercepted before reaching Tako (for example another listener on :443 or pf loopback skip). Check `lsof -nP -iTCP:443 -sTCP:LISTEN` and pf forwarding, then re-run `tako dev`."
        )
    } else {
        format!(
            "Local HTTPS endpoint is unreachable at https://{host}/ ({loopback_error}). Check local 80/443 forwarding and try again."
        )
    }
}

#[cfg(test)]
async fn wait_for_localhost_tcp_port_open(
    port: u16,
    attempts: usize,
    timeout_ms: u64,
    retry_delay_ms: u64,
) -> bool {
    for attempt in 0..attempts {
        if localhost_tcp_port_open(port, timeout_ms) {
            return true;
        }
        if attempt + 1 < attempts {
            tokio::time::sleep(Duration::from_millis(retry_delay_ms)).await;
        }
    }
    false
}

async fn wait_for_loopback_tcp_port_open(
    port: u16,
    attempts: usize,
    timeout_ms: u64,
    retry_delay_ms: u64,
) -> bool {
    for attempt in 0..attempts {
        if loopback_tcp_port_open(port, timeout_ms) {
            return true;
        }
        if attempt + 1 < attempts {
            tokio::time::sleep(Duration::from_millis(retry_delay_ms)).await;
        }
    }
    false
}

fn port_from_listen(listen: &str) -> Option<u16> {
    listen
        .rsplit(':')
        .next()
        .and_then(|p| p.parse::<u16>().ok())
}

fn restart_required_for_requested_listen(
    existing_listen: Option<&str>,
    requested_listen: &str,
) -> bool {
    match existing_listen {
        Some(current) => current != requested_listen,
        None => false,
    }
}

fn doctor_dev_server_lines(
    listen: &str,
    port: u64,
    local_443_forwarding: bool,
    local_80_forwarding: bool,
    local_dns_enabled: bool,
    local_dns_port: u16,
) -> Vec<String> {
    let mut lines = vec!["dev-server:".to_string(), format!("  listen: {}", listen)];

    let port_is_duplicate = u16::try_from(port)
        .ok()
        .zip(port_from_listen(listen))
        .is_some_and(|(reported, from_listen)| reported == from_listen);
    if port > 0 && !port_is_duplicate {
        lines.push(format!("  port: {}", port));
    }

    lines.push(format!("  local_443_forwarding: {}", local_443_forwarding));
    lines.push(format!("  local_80_forwarding: {}", local_80_forwarding));
    lines.push(format!("  local_dns_enabled: {}", local_dns_enabled));
    lines.push(format!("  local_dns_port: {}", local_dns_port));
    lines
}

fn doctor_dev_server_unavailable_lines() -> Vec<String> {
    vec![
        "dev-server:".to_string(),
        "  status: not running".to_string(),
    ]
}

#[cfg(any(target_os = "macos", test))]
fn doctor_local_forwarding_preflight_lines(
    advertised_ip: &str,
    listen_port: u16,
    https_pf_ok: bool,
    http_pf_ok: bool,
    https_tcp_ok: bool,
    http_tcp_ok: bool,
) -> Vec<String> {
    vec![
        "preflight:".to_string(),
        format!(
            "- pf {}:443 -> 127.0.0.1:{} ({})",
            advertised_ip,
            listen_port,
            if https_pf_ok { "ok" } else { "missing" }
        ),
        format!(
            "- pf {}:80 -> 127.0.0.1:{} ({})",
            advertised_ip,
            LOCAL_HTTP_REDIRECT_PORT,
            if http_pf_ok { "ok" } else { "missing" }
        ),
        format!(
            "- tcp {}:443 ({})",
            advertised_ip,
            if https_tcp_ok { "ok" } else { "unreachable" }
        ),
        format!(
            "- tcp {}:80 ({})",
            advertised_ip,
            if http_tcp_ok { "ok" } else { "unreachable" }
        ),
    ]
}

fn is_dev_server_unavailable_error_message(message: &str) -> bool {
    let normalized = message.to_ascii_lowercase();
    normalized.contains("connection refused")
        || normalized.contains("no such file or directory")
        || normalized.contains("operation not permitted")
        || normalized.contains("permission denied")
}

#[cfg(test)]
async fn tcp_probe(addr: (&str, u16), timeout_ms: u64) -> bool {
    timeout(
        std::time::Duration::from_millis(timeout_ms),
        tokio::net::TcpStream::connect(addr),
    )
    .await
    .is_ok_and(|r| r.is_ok())
}

#[cfg(target_os = "macos")]
fn system_resolver_ipv4(hostname: &str) -> Option<String> {
    use std::net::ToSocketAddrs;

    (hostname, 443)
        .to_socket_addrs()
        .ok()?
        .find_map(|addr| match addr.ip() {
            std::net::IpAddr::V4(v4) => Some(v4.to_string()),
            std::net::IpAddr::V6(_) => None,
        })
}

#[cfg(target_os = "macos")]
fn local_dns_resolver_values() -> Option<(String, u16)> {
    let contents = std::fs::read_to_string(TAKO_RESOLVER_FILE).ok()?;
    let (nameserver, port) = parse_local_dns_resolver(&contents);
    Some((nameserver?, port?))
}

#[cfg(test)]
mod tests {
    use super::{
        DevEvent, LogLevel, ScopedLog, StoredLogEvent, app_log_scope, child_log_level_and_message,
        compute_dev_hosts, dev_disconnect_grace, dev_disconnect_grace_label,
        dev_disconnect_grace_ttl_ms, dev_idle_timeout, dev_initial_instance_count,
        dev_server_ready_log, dev_server_starting_log, dev_server_tls_paths_for_home,
        dev_startup_lines, doctor_dev_server_lines, doctor_dev_server_unavailable_lines,
        doctor_local_forwarding_preflight_lines, ensure_dev_server_tls_material_for_home,
        ensure_local_80_443_forwarding, ensure_local_dns_resolver_configured,
        host_and_port_from_url, is_dev_server_unavailable_error_message, listed_apps_contain_app,
        local_dns_resolver_contents, local_https_probe_error, parse_local_dns_resolver,
        parse_stored_log_line, pf_anchor_maps_loopback_port, pf_conf_forwarding_hook_block,
        pf_conf_with_forwarding_hook, pfctl_args, pfctl_shell_command, port_from_listen,
        preferred_public_url, replay_and_follow_logs, restart_required_for_requested_listen,
        selected_public_url_port, should_linger_after_disconnect, tcp_probe,
        wait_for_localhost_tcp_port_open,
    };
    use crate::config::TakoToml;
    use crate::dev::LocalCA;
    use std::path::Path;
    use std::time::Duration;
    use tempfile::TempDir;
    use tokio::sync::{mpsc, watch};

    fn listed_app(app_name: &str) -> crate::dev_server_client::ListedApp {
        crate::dev_server_client::ListedApp {
            app_name: app_name.to_string(),
            hosts: Vec::new(),
            upstream_port: 0,
            pid: None,
        }
    }

    #[test]
    fn dev_startup_lines_quiet_is_short() {
        let lines = dev_startup_lines(
            false,
            "app",
            "fake",
            Path::new("index.ts"),
            "https://app.tako.local:8443/",
        );
        assert_eq!(lines[0], "https://app.tako.local:8443/");
        assert!(lines.iter().all(|l| !l.contains("Tako Dev Server")));
    }

    #[test]
    fn dev_startup_lines_verbose_includes_banner() {
        let lines = dev_startup_lines(
            true,
            "app",
            "fake",
            Path::new("index.ts"),
            "https://app.tako.local:8443/",
        );
        assert!(lines.iter().any(|l| l == "Tako Dev Server"));
        assert!(lines.iter().any(|l| l.starts_with("URL:")));
    }

    #[tokio::test]
    async fn tcp_probe_detects_open_port() {
        let Ok(listener) = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await else {
            return;
        };
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let _ = listener.accept().await;
        });

        assert!(tcp_probe(("127.0.0.1", port), 200).await);
    }

    #[tokio::test]
    async fn tcp_probe_detects_closed_port() {
        // Use port 0 (invalid for TCP) to get a deterministic failure.
        assert!(!tcp_probe(("127.0.0.1", 0), 50).await);
    }

    #[tokio::test]
    async fn wait_for_localhost_port_open_retries_until_port_is_open() {
        let Ok(listener) = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await else {
            return;
        };
        let port = listener.local_addr().unwrap().port();
        drop(listener);

        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(80)).await;
            let Ok(listener) = tokio::net::TcpListener::bind(("127.0.0.1", port)).await else {
                return;
            };
            let _ = listener.accept().await;
        });

        assert!(wait_for_localhost_tcp_port_open(port, 10, 10, 25).await);
    }

    #[tokio::test]
    async fn wait_for_localhost_port_open_returns_false_when_port_stays_closed() {
        assert!(!wait_for_localhost_tcp_port_open(0, 3, 10, 5).await);
    }

    #[test]
    fn dev_server_starting_log_has_scope_and_message() {
        let log = dev_server_starting_log();
        assert!(matches!(log.level, LogLevel::Info));
        assert_eq!(log.scope, "tako");
        assert_eq!(log.message, "Starting dev server");
    }

    #[test]
    fn dev_server_ready_log_has_scope_and_message() {
        let log = dev_server_ready_log(47831);
        assert!(matches!(log.level, LogLevel::Info));
        assert_eq!(log.scope, "tako");
        assert_eq!(log.message, "Dev server listening on localhost:47831");
    }

    #[test]
    fn log_level_display_uses_five_levels() {
        assert_eq!(LogLevel::Debug.to_string(), "DEBUG");
        assert_eq!(LogLevel::Info.to_string(), "INFO");
        assert_eq!(LogLevel::Warn.to_string(), "WARN");
        assert_eq!(LogLevel::Error.to_string(), "ERROR");
        assert_eq!(LogLevel::Fatal.to_string(), "FATAL");
    }

    #[test]
    fn dev_starts_with_one_instance() {
        assert_eq!(dev_initial_instance_count(), 1);
    }

    #[test]
    fn dev_idle_timeout_is_thirty_minutes() {
        assert_eq!(dev_idle_timeout(), Duration::from_secs(30 * 60));
    }

    #[test]
    fn dev_disconnect_grace_matches_build_profile() {
        let expected_secs = if cfg!(debug_assertions) { 10 } else { 10 * 60 };
        assert_eq!(dev_disconnect_grace(), Duration::from_secs(expected_secs));
    }

    #[test]
    fn dev_disconnect_grace_ttl_matches_duration() {
        assert_eq!(
            dev_disconnect_grace_ttl_ms(),
            (dev_disconnect_grace().as_secs() * 1000) as u64
        );
    }

    #[test]
    fn dev_disconnect_grace_label_matches_duration() {
        let expected = if cfg!(debug_assertions) {
            "10 seconds"
        } else {
            "10 minutes"
        };
        assert_eq!(dev_disconnect_grace_label(), expected);
    }

    #[test]
    fn listed_apps_contain_app_matches_on_app_name() {
        let apps = vec![listed_app("one"), listed_app("two")];
        assert!(listed_apps_contain_app(&apps, "one"));
        assert!(listed_apps_contain_app(&apps, "two"));
        assert!(!listed_apps_contain_app(&apps, "three"));
    }

    #[test]
    fn regular_exit_with_running_app_enters_disconnect_grace() {
        assert!(should_linger_after_disconnect(false, true));
    }

    #[test]
    fn terminate_request_disables_disconnect_grace() {
        assert!(!should_linger_after_disconnect(true, true));
    }

    #[test]
    fn no_running_app_disables_disconnect_grace() {
        assert!(!should_linger_after_disconnect(false, false));
    }

    #[test]
    fn app_logs_use_app_scope() {
        assert_eq!(app_log_scope(), "app");
    }

    #[test]
    fn child_log_level_parser_extracts_debug_and_message() {
        let (level, message) = child_log_level_and_message(LogLevel::Info, "[DEBUG] hello");
        assert!(matches!(level, LogLevel::Debug));
        assert_eq!(message, "hello");
    }

    #[test]
    fn child_log_level_parser_extracts_warning_prefixes_case_insensitively() {
        let (level, message) = child_log_level_and_message(LogLevel::Info, "warning: low disk");
        assert!(matches!(level, LogLevel::Warn));
        assert_eq!(message, "low disk");
    }

    #[test]
    fn child_log_level_parser_maps_trace_to_debug() {
        let (level, message) = child_log_level_and_message(LogLevel::Info, "trace startup");
        assert!(matches!(level, LogLevel::Debug));
        assert_eq!(message, "startup");
    }

    #[test]
    fn child_log_level_parser_falls_back_when_no_level_prefix_exists() {
        let (level, message) = child_log_level_and_message(LogLevel::Warn, "connected");
        assert!(matches!(level, LogLevel::Warn));
        assert_eq!(message, "connected");
    }

    #[test]
    fn child_log_level_parser_does_not_match_partial_prefixes() {
        let (level, message) = child_log_level_and_message(LogLevel::Info, "debugger attached");
        assert!(matches!(level, LogLevel::Info));
        assert_eq!(message, "debugger attached");
    }

    #[test]
    fn stored_log_line_round_trips_json() {
        let line = ScopedLog {
            timestamp: "12:03:07".to_string(),
            level: LogLevel::Info,
            scope: "app".to_string(),
            message: "hello".to_string(),
        };
        let encoded = serde_json::to_string(&line).unwrap();
        assert!(encoded.contains(r#""timestamp":"12:03:07""#));
        assert!(!encoded.contains(r#""h":"#));
        assert!(!encoded.contains(r#""m":"#));
        assert!(!encoded.contains(r#""s":"#));
        let decoded = parse_stored_log_line(&encoded).unwrap();

        let StoredLogEvent::Log(decoded) = decoded else {
            panic!("expected log event");
        };

        assert_eq!(decoded.timestamp, "12:03:07");
        assert_eq!(decoded.scope, "app");
        assert_eq!(decoded.message, "hello");
    }

    #[test]
    fn stored_log_line_round_trips_fatal_level() {
        let line = ScopedLog {
            timestamp: "12:03:07".to_string(),
            level: LogLevel::Fatal,
            scope: "tako".to_string(),
            message: "fatal issue".to_string(),
        };
        let encoded = serde_json::to_string(&line).unwrap();
        let decoded = parse_stored_log_line(&encoded).unwrap();
        let StoredLogEvent::Log(decoded) = decoded else {
            panic!("expected log event");
        };
        assert!(matches!(decoded.level, LogLevel::Fatal));
    }

    #[test]
    fn stored_log_line_parses_legacy_hms_json() {
        let decoded = parse_stored_log_line(
            r#"{"h":12,"m":3,"s":7,"level":"Info","scope":"app","message":"hello"}"#,
        )
        .unwrap();

        let StoredLogEvent::Log(decoded) = decoded else {
            panic!("expected log event");
        };
        assert_eq!(decoded.timestamp, "12:03:07");
        assert!(matches!(decoded.level, LogLevel::Info));
        assert_eq!(decoded.scope, "app");
        assert_eq!(decoded.message, "hello");
    }

    #[test]
    fn stored_log_line_parses_clear_logs_marker() {
        let decoded = parse_stored_log_line(r#"{"type":"clear_logs"}"#).unwrap();
        assert!(matches!(decoded, StoredLogEvent::ClearLogs));
    }

    #[test]
    fn stored_log_line_parses_app_started_marker() {
        let decoded = parse_stored_log_line(r#"{"type":"app_event","event":"started"}"#).unwrap();
        assert!(matches!(
            decoded,
            StoredLogEvent::AppEvent(DevEvent::AppStarted)
        ));
    }

    #[test]
    fn stored_log_line_parses_app_pid_marker() {
        let decoded =
            parse_stored_log_line(r#"{"type":"app_event","event":"pid","pid":4242}"#).unwrap();
        assert!(matches!(
            decoded,
            StoredLogEvent::AppEvent(DevEvent::AppPid(4242))
        ));
    }

    #[tokio::test]
    async fn replay_and_follow_logs_emits_logs_cleared_event_for_marker() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("events.jsonl");
        std::fs::write(&log_path, b"{\"type\":\"clear_logs\"}\n").unwrap();

        let (event_tx, mut event_rx) = mpsc::channel::<DevEvent>(4);
        let (stop_tx, stop_rx) = watch::channel(false);

        let handle = tokio::spawn(async move {
            replay_and_follow_logs(log_path, None, Some(event_tx), stop_rx, false).await;
        });

        let event = tokio::time::timeout(Duration::from_secs(1), event_rx.recv())
            .await
            .expect("timed out waiting for event")
            .expect("event channel closed unexpectedly");
        assert!(matches!(event, DevEvent::LogsCleared));

        let _ = stop_tx.send(true);
        let _ = tokio::time::timeout(Duration::from_secs(1), handle).await;
    }

    #[tokio::test]
    async fn replay_and_follow_logs_emits_logs_ready_after_initial_replay() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("events.jsonl");
        std::fs::write(&log_path, b"").unwrap();

        let (event_tx, mut event_rx) = mpsc::channel::<DevEvent>(4);
        let (stop_tx, stop_rx) = watch::channel(false);

        let handle = tokio::spawn(async move {
            replay_and_follow_logs(log_path, None, Some(event_tx), stop_rx, false).await;
        });

        let event = tokio::time::timeout(Duration::from_secs(1), event_rx.recv())
            .await
            .expect("timed out waiting for event")
            .expect("event channel closed unexpectedly");
        assert!(matches!(event, DevEvent::LogsReady));

        let _ = stop_tx.send(true);
        let _ = tokio::time::timeout(Duration::from_secs(1), handle).await;
    }

    #[tokio::test]
    async fn replay_and_follow_logs_emits_app_started_event_for_marker() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("events.jsonl");
        std::fs::write(
            &log_path,
            b"{\"type\":\"app_event\",\"event\":\"started\"}\n",
        )
        .unwrap();

        let (event_tx, mut event_rx) = mpsc::channel::<DevEvent>(4);
        let (stop_tx, stop_rx) = watch::channel(false);

        let handle = tokio::spawn(async move {
            replay_and_follow_logs(log_path, None, Some(event_tx), stop_rx, false).await;
        });

        let event = tokio::time::timeout(Duration::from_secs(1), event_rx.recv())
            .await
            .expect("timed out waiting for event")
            .expect("event channel closed unexpectedly");
        assert!(matches!(event, DevEvent::AppStarted));

        let _ = stop_tx.send(true);
        let _ = tokio::time::timeout(Duration::from_secs(1), handle).await;
    }

    #[tokio::test]
    async fn replay_and_follow_logs_emits_app_pid_event_for_marker() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("events.jsonl");
        std::fs::write(
            &log_path,
            b"{\"type\":\"app_event\",\"event\":\"pid\",\"pid\":4242}\n",
        )
        .unwrap();

        let (event_tx, mut event_rx) = mpsc::channel::<DevEvent>(4);
        let (stop_tx, stop_rx) = watch::channel(false);

        let handle = tokio::spawn(async move {
            replay_and_follow_logs(log_path, None, Some(event_tx), stop_rx, false).await;
        });

        let event = tokio::time::timeout(Duration::from_secs(1), event_rx.recv())
            .await
            .expect("timed out waiting for event")
            .expect("event channel closed unexpectedly");
        assert!(matches!(event, DevEvent::AppPid(4242)));

        let _ = stop_tx.send(true);
        let _ = tokio::time::timeout(Duration::from_secs(1), handle).await;
    }

    #[tokio::test]
    async fn replay_and_follow_logs_ignores_followed_app_event_markers_for_owner_tail() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("events.jsonl");
        std::fs::write(&log_path, b"").unwrap();

        let (event_tx, mut event_rx) = mpsc::channel::<DevEvent>(8);
        let (stop_tx, stop_rx) = watch::channel(false);
        let log_path_for_task = log_path.clone();

        let handle = tokio::spawn(async move {
            replay_and_follow_logs(log_path_for_task, None, Some(event_tx), stop_rx, true).await;
        });

        let event = tokio::time::timeout(Duration::from_secs(1), event_rx.recv())
            .await
            .expect("timed out waiting for logs-ready event")
            .expect("event channel closed unexpectedly");
        assert!(matches!(event, DevEvent::LogsReady));

        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&log_path)
            .unwrap();
        std::io::Write::write_all(
            &mut file,
            b"{\"type\":\"app_event\",\"event\":\"started\"}\n",
        )
        .unwrap();

        let unexpected = tokio::time::timeout(Duration::from_millis(300), event_rx.recv()).await;
        assert!(
            unexpected.is_err(),
            "owner tail should not echo persisted app lifecycle markers"
        );

        let _ = stop_tx.send(true);
        let _ = tokio::time::timeout(Duration::from_secs(1), handle).await;
    }

    #[test]
    fn restart_not_required_when_no_existing_server() {
        assert!(!restart_required_for_requested_listen(
            None,
            "127.0.0.1:47831"
        ));
    }

    #[test]
    fn restart_not_required_when_existing_listen_matches() {
        assert!(!restart_required_for_requested_listen(
            Some("127.0.0.1:47831"),
            "127.0.0.1:47831"
        ));
    }

    #[test]
    fn restart_required_when_existing_listen_differs() {
        assert!(restart_required_for_requested_listen(
            Some("127.0.0.1:8443"),
            "127.0.0.1:47831"
        ));
    }

    #[test]
    fn parse_port_from_listen_handles_valid_and_invalid_values() {
        assert_eq!(port_from_listen("127.0.0.1:47831"), Some(47831));
        assert_eq!(port_from_listen("localhost:443"), Some(443));
        assert_eq!(port_from_listen("bad-listen"), None);
        assert_eq!(port_from_listen("host:not-a-port"), None);
    }

    #[test]
    fn host_and_port_parser_handles_default_and_explicit_ports() {
        assert_eq!(
            host_and_port_from_url("https://app.tako.local/"),
            Some(("app.tako.local".to_string(), 443))
        );
        assert_eq!(
            host_and_port_from_url("https://app.tako.local:47831/"),
            Some(("app.tako.local".to_string(), 47831))
        );
    }

    #[test]
    fn selected_public_url_port_falls_back_when_forwarding_probe_fails() {
        assert_eq!(selected_public_url_port(47831, 443, false, false), 47831);
    }

    #[test]
    fn selected_public_url_port_keeps_https_when_forwarding_probe_succeeds() {
        assert_eq!(selected_public_url_port(47831, 443, true, false), 443);
    }

    #[test]
    fn selected_public_url_port_keeps_explicit_port() {
        assert_eq!(selected_public_url_port(47831, 47831, false, false), 47831);
    }

    #[test]
    fn selected_public_url_port_keeps_https_when_required() {
        assert_eq!(selected_public_url_port(47831, 443, false, true), 443);
    }

    #[test]
    fn local_https_probe_error_mentions_interception_when_daemon_is_reachable() {
        let msg = local_https_probe_error("bun-example.tako.local", 47831, "timed out", true);
        assert!(msg.contains("likely intercepted"));
        assert!(msg.contains("127.0.0.1:47831"));
    }

    #[test]
    fn local_https_probe_error_mentions_forwarding_when_daemon_is_not_reachable() {
        let msg = local_https_probe_error("bun-example.tako.local", 47831, "timed out", false);
        assert!(msg.contains("Check local 80/443 forwarding"));
    }

    #[test]
    fn doctor_omits_duplicate_port_line_when_listen_includes_same_port() {
        let lines = doctor_dev_server_lines("127.0.0.1:47831", 47831, false, false, true, 53535);
        assert!(
            !lines.iter().any(|line| line.starts_with("  port:")),
            "doctor output should not duplicate listen port: {lines:?}"
        );
    }

    #[test]
    fn doctor_keeps_port_line_when_listen_does_not_include_port() {
        let lines = doctor_dev_server_lines("(unknown)", 47831, false, false, true, 53535);
        assert!(
            lines.iter().any(|line| line == "  port: 47831"),
            "doctor output should keep explicit port when listen does not include one: {lines:?}"
        );
    }

    #[test]
    fn doctor_preflight_lines_include_clear_failure_reasons() {
        let lines =
            doctor_local_forwarding_preflight_lines("127.77.0.1", 47831, false, true, false, true);
        assert!(lines.iter().any(|line| line.contains("pf 127.77.0.1:443")));
        assert!(lines.iter().any(|line| line.contains("(missing)")));
        assert!(
            lines
                .iter()
                .any(|line| line.contains("tcp 127.77.0.1:443 (unreachable)"))
        );
        assert!(
            lines
                .iter()
                .any(|line| line.contains("tcp 127.77.0.1:80 (ok)"))
        );
    }

    #[test]
    fn doctor_reports_not_running_when_dev_server_is_unavailable() {
        assert_eq!(
            doctor_dev_server_unavailable_lines(),
            vec![
                "dev-server:".to_string(),
                "  status: not running".to_string()
            ]
        );
    }

    #[test]
    fn unavailable_error_detection_matches_missing_or_stale_socket_errors() {
        assert!(is_dev_server_unavailable_error_message(
            "No such file or directory (os error 2)"
        ));
        assert!(is_dev_server_unavailable_error_message(
            "Connection refused (os error 61)"
        ));
        assert!(is_dev_server_unavailable_error_message(
            "Operation not permitted (os error 1)"
        ));
        assert!(is_dev_server_unavailable_error_message(
            "Permission denied (os error 13)"
        ));
        assert!(!is_dev_server_unavailable_error_message(
            "failed to parse response"
        ));
    }

    #[test]
    fn local_dns_resolver_template_targets_loopback_port() {
        assert_eq!(
            local_dns_resolver_contents(53535),
            "nameserver 127.0.0.1\nport 53535\n"
        );
    }

    #[test]
    fn dev_server_tls_paths_are_under_certs_dir() {
        let home = Path::new("/tmp/tako-home");
        let (cert_path, key_path) = dev_server_tls_paths_for_home(home);
        assert_eq!(
            cert_path,
            Path::new("/tmp/tako-home/certs/fullchain.pem").to_path_buf()
        );
        assert_eq!(
            key_path,
            Path::new("/tmp/tako-home/certs/privkey.pem").to_path_buf()
        );
    }

    #[test]
    fn ensure_dev_server_tls_material_writes_cert_and_key_when_missing() {
        let temp = TempDir::new().unwrap();
        let ca = LocalCA::generate().unwrap();
        ensure_dev_server_tls_material_for_home(&ca, temp.path()).unwrap();

        let (cert_path, key_path) = dev_server_tls_paths_for_home(temp.path());
        let cert = std::fs::read_to_string(cert_path).unwrap();
        let key = std::fs::read_to_string(key_path).unwrap();
        assert!(cert.contains("BEGIN CERTIFICATE"));
        assert!(key.contains("BEGIN PRIVATE KEY"));
    }

    #[test]
    fn ensure_dev_server_tls_material_keeps_existing_files() {
        let temp = TempDir::new().unwrap();
        let (cert_path, key_path) = dev_server_tls_paths_for_home(temp.path());
        std::fs::create_dir_all(cert_path.parent().unwrap()).unwrap();
        std::fs::write(&cert_path, "existing-cert").unwrap();
        std::fs::write(&key_path, "existing-key").unwrap();

        let ca = LocalCA::generate().unwrap();
        ensure_dev_server_tls_material_for_home(&ca, temp.path()).unwrap();

        let cert = std::fs::read_to_string(cert_path).unwrap();
        let key = std::fs::read_to_string(key_path).unwrap();
        assert_eq!(cert, "existing-cert");
        assert_eq!(key, "existing-key");
    }

    #[test]
    fn parse_local_dns_resolver_extracts_nameserver_and_port() {
        let (ns, port) =
            parse_local_dns_resolver("# tako resolver\nnameserver 127.0.0.1\nport 53535\n");
        assert_eq!(ns.as_deref(), Some("127.0.0.1"));
        assert_eq!(port, Some(53535));
    }

    #[test]
    fn parse_local_dns_resolver_prefers_latest_valid_entries() {
        let (ns, port) = parse_local_dns_resolver(
            "# stale resolver values\nnameserver 10.0.0.1\nport not-a-number\nnameserver 127.0.0.1\nport 53535\n",
        );
        assert_eq!(ns.as_deref(), Some("127.0.0.1"));
        assert_eq!(port, Some(53535));
    }

    #[test]
    fn parse_local_dns_resolver_ignores_unknown_lines() {
        let (ns, port) = parse_local_dns_resolver(
            "# unrelated\nsearch local\noptions ndots:1\nnameserver 127.0.0.1\n",
        );
        assert_eq!(ns.as_deref(), Some("127.0.0.1"));
        assert_eq!(port, None);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn ensure_local_dns_resolver_non_interactive_error_is_actionable() {
        let err = ensure_local_dns_resolver_configured(65535)
            .expect_err("non-interactive setup should fail when resolver is not configured");
        let text = err.to_string();
        assert!(text.contains("/etc/resolver/tako.local"));
        assert!(text.contains("run `tako dev` interactively once"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn ensure_local_forwarding_non_interactive_error_is_actionable() {
        let err = ensure_local_80_443_forwarding(65535)
            .expect_err("non-interactive setup should fail when forwarding is not configured");
        let text = err.to_string();
        assert!(text.contains("local 80/443 forwarding is not configured"));
        assert!(text.contains("run `tako dev` interactively once"));
    }

    #[test]
    fn detects_pf_anchor_rule_for_requested_port_and_loopback_address() {
        let rules = "rdr pass on lo0 inet proto tcp from any to 127.77.0.1 port 443 -> 127.0.0.1 port 47831";
        assert!(pf_anchor_maps_loopback_port(
            rules,
            "127.77.0.1",
            443,
            47831
        ));
    }

    #[test]
    fn detects_pf_anchor_rule_for_http_redirect_port() {
        let rules =
            "rdr pass on lo0 inet proto tcp from any to 127.77.0.1 port 80 -> 127.0.0.1 port 47830";
        assert!(pf_anchor_maps_loopback_port(rules, "127.77.0.1", 80, 47830));
    }

    #[test]
    fn ignores_pf_anchor_rule_for_different_port() {
        let rules = "rdr pass on lo0 inet proto tcp from any to 127.77.0.1 port 443 -> 127.0.0.1 port 47831";
        assert!(!pf_anchor_maps_loopback_port(
            rules,
            "127.77.0.1",
            443,
            8443
        ));
    }

    #[test]
    fn ignores_pf_anchor_rule_for_different_loopback_address() {
        let rules =
            "rdr pass on lo0 inet proto tcp from any to 127.0.0.1 port 443 -> 127.0.0.1 port 47831";
        assert!(!pf_anchor_maps_loopback_port(
            rules,
            "127.77.0.1",
            443,
            47831
        ));
    }

    #[test]
    fn ignores_pf_anchor_without_443_redirect() {
        let rules =
            "rdr pass on lo0 inet proto tcp from any to 127.77.0.1 port 80 -> 127.0.0.1 port 47831";
        assert!(!pf_anchor_maps_loopback_port(
            rules,
            "127.77.0.1",
            443,
            47831
        ));
    }

    #[test]
    fn pfctl_args_include_quiet_flag() {
        assert_eq!(
            pfctl_args(&["-f", "/etc/pf.conf"]),
            vec!["pfctl", "-q", "-f", "/etc/pf.conf"]
        );
    }

    #[test]
    fn pfctl_shell_command_redirects_output_to_dev_null() {
        assert_eq!(pfctl_shell_command(&["-e"]), "pfctl -q -e >/dev/null 2>&1");
    }

    #[test]
    fn prefers_local_url_when_80_443_forwarding_is_detected() {
        let url = preferred_public_url(
            "bun-example.tako.local",
            "https://bun-example.tako.local:47831/",
            47831,
            443,
        );
        assert_eq!(url, "https://bun-example.tako.local/");
    }

    #[test]
    fn prefers_daemon_url_when_display_and_listen_ports_match() {
        let url = preferred_public_url(
            "bun-example.tako.local",
            "https://bun-example.tako.local:47831/",
            47831,
            47831,
        );
        assert_eq!(url, "https://bun-example.tako.local:47831/");
    }

    #[test]
    fn falls_back_to_default_host_when_development_routes_are_missing() {
        let cfg = TakoToml::default();
        let hosts = compute_dev_hosts("app", &cfg, "app.tako.local").unwrap();
        assert_eq!(hosts, vec!["app.tako.local".to_string()]);
    }

    #[test]
    fn falls_back_to_default_host_when_development_routes_are_empty() {
        let cfg = TakoToml::parse("[envs.development]\nroutes = []\n").unwrap();
        let hosts = compute_dev_hosts("app", &cfg, "app.tako.local").unwrap();
        assert_eq!(hosts, vec!["app.tako.local".to_string()]);
    }

    #[test]
    fn keeps_hosts_unchanged_when_dev_routes_are_explicit() {
        let cfg =
            TakoToml::parse("[envs.development]\nroutes = [\"api.app.tako.local\"]\n").unwrap();
        let hosts = compute_dev_hosts("app", &cfg, "app.tako.local").unwrap();
        assert_eq!(hosts, vec!["api.app.tako.local".to_string()]);
    }

    #[test]
    fn explicit_dev_routes_do_not_fallback_to_default_when_only_wildcards_are_set() {
        let cfg = TakoToml::parse("[envs.development]\nroutes = [\"*.app.tako.local\"]\n").unwrap();
        let err = compute_dev_hosts("app", &cfg, "app.tako.local").unwrap_err();
        assert!(err.contains("none are exact hostnames"));
    }

    #[test]
    fn pf_conf_hook_block_contains_rdr_anchor_and_load_lines() {
        let block = pf_conf_forwarding_hook_block();
        assert!(block.contains("rdr-anchor \"tako\""));
        assert!(block.contains("load anchor \"tako\""));
    }

    #[test]
    fn pf_conf_with_forwarding_hook_is_idempotent() {
        let existing = "set skip on lo0\n";
        let once = pf_conf_with_forwarding_hook(existing);
        let twice = pf_conf_with_forwarding_hook(&once);
        assert_eq!(once, twice);
    }

    #[test]
    fn pf_conf_with_forwarding_hook_appends_when_missing() {
        let existing = "set skip on lo0\n";
        let updated = pf_conf_with_forwarding_hook(existing);
        assert!(updated.starts_with("set skip on lo0"));
        assert!(updated.contains("# >>> tako >>>"));
    }

    #[test]
    fn pf_conf_with_forwarding_hook_replaces_legacy_anchor_block() {
        let existing = r#"scrub-anchor "com.apple/*"

# >>> tako >>>
anchor "tako.portless"
load anchor "tako.portless" from "/etc/pf.anchors/tako.portless"
# <<< tako <<<
"#;
        let updated = pf_conf_with_forwarding_hook(existing);
        assert!(updated.contains(r#"rdr-anchor "tako""#));
        assert!(!updated.contains("\nanchor \"tako.portless\"\n"));
        assert!(!updated.contains("tako.portless"));
        assert_eq!(updated.matches("# >>> tako >>>").count(), 1);
    }

    #[test]
    fn pf_conf_with_forwarding_hook_inserts_after_rdr_anchor_section() {
        let existing = r#"scrub-anchor "com.apple/*"
nat-anchor "com.apple/*"
rdr-anchor "com.apple/*"
dummynet-anchor "com.apple/*"
anchor "com.apple/*"
load anchor "com.apple" from "/etc/pf.anchors/com.apple"
"#;
        let updated = pf_conf_with_forwarding_hook(existing);

        let lines: Vec<&str> = updated.lines().collect();
        let tako_block_pos = lines
            .iter()
            .position(|line| line.trim() == "# >>> tako >>>")
            .unwrap();
        let rdr_anchor_pos = lines
            .iter()
            .position(|line| line.trim() == "rdr-anchor \"com.apple/*\"")
            .unwrap();
        let filter_anchor_pos = lines
            .iter()
            .position(|line| line.trim() == "anchor \"com.apple/*\"")
            .unwrap();

        assert!(
            tako_block_pos > rdr_anchor_pos,
            "tako block should appear after existing rdr anchors"
        );
        assert!(
            tako_block_pos < filter_anchor_pos,
            "tako block should be inserted before filter anchors"
        );
    }

    #[test]
    fn pf_conf_with_forwarding_hook_falls_back_to_before_filter_anchor_when_no_rdr_anchor_exists() {
        let existing = r#"set block-policy drop
anchor "com.apple/*"
load anchor "com.apple" from "/etc/pf.anchors/com.apple"
"#;
        let updated = pf_conf_with_forwarding_hook(existing);

        let lines: Vec<&str> = updated.lines().collect();
        let tako_block_pos = lines
            .iter()
            .position(|line| line.trim() == "# >>> tako >>>")
            .unwrap();
        let filter_anchor_pos = lines
            .iter()
            .position(|line| line.trim() == "anchor \"com.apple/*\"")
            .unwrap();
        assert!(
            tako_block_pos < filter_anchor_pos,
            "tako block should be inserted before filter anchors"
        );
    }
}

pub async fn doctor(dns: bool) -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(not(target_os = "macos"))]
    let _ = dns;

    let info = match crate::dev_server_client::info().await {
        Ok(info) => info,
        Err(e) => {
            let message = e.to_string();
            if is_dev_server_unavailable_error_message(&message) {
                for line in doctor_dev_server_unavailable_lines() {
                    println!("{line}");
                }
                crate::output::muted(
                    "Run `tako dev` to start the local dev daemon, then re-run `tako doctor`.",
                );
                return Ok(());
            }
            return Err(e);
        }
    };

    let i = info.get("info").unwrap_or(&serde_json::Value::Null);
    let listen = i
        .get("listen")
        .and_then(|v| v.as_str())
        .unwrap_or("(unknown)");
    let port = i.get("port").and_then(|v| v.as_u64()).unwrap_or(0);
    #[cfg(target_os = "macos")]
    let advertised_ip = i
        .get("advertised_ip")
        .and_then(|v| v.as_str())
        .unwrap_or(DEV_LOOPBACK_ADDR);
    let local_dns_enabled = i
        .get("local_dns_enabled")
        .and_then(|b| b.as_bool())
        .unwrap_or(false);
    let local_dns_port = i
        .get("local_dns_port")
        .and_then(|v| v.as_u64())
        .and_then(|v| u16::try_from(v).ok())
        .unwrap_or(LOCAL_DNS_PORT);
    #[cfg(target_os = "macos")]
    let listen_port = port_from_listen(listen).unwrap_or_default();
    #[cfg(target_os = "macos")]
    let (
        https_pf_ok,
        http_pf_ok,
        https_tcp_ok,
        http_tcp_ok,
        local_443_forwarding,
        local_80_forwarding,
    ) = if listen_port > 0 {
        let https_pf_ok = has_pf_https_forwarding(listen_port);
        let http_pf_ok = has_pf_http_redirect_forwarding();
        let https_tcp_ok = tcp_port_open(advertised_ip, 443, 150);
        let http_tcp_ok = tcp_port_open(advertised_ip, 80, 150);
        (
            https_pf_ok,
            http_pf_ok,
            https_tcp_ok,
            http_tcp_ok,
            https_pf_ok && https_tcp_ok,
            http_pf_ok && http_tcp_ok,
        )
    } else {
        (false, false, false, false, false, false)
    };
    #[cfg(not(target_os = "macos"))]
    let (local_443_forwarding, local_80_forwarding) = (false, false);

    for line in doctor_dev_server_lines(
        listen,
        port,
        local_443_forwarding,
        local_80_forwarding,
        local_dns_enabled,
        local_dns_port,
    ) {
        println!("{line}");
    }

    #[cfg(target_os = "macos")]
    {
        println!();
        for line in doctor_local_forwarding_preflight_lines(
            advertised_ip,
            listen_port,
            https_pf_ok,
            http_pf_ok,
            https_tcp_ok,
            http_tcp_ok,
        ) {
            println!("{line}");
        }
    }

    let apps = crate::dev_server_client::list_apps()
        .await
        .unwrap_or_default();
    if !apps.is_empty() {
        println!();
        println!("apps:");
        for a in &apps {
            let hosts = if a.hosts.is_empty() {
                "(default)".to_string()
            } else {
                a.hosts.join(",")
            };
            if let Some(pid) = a.pid {
                println!(
                    "- {}  hosts={}  port={}  pid={}",
                    a.app_name, hosts, a.upstream_port, pid
                );
            } else {
                println!(
                    "- {}  hosts={}  port={}",
                    a.app_name, hosts, a.upstream_port
                );
            }
        }
    }

    // Best-effort local DNS checks (macOS only).
    #[cfg(target_os = "macos")]
    {
        if dns {
            let expected_ip = advertised_ip;
            println!();
            println!("local-dns:");

            match local_dns_resolver_values() {
                Some((nameserver, port)) if nameserver == "127.0.0.1" && port == local_dns_port => {
                    println!(
                        "- resolver {} -> nameserver {} port {} (ok)",
                        TAKO_RESOLVER_FILE, nameserver, port
                    );
                }
                Some((nameserver, port)) => {
                    println!(
                        "- resolver {} -> nameserver {} port {} (conflict; expected 127.0.0.1:{})",
                        TAKO_RESOLVER_FILE, nameserver, port, local_dns_port
                    );
                }
                None => {
                    println!("- resolver {} -> (missing)", TAKO_RESOLVER_FILE);
                }
            }

            for a in &apps {
                let hosts = if a.hosts.is_empty() {
                    vec![crate::dev::get_tako_domain(&a.app_name)]
                } else {
                    a.hosts.clone()
                };
                for host in hosts.into_iter().filter(|h| h.ends_with(".tako.local")) {
                    match system_resolver_ipv4(&host) {
                        Some(ip) if ip == expected_ip => {
                            println!("- {} -> {} (ok)", host, ip);
                        }
                        Some(ip) => {
                            println!("- {} -> {} (conflict; expected {})", host, ip, expected_ip);
                        }
                        None => {
                            println!("- {} -> (no answer)", host);
                        }
                    }
                }
            }
        }
    }

    Ok(())
}

/// Run the dev server
pub async fn run(
    public_port: u16,
    tui: bool,
    no_tui: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let project_dir = current_dir()?;
    let cfg = load_dev_tako_toml(&project_dir)?;
    let preset_ref = resolve_dev_preset_ref(&cfg)?;
    let (build_preset, _) = load_build_preset(&project_dir, &preset_ref)
        .await
        .map_err(|e| format!("Failed to resolve build preset '{}': {}", preset_ref, e))?;
    let main = crate::commands::deploy::resolve_deploy_main(&cfg, build_preset.main.as_deref())
        .map_err(|e| format!("Failed to resolve deploy entrypoint: {}", e))?;

    let runtime_name = build_preset.name.clone();

    let app_name = cfg.name.clone().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "Missing top-level `name` in tako.toml.",
        )
    })?;
    let domain = LocalCA::app_domain(&app_name);
    let local_ca = setup_local_ca().await?;
    ensure_dev_server_tls_material(&local_ca)?;
    ensure_local_dns_resolver_configured(LOCAL_DNS_PORT)?;

    let require_loopback_https = cfg!(target_os = "macos");
    if require_loopback_https {
        ensure_local_80_443_forwarding(public_port)?;
    } else if let Err(e) = ensure_local_80_443_forwarding(public_port) {
        crate::output::warning(&format!(
            "Could not enable local 80/443 forwarding automatically: {}",
            e
        ));
        crate::output::muted("Continuing with explicit dev port URL.");
    }

    let mut public_url_port = if require_loopback_https || has_local_80_443_forwarding(public_port)
    {
        443
    } else {
        public_port
    };
    let daemon_dns_ip = if public_url_port == 443 {
        DEV_LOOPBACK_ADDR
    } else {
        "127.0.0.1"
    };
    let listen_addr = format!("127.0.0.1:{}", public_port);

    // If a dev server is already running on another listen address, ask whether to restart it.
    let existing_info = crate::dev_server_client::info().await.ok();
    let existing_listen = existing_info.as_ref().and_then(|v| {
        v.get("info")
            .and_then(|i| i.get("listen"))
            .and_then(|s| s.as_str())
            .map(|s| s.to_string())
    });
    let existing_advertised_ip = existing_info.as_ref().and_then(|v| {
        v.get("info")
            .and_then(|i| i.get("advertised_ip"))
            .and_then(|s| s.as_str())
            .map(|s| s.to_string())
    });
    let restart_for_listen =
        restart_required_for_requested_listen(existing_listen.as_deref(), &listen_addr);
    let restart_for_dns = existing_advertised_ip
        .as_deref()
        .map(|ip| ip != daemon_dns_ip)
        .unwrap_or(false);

    if restart_for_listen || restart_for_dns {
        let current_listen = existing_listen.unwrap_or_else(|| "(unknown)".to_string());
        let current_dns_ip = existing_advertised_ip.unwrap_or_else(|| "(unknown)".to_string());
        let restart_reason = if restart_for_listen && restart_for_dns {
            format!("listen {} and DNS {}", current_listen, current_dns_ip)
        } else if restart_for_listen {
            format!("listen {}", current_listen)
        } else {
            format!("DNS {}", current_dns_ip)
        };

        if crate::output::is_interactive() {
            crate::output::section("Dev Server");
            crate::output::warning(&format!(
                "A dev server is already running with {}.",
                restart_reason
            ));
            let should_restart = crate::output::confirm(
                &format!(
                    "Restart it with listen {} and DNS {}?",
                    listen_addr, daemon_dns_ip
                ),
                true,
            )?;
            if !should_restart {
                return Err(format!("Kept existing dev server on {}.", current_listen).into());
            }

            crate::dev_server_client::stop_server().await?;
            for _ in 0..40 {
                if crate::dev_server_client::info().await.is_err() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        } else {
            return Err(format!(
                "A dev server is already running with listen {} and DNS {}. Stop it first and re-run `tako dev`.",
                current_listen, current_dns_ip
            )
            .into());
        }
    }

    // Compute initial dev config snapshot from tako.toml.
    let dev_hosts = compute_dev_hosts(&app_name, &cfg, &domain)
        .map_err(|e| format!("invalid development routes: {}", e))?;
    let primary_host = dev_hosts.first().cloned().unwrap_or_else(|| domain.clone());

    let hosts_state = Arc::new(tokio::sync::Mutex::new(dev_hosts.clone()));
    let env = compute_dev_env(&cfg);
    let env_state = Arc::new(tokio::sync::Mutex::new(env));

    // Create channels for communication (child stdout/stderr + file watcher events).
    let (log_tx, log_rx) = mpsc::channel::<ScopedLog>(1000);
    let (event_tx, event_rx) = mpsc::channel::<DevEvent>(100);

    let (control_tx, mut control_rx) = mpsc::channel::<tui::ControlCmd>(32);
    let (should_exit_tx, mut should_exit_rx) = watch::channel(false);
    let terminate_requested = Arc::new(AtomicBool::new(false));

    let interactive = std::io::stdin().is_terminal() && std::io::stdout().is_terminal();
    let use_tui = if no_tui {
        false
    } else if tui {
        true
    } else {
        interactive
    };

    // Allocate an ephemeral port for the app.
    let (upstream_port, reserve_listener) = reserve_ephemeral_port()?;
    let cmd = resolve_dev_start_command(&build_preset, &main)
        .map_err(|e| format!("Invalid dev start command: {}", e))?;

    // Keep receivers optional until we decide whether to launch the TUI.
    let mut log_rx_opt = Some(log_rx);
    let mut event_rx_opt = Some(event_rx);
    let mut tui_handle = None;

    // Ensure the dev daemon is running.

    let _ = log_tx.send(dev_server_starting_log()).await;

    if let Err(e) = crate::dev_server_client::ensure_running(&listen_addr, daemon_dns_ip).await {
        let msg = e.to_string();
        let _ = log_tx
            .send(ScopedLog::error(
                "tako",
                format!("✗ dev server failed to start: {}", msg),
            ))
            .await;
        let _ = event_tx.send(DevEvent::AppError(msg.clone())).await;

        return Err(msg.into());
    }

    let _ = log_tx.send(dev_server_ready_log(public_port)).await;

    if public_url_port == 443 {
        let https_ready = wait_for_loopback_tcp_port_open(
            443,
            LOOPBACK_PORT_PROBE_ATTEMPTS,
            LOOPBACK_PORT_PROBE_TIMEOUT_MS,
            LOOPBACK_PORT_PROBE_RETRY_DELAY_MS,
        )
        .await;
        let http_ready = wait_for_loopback_tcp_port_open(
            80,
            LOOPBACK_PORT_PROBE_ATTEMPTS,
            LOOPBACK_PORT_PROBE_TIMEOUT_MS,
            LOOPBACK_PORT_PROBE_RETRY_DELAY_MS,
        )
        .await;

        if !https_ready || !http_ready {
            if let Err(e) = repair_local_80_443_forwarding(public_port) {
                if require_loopback_https {
                    return Err(format!(
                        "Could not repair local 80/443 forwarding automatically: {}",
                        e
                    )
                    .into());
                } else {
                    crate::output::warning(&format!(
                        "Could not repair local 80/443 forwarding automatically: {}",
                        e
                    ));
                    crate::output::muted("Continuing with explicit dev port URL.");
                    public_url_port = public_port;
                }
            } else {
                let repaired_https_ready = wait_for_loopback_tcp_port_open(
                    443,
                    LOOPBACK_PORT_PROBE_ATTEMPTS,
                    LOOPBACK_PORT_PROBE_TIMEOUT_MS,
                    LOOPBACK_PORT_PROBE_RETRY_DELAY_MS,
                )
                .await;
                let repaired_http_ready = wait_for_loopback_tcp_port_open(
                    80,
                    LOOPBACK_PORT_PROBE_ATTEMPTS,
                    LOOPBACK_PORT_PROBE_TIMEOUT_MS,
                    LOOPBACK_PORT_PROBE_RETRY_DELAY_MS,
                )
                .await;
                if !repaired_https_ready || !repaired_http_ready {
                    if require_loopback_https {
                        return Err(format!(
                            "Local 80/443 forwarding is configured but {DEV_LOOPBACK_ADDR}:80/443 is unreachable."
                        )
                        .into());
                    } else {
                        crate::output::warning(&format!(
                            "Local 80/443 forwarding is configured but {DEV_LOOPBACK_ADDR}:80/443 is unreachable."
                        ));
                        crate::output::muted("Continuing with explicit dev port URL.");
                        public_url_port = public_port;
                    }
                }
            }
        }
    } else if has_local_80_443_forwarding(public_port)
        && loopback_tcp_port_open(443, 150)
        && loopback_tcp_port_open(80, 150)
    {
        // Accept pre-existing local forwarding (e.g. configured outside Tako)
        // so displayed URLs stay truly reachable without requiring a re-setup.
        public_url_port = 443;
    }

    if public_url_port == 443 {
        let Ok(loopback_ip) = DEV_LOOPBACK_ADDR.parse::<Ipv4Addr>() else {
            return Err(format!("Invalid loopback address: {DEV_LOOPBACK_ADDR}").into());
        };
        let probe_result = wait_for_https_host_reachable_via_ip(
            &primary_host,
            loopback_ip,
            443,
            LOCALHOST_443_HTTPS_PROBE_ATTEMPTS,
            LOCALHOST_443_HTTPS_PROBE_TIMEOUT_MS,
            LOCALHOST_443_HTTPS_PROBE_RETRY_DELAY_MS,
        )
        .await;
        if require_loopback_https && let Err(loopback_error) = probe_result.as_ref() {
            let daemon_reachable_directly = wait_for_https_host_reachable_via_ip(
                &primary_host,
                Ipv4Addr::new(127, 0, 0, 1),
                public_port,
                LOCALHOST_443_HTTPS_PROBE_ATTEMPTS,
                LOCALHOST_443_HTTPS_PROBE_TIMEOUT_MS,
                LOCALHOST_443_HTTPS_PROBE_RETRY_DELAY_MS,
            )
            .await
            .is_ok();
            return Err(local_https_probe_error(
                &primary_host,
                public_port,
                loopback_error,
                daemon_reachable_directly,
            )
            .into());
        }

        let probe_ok = probe_result.is_ok();
        let next_port = selected_public_url_port(
            public_port,
            public_url_port,
            probe_ok,
            require_loopback_https,
        );
        if next_port != public_url_port {
            crate::output::warning(
                "Local 80/443 forwarding is configured but the dev HTTPS endpoint is unreachable.",
            );
            crate::output::muted("Continuing with explicit dev port URL.");
            public_url_port = next_port;
        }
    }

    let final_dns_ip = if public_url_port == 443 {
        DEV_LOOPBACK_ADDR
    } else {
        "127.0.0.1"
    };
    if final_dns_ip != daemon_dns_ip {
        crate::dev_server_client::stop_server().await?;
        for _ in 0..40 {
            if crate::dev_server_client::info().await.is_err() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        if let Err(e) = crate::dev_server_client::ensure_running(&listen_addr, final_dns_ip).await {
            let msg = e.to_string();
            let _ = log_tx
                .send(ScopedLog::error(
                    "tako",
                    format!("✗ dev server failed to start: {}", msg),
                ))
                .await;
            let _ = event_tx.send(DevEvent::AppError(msg.clone())).await;
            return Err(msg.into());
        }
    }

    let tako_home = crate::paths::tako_home_dir()?;
    let (lock_guard, log_store_path) = match acquire_dev_lock(
        &tako_home,
        &project_dir,
        &app_name,
        &primary_host,
        public_url_port,
    )? {
        DevLockAcquire::Owned(lock) => {
            let log_store_path = lock.log_path.clone();
            (lock, log_store_path)
        }
        DevLockAcquire::Attached(session) => {
            return run_attached_dev_client(&app_name, use_tui, session).await;
        }
    };
    let _lock = lock_guard;

    prepare_shared_log_store_for_new_owner(&log_store_path).await;
    let (log_watch_stop_tx, log_watch_stop_rx) = watch::channel(false);
    {
        let log_store_path = log_store_path.clone();
        let event_tx = event_tx.clone();
        tokio::spawn(async move {
            replay_and_follow_logs(
                log_store_path,
                None,
                Some(event_tx),
                log_watch_stop_rx,
                true,
            )
            .await;
        });
    }

    let token = crate::dev_server_client::get_token().await?;

    // Keep one app process running on startup.
    let child_state = std::sync::Arc::new(tokio::sync::Mutex::new(None::<tokio::process::Child>));
    let reserve_state = std::sync::Arc::new(tokio::sync::Mutex::new(Some(reserve_listener)));
    if dev_initial_instance_count() > 0 {
        // Free the reserved port so the app process can bind immediately.
        let _ = reserve_state.lock().await.take();

        let env = env_state.lock().await.clone();
        match spawn_app_process(
            &cmd,
            &env,
            &project_dir,
            upstream_port,
            log_tx.clone(),
            app_log_scope(),
        )
        .await
        {
            Ok(child) => {
                if let Some(pid) = child.id() {
                    emit_persisted_app_event(&event_tx, &log_store_path, DevEvent::AppPid(pid))
                        .await;
                }
                *child_state.lock().await = Some(child);
                emit_persisted_app_event(&event_tx, &log_store_path, DevEvent::AppStarted).await;
            }
            Err(e) => {
                let msg = e.to_string();
                let _ = log_tx
                    .send(ScopedLog::error(
                        "tako",
                        format!("✗ failed to start app: {}", msg),
                    ))
                    .await;
                emit_persisted_app_event(&event_tx, &log_store_path, DevEvent::AppError(msg)).await;
            }
        }
    }

    // Register the lease (daemon routes by Host header).
    let lease_hosts = hosts_state.lock().await.clone();
    let lease_active = child_state.lock().await.is_some();
    let lease = crate::dev_server_client::register_lease(
        &token,
        &app_name,
        &lease_hosts,
        upstream_port,
        lease_active,
        LEASE_TTL_MS,
    )
    .await?;

    if lease_hosts
        .iter()
        .any(|h| h.ends_with(&format!(".{}", crate::dev::TAKO_LOCAL_DOMAIN)))
        && let Ok(info) = crate::dev_server_client::info().await
    {
        let local_dns_enabled = info
            .get("info")
            .and_then(|i| i.get("local_dns_enabled"))
            .and_then(|b| b.as_bool())
            .unwrap_or(false);
        if !local_dns_enabled {
            crate::output::warning(
                "Local DNS is unavailable; .tako.local hostnames may not resolve.",
            );
            crate::output::muted("Run `tako doctor` for diagnostics.");
        }
    }
    let lease_id = std::sync::Arc::new(tokio::sync::Mutex::new(lease.lease_id));

    if use_tui {
        let public_port_for_tui = public_url_port;
        let hosts = hosts_state.lock().await.clone();
        let app_name_for_tui = app_name.clone();
        let adapter_name_for_tui = runtime_name.clone();
        let control_tx_for_tui = control_tx.clone();
        let log_store_for_tui = log_store_path.clone();
        let log_rx = log_rx_opt.take().unwrap();
        let event_rx = event_rx_opt.take().unwrap();
        tui_handle = Some(tokio::spawn(async move {
            tui::run_dev_tui(
                app_name_for_tui,
                adapter_name_for_tui,
                hosts,
                public_port_for_tui,
                upstream_port,
                log_rx,
                event_rx,
                control_tx_for_tui,
                Some(log_store_for_tui),
            )
            .await
            .map_err(|e| e.to_string())
        }));
    }

    let verbose = crate::output::is_verbose();
    let url = preferred_public_url(&primary_host, &lease.url, public_port, public_url_port);
    if !use_tui {
        for line in dev_startup_lines(
            verbose,
            &app_name,
            &runtime_name,
            &project_dir.join(&main),
            &url,
        ) {
            println!("{}", line);
        }
    }

    // Lease heartbeat loop.
    let (hb_stop_tx, mut hb_stop_rx) = watch::channel(false);
    {
        let token = token.clone();
        let app_name = app_name.clone();
        let lease_id = lease_id.clone();
        let log_tx = log_tx.clone();
        let hosts_state = hosts_state.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(Duration::from_secs(LEASE_HEARTBEAT_SECS));
            loop {
                tokio::select! {
                    _ = ticker.tick() => {
                        let id = lease_id.lock().await.clone();
                        let err_msg = match crate::dev_server_client::renew_lease(&token, &id, LEASE_TTL_MS).await {
                            Ok(()) => None,
                            Err(e) => Some(e.to_string()),
                        };
                        if let Some(msg) = err_msg {
                            // If the lease got lost (daemon restart), try to re-register.
                            let hosts = hosts_state.lock().await.clone();
                            let new_id = crate::dev_server_client::register_lease(
                                &token,
                                &app_name,
                                &hosts,
                                upstream_port,
                                false,
                                LEASE_TTL_MS,
                            )
                            .await
                            .ok()
                            .map(|info| info.lease_id);

                            if let Some(new_id) = new_id {
                                *lease_id.lock().await = new_id;
                            } else {
                                        let _ = log_tx
                                            .send(ScopedLog::warn(
                                                "tako",
                                                format!("✗ lease heartbeat failed: {}", msg),
                                            ))
                                            .await;
                            }
                        }
                    }
                    _ = hb_stop_rx.changed() => {
                        if *hb_stop_rx.borrow() {
                            break;
                        }
                    }
                }
            }
        });
    }

    // Watch tako.toml for config changes (env vars + dev routes).
    let (cfg_tx, cfg_rx) = mpsc::channel::<()>(8);
    let _cfg_handle = watcher::ConfigWatcher::new(project_dir.clone(), cfg_tx)?.start()?;

    if verbose && !use_tui {
        println!(
            "Starting server at {}...",
            dev_url(&primary_host, public_url_port)
        );
        println!("Press Ctrl+c to stop");
        println!();
    }

    // Supervisor: apply control commands to the local child process.
    {
        let child_state = child_state.clone();
        let reserve_state = reserve_state.clone();
        let token = token.clone();
        let lease_id = lease_id.clone();
        let cmd = cmd.clone();
        let env_state = env_state.clone();
        let project_dir = project_dir.clone();
        let app_name = app_name.clone();
        let log_tx = log_tx.clone();
        let event_tx = event_tx.clone();
        let log_store_path = log_store_path.clone();
        let should_exit_tx = should_exit_tx.clone();
        let terminate_requested = terminate_requested.clone();
        tokio::spawn(async move {
            while let Some(cmd_in) = control_rx.recv().await {
                match cmd_in {
                    tui::ControlCmd::Restart => {
                        let mut lock = child_state.lock().await;
                        if let Some(mut child) = lock.take() {
                            let _ = child.kill().await;
                            let _ = child.wait().await;
                        }

                        // Ensure the reserved port is free for the app.
                        let _ = reserve_state.lock().await.take();

                        let env = env_state.lock().await.clone();
                        let restarted = spawn_app_process(
                            &cmd,
                            &env,
                            &project_dir,
                            upstream_port,
                            log_tx.clone(),
                            app_log_scope(),
                        )
                        .await
                        .map_err(|e| e.to_string());
                        match restarted {
                            Ok(child) => {
                                let id = lease_id.lock().await.clone();
                                let _ =
                                    crate::dev_server_client::set_lease_active(&token, &id, true)
                                        .await;
                                if let Some(pid) = child.id() {
                                    emit_persisted_app_event(
                                        &event_tx,
                                        &log_store_path,
                                        DevEvent::AppPid(pid),
                                    )
                                    .await;
                                }
                                *lock = Some(child);
                                emit_persisted_app_event(
                                    &event_tx,
                                    &log_store_path,
                                    DevEvent::AppStarted,
                                )
                                .await;
                            }
                            Err(msg) => {
                                let _ = log_tx
                                    .send(ScopedLog::error(
                                        "tako",
                                        format!("✗ restart failed: {}", msg),
                                    ))
                                    .await;
                                emit_persisted_app_event(
                                    &event_tx,
                                    &log_store_path,
                                    DevEvent::AppError(msg),
                                )
                                .await;
                            }
                        }
                    }
                    tui::ControlCmd::Terminate => {
                        terminate_requested.store(true, Ordering::Relaxed);
                        let mut lock = child_state.lock().await;
                        if let Some(mut child) = lock.take() {
                            let _ = child.kill().await;
                            let _ = child.wait().await;
                        }
                        let id = lease_id.lock().await.clone();
                        let _ =
                            crate::dev_server_client::set_lease_active(&token, &id, false).await;
                        emit_persisted_app_event(&event_tx, &log_store_path, DevEvent::AppStopped)
                            .await;
                        let _ = should_exit_tx.send(true);
                        break;
                    }
                    tui::ControlCmd::ClearLogs => {
                        append_clear_logs_marker_to_store(&log_store_path).await;
                        let _ = event_tx.send(DevEvent::LogsCleared).await;
                    }
                }
            }

            // Keep lints happy.
            let _ = app_name;
        });
    }

    // Config change loop (tako.toml): update env + routing.
    {
        let project_dir = project_dir.clone();
        let app_name = app_name.clone();
        let domain = domain.clone();
        let env_state = env_state.clone();
        let hosts_state = hosts_state.clone();
        let token = token.clone();
        let lease_id = lease_id.clone();
        let child_state = child_state.clone();
        let log_tx = log_tx.clone();
        let mut cfg_rx = cfg_rx;
        let control_tx = control_tx.clone();
        tokio::spawn(async move {
            while cfg_rx.recv().await.is_some() {
                let cfg = match load_dev_tako_toml(&project_dir) {
                    Ok(c) => c,
                    Err(e) => {
                        let msg = e.to_string();
                        let _ = log_tx
                            .send(ScopedLog::error(
                                "tako",
                                format!("✗ tako.toml parse error: {}", msg),
                            ))
                            .await;
                        continue;
                    }
                };

                let new_env = compute_dev_env(&cfg);
                let mut restart_needed = false;
                {
                    let mut cur = env_state.lock().await;
                    if *cur != new_env {
                        *cur = new_env;
                        restart_needed = true;
                    }
                }

                let new_hosts = match compute_dev_hosts(&app_name, &cfg, &domain) {
                    Ok(hosts) => hosts,
                    Err(msg) => {
                        let _ = log_tx
                            .send(ScopedLog::error(
                                "tako",
                                format!("✗ invalid development routes: {}", msg),
                            ))
                            .await;
                        continue;
                    }
                };
                let hosts_changed = {
                    let mut cur_hosts = hosts_state.lock().await;
                    if *cur_hosts != new_hosts {
                        *cur_hosts = new_hosts.clone();
                        true
                    } else {
                        false
                    }
                };

                if hosts_changed {
                    let is_active = child_state.lock().await.is_some();
                    let r = crate::dev_server_client::register_lease(
                        &token,
                        &app_name,
                        &new_hosts,
                        upstream_port,
                        is_active,
                        LEASE_TTL_MS,
                    )
                    .await
                    .map_err(|e| e.to_string());

                    match r {
                        Ok(info) => {
                            *lease_id.lock().await = info.lease_id;
                            let _ = log_tx
                                .send(ScopedLog::info(
                                    "tako",
                                    "✓ updated dev routing from tako.toml",
                                ))
                                .await;
                        }
                        Err(msg) => {
                            let _ = log_tx
                                .send(ScopedLog::warn(
                                    "tako",
                                    format!("✗ failed to update dev routing: {}", msg),
                                ))
                                .await;
                        }
                    }
                }

                if restart_needed {
                    let _ = log_tx
                        .send(ScopedLog::info(
                            "tako",
                            "✓ env changed in tako.toml; restarting",
                        ))
                        .await;
                    let _ = control_tx.send(tui::ControlCmd::Restart).await;
                }
            }
        });
    }

    // Scale-to-0 on idle and wake on request.
    {
        let last_req = std::sync::Arc::new(tokio::sync::Mutex::new(std::time::Instant::now()));
        let inflight = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let last_req2 = last_req.clone();

        {
            let token = token.clone();
            let lease_id = lease_id.clone();
            let lease_hosts = lease_hosts.clone();
            let child_state = child_state.clone();
            let reserve_state = reserve_state.clone();
            let cmd = cmd.clone();
            let env_state = env_state.clone();
            let project_dir = project_dir.clone();
            let log_tx = log_tx.clone();
            let event_tx = event_tx.clone();
            let log_store_path = log_store_path.clone();
            let last_req = last_req.clone();
            let inflight = inflight.clone();

            let mut ev_rx = match crate::dev_server_client::subscribe_events(&token).await {
                Ok(rx) => Some(rx),
                Err(e) => {
                    let _ = log_tx
                        .send(ScopedLog::warn(
                            "tako",
                            format!("✗ failed to subscribe to dev server events: {}", e),
                        ))
                        .await;
                    None
                }
            };

            if let Some(mut ev_rx) = ev_rx.take() {
                tokio::spawn(async move {
                    while let Some(ev) = ev_rx.recv().await {
                        match ev {
                            crate::dev_server_client::DevServerEvent::RequestStarted { host } => {
                                if !lease_hosts.iter().any(|h| h == &host) {
                                    continue;
                                }

                                inflight.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                                *last_req.lock().await = std::time::Instant::now();

                                let mut lock = child_state.lock().await;
                                if lock.is_none() {
                                    emit_persisted_app_event(
                                        &event_tx,
                                        &log_store_path,
                                        DevEvent::AppLaunching,
                                    )
                                    .await;
                                    let _ = log_tx
                                        .send(ScopedLog::info(
                                            "tako",
                                            format!("starting app on request ({})", host),
                                        ))
                                        .await;

                                    // Free the reserved port for the app.
                                    let _ = reserve_state.lock().await.take();

                                    let env = env_state.lock().await.clone();
                                    match spawn_app_process(
                                        &cmd,
                                        &env,
                                        &project_dir,
                                        upstream_port,
                                        log_tx.clone(),
                                        app_log_scope(),
                                    )
                                    .await
                                    {
                                        Ok(child) => {
                                            let id = lease_id.lock().await.clone();
                                            let _ = crate::dev_server_client::set_lease_active(
                                                &token, &id, true,
                                            )
                                            .await;
                                            if let Some(pid) = child.id() {
                                                emit_persisted_app_event(
                                                    &event_tx,
                                                    &log_store_path,
                                                    DevEvent::AppPid(pid),
                                                )
                                                .await;
                                            }
                                            *lock = Some(child);
                                            emit_persisted_app_event(
                                                &event_tx,
                                                &log_store_path,
                                                DevEvent::AppStarted,
                                            )
                                            .await;
                                        }
                                        Err(e) => {
                                            let msg = e.to_string();
                                            drop(e);
                                            let _ = log_tx
                                                .send(ScopedLog::error(
                                                    "tako",
                                                    format!("✗ failed to start app: {}", msg),
                                                ))
                                                .await;
                                            emit_persisted_app_event(
                                                &event_tx,
                                                &log_store_path,
                                                DevEvent::AppError(msg),
                                            )
                                            .await;
                                        }
                                    }
                                }
                            }
                            crate::dev_server_client::DevServerEvent::RequestFinished { host } => {
                                if !lease_hosts.iter().any(|h| h == &host) {
                                    continue;
                                }
                                inflight
                                    .fetch_update(
                                        std::sync::atomic::Ordering::Relaxed,
                                        std::sync::atomic::Ordering::Relaxed,
                                        |v| Some(v.saturating_sub(1)),
                                    )
                                    .ok();
                            }
                        }
                    }
                });
            }
        }

        {
            let token = token.clone();
            let lease_id = lease_id.clone();
            let child_state = child_state.clone();
            let reserve_state = reserve_state.clone();
            let event_tx = event_tx.clone();
            let log_store_path = log_store_path.clone();
            let inflight = inflight.clone();
            tokio::spawn(async move {
                let idle_timeout = dev_idle_timeout();
                let mut ticker = tokio::time::interval(Duration::from_secs(10));
                loop {
                    ticker.tick().await;
                    let idle_for =
                        std::time::Instant::now().duration_since(*last_req2.lock().await);
                    if idle_for < idle_timeout {
                        continue;
                    }

                    if inflight.load(std::sync::atomic::Ordering::Relaxed) > 0 {
                        continue;
                    }

                    let mut lock = child_state.lock().await;
                    let Some(mut child) = lock.take() else {
                        continue;
                    };

                    let _ = child.kill().await;
                    let _ = child.wait().await;

                    emit_persisted_app_event(&event_tx, &log_store_path, DevEvent::AppStopped)
                        .await;

                    let id = lease_id.lock().await.clone();
                    let _ = crate::dev_server_client::set_lease_active(&token, &id, false).await;

                    // Re-reserve the upstream port.
                    if reserve_state.lock().await.is_none()
                        && let Ok(std_listener) =
                            std::net::TcpListener::bind(("127.0.0.1", upstream_port))
                    {
                        let _ = std_listener.set_nonblocking(true);
                        if let Ok(listener) = TcpListener::from_std(std_listener) {
                            *reserve_state.lock().await = Some(listener);
                        }
                    }
                }
            });
        }
    }

    {
        let should_exit_tx_ctrlc = should_exit_tx.clone();
        tokio::spawn(async move {
            if tokio::signal::ctrl_c().await.is_ok() {
                let _ = should_exit_tx_ctrlc.send(true);
                if verbose {
                    println!("\nShutting down...");
                }
            }
        });
    }
    #[cfg(unix)]
    {
        let should_exit_tx_term = should_exit_tx.clone();
        let terminate_requested = terminate_requested.clone();
        tokio::spawn(async move {
            if let Ok(mut sigterm) =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            {
                let _ = sigterm.recv().await;
                terminate_requested.store(true, Ordering::Relaxed);
                let _ = should_exit_tx_term.send(true);
                if verbose {
                    println!("\nTerminating...");
                }
            }
        });

        let should_exit_tx_hup = should_exit_tx.clone();
        tokio::spawn(async move {
            if let Ok(mut sighup) =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())
            {
                let _ = sighup.recv().await;
                let _ = should_exit_tx_hup.send(true);
                if verbose {
                    println!("\nDisconnected from terminal.");
                }
            }
        });
    }

    if use_tui {
        if let Some(mut handle) = tui_handle.take() {
            let mut tui_result = None;
            tokio::select! {
                r = &mut handle => {
                    tui_result = Some(r);
                }
                _ = async {
                    while should_exit_rx.changed().await.is_ok() {
                        if *should_exit_rx.borrow() {
                            break;
                        }
                    }
                } => {}
            }

            if let Some(r) = tui_result {
                match r {
                    Ok(r) => {
                        if let Err(msg) = r {
                            return Err(msg.into());
                        }
                    }
                    Err(e) => {
                        return Err(format!("tui task failed: {}", e).into());
                    }
                }
            } else {
                handle.abort();
                let _ = handle.await;
            }
        }
    } else {
        let mut log_rx = log_rx_opt.take().expect("non-TUI should have log rx");
        let mut event_rx = event_rx_opt.take().expect("non-TUI should have event rx");
        let log_store_path_for_stdout = log_store_path.clone();
        // Handle events and logs (plain stdout)
        tokio::select! {
            _ = async {
                loop {
                    tokio::select! {
                        Some(log) = log_rx.recv() => {
                            append_log_to_store(&log_store_path_for_stdout, &log).await;
                            println!(
                                "{} {:<5} [{}] {}",
                                log.timestamp, log.level, log.scope, log.message
                            );
                        }
                        Some(event) = event_rx.recv() => {
                            match event {
                                DevEvent::AppStarted => {
                                    // Intentionally quiet on normal startup.
                                }
                                DevEvent::AppLaunching => {
                                    println!("Starting app...");
                                }
                                DevEvent::AppStopped => {
                                    println!("○ App stopped (idle)");
                                }
                                DevEvent::AppPid(_) => {
                                    // No-op in non-TUI mode.
                                }
                                DevEvent::AppError(e) => {
                                    eprintln!("✗ App error: {}", e);
                                }
                                DevEvent::LogsCleared => {
                                    println!("logs cleared");
                                }
                                DevEvent::LogsReady => {}
                            }
                        }
                    }
                }
            } => {}
            _ = async {
                while should_exit_rx.changed().await.is_ok() {
                    if *should_exit_rx.borrow() {
                        break;
                    }
                }
            } => {}
        }
    }

    // Cleanup.
    let _ = hb_stop_tx.send(true);
    let id = lease_id.lock().await.clone();
    let mut child = {
        let mut lock = child_state.lock().await;
        lock.take()
    };
    let app_was_running = child.is_some();
    if let Some(mut child) = child.take() {
        let _ = child.kill().await;
        let _ = child.wait().await;
    }
    let mut kept_alive = false;
    if should_linger_after_disconnect(terminate_requested.load(Ordering::Relaxed), app_was_running)
    {
        let env = env_state.lock().await.clone();
        let lease_renewed =
            crate::dev_server_client::renew_lease(&token, &id, dev_disconnect_grace_ttl_ms())
                .await
                .is_ok();
        let lease_reactivated = crate::dev_server_client::set_lease_active(&token, &id, true)
            .await
            .is_ok();

        if lease_renewed && lease_reactivated {
            match spawn_lingering_app_process(
                &cmd,
                &env,
                &project_dir,
                upstream_port,
                dev_disconnect_grace(),
            ) {
                Ok(_) => {
                    kept_alive = true;
                    crate::output::muted(&format!(
                        "Session ended. Keeping app and routes alive for {}.",
                        dev_disconnect_grace_label()
                    ));
                }
                Err(e) => {
                    crate::output::warning(&format!(
                        "Could not keep app alive after disconnect: {}",
                        e
                    ));
                }
            }
        } else {
            crate::output::warning(
                "Could not extend dev lease after disconnect; stopping app immediately.",
            );
        }
    }

    if !kept_alive {
        let _ = crate::dev_server_client::unregister_lease(&token, &id).await;
    }
    let _ = log_watch_stop_tx.send(true);
    if verbose {
        println!("Goodbye!");
    }
    Ok(())
}

fn reserve_ephemeral_port() -> Result<(u16, TcpListener), Box<dyn std::error::Error>> {
    let std_listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    std_listener.set_nonblocking(true)?;
    let port = std_listener.local_addr()?.port();
    let listener = TcpListener::from_std(std_listener)?;
    Ok((port, listener))
}

#[derive(Debug)]
struct DevLock {
    path: std::path::PathBuf,
    log_path: std::path::PathBuf,
}

#[derive(Debug, Clone)]
struct AttachedDevSession {
    owner_pid: u32,
    url: String,
    log_path: std::path::PathBuf,
}

#[derive(Debug)]
enum DevLockAcquire {
    Owned(DevLock),
    Attached(AttachedDevSession),
}

#[derive(Debug, Default)]
struct LockFileMetadata {
    pid: Option<u32>,
    url: Option<String>,
    log_path: Option<std::path::PathBuf>,
}

impl Drop for DevLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn acquire_dev_lock(
    tako_home: &std::path::Path,
    project_dir: &std::path::Path,
    app_name: &str,
    domain: &str,
    public_port: u16,
) -> Result<DevLockAcquire, Box<dyn std::error::Error>> {
    let lock_dir = tako_home.join("dev").join("locks");
    let log_dir = tako_home.join("dev").join("logs");
    std::fs::create_dir_all(&lock_dir)?;
    std::fs::create_dir_all(&log_dir)?;

    let suffix = dev_session_suffix(project_dir);
    let path = lock_dir.join(format!("{}-{}.lock", app_name, suffix));
    let log_path = log_dir.join(format!("{}-{}.jsonl", app_name, suffix));
    let default_url = dev_url(domain, public_port);
    let deadline = std::time::Instant::now() + Duration::from_millis(DEV_LOCK_ACQUIRE_TIMEOUT_MS);
    let poll_interval = Duration::from_millis(DEV_LOCK_RETRY_POLL_MS);
    let incomplete_stale_age = Duration::from_millis(DEV_LOCK_INCOMPLETE_STALE_AGE_MS);

    loop {
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut f) => {
                let pid = std::process::id();
                let lock_contents = format!(
                    "pid={pid}\nurl={default_url}\nlog_path={}\n",
                    log_path.display()
                );
                f.write_all(lock_contents.as_bytes())?;
                return Ok(DevLockAcquire::Owned(DevLock {
                    path,
                    log_path: log_path.clone(),
                }));
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                let contents = std::fs::read_to_string(&path).unwrap_or_default();
                let meta = parse_lock_file_metadata(
                    &contents,
                    fallback_log_path_for_lock_file(&path, &log_dir),
                );

                if let Some(pid) = meta.pid
                    && process_is_running(pid)
                {
                    return Ok(DevLockAcquire::Attached(AttachedDevSession {
                        owner_pid: pid,
                        url: meta.url.unwrap_or_else(|| default_url.clone()),
                        log_path: meta.log_path.unwrap_or_else(|| log_path.clone()),
                    }));
                }

                if meta.pid.is_some() {
                    // Stale lock with dead owner PID.
                    let _ = std::fs::remove_file(&path);
                    continue;
                }

                // Lock file can be temporarily incomplete while another process is writing it.
                if lock_file_older_than(&path, incomplete_stale_age) {
                    let _ = std::fs::remove_file(&path);
                    continue;
                }

                if std::time::Instant::now() >= deadline {
                    break;
                }
                std::thread::sleep(poll_interval);
            }
            Err(e) => return Err(e.into()),
        }
    }

    Err("could not acquire dev lock".into())
}

fn lock_file_older_than(path: &Path, age: Duration) -> bool {
    let modified = std::fs::metadata(path).ok().and_then(|m| m.modified().ok());
    modified
        .and_then(|t| std::time::SystemTime::now().duration_since(t).ok())
        .is_some_and(|elapsed| elapsed >= age)
}

fn listed_apps_contain_app(apps: &[crate::dev_server_client::ListedApp], app_name: &str) -> bool {
    apps.iter().any(|app| app.app_name == app_name)
}

async fn attached_app_still_registered(app_name: &str) -> bool {
    crate::dev_server_client::list_apps()
        .await
        .ok()
        .is_some_and(|apps| listed_apps_contain_app(&apps, app_name))
}

fn dev_session_suffix(project_dir: &Path) -> String {
    let canonical =
        std::fs::canonicalize(project_dir).unwrap_or_else(|_| project_dir.to_path_buf());
    let mut h = sha2::Sha256::new();
    h.update(canonical.to_string_lossy().as_bytes());
    let digest = h.finalize();
    hex::encode(&digest[..4])
}

fn fallback_log_path_for_lock_file(lock_path: &Path, log_dir: &Path) -> PathBuf {
    let stem = lock_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("dev-session");
    log_dir.join(format!("{stem}.jsonl"))
}

fn parse_lock_file_metadata(contents: &str, fallback_log_path: PathBuf) -> LockFileMetadata {
    let mut meta = LockFileMetadata::default();
    for line in contents.lines() {
        if let Some(v) = line.strip_prefix("pid=") {
            meta.pid = v.parse::<u32>().ok();
            continue;
        }
        if let Some(v) = line.strip_prefix("url=") {
            let trimmed = v.trim();
            if !trimmed.is_empty() {
                meta.url = Some(trimmed.to_string());
            }
            continue;
        }
        if let Some(v) = line.strip_prefix("log_path=") {
            let trimmed = v.trim();
            if !trimmed.is_empty() {
                meta.log_path = Some(PathBuf::from(trimmed));
            }
        }
    }

    if meta.log_path.is_none() {
        meta.log_path = Some(fallback_log_path);
    }
    meta
}

fn process_is_running(pid: u32) -> bool {
    let mut sys = sysinfo::System::new();
    sys.refresh_processes(
        sysinfo::ProcessesToUpdate::Some(&[sysinfo::Pid::from_u32(pid)]),
        false,
    );
    sys.process(sysinfo::Pid::from_u32(pid)).is_some()
}

#[derive(Debug)]
enum StoredLogEvent {
    Log(ScopedLog),
    ClearLogs,
    AppEvent(DevEvent),
}

fn parse_app_event_marker(v: &serde_json::Value) -> Option<DevEvent> {
    if v.get("type").and_then(|x| x.as_str()) != Some(DEV_LOG_APP_EVENT_MARKER_TYPE) {
        return None;
    }

    let event = v.get("event").and_then(|x| x.as_str())?;
    match event {
        "launching" => Some(DevEvent::AppLaunching),
        "started" => Some(DevEvent::AppStarted),
        "stopped" => Some(DevEvent::AppStopped),
        "pid" => v
            .get("pid")
            .and_then(|x| x.as_u64())
            .and_then(|pid| u32::try_from(pid).ok())
            .map(DevEvent::AppPid),
        "error" => v
            .get("message")
            .and_then(|x| x.as_str())
            .map(|msg| DevEvent::AppError(msg.to_string())),
        _ => None,
    }
}

fn app_event_marker_payload(event: &DevEvent) -> Option<serde_json::Value> {
    match event {
        DevEvent::AppLaunching => {
            Some(serde_json::json!({ "type": DEV_LOG_APP_EVENT_MARKER_TYPE, "event": "launching" }))
        }
        DevEvent::AppStarted => {
            Some(serde_json::json!({ "type": DEV_LOG_APP_EVENT_MARKER_TYPE, "event": "started" }))
        }
        DevEvent::AppStopped => {
            Some(serde_json::json!({ "type": DEV_LOG_APP_EVENT_MARKER_TYPE, "event": "stopped" }))
        }
        DevEvent::AppPid(pid) => Some(serde_json::json!({
            "type": DEV_LOG_APP_EVENT_MARKER_TYPE,
            "event": "pid",
            "pid": pid,
        })),
        DevEvent::AppError(message) => Some(serde_json::json!({
            "type": DEV_LOG_APP_EVENT_MARKER_TYPE,
            "event": "error",
            "message": message,
        })),
        DevEvent::LogsCleared | DevEvent::LogsReady => None,
    }
}

fn parse_stored_log_line(line: &str) -> Option<StoredLogEvent> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }

    if let Ok(log) = serde_json::from_str::<ScopedLog>(trimmed) {
        return Some(StoredLogEvent::Log(log));
    }

    if let Ok(v) = serde_json::from_str::<serde_json::Value>(trimmed)
        && let Some(marker_type) = v.get("type").and_then(|x| x.as_str())
    {
        if marker_type == DEV_LOG_CLEAR_MARKER_TYPE {
            return Some(StoredLogEvent::ClearLogs);
        }

        if marker_type == DEV_LOG_APP_EVENT_MARKER_TYPE {
            return parse_app_event_marker(&v).map(StoredLogEvent::AppEvent);
        }
    }

    Some(StoredLogEvent::Log(ScopedLog::info(
        "app",
        trimmed.to_string(),
    )))
}

#[cfg(test)]
async fn ensure_shared_log_store(log_path: &Path) {
    if let Some(parent) = log_path.parent() {
        let _ = tokio::fs::create_dir_all(parent).await;
    }

    let _ = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
        .await;
}

#[cfg(test)]
async fn reset_shared_log_store(log_path: &Path) {
    if let Some(parent) = log_path.parent() {
        let _ = tokio::fs::create_dir_all(parent).await;
    }

    let _ = tokio::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(log_path)
        .await;
}

async fn prepare_shared_log_store_for_new_owner(log_path: &Path) {
    if let Some(parent) = log_path.parent() {
        let _ = tokio::fs::create_dir_all(parent).await;
    }

    let marker = format!(r#"{{"type":"{}"}}"#, DEV_LOG_CLEAR_MARKER_TYPE);
    let _ = tokio::fs::write(log_path, marker + "\n").await;
}

async fn append_log_to_store(log_path: &Path, line: &ScopedLog) {
    let mut encoded = match serde_json::to_string(line) {
        Ok(s) => s,
        Err(_) => return,
    };
    encoded.push('\n');

    let file = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
        .await;

    let Ok(mut file) = file else {
        return;
    };
    let _ = file.write_all(encoded.as_bytes()).await;
}

async fn append_clear_logs_marker_to_store(log_path: &Path) {
    let mut marker = format!(r#"{{"type":"{}"}}"#, DEV_LOG_CLEAR_MARKER_TYPE);
    marker.push('\n');
    let file = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
        .await;

    let Ok(mut file) = file else {
        return;
    };

    let _ = file.write_all(marker.as_bytes()).await;
}

async fn append_app_event_marker_to_store(log_path: &Path, event: &DevEvent) {
    let Some(marker) = app_event_marker_payload(event) else {
        return;
    };

    let mut encoded = marker.to_string();
    encoded.push('\n');

    let file = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
        .await;

    let Ok(mut file) = file else {
        return;
    };

    let _ = file.write_all(encoded.as_bytes()).await;
}

async fn emit_persisted_app_event(
    event_tx: &mpsc::Sender<DevEvent>,
    log_path: &Path,
    event: DevEvent,
) {
    append_app_event_marker_to_store(log_path, &event).await;
    let _ = event_tx.send(event).await;
}

async fn replay_and_follow_logs(
    log_path: PathBuf,
    log_tx: Option<mpsc::Sender<ScopedLog>>,
    event_tx: Option<mpsc::Sender<DevEvent>>,
    mut stop_rx: watch::Receiver<bool>,
    start_from_end: bool,
) {
    loop {
        if *stop_rx.borrow() {
            return;
        }

        let mut file = match tokio::fs::File::open(&log_path).await {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                tokio::time::sleep(Duration::from_millis(DEV_LOG_TAIL_POLL_MS)).await;
                continue;
            }
            Err(_) => return,
        };

        if start_from_end {
            let _ = file.seek(std::io::SeekFrom::End(0)).await;
        }

        let mut reader = tokio::io::BufReader::new(file);
        let mut buf = String::new();
        let mut replay_completed = false;

        loop {
            tokio::select! {
                changed = stop_rx.changed() => {
                    if changed.is_ok() && *stop_rx.borrow() {
                        return;
                    }
                }
                read = reader.read_line(&mut buf) => {
                    match read {
                        Ok(0) => {
                            if !replay_completed {
                                replay_completed = true;
                                if let Some(tx) = event_tx.as_ref() {
                                    let _ = tx.send(DevEvent::LogsReady).await;
                                }
                            }
                            buf.clear();
                            tokio::time::sleep(Duration::from_millis(DEV_LOG_TAIL_POLL_MS)).await;
                        }
                        Ok(_) => {
                            match parse_stored_log_line(&buf) {
                                Some(StoredLogEvent::Log(log)) => {
                                    if let Some(tx) = log_tx.as_ref() {
                                        let _ = tx.send(log).await;
                                    }
                                }
                                Some(StoredLogEvent::ClearLogs) => {
                                    if let Some(tx) = event_tx.as_ref() {
                                        let _ = tx.send(DevEvent::LogsCleared).await;
                                    }
                                }
                                Some(StoredLogEvent::AppEvent(event)) => {
                                    // The owning session receives app lifecycle events directly
                                    // from the supervisor; only attached sessions should rebuild
                                    // app state from persisted markers.
                                    if !start_from_end
                                        && let Some(tx) = event_tx.as_ref()
                                    {
                                        let _ = tx.send(event).await;
                                    }
                                }
                                None => {}
                            }
                            buf.clear();
                        }
                        Err(_) => return,
                    }
                }
            }
        }
    }
}

fn host_and_port_from_url(url: &str) -> Option<(String, u16)> {
    let no_scheme = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let host_port = no_scheme.split('/').next().unwrap_or("");
    if host_port.is_empty() {
        return None;
    }

    if let Some((host, port)) = host_port.rsplit_once(':')
        && let Ok(port) = port.parse::<u16>()
    {
        return Some((host.to_string(), port));
    }

    Some((host_port.to_string(), 443))
}

async fn send_terminate_to_owner(pid: u32) -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(unix)]
    {
        let status = tokio::process::Command::new("kill")
            .args(["-TERM", &pid.to_string()])
            .status()
            .await?;
        if status.success() {
            Ok(())
        } else {
            Err(format!("failed to send SIGTERM to pid {pid}").into())
        }
    }

    #[cfg(not(unix))]
    {
        let status = tokio::process::Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T"])
            .status()
            .await?;
        if status.success() {
            Ok(())
        } else {
            Err(format!("failed to terminate pid {pid}").into())
        }
    }
}

async fn run_attached_dev_client(
    app_name: &str,
    use_tui: bool,
    session: AttachedDevSession,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut hosts = Vec::new();
    let mut public_port = host_and_port_from_url(&session.url)
        .map(|(_, p)| p)
        .unwrap_or(443);
    let mut upstream_port = 0u16;

    if let Ok(info) = crate::dev_server_client::info().await {
        public_port = info
            .get("info")
            .and_then(|i| i.get("port"))
            .and_then(|p| p.as_u64())
            .map(|p| p as u16)
            .unwrap_or(public_port);
    }

    if let Ok(apps) = crate::dev_server_client::list_apps().await
        && let Some(app) = apps.into_iter().find(|a| a.app_name == app_name)
    {
        if !app.hosts.is_empty() {
            hosts = app.hosts;
        }
        upstream_port = app.upstream_port;
    }

    if hosts.is_empty()
        && let Some((host, _)) = host_and_port_from_url(&session.url)
    {
        hosts.push(host);
    }

    let (log_tx, log_rx) = mpsc::channel::<ScopedLog>(1000);
    let (event_tx, event_rx) = mpsc::channel::<DevEvent>(32);
    let (control_tx, mut control_rx) = mpsc::channel::<tui::ControlCmd>(32);
    let (stop_tx, stop_rx) = watch::channel(false);

    // Count attached sessions as control clients by keeping an events subscription open.
    if let Ok(token) = crate::dev_server_client::get_token().await
        && let Ok(mut ev_rx) = crate::dev_server_client::subscribe_events(&token).await
    {
        tokio::spawn(async move { while ev_rx.recv().await.is_some() {} });
    }

    {
        let log_path = session.log_path.clone();
        let log_tx = log_tx.clone();
        let event_tx = event_tx.clone();
        let stop_rx = stop_rx.clone();
        tokio::spawn(async move {
            replay_and_follow_logs(log_path, Some(log_tx), Some(event_tx), stop_rx, false).await;
        });
    }

    {
        let log_tx = log_tx.clone();
        let event_tx = event_tx.clone();
        let stop_tx = stop_tx.clone();
        let owner_pid = session.owner_pid;
        let app_name = app_name.to_string();
        tokio::spawn(async move {
            let mut owner_exit_logged = false;
            loop {
                tokio::time::sleep(Duration::from_millis(500)).await;
                if !process_is_running(owner_pid) {
                    if !owner_exit_logged {
                        owner_exit_logged = true;
                        let _ = log_tx
                            .send(ScopedLog::info(
                                "tako",
                                "owning dev session ended; waiting for app shutdown",
                            ))
                            .await;
                    }

                    if attached_app_still_registered(&app_name).await {
                        continue;
                    }

                    let _ = log_tx
                        .send(ScopedLog::info("tako", "dev session terminated"))
                        .await;
                    let _ = event_tx.send(DevEvent::AppStopped).await;
                    let _ = stop_tx.send(true);
                    break;
                }
            }
        });
    }

    {
        let log_tx = log_tx.clone();
        let stop_tx = stop_tx.clone();
        let owner_pid = session.owner_pid;
        let log_path = session.log_path.clone();
        tokio::spawn(async move {
            while let Some(cmd) = control_rx.recv().await {
                match cmd {
                    tui::ControlCmd::Restart => {
                        let _ = log_tx
                            .send(ScopedLog::info(
                                "tako",
                                "restart is only available in the owning session",
                            ))
                            .await;
                    }
                    tui::ControlCmd::Terminate => {
                        let msg = match send_terminate_to_owner(owner_pid).await {
                            Ok(()) => "sent terminate signal".to_string(),
                            Err(e) => format!("failed to terminate owner: {}", e),
                        };
                        let _ = log_tx.send(ScopedLog::info("tako", msg)).await;
                        let _ = stop_tx.send(true);
                        break;
                    }
                    tui::ControlCmd::ClearLogs => {
                        append_clear_logs_marker_to_store(&log_path).await;
                    }
                }
            }
        });
    }

    if use_tui {
        let adapter_name = if let Ok(project_dir) = std::env::current_dir() {
            if let Ok(cfg) = load_dev_tako_toml(&project_dir) {
                if let Ok(preset_ref) = resolve_dev_preset_ref(&cfg) {
                    match load_build_preset(&project_dir, &preset_ref).await {
                        Ok((preset, _)) => preset.name,
                        Err(_) => infer_preset_name_from_ref(&preset_ref),
                    }
                } else {
                    String::new()
                }
            } else {
                String::new()
            }
        } else {
            String::new()
        };

        tui::run_dev_tui(
            app_name.to_string(),
            adapter_name,
            hosts,
            public_port,
            upstream_port,
            log_rx,
            event_rx,
            control_tx,
            None,
        )
        .await?;
    } else {
        println!("{}", session.url);
        println!("Attached to existing dev session for '{}'.", app_name);

        let mut log_rx = log_rx;
        let mut event_rx = event_rx;
        let mut stop_rx = stop_rx.clone();
        tokio::select! {
            _ = async {
                loop {
                    tokio::select! {
                        Some(log) = log_rx.recv() => {
                            println!(
                                "{} {:<5} [{}] {}",
                                log.timestamp, log.level, log.scope, log.message
                            );
                        }
                        Some(event) = event_rx.recv() => {
                            match event {
                                DevEvent::AppStopped => println!("○ App stopped (idle)"),
                                DevEvent::AppError(e) => eprintln!("✗ App error: {}", e),
                                DevEvent::LogsCleared => println!("logs cleared"),
                                DevEvent::AppLaunching
                                | DevEvent::AppStarted
                                | DevEvent::AppPid(_)
                                | DevEvent::LogsReady => {}
                            }
                        }
                        else => break,
                    }
                }
            } => {}
            _ = async {
                while stop_rx.changed().await.is_ok() {
                    if *stop_rx.borrow() {
                        break;
                    }
                }
            } => {}
            _ = tokio::signal::ctrl_c() => {}
        }
    }

    let _ = stop_tx.send(true);
    Ok(())
}

#[cfg(test)]
mod dev_lock_tests {
    use super::*;

    #[test]
    fn dev_lock_attaches_second_instance() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();

        let project_dir = dir.path().join("proj");
        std::fs::create_dir_all(&project_dir).unwrap();

        let first = acquire_dev_lock(home, &project_dir, "app", "app.tako.local", 443).unwrap();
        let _first_lock = match first {
            DevLockAcquire::Owned(lock) => lock,
            DevLockAcquire::Attached(_) => panic!("first lock should be owned"),
        };

        let second = acquire_dev_lock(home, &project_dir, "app", "app.tako.local", 443).unwrap();
        match second {
            DevLockAcquire::Owned(_) => panic!("second lock should attach"),
            DevLockAcquire::Attached(session) => {
                assert_eq!(session.owner_pid, std::process::id());
                assert_eq!(session.url, "https://app.tako.local/");
                assert!(
                    session
                        .log_path
                        .file_name()
                        .and_then(|f| f.to_str())
                        .map(|f| f.starts_with("app-") && f.ends_with(".jsonl"))
                        .unwrap_or(false),
                    "expected app log store path, got: {}",
                    session.log_path.display()
                );
            }
        }
    }

    #[test]
    fn dev_lock_waits_for_inflight_metadata_write_and_attaches() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();

        let project_dir = dir.path().join("proj");
        std::fs::create_dir_all(&project_dir).unwrap();

        let lock_dir = home.join("dev").join("locks");
        let log_dir = home.join("dev").join("logs");
        std::fs::create_dir_all(&lock_dir).unwrap();
        std::fs::create_dir_all(&log_dir).unwrap();

        let suffix = dev_session_suffix(&project_dir);
        let lock_path = lock_dir.join(format!("app-{suffix}.lock"));
        let log_path = log_dir.join(format!("app-{suffix}.jsonl"));
        std::fs::write(&lock_path, b"").unwrap();

        let owner_pid = std::process::id();
        let lock_path_for_writer = lock_path.clone();
        let log_path_for_writer = log_path.clone();
        let writer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(40));
            let contents = format!(
                "pid={owner_pid}\nurl=https://app.tako.local/\nlog_path={}\n",
                log_path_for_writer.display()
            );
            std::fs::write(lock_path_for_writer, contents).unwrap();
        });

        let acquired = acquire_dev_lock(home, &project_dir, "app", "app.tako.local", 443).unwrap();
        writer.join().unwrap();

        match acquired {
            DevLockAcquire::Owned(_) => {
                panic!("expected to attach while lock metadata was being written")
            }
            DevLockAcquire::Attached(session) => {
                assert_eq!(session.owner_pid, owner_pid);
                assert_eq!(session.log_path, log_path);
            }
        }
    }

    #[test]
    fn dev_lock_allows_same_app_in_different_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();

        let p1 = dir.path().join("proj1");
        let p2 = dir.path().join("proj2");
        std::fs::create_dir_all(&p1).unwrap();
        std::fs::create_dir_all(&p2).unwrap();

        let l1 = acquire_dev_lock(home, &p1, "app", "app.tako.local", 443).unwrap();
        let l2 = acquire_dev_lock(home, &p2, "app", "app.tako.local", 443).unwrap();

        assert!(matches!(l1, DevLockAcquire::Owned(_)));
        assert!(matches!(l2, DevLockAcquire::Owned(_)));
    }

    #[test]
    fn lock_metadata_uses_fallback_log_path_when_missing() {
        let fallback = PathBuf::from("/tmp/fallback.jsonl");
        let meta =
            parse_lock_file_metadata("pid=42\nurl=https://app.tako.local/\n", fallback.clone());

        assert_eq!(meta.pid, Some(42));
        assert_eq!(meta.url.as_deref(), Some("https://app.tako.local/"));
        assert_eq!(meta.log_path, Some(fallback));
    }

    #[tokio::test]
    async fn ensure_shared_log_store_preserves_existing_contents() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("dev").join("logs").join("shared.jsonl");

        if let Some(parent) = log_path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&log_path, b"existing-line\n").unwrap();

        ensure_shared_log_store(&log_path).await;

        let contents = std::fs::read_to_string(&log_path).unwrap();
        assert_eq!(contents, "existing-line\n");
    }

    #[tokio::test]
    async fn reset_shared_log_store_truncates_existing_contents() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("dev").join("logs").join("shared.jsonl");

        if let Some(parent) = log_path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&log_path, b"old-line\n").unwrap();

        reset_shared_log_store(&log_path).await;

        let contents = std::fs::read_to_string(&log_path).unwrap();
        assert_eq!(contents, "");
    }

    #[tokio::test]
    async fn prepare_shared_log_store_for_new_owner_writes_clear_boundary_marker() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("dev").join("logs").join("shared.jsonl");

        if let Some(parent) = log_path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&log_path, b"old-line\n").unwrap();

        prepare_shared_log_store_for_new_owner(&log_path).await;

        let contents = std::fs::read_to_string(&log_path).unwrap();
        assert_eq!(
            contents,
            format!(r#"{{"type":"{}"}}"#, DEV_LOG_CLEAR_MARKER_TYPE) + "\n"
        );
    }
}

fn spawn_lingering_app_process(
    cmd: &[String],
    env: &std::collections::HashMap<String, String>,
    project_dir: &Path,
    port: u16,
    grace: Duration,
) -> Result<u32, Box<dyn std::error::Error>> {
    let pid = spawn_detached_app_process(cmd, env, project_dir, port)?;
    if let Err(e) = spawn_linger_reaper(pid, grace) {
        let _ = terminate_process_by_pid(pid);
        return Err(e);
    }
    Ok(pid)
}

fn spawn_detached_app_process(
    cmd: &[String],
    env: &std::collections::HashMap<String, String>,
    project_dir: &Path,
    port: u16,
) -> Result<u32, Box<dyn std::error::Error>> {
    if cmd.is_empty() {
        return Err("runtime returned empty run command".into());
    }

    let mut c = std::process::Command::new(&cmd[0]);
    if cmd.len() > 1 {
        c.args(&cmd[1..]);
    }
    c.current_dir(project_dir)
        .env("PORT", port.to_string())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());

    for (k, v) in env {
        c.env(k, v);
    }

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        c.process_group(0);
    }

    let child = c.spawn()?;
    Ok(child.id())
}

fn spawn_linger_reaper(pid: u32, grace: Duration) -> Result<(), Box<dyn std::error::Error>> {
    let secs = grace.as_secs().max(1);

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let script = format!(
            "sleep {secs}; kill -TERM {pid} >/dev/null 2>&1; sleep 2; kill -KILL {pid} >/dev/null 2>&1 || true"
        );
        let mut c = std::process::Command::new("sh");
        c.args(["-c", &script])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .process_group(0);
        let _ = c.spawn()?;
    }

    #[cfg(not(unix))]
    {
        let script =
            format!("timeout /T {secs} /NOBREAK >NUL & taskkill /PID {pid} /T /F >NUL 2>&1");
        let mut c = std::process::Command::new("cmd");
        c.args(["/C", &script])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        let _ = c.spawn()?;
    }

    Ok(())
}

fn terminate_process_by_pid(pid: u32) -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(unix)]
    {
        let status = std::process::Command::new("kill")
            .args(["-TERM", &pid.to_string()])
            .status()?;
        if status.success() {
            Ok(())
        } else {
            Err(format!("failed to terminate pid {pid}").into())
        }
    }

    #[cfg(not(unix))]
    {
        let status = std::process::Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .status()?;
        if status.success() {
            Ok(())
        } else {
            Err(format!("failed to terminate pid {pid}").into())
        }
    }
}

async fn spawn_app_process(
    cmd: &[String],
    env: &std::collections::HashMap<String, String>,
    project_dir: &Path,
    port: u16,
    log_tx: mpsc::Sender<ScopedLog>,
    scope: String,
) -> Result<tokio::process::Child, Box<dyn std::error::Error + Send + Sync>> {
    if cmd.is_empty() {
        return Err("runtime returned empty run command".into());
    }

    let mut c = tokio::process::Command::new(&cmd[0]);
    if cmd.len() > 1 {
        c.args(&cmd[1..]);
    }
    c.current_dir(project_dir)
        .env("PORT", port.to_string())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    for (k, v) in env {
        c.env(k, v);
    }

    let mut child = c.spawn()?;
    if let Some(stdout) = child.stdout.take() {
        let tx = log_tx.clone();
        let scope = scope.clone();
        tokio::spawn(async move { read_child_lines(stdout, tx, scope, LogLevel::Info).await });
    }
    if let Some(stderr) = child.stderr.take() {
        let tx = log_tx.clone();
        let scope = scope.clone();
        tokio::spawn(async move { read_child_lines(stderr, tx, scope, LogLevel::Warn).await });
    }
    Ok(child)
}

fn strip_ascii_case_prefix<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    if s.len() < prefix.len() {
        return None;
    }
    let (head, tail) = s.split_at(prefix.len());
    head.eq_ignore_ascii_case(prefix).then_some(tail)
}

fn prefixed_child_log_level_and_message(line: &str) -> Option<(LogLevel, String)> {
    let trimmed = line.trim_start();
    if trimmed.is_empty() {
        return None;
    }

    let candidates = [
        ("[TRACE]", LogLevel::Debug),
        ("[DEBUG]", LogLevel::Debug),
        ("[INFO]", LogLevel::Info),
        ("[WARN]", LogLevel::Warn),
        ("[WARNING]", LogLevel::Warn),
        ("[ERROR]", LogLevel::Error),
        ("[FATAL]", LogLevel::Fatal),
        ("TRACE", LogLevel::Debug),
        ("DEBUG", LogLevel::Debug),
        ("INFO", LogLevel::Info),
        ("WARN", LogLevel::Warn),
        ("WARNING", LogLevel::Warn),
        ("ERROR", LogLevel::Error),
        ("FATAL", LogLevel::Fatal),
    ];

    for (prefix, level) in candidates {
        let Some(rest) = strip_ascii_case_prefix(trimmed, prefix) else {
            continue;
        };

        if !prefix.starts_with('[')
            && rest
                .chars()
                .next()
                .is_some_and(|ch| !ch.is_whitespace() && ch != ':' && ch != '-' && ch != '|')
        {
            continue;
        }

        let message = rest.trim_start_matches(|ch: char| {
            ch.is_whitespace() || ch == ':' || ch == '-' || ch == '|'
        });
        let message = if message.is_empty() { trimmed } else { message };
        return Some((level.clone(), message.to_string()));
    }

    None
}

fn child_log_level_and_message(default_level: LogLevel, line: &str) -> (LogLevel, String) {
    prefixed_child_log_level_and_message(line).unwrap_or((default_level, line.to_string()))
}

async fn read_child_lines<R>(r: R, log_tx: mpsc::Sender<ScopedLog>, scope: String, level: LogLevel)
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut lines = tokio::io::BufReader::new(r).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let (line_level, line_message) = child_log_level_and_message(level.clone(), &line);
        let _ = log_tx
            .send(ScopedLog::at(line_level, scope.clone(), line_message))
            .await;
    }
}

/// Events from the dev server
#[derive(Debug, Clone)]
pub enum DevEvent {
    AppLaunching,
    AppStarted,
    AppStopped,
    AppPid(u32),
    AppError(String),
    LogsCleared,
    LogsReady,
}

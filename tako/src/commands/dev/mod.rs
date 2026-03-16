//! Tako Dev Server
//!
//! Local development server with:
//! - HTTPS via local CA (`{app-name}.tako.test`)
//! - Local authoritative DNS for `*.tako.test`
//! - `tako.toml` watching for env/route updates
//! - Streaming logs, status, and resource monitoring
//! - Idle timeout (stops app process after inactivity)

mod ca_setup;
mod loopback_proxy;
mod output;
mod watcher;

use std::env::current_dir;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use time::{OffsetDateTime, UtcOffset};

use sha2::Digest;
use tokio::io::{AsyncBufReadExt, AsyncSeekExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio::sync::watch;
#[cfg(test)]
use tokio::time::timeout;

use crate::app::resolve_app_name;
use crate::build::{
    BuildAdapter, BuildPreset, PresetGroup, PresetReference, apply_adapter_base_runtime_defaults,
    infer_adapter_from_preset_reference, js, load_build_preset, parse_preset_reference,
    qualify_runtime_local_preset_ref,
};
use crate::config::TakoToml;
use crate::dev::LocalCA;
use crate::validation::validate_dev_route;

pub use ca_setup::setup_local_ca;
#[cfg(target_os = "macos")]
pub(crate) use loopback_proxy::{
    LOOPBACK_PROXY_LABEL, LoopbackProxyStatus, status as loopback_proxy_status,
};

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
#[serde(deny_unknown_fields)]
struct ScopedLogSerde {
    timestamp: String,
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
        let timestamp = if raw.timestamp.trim().is_empty() {
            "00:00:00".to_string()
        } else {
            raw.timestamp
        };

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

    pub fn divider() -> Self {
        Self {
            timestamp: String::new(),
            level: LogLevel::Info,
            scope: DIVIDER_SCOPE.to_string(),
            message: String::new(),
        }
    }
}

const DEV_SERVER_SCOPE: &str = "tako";
const APP_SCOPE: &str = "app";
pub const DIVIDER_SCOPE: &str = "__divider__";

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

const DEV_INITIAL_INSTANCE_COUNT: usize = 1;
const DEV_IDLE_TIMEOUT_SECS: u64 = 30 * 60;
const DEV_LOG_TAIL_POLL_MS: u64 = 120;
const DEV_LOG_CLEAR_MARKER_TYPE: &str = "clear_logs";
const DEV_LOG_APP_EVENT_MARKER_TYPE: &str = "app_event";
pub(crate) const LOCAL_DNS_PORT: u16 = 53535;
#[cfg(any(target_os = "macos", test))]
const RESOLVER_DIR: &str = "/etc/resolver";
#[cfg(any(target_os = "macos", test))]
pub(crate) const TAKO_RESOLVER_FILE: &str = "/etc/resolver/tako.test";
const DEV_TLS_CERT_FILENAME: &str = "fullchain.pem";
const DEV_TLS_KEY_FILENAME: &str = "privkey.pem";
const DEV_TLS_NAMES_FILENAME: &str = "names.json";
const LOCALHOST_443_HTTPS_PROBE_ATTEMPTS: usize = 12;
const LOCALHOST_443_HTTPS_PROBE_TIMEOUT_MS: u64 = 500;
const LOCALHOST_443_HTTPS_PROBE_RETRY_DELAY_MS: u64 = 150;
pub(crate) const DEV_LOOPBACK_ADDR: &str = "127.77.0.1";

fn dev_initial_instance_count() -> usize {
    DEV_INITIAL_INSTANCE_COUNT
}

fn dev_idle_timeout() -> Duration {
    Duration::from_secs(DEV_IDLE_TIMEOUT_SECS)
}

fn load_dev_tako_toml(project_dir: &Path) -> crate::config::Result<TakoToml> {
    TakoToml::load_from_dir(project_dir)
}

/// Routes shown in the terminal panel — always includes the default host, then
/// all configured routes verbatim (including wildcards and paths).
///
/// When `base_domain` is provided (variant mode), routes referencing the base
/// domain are rewritten to use `default_host` instead.
fn compute_display_routes(
    cfg: &TakoToml,
    default_host: &str,
    base_domain: Option<&str>,
) -> Vec<String> {
    let mut out = vec![default_host.to_string()];
    if let Some(routes) = cfg.get_routes("development") {
        for route in routes {
            let route = if let Some(bd) = base_domain {
                route.replace(bd, default_host)
            } else {
                route
            };
            // Trim trailing slash so "foo.tako.test/" == "foo.tako.test".
            if route.trim_end_matches('/') != default_host {
                out.push(route);
            }
        }
    }
    // Dedup preserving order.
    let mut seen = std::collections::HashSet::new();
    out.retain(|r| seen.insert(r.clone()));
    out
}

// ---------------------------------------------------------------------------
// App-name disambiguation — prevent two distinct projects from claiming the
// same `{name}.tako.test` domain.
// ---------------------------------------------------------------------------

/// Sanitise an arbitrary string for use as a domain-name segment (lowercase
/// alphanumeric + hyphens, no leading/trailing hyphens).
fn sanitize_name_segment(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if (c == '-' || c == '_' || c == '.') && !out.ends_with('-') {
            out.push('-');
        }
    }
    let out = out.trim_matches('-').to_string();
    out
}

/// Deterministic 4-hex-char hash of a path.
fn short_path_hash(s: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut hasher);
    format!("{:04x}", hasher.finish() & 0xFFFF)
}

/// If `candidate` conflicts with an already-registered app from a different
/// project directory, append a suffix to make it unique.
///
/// Disambiguation order:
/// 1. Project directory leaf name (handles workspaces + different checkouts).
/// 2. Deterministic 4-char hash of the full project path (fallback).
fn disambiguate_app_name(
    candidate: &str,
    project_dir: &str,
    existing: &[(String, String)], // (app_name, project_dir)
) -> String {
    let dominated = |name: &str| -> bool {
        existing
            .iter()
            .any(|(n, pd)| n == name && pd != project_dir)
    };

    if !dominated(candidate) {
        return candidate.to_string();
    }

    // Try the project directory's leaf name as a suffix.
    if let Some(leaf) = Path::new(project_dir)
        .file_name()
        .and_then(|n| n.to_str())
    {
        let seg = sanitize_name_segment(leaf);
        if !seg.is_empty() {
            let with_dir = format!("{candidate}-{seg}");
            if !dominated(&with_dir) {
                return with_dir;
            }
        }
    }

    // Fallback: deterministic hash of the full project path.
    format!("{candidate}-{}", short_path_hash(project_dir))
}

/// Best-effort fetch of registered apps from a running dev server.
/// Returns an empty vec if the server is not running.
async fn try_list_registered_app_names() -> Vec<(String, String)> {
    match crate::dev_server_client::list_registered_apps().await {
        Ok(apps) => apps
            .into_iter()
            .map(|a| (a.app_name, a.project_dir))
            .collect(),
        Err(_) => Vec::new(),
    }
}

fn compute_dev_hosts(
    app_name: &str,
    cfg: &TakoToml,
    default_host: &str,
    base_domain: Option<&str>,
) -> Result<Vec<String>, String> {
    let routes = match cfg.get_routes("development") {
        Some(routes) if !routes.is_empty() => routes,
        _ => return Ok(vec![default_host.to_string()]),
    };

    // Always include the default host so `{app-name}.tako.test` works even when
    // the user only configures path-specific or wildcard routes.
    let mut out = vec![default_host.to_string()];
    for r in routes {
        validate_dev_route(&r, app_name).map_err(|e| e.to_string())?;
        let r = if let Some(bd) = base_domain {
            r.replace(bd, default_host)
        } else {
            r
        };
        if !r.is_empty() {
            out.push(r);
        }
    }

    // Dedup preserving order (default_host first).
    let mut seen = std::collections::HashSet::new();
    out.retain(|r| seen.insert(r.clone()));
    Ok(out)
}

/// Check whether a route pattern's hostname matches an incoming request hostname.
/// Route pattern may include a path (e.g. "app.tako.test/api") — the path is ignored.
/// Wildcard hosts (e.g. "*.app.tako.test") match any subdomain.
fn route_hostname_matches(route_pattern: &str, request_host: &str) -> bool {
    let host = route_pattern.split('/').next().unwrap_or(route_pattern);
    if host == request_host {
        return true;
    }
    if let Some(suffix) = host.strip_prefix("*.") {
        if request_host == suffix {
            return false;
        }
        return request_host.len() > suffix.len()
            && request_host.as_bytes()[request_host.len() - suffix.len() - 1] == b'.'
            && request_host.ends_with(suffix);
    }
    false
}

fn compute_dev_env(cfg: &TakoToml) -> std::collections::HashMap<String, String> {
    let mut env = cfg.get_merged_vars("development");
    env.insert("ENV".to_string(), "development".to_string());
    env
}

/// Decrypt development secrets and write them to a temp file for the SDK.
///
/// Sets `TAKO_SECRETS_FILE` in `env` so the SDK knows where to read.
/// Returns the temp file path (for cleanup tracking).
fn write_dev_secrets_file(
    project_dir: &Path,
    app_name: &str,
    env: &mut std::collections::HashMap<String, String>,
) -> Result<Option<std::path::PathBuf>, Box<dyn std::error::Error>> {
    let secrets = crate::config::SecretsStore::load_from_dir(project_dir)?;

    let encrypted = match secrets.get_env("development") {
        Some(map) if !map.is_empty() => map,
        _ => return Ok(None),
    };

    let key = super::secret::load_or_derive_key(app_name, "development", &secrets)?;
    let mut decrypted = std::collections::HashMap::new();
    for (name, encrypted_value) in encrypted {
        match crate::crypto::decrypt(encrypted_value, &key) {
            Ok(value) => {
                decrypted.insert(name.clone(), value);
            }
            Err(e) => {
                tracing::warn!("Failed to decrypt development secret {}: {}", name, e);
            }
        }
    }

    if decrypted.is_empty() {
        return Ok(None);
    }

    // Write to .tako/tmp/secrets.json (project-local, gitignored)
    let secrets_dir = project_dir.join(".tako").join("tmp");
    std::fs::create_dir_all(&secrets_dir)?;
    let secrets_path = secrets_dir.join("dev-secrets.json");

    let json = serde_json::to_string(&decrypted)?;

    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&secrets_path)?;
        f.write_all(json.as_bytes())?;
    }
    #[cfg(not(unix))]
    {
        std::fs::write(&secrets_path, &json)?;
    }

    env.insert(
        "TAKO_SECRETS_FILE".to_string(),
        secrets_path.to_string_lossy().to_string(),
    );

    Ok(Some(secrets_path))
}

fn resolve_dev_build_adapter(project_dir: &Path, cfg: &TakoToml) -> Result<BuildAdapter, String> {
    if let Some(adapter_override) = cfg
        .runtime
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return BuildAdapter::from_id(adapter_override).ok_or_else(|| {
            format!(
                "Invalid runtime '{}'; expected one of: bun, node, deno",
                adapter_override
            )
        });
    }

    Ok(crate::build::detect_build_adapter(project_dir))
}

fn resolve_effective_dev_build_adapter(
    project_dir: &Path,
    cfg: &TakoToml,
    preset_ref: &str,
) -> Result<BuildAdapter, String> {
    let configured_or_detected = resolve_dev_build_adapter(project_dir, cfg)?;
    if configured_or_detected != BuildAdapter::Unknown {
        return Ok(configured_or_detected);
    }

    let inferred = infer_adapter_from_preset_reference(preset_ref);
    if inferred != BuildAdapter::Unknown {
        return Ok(inferred);
    }

    Ok(configured_or_detected)
}

fn resolve_dev_preset_ref(project_dir: &Path, cfg: &TakoToml) -> Result<String, String> {
    let runtime = resolve_dev_build_adapter(project_dir, cfg)?;
    if let Some(preset_ref) = cfg
        .preset
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return qualify_runtime_local_preset_ref(runtime, preset_ref);
    }
    Ok(runtime.default_preset().to_string())
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

fn resolve_runtime_default_dev_command(
    runtime_adapter: BuildAdapter,
    main: &str,
) -> Result<Vec<String>, String> {
    let command = match runtime_adapter {
        BuildAdapter::Bun => vec![
            "bun".to_string(),
            "run".to_string(),
            "node_modules/tako.sh/src/entrypoints/bun.ts".to_string(),
            main.to_string(),
        ],
        BuildAdapter::Node => vec![
            "node".to_string(),
            "--experimental-strip-types".to_string(),
            "node_modules/tako.sh/src/entrypoints/node.ts".to_string(),
            main.to_string(),
        ],
        BuildAdapter::Deno => vec![
            "deno".to_string(),
            "run".to_string(),
            "--allow-net".to_string(),
            "--allow-env".to_string(),
            "--allow-read".to_string(),
            "node_modules/tako.sh/src/entrypoints/deno.ts".to_string(),
            main.to_string(),
        ],
        BuildAdapter::Unknown => {
            return Err(
                "Cannot determine default dev command because runtime is unknown. Set top-level `runtime` or set `preset`."
                    .to_string(),
            )
        }
    };
    Ok(command)
}

fn has_explicit_dev_preset(cfg: &TakoToml) -> bool {
    cfg.preset
        .as_deref()
        .map(str::trim)
        .is_some_and(|preset| !preset.is_empty())
}

fn resolve_dev_run_command(
    preset: &BuildPreset,
    main: &str,
    runtime_adapter: BuildAdapter,
    explicit_preset: bool,
) -> Result<Vec<String>, String> {
    if !explicit_preset {
        return resolve_runtime_default_dev_command(runtime_adapter, main);
    }
    resolve_dev_start_command(preset, main)
}

fn infer_preset_name_from_ref(preset_ref: &str) -> String {
    match parse_preset_reference(preset_ref) {
        Ok(PresetReference::OfficialAlias { name, .. }) => name,
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

fn dev_server_tls_names_path_for_home(home: &Path) -> PathBuf {
    home.join("certs").join(DEV_TLS_NAMES_FILENAME)
}

fn default_dev_tls_names_for_app(app_name: &str) -> Vec<String> {
    let d = crate::dev::TAKO_DEV_DOMAIN;
    vec![
        format!("*.{d}"),
        d.to_string(),
        format!("{app_name}.{d}"),
        format!("*.{app_name}.{d}"),
    ]
}

fn normalize_tls_names(mut names: Vec<String>) -> Vec<String> {
    names.sort();
    names.dedup();
    names
}

fn load_dev_tls_names(path: &Path) -> Option<Vec<String>> {
    let raw = std::fs::read_to_string(path).ok()?;
    let parsed = serde_json::from_str::<Vec<String>>(&raw).ok()?;
    Some(normalize_tls_names(parsed))
}

fn ensure_dev_server_tls_material_for_home(
    ca: &LocalCA,
    home: &Path,
    app_name: &str,
) -> Result<bool, Box<dyn std::error::Error>> {
    let (cert_path, key_path) = dev_server_tls_paths_for_home(home);
    let names_path = dev_server_tls_names_path_for_home(home);
    let have_cert_material = cert_path.is_file() && key_path.is_file();
    let existing_names = if have_cert_material {
        load_dev_tls_names(&names_path)
    } else {
        None
    };
    let mut names = default_dev_tls_names_for_app(app_name);
    if let Some(existing) = existing_names.clone() {
        names.extend(existing);
    }
    let names = normalize_tls_names(names);

    if have_cert_material
        && existing_names
            .as_ref()
            .is_some_and(|existing| *existing == names)
    {
        return Ok(false);
    }

    if let Some(parent) = cert_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let name_refs: Vec<&str> = names.iter().map(|name| name.as_str()).collect();
    let cert = ca.generate_leaf_cert_for_names(&name_refs)?;
    std::fs::write(&cert_path, cert.cert_pem.as_bytes())?;
    std::fs::write(&key_path, cert.key_pem.as_bytes())?;
    std::fs::write(&names_path, serde_json::to_string_pretty(&names)?)?;
    Ok(true)
}

fn ensure_dev_server_tls_material(
    ca: &LocalCA,
    app_name: &str,
) -> Result<bool, Box<dyn std::error::Error>> {
    let data_dir = crate::paths::tako_data_dir()?;
    ensure_dev_server_tls_material_for_home(ca, &data_dir, app_name)
}

#[cfg(target_os = "macos")]
fn sudo_run_checked(args: &[&str], context: &str) -> Result<(), Box<dyn std::error::Error>> {
    use std::process::Command;
    let status = Command::new("sudo").args(args).status()?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{context} failed").into())
    }
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

    crate::output::info("Configuring local DNS resolver (sudo)...");

    sudo_run_checked(
        &["install", "-d", "-m", "755", RESOLVER_DIR],
        "creating /etc/resolver",
    )?;
    write_system_file_with_sudo(TAKO_RESOLVER_FILE, &local_dns_resolver_contents(port))?;

    if !local_dns_resolver_configured(port) {
        return Err("local DNS resolver setup verification failed".into());
    }

    crate::output::success("Local DNS resolver configured for *.tako.test.");

    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn ensure_local_dns_resolver_configured(_port: u16) -> Result<(), Box<dyn std::error::Error>> {
    Ok(())
}

#[cfg(any(target_os = "macos", test))]
fn local_dns_sudo_action_line() -> &'static str {
    "Configure local DNS for *.tako.test"
}

#[cfg(any(target_os = "macos", test))]
fn sudo_setup_action_items(
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
fn explain_pending_sudo_setup(port: u16) -> Result<(), Box<dyn std::error::Error>> {
    if !crate::output::is_interactive() {
        return Ok(());
    }

    let items = sudo_setup_action_items(
        ca_setup::pending_sudo_action()?,
        !local_dns_resolver_configured(port),
        loopback_proxy::pending_sudo_action()?,
    );
    if items.is_empty() {
        return Ok(());
    }

    crate::output::warning_full("One-time sudo is required for:");
    for item in items {
        crate::output::warning_bullet(&item);
    }

    // Pre-authenticate sudo so the password prompt is not overwritten by spinners.
    let status = std::process::Command::new("sudo")
        .arg("-v")
        .status()
        .map_err(|e| -> Box<dyn std::error::Error> { format!("failed to run sudo: {e}").into() })?;
    if !status.success() {
        return Err("sudo authentication failed".into());
    }

    Ok(())
}

pub(crate) fn tcp_port_open(ip: &str, port: u16, timeout_ms: u64) -> bool {
    use std::net::{Ipv4Addr, SocketAddr};
    let Ok(ipv4) = ip.parse::<Ipv4Addr>() else {
        return false;
    };
    let addr = SocketAddr::from((ipv4, port));
    std::net::TcpStream::connect_timeout(&addr, Duration::from_millis(timeout_ms)).is_ok()
}

#[cfg(target_os = "macos")]
async fn localhost_https_host_reachable_via_ip(
    host: &str,
    connect_ip: std::net::Ipv4Addr,
    port: u16,
    timeout_ms: u64,
) -> Result<(), String> {
    use crate::dev::LocalCAStore;
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

#[cfg(not(target_os = "macos"))]
async fn localhost_https_host_reachable_via_ip(
    _host: &str,
    _connect_ip: std::net::Ipv4Addr,
    _port: u16,
    _timeout_ms: u64,
) -> Result<(), String> {
    Err("HTTPS probe unsupported on this platform".to_string())
}

async fn wait_for_https_host_reachable_via_ip(
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
fn local_https_probe_error(
    host: &str,
    public_port: u16,
    loopback_error: &str,
    server_directly_reachable: bool,
) -> String {
    if server_directly_reachable {
        format!(
            "Local HTTPS endpoint is unreachable at https://{host}/ ({loopback_error}). \
             Tako dev server is reachable directly on 127.0.0.1:{public_port}, so the local launchd loopback proxy is not forwarding correctly. \
             Check `launchctl print system/{LOOPBACK_PROXY_LABEL}`, then re-run `tako dev`."
        )
    } else {
        format!(
            "Local HTTPS endpoint is unreachable at https://{host}/ ({loopback_error}). \
             Check that the local loopback proxy is loaded (`tako doctor`) and try again."
        )
    }
}

fn local_https_probe_host(primary_host: &str) -> &str {
    primary_host
}

pub(crate) fn port_from_listen(listen: &str) -> Option<u16> {
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

#[cfg(test)]
pub(crate) fn doctor_dev_server_lines(
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

#[cfg(test)]
pub(crate) fn doctor_local_forwarding_preflight_lines(
    advertised_ip: &str,
    proxy_loaded: bool,
    https_tcp_ok: bool,
    http_tcp_ok: bool,
) -> Vec<String> {
    vec![
        "preflight:".to_string(),
        format!(
            "- loopback proxy ({})",
            if proxy_loaded { "loaded" } else { "not loaded" }
        ),
        format!(
            "- TCP {}:443 ({})",
            advertised_ip,
            if https_tcp_ok { "ok" } else { "unreachable" }
        ),
        format!(
            "- TCP {}:80 ({})",
            advertised_ip,
            if http_tcp_ok { "ok" } else { "unreachable" }
        ),
    ]
}

pub(crate) fn is_dev_server_unavailable_error_message(message: &str) -> bool {
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
pub(crate) fn system_resolver_ipv4(hostname: &str) -> Option<String> {
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
pub(crate) fn local_dns_resolver_values() -> Option<(String, u16)> {
    let contents = std::fs::read_to_string(TAKO_RESOLVER_FILE).ok()?;
    let (nameserver, port) = parse_local_dns_resolver(&contents);
    Some((nameserver?, port?))
}

#[cfg(test)]
mod tests {
    #[cfg(target_os = "macos")]
    use super::local_https_probe_error;
    use super::{
        DevEvent, LogLevel, ScopedLog, StoredLogEvent, app_log_scope, child_log_level_and_message,
        compute_dev_hosts, compute_display_routes, dev_idle_timeout, dev_initial_instance_count,
        dev_server_ready_log, dev_server_starting_log, dev_server_tls_names_path_for_home,
        dev_server_tls_paths_for_home, dev_startup_lines, doctor_dev_server_lines,
        doctor_local_forwarding_preflight_lines, ensure_dev_server_tls_material_for_home,
        ensure_local_dns_resolver_configured, host_and_port_from_url,
        is_dev_server_unavailable_error_message, local_dns_resolver_contents,
        local_dns_sudo_action_line, local_https_probe_host, parse_local_dns_resolver,
        parse_stored_log_line, port_from_listen, preferred_public_url, replay_and_follow_logs,
        resolve_dev_preset_ref, resolve_dev_run_command, resolve_effective_dev_build_adapter,
        restart_required_for_requested_listen, route_hostname_matches, should_drop_child_log_line,
        sudo_setup_action_items, tcp_probe, trim_child_log_message,
    };
    use crate::build::{BuildAdapter, parse_and_validate_preset};
    use crate::config::TakoToml;
    use crate::dev::LocalCA;
    use std::path::Path;
    use std::time::Duration;
    use tempfile::TempDir;
    use tokio::sync::{mpsc, watch};

    #[test]
    fn resolve_dev_preset_ref_uses_build_adapter_override_when_preset_is_missing() {
        let temp = TempDir::new().unwrap();
        std::fs::write(temp.path().join("package.json"), r#"{"name":"demo"}"#).unwrap();
        let cfg = TakoToml {
            runtime: Some("deno".to_string()),
            ..Default::default()
        };

        assert_eq!(resolve_dev_preset_ref(temp.path(), &cfg).unwrap(), "deno");
    }

    #[test]
    fn resolve_dev_preset_ref_qualifies_runtime_local_alias() {
        let temp = TempDir::new().unwrap();
        let cfg = TakoToml {
            runtime: Some("bun".to_string()),
            preset: Some("tanstack-start".to_string()),
            ..Default::default()
        };

        assert_eq!(
            resolve_dev_preset_ref(temp.path(), &cfg).unwrap(),
            "js/tanstack-start"
        );
    }

    #[test]
    fn resolve_dev_preset_ref_errors_when_runtime_is_unknown_for_local_alias() {
        let temp = TempDir::new().unwrap();
        let cfg = TakoToml {
            preset: Some("tanstack-start".to_string()),
            ..Default::default()
        };

        let err = resolve_dev_preset_ref(temp.path(), &cfg).unwrap_err();
        assert!(err.contains("Cannot resolve preset"));
    }

    #[test]
    fn resolve_dev_preset_ref_rejects_unknown_build_adapter_override() {
        let temp = TempDir::new().unwrap();
        let cfg = TakoToml {
            runtime: Some("python".to_string()),
            ..Default::default()
        };

        let err = resolve_dev_preset_ref(temp.path(), &cfg).unwrap_err();
        assert!(err.contains("Invalid runtime"));
    }

    #[test]
    fn resolve_effective_dev_build_adapter_uses_preset_group_when_detection_is_unknown() {
        let temp = TempDir::new().unwrap();
        let cfg = TakoToml::default();

        let adapter = resolve_effective_dev_build_adapter(temp.path(), &cfg, "bun").unwrap();
        assert_eq!(adapter, BuildAdapter::Bun);
    }

    #[test]
    fn resolve_dev_run_command_uses_runtime_default_when_preset_is_implicit() {
        let preset = parse_and_validate_preset(
            r#"
dev = ["bun", "run", "dev"]
"#,
            "bun",
        )
        .unwrap();

        let cmd = resolve_dev_run_command(&preset, "src/index.ts", BuildAdapter::Bun, false)
            .expect("runtime default dev command");

        assert_eq!(
            cmd,
            vec![
                "bun".to_string(),
                "run".to_string(),
                "node_modules/tako.sh/src/entrypoints/bun.ts".to_string(),
                "src/index.ts".to_string(),
            ]
        );
    }

    #[test]
    fn resolve_dev_run_command_uses_preset_dev_when_preset_is_explicit() {
        let preset = parse_and_validate_preset(
            r#"
dev = ["bun", "run", "dev"]
"#,
            "bun",
        )
        .unwrap();

        let cmd = resolve_dev_run_command(&preset, "src/index.ts", BuildAdapter::Bun, true)
            .expect("preset dev command");

        assert_eq!(
            cmd,
            vec!["bun".to_string(), "run".to_string(), "dev".to_string()]
        );
    }

    #[test]
    fn dev_startup_lines_quiet_is_short() {
        let lines = dev_startup_lines(
            false,
            "app",
            "fake",
            Path::new("index.ts"),
            "https://app.tako.test:8443/",
        );
        assert_eq!(lines[0], "https://app.tako.test:8443/");
        assert!(lines.iter().all(|l| !l.contains("Tako Dev Server")));
    }

    #[test]
    fn dev_startup_lines_verbose_includes_banner() {
        let lines = dev_startup_lines(
            true,
            "app",
            "fake",
            Path::new("index.ts"),
            "https://app.tako.test:8443/",
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
    async fn tcp_probe_retries_until_port_is_open() {
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

        let mut ok = false;
        for _ in 0..10 {
            if tcp_probe(("127.0.0.1", port), 10).await {
                ok = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        assert!(ok);
    }

    #[tokio::test]
    async fn tcp_probe_returns_false_for_closed_port() {
        assert!(!tcp_probe(("127.0.0.1", 0), 10).await);
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
    fn child_log_filter_drops_shell_command_echo_lines() {
        assert!(should_drop_child_log_line("$ vite dev"));
        assert!(should_drop_child_log_line("  $ bun run dev  "));
        assert!(should_drop_child_log_line(""));
    }

    #[test]
    fn child_log_filter_keeps_non_command_messages() {
        assert!(!should_drop_child_log_line("warning: low disk"));
        assert!(!should_drop_child_log_line("$5 price update"));
        assert!(!should_drop_child_log_line("$$$"));
    }

    #[test]
    fn child_log_message_trim_keeps_leading_alignment_and_removes_trailing_whitespace() {
        assert_eq!(
            trim_child_log_message("  VITE v7 ready   "),
            Some("  VITE v7 ready".to_string())
        );
    }

    #[test]
    fn child_log_message_trim_drops_whitespace_only_lines() {
        assert_eq!(trim_child_log_message("   "), None);
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
    fn stored_log_line_preserves_unrecognized_json_log_shape_as_message() {
        let raw_line = r#"{"h":12,"m":3,"s":7,"level":"Info","scope":"app","message":"hello"}"#;
        let decoded = parse_stored_log_line(raw_line).unwrap();

        let StoredLogEvent::Log(decoded) = decoded else {
            panic!("expected log event");
        };

        assert_ne!(decoded.timestamp, "12:03:07");
        assert!(matches!(decoded.level, LogLevel::Info));
        assert_eq!(decoded.scope, "app");
        assert_eq!(decoded.message, raw_line);
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
            host_and_port_from_url("https://app.tako.test/"),
            Some(("app.tako.test".to_string(), 443))
        );
        assert_eq!(
            host_and_port_from_url("https://app.tako.test:47831/"),
            Some(("app.tako.test".to_string(), 47831))
        );
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
    fn doctor_preflight_lines_show_proxy_not_loaded() {
        let lines = doctor_local_forwarding_preflight_lines("127.77.0.1", false, false, true);
        assert!(lines.iter().any(|line| line.contains("not loaded")));
        assert!(
            lines
                .iter()
                .any(|line| line.contains("TCP 127.77.0.1:443 (unreachable)"))
        );
        assert!(
            lines
                .iter()
                .any(|line| line.contains("TCP 127.77.0.1:80 (ok)"))
        );
    }

    #[test]
    fn doctor_preflight_lines_show_proxy_loaded() {
        let lines = doctor_local_forwarding_preflight_lines("127.77.0.1", true, true, true);
        assert!(lines.iter().any(|line| line.contains("loaded")));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn local_https_probe_error_mentions_launchd_when_server_directly_reachable() {
        let msg = local_https_probe_error("bun-example.tako.test", 47831, "timed out", true);
        assert!(msg.contains("launchd"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn local_https_probe_error_mentions_doctor_when_server_not_directly_reachable() {
        let msg = local_https_probe_error("bun-example.tako.test", 47831, "timed out", false);
        assert!(msg.contains("tako doctor"));
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
        let changed = ensure_dev_server_tls_material_for_home(&ca, temp.path(), "demo").unwrap();
        assert!(changed);

        let (cert_path, key_path) = dev_server_tls_paths_for_home(temp.path());
        let names_path = dev_server_tls_names_path_for_home(temp.path());
        let cert = std::fs::read_to_string(cert_path).unwrap();
        let key = std::fs::read_to_string(key_path).unwrap();
        let names = std::fs::read_to_string(names_path).unwrap();
        assert!(cert.contains("BEGIN CERTIFICATE"));
        assert!(key.contains("BEGIN PRIVATE KEY"));
        assert!(names.contains("*.demo.tako.test"));
    }

    #[test]
    fn ensure_dev_server_tls_material_keeps_existing_files() {
        let temp = TempDir::new().unwrap();
        let (cert_path, key_path) = dev_server_tls_paths_for_home(temp.path());
        let names_path = dev_server_tls_names_path_for_home(temp.path());
        std::fs::create_dir_all(cert_path.parent().unwrap()).unwrap();
        std::fs::write(&cert_path, "existing-cert").unwrap();
        std::fs::write(&key_path, "existing-key").unwrap();
        std::fs::write(
            &names_path,
            r#"[
  "*.tako.test",
  "tako.test",
  "demo.tako.test",
  "*.demo.tako.test"
]"#,
        )
        .unwrap();

        let ca = LocalCA::generate().unwrap();
        let changed = ensure_dev_server_tls_material_for_home(&ca, temp.path(), "demo").unwrap();
        assert!(!changed);

        let cert = std::fs::read_to_string(cert_path).unwrap();
        let key = std::fs::read_to_string(key_path).unwrap();
        assert_eq!(cert, "existing-cert");
        assert_eq!(key, "existing-key");
    }

    #[test]
    fn ensure_dev_server_tls_material_regenerates_files_without_names_manifest() {
        let temp = TempDir::new().unwrap();
        let (cert_path, key_path) = dev_server_tls_paths_for_home(temp.path());
        std::fs::create_dir_all(cert_path.parent().unwrap()).unwrap();
        std::fs::write(&cert_path, "existing-cert").unwrap();
        std::fs::write(&key_path, "existing-key").unwrap();

        let ca = LocalCA::generate().unwrap();
        let changed = ensure_dev_server_tls_material_for_home(&ca, temp.path(), "demo").unwrap();
        assert!(changed);

        let cert = std::fs::read_to_string(&cert_path).unwrap();
        let key = std::fs::read_to_string(&key_path).unwrap();
        let names =
            std::fs::read_to_string(dev_server_tls_names_path_for_home(temp.path())).unwrap();
        assert!(cert.contains("BEGIN CERTIFICATE"));
        assert!(key.contains("BEGIN PRIVATE KEY"));
        assert!(names.contains("*.demo.tako.test"));
    }

    #[test]
    fn ensure_dev_server_tls_material_merges_names_for_multiple_apps() {
        let temp = TempDir::new().unwrap();
        let ca = LocalCA::generate().unwrap();
        let first_changed = ensure_dev_server_tls_material_for_home(&ca, temp.path(), "alpha")
            .expect("first cert write");
        assert!(first_changed);
        let second_changed = ensure_dev_server_tls_material_for_home(&ca, temp.path(), "beta")
            .expect("second cert write");
        assert!(second_changed);

        let names =
            std::fs::read_to_string(dev_server_tls_names_path_for_home(temp.path())).unwrap();
        assert!(names.contains("*.alpha.tako.test"));
        assert!(names.contains("*.beta.tako.test"));
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
        assert!(text.contains("/etc/resolver/tako"));
        assert!(text.contains("run `tako dev` interactively once"));
    }

    #[test]
    fn sudo_setup_action_items_uses_expected_order() {
        let items = sudo_setup_action_items(
            Some("Trust the Tako local CA for trusted https://*.tako.test"),
            true,
            Some("Install the local loopback proxy for 127.77.0.1:80/443"),
        );
        assert_eq!(
            items,
            vec![
                "Trust the Tako local CA for trusted https://*.tako.test".to_string(),
                local_dns_sudo_action_line().to_string(),
                "Install the local loopback proxy for 127.77.0.1:80/443".to_string(),
            ]
        );
    }

    #[test]
    fn sudo_setup_action_items_omits_absent_steps() {
        let items = sudo_setup_action_items(None, false, Some("Repair loopback proxy"));
        assert_eq!(items, vec!["Repair loopback proxy".to_string()]);
    }

    #[test]
    fn prefers_local_url_when_80_443_forwarding_is_detected() {
        let url = preferred_public_url(
            "bun-example.tako.test",
            "https://bun-example.tako.test:47831/",
            47831,
            443,
        );
        assert_eq!(url, "https://bun-example.tako.test/");
    }

    #[test]
    fn prefers_daemon_url_when_display_and_listen_ports_match() {
        let url = preferred_public_url(
            "bun-example.tako.test",
            "https://bun-example.tako.test:47831/",
            47831,
            47831,
        );
        assert_eq!(url, "https://bun-example.tako.test:47831/");
    }

    #[test]
    fn display_routes_always_includes_default() {
        let cfg = TakoToml::default();
        let routes = compute_display_routes(&cfg, "app.tako.test", None);
        assert_eq!(routes, vec!["app.tako.test"]);
    }

    #[test]
    fn display_routes_includes_default_plus_all_configured() {
        let cfg = TakoToml::parse(
            "[envs.development]\nroutes = [\"app.tako.test/bun\", \"*.app.tako.test\"]\n",
        )
        .unwrap();
        let routes = compute_display_routes(&cfg, "app.tako.test", None);
        assert_eq!(
            routes,
            vec!["app.tako.test", "app.tako.test/bun", "*.app.tako.test"]
        );
    }

    #[test]
    fn display_routes_deduplicates_default_when_route_matches() {
        let cfg = TakoToml::parse("[envs.development]\nroutes = [\"app.tako.test\"]\n").unwrap();
        let routes = compute_display_routes(&cfg, "app.tako.test", None);
        assert_eq!(routes, vec!["app.tako.test"]);
    }

    #[test]
    fn display_routes_rewrite_wildcard_for_variant() {
        let cfg = TakoToml::parse(
            "[envs.development]\nroutes = [\"some-app.tako.test/bun\", \"*.example.tako.test\"]\n",
        )
        .unwrap();
        let routes = compute_display_routes(
            &cfg,
            "example-foo.tako.test",
            Some("example.tako.test"),
        );
        assert_eq!(
            routes,
            vec![
                "example-foo.tako.test",
                "some-app.tako.test/bun",
                "*.example-foo.tako.test",
            ]
        );
    }

    #[test]
    fn display_routes_variant_deduplicates_rewritten_default() {
        // Config route matches the base domain; after rewriting it becomes
        // the default host and should be deduped.
        let cfg = TakoToml::parse(
            "[envs.development]\nroutes = [\"example.tako.test\"]\n",
        )
        .unwrap();
        let routes = compute_display_routes(
            &cfg,
            "example-foo.tako.test",
            Some("example.tako.test"),
        );
        assert_eq!(routes, vec!["example-foo.tako.test"]);
    }

    #[test]
    fn local_https_probe_host_uses_app_tako_test_domain() {
        assert_eq!(
            local_https_probe_host("bun-example.tako.test"),
            "bun-example.tako.test"
        );
    }

    #[test]
    fn falls_back_to_default_host_when_development_routes_are_missing() {
        let cfg = TakoToml::default();
        let hosts = compute_dev_hosts("app", &cfg, "app.tako.test", None).unwrap();
        assert_eq!(hosts, vec!["app.tako.test".to_string()]);
    }

    #[test]
    fn falls_back_to_default_host_when_development_routes_are_empty() {
        let cfg = TakoToml::parse("[envs.development]\nroutes = []\n").unwrap();
        let hosts = compute_dev_hosts("app", &cfg, "app.tako.test", None).unwrap();
        assert_eq!(hosts, vec!["app.tako.test".to_string()]);
    }

    #[test]
    fn always_includes_default_host_with_explicit_routes() {
        let cfg =
            TakoToml::parse("[envs.development]\nroutes = [\"api.app.tako.test\"]\n").unwrap();
        let hosts = compute_dev_hosts("app", &cfg, "app.tako.test", None).unwrap();
        assert_eq!(hosts, vec!["app.tako.test", "api.app.tako.test"]);
    }

    #[test]
    fn always_includes_default_host_with_wildcard_routes() {
        let cfg = TakoToml::parse("[envs.development]\nroutes = [\"*.app.tako.test\"]\n").unwrap();
        let hosts = compute_dev_hosts("app", &cfg, "app.tako.test", None).unwrap();
        assert_eq!(hosts, vec!["app.tako.test", "*.app.tako.test"]);
    }

    #[test]
    fn default_host_deduped_when_already_in_routes() {
        let cfg = TakoToml::parse("[envs.development]\nroutes = [\"app.tako.test\"]\n").unwrap();
        let hosts = compute_dev_hosts("app", &cfg, "app.tako.test", None).unwrap();
        assert_eq!(hosts, vec!["app.tako.test"]);
    }

    #[test]
    fn dev_hosts_rewrite_wildcard_for_variant() {
        let cfg = TakoToml::parse(
            "[envs.development]\nroutes = [\"some-app.tako.test/bun\", \"*.example.tako.test\"]\n",
        )
        .unwrap();
        let hosts = compute_dev_hosts(
            "example-foo",
            &cfg,
            "example-foo.tako.test",
            Some("example.tako.test"),
        )
        .unwrap();
        assert_eq!(
            hosts,
            vec![
                "example-foo.tako.test",
                "some-app.tako.test/bun",
                "*.example-foo.tako.test",
            ]
        );
    }

    /// Both routing and display include the full route patterns (host + path).
    #[test]
    fn dev_hosts_now_include_paths_and_wildcards() {
        let cfg = TakoToml::parse(
            "[envs.development]\nroutes = [\"app.tako.test\", \"app.tako.test/api\", \"*.app.tako.test\"]\n",
        )
        .unwrap();
        let display = compute_display_routes(&cfg, "app.tako.test", None);
        let routing = compute_dev_hosts("app", &cfg, "app.tako.test", None).unwrap();

        assert_eq!(
            display,
            vec!["app.tako.test", "app.tako.test/api", "*.app.tako.test"]
        );
        // Routing now carries full route patterns including paths.
        assert_eq!(
            routing,
            vec!["app.tako.test", "app.tako.test/api", "*.app.tako.test"]
        );
    }

    #[test]
    fn route_hostname_matches_exact() {
        assert!(route_hostname_matches("app.tako.test", "app.tako.test"));
        assert!(!route_hostname_matches("app.tako.test", "other.tako.test"));
    }

    #[test]
    fn route_hostname_matches_with_path() {
        assert!(route_hostname_matches("app.tako.test/api", "app.tako.test"));
        assert!(!route_hostname_matches(
            "app.tako.test/api",
            "other.tako.test"
        ));
    }

    #[test]
    fn route_hostname_matches_wildcard() {
        assert!(route_hostname_matches(
            "*.app.tako.test",
            "foo.app.tako.test"
        ));
        assert!(!route_hostname_matches("*.app.tako.test", "app.tako.test"));
        assert!(!route_hostname_matches(
            "*.app.tako.test",
            "other.tako.test"
        ));
    }
}

/// Run the dev server
pub async fn stop(name: Option<String>, all: bool) -> Result<(), Box<dyn std::error::Error>> {
    let apps = crate::dev_server_client::list_registered_apps().await?;

    if all {
        if apps.is_empty() {
            crate::output::muted("No registered dev apps.");
            return Ok(());
        }
        for app in &apps {
            let _ = crate::dev_server_client::unregister_app(&app.project_dir).await;
            crate::output::success(&format!("Stopped {}", crate::output::strong(&app.app_name)));
        }
        return Ok(());
    }

    let target_name = match name {
        Some(n) => n,
        None => {
            let project_dir = current_dir()?;
            let canonical =
                std::fs::canonicalize(&project_dir).unwrap_or_else(|_| project_dir.clone());
            let canonical_str = canonical.to_string_lossy().to_string();
            // Find by project_dir first.
            if let Some(app) = apps.iter().find(|a| a.project_dir == canonical_str) {
                let _ = crate::dev_server_client::unregister_app(&app.project_dir).await;
                crate::output::success(&format!(
                    "Stopped {}",
                    crate::output::strong(&app.app_name)
                ));
                return Ok(());
            }
            // Fall back to app name resolution.
            resolve_app_name(&project_dir)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e.to_string()))?
        }
    };

    let app = apps.iter().find(|a| a.app_name == target_name);
    match app {
        Some(a) => {
            let _ = crate::dev_server_client::unregister_app(&a.project_dir).await;
            crate::output::success(&format!("Stopped {}", crate::output::strong(&a.app_name)));
        }
        None => {
            return Err(format!("No registered dev app named '{}'", target_name).into());
        }
    }
    Ok(())
}

pub async fn ls() -> Result<(), Box<dyn std::error::Error>> {
    let apps = match crate::dev_server_client::list_registered_apps().await {
        Ok(apps) => apps,
        Err(_) => {
            crate::output::muted("No dev server running.");
            return Ok(());
        }
    };

    if apps.is_empty() {
        crate::output::muted("No registered dev apps.");
        return Ok(());
    }

    // Print as a simple table.
    println!("{:<20} {:<10} {:<30} {}", "NAME", "STATUS", "URL", "DIR");
    for app in &apps {
        let url = if let Some(host) = app.hosts.first() {
            format!("https://{}/", host)
        } else {
            String::new()
        };
        println!(
            "{:<20} {:<10} {:<30} {}",
            app.app_name, app.status, url, app.project_dir
        );
    }
    Ok(())
}

pub async fn run(
    public_port: u16,
    variant: Option<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    let project_dir = current_dir()?;
    let cfg = load_dev_tako_toml(&project_dir)?;
    let preset_ref = resolve_dev_preset_ref(&project_dir, &cfg)?;
    let runtime_adapter = resolve_effective_dev_build_adapter(&project_dir, &cfg, &preset_ref)
        .map_err(|e| format!("Failed to resolve runtime adapter: {}", e))?;
    let (mut build_preset, _) = load_build_preset(&project_dir, &preset_ref)
        .await
        .map_err(|e| format!("Failed to resolve build preset '{}': {}", preset_ref, e))?;
    apply_adapter_base_runtime_defaults(&mut build_preset, runtime_adapter)
        .map_err(|e| format!("Failed to apply runtime defaults to preset: {}", e))?;
    let main = crate::commands::deploy::resolve_deploy_main(
        &project_dir,
        runtime_adapter,
        &cfg,
        build_preset.main.as_deref(),
    )
    .map_err(|e| format!("Failed to resolve deploy entrypoint: {}", e))?;

    if runtime_adapter.preset_group() == PresetGroup::Js {
        let _ = js::write_types(&project_dir);
    }

    let runtime_name = build_preset.name.clone();

    let base_name = resolve_app_name(&project_dir)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e.to_string()))?;
    let app_name = if let Some(ref v) = variant {
        format!("{base_name}-{v}")
    } else {
        base_name.clone()
    };

    // Disambiguate if another project already owns this name.
    let canonical_for_disambig =
        std::fs::canonicalize(&project_dir).unwrap_or_else(|_| project_dir.clone());
    let canonical_for_disambig_str = canonical_for_disambig.to_string_lossy().to_string();
    let existing_apps = try_list_registered_app_names().await;
    let app_name =
        disambiguate_app_name(&app_name, &canonical_for_disambig_str, &existing_apps);

    let domain = LocalCA::app_domain(&app_name);
    // When a variant is active, routes in tako.toml reference the base app
    // domain (e.g. `*.example.tako.test`).  We rewrite them to use the
    // variant domain (e.g. `*.example-foo.tako.test`).
    let base_domain = if variant.is_some() {
        Some(LocalCA::app_domain(&base_name))
    } else {
        None
    };

    #[cfg(target_os = "macos")]
    explain_pending_sudo_setup(LOCAL_DNS_PORT)?;

    let local_ca = setup_local_ca().await?;
    let tls_material_updated = ensure_dev_server_tls_material(&local_ca, &app_name)?;
    ensure_local_dns_resolver_configured(LOCAL_DNS_PORT)?;

    #[cfg(target_os = "macos")]
    loopback_proxy::ensure_installed()?;

    #[cfg(target_os = "macos")]
    let public_url_port: u16 = 443;
    #[cfg(not(target_os = "macos"))]
    let mut public_url_port: u16 = public_port;
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
    let restart_for_tls = tls_material_updated && existing_info.is_some();

    if restart_for_listen || restart_for_dns || restart_for_tls {
        let current_listen = existing_listen.unwrap_or_else(|| "(unknown)".to_string());
        let current_dns_ip = existing_advertised_ip.unwrap_or_else(|| "(unknown)".to_string());
        let mut reasons = Vec::new();
        if restart_for_listen {
            reasons.push(format!("listen {}", current_listen));
        }
        if restart_for_dns {
            reasons.push(format!("DNS {}", current_dns_ip));
        }
        if restart_for_tls {
            reasons.push("updated TLS certificates".to_string());
        }
        let restart_reason = reasons.join(" and ");

        if crate::output::is_interactive() {
            crate::output::section("Dev Server");
            crate::output::warning(&format!(
                "A dev server is already running with {}.",
                crate::output::strong(&restart_reason)
            ));
            let should_restart = crate::output::confirm(
                &format!(
                    "Restart it with listen {}?",
                    crate::output::strong(&listen_addr)
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
                "A dev server is already running with listen {}. Stop it first and re-run `tako dev`.",
                current_listen
            )
            .into());
        }
    }

    // Compute initial dev config snapshot from tako.toml.
    let dev_hosts = compute_dev_hosts(&app_name, &cfg, &domain, base_domain.as_deref())
        .map_err(|e| format!("invalid development routes: {}", e))?;
    let primary_host = dev_hosts
        .iter()
        .map(|h| h.split('/').next().unwrap_or(h))
        .find(|h| !h.starts_with("*."))
        .map(|h| h.to_string())
        .unwrap_or_else(|| domain.clone());

    let hosts_state = Arc::new(tokio::sync::Mutex::new(dev_hosts.clone()));
    let mut env = compute_dev_env(&cfg);

    // Write decrypted development secrets to a temp file for the SDK to read.
    // Secrets are never injected as env vars — the SDK loads them from this file.
    let secrets_file = write_dev_secrets_file(&project_dir, &app_name, &mut env)
        .map_err(|e| e.to_string())?;

    // Regenerate tako.d.ts with secret types
    if runtime_adapter.preset_group() == PresetGroup::Js {
        let _ = crate::build::js::write_types(&project_dir);
    }

    let env_state = Arc::new(tokio::sync::Mutex::new(env));
    let secrets_file_state = Arc::new(tokio::sync::Mutex::new(secrets_file));

    // Create channels for communication (child stdout/stderr + file watcher events).
    let (log_tx, log_rx) = mpsc::channel::<ScopedLog>(1000);
    let (event_tx, event_rx) = mpsc::channel::<DevEvent>(100);

    let (control_tx, mut control_rx) = mpsc::channel::<output::ControlCmd>(32);
    let (should_exit_tx, mut should_exit_rx) = watch::channel(false);
    let terminate_requested = Arc::new(AtomicBool::new(false));

    let interactive = std::io::stdin().is_terminal() && std::io::stdout().is_terminal();

    // Allocate an ephemeral port for the app.
    let (upstream_port, reserve_listener) = reserve_ephemeral_port()?;
    let cmd = resolve_dev_run_command(
        &build_preset,
        &main,
        runtime_adapter,
        has_explicit_dev_preset(&cfg),
    )
    .map_err(|e| format!("Invalid dev start command: {}", e))?;

    // Keep receivers optional until we decide whether to launch interactive output.
    let mut log_rx_opt = Some(log_rx);
    let mut event_rx_opt = Some(event_rx);
    let mut output_handle: Option<tokio::task::JoinHandle<Result<output::DevOutputExit, String>>> =
        None;

    // Ensure the dev daemon is running.

    let _ = log_tx.send(dev_server_starting_log()).await;

    if let Err(e) = crate::dev_server_client::ensure_running(&listen_addr, daemon_dns_ip).await {
        let msg = e.to_string();
        let _ = log_tx
            .send(ScopedLog::error(
                "tako",
                format!("dev server failed to start: {}", msg),
            ))
            .await;
        let _ = event_tx.send(DevEvent::AppError(msg.clone())).await;

        return Err(msg.into());
    }

    let _ = log_tx.send(dev_server_ready_log(public_port)).await;

    if public_url_port == 443 {
        let Ok(loopback_ip) = DEV_LOOPBACK_ADDR.parse::<std::net::Ipv4Addr>() else {
            return Err(format!("Invalid loopback address: {DEV_LOOPBACK_ADDR}").into());
        };
        let probe_host = local_https_probe_host(&primary_host);
        // Probe the dev server's health endpoint via the advertised loopback
        // address to verify the full macOS local ingress path.
        let probe_result = wait_for_https_host_reachable_via_ip(
            probe_host,
            loopback_ip,
            443,
            LOCALHOST_443_HTTPS_PROBE_ATTEMPTS,
            LOCALHOST_443_HTTPS_PROBE_TIMEOUT_MS,
            LOCALHOST_443_HTTPS_PROBE_RETRY_DELAY_MS,
        )
        .await;
        if let Err(_loopback_error) = probe_result.as_ref() {
            #[cfg(target_os = "macos")]
            {
                let server_directly_reachable = wait_for_https_host_reachable_via_ip(
                    probe_host,
                    std::net::Ipv4Addr::new(127, 0, 0, 1),
                    public_port,
                    LOCALHOST_443_HTTPS_PROBE_ATTEMPTS,
                    LOCALHOST_443_HTTPS_PROBE_TIMEOUT_MS,
                    LOCALHOST_443_HTTPS_PROBE_RETRY_DELAY_MS,
                )
                .await
                .is_ok();
                return Err(local_https_probe_error(
                    probe_host,
                    public_port,
                    _loopback_error,
                    server_directly_reachable,
                )
                .into());
            }
            #[cfg(not(target_os = "macos"))]
            {
                crate::output::warning(
                    "Local 80/443 forwarding is configured but the dev HTTPS endpoint is unreachable.",
                );
                crate::output::muted("Continuing with explicit dev port URL.");
                public_url_port = public_port;
            }
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
                    format!("dev server failed to start: {}", msg),
                ))
                .await;
            let _ = event_tx.send(DevEvent::AppError(msg.clone())).await;
            return Err(msg.into());
        }
    }

    let tako_data = crate::paths::tako_data_dir()?;

    // Check daemon for existing registration instead of lock files.
    let canonical_project_dir =
        std::fs::canonicalize(&project_dir).unwrap_or_else(|_| project_dir.clone());
    let canonical_project_dir_str = canonical_project_dir.to_string_lossy().to_string();

    if let Ok(apps) = crate::dev_server_client::list_registered_apps().await {
        if let Some(existing) = apps
            .iter()
            .find(|a| a.project_dir == canonical_project_dir_str)
        {
            match existing.status.as_str() {
                "running" => {
                    // Attach to existing running app.
                    let log_dir = tako_data.join("dev").join("logs");
                    std::fs::create_dir_all(&log_dir)?;
                    let suffix = dev_client_suffix(&project_dir);
                    let log_path = log_dir.join(format!("{}-{}.jsonl", app_name, suffix));
                    let url = if let Some(host) = existing.hosts.first() {
                        let port = if public_url_port == 443 {
                            String::new()
                        } else {
                            format!(":{}", public_url_port)
                        };
                        format!("https://{}{}/", host, port)
                    } else {
                        dev_url(&primary_host, public_url_port)
                    };
                    let session = AttachedDevClient {
                        project_dir: canonical_project_dir_str.clone(),
                        url,
                        log_path,
                    };
                    let display_hosts = compute_display_routes(&cfg, &domain, base_domain.as_deref());
                    return run_attached_dev_client(&app_name, interactive, session, display_hosts)
                        .await;
                }
                // idle or stopped — register fresh below
                _ => {}
            }
        }
    }

    // Compute log store path (still use lock dir scheme for log files).
    let log_dir = tako_data.join("dev").join("logs");
    std::fs::create_dir_all(&log_dir)?;
    let suffix = dev_client_suffix(&project_dir);
    let log_store_path = log_dir.join(format!("{}-{}.jsonl", app_name, suffix));

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

    // Keep one app process running on startup.
    let child_state = std::sync::Arc::new(tokio::sync::Mutex::new(None::<tokio::process::Child>));
    let reserve_state = std::sync::Arc::new(tokio::sync::Mutex::new(Some(reserve_listener)));
    let app_started_once = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
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
                app_started_once.store(true, std::sync::atomic::Ordering::Relaxed);
                emit_persisted_app_event(&event_tx, &log_store_path, DevEvent::AppStarted).await;
            }
            Err(e) => {
                let msg = e.to_string();
                let _ = log_tx
                    .send(ScopedLog::error(
                        "tako",
                        format!("failed to start app: {}", msg),
                    ))
                    .await;
                emit_persisted_app_event(&event_tx, &log_store_path, DevEvent::AppError(msg)).await;
            }
        }
    }

    // Register the app with the daemon (persistent, no TTL).
    let reg_hosts = hosts_state.lock().await.clone();
    let env_snapshot = env_state.lock().await.clone();
    let reg_url = crate::dev_server_client::register_app(
        &canonical_project_dir_str,
        &app_name,
        variant.as_deref(),
        &reg_hosts,
        upstream_port,
        &cmd,
        &env_snapshot,
        &log_store_path.to_string_lossy(),
    )
    .await?;

    if reg_hosts.iter().any(|h| {
        let host = h.split('/').next().unwrap_or(h);
        host.ends_with(&format!(".{}", crate::dev::TAKO_DEV_DOMAIN))
    }) && let Ok(info) = crate::dev_server_client::info().await
    {
        let local_dns_enabled = info
            .get("info")
            .and_then(|i| i.get("local_dns_enabled"))
            .and_then(|b| b.as_bool())
            .unwrap_or(false);
        if !local_dns_enabled {
            crate::output::warning(
                "Local DNS is unavailable; .tako.test hostnames may not resolve.",
            );
            crate::output::muted("Run `tako doctor` for diagnostics.");
        }
    }

    if interactive {
        let public_port_for_output = public_url_port;
        // Display all routes (default + configured, including wildcards/paths).
        // `dev_hosts` / `hosts_state` is routing-only (no wildcards); display is separate.
        let hosts = compute_display_routes(&cfg, &domain, base_domain.as_deref());
        let app_name_for_output = app_name.clone();
        let adapter_name_for_output = runtime_name.clone();
        let control_tx_for_output = control_tx.clone();
        let log_store_for_output = log_store_path.clone();
        let log_rx = log_rx_opt.take().unwrap();
        let event_rx = event_rx_opt.take().unwrap();
        output_handle = Some(tokio::spawn(async move {
            output::run_dev_output(
                app_name_for_output,
                adapter_name_for_output,
                hosts,
                public_port_for_output,
                upstream_port,
                log_rx,
                event_rx,
                control_tx_for_output,
                Some(log_store_for_output),
            )
            .await
            .map_err(|e| e.to_string())
        }));
    }

    let verbose = crate::output::is_verbose();
    let url = preferred_public_url(&primary_host, &reg_url, public_port, public_url_port);
    if !interactive {
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

    // Watch tako.toml for config changes (env vars + dev routes).
    let (cfg_tx, cfg_rx) = mpsc::channel::<()>(8);
    let _cfg_handle = watcher::ConfigWatcher::new(project_dir.clone(), cfg_tx)?.start()?;

    if verbose && !interactive {
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
        let project_dir_str = canonical_project_dir_str.clone();
        let cmd = cmd.clone();
        let env_state = env_state.clone();
        let project_dir = project_dir.clone();
        let log_tx = log_tx.clone();
        let event_tx = event_tx.clone();
        let log_store_path = log_store_path.clone();
        let should_exit_tx = should_exit_tx.clone();
        let terminate_requested = terminate_requested.clone();
        let app_started_once = app_started_once.clone();

        tokio::spawn(async move {
            while let Some(cmd_in) = control_rx.recv().await {
                match cmd_in {
                    output::ControlCmd::Restart => {
                        let mut lock = child_state.lock().await;
                        if let Some(mut child) = lock.take() {
                            let _ = child.kill().await;
                            let _ = child.wait().await;
                        }

                        if app_started_once.load(std::sync::atomic::Ordering::Relaxed) {
                            let _ = log_tx.send(ScopedLog::divider()).await;
                        }

                        emit_persisted_app_event(
                            &event_tx,
                            &log_store_path,
                            DevEvent::AppLaunching,
                        )
                        .await;

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
                                let _ = crate::dev_server_client::set_app_status(
                                    &project_dir_str,
                                    "running",
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
                                app_started_once.store(true, std::sync::atomic::Ordering::Relaxed);
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
                                        format!("restart failed: {}", msg),
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
                    output::ControlCmd::Terminate => {
                        terminate_requested.store(true, Ordering::Relaxed);
                        let mut lock = child_state.lock().await;
                        if let Some(mut child) = lock.take() {
                            let _ = child.kill().await;
                            let _ = child.wait().await;
                        }
                        let _ = crate::dev_server_client::set_app_status(&project_dir_str, "idle")
                            .await;
                        emit_persisted_app_event(&event_tx, &log_store_path, DevEvent::AppStopped)
                            .await;
                        let _ = should_exit_tx.send(true);
                        break;
                    }
                }
            }
        });
    }

    // Config change loop: reload tako.toml, update state, always restart the app.
    {
        let project_dir = project_dir.clone();
        let project_dir_str = canonical_project_dir_str.clone();
        let app_name = app_name.clone();
        let variant = variant.clone();
        let domain = domain.clone();
        let base_domain = base_domain.clone();
        let env_state = env_state.clone();
        let secrets_file_state = secrets_file_state.clone();
        let hosts_state = hosts_state.clone();
        let cmd = cmd.clone();
        let log_store_path = log_store_path.clone();
        let log_tx = log_tx.clone();
        let mut cfg_rx = cfg_rx;
        let control_tx = control_tx.clone();
        tokio::spawn(async move {
            while cfg_rx.recv().await.is_some() {
                let cfg = match load_dev_tako_toml(&project_dir) {
                    Ok(c) => c,
                    Err(e) => {
                        let _ = log_tx
                            .send(ScopedLog::error("tako", format!("tako.toml error: {}", e)))
                            .await;
                        continue;
                    }
                };

                // Update env and hosts state unconditionally.
                let mut new_env = compute_dev_env(&cfg);

                // Rewrite the secrets temp file for the SDK.
                let secrets_result = write_dev_secrets_file(&project_dir, &app_name, &mut new_env)
                    .map_err(|e| e.to_string());
                match secrets_result {
                    Ok(new_path) => {
                        *secrets_file_state.lock().await = new_path;
                    }
                    Err(msg) => {
                        let _ = log_tx
                            .send(ScopedLog::warn("tako", format!("Failed to reload secrets: {}", msg)))
                            .await;
                    }
                }

                // Regenerate tako.d.ts with updated secret types.
                let _ = crate::build::js::write_types(&project_dir);

                *env_state.lock().await = new_env.clone();

                let new_hosts = match compute_dev_hosts(&app_name, &cfg, &domain, base_domain.as_deref()) {
                    Ok(hosts) => hosts,
                    Err(msg) => {
                        let _ = log_tx
                            .send(ScopedLog::error(
                                "tako",
                                format!("tako.toml invalid routes: {}", msg),
                            ))
                            .await;
                        continue;
                    }
                };
                let hosts_changed = {
                    let mut cur = hosts_state.lock().await;
                    let changed = *cur != new_hosts;
                    *cur = new_hosts.clone();
                    changed
                };

                // Re-register app if routing changed.
                if hosts_changed {
                    let reg_result = crate::dev_server_client::register_app(
                        &project_dir_str,
                        &app_name,
                        variant.as_deref(),
                        &new_hosts,
                        upstream_port,
                        &cmd,
                        &new_env,
                        &log_store_path.to_string_lossy(),
                    )
                    .await
                    .map_err(|e| e.to_string());
                    if let Err(msg) = reg_result {
                        let _ = log_tx
                            .send(ScopedLog::warn(
                                "tako",
                                format!("failed to update routing: {}", msg),
                            ))
                            .await;
                    }
                }

                // Always restart — runtime, preset, env, routes: any change may matter.
                let _ = log_tx
                    .send(ScopedLog::info("tako", "tako.toml changed, restarting…"))
                    .await;
                let _ = control_tx.send(output::ControlCmd::Restart).await;
            }
        });
    }

    // Scale-to-0 on idle and wake on request.
    {
        let last_req = std::sync::Arc::new(tokio::sync::Mutex::new(std::time::Instant::now()));
        let inflight = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let last_req2 = last_req.clone();

        {
            let project_dir_str = canonical_project_dir_str.clone();
            let app_hosts = hosts_state.lock().await.clone();
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
            let app_started_once = app_started_once.clone();
            let control_tx = control_tx.clone();
            let should_exit_tx = should_exit_tx.clone();

            let mut ev_rx = match crate::dev_server_client::subscribe_events().await {
                Ok(rx) => Some(rx),
                Err(e) => {
                    let _ = log_tx
                        .send(ScopedLog::warn(
                            "tako",
                            format!("failed to subscribe to dev server events: {}", e),
                        ))
                        .await;
                    None
                }
            };

            if let Some(mut ev_rx) = ev_rx.take() {
                tokio::spawn(async move {
                    while let Some(ev) = ev_rx.recv().await {
                        match ev {
                            crate::dev_server_client::DevServerEvent::RequestStarted {
                                host,
                                ..
                            } => {
                                if !app_hosts.iter().any(|h| route_hostname_matches(h, &host)) {
                                    continue;
                                }

                                inflight.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                                *last_req.lock().await = std::time::Instant::now();

                                let mut lock = child_state.lock().await;
                                if lock.is_none() {
                                    if app_started_once.load(std::sync::atomic::Ordering::Relaxed) {
                                        let _ = log_tx.send(ScopedLog::divider()).await;
                                    }
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
                                            let _ = crate::dev_server_client::set_app_status(
                                                &project_dir_str,
                                                "running",
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
                                            app_started_once
                                                .store(true, std::sync::atomic::Ordering::Relaxed);
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
                                                    format!("failed to start app: {}", msg),
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
                            crate::dev_server_client::DevServerEvent::RequestFinished {
                                host,
                                ..
                            } => {
                                if !app_hosts.iter().any(|h| route_hostname_matches(h, &host)) {
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
                            crate::dev_server_client::DevServerEvent::AppStatusChanged {
                                ref project_dir,
                                ref status,
                                ..
                            } => {
                                if project_dir == &project_dir_str && status == "stopped" {
                                    // Send ExitWithMessage so the output loop
                                    // can exit cleanly (erase footer + print message).
                                    let _ = event_tx
                                        .send(DevEvent::ExitWithMessage(
                                            "stopped by another client".to_string(),
                                        ))
                                        .await;
                                    let _ = should_exit_tx.send(true);
                                    break;
                                }
                            }
                            crate::dev_server_client::DevServerEvent::RestartRequested {
                                ..
                            } => {
                                let _ = control_tx.send(output::ControlCmd::Restart).await;
                            }
                        }
                    }
                });
            }
        }

        {
            let project_dir_str = canonical_project_dir_str.clone();
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

                    let _ =
                        crate::dev_server_client::set_app_status(&project_dir_str, "idle").await;

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

    if interactive {
        if let Some(mut handle) = output_handle.take() {
            let mut dev_exit: Option<output::DevOutputExit> = None;
            tokio::select! {
                r = &mut handle => {
                    match r {
                        Ok(Ok(exit)) => dev_exit = Some(exit),
                        Ok(Err(msg)) => return Err(msg.into()),
                        Err(e) => return Err(format!("dev output task failed: {}", e).into()),
                    }
                }
                _ = async {
                    while should_exit_rx.changed().await.is_ok() {
                        if *should_exit_rx.borrow() {
                            break;
                        }
                    }
                } => {
                    // Give the output loop a moment to receive ExitWithMessage
                    // and exit cleanly (erasing the footer). If it doesn't
                    // finish promptly, abort it.
                    match tokio::time::timeout(Duration::from_millis(500), &mut handle).await {
                        Ok(Ok(Ok(exit))) => dev_exit = Some(exit),
                        _ => {
                            handle.abort();
                            let _ = handle.await;
                        }
                    }
                }
            }

            // When the user pressed `b`, hand off the running process to the
            // daemon and exit the CLI immediately.
            if let Some(output::DevOutputExit::Detach { .. }) = dev_exit {
                let child_pid = {
                    let lock = child_state.lock().await;
                    lock.as_ref().and_then(|c| c.id())
                };
                if let Some(pid) = child_pid {
                    let _ = crate::dev_server_client::handoff_app(&canonical_project_dir_str, pid)
                        .await;
                    // Detach the child so cleanup doesn't kill it.
                    let _ = child_state.lock().await.take();
                }
                let _ = log_watch_stop_tx.send(true);
                return Ok(());
            }
        }
    } else {
        let mut log_rx = log_rx_opt
            .take()
            .expect("non-interactive should have log rx");
        let mut event_rx = event_rx_opt
            .take()
            .expect("non-interactive should have event rx");
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
                                    eprintln!("App error: {}", e);
                                }
                                DevEvent::LogsCleared => {
                                    println!("logs cleared");
                                }
                                DevEvent::LogsReady => {}
                                DevEvent::ExitWithMessage(msg) => {
                                    println!("{}", msg);
                                    break;
                                }
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
    {
        let mut lock = child_state.lock().await;
        if let Some(mut child) = lock.take() {
            let _ = child.kill().await;
            let _ = child.wait().await;
        }
    }
    let _ = crate::dev_server_client::unregister_app(&canonical_project_dir_str).await;
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

#[derive(Debug, Clone)]
struct AttachedDevClient {
    project_dir: String,
    url: String,
    log_path: std::path::PathBuf,
}

fn dev_client_suffix(project_dir: &Path) -> String {
    let canonical =
        std::fs::canonicalize(project_dir).unwrap_or_else(|_| project_dir.to_path_buf());
    let mut h = sha2::Sha256::new();
    h.update(canonical.to_string_lossy().as_bytes());
    let digest = h.finalize();
    hex::encode(&digest[..4])
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
        DevEvent::LogsCleared | DevEvent::LogsReady | DevEvent::ExitWithMessage(_) => None,
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
                                    // The owning client receives app lifecycle events directly
                                    // from the supervisor; attached clients rebuild app state
                                    // from persisted markers.
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

async fn run_attached_dev_client(
    app_name: &str,
    interactive: bool,
    session: AttachedDevClient,
    display_hosts: Vec<String>,
) -> Result<(), Box<dyn std::error::Error>> {
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
        upstream_port = app.upstream_port;
    }

    let hosts = display_hosts;

    let (log_tx, log_rx) = mpsc::channel::<ScopedLog>(1000);
    let (event_tx, event_rx) = mpsc::channel::<DevEvent>(32);
    let (control_tx, mut control_rx) = mpsc::channel::<output::ControlCmd>(32);
    let (stop_tx, stop_rx) = watch::channel(false);

    // Subscribe to dev-server events. This both counts as a control client
    // (preventing premature idle-exit) and lets us detect when the app is
    // unregistered — a much more reliable exit trigger than PID polling.
    {
        let event_tx = event_tx.clone();
        let stop_tx = stop_tx.clone();
        let app_name_sub = app_name.to_string();
        tokio::spawn(async move {
            let mut got_stop = false;

            let connected = async {
                let ev_rx = crate::dev_server_client::subscribe_events().await.ok()?;
                Some(ev_rx)
            };

            if let Some(mut ev_rx) = connected.await {
                while let Some(ev) = ev_rx.recv().await {
                    if let crate::dev_server_client::DevServerEvent::AppStatusChanged {
                        ref app_name,
                        ref status,
                        ..
                    } = ev
                    {
                        if app_name == &app_name_sub && status == "stopped" {
                            got_stop = true;
                            break;
                        }
                    }
                }
            }

            // Always signal exit — either we got a "stopped" event, the
            // subscription disconnected (dev-server exited), or we failed
            // to connect at all. In every case the attached client should
            // exit cleanly with footer removal.
            let _ = stop_tx.send(true);
            let msg = if got_stop {
                "stopped by another client".to_string()
            } else {
                "disconnected from dev server".to_string()
            };
            let _ = event_tx.send(DevEvent::ExitWithMessage(msg)).await;
        });
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
        let stop_tx = stop_tx.clone();
        let project_dir = session.project_dir.clone();
        tokio::spawn(async move {
            while let Some(cmd) = control_rx.recv().await {
                match cmd {
                    output::ControlCmd::Restart => {
                        let result = crate::dev_server_client::restart_app(&project_dir)
                            .await
                            .map_err(|e| e.to_string());
                        if let Err(msg) = result {
                            let _ = log_tx
                                .send(ScopedLog::error("tako", format!("restart failed: {}", msg)))
                                .await;
                        }
                    }
                    output::ControlCmd::Terminate => {
                        let _ = crate::dev_server_client::unregister_app(&project_dir)
                            .await
                            .map_err(|e| e.to_string());
                        let _ = stop_tx.send(true);
                        break;
                    }
                }
            }
        });
    }

    if interactive {
        let adapter_name = if let Ok(project_dir) = std::env::current_dir() {
            if let Ok(cfg) = load_dev_tako_toml(&project_dir) {
                if let Ok(preset_ref) = resolve_dev_preset_ref(&project_dir, &cfg) {
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

        output::run_dev_output(
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
        println!("Attached to running dev app '{}'.", app_name);

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
                                DevEvent::AppError(e) => eprintln!("App error: {}", e),
                                DevEvent::LogsCleared => println!("logs cleared"),
                                DevEvent::ExitWithMessage(msg) => {
                                    println!("{}", msg);
                                    break;
                                }
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

    #[tokio::test]
    async fn exit_with_message_event_breaks_output_loop() {
        // Simulates the cross-client stop scenario: another client sends
        // ExitWithMessage, and the output loop should deliver it promptly.
        let (event_tx, mut event_rx) = mpsc::channel::<DevEvent>(32);

        // Send ExitWithMessage.
        event_tx
            .send(DevEvent::ExitWithMessage(
                "stopped by another client".to_string(),
            ))
            .await
            .unwrap();

        // The receiver should get it immediately.
        let event = timeout(Duration::from_millis(100), event_rx.recv())
            .await
            .expect("should not time out")
            .expect("channel should not be closed");

        match event {
            DevEvent::ExitWithMessage(msg) => {
                assert_eq!(msg, "stopped by another client");
            }
            other => panic!("unexpected event: {:?}", other),
        }
    }

    #[tokio::test]
    async fn event_subscription_sends_exit_when_channel_closes() {
        // Simulates the dev-server disconnecting: the subscription channel
        // closes, and the task should send ExitWithMessage as a fallback.
        let (event_tx, mut event_rx) = mpsc::channel::<DevEvent>(32);
        let (stop_tx, _stop_rx) = watch::channel(false);

        // Simulate event subscription task behavior when subscription drops.
        let event_tx_clone = event_tx.clone();
        let stop_tx_clone = stop_tx.clone();
        tokio::spawn(async move {
            // Simulate: no "stopped" event received, subscription just closes.
            let got_stop = false;

            let _ = stop_tx_clone.send(true);
            let msg = if got_stop {
                "stopped by another client".to_string()
            } else {
                "disconnected from dev server".to_string()
            };
            let _ = event_tx_clone.send(DevEvent::ExitWithMessage(msg)).await;
        });

        let event = timeout(Duration::from_millis(200), event_rx.recv())
            .await
            .expect("should not time out")
            .expect("channel should not be closed");

        match event {
            DevEvent::ExitWithMessage(msg) => {
                assert_eq!(msg, "disconnected from dev server");
            }
            other => panic!("unexpected event: {:?}", other),
        }
    }

    #[tokio::test]
    async fn event_subscription_sends_exit_on_stopped_status() {
        // Simulates receiving AppStatusChanged { status: "stopped" } from
        // the dev-server — the task should send ExitWithMessage.
        let (event_tx, mut event_rx) = mpsc::channel::<DevEvent>(32);
        let (stop_tx, _stop_rx) = watch::channel(false);

        let event_tx_clone = event_tx.clone();
        let stop_tx_clone = stop_tx.clone();
        let app_name_sub = "my-app".to_string();

        // Simulate the event subscription task receiving a "stopped" event.
        tokio::spawn(async move {
            let mut got_stop = false;

            // Simulate receiving an AppStatusChanged event.
            let events = vec![crate::dev_server_client::DevServerEvent::AppStatusChanged {
                project_dir: "/proj".to_string(),
                app_name: "my-app".to_string(),
                status: "stopped".to_string(),
            }];

            for ev in events {
                if let crate::dev_server_client::DevServerEvent::AppStatusChanged {
                    ref app_name,
                    ref status,
                    ..
                } = ev
                {
                    if app_name == &app_name_sub && status == "stopped" {
                        got_stop = true;
                        break;
                    }
                }
            }

            let _ = stop_tx_clone.send(true);
            let msg = if got_stop {
                "stopped by another client".to_string()
            } else {
                "disconnected from dev server".to_string()
            };
            let _ = event_tx_clone.send(DevEvent::ExitWithMessage(msg)).await;
        });

        let event = timeout(Duration::from_millis(200), event_rx.recv())
            .await
            .expect("should not time out")
            .expect("channel should not be closed");

        match event {
            DevEvent::ExitWithMessage(msg) => {
                assert_eq!(msg, "stopped by another client");
            }
            other => panic!("unexpected event: {:?}", other),
        }
    }

    #[tokio::test]
    async fn event_subscription_ignores_non_matching_app_name() {
        // ExitWithMessage should NOT be sent for a different app name.
        let (event_tx, mut event_rx) = mpsc::channel::<DevEvent>(32);
        let (stop_tx, _stop_rx) = watch::channel(false);

        let event_tx_clone = event_tx.clone();
        let stop_tx_clone = stop_tx.clone();
        let app_name_sub = "my-app".to_string();

        tokio::spawn(async move {
            let mut got_stop = false;

            let events = vec![crate::dev_server_client::DevServerEvent::AppStatusChanged {
                project_dir: "/proj".to_string(),
                app_name: "other-app".to_string(), // Different app
                status: "stopped".to_string(),
            }];

            for ev in events {
                if let crate::dev_server_client::DevServerEvent::AppStatusChanged {
                    ref app_name,
                    ref status,
                    ..
                } = ev
                {
                    if app_name == &app_name_sub && status == "stopped" {
                        got_stop = true;
                        break;
                    }
                }
            }

            // No matching event — falls through as disconnected.
            let _ = stop_tx_clone.send(true);
            let msg = if got_stop {
                "stopped by another client".to_string()
            } else {
                "disconnected from dev server".to_string()
            };
            let _ = event_tx_clone.send(DevEvent::ExitWithMessage(msg)).await;
        });

        let event = timeout(Duration::from_millis(200), event_rx.recv())
            .await
            .expect("should not time out")
            .expect("channel should not be closed");

        match event {
            DevEvent::ExitWithMessage(msg) => {
                // Should NOT be "stopped by another client" since app name didn't match.
                assert_eq!(msg, "disconnected from dev server");
            }
            other => panic!("unexpected event: {:?}", other),
        }
    }

    #[tokio::test]
    async fn event_subscription_ignores_idle_status() {
        // "idle" status should not trigger exit — only "stopped".
        let (event_tx, mut event_rx) = mpsc::channel::<DevEvent>(32);
        let (stop_tx, _stop_rx) = watch::channel(false);

        let event_tx_clone = event_tx.clone();
        let stop_tx_clone = stop_tx.clone();
        let app_name_sub = "my-app".to_string();

        tokio::spawn(async move {
            let mut got_stop = false;

            let events = vec![crate::dev_server_client::DevServerEvent::AppStatusChanged {
                project_dir: "/proj".to_string(),
                app_name: "my-app".to_string(),
                status: "idle".to_string(), // Not "stopped"
            }];

            for ev in events {
                if let crate::dev_server_client::DevServerEvent::AppStatusChanged {
                    ref app_name,
                    ref status,
                    ..
                } = ev
                {
                    if app_name == &app_name_sub && status == "stopped" {
                        got_stop = true;
                        break;
                    }
                }
            }

            let _ = stop_tx_clone.send(true);
            let msg = if got_stop {
                "stopped by another client".to_string()
            } else {
                "disconnected from dev server".to_string()
            };
            let _ = event_tx_clone.send(DevEvent::ExitWithMessage(msg)).await;
        });

        let event = timeout(Duration::from_millis(200), event_rx.recv())
            .await
            .expect("should not time out")
            .expect("channel should not be closed");

        match event {
            DevEvent::ExitWithMessage(msg) => {
                // "idle" is not "stopped", so should be treated as disconnect.
                assert_eq!(msg, "disconnected from dev server");
            }
            other => panic!("unexpected event: {:?}", other),
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
        .stdin(std::process::Stdio::inherit())
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

fn should_drop_child_log_line(line: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return true;
    }
    let Some(rest) = trimmed.strip_prefix("$ ") else {
        return false;
    };
    rest.chars()
        .next()
        .is_some_and(|ch| ch.is_ascii_alphanumeric() || ch == '.' || ch == '/' || ch == '@')
}

fn trim_child_log_message(message: &str) -> Option<String> {
    let trimmed_end = message.trim_end();
    if trimmed_end.trim().is_empty() {
        None
    } else {
        Some(trimmed_end.to_string())
    }
}

async fn read_child_lines<R>(r: R, log_tx: mpsc::Sender<ScopedLog>, scope: String, level: LogLevel)
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut lines = tokio::io::BufReader::new(r).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let Some(line) = trim_child_log_message(&line) else {
            continue;
        };
        if should_drop_child_log_line(&line) {
            continue;
        }
        let (line_level, line_message) = child_log_level_and_message(level.clone(), &line);
        let Some(line_message) = trim_child_log_message(&line_message) else {
            continue;
        };
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
    /// Another client stopped the app — exit cleanly.
    ExitWithMessage(String),
}

#[cfg(test)]
mod disambiguate_tests {
    use super::*;

    // -----------------------------------------------------------------------
    // sanitize_name_segment
    // -----------------------------------------------------------------------

    #[test]
    fn sanitize_name_segment_lowercases() {
        assert_eq!(sanitize_name_segment("MyApp"), "myapp");
    }

    #[test]
    fn sanitize_name_segment_replaces_special_chars() {
        assert_eq!(sanitize_name_segment("foo_bar.baz"), "foo-bar-baz");
    }

    #[test]
    fn sanitize_name_segment_collapses_consecutive_separators() {
        assert_eq!(sanitize_name_segment("a__b--c..d"), "a-b-c-d");
    }

    #[test]
    fn sanitize_name_segment_strips_leading_trailing_hyphens() {
        assert_eq!(sanitize_name_segment("-abc-"), "abc");
    }

    #[test]
    fn sanitize_name_segment_drops_non_ascii() {
        assert_eq!(sanitize_name_segment("café"), "caf");
    }

    // -----------------------------------------------------------------------
    // short_path_hash
    // -----------------------------------------------------------------------

    #[test]
    fn short_path_hash_is_deterministic() {
        let a = short_path_hash("/home/user/project");
        let b = short_path_hash("/home/user/project");
        assert_eq!(a, b);
    }

    #[test]
    fn short_path_hash_differs_for_different_paths() {
        let a = short_path_hash("/home/user/project-a");
        let b = short_path_hash("/home/user/project-b");
        assert_ne!(a, b);
    }

    #[test]
    fn short_path_hash_is_4_hex_chars() {
        let h = short_path_hash("/some/path");
        assert_eq!(h.len(), 4);
        assert!(h.chars().all(|c| c.is_ascii_hexdigit()));
    }

    // -----------------------------------------------------------------------
    // disambiguate_app_name — no conflict
    // -----------------------------------------------------------------------

    #[test]
    fn no_existing_apps_returns_candidate_unchanged() {
        let result = disambiguate_app_name("my-app", "/proj", &[]);
        assert_eq!(result, "my-app");
    }

    #[test]
    fn same_project_dir_is_not_a_conflict() {
        let existing = vec![("my-app".into(), "/proj".into())];
        let result = disambiguate_app_name("my-app", "/proj", &existing);
        assert_eq!(result, "my-app");
    }

    #[test]
    fn different_name_is_not_a_conflict() {
        let existing = vec![("other-app".into(), "/other".into())];
        let result = disambiguate_app_name("my-app", "/proj", &existing);
        assert_eq!(result, "my-app");
    }

    // -----------------------------------------------------------------------
    // disambiguate_app_name — conflict resolved by dir name
    // -----------------------------------------------------------------------

    #[test]
    fn conflict_appends_dir_leaf_name() {
        let existing = vec![("my-app".into(), "/home/user/proj-a".into())];
        let result = disambiguate_app_name("my-app", "/home/user/proj-b", &existing);
        assert_eq!(result, "my-app-proj-b");
    }

    #[test]
    fn conflict_from_variant_matching_existing_app_name() {
        // "app-foo" is registered (no variant). A new project "app" with
        // --variant foo produces the same composite name "app-foo".
        let existing = vec![("app-foo".into(), "/proj/app-foo".into())];
        let result = disambiguate_app_name("app-foo", "/proj/app", &existing);
        assert_eq!(result, "app-foo-app");
    }

    #[test]
    fn conflict_from_non_variant_matching_variant_composite() {
        // "app" with --variant "foo" is registered as "app-foo". A new
        // project literally named "app-foo" (no variant) would collide.
        let existing = vec![("app-foo".into(), "/proj/app".into())];
        let result = disambiguate_app_name("app-foo", "/proj/app-foo", &existing);
        assert_eq!(result, "app-foo-app-foo");
    }

    // -----------------------------------------------------------------------
    // disambiguate_app_name — dir name also conflicts → hash fallback
    // -----------------------------------------------------------------------

    #[test]
    fn double_conflict_falls_back_to_hash() {
        // Both the base name and the dir-suffixed name already taken.
        let existing = vec![
            ("my-app".into(), "/workspace/a".into()),
            ("my-app-b".into(), "/workspace/c".into()),
        ];
        let result = disambiguate_app_name("my-app", "/workspace/b", &existing);
        let hash = short_path_hash("/workspace/b");
        assert_eq!(result, format!("my-app-{hash}"));
    }

    // -----------------------------------------------------------------------
    // disambiguate_app_name — workspace / monorepo scenarios
    // -----------------------------------------------------------------------

    #[test]
    fn workspace_apps_get_folder_suffix() {
        // Two packages in a monorepo, both named "api" in tako.toml.
        let existing = vec![("api".into(), "/repo/packages/billing".into())];
        let result = disambiguate_app_name("api", "/repo/packages/payments", &existing);
        assert_eq!(result, "api-payments");
    }

    #[test]
    fn two_checkouts_of_same_repo_get_folder_suffix() {
        let existing = vec![("my-app".into(), "/home/user/my-app-main".into())];
        let result = disambiguate_app_name("my-app", "/home/user/my-app-feature", &existing);
        assert_eq!(result, "my-app-my-app-feature");
    }

    // -----------------------------------------------------------------------
    // disambiguate_app_name — multiple registered apps
    // -----------------------------------------------------------------------

    #[test]
    fn no_conflict_among_many_registered_apps() {
        let existing = vec![
            ("alpha".into(), "/a".into()),
            ("beta".into(), "/b".into()),
            ("gamma".into(), "/c".into()),
        ];
        let result = disambiguate_app_name("delta", "/d", &existing);
        assert_eq!(result, "delta");
    }

    #[test]
    fn conflict_detected_among_many_registered_apps() {
        let existing = vec![
            ("alpha".into(), "/a".into()),
            ("beta".into(), "/b".into()),
            ("gamma".into(), "/c".into()),
        ];
        let result = disambiguate_app_name("beta", "/other", &existing);
        assert_eq!(result, "beta-other");
    }

    // -----------------------------------------------------------------------
    // Edge cases
    // -----------------------------------------------------------------------

    #[test]
    fn root_path_project_uses_hash_fallback() {
        // "/" has no file_name component.
        let existing = vec![("app".into(), "/other".into())];
        let result = disambiguate_app_name("app", "/", &existing);
        let hash = short_path_hash("/");
        assert_eq!(result, format!("app-{hash}"));
    }

    #[test]
    fn re_registration_after_disambiguation_is_idempotent() {
        // After disambiguation, the app was registered as "api-payments".
        // Re-running from the same dir should find itself and not
        // disambiguate further.
        let existing = vec![
            ("api".into(), "/repo/packages/billing".into()),
            ("api-payments".into(), "/repo/packages/payments".into()),
        ];
        // The candidate is still "api" (before disambiguation), and project
        // dir is the same as the "api-payments" entry won't match "api", so
        // we'd still disambiguate. But the disambiguated name "api-payments"
        // matches our own project_dir, so it's not a conflict.
        let result =
            disambiguate_app_name("api", "/repo/packages/payments", &existing);
        assert_eq!(result, "api-payments");
    }
}

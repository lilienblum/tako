//! Tako Dev Client
//!
//! CLI client for the tako-dev-server daemon:
//! - HTTPS via local CA (`{app-name}.test` / `{app-name}.tako.test`)
//! - Local authoritative DNS for `*.test` and `*.tako.test`
//! - `tako.toml` watching for env/route updates
//! - Streaming logs, status, and resource monitoring
//! - Process lifecycle managed by the daemon

mod ca_setup;
mod client;
mod linux_setup;
mod local_setup;
pub(crate) mod loopback_proxy;
mod output;
mod output_render;
mod project;
mod runner;
mod tls;
mod watcher;

use std::path::Path;
use std::sync::OnceLock;
#[cfg(test)]
use std::time::Duration;

use serde::{Deserialize, Serialize};
use time::{OffsetDateTime, UtcOffset};

use crate::app::resolve_app_name_from_config_path;
use crate::build::{PresetGroup, apply_adapter_base_runtime_defaults, js};
use crate::config::TakoToml;
use crate::dev::LocalCA;
use client::{ConnectedDevClient, LogStreamEvent, parse_log_line, run_connected_dev_client};
#[cfg(target_os = "macos")]
use local_setup::local_https_probe_error;
use local_setup::{
    ensure_local_dns_resolver_configured, explain_pending_sudo_setup, local_https_probe_host,
    wait_for_https_host_reachable_via_ip,
};
#[cfg(test)]
use local_setup::{
    local_dns_resolver_contents, local_dns_sudo_action_line, parse_local_dns_resolver,
    sudo_setup_action_items, tcp_probe,
};
use project::{
    compute_dev_env, compute_dev_hosts, compute_display_routes, dev_startup_lines, dev_url,
    disambiguate_app_name, has_explicit_dev_preset, infer_preset_name_from_ref, inject_dev_secrets,
    preferred_public_url, resolve_dev_preset_ref, resolve_dev_run_command,
    resolve_effective_dev_build_adapter, try_list_registered_app_names,
};
#[cfg(test)]
use project::{route_hostname_matches, sanitize_name_segment, short_path_hash};
use tls::ensure_dev_server_tls_material;
#[cfg(test)]
use tls::{
    dev_server_tls_names_path_for_home, dev_server_tls_paths_for_home,
    ensure_dev_server_tls_material_for_home,
};

pub use ca_setup::setup_local_ca;
#[cfg(target_os = "linux")]
pub(crate) use linux_setup::{LinuxSetupStatus, status as linux_setup_status};
pub(crate) use local_setup::{is_dev_server_unavailable_error_message, local_dns_resolver_values};
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

    #[allow(dead_code)]
    pub fn divider(label: &str) -> Self {
        Self {
            timestamp: String::new(),
            level: LogLevel::Info,
            scope: DIVIDER_SCOPE.to_string(),
            message: label.to_string(),
        }
    }
}

#[cfg(test)]
const APP_SCOPE: &str = "app";
pub const DIVIDER_SCOPE: &str = "__divider__";

#[cfg(test)]
fn app_log_scope() -> String {
    APP_SCOPE.to_string()
}

#[cfg(test)]
const DEV_INITIAL_INSTANCE_COUNT: usize = 1;
#[cfg(test)]
const DEV_IDLE_TIMEOUT_SECS: u64 = 30 * 60;
pub(crate) const LOCAL_DNS_PORT: u16 = 53535;
#[cfg(target_os = "macos")]
const RESOLVER_DIR: &str = "/etc/resolver";
#[cfg(target_os = "macos")]
pub(crate) const TAKO_RESOLVER_FILE: &str = "/etc/resolver/tako.test";
#[cfg(target_os = "macos")]
pub(crate) const SHORT_RESOLVER_FILE: &str = "/etc/resolver/test";
const LOCALHOST_443_HTTPS_PROBE_ATTEMPTS: usize = 12;
const LOCALHOST_443_HTTPS_PROBE_TIMEOUT_MS: u64 = 500;
const LOCALHOST_443_HTTPS_PROBE_RETRY_DELAY_MS: u64 = 150;
pub(crate) const DEV_LOOPBACK_ADDR: &str = "127.77.0.1";

#[cfg(test)]
fn dev_initial_instance_count() -> usize {
    DEV_INITIAL_INSTANCE_COUNT
}

#[cfg(test)]
fn dev_idle_timeout() -> Duration {
    Duration::from_secs(DEV_IDLE_TIMEOUT_SECS)
}

fn load_dev_tako_toml(config_path: &Path) -> crate::config::Result<TakoToml> {
    TakoToml::load_from_file(config_path)
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

#[cfg(any(target_os = "macos", target_os = "linux"))]
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

#[cfg(test)]
mod tests {
    use super::client::host_and_port_from_url;
    use super::{
        DevEvent, LogLevel, LogStreamEvent, ScopedLog, app_log_scope, child_log_level_and_message,
        compute_dev_hosts, compute_display_routes, dev_idle_timeout, dev_initial_instance_count,
        dev_server_tls_names_path_for_home, dev_server_tls_paths_for_home, dev_startup_lines,
        doctor_dev_server_lines, doctor_local_forwarding_preflight_lines,
        ensure_dev_server_tls_material_for_home, is_dev_server_unavailable_error_message,
        local_dns_resolver_contents, local_dns_sudo_action_line, local_https_probe_host,
        parse_local_dns_resolver, parse_log_line, port_from_listen, preferred_public_url,
        resolve_dev_preset_ref, resolve_dev_run_command, resolve_effective_dev_build_adapter,
        restart_required_for_requested_listen, route_hostname_matches, should_drop_child_log_line,
        sudo_setup_action_items, tcp_probe, trim_child_log_message,
    };
    #[cfg(target_os = "macos")]
    use super::{ensure_local_dns_resolver_configured, local_https_probe_error};
    use crate::build::{BuildAdapter, parse_and_validate_preset};
    use crate::config::TakoToml;
    use crate::dev::LocalCA;
    use std::path::Path;
    use std::time::Duration;
    use tempfile::TempDir;

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
            "javascript/tanstack-start"
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
    fn resolve_dev_run_command_uses_sdk_entrypoint_for_bun() {
        let preset = parse_and_validate_preset(
            r#"
main = "src/index.ts"
"#,
            "bun",
        )
        .unwrap();

        let pd = Path::new("/project");
        let cmd = resolve_dev_run_command(
            &TakoToml::default(),
            &preset,
            "src/index.ts",
            BuildAdapter::Bun,
            false,
            pd,
        )
        .expect("runtime default dev command");

        // JS dev runs through the SDK entrypoint, same as production.
        assert_eq!(cmd[0], "bun");
        assert!(cmd.iter().any(|a| a.contains("entrypoints/bun.mjs")));
        assert!(cmd.last().unwrap().ends_with("src/index.ts"));
    }

    #[test]
    fn resolve_dev_run_command_uses_sdk_entrypoint_for_node() {
        let preset = parse_and_validate_preset(
            r#"
main = "dist/server/tako-entry.mjs"
"#,
            "tanstack-start",
        )
        .unwrap();

        let pd = Path::new("/project");
        let cmd = resolve_dev_run_command(
            &TakoToml::default(),
            &preset,
            "src/index.ts",
            BuildAdapter::Node,
            true,
            pd,
        )
        .expect("runtime default dev command");

        assert_eq!(cmd[0], "node");
        assert!(cmd.iter().any(|a| a.contains("entrypoints/node.mjs")));
        assert!(cmd.last().unwrap().ends_with("src/index.ts"));
    }

    #[test]
    fn resolve_dev_run_command_preset_dev_overrides_runtime_default() {
        let mut preset = parse_and_validate_preset(
            r#"
main = "src/index.ts"
"#,
            "vite",
        )
        .unwrap();
        preset.dev = vec!["vite".to_string(), "dev".to_string()];

        let pd = Path::new("/project");
        let cmd = resolve_dev_run_command(
            &TakoToml::default(),
            &preset,
            "src/index.ts",
            BuildAdapter::Bun,
            true,
            pd,
        )
        .expect("preset dev command");

        assert_eq!(cmd, vec!["vite", "dev"]);
    }

    #[test]
    fn resolve_dev_run_command_config_dev_overrides_preset() {
        let mut preset = parse_and_validate_preset(
            r#"
main = "src/index.ts"
"#,
            "vite",
        )
        .unwrap();
        preset.dev = vec!["vite".to_string(), "dev".to_string()];

        let cfg = TakoToml {
            dev: vec!["custom".to_string(), "cmd".to_string()],
            ..Default::default()
        };

        let pd = Path::new("/project");
        let cmd =
            resolve_dev_run_command(&cfg, &preset, "src/index.ts", BuildAdapter::Bun, true, pd)
                .expect("config dev command");

        assert_eq!(cmd, vec!["custom", "cmd"]);
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
        let decoded = parse_log_line(&encoded).unwrap();

        let LogStreamEvent::Log(decoded) = decoded else {
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
        let decoded = parse_log_line(&encoded).unwrap();
        let LogStreamEvent::Log(decoded) = decoded else {
            panic!("expected log event");
        };
        assert!(matches!(decoded.level, LogLevel::Fatal));
    }

    #[test]
    fn stored_log_line_preserves_unrecognized_json_log_shape_as_message() {
        let raw_line = r#"{"h":12,"m":3,"s":7,"level":"Info","scope":"app","message":"hello"}"#;
        let decoded = parse_log_line(raw_line).unwrap();

        let LogStreamEvent::Log(decoded) = decoded else {
            panic!("expected log event");
        };

        assert_ne!(decoded.timestamp, "12:03:07");
        assert!(matches!(decoded.level, LogLevel::Info));
        assert_eq!(decoded.scope, "app");
        assert_eq!(decoded.message, raw_line);
    }

    #[test]
    fn stored_log_line_parses_app_started_marker() {
        let decoded = parse_log_line(r#"{"type":"app_event","event":"started"}"#).unwrap();
        assert!(matches!(
            decoded,
            LogStreamEvent::AppEvent(DevEvent::AppStarted)
        ));
    }

    #[test]
    fn stored_log_line_parses_app_pid_marker() {
        let decoded = parse_log_line(r#"{"type":"app_event","event":"pid","pid":4242}"#).unwrap();
        assert!(matches!(
            decoded,
            LogStreamEvent::AppEvent(DevEvent::AppPid(4242))
        ));
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
  "*.demo.tako.test",
  "*.demo.test",
  "*.tako.test",
  "*.test",
  "demo.tako.test",
  "demo.test",
  "tako.test",
  "test"
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
            Some("Trust the Tako local CA for trusted https://*.test"),
            true,
            Some("Install the local loopback proxy for 127.77.0.1:80/443"),
        );
        assert_eq!(
            items,
            vec![
                "Trust the Tako local CA for trusted https://*.test".to_string(),
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
        let routes =
            compute_display_routes(&cfg, "example-foo.tako.test", Some("example.tako.test"));
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
        let cfg =
            TakoToml::parse("[envs.development]\nroutes = [\"example.tako.test\"]\n").unwrap();
        let routes =
            compute_display_routes(&cfg, "example-foo.tako.test", Some("example.tako.test"));
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

pub use runner::{ls, run, stop};

#[cfg(test)]
fn strip_ascii_case_prefix<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    if s.len() < prefix.len() {
        return None;
    }
    let (head, tail) = s.split_at(prefix.len());
    head.eq_ignore_ascii_case(prefix).then_some(tail)
}

#[cfg(test)]
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

#[cfg(test)]
fn child_log_level_and_message(default_level: LogLevel, line: &str) -> (LogLevel, String) {
    prefixed_child_log_level_and_message(line).unwrap_or((default_level, line.to_string()))
}

#[cfg(test)]
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

#[cfg(test)]
fn trim_child_log_message(message: &str) -> Option<String> {
    let trimmed_end = message.trim_end();
    if trimmed_end.trim().is_empty() {
        None
    } else {
        Some(trimmed_end.to_string())
    }
}

/// Events from the dev server
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum DevEvent {
    AppLaunching,
    AppStarted,
    AppStopped,
    AppProcessExited(String),
    AppPid(u32),
    AppError(String),
    /// A CLI client connected — `is_self` true for the current client.
    ClientConnected {
        is_self: bool,
        client_id: u32,
    },
    /// A CLI client disconnected.
    ClientDisconnected {
        client_id: u32,
    },
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
        let existing = vec![("my-app".into(), "/proj/tako.toml".into())];
        let result = disambiguate_app_name("my-app", "/proj/tako.toml", &existing);
        assert_eq!(result, "my-app");
    }

    #[test]
    fn different_name_is_not_a_conflict() {
        let existing = vec![("other-app".into(), "/other/tako.toml".into())];
        let result = disambiguate_app_name("my-app", "/proj/tako.toml", &existing);
        assert_eq!(result, "my-app");
    }

    // -----------------------------------------------------------------------
    // disambiguate_app_name — conflict resolved by dir name
    // -----------------------------------------------------------------------

    #[test]
    fn conflict_appends_dir_leaf_name() {
        let existing = vec![("my-app".into(), "/home/user/proj-a/tako.toml".into())];
        let result = disambiguate_app_name("my-app", "/home/user/proj-b/tako.toml", &existing);
        assert_eq!(result, "my-app-proj-b");
    }

    #[test]
    fn conflict_from_variant_matching_existing_app_name() {
        // "app-foo" is registered (no variant). A new project "app" with
        // --variant foo produces the same composite name "app-foo".
        let existing = vec![("app-foo".into(), "/proj/app-foo/tako.toml".into())];
        let result = disambiguate_app_name("app-foo", "/proj/app/tako.toml", &existing);
        assert_eq!(result, "app-foo-app");
    }

    #[test]
    fn conflict_from_non_variant_matching_variant_composite() {
        // "app" with --variant "foo" is registered as "app-foo". A new
        // project literally named "app-foo" (no variant) would collide.
        let existing = vec![("app-foo".into(), "/proj/app/tako.toml".into())];
        let result = disambiguate_app_name("app-foo", "/proj/app-foo/tako.toml", &existing);
        assert_eq!(result, "app-foo-app-foo");
    }

    // -----------------------------------------------------------------------
    // disambiguate_app_name — dir name also conflicts → hash fallback
    // -----------------------------------------------------------------------

    #[test]
    fn double_conflict_falls_back_to_hash() {
        // Both the base name and the dir-suffixed name are already taken by
        // different configs, so the third config falls back to a hash.
        let existing = vec![
            ("my-app".into(), "/workspace/a/tako.toml".into()),
            ("my-app-b".into(), "/workspace/b/tako.toml".into()),
        ];
        let result = disambiguate_app_name("my-app", "/workspace/c/b/tako.toml", &existing);
        let hash = short_path_hash("/workspace/c/b/tako.toml");
        assert_eq!(result, format!("my-app-{hash}"));
    }

    // -----------------------------------------------------------------------
    // disambiguate_app_name — workspace / monorepo scenarios
    // -----------------------------------------------------------------------

    #[test]
    fn workspace_apps_get_folder_suffix() {
        // Two packages in a monorepo, both named "api" in tako.toml.
        let existing = vec![("api".into(), "/repo/packages/billing/tako.toml".into())];
        let result = disambiguate_app_name("api", "/repo/packages/payments/tako.toml", &existing);
        assert_eq!(result, "api-payments");
    }

    #[test]
    fn two_checkouts_of_same_repo_get_folder_suffix() {
        let existing = vec![("my-app".into(), "/home/user/my-app-main/tako.toml".into())];
        let result =
            disambiguate_app_name("my-app", "/home/user/my-app-feature/tako.toml", &existing);
        assert_eq!(result, "my-app-my-app-feature");
    }

    // -----------------------------------------------------------------------
    // disambiguate_app_name — multiple registered apps
    // -----------------------------------------------------------------------

    #[test]
    fn no_conflict_among_many_registered_apps() {
        let existing = vec![
            ("alpha".into(), "/a/tako.toml".into()),
            ("beta".into(), "/b/tako.toml".into()),
            ("gamma".into(), "/c/tako.toml".into()),
        ];
        let result = disambiguate_app_name("delta", "/d/tako.toml", &existing);
        assert_eq!(result, "delta");
    }

    #[test]
    fn conflict_detected_among_many_registered_apps() {
        let existing = vec![
            ("alpha".into(), "/a/tako.toml".into()),
            ("beta".into(), "/b/tako.toml".into()),
            ("gamma".into(), "/c/tako.toml".into()),
        ];
        let result = disambiguate_app_name("beta", "/other/tako.toml", &existing);
        assert_eq!(result, "beta-other");
    }

    // -----------------------------------------------------------------------
    // Edge cases
    // -----------------------------------------------------------------------

    #[test]
    fn root_path_project_uses_hash_fallback() {
        // "/" has no file_name component.
        let existing = vec![("app".into(), "/other/tako.toml".into())];
        let result = disambiguate_app_name("app", "/tako.toml", &existing);
        let hash = short_path_hash("/tako.toml");
        assert_eq!(result, format!("app-{hash}"));
    }

    #[test]
    fn re_registration_after_disambiguation_is_idempotent() {
        // After disambiguation, the app was registered as "api-payments".
        // Re-running from the same dir should find itself and not
        // disambiguate further.
        let existing = vec![
            ("api".into(), "/repo/packages/billing/tako.toml".into()),
            (
                "api-payments".into(),
                "/repo/packages/payments/tako.toml".into(),
            ),
        ];
        let result = disambiguate_app_name("api", "/repo/packages/payments/tako.toml", &existing);
        assert_eq!(result, "api-payments");
    }
}

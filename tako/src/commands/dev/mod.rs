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
pub(crate) mod dev_proxy;
mod linux_setup;
mod local_setup;
mod output;
mod output_render;
mod project;
mod runner;
mod shared;
mod tls;
mod types;
mod watcher;

#[cfg(test)]
use std::time::Duration;

use crate::app::resolve_app_name_from_config_path;
use crate::build::{PresetGroup, apply_adapter_base_runtime_defaults, js};
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
#[cfg(test)]
use shared::{doctor_dev_server_lines, doctor_local_forwarding_preflight_lines};
use tls::ensure_dev_server_tls_material;
#[cfg(test)]
use tls::{
    dev_server_tls_names_path_for_home, dev_server_tls_paths_for_home,
    ensure_dev_server_tls_material_for_home,
};
pub use types::{DIVIDER_SCOPE, DevEvent, LogLevel, ScopedLog};
#[cfg(test)]
use types::{
    app_log_scope, child_log_level_and_message, should_drop_child_log_line, trim_child_log_message,
};

pub use ca_setup::setup_local_ca;
#[cfg(target_os = "macos")]
pub(crate) use dev_proxy::{DEV_PROXY_LABEL, DevProxyStatus, status as dev_proxy_status};
#[cfg(target_os = "linux")]
pub(crate) use linux_setup::{LinuxSetupStatus, status as linux_setup_status};
pub(crate) use local_setup::is_dev_server_unavailable_error_message;
#[cfg(target_os = "macos")]
pub(crate) use local_setup::local_dns_resolver_values;

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

#[cfg(any(target_os = "macos", target_os = "linux"))]
pub(crate) use shared::system_resolver_ipv4;
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub(crate) use shared::{
    load_dev_tako_toml, port_from_listen, restart_required_for_requested_listen,
};

pub use runner::{ls, run, stop};
#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::env::current_dir;
use std::process::Command;
use std::sync::OnceLock;

use crate::app::resolve_app_name;
use crate::commands::server;
use crate::config::{SecretsStore, ServerEntry, ServersToml, TakoToml};
use crate::output;
use crate::ssh::{SshClient, SshConfig};
use tako_core::{AppState, AppStatus, InstanceState, Response};
use time::{OffsetDateTime, UtcOffset};

/// Server status result from querying a remote server
#[derive(Debug)]
struct ServerStatusResult {
    service_status: String,
    server_version: Option<String>,
    app_status: Option<AppStatus>,
    deployed_at_unix_secs: Option<i64>,
    error: Option<String>,
}

static LOCAL_OFFSET: OnceLock<UtcOffset> = OnceLock::new();

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StatusLineLevel {
    Success,
    Warning,
    Error,
}

pub async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let project_dir = current_dir()?;

    // Load configuration
    let tako_config = TakoToml::load_from_dir(&project_dir)?;
    let mut servers = ServersToml::load()?;
    let secrets = SecretsStore::load_from_dir(&project_dir)?;

    // Resolve app name the same way deploy does.
    let app_name = resolve_app_name(&project_dir).unwrap_or_else(|_| {
        project_dir
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown")
            .to_string()
    });

    if servers.is_empty()
        && server::prompt_to_add_server(
            "No servers configured yet. Add one now to see live deployment status.",
        )
        .await?
        .is_some()
    {
        servers = ServersToml::load()?;
    }

    // Show deploy environments (exclude development)
    let env_names = display_environment_names_for_status(&tako_config, &servers);
    if env_names.is_empty() {
        output::warning("No deployment environments to show.");
        output::muted(
            "Map a server in tako.toml (e.g. [servers.<name>] env = \"production\") or add [envs.<name>].",
        );
        return Ok(());
    }

    let env_server_map: Vec<(String, Vec<String>)> = env_names
        .iter()
        .map(|env_name| {
            (
                env_name.clone(),
                resolve_env_server_names_for_status(&tako_config, &servers, env_name),
            )
        })
        .collect();

    let status_target_names = collect_status_target_names(&env_server_map);
    let mut status_tasks = spawn_status_tasks(&servers, &status_target_names, &app_name);
    let mut server_statuses: HashMap<String, ServerStatusResult> = HashMap::new();

    for (env_name, server_names) in env_server_map {
        output::section(&env_name);

        if server_names.is_empty() {
            output::muted("No servers mapped to this environment.");
            continue;
        }

        for server_name in &server_names {
            match servers.get(server_name.as_str()) {
                Some(entry) => {
                    ensure_server_status(
                        server_name,
                        entry,
                        &mut status_tasks,
                        &mut server_statuses,
                    )
                    .await?;
                    let status = server_statuses.get(server_name.as_str());
                    let heading = format_server_status_heading(server_name, status);
                    match status_line_level(status) {
                        StatusLineLevel::Success => output::success(&heading),
                        StatusLineLevel::Warning => output::warning(&heading),
                        StatusLineLevel::Error => output::error(&heading),
                    }
                    for detail in format_server_status_details(status) {
                        output::muted(&format!("  {}", detail));
                    }
                }
                None => {
                    output::error(&format!(
                        "{}: missing from global server inventory",
                        server_name
                    ));
                }
            }
        }
    }

    // Show secrets summary
    if !secrets.is_empty() {
        output::section("Secrets");
        let discrepancies = secrets.find_discrepancies();
        if discrepancies.is_empty() {
            output::success(&format!(
                "{} configured (consistent across environments)",
                secrets.all_secret_names().len()
            ));
        } else {
            output::warning(&format!(
                "{} configured ({} with discrepancies)",
                secrets.all_secret_names().len(),
                discrepancies.len()
            ));
            output::muted(&format!(
                "Run {} for details",
                output::emphasized("tako secret ls")
            ));
        }
    }

    Ok(())
}

fn collect_status_target_names(env_server_map: &[(String, Vec<String>)]) -> Vec<String> {
    let mut names = env_server_map
        .iter()
        .flat_map(|(_, server_names)| server_names.iter().cloned())
        .collect::<Vec<_>>();
    names.sort();
    names.dedup();
    names
}

fn spawn_status_tasks(
    servers: &ServersToml,
    status_target_names: &[String],
    app_name: &str,
) -> HashMap<String, tokio::task::JoinHandle<ServerStatusResult>> {
    let mut tasks = HashMap::new();

    for server_name in status_target_names {
        if let Some(entry) = servers.get(server_name.as_str()) {
            let host = entry.host.clone();
            let port = entry.port;
            let app = app_name.to_string();
            let handle = tokio::spawn(async move { query_server_status(&host, port, &app).await });
            tasks.insert(server_name.clone(), handle);
        }
    }

    tasks
}

async fn ensure_server_status(
    server_name: &str,
    server_entry: &ServerEntry,
    status_tasks: &mut HashMap<String, tokio::task::JoinHandle<ServerStatusResult>>,
    server_statuses: &mut HashMap<String, ServerStatusResult>,
) -> Result<(), Box<dyn std::error::Error>> {
    if server_statuses.contains_key(server_name) {
        return Ok(());
    }

    let host_port_fallback = format!("{}:{}", server_entry.host, server_entry.port);
    let Some(handle) = status_tasks.remove(server_name) else {
        server_statuses.insert(
            server_name.to_string(),
            ServerStatusResult {
                service_status: "unknown".to_string(),
                server_version: None,
                app_status: None,
                deployed_at_unix_secs: None,
                error: Some(format!(
                    "status query task missing for {}",
                    host_port_fallback
                )),
            },
        );
        return Ok(());
    };

    let join_result =
        output::with_spinner_async(format!("Checking {}...", server_name), async move {
            handle.await
        })
        .await?;

    let status = match join_result {
        Ok(result) => result,
        Err(e) => ServerStatusResult {
            service_status: "unknown".to_string(),
            server_version: None,
            app_status: None,
            deployed_at_unix_secs: None,
            error: Some(format!("status query task failed: {}", e)),
        },
    };

    server_statuses.insert(server_name.to_string(), status);
    Ok(())
}

/// Query a single server for its status
async fn query_server_status(host: &str, port: u16, app_name: &str) -> ServerStatusResult {
    let config = SshConfig::from_server(host, port);
    let mut client = SshClient::new(config);

    // Try to connect
    if let Err(e) = client.connect().await {
        return ServerStatusResult {
            service_status: "unknown".to_string(),
            server_version: None,
            app_status: None,
            deployed_at_unix_secs: None,
            error: Some(format!("SSH connect failed: {}", e)),
        };
    }

    let server_version = client
        .tako_version()
        .await
        .ok()
        .flatten()
        .map(normalize_server_version);

    // Check tako-server service status
    let service_status = match client.tako_status().await {
        Ok(status) => status,
        Err(e) => {
            let _ = client.disconnect().await;
            return ServerStatusResult {
                service_status: "unknown".to_string(),
                server_version,
                app_status: None,
                deployed_at_unix_secs: None,
                error: Some(format!("Failed to check service: {}", e)),
            };
        }
    };

    let mut app_status = None;
    let mut deployed_at_unix_secs = None;
    let mut error = None;

    // If service is active, query app status via socket.
    if service_status == "active" {
        match client.tako_app_status(app_name).await {
            Ok(Response::Ok { data }) => match serde_json::from_value::<AppStatus>(data) {
                Ok(status) => {
                    deployed_at_unix_secs =
                        query_app_deployed_at_unix_secs(&client, app_name, &status.version).await;
                    app_status = Some(status);
                }
                Err(e) => {
                    error = Some(format!("Failed to parse app status: {}", e));
                }
            },
            Ok(Response::Error { message }) => {
                // App might not be deployed yet
                if !message.contains("not found") {
                    error = Some(message);
                }
            }
            Err(e) => {
                // Socket might not be available.
                error = Some(format!("Socket query failed: {}", e));
            }
        }
    }

    let _ = client.disconnect().await;

    ServerStatusResult {
        service_status,
        server_version,
        app_status,
        deployed_at_unix_secs,
        error,
    }
}

fn normalize_server_version(raw: String) -> String {
    raw.trim()
        .strip_prefix("tako-server ")
        .unwrap_or(raw.trim())
        .to_string()
}

fn local_offset() -> UtcOffset {
    *LOCAL_OFFSET.get_or_init(|| UtcOffset::current_local_offset().unwrap_or(UtcOffset::UTC))
}

fn format_unix_timestamp_local(unix_secs: i64) -> Option<String> {
    format_unix_timestamp_with_date_command(unix_secs)
        .or_else(|| format_unix_timestamp_with_offset(unix_secs, local_offset()))
}

fn shell_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

async fn query_app_deployed_at_unix_secs(
    client: &SshClient,
    app_name: &str,
    version: &str,
) -> Option<i64> {
    let release_dir = format!("/opt/tako/apps/{}/releases/{}", app_name, version);
    let quoted = shell_single_quote(&release_dir);
    let cmd = format!(
        "if [ -d {path} ]; then (stat -c '%Y' {path} 2>/dev/null || stat -f '%m' {path} 2>/dev/null || echo -); else echo -; fi",
        path = quoted
    );

    let Ok(output) = client.exec(&cmd).await else {
        return None;
    };

    output
        .stdout
        .lines()
        .next()
        .and_then(|line| line.trim().parse::<i64>().ok())
}

fn resolve_env_server_names_for_status(
    tako_config: &TakoToml,
    servers: &ServersToml,
    env_name: &str,
) -> Vec<String> {
    let mut names: Vec<String> = tako_config
        .get_servers_for_env(env_name)
        .into_iter()
        .map(|name| name.to_string())
        .collect();

    if names.is_empty()
        && env_name == "production"
        && servers.len() == 1
        && let Some(only) = servers.names().into_iter().next()
    {
        names.push(only.to_string());
    }

    names
}

fn display_environment_names_for_status(
    tako_config: &TakoToml,
    servers: &ServersToml,
) -> Vec<String> {
    resolve_status_environment_names(tako_config, servers)
        .into_iter()
        .filter(|env| env != "development")
        .collect()
}

fn resolve_status_environment_names(tako_config: &TakoToml, servers: &ServersToml) -> Vec<String> {
    let mut env_names = tako_config.get_environment_names();

    for server in tako_config.servers.values() {
        if !env_names.iter().any(|name| name == &server.env) {
            env_names.push(server.env.clone());
        }
    }

    let has_explicit_production_mapping =
        tako_config.servers.values().any(|s| s.env == "production");
    let has_single_server_fallback =
        servers.len() == 1 && tako_config.get_servers_for_env("production").is_empty();

    if !env_names.iter().any(|name| name == "production")
        && (env_names.is_empty() || has_explicit_production_mapping || has_single_server_fallback)
    {
        env_names.push("production".to_string());
    }

    env_names.sort();
    env_names.dedup();
    env_names
}

fn format_server_status_heading(server_name: &str, status: Option<&ServerStatusResult>) -> String {
    let (service, _, _) = status_columns(status);
    let version = status
        .and_then(|result| result.server_version.as_deref())
        .map(display_server_version)
        .unwrap_or_else(|| "unknown".to_string());
    format!(
        "{} ({}) {}",
        server_name,
        version,
        display_service_state(&service)
    )
}

fn format_server_status_details(status: Option<&ServerStatusResult>) -> [String; 3] {
    let (_, build, state) = status_columns(status);
    let deployed_at = status
        .and_then(|result| result.deployed_at_unix_secs)
        .and_then(format_unix_timestamp_local)
        .unwrap_or_else(|| "-".to_string());
    [
        format!("instances: {}", format_instance_summary(status, &state)),
        format!("build: {}", build),
        format!("deployed: {}", deployed_at),
    ]
}

fn display_server_version(version: &str) -> String {
    if version.starts_with('v') {
        version.to_string()
    } else {
        format!("v{}", version)
    }
}

fn display_service_state(service: &str) -> &str {
    match service {
        "active" => "up",
        "inactive" | "failed" => "down",
        other => other,
    }
}

fn format_instance_summary(status: Option<&ServerStatusResult>, fallback_state: &str) -> String {
    if let Some(result) = status
        && let Some(app_status) = &result.app_status
    {
        let healthy = app_status
            .instances
            .iter()
            .filter(|i| i.state == InstanceState::Healthy || i.state == InstanceState::Ready)
            .count();
        return format!(
            "{}/{} ({})",
            healthy,
            app_status.instances.len(),
            app_status.state
        );
    }

    format!("-/- ({})", fallback_state)
}

fn format_unix_timestamp_with_offset(unix_secs: i64, offset: UtcOffset) -> Option<String> {
    let dt = OffsetDateTime::from_unix_timestamp(unix_secs)
        .ok()?
        .to_offset(offset);
    Some(format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        dt.year(),
        dt.month() as u8,
        dt.day(),
        dt.hour(),
        dt.minute(),
        dt.second()
    ))
}

fn format_unix_timestamp_with_date_command(unix_secs: i64) -> Option<String> {
    let unix = unix_secs.to_string();

    // macOS/BSD date
    let mut bsd = Command::new("date");
    bsd.args(["-r", &unix, "+%c"]);
    if let Some(value) = run_local_date_command(bsd) {
        return Some(value);
    }

    // GNU date
    let mut gnu = Command::new("date");
    gnu.args(["-d", &format!("@{}", unix_secs), "+%c"]);
    run_local_date_command(gnu)
}

fn run_local_date_command(mut cmd: Command) -> Option<String> {
    let output = cmd.output().ok()?;
    if !output.status.success() {
        return None;
    }

    let text = String::from_utf8(output.stdout).ok()?;
    let trimmed = text.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn status_line_level(status: Option<&ServerStatusResult>) -> StatusLineLevel {
    let Some(result) = status else {
        return StatusLineLevel::Error;
    };

    if result.service_status == "unknown" {
        return StatusLineLevel::Error;
    }

    if result.service_status != "active" {
        return StatusLineLevel::Warning;
    }

    if result.error.is_some() {
        return StatusLineLevel::Warning;
    }

    match result.app_status.as_ref().map(|app| app.state) {
        Some(AppState::Running | AppState::Idle) => StatusLineLevel::Success,
        Some(AppState::Deploying | AppState::Stopped) => StatusLineLevel::Warning,
        Some(AppState::Error) => StatusLineLevel::Error,
        None => StatusLineLevel::Warning,
    }
}

fn status_columns(status: Option<&ServerStatusResult>) -> (String, String, String) {
    match status {
        Some(result) => {
            if let Some(app_status) = &result.app_status {
                let healthy = app_status
                    .instances
                    .iter()
                    .filter(|i| {
                        i.state == InstanceState::Healthy || i.state == InstanceState::Ready
                    })
                    .count();
                let state = format!(
                    "{} ({}/{})",
                    app_status.state,
                    healthy,
                    app_status.instances.len()
                );
                return (
                    result.service_status.clone(),
                    app_status.version.clone(),
                    state,
                );
            }

            if result.service_status == "active" {
                let app_state = if let Some(err) = &result.error {
                    format!("unknown ({})", truncate_str(err, 32))
                } else {
                    "not deployed".to_string()
                };
                ("active".to_string(), "-".to_string(), app_state)
            } else {
                let app_state = if let Some(err) = &result.error {
                    format!("unknown ({})", truncate_str(err, 32))
                } else {
                    "unavailable".to_string()
                };
                (result.service_status.clone(), "-".to_string(), app_state)
            }
        }
        None => (
            "unknown".to_string(),
            "-".to_string(),
            "not queried".to_string(),
        ),
    }
}

/// Truncate a string to a maximum length
fn truncate_str(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        format!("{}...", &s[..max_len - 3])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ServerConfig, ServerEntry};
    use tako_core::InstanceStatus;

    #[test]
    fn resolve_env_server_names_uses_single_production_server_fallback() {
        let tako_config = TakoToml::default();
        let mut servers = ServersToml::default();
        servers.servers.insert(
            "solo".to_string(),
            ServerEntry {
                host: "127.0.0.1".to_string(),
                port: 22,
                description: None,
            },
        );

        let names = resolve_env_server_names_for_status(&tako_config, &servers, "production");
        assert_eq!(names, vec!["solo".to_string()]);
    }

    #[test]
    fn resolve_status_environment_names_includes_production_when_only_server_mapping_exists() {
        let mut tako_config = TakoToml::default();
        tako_config.servers.insert(
            "tako-server".to_string(),
            ServerConfig {
                env: "production".to_string(),
                instances: None,
                port: None,
                idle_timeout: None,
            },
        );

        let mut servers = ServersToml::default();
        servers.servers.insert(
            "tako-server".to_string(),
            ServerEntry {
                host: "127.0.0.1".to_string(),
                port: 2222,
                description: None,
            },
        );

        let envs = resolve_status_environment_names(&tako_config, &servers);
        assert_eq!(envs, vec!["production".to_string()]);
    }

    #[test]
    fn resolve_status_environment_names_includes_production_for_single_server_fallback() {
        let tako_config = TakoToml::default();
        let mut servers = ServersToml::default();
        servers.servers.insert(
            "solo".to_string(),
            ServerEntry {
                host: "127.0.0.1".to_string(),
                port: 22,
                description: None,
            },
        );

        let envs = resolve_status_environment_names(&tako_config, &servers);
        assert_eq!(envs, vec!["production".to_string()]);
    }

    #[test]
    fn display_environment_names_for_status_excludes_development() {
        let mut tako_config = TakoToml::default();
        tako_config.envs.insert(
            "development".to_string(),
            crate::config::EnvConfig::default(),
        );
        tako_config
            .envs
            .insert("staging".to_string(), crate::config::EnvConfig::default());
        let servers = ServersToml::default();

        let envs = display_environment_names_for_status(&tako_config, &servers);
        assert_eq!(envs, vec!["staging".to_string()]);
    }

    #[test]
    fn status_columns_show_service_build_and_state() {
        let app_status = AppStatus {
            name: "demo".to_string(),
            version: "v123".to_string(),
            instances: vec![
                InstanceStatus {
                    id: 1,
                    state: InstanceState::Healthy,
                    port: 3001,
                    pid: Some(111),
                    uptime_secs: 10,
                    requests_total: 0,
                },
                InstanceStatus {
                    id: 2,
                    state: InstanceState::Starting,
                    port: 3002,
                    pid: Some(112),
                    uptime_secs: 1,
                    requests_total: 0,
                },
            ],
            state: AppState::Running,
            last_error: None,
        };
        let result = ServerStatusResult {
            service_status: "active".to_string(),
            server_version: Some("0.1.0".to_string()),
            app_status: Some(app_status),
            deployed_at_unix_secs: Some(1_707_386_400),
            error: None,
        };

        let (service, build, state) = status_columns(Some(&result));
        assert_eq!(service, "active");
        assert_eq!(build, "v123");
        assert_eq!(state, "running (1/2)");
    }

    #[test]
    fn format_server_status_heading_includes_service_and_server_version() {
        let result = ServerStatusResult {
            service_status: "active".to_string(),
            server_version: Some("0.1.0".to_string()),
            app_status: None,
            deployed_at_unix_secs: None,
            error: None,
        };

        let heading = format_server_status_heading("s1", Some(&result));
        assert_eq!(heading, "s1 (v0.1.0) up");
    }

    #[test]
    fn format_server_status_details_outputs_instances_build_and_deployed_lines() {
        let result = ServerStatusResult {
            service_status: "active".to_string(),
            server_version: Some("0.1.0".to_string()),
            app_status: Some(AppStatus {
                name: "demo".to_string(),
                version: "v123".to_string(),
                instances: vec![],
                state: AppState::Idle,
                last_error: None,
            }),
            deployed_at_unix_secs: Some(0),
            error: None,
        };

        let lines = format_server_status_details(Some(&result));
        assert_eq!(lines[0], "instances: 0/0 (idle)");
        assert_eq!(lines[1], "build: v123");
        assert!(!lines[2].trim().is_empty());
        assert!(lines[2].starts_with("deployed: "));
    }

    #[test]
    fn status_line_level_marks_active_running_as_success() {
        let result = ServerStatusResult {
            service_status: "active".to_string(),
            server_version: Some("0.1.0".to_string()),
            app_status: Some(AppStatus {
                name: "demo".to_string(),
                version: "v123".to_string(),
                instances: vec![],
                state: AppState::Running,
                last_error: None,
            }),
            deployed_at_unix_secs: Some(1_707_386_400),
            error: None,
        };

        assert_eq!(status_line_level(Some(&result)), StatusLineLevel::Success);
    }

    #[test]
    fn status_line_level_marks_inactive_or_errors_as_non_success() {
        let inactive = ServerStatusResult {
            service_status: "inactive".to_string(),
            server_version: Some("0.1.0".to_string()),
            app_status: None,
            deployed_at_unix_secs: None,
            error: None,
        };
        let errored = ServerStatusResult {
            service_status: "active".to_string(),
            server_version: Some("0.1.0".to_string()),
            app_status: None,
            deployed_at_unix_secs: None,
            error: Some("SSH connect failed".to_string()),
        };

        assert_eq!(status_line_level(Some(&inactive)), StatusLineLevel::Warning);
        assert_eq!(status_line_level(Some(&errored)), StatusLineLevel::Warning);
        assert_eq!(status_line_level(None), StatusLineLevel::Error);
    }

    #[test]
    fn truncate_str_adds_ellipsis_for_long_strings() {
        assert_eq!(truncate_str("abcdef", 5), "ab...");
    }

    #[test]
    fn collect_status_target_names_is_sorted_and_deduplicated() {
        let env_map = vec![
            (
                "production".to_string(),
                vec!["b".to_string(), "a".to_string(), "a".to_string()],
            ),
            (
                "staging".to_string(),
                vec!["c".to_string(), "b".to_string()],
            ),
        ];
        let names = collect_status_target_names(&env_map);
        assert_eq!(
            names,
            vec!["a".to_string(), "b".to_string(), "c".to_string()]
        );
    }

    #[test]
    fn normalize_server_version_strips_binary_prefix() {
        assert_eq!(
            normalize_server_version("tako-server 0.1.0".to_string()),
            "0.1.0"
        );
        assert_eq!(normalize_server_version("0.2.0".to_string()), "0.2.0");
    }

    #[test]
    fn format_unix_timestamp_with_offset_formats_in_requested_timezone() {
        let utc = format_unix_timestamp_with_offset(0, UtcOffset::UTC).unwrap();
        assert_eq!(utc, "1970-01-01 00:00:00");

        let plus_two = UtcOffset::from_hms(2, 0, 0).unwrap();
        let plus_two_formatted = format_unix_timestamp_with_offset(0, plus_two).unwrap();
        assert_eq!(plus_two_formatted, "1970-01-01 02:00:00");
    }

    #[test]
    fn format_unix_timestamp_local_uses_locale_or_fallback() {
        let rendered = format_unix_timestamp_local(0).expect("formatted timestamp");
        assert!(!rendered.is_empty());
    }

    #[test]
    fn display_server_version_prefixes_v_when_missing() {
        assert_eq!(display_server_version("0.1.0"), "v0.1.0");
        assert_eq!(display_server_version("v0.2.0"), "v0.2.0");
    }

    #[test]
    fn display_service_state_maps_active_and_inactive_to_simple_terms() {
        assert_eq!(display_service_state("active"), "up");
        assert_eq!(display_service_state("inactive"), "down");
        assert_eq!(display_service_state("failed"), "down");
        assert_eq!(display_service_state("unknown"), "unknown");
    }

    #[test]
    fn format_instance_summary_uses_state_when_app_missing() {
        assert_eq!(
            format_instance_summary(None, "not deployed"),
            "-/- (not deployed)"
        );
    }

    #[test]
    fn shell_single_quote_escapes_single_quotes() {
        assert_eq!(shell_single_quote("a'b"), "'a'\\''b'");
    }
}

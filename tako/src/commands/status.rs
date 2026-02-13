use std::collections::HashMap;
use std::process::Command;
use std::sync::OnceLock;

use crate::commands::server;
use crate::config::ServersToml;
use crate::output;
use crate::ssh::{SshClient, SshConfig};
use tako_core::{AppState, AppStatus, InstanceState, Response};
use time::{OffsetDateTime, UtcOffset};

/// Server status result from querying a remote server
#[derive(Debug, Clone)]
struct ServerStatusResult {
    service_status: String,
    server_version: Option<String>,
    app_status: Option<AppStatus>,
    deployed_at_unix_secs: Option<i64>,
    error: Option<String>,
}

/// Global app status discovered on a specific server.
#[derive(Debug)]
struct GlobalAppStatusResult {
    app_name: String,
    env_name: String,
    status: ServerStatusResult,
}

/// Global server status result with all apps discovered on a server.
#[derive(Debug)]
struct GlobalServerStatusResult {
    service_status: String,
    server_version: Option<String>,
    apps: Vec<GlobalAppStatusResult>,
    error: Option<String>,
}

static LOCAL_OFFSET: OnceLock<UtcOffset> = OnceLock::new();
const SERVER_SEPARATOR: &str = "────────────────────────────────────────";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StatusLineLevel {
    Success,
    Warning,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AppStateTone {
    Success,
    Warning,
    Error,
    Muted,
}

pub async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut servers = ServersToml::load()?;

    if servers.is_empty()
        && server::prompt_to_add_server(
            "No servers configured yet. Add one now to see deployment status.",
        )
        .await?
        .is_some()
    {
        servers = ServersToml::load()?;
    }

    run_global_status(&servers).await
}

async fn run_global_status(servers: &ServersToml) -> Result<(), Box<dyn std::error::Error>> {
    if servers.is_empty() {
        output::warning("No servers configured.");
        output::muted("Run 'tako servers add --name <name> <host>' to add one.");
        return Ok(());
    }

    let server_names = sorted_server_names(servers);

    let mut server_results = collect_global_status_results(servers, &server_names).await?;
    render_global_status(&server_names, &mut server_results);

    Ok(())
}

fn sorted_server_names(servers: &ServersToml) -> Vec<String> {
    let mut server_names: Vec<String> = servers.names().into_iter().map(str::to_string).collect();
    server_names.sort();
    server_names
}

async fn collect_global_status_results(
    servers: &ServersToml,
    server_names: &[String],
) -> Result<HashMap<String, GlobalServerStatusResult>, Box<dyn std::error::Error>> {
    let mut tasks = spawn_global_status_tasks(servers, server_names);
    let mut server_results: HashMap<String, GlobalServerStatusResult> = HashMap::new();

    for server_name in server_names {
        let Some(handle) = tasks.remove(server_name.as_str()) else {
            continue;
        };

        let status = output::with_spinner_async(
            format!("Loading {}...", server_name),
            collect_global_status_task(handle),
        )
        .await?;
        server_results.insert(server_name.clone(), status);
    }

    Ok(server_results)
}

fn render_global_status(
    server_names: &[String],
    server_results: &mut HashMap<String, GlobalServerStatusResult>,
) {
    let mut first_server = true;

    for server_name in server_names {
        let Some(global) = server_results.remove(server_name.as_str()) else {
            continue;
        };

        if !first_server {
            output_server_separator();
        }
        first_server = false;

        let server_status = ServerStatusResult {
            service_status: global.service_status.clone(),
            server_version: global.server_version.clone(),
            app_status: None,
            deployed_at_unix_secs: None,
            error: global.error.clone(),
        };

        let heading = format_server_status_heading(server_name, Some(&server_status));
        let summary_level = server_summary_line_level(Some(&server_status));
        output_status_heading(summary_level, &heading);

        if let Some(err) = server_status.error.as_deref() {
            output::muted(err);
        }

        if global.apps.is_empty() {
            output::muted("No deployed apps.");
            continue;
        }

        let mut apps = global.apps;
        sort_global_apps(&mut apps);
        for app in &apps {
            let status = Some(&app.status);
            let heading = format_deployed_app_heading(app);
            let details = format_server_status_details(status);
            output_app_status_block(&heading, status, &details);
        }
    }
}

fn spawn_global_status_tasks(
    servers: &ServersToml,
    server_names: &[String],
) -> HashMap<String, tokio::task::JoinHandle<GlobalServerStatusResult>> {
    let mut tasks = HashMap::new();

    for server_name in server_names {
        let Some(entry) = servers.get(server_name.as_str()) else {
            continue;
        };

        let name = server_name.clone();
        let host = entry.host.clone();
        let port = entry.port;
        let handle =
            tokio::spawn(async move { query_global_server_status(&name, &host, port).await });
        tasks.insert(server_name.clone(), handle);
    }

    tasks
}

async fn collect_global_status_task(
    handle: tokio::task::JoinHandle<GlobalServerStatusResult>,
) -> GlobalServerStatusResult {
    match handle.await {
        Ok(status) => status,
        Err(err) => GlobalServerStatusResult {
            service_status: "unknown".to_string(),
            server_version: None,
            apps: Vec::new(),
            error: Some(format!("Status task failed: {}", err)),
        },
    }
}

fn sort_global_apps(apps: &mut [GlobalAppStatusResult]) {
    apps.sort_by(|a, b| {
        let a_version = a
            .status
            .app_status
            .as_ref()
            .map(|s| s.version.as_str())
            .unwrap_or_default();
        let b_version = b
            .status
            .app_status
            .as_ref()
            .map(|s| s.version.as_str())
            .unwrap_or_default();
        (&a.app_name, &a.env_name, a_version).cmp(&(&b.app_name, &b.env_name, b_version))
    });
}

async fn query_global_server_status(
    server_name: &str,
    host: &str,
    port: u16,
) -> GlobalServerStatusResult {
    let config = SshConfig::from_server(host, port);
    let mut client = SshClient::new(config);

    if let Err(e) = client.connect().await {
        return GlobalServerStatusResult {
            service_status: "unknown".to_string(),
            server_version: None,
            apps: Vec::new(),
            error: Some(format!("SSH connect failed: {}", e)),
        };
    }

    let server_version = client
        .tako_version()
        .await
        .ok()
        .flatten()
        .map(normalize_server_version);

    let service_status = match client.tako_status().await {
        Ok(status) => status,
        Err(e) => {
            let _ = client.disconnect().await;
            return GlobalServerStatusResult {
                service_status: "unknown".to_string(),
                server_version,
                apps: Vec::new(),
                error: Some(format!("Failed to check service: {}", e)),
            };
        }
    };

    let mut effective_service_status = service_status.clone();
    let mut apps = Vec::new();
    let mut error = None;

    if service_status == "active" || service_status == "unknown" {
        match client.tako_list_apps().await {
            Ok(response) => match parse_list_apps_response(response) {
                Ok(app_names) => {
                    if service_status == "unknown" {
                        // Non-systemd hosts can report "unknown" even when the
                        // management socket is healthy and serving commands.
                        effective_service_status = "active".to_string();
                    }

                    for app_name in app_names {
                        let status = query_connected_app_status(
                            &mut client,
                            &effective_service_status,
                            server_version.clone(),
                            &app_name,
                        )
                        .await;
                        for mut build_status in expand_status_by_running_builds(status) {
                            let app_version = build_status
                                .app_status
                                .as_ref()
                                .map(|app| app.version.clone());

                            let env_name = if let Some(app_version) = app_version {
                                build_status.deployed_at_unix_secs =
                                    query_app_deployed_at_unix_secs(
                                        &client,
                                        &app_name,
                                        &app_version,
                                    )
                                    .await;

                                query_remote_app_env_for_server(
                                    &client,
                                    &app_name,
                                    &app_version,
                                    server_name,
                                )
                                .await
                                .unwrap_or_else(|| "unknown".to_string())
                            } else {
                                "unknown".to_string()
                            };

                            apps.push(GlobalAppStatusResult {
                                app_name: app_name.clone(),
                                env_name,
                                status: build_status,
                            });
                        }
                    }
                }
                Err(e) => {
                    error = Some(e);
                }
            },
            Err(e) => {
                error = Some(format!("Socket query failed: {}", e));
            }
        }
    }

    let _ = client.disconnect().await;

    GlobalServerStatusResult {
        service_status: effective_service_status,
        server_version,
        apps,
        error,
    }
}

async fn query_connected_app_status(
    client: &mut SshClient,
    service_status: &str,
    server_version: Option<String>,
    app_name: &str,
) -> ServerStatusResult {
    let mut app_status = None;
    let mut deployed_at_unix_secs = None;
    let mut error = None;

    if service_status == "active" {
        match client.tako_app_status(app_name).await {
            Ok(Response::Ok { data }) => match serde_json::from_value::<AppStatus>(data) {
                Ok(status) => {
                    deployed_at_unix_secs =
                        query_app_deployed_at_unix_secs(client, app_name, &status.version).await;
                    app_status = Some(status);
                }
                Err(e) => {
                    error = Some(format!("Failed to parse app status: {}", e));
                }
            },
            Ok(Response::Error { message }) => {
                if !message.contains("not found") {
                    error = Some(message);
                }
            }
            Err(e) => {
                error = Some(format!("Socket query failed: {}", e));
            }
        }
    }

    ServerStatusResult {
        service_status: service_status.to_string(),
        server_version,
        app_status,
        deployed_at_unix_secs,
        error,
    }
}

fn expand_status_by_running_builds(status: ServerStatusResult) -> Vec<ServerStatusResult> {
    let Some(app_status) = status.app_status.as_ref() else {
        return vec![status];
    };

    if app_status.builds.is_empty() {
        return vec![status];
    }

    let mut per_build = Vec::new();
    for build in &app_status.builds {
        if build.instances.is_empty() {
            continue;
        }

        per_build.push(ServerStatusResult {
            service_status: status.service_status.clone(),
            server_version: status.server_version.clone(),
            app_status: Some(AppStatus {
                name: app_status.name.clone(),
                version: build.version.clone(),
                instances: build.instances.clone(),
                builds: Vec::new(),
                state: build.state,
                last_error: app_status.last_error.clone(),
            }),
            deployed_at_unix_secs: status.deployed_at_unix_secs,
            error: status.error.clone(),
        });
    }

    if per_build.is_empty() {
        vec![status]
    } else {
        per_build
    }
}

fn parse_list_apps_response(response: Response) -> Result<Vec<String>, String> {
    match response {
        Response::Ok { data } => {
            let mut names = data
                .get("apps")
                .and_then(|value| value.as_array())
                .map(|apps| {
                    apps.iter()
                        .filter_map(|app| {
                            app.get("name")
                                .and_then(|name| name.as_str())
                                .map(|name| name.to_string())
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            names.sort();
            names.dedup();
            Ok(names)
        }
        Response::Error { message } => Err(format!("tako-server error (list): {}", message)),
    }
}

async fn query_remote_app_env_for_server(
    client: &SshClient,
    app_name: &str,
    version: &str,
    server_name: &str,
) -> Option<String> {
    let release_toml = format!("/opt/tako/apps/{}/releases/{}/tako.toml", app_name, version);
    let quoted = shell_single_quote(&release_toml);
    let cmd = format!("if [ -f {path} ]; then cat {path}; fi", path = quoted);
    let output = client.exec(&cmd).await.ok()?;
    let content = output.stdout;
    if content.trim().is_empty() {
        return None;
    }
    parse_server_env_from_tako_toml(&content, server_name)
}

fn parse_server_env_from_tako_toml(content: &str, server_name: &str) -> Option<String> {
    let value: toml::Value = toml::from_str(content).ok()?;
    let servers = value.get("servers")?.as_table()?;

    if let Some(entry) = servers.get(server_name)
        && let Some(table) = entry.as_table()
        && let Some(env) = table.get("env").and_then(|v| v.as_str())
    {
        return Some(env.to_string());
    }

    // Fallback: if the file has exactly one server mapping, use it.
    let mut envs = Vec::new();
    for value in servers.values() {
        if let Some(table) = value.as_table()
            && let Some(env) = table.get("env").and_then(|v| v.as_str())
        {
            envs.push(env.to_string());
        }
    }
    envs.sort();
    envs.dedup();
    if envs.len() == 1 {
        envs.into_iter().next()
    } else {
        None
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
    let (_, build, _) = status_columns(status);
    let deployed_at = status
        .and_then(|result| result.deployed_at_unix_secs)
        .and_then(format_unix_timestamp_local)
        .unwrap_or_else(|| "-".to_string());
    [
        format!("instances: {}", format_instance_summary(status)),
        format!("build: {}", build),
        format!("deployed: {}", deployed_at),
    ]
}

fn format_deployed_app_heading(app: &GlobalAppStatusResult) -> String {
    format!("{} ({})", app.app_name, app.env_name)
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

#[cfg(test)]
fn app_status_heading_line(heading: &str, state: &str) -> String {
    format!("  ┌ {} {}", heading, state)
}

#[cfg(test)]
fn app_detail_line(detail: &str, is_last: bool) -> String {
    let connector = if is_last { "└" } else { "│" };
    format!("  {} {}", connector, detail)
}

fn server_separator_line() -> &'static str {
    SERVER_SEPARATOR
}

fn output_server_separator() {
    println!("{}", output::brand_muted(server_separator_line()));
}

fn output_app_status_block(heading: &str, status: Option<&ServerStatusResult>, details: &[String]) {
    let (state_label, tone) = app_state_label_and_tone(status);
    output_app_status_heading(heading, &state_label, tone);
    for (idx, detail) in details.iter().enumerate() {
        let is_last = idx + 1 == details.len();
        output_app_detail_line(detail, is_last);
    }
}

fn output_app_detail_line(detail: &str, is_last: bool) {
    let connector = if is_last { "└" } else { "│" };
    println!(
        "  {} {}",
        output::brand_accent(connector).bold(),
        output::brand_muted(detail)
    );
}

fn output_app_status_heading(heading: &str, state_label: &str, tone: AppStateTone) {
    let state = match tone {
        AppStateTone::Success => output::brand_success(state_label),
        AppStateTone::Warning => output::brand_warning(state_label),
        AppStateTone::Error => output::brand_error(state_label),
        AppStateTone::Muted => output::brand_muted(state_label),
    };
    println!(
        "  {} {} {}",
        output::brand_accent("┌").bold(),
        output::brand_fg(heading).bold(),
        state
    );
}

fn output_status_heading(level: StatusLineLevel, heading: &str) {
    match level {
        StatusLineLevel::Success => output::success(heading),
        StatusLineLevel::Warning => output::warning(heading),
        StatusLineLevel::Error => output::error(heading),
    }
}

fn format_instance_summary(status: Option<&ServerStatusResult>) -> String {
    if let Some(result) = status
        && let Some(app_status) = &result.app_status
    {
        let healthy = app_status
            .instances
            .iter()
            .filter(|i| i.state == InstanceState::Healthy || i.state == InstanceState::Ready)
            .count();
        return format!("{}/{}", healthy, app_status.instances.len());
    }

    "-/-".to_string()
}

fn app_state_label_and_tone(status: Option<&ServerStatusResult>) -> (String, AppStateTone) {
    let Some(result) = status else {
        return ("unknown".to_string(), AppStateTone::Warning);
    };

    if let Some(app_status) = &result.app_status {
        return match app_status.state {
            AppState::Running => ("running".to_string(), AppStateTone::Success),
            AppState::Idle => ("idle".to_string(), AppStateTone::Muted),
            AppState::Deploying => ("deploying".to_string(), AppStateTone::Warning),
            AppState::Stopped => ("stopped".to_string(), AppStateTone::Warning),
            AppState::Error => ("error".to_string(), AppStateTone::Error),
        };
    }

    if result.service_status == "active" {
        if result.error.is_some() {
            ("unknown".to_string(), AppStateTone::Warning)
        } else {
            ("not deployed".to_string(), AppStateTone::Muted)
        }
    } else if result.service_status == "unknown" {
        ("unknown".to_string(), AppStateTone::Warning)
    } else {
        ("unavailable".to_string(), AppStateTone::Warning)
    }
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

#[cfg(test)]
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

fn server_summary_line_level(status: Option<&ServerStatusResult>) -> StatusLineLevel {
    let Some(result) = status else {
        return StatusLineLevel::Error;
    };

    if result.service_status == "unknown" {
        return StatusLineLevel::Error;
    }

    if result.service_status == "active" {
        if result.error.is_some() {
            return StatusLineLevel::Warning;
        }
        return StatusLineLevel::Success;
    }

    StatusLineLevel::Warning
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
    use tako_core::{BuildStatus, InstanceStatus};

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
            builds: vec![],
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
                builds: vec![],
                state: AppState::Idle,
                last_error: None,
            }),
            deployed_at_unix_secs: Some(0),
            error: None,
        };

        let lines = format_server_status_details(Some(&result));
        assert_eq!(lines[0], "instances: 0/0");
        assert_eq!(lines[1], "build: v123");
        assert!(!lines[2].trim().is_empty());
        assert!(lines[2].starts_with("deployed: "));
    }

    #[test]
    fn format_deployed_app_heading_compacts_app_and_env() {
        let app = GlobalAppStatusResult {
            app_name: "bun-example".to_string(),
            env_name: "production".to_string(),
            status: ServerStatusResult {
                service_status: "active".to_string(),
                server_version: Some("0.1.0".to_string()),
                app_status: Some(AppStatus {
                    name: "demo".to_string(),
                    version: "v123".to_string(),
                    instances: vec![],
                    builds: vec![],
                    state: AppState::Idle,
                    last_error: None,
                }),
                deployed_at_unix_secs: Some(0),
                error: None,
            },
        };

        assert_eq!(
            format_deployed_app_heading(&app),
            "bun-example (production)"
        );
    }

    #[test]
    fn format_server_status_details_outputs_instance_state() {
        let result = ServerStatusResult {
            service_status: "active".to_string(),
            server_version: Some("0.1.0".to_string()),
            app_status: Some(AppStatus {
                name: "demo".to_string(),
                version: "v123".to_string(),
                instances: vec![],
                builds: vec![],
                state: AppState::Idle,
                last_error: None,
            }),
            deployed_at_unix_secs: Some(0),
            error: None,
        };

        let details = format_server_status_details(Some(&result));
        assert_eq!(details[0], "instances: 0/0");
    }

    #[test]
    fn sort_global_apps_orders_by_app_then_env() {
        let status = ServerStatusResult {
            service_status: "active".to_string(),
            server_version: None,
            app_status: None,
            deployed_at_unix_secs: None,
            error: None,
        };
        let mut apps = vec![
            GlobalAppStatusResult {
                app_name: "web".to_string(),
                env_name: "staging".to_string(),
                status: status.clone(),
            },
            GlobalAppStatusResult {
                app_name: "api".to_string(),
                env_name: "production".to_string(),
                status: status.clone(),
            },
            GlobalAppStatusResult {
                app_name: "web".to_string(),
                env_name: "production".to_string(),
                status,
            },
        ];

        sort_global_apps(&mut apps);

        let ordered: Vec<(String, String)> = apps
            .into_iter()
            .map(|entry| (entry.app_name, entry.env_name))
            .collect();
        assert_eq!(
            ordered,
            vec![
                ("api".to_string(), "production".to_string()),
                ("web".to_string(), "production".to_string()),
                ("web".to_string(), "staging".to_string()),
            ]
        );
    }

    #[test]
    fn expand_status_by_running_builds_returns_one_entry_per_build() {
        let status = ServerStatusResult {
            service_status: "active".to_string(),
            server_version: Some("0.1.0".to_string()),
            app_status: Some(AppStatus {
                name: "demo".to_string(),
                version: "v2".to_string(),
                instances: vec![],
                builds: vec![
                    BuildStatus {
                        version: "v1".to_string(),
                        state: AppState::Running,
                        instances: vec![InstanceStatus {
                            id: 1,
                            state: InstanceState::Healthy,
                            port: 3001,
                            pid: Some(111),
                            uptime_secs: 10,
                            requests_total: 0,
                        }],
                    },
                    BuildStatus {
                        version: "v2".to_string(),
                        state: AppState::Running,
                        instances: vec![InstanceStatus {
                            id: 2,
                            state: InstanceState::Healthy,
                            port: 3002,
                            pid: Some(222),
                            uptime_secs: 12,
                            requests_total: 0,
                        }],
                    },
                ],
                state: AppState::Deploying,
                last_error: None,
            }),
            deployed_at_unix_secs: None,
            error: None,
        };

        let expanded = expand_status_by_running_builds(status);
        let versions: Vec<&str> = expanded
            .iter()
            .filter_map(|entry| entry.app_status.as_ref().map(|app| app.version.as_str()))
            .collect();
        assert_eq!(expanded.len(), 2);
        assert!(versions.contains(&"v1"));
        assert!(versions.contains(&"v2"));
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
                builds: vec![],
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
    fn server_summary_line_level_marks_active_without_errors_as_success() {
        let status = ServerStatusResult {
            service_status: "active".to_string(),
            server_version: Some("0.1.0".to_string()),
            app_status: None,
            deployed_at_unix_secs: None,
            error: None,
        };
        assert_eq!(
            server_summary_line_level(Some(&status)),
            StatusLineLevel::Success
        );
    }

    #[test]
    fn truncate_str_adds_ellipsis_for_long_strings() {
        assert_eq!(truncate_str("abcdef", 5), "ab...");
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
    fn format_instance_summary_uses_dash_when_app_missing() {
        assert_eq!(format_instance_summary(None), "-/-");
    }

    #[test]
    fn shell_single_quote_escapes_single_quotes() {
        assert_eq!(shell_single_quote("a'b"), "'a'\\''b'");
    }

    #[test]
    fn parse_list_apps_response_extracts_unique_sorted_names() {
        let response = Response::Ok {
            data: serde_json::json!({
                "apps": [
                    { "name": "web" },
                    { "name": "api" },
                    { "name": "api" }
                ]
            }),
        };

        let names = parse_list_apps_response(response).unwrap();
        assert_eq!(names, vec!["api".to_string(), "web".to_string()]);
    }

    #[test]
    fn parse_server_env_from_tako_toml_prefers_matching_server_name() {
        let content = r#"
[servers]
instances = 0

[servers.eu]
env = "production"

[servers.us]
env = "staging"
"#;
        let env = parse_server_env_from_tako_toml(content, "us");
        assert_eq!(env.as_deref(), Some("staging"));
    }

    #[test]
    fn parse_server_env_from_tako_toml_falls_back_to_single_mapping() {
        let content = r#"
[servers.only]
env = "production"
"#;
        let env = parse_server_env_from_tako_toml(content, "missing");
        assert_eq!(env.as_deref(), Some("production"));
    }

    #[test]
    fn parse_server_env_from_tako_toml_returns_none_for_ambiguous_mappings() {
        let content = r#"
[servers.first]
env = "production"

[servers.second]
env = "staging"
"#;
        assert!(parse_server_env_from_tako_toml(content, "missing").is_none());
    }

    #[test]
    fn app_status_heading_line_has_indent_and_state() {
        assert_eq!(
            app_status_heading_line("bun-example (production)", "running"),
            "  ┌ bun-example (production) running"
        );
        assert_eq!(
            app_status_heading_line("bun-example (production)", "deploying"),
            "  ┌ bun-example (production) deploying"
        );
        assert_eq!(
            app_status_heading_line("bun-example (production)", "error"),
            "  ┌ bun-example (production) error"
        );
    }

    #[test]
    fn app_detail_line_has_consistent_indent() {
        assert_eq!(
            app_detail_line("instances: 1/1 (running)", false),
            "  │ instances: 1/1 (running)"
        );
        assert_eq!(
            app_detail_line("deployed: Fri Feb 13 10:22:20 2026", true),
            "  └ deployed: Fri Feb 13 10:22:20 2026"
        );
    }

    #[test]
    fn server_separator_line_is_visible_horizontal_rule() {
        assert_eq!(
            server_separator_line(),
            "────────────────────────────────────────"
        );
    }

    #[test]
    fn app_state_label_and_tone_marks_idle_as_muted() {
        let status = ServerStatusResult {
            service_status: "active".to_string(),
            server_version: Some("0.1.0".to_string()),
            app_status: Some(AppStatus {
                name: "demo".to_string(),
                version: "v123".to_string(),
                instances: vec![],
                builds: vec![],
                state: AppState::Idle,
                last_error: None,
            }),
            deployed_at_unix_secs: None,
            error: None,
        };

        let (label, tone) = app_state_label_and_tone(Some(&status));
        assert_eq!(label, "idle");
        assert_eq!(tone, AppStateTone::Muted);
    }

    #[test]
    fn app_state_label_and_tone_marks_running_as_success() {
        let status = ServerStatusResult {
            service_status: "active".to_string(),
            server_version: Some("0.1.0".to_string()),
            app_status: Some(AppStatus {
                name: "demo".to_string(),
                version: "v123".to_string(),
                instances: vec![],
                builds: vec![],
                state: AppState::Running,
                last_error: None,
            }),
            deployed_at_unix_secs: None,
            error: None,
        };

        let (label, tone) = app_state_label_and_tone(Some(&status));
        assert_eq!(label, "running");
        assert_eq!(tone, AppStateTone::Success);
    }

    #[tokio::test]
    async fn collect_global_status_task_maps_join_error_to_unknown_status() {
        let handle: tokio::task::JoinHandle<GlobalServerStatusResult> = tokio::spawn(async move {
            panic!("boom");
        });

        let result = collect_global_status_task(handle).await;

        assert_eq!(result.service_status, "unknown");
        assert!(result.server_version.is_none());
        assert!(result.apps.is_empty());
        assert!(
            result
                .error
                .as_deref()
                .unwrap_or_default()
                .contains("Status task failed")
        );
    }
}

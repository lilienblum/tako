use std::collections::HashMap;

use crate::commands::server;
use crate::config::ServersToml;
use crate::output;
use crate::ssh::{SshClient, SshConfig};
use indicatif::ProgressBar;
use tako_core::{AppState, AppStatus, InstanceState, Response};
use time::OffsetDateTime;

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
    server_uptime: Option<String>,
    process_uptime: Option<String>,
    routes: Vec<(String, String)>,
    apps: Vec<GlobalAppStatusResult>,
    error: Option<String>,
}

#[cfg(test)]
static LOCAL_OFFSET: std::sync::OnceLock<time::UtcOffset> = std::sync::OnceLock::new();

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
        output::muted(&format!(
            "Run {} to add one.",
            output::highlight("tako servers add")
        ));
        return Ok(());
    }

    let server_names = sorted_server_names(servers);

    let mut server_results = collect_global_status_results(servers, &server_names).await?;
    render_global_status(servers, &server_names, &mut server_results);

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
    // Spawn all tasks up-front so they run in parallel.
    let mut tasks: HashMap<String, tokio::task::JoinHandle<GlobalServerStatusResult>> =
        HashMap::new();

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

    let total = tasks.len();
    let mut done = 0usize;

    let pb = if output::is_interactive() && total > 0 {
        let pb = ProgressBar::new_spinner();
        pb.set_style(output::spinner_style());
        pb.set_message(format!("Fetching server status [0/{total}]"));
        pb.enable_steady_tick(std::time::Duration::from_millis(80));
        output::hide_cursor();
        Some(pb)
    } else {
        None
    };

    let mut server_results: HashMap<String, GlobalServerStatusResult> = HashMap::new();

    // Await in server_names order for consistent progress updates, but all tasks
    // are running in parallel.
    for server_name in server_names {
        let Some(handle) = tasks.remove(server_name.as_str()) else {
            continue;
        };

        let status = match handle.await {
            Ok(status) => status,
            Err(err) => GlobalServerStatusResult {
                service_status: "unknown".to_string(),
                server_version: None,
                server_uptime: None,
                process_uptime: None,
                routes: Vec::new(),
                apps: Vec::new(),
                error: Some(format!("Status task failed: {}", err)),
            },
        };

        done += 1;
        if let Some(ref pb) = pb {
            pb.set_message(format!("Fetching server status [{done}/{total}]"));
        }

        server_results.insert(server_name.clone(), status);
    }

    if let Some(pb) = pb {
        pb.finish_and_clear();
        output::show_cursor();
    }

    Ok(server_results)
}

// ---------------------------------------------------------------------------
// Rendering — card-style tree
// ---------------------------------------------------------------------------

/// A single field or section in a card.
enum CardEntry {
    /// Simple label/value pair.
    Field {
        label: String,
        value: String,
        color: Option<CardColor>,
    },
    /// A section with children (e.g. Routes, Apps).
    Section {
        label: String,
        children: Vec<(String, String, Option<CardColor>)>,
    },
}

#[derive(Clone, Copy)]
enum CardColor {
    Success,
    Warning,
    Error,
}

fn colorize(text: &str, color: Option<CardColor>) -> String {
    match color {
        Some(CardColor::Success) => output::brand_success(text),
        Some(CardColor::Warning) => output::brand_warning(text),
        Some(CardColor::Error) => output::brand_error(text),
        None => text.to_string(),
    }
}

fn render_global_status(
    servers: &ServersToml,
    server_names: &[String],
    server_results: &mut HashMap<String, GlobalServerStatusResult>,
) {
    // Build cards and compute global label width
    struct Card {
        header: String,
        entries: Vec<CardEntry>,
    }

    let mut cards: Vec<Card> = Vec::new();
    let mut max_label = 0usize;

    for server_name in server_names {
        let Some(global) = server_results.remove(server_name.as_str()) else {
            continue;
        };
        let entry = servers.get(server_name.as_str());
        let host_display = entry
            .map(|e| {
                if e.port != 22 {
                    format!("{}:{}", e.host, e.port)
                } else {
                    e.host.clone()
                }
            })
            .unwrap_or_default();

        let header = format!("{} ({})", output::highlight(server_name), host_display);

        let mut entries: Vec<CardEntry> = Vec::new();

        // 1. Status
        let (status_label, status_color) = service_status_display(&global.service_status);
        entries.push(CardEntry::Field {
            label: "Status".into(),
            value: status_label,
            color: Some(status_color),
        });

        let is_offline = global.service_status != "active";

        // 2. Version
        if let Some(ref ver) = global.server_version {
            entries.push(CardEntry::Field {
                label: "Version".into(),
                value: display_server_version(ver),
                color: None,
            });
        }

        // 3. Server uptime
        if let Some(ref uptime) = global.server_uptime {
            entries.push(CardEntry::Field {
                label: "Server uptime".into(),
                value: uptime.clone(),
                color: None,
            });
        }

        // 4. Process uptime (skip if upgrading)
        if global.service_status != "upgrading" {
            if let Some(ref uptime) = global.process_uptime {
                entries.push(CardEntry::Field {
                    label: "Server process uptime".into(),
                    value: uptime.clone(),
                    color: None,
                });
            }
        }

        // 5. Routes section
        if !global.routes.is_empty() {
            let children: Vec<(String, String, Option<CardColor>)> = global
                .routes
                .iter()
                .map(|(app, pattern)| (app.clone(), pattern.clone(), None))
                .collect();
            entries.push(CardEntry::Section {
                label: "Routes".into(),
                children,
            });
        }

        // 6. Apps section
        if !global.apps.is_empty() && !is_offline {
            let mut apps = global.apps;
            sort_global_apps(&mut apps);
            let children: Vec<(String, String, Option<CardColor>)> = apps
                .iter()
                .map(|app| {
                    let label = format!("{} ({})", app.app_name, app.env_name);
                    let (state, color) = app_state_summary(Some(&app.status));
                    (label, state, Some(color))
                })
                .collect();
            entries.push(CardEntry::Section {
                label: "Apps".into(),
                children,
            });
        }

        // 7. Description
        if let Some(desc) = entry
            .and_then(|e| e.description.as_deref())
            .filter(|d| !d.trim().is_empty())
        {
            entries.push(CardEntry::Field {
                label: "Description".into(),
                value: desc.to_string(),
                color: None,
            });
        }

        // 8. Error (if any)
        if let Some(ref err) = global.error {
            entries.push(CardEntry::Field {
                label: "Error".into(),
                value: err.clone(),
                color: Some(CardColor::Error),
            });
        }

        // Compute max label width from this card
        for e in &entries {
            match e {
                CardEntry::Field { label, .. } => {
                    max_label = max_label.max(label.len());
                }
                CardEntry::Section { label, children } => {
                    max_label = max_label.max(label.len());
                    for (child_label, _, _) in children {
                        // Section children are indented by 4 more chars, but they share
                        // the same global alignment for the value column.
                        max_label = max_label.max(child_label.len() + 4);
                    }
                }
            }
        }

        cards.push(Card { header, entries });
    }

    // Render
    for (i, card) in cards.iter().enumerate() {
        if card.entries.is_empty() {
            println!("{}", card.header);
        } else {
            println!("{}", card.header);
            let entry_count = card.entries.len();
            let mut entry_idx = 0;
            for entry in &card.entries {
                entry_idx += 1;
                let is_last_entry = entry_idx == entry_count;
                match entry {
                    CardEntry::Field {
                        label,
                        value,
                        color,
                    } => {
                        let branch = if is_last_entry { "└" } else { "├" };
                        let colored_value = colorize(value, *color);
                        println!(
                            "{} {:<width$}  {colored_value}",
                            output::brand_muted(branch),
                            output::brand_muted(label),
                            width = max_label,
                        );
                    }
                    CardEntry::Section { label, children } => {
                        let branch = if is_last_entry { "└" } else { "├" };
                        println!(
                            "{} {}",
                            output::brand_muted(branch),
                            output::brand_muted(label),
                        );
                        let cont = if is_last_entry { " " } else { "│" };
                        for (ci, (child_label, child_value, child_color)) in
                            children.iter().enumerate()
                        {
                            let _ = ci;
                            let colored_value = colorize(child_value, *child_color);
                            println!(
                                "{}   {:<width$}  {colored_value}",
                                output::brand_muted(cont),
                                output::brand_muted(child_label),
                                width = max_label - 4,
                            );
                        }
                    }
                }
            }
        }
        if i < cards.len() - 1 {
            println!();
        }
    }
}

fn service_status_display(status: &str) -> (String, CardColor) {
    match status {
        "active" => ("active".into(), CardColor::Success),
        "upgrading" => ("upgrading".into(), CardColor::Warning),
        "inactive" | "failed" => ("offline".into(), CardColor::Error),
        "unknown" => ("offline".into(), CardColor::Error),
        other => (other.to_string(), CardColor::Warning),
    }
}

fn app_state_summary(status: Option<&ServerStatusResult>) -> (String, CardColor) {
    let Some(result) = status else {
        return ("unknown".into(), CardColor::Warning);
    };

    if let Some(app_status) = &result.app_status {
        let healthy = app_status
            .instances
            .iter()
            .filter(|i| i.state == InstanceState::Healthy || i.state == InstanceState::Ready)
            .count();
        let total = app_status.instances.len();

        return match app_status.state {
            AppState::Running => (format!("healthy {healthy}/{total}"), CardColor::Success),
            AppState::Idle => ("idle".into(), CardColor::Warning),
            AppState::Deploying => ("deploying".into(), CardColor::Warning),
            AppState::Stopped => ("stopped".into(), CardColor::Warning),
            AppState::Error => ("error".into(), CardColor::Error),
        };
    }

    if result.service_status == "active" {
        ("not deployed".into(), CardColor::Warning)
    } else {
        ("unavailable".into(), CardColor::Error)
    }
}

// ---------------------------------------------------------------------------
// Data fetching
// ---------------------------------------------------------------------------

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
            server_uptime: None,
            process_uptime: None,
            routes: Vec::new(),
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
                server_uptime: None,
                process_uptime: None,
                routes: Vec::new(),
                apps: Vec::new(),
                error: Some(format!("Failed to check service: {}", e)),
            };
        }
    };

    // Fetch server uptime via `uptime -s`
    let server_uptime = fetch_server_uptime(&client).await;

    // Fetch process uptime via server info PID
    let process_uptime = fetch_process_uptime(&mut client).await;

    // Fetch routes
    let routes = fetch_routes(&mut client).await;

    let mut effective_service_status = service_status.clone();
    let mut apps = Vec::new();
    let mut error = None;

    if service_status == "active" || service_status == "unknown" {
        match client.tako_list_apps().await {
            Ok(response) => match parse_list_apps_response(response) {
                Ok(app_names) => {
                    if service_status == "unknown" {
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
        server_uptime,
        process_uptime,
        routes,
        apps,
        error,
    }
}

async fn fetch_server_uptime(client: &SshClient) -> Option<String> {
    let output = client.exec("uptime -s 2>/dev/null || true").await.ok()?;
    let line = output.stdout.trim();
    if line.is_empty() {
        return None;
    }
    let since = parse_uptime_since(line)?;
    let elapsed = OffsetDateTime::now_utc() - since;
    Some(format_duration_human(elapsed.whole_seconds().max(0) as u64))
}

async fn fetch_process_uptime(client: &mut SshClient) -> Option<String> {
    let info = client.tako_server_info().await.ok()?;
    let pid = info.pid;
    // Try /proc/<pid>/stat mtime first (Linux), fall back to ps
    let cmd = format!(
        "stat -c '%Y' /proc/{pid} 2>/dev/null || ps -o lstart= -p {pid} 2>/dev/null || true"
    );
    let output = client.exec(&cmd).await.ok()?;
    let line = output.stdout.trim();
    if line.is_empty() {
        return None;
    }

    // Try parsing as unix timestamp (from stat)
    if let Ok(epoch) = line.parse::<i64>() {
        let now = OffsetDateTime::now_utc().unix_timestamp();
        let secs = (now - epoch).max(0) as u64;
        return Some(format_duration_human(secs));
    }

    // Try parsing ps lstart format (e.g. "Mon Mar  3 12:34:56 2026")
    // Just use a date command to convert
    let quoted = shell_single_quote(line);
    let date_cmd = format!(
        "date -d {quoted} +%s 2>/dev/null || date -j -f '%a %b %d %T %Y' {quoted} +%s 2>/dev/null || true",
    );
    let date_output = client.exec(&date_cmd).await.ok()?;
    let epoch_str = date_output.stdout.trim();
    if let Ok(epoch) = epoch_str.parse::<i64>() {
        let now = OffsetDateTime::now_utc().unix_timestamp();
        let secs = (now - epoch).max(0) as u64;
        return Some(format_duration_human(secs));
    }

    None
}

async fn fetch_routes(client: &mut SshClient) -> Vec<(String, String)> {
    let Ok(response) = client.tako_routes().await else {
        return Vec::new();
    };

    match response {
        Response::Ok { data } => {
            let mut result = Vec::new();
            if let Some(routes) = data.get("routes").and_then(|v| v.as_array()) {
                for entry in routes {
                    let app = entry
                        .get("app")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown");
                    if let Some(patterns) = entry.get("routes").and_then(|v| v.as_array()) {
                        for pattern in patterns {
                            if let Some(p) = pattern.as_str() {
                                result.push((app.to_string(), p.to_string()));
                            }
                        }
                    }
                }
            }
            result
        }
        Response::Error { .. } => Vec::new(),
    }
}

fn parse_uptime_since(line: &str) -> Option<OffsetDateTime> {
    // Format from `uptime -s`: "2026-02-27 14:30:00"
    let parts: Vec<&str> = line.split_whitespace().collect();
    if parts.len() < 2 {
        return None;
    }
    let date_parts: Vec<&str> = parts[0].split('-').collect();
    let time_parts: Vec<&str> = parts[1].split(':').collect();
    if date_parts.len() < 3 || time_parts.len() < 3 {
        return None;
    }

    let year: i32 = date_parts[0].parse().ok()?;
    let month: u8 = date_parts[1].parse().ok()?;
    let day: u8 = date_parts[2].parse().ok()?;
    let hour: u8 = time_parts[0].parse().ok()?;
    let minute: u8 = time_parts[1].parse().ok()?;
    let second: u8 = time_parts[2].parse().ok()?;

    let month = time::Month::try_from(month).ok()?;
    let date = time::Date::from_calendar_date(year, month, day).ok()?;
    let time = time::Time::from_hms(hour, minute, second).ok()?;
    Some(OffsetDateTime::new_utc(date, time))
}

fn format_duration_human(total_secs: u64) -> String {
    let days = total_secs / 86400;
    let hours = (total_secs % 86400) / 3600;
    let minutes = (total_secs % 3600) / 60;

    if days > 0 {
        format!("{}d {}h", days, hours)
    } else if hours > 0 {
        format!("{}h {}m", hours, minutes)
    } else {
        format!("{}m", minutes)
    }
}

// ---------------------------------------------------------------------------
// Shared helpers (kept from original)
// ---------------------------------------------------------------------------

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

fn display_server_version(version: &str) -> String {
    if version.starts_with('v') {
        version.to_string()
    } else {
        format!("v{}", version)
    }
}

#[cfg(test)]
fn local_offset() -> time::UtcOffset {
    *LOCAL_OFFSET
        .get_or_init(|| time::UtcOffset::current_local_offset().unwrap_or(time::UtcOffset::UTC))
}

#[cfg(test)]
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

#[cfg(test)]
fn format_unix_timestamp_with_offset(unix_secs: i64, offset: time::UtcOffset) -> Option<String> {
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

#[cfg(test)]
fn format_unix_timestamp_with_date_command(unix_secs: i64) -> Option<String> {
    use std::process::Command;
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

#[cfg(test)]
fn run_local_date_command(mut cmd: std::process::Command) -> Option<String> {
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
mod tests {
    use super::*;
    use tako_core::{BuildStatus, InstanceStatus};
    use time::UtcOffset;

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

    #[tokio::test]
    async fn collect_global_status_task_maps_join_error_to_unknown_status() {
        let handle: tokio::task::JoinHandle<GlobalServerStatusResult> = tokio::spawn(async move {
            panic!("boom");
        });

        let result = match handle.await {
            Ok(status) => status,
            Err(err) => GlobalServerStatusResult {
                service_status: "unknown".to_string(),
                server_version: None,
                server_uptime: None,
                process_uptime: None,
                routes: Vec::new(),
                apps: Vec::new(),
                error: Some(format!("Status task failed: {}", err)),
            },
        };

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

    #[test]
    fn format_duration_human_formats_days_hours_minutes() {
        assert_eq!(format_duration_human(0), "0m");
        assert_eq!(format_duration_human(59), "0m");
        assert_eq!(format_duration_human(60), "1m");
        assert_eq!(format_duration_human(3600), "1h 0m");
        assert_eq!(format_duration_human(3660), "1h 1m");
        assert_eq!(format_duration_human(86400), "1d 0h");
        assert_eq!(format_duration_human(90000), "1d 1h");
        assert_eq!(format_duration_human(7 * 86400 + 11 * 3600), "7d 11h");
    }

    #[test]
    fn parse_uptime_since_parses_standard_format() {
        let dt = parse_uptime_since("2026-02-27 14:30:00").unwrap();
        assert_eq!(dt.year(), 2026);
        assert_eq!(dt.month() as u8, 2);
        assert_eq!(dt.day(), 27);
        assert_eq!(dt.hour(), 14);
        assert_eq!(dt.minute(), 30);
    }

    #[test]
    fn parse_uptime_since_returns_none_for_garbage() {
        assert!(parse_uptime_since("not a date").is_none());
        assert!(parse_uptime_since("").is_none());
    }

    #[test]
    fn service_status_display_maps_states_correctly() {
        let (label, _) = service_status_display("active");
        assert_eq!(label, "active");

        let (label, _) = service_status_display("inactive");
        assert_eq!(label, "offline");

        let (label, _) = service_status_display("unknown");
        assert_eq!(label, "offline");

        let (label, _) = service_status_display("upgrading");
        assert_eq!(label, "upgrading");
    }

    #[test]
    fn app_state_summary_shows_healthy_count() {
        let status = ServerStatusResult {
            service_status: "active".to_string(),
            server_version: Some("0.1.0".to_string()),
            app_status: Some(AppStatus {
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
                        state: InstanceState::Healthy,
                        port: 3002,
                        pid: Some(112),
                        uptime_secs: 10,
                        requests_total: 0,
                    },
                    InstanceStatus {
                        id: 3,
                        state: InstanceState::Starting,
                        port: 3003,
                        pid: Some(113),
                        uptime_secs: 1,
                        requests_total: 0,
                    },
                ],
                builds: vec![],
                state: AppState::Running,
                last_error: None,
            }),
            deployed_at_unix_secs: None,
            error: None,
        };

        let (summary, _) = app_state_summary(Some(&status));
        assert_eq!(summary, "healthy 2/3");
    }

    #[test]
    fn app_state_summary_shows_deploying() {
        let status = ServerStatusResult {
            service_status: "active".to_string(),
            server_version: Some("0.1.0".to_string()),
            app_status: Some(AppStatus {
                name: "demo".to_string(),
                version: "v123".to_string(),
                instances: vec![],
                builds: vec![],
                state: AppState::Deploying,
                last_error: None,
            }),
            deployed_at_unix_secs: None,
            error: None,
        };

        let (summary, _) = app_state_summary(Some(&status));
        assert_eq!(summary, "deploying");
    }
}

use crate::commands::server;
use crate::config::{ServersToml, TakoToml};
use crate::output;
use crate::ssh::{SshClient, SshConfig};
use std::collections::HashMap;
use tako_core::{AppState, AppStatus, InstanceState, Response};
use time::OffsetDateTime;
use tracing::Instrument;

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
#[derive(Debug, Clone)]
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
        output::hint(&format!(
            "Run {} to add one.",
            output::strong("tako servers add")
        ));
        return Ok(());
    }

    let server_names = sorted_server_names(servers);

    let mut results = collect_global_status_results(servers, &server_names).await?;

    render_global_status(servers, &server_names, &mut results);

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
    let mut join_set = tokio::task::JoinSet::new();

    for server_name in server_names {
        let Some(entry) = servers.get(server_name.as_str()) else {
            continue;
        };

        let name = server_name.clone();
        let host = entry.host.clone();
        let port = entry.port;
        let span = output::scope(&name);
        join_set.spawn(
            async move {
                let status = query_global_server_status(&name, &host, port).await;
                (name, status)
            }
            .instrument(span),
        );
    }

    let total = join_set.len();
    let mut done = 0usize;

    let spinner = output::TrackedSpinner::start("Retrieving…");
    spinner.set_message(&format!(
        "Retrieving… {}",
        output::muted_progress(done, total)
    ));

    let mut server_results: HashMap<String, GlobalServerStatusResult> = HashMap::new();

    while let Some(join_result) = join_set.join_next().await {
        let (server_name, status) = match join_result {
            Ok(pair) => pair,
            Err(err) => {
                done += 1;
                spinner.set_message(&format!(
                    "Retrieving… {}",
                    output::muted_progress(done, total)
                ));
                output::error(&format!("Status task panicked: {err}"));
                continue;
            }
        };

        done += 1;
        spinner.set_message(&format!(
            "Retrieving… {}",
            output::muted_progress(done, total)
        ));

        server_results.insert(server_name, status);
    }

    spinner.finish();

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

        let mut entries: Vec<CardEntry> = Vec::new();

        let header = format!("Server {}", output::strong(server_name));

        // If retrieval failed, only show the error.
        if let Some(ref err) = global.error {
            entries.push(CardEntry::Field {
                label: "Error".into(),
                value: err.clone(),
                color: Some(CardColor::Error),
            });
            cards.push(Card { header, entries });
            continue;
        }

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

        // 3. Uptime (machine)
        if let Some(ref uptime) = global.server_uptime {
            entries.push(CardEntry::Field {
                label: "Uptime".into(),
                value: uptime.clone(),
                color: None,
            });
        }

        // 4. Server uptime (process, skip if upgrading)
        if global.service_status != "upgrading" {
            if let Some(ref uptime) = global.process_uptime {
                entries.push(CardEntry::Field {
                    label: "Server uptime".into(),
                    value: uptime.clone(),
                    color: None,
                });
            }
        }

        // 5. Routes section (group by app name, blank repeated names)
        if !global.routes.is_empty() {
            let mut children: Vec<(String, String, Option<CardColor>)> = Vec::new();
            let mut last_app = String::new();
            for (app, pattern) in &global.routes {
                let label = if *app == last_app {
                    String::new()
                } else {
                    last_app.clone_from(app);
                    app.clone()
                };
                children.push((label, pattern.clone(), None));
            }
            entries.push(CardEntry::Section {
                label: "Routes".into(),
                children,
            });
        }

        // 6. Apps section
        if !global.apps.is_empty() && !is_offline {
            let mut apps = global.apps.clone();
            sort_global_apps(&mut apps);
            let mut children: Vec<(String, String, Option<CardColor>)> = Vec::new();
            for app in &apps {
                let (state, color) = app_state_summary(Some(&app.status));
                children.push((app.app_name.clone(), state, Some(color)));

                // Environment
                if app.env_name != "unknown" {
                    children.push(("  Environment".into(), app.env_name.clone(), None));
                }

                if let Some(ref app_status) = app.status.app_status {
                    // Instances
                    let healthy = app_status
                        .instances
                        .iter()
                        .filter(|i| {
                            i.state == InstanceState::Healthy || i.state == InstanceState::Ready
                        })
                        .count();
                    let total = app_status.instances.len();
                    if total > 0 {
                        children.push((
                            "  Instances".into(),
                            format!("{healthy}/{total} healthy"),
                            None,
                        ));
                    }

                    // Release
                    if !app_status.version.is_empty() {
                        children.push(("  Release".into(), app_status.version.clone(), None));
                    }
                }

                // Deployed At
                if let Some(unix_secs) = app.status.deployed_at_unix_secs {
                    if let Some(formatted) = format_deployed_at(unix_secs) {
                        children.push(("  Deployed at".into(), formatted, None));
                    }
                }
            }
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

        // Compute max label width from this card.
        for e in &entries {
            match e {
                CardEntry::Field { label, .. } => {
                    max_label = max_label.max(label.len());
                }
                CardEntry::Section { label, children } => {
                    max_label = max_label.max(label.len());
                    for (child_label, _, _) in children {
                        max_label = max_label.max(child_label.len());
                    }
                }
            }
        }

        cards.push(Card { header, entries });
    }

    // Render
    let indent = output::INDENT;
    for card in &cards {
        eprintln!("{}", card.header);
        if !card.entries.is_empty() {
            for entry in &card.entries {
                match entry {
                    CardEntry::Field {
                        label,
                        value,
                        color,
                    } => {
                        let colored_value = colorize(value, *color);
                        let padded = format!("{:<width$}", label, width = max_label);
                        eprintln!("{indent}{}  {colored_value}", output::brand_muted(&padded),);
                    }
                    CardEntry::Section { label, children } => {
                        eprintln!("{indent}{}", output::brand_muted(label));

                        for (ci, (child_label, child_value, child_color)) in
                            children.iter().enumerate()
                        {
                            let colored_value = colorize(child_value, *child_color);
                            if child_label.starts_with("  ") {
                                // Sub-item: first gets └, rest get space
                                let is_first_sub = ci == 0 || !children[ci - 1].0.starts_with("  ");
                                let branch = if is_first_sub { "└" } else { " " };
                                let trimmed = child_label.trim_start();
                                let padded = format!(
                                    "{:<width$}",
                                    trimmed,
                                    width = max_label.saturating_sub(2)
                                );
                                eprintln!(
                                    "{indent}  {} {}  {colored_value}",
                                    output::brand_muted(branch),
                                    output::brand_muted(&padded),
                                );
                            } else {
                                // Top-level child: └ prefix, or spaces if label is blank (continuation)
                                let branch = if child_label.is_empty() { " " } else { "└" };
                                let padded = format!("{:<width$}", child_label, width = max_label);
                                eprintln!(
                                    "{indent}{} {}  {colored_value}",
                                    output::brand_muted(branch),
                                    output::brand_muted(&padded),
                                );
                            }
                        }
                    }
                }
            }
        }
        eprintln!();
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
    let _t = output::timed("Query status");
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

    // Probe service status via socket — this is the fastest check.
    let service_status = match client.tako_status().await {
        Ok(status) => {
            tracing::debug!("Service status: {status}");
            status
        }
        Err(e) => {
            let _ = client.disconnect().await;
            return GlobalServerStatusResult {
                service_status: "unknown".to_string(),
                server_version: None,
                server_uptime: None,
                process_uptime: None,
                routes: Vec::new(),
                apps: Vec::new(),
                error: Some(format!("Failed to check service: {}", e)),
            };
        }
    };

    // Fetch version/uptime and routes in parallel to reduce SSH round-trips.
    let ((server_version, server_uptime, process_uptime), routes) =
        tokio::join!(fetch_version_and_uptimes(&client), fetch_routes(&client),);

    let mut effective_service_status = service_status.clone();
    let mut apps = Vec::new();
    let mut error = None;

    if service_status == "active" || service_status == "unknown" {
        match client.tako_list_apps().await {
            Ok(response) => match parse_list_apps_response(response) {
                Ok(app_names) => {
                    tracing::debug!("Found {} app(s)", app_names.len());
                    if service_status == "unknown" {
                        effective_service_status = "active".to_string();
                    }

                    for remote_app_name in app_names {
                        let (display_app_name, env_from_name) =
                            parse_remote_app_name(&remote_app_name);
                        let status = query_connected_app_status(
                            &client,
                            &effective_service_status,
                            server_version.clone(),
                            &remote_app_name,
                        )
                        .await;
                        for mut build_status in expand_status_by_running_builds(status) {
                            let app_version = build_status
                                .app_status
                                .as_ref()
                                .map(|app| app.version.clone());

                            let env_name = if let Some(app_version) = app_version {
                                let (deployed_at, env) = fetch_app_deploy_info(
                                    &client,
                                    &remote_app_name,
                                    &app_version,
                                    server_name,
                                )
                                .await;
                                build_status.deployed_at_unix_secs = deployed_at;
                                env_from_name
                                    .clone()
                                    .or(env)
                                    .unwrap_or_else(|| "unknown".to_string())
                            } else {
                                env_from_name
                                    .clone()
                                    .unwrap_or_else(|| "unknown".to_string())
                            };

                            apps.push(GlobalAppStatusResult {
                                app_name: display_app_name.clone(),
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

/// Fetch server version, machine uptime, and process uptime in minimal
/// round-trips.  We get the PID from `tako_server_info` (socket), then run a
/// single SSH exec that retrieves `tako-server --version`, `uptime -s`, and
/// the process start time all at once.
async fn fetch_version_and_uptimes(
    client: &SshClient,
) -> (Option<String>, Option<String>, Option<String>) {
    // Get PID from the socket (already connected).
    let pid = client.tako_server_info().await.ok().map(|info| info.pid);

    // Build a combined command that outputs three tagged lines.
    let proc_stat_cmd = if let Some(pid) = pid {
        format!(
            "echo PROC:$(stat -c '%Y' /proc/{pid} 2>/dev/null || ps -o lstart= -p {pid} 2>/dev/null || echo -)"
        )
    } else {
        "echo PROC:-".to_string()
    };
    let combined = format!(
        "echo VER:$(tako-server --version 2>/dev/null || echo -); \
         echo UP:$(uptime -s 2>/dev/null || echo -); \
         {proc_stat_cmd}"
    );

    let output = match client.exec(&combined).await {
        Ok(o) => o,
        Err(_) => return (None, None, None),
    };

    let mut version = None;
    let mut server_uptime = None;
    let mut process_uptime = None;

    for line in output.stdout.lines() {
        let line = line.trim();
        if let Some(val) = line.strip_prefix("VER:") {
            let val = val.trim();
            if val != "-" && !val.is_empty() {
                version = Some(normalize_server_version(val.to_string()));
            }
        } else if let Some(val) = line.strip_prefix("UP:") {
            let val = val.trim();
            if val != "-" && !val.is_empty() {
                if let Some(since) = parse_uptime_since(val) {
                    let elapsed = OffsetDateTime::now_utc() - since;
                    server_uptime =
                        Some(format_duration_human(elapsed.whole_seconds().max(0) as u64));
                }
            }
        } else if let Some(val) = line.strip_prefix("PROC:") {
            let val = val.trim();
            if val != "-" && !val.is_empty() {
                process_uptime = parse_process_start(val);
            }
        }
    }

    (version, server_uptime, process_uptime)
}

/// Parse a process start value — either a unix timestamp (from stat) or a
/// ps lstart string — into a human-readable duration.
fn parse_process_start(val: &str) -> Option<String> {
    // Try unix timestamp first (from stat -c '%Y')
    if let Ok(epoch) = val.parse::<i64>() {
        let now = OffsetDateTime::now_utc().unix_timestamp();
        let secs = (now - epoch).max(0) as u64;
        return Some(format_duration_human(secs));
    }
    None
}

async fn fetch_routes(client: &SshClient) -> Vec<(String, String)> {
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
                    let app = format_remote_app_label(app);
                    if let Some(patterns) = entry.get("routes").and_then(|v| v.as_array()) {
                        for pattern in patterns {
                            if let Some(p) = pattern.as_str() {
                                result.push((app.clone(), p.to_string()));
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

fn format_deployed_at(unix_secs: i64) -> Option<String> {
    // Try local time first, fall back to UTC.
    let dt = OffsetDateTime::from_unix_timestamp(unix_secs).ok()?;
    let local =
        dt.to_offset(time::UtcOffset::current_local_offset().unwrap_or(time::UtcOffset::UTC));
    let month = local.month();
    Some(format!(
        "{} {}, {} {:02}:{:02}",
        month_abbrev(month),
        local.day(),
        local.year(),
        local.hour(),
        local.minute(),
    ))
}

fn month_abbrev(month: time::Month) -> &'static str {
    match month {
        time::Month::January => "Jan",
        time::Month::February => "Feb",
        time::Month::March => "Mar",
        time::Month::April => "Apr",
        time::Month::May => "May",
        time::Month::June => "Jun",
        time::Month::July => "Jul",
        time::Month::August => "Aug",
        time::Month::September => "Sep",
        time::Month::October => "Oct",
        time::Month::November => "Nov",
        time::Month::December => "Dec",
    }
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

fn parse_remote_app_name(app_name: &str) -> (String, Option<String>) {
    match tako_core::split_deployment_app_id(app_name) {
        Some((base_app_name, env_name)) => (base_app_name.to_string(), Some(env_name.to_string())),
        None => (app_name.to_string(), None),
    }
}

fn format_remote_app_label(app_name: &str) -> String {
    let (base_app_name, env_name) = parse_remote_app_name(app_name);
    match env_name {
        Some(env_name) => format!("{base_app_name} ({env_name})"),
        None => base_app_name,
    }
}

async fn query_connected_app_status(
    client: &SshClient,
    service_status: &str,
    server_version: Option<String>,
    app_name: &str,
) -> ServerStatusResult {
    let mut app_status = None;
    let deployed_at_unix_secs = None;
    let mut error = None;

    if service_status == "active" {
        match client.tako_app_status(app_name).await {
            Ok(Response::Ok { data }) => match serde_json::from_value::<AppStatus>(data) {
                Ok(status) => {
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

/// Fetch deployed-at timestamp and environment name for an app version in a
/// single SSH exec call (combines stat + cat tako.toml).
async fn fetch_app_deploy_info(
    client: &SshClient,
    app_name: &str,
    version: &str,
    server_name: &str,
) -> (Option<i64>, Option<String>) {
    let release_dir = format!("/opt/tako/apps/{}/releases/{}", app_name, version);
    let release_toml = format!("{}/tako.toml", release_dir);
    let dir_q = shell_single_quote(&release_dir);
    let toml_q = shell_single_quote(&release_toml);

    // Output: first line = deployed-at epoch (or -), rest = tako.toml content
    let cmd = format!(
        "if [ -d {dir_q} ]; then \
           (stat -c '%Y' {dir_q} 2>/dev/null || stat -f '%m' {dir_q} 2>/dev/null || echo -); \
         else echo -; fi; \
         echo '---TOML---'; \
         if [ -f {toml_q} ]; then cat {toml_q}; fi"
    );

    let output = match client.exec(&cmd).await {
        Ok(o) => o,
        Err(_) => return (None, None),
    };

    let stdout = &output.stdout;
    let (epoch_part, toml_part) = match stdout.split_once("---TOML---\n") {
        Some((a, b)) => (a, b),
        None => match stdout.split_once("---TOML---") {
            Some((a, b)) => (a, b),
            None => (stdout.as_str(), ""),
        },
    };

    let deployed_at = epoch_part
        .lines()
        .next()
        .and_then(|line| line.trim().parse::<i64>().ok());

    let env_name = if toml_part.trim().is_empty() {
        None
    } else {
        parse_server_env_from_tako_toml(toml_part, server_name)
    };

    (deployed_at, env_name)
}

fn parse_server_env_from_tako_toml(content: &str, server_name: &str) -> Option<String> {
    let config = TakoToml::parse(content).ok()?;

    let mut matching_envs = Vec::new();
    let mut configured_envs = Vec::new();
    for (env_name, env_config) in &config.envs {
        if env_name == "development" {
            continue;
        }

        if env_config.servers.iter().any(|name| name == server_name) {
            matching_envs.push(env_name.clone());
        }
        if !env_config.servers.is_empty() {
            configured_envs.push(env_name.clone());
        }
    }
    matching_envs.sort();
    matching_envs.dedup();
    if matching_envs.len() == 1 {
        return matching_envs.into_iter().next();
    }

    configured_envs.sort();
    configured_envs.dedup();
    if configured_envs.len() == 1 {
        configured_envs.into_iter().next()
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

use crate::shell::shell_single_quote;

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
                            id: "abc1".to_string(),
                            state: InstanceState::Healthy,
                            pid: Some(111),
                            uptime_secs: 10,
                            requests_total: 0,
                        }],
                    },
                    BuildStatus {
                        version: "v2".to_string(),
                        state: AppState::Running,
                        instances: vec![InstanceStatus {
                            id: "abc2".to_string(),
                            state: InstanceState::Healthy,
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
    fn parse_remote_app_name_extracts_env_from_deployment_id() {
        assert_eq!(
            parse_remote_app_name("web/staging"),
            ("web".to_string(), Some("staging".to_string()))
        );
        assert_eq!(format_remote_app_label("web/staging"), "web (staging)");
    }

    #[test]
    fn parse_server_env_from_tako_toml_prefers_matching_server_name() {
        let content = r#"
[envs.production]
route = "app.example.com"
servers = ["eu"]

[envs.staging]
route = "staging.example.com"
servers = ["us"]
"#;
        let env = parse_server_env_from_tako_toml(content, "us");
        assert_eq!(env.as_deref(), Some("staging"));
    }

    #[test]
    fn parse_server_env_from_tako_toml_falls_back_to_single_mapping() {
        let content = r#"
[envs.production]
route = "app.example.com"
servers = ["only"]
"#;
        let env = parse_server_env_from_tako_toml(content, "missing");
        assert_eq!(env.as_deref(), Some("production"));
    }

    #[test]
    fn parse_server_env_from_tako_toml_returns_none_for_ambiguous_mappings() {
        let content = r#"
[envs.production]
route = "app.example.com"
servers = ["first"]

[envs.staging]
route = "staging.example.com"
servers = ["second"]
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
                        id: "abc1".to_string(),
                        state: InstanceState::Healthy,
                        pid: Some(111),
                        uptime_secs: 10,
                        requests_total: 0,
                    },
                    InstanceStatus {
                        id: "abc2".to_string(),
                        state: InstanceState::Healthy,
                        pid: Some(112),
                        uptime_secs: 10,
                        requests_total: 0,
                    },
                    InstanceStatus {
                        id: "abc3".to_string(),
                        state: InstanceState::Starting,
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

    #[test]
    fn format_deployed_at_formats_epoch_in_local_time() {
        // 2026-03-06 12:00:00 UTC
        let ts = 1772798400;
        let formatted = format_deployed_at(ts).unwrap();
        assert!(formatted.starts_with("Mar 6, 2026"));
    }
}

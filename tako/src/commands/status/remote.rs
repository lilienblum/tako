use super::time::{format_duration_human, parse_uptime_since};
use super::{GlobalAppStatusResult, GlobalServerStatusResult, ServerStatusResult};
use crate::config::TakoToml;
use crate::output;
use crate::shell::shell_single_quote;
use crate::ssh::{SshClient, SshConfig};
use tako_core::{AppStatus, Response};
use time::OffsetDateTime;

pub(super) async fn query_global_server_status(
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

async fn fetch_version_and_uptimes(
    client: &SshClient,
) -> (Option<String>, Option<String>, Option<String>) {
    let pid = client.tako_server_info().await.ok().map(|info| info.pid);

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
            if val != "-"
                && !val.is_empty()
                && let Some(since) = parse_uptime_since(val)
            {
                let elapsed = OffsetDateTime::now_utc() - since;
                server_uptime = Some(format_duration_human(elapsed.whole_seconds().max(0) as u64));
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

fn parse_process_start(val: &str) -> Option<String> {
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

pub(super) fn sort_global_apps(apps: &mut [GlobalAppStatusResult]) {
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

pub(super) fn parse_remote_app_name(app_name: &str) -> (String, Option<String>) {
    match tako_core::split_deployment_app_id(app_name) {
        Some((base_app_name, env_name)) => (base_app_name.to_string(), Some(env_name.to_string())),
        None => (app_name.to_string(), None),
    }
}

pub(super) fn format_remote_app_label(app_name: &str) -> String {
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

pub(super) fn expand_status_by_running_builds(
    status: ServerStatusResult,
) -> Vec<ServerStatusResult> {
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

pub(super) fn parse_list_apps_response(response: Response) -> Result<Vec<String>, String> {
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

pub(super) fn parse_server_env_from_tako_toml(content: &str, server_name: &str) -> Option<String> {
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

pub(super) fn normalize_server_version(raw: String) -> String {
    raw.trim()
        .strip_prefix("tako-server ")
        .unwrap_or(raw.trim())
        .to_string()
}

pub(super) fn display_server_version(version: &str) -> String {
    if version.starts_with('v') {
        version.to_string()
    } else {
        format!("v{}", version)
    }
}

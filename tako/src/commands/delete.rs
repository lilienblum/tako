use std::collections::{BTreeMap, BTreeSet};
use std::env::current_dir;

use crate::app::resolve_app_name;
use crate::config::{ServerEntry, ServersToml, TakoToml};
use crate::output;
use crate::ssh::{SshClient, SshConfig};
use tako_core::{AppStatus, Command, Response};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct RemoteDeployment {
    app: String,
    env: String,
    server_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AppEnvOption {
    app: String,
    env: String,
    server_count: usize,
}

pub fn run(env: Option<&str>, assume_yes: bool) -> Result<(), Box<dyn std::error::Error>> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(run_async(env, assume_yes))
}

async fn run_async(
    requested_env: Option<&str>,
    assume_yes: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let project_dir = current_dir()?;
    let interactive = output::is_interactive();

    let project_tako = load_optional_project_tako_toml(&project_dir)?;
    let project_app =
        if project_tako.is_some() {
            Some(resolve_app_name(&project_dir).map_err(|e| {
                std::io::Error::new(std::io::ErrorKind::InvalidInput, e.to_string())
            })?)
        } else {
            None
        };

    let servers = ServersToml::load()?;

    validate_confirmation_mode(assume_yes, interactive)
        .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;

    let mut discovered_deployments: Option<Vec<RemoteDeployment>> = None;

    let (app_name, env) = if let Some(tako_config) = project_tako.as_ref() {
        let app_name = project_app
            .clone()
            .ok_or_else(|| "Could not resolve app name from project context".to_string())?;

        let env = match requested_env {
            Some(env) => validate_project_delete_env(env, tako_config)
                .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?,
            None => {
                if !interactive {
                    validate_project_delete_env("production", tako_config)
                        .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?
                } else {
                    let deployments = discover_remote_deployments_with_progress(&servers).await?;
                    let options = app_env_options(&deployments, Some(&app_name));
                    if options.is_empty() {
                        return Err(format!(
                            "No deployed environments found for app '{}' on configured servers.",
                            app_name
                        )
                        .into());
                    }
                    select_env_for_app(&app_name, &options)?
                }
            }
        };

        (app_name, env)
    } else {
        let deployments = discover_remote_deployments_with_progress(&servers).await?;
        if deployments.is_empty() {
            return Err("No deployed apps found on configured servers.".into());
        }

        let target = match requested_env {
            Some(env) => {
                validate_non_project_delete_env(env)
                    .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;
                let app = resolve_app_for_env_without_project(&deployments, env, interactive)?;
                (app, env.to_string())
            }
            None => {
                if !interactive {
                    return Err(
                        "No project context and no --env. Run this command interactively to select app/environment, or run from a project directory."
                            .into(),
                    );
                }
                let options = app_env_options(&deployments, None);
                select_app_env(&options)?
            }
        };

        discovered_deployments = Some(deployments);
        target
    };

    let server_names = if let Some(tako_config) = project_tako.as_ref() {
        resolve_delete_server_names(tako_config, &servers, &env)
            .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?
    } else {
        let deployments = discovered_deployments
            .as_deref()
            .ok_or_else(|| "Missing deployment discovery data".to_string())?;
        resolve_delete_server_names_from_deployments(deployments, &app_name, &env)
            .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?
    };

    if should_confirm_delete(assume_yes, interactive) {
        let prompt = format_delete_confirm_prompt(&app_name, &env, server_names.len());
        let description = format_delete_confirm_hint(&app_name, &server_names);
        let confirmed = output::confirm_with_description(&prompt, Some(&description), false)?;
        if !confirmed {
            return Err("Delete cancelled.".into());
        }
    }

    output::section("Delete");
    output::step(&format!(
        "Deleting {} from {}",
        output::emphasized(&app_name),
        output::emphasized(&env)
    ));

    let total_servers = server_names.len();
    let mut success_count = 0usize;
    let mut errors = Vec::new();
    let use_per_server_spinner = should_use_per_server_delete_spinner(total_servers, interactive);
    if use_per_server_spinner {
        for server_name in &server_names {
            let Some(server) = servers.get(server_name) else {
                return Err(format_server_not_found_error(server_name).into());
            };
            let label = format!("Deleting from {}...", output::emphasized(server_name));
            let result =
                output::with_spinner_async(label, delete_from_server(server, &app_name)).await?;
            match result {
                Ok(()) => {
                    output::success(&format_server_delete_success_for_output(
                        server_name,
                        server,
                        output::is_verbose(),
                    ));
                    success_count += 1;
                }
                Err(e) => {
                    output::error(&format_server_delete_failure_for_output(
                        server_name,
                        server,
                        &e.to_string(),
                        output::is_verbose(),
                    ));
                    errors.push(format!("{}: {}", server_name, e));
                }
            }
        }
    } else {
        let mut handles = Vec::new();
        for server_name in &server_names {
            let Some(server) = servers.get(server_name) else {
                return Err(format_server_not_found_error(server_name).into());
            };

            let server_name = server_name.clone();
            let server = server.clone();
            let app_name = app_name.clone();
            let handle = tokio::spawn(async move {
                let result = delete_from_server(&server, &app_name).await;
                (server_name, server, result)
            });
            handles.push(handle);
        }

        for handle in handles {
            match handle.await {
                Ok((server_name, server, Ok(()))) => {
                    output::success(&format_server_delete_success_for_output(
                        &server_name,
                        &server,
                        output::is_verbose(),
                    ));
                    success_count += 1;
                }
                Ok((server_name, server, Err(e))) => {
                    output::error(&format_server_delete_failure_for_output(
                        &server_name,
                        &server,
                        &e.to_string(),
                        output::is_verbose(),
                    ));
                    errors.push(format!("{}: {}", server_name, e));
                }
                Err(e) => errors.push(format!("Task panic: {}", e)),
            }
        }
    }

    output::section("Summary");
    if errors.is_empty() {
        output::success(&format!(
            "Deleted {} from {}",
            output::emphasized(&app_name),
            output::emphasized(&env)
        ));
        Ok(())
    } else {
        output::warning(&format_delete_summary_warning(success_count, total_servers));
        for err in &errors {
            output::error(err);
        }

        Err(format!("{} server(s) failed", errors.len()).into())
    }
}

fn load_optional_project_tako_toml(
    project_dir: &std::path::Path,
) -> Result<Option<TakoToml>, Box<dyn std::error::Error>> {
    let path = project_dir.join("tako.toml");
    if !path.exists() {
        return Ok(None);
    }
    Ok(Some(TakoToml::load_from_dir(project_dir)?))
}

async fn discover_remote_deployments(
    servers: &ServersToml,
) -> Result<Vec<RemoteDeployment>, Box<dyn std::error::Error>> {
    if servers.is_empty() {
        return Err("No servers have been added. Run 'tako servers add <host>' first.".into());
    }

    let mut names: Vec<String> = servers.names().into_iter().map(str::to_string).collect();
    names.sort();

    let mut handles = Vec::new();
    for server_name in names {
        let Some(server) = servers.get(&server_name) else {
            continue;
        };

        let server_name_for_task = server_name.clone();
        let server = server.clone();
        handles.push(tokio::spawn(async move {
            let result = discover_server_deployments(&server_name_for_task, &server).await;
            (server_name_for_task, result)
        }));
    }

    let mut all = Vec::new();
    for handle in handles {
        match handle.await {
            Ok((_server_name, Ok(mut deployments))) => {
                all.append(&mut deployments);
            }
            Ok((server_name, Err(e))) => {
                return Err(
                    format!("Failed to query deployed apps on '{}': {}", server_name, e).into(),
                );
            }
            Err(e) => {
                return Err(format!("Deployment discovery task panic: {}", e).into());
            }
        }
    }

    all.sort();
    all.dedup();
    Ok(all)
}

async fn discover_remote_deployments_with_progress(
    servers: &ServersToml,
) -> Result<Vec<RemoteDeployment>, Box<dyn std::error::Error>> {
    if output::is_interactive() {
        let deployments = output::with_spinner_async("Discovering deployed apps...", async {
            discover_remote_deployments(servers)
                .await
                .map_err(|e| e.to_string())
        })
        .await?
        .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;
        return Ok(deployments);
    }
    discover_remote_deployments(servers).await
}

async fn discover_server_deployments(
    server_name: &str,
    server: &ServerEntry,
) -> Result<Vec<RemoteDeployment>, Box<dyn std::error::Error + Send + Sync>> {
    let ssh_config = SshConfig::from_server(&server.host, server.port);
    let mut ssh = SshClient::new(ssh_config);
    ssh.connect().await?;

    let result = async {
        let list = ssh.tako_list_apps().await?;
        let app_names = parse_list_apps_response(list)
            .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { e.into() })?;

        let mut deployments = Vec::new();
        for app_name in app_names {
            let env = query_connected_app_env(&mut ssh, &app_name, server_name)
                .await
                .unwrap_or_else(|| "unknown".to_string());
            deployments.push(RemoteDeployment {
                app: app_name,
                env,
                server_name: server_name.to_string(),
            });
        }

        Ok::<Vec<RemoteDeployment>, Box<dyn std::error::Error + Send + Sync>>(deployments)
    }
    .await;

    let _ = ssh.disconnect().await;
    result
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

async fn query_connected_app_env(
    client: &mut SshClient,
    app_name: &str,
    server_name: &str,
) -> Option<String> {
    let response = client.tako_app_status(app_name).await.ok()?;
    let status = match response {
        Response::Ok { data } => serde_json::from_value::<AppStatus>(data).ok()?,
        Response::Error { .. } => return None,
    };

    query_remote_app_env_for_server(client, app_name, &status.version, server_name).await
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

fn app_env_options(
    deployments: &[RemoteDeployment],
    app_filter: Option<&str>,
) -> Vec<AppEnvOption> {
    let mut grouped: BTreeMap<(String, String), BTreeSet<String>> = BTreeMap::new();

    for deployment in deployments {
        if let Some(filter) = app_filter
            && deployment.app != filter
        {
            continue;
        }

        grouped
            .entry((deployment.app.clone(), deployment.env.clone()))
            .or_default()
            .insert(deployment.server_name.clone());
    }

    grouped
        .into_iter()
        .map(|((app, env), servers)| AppEnvOption {
            app,
            env,
            server_count: servers.len(),
        })
        .collect()
}

fn select_env_for_app(
    app_name: &str,
    options: &[AppEnvOption],
) -> Result<String, Box<dyn std::error::Error>> {
    let choices = options
        .iter()
        .map(|option| {
            (
                format!("{} ({} server(s))", option.env, option.server_count),
                option.env.clone(),
            )
        })
        .collect::<Vec<_>>();

    output::select(
        &format!("Select environment to delete '{}' from", app_name),
        Some("Choose an environment and press Enter."),
        choices,
    )
    .map_err(|e| -> Box<dyn std::error::Error> { Box::new(e) })
}

fn select_app_env(
    options: &[AppEnvOption],
) -> Result<(String, String), Box<dyn std::error::Error>> {
    let choices = options
        .iter()
        .map(|option| {
            (
                format!(
                    "{}  app={} ({} server(s))",
                    option.env, option.app, option.server_count
                ),
                (option.app.clone(), option.env.clone()),
            )
        })
        .collect::<Vec<_>>();

    output::select(
        "Select app/environment to delete",
        Some("Choose a deployed target and press Enter."),
        choices,
    )
    .map_err(|e| -> Box<dyn std::error::Error> { Box::new(e) })
}

fn resolve_app_for_env_without_project(
    deployments: &[RemoteDeployment],
    env: &str,
    interactive: bool,
) -> Result<String, Box<dyn std::error::Error>> {
    let options = app_env_options(deployments, None)
        .into_iter()
        .filter(|option| option.env == env)
        .collect::<Vec<_>>();

    if options.is_empty() {
        return Err(format!(
            "No deployed apps found for environment '{}' on configured servers.",
            env
        )
        .into());
    }

    let mut apps: Vec<String> = options.iter().map(|option| option.app.clone()).collect();
    apps.sort();
    apps.dedup();

    if apps.len() == 1 {
        return Ok(apps.remove(0));
    }

    if !interactive {
        return Err(format!(
            "Multiple apps are deployed to environment '{}'. Run interactively to select one.",
            env
        )
        .into());
    }

    let app_choices = apps
        .into_iter()
        .map(|app| {
            let server_count = options
                .iter()
                .find(|option| option.app == app)
                .map(|option| option.server_count)
                .unwrap_or(0);
            (format!("{} ({} server(s))", app, server_count), app)
        })
        .collect::<Vec<_>>();

    output::select(
        &format!("Select app to delete from '{}'", env),
        Some("Choose an app and press Enter."),
        app_choices,
    )
    .map_err(|e| -> Box<dyn std::error::Error> { Box::new(e) })
}

fn validate_project_delete_env(env: &str, tako_config: &TakoToml) -> Result<String, String> {
    validate_non_project_delete_env(env)?;
    if !tako_config.envs.contains_key(env) {
        let available = available_environment_names(tako_config);
        let available_text = if available.is_empty() {
            "(none)".to_string()
        } else {
            available.join(", ")
        };
        return Err(format!(
            "Environment '{}' not found. Available: {}",
            env, available_text
        ));
    }
    Ok(env.to_string())
}

fn validate_non_project_delete_env(env: &str) -> Result<(), String> {
    if env == "development" {
        return Err(
            "Environment 'development' is reserved for local development and cannot be deleted."
                .to_string(),
        );
    }
    Ok(())
}

fn available_environment_names(tako_config: &TakoToml) -> Vec<String> {
    let mut names: Vec<String> = tako_config.envs.keys().cloned().collect();
    names.sort();
    names
}

fn resolve_delete_server_names(
    tako_config: &TakoToml,
    servers: &ServersToml,
    env: &str,
) -> Result<Vec<String>, String> {
    let mapped: Vec<String> = tako_config
        .get_servers_for_env(env)
        .into_iter()
        .map(|s| s.to_string())
        .collect();
    if !mapped.is_empty() {
        return Ok(mapped);
    }

    if env == "production" && servers.len() == 1 {
        let server_name = servers.names().into_iter().next().unwrap_or("<server>");
        return Ok(vec![server_name.to_string()]);
    }

    if servers.is_empty() {
        return Err(format!(
            "No servers have been added. Run 'tako servers add <host>' first, then map it in tako.toml with [servers.<name>] env = \"{}\".",
            env
        ));
    }

    Err(format!(
        "No servers configured for environment '{}'. Add [servers.<name>] with env = \"{}\" to tako.toml.",
        env, env
    ))
}

fn resolve_delete_server_names_from_deployments(
    deployments: &[RemoteDeployment],
    app_name: &str,
    env: &str,
) -> Result<Vec<String>, String> {
    let mut names: Vec<String> = deployments
        .iter()
        .filter(|d| d.app == app_name && d.env == env)
        .map(|d| d.server_name.clone())
        .collect();
    names.sort();
    names.dedup();

    if names.is_empty() {
        return Err(format!(
            "App '{}' is not deployed to environment '{}' on configured servers.",
            app_name, env
        ));
    }

    Ok(names)
}

fn validate_confirmation_mode(assume_yes: bool, interactive: bool) -> Result<(), String> {
    if !assume_yes && !interactive {
        return Err(
            "Delete requires --yes in non-interactive mode to avoid accidental removal."
                .to_string(),
        );
    }
    Ok(())
}

fn should_confirm_delete(assume_yes: bool, interactive: bool) -> bool {
    !assume_yes && interactive
}

fn should_use_per_server_delete_spinner(server_count: usize, interactive: bool) -> bool {
    interactive && server_count == 1
}

async fn delete_from_server(
    server: &ServerEntry,
    app_name: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let ssh_config = SshConfig::from_server(&server.host, server.port);
    let mut ssh = SshClient::new(ssh_config);
    ssh.connect().await?;

    let result = async {
        let cmd = Command::Delete {
            app: app_name.to_string(),
        };
        let json = serde_json::to_string(&cmd)?;
        let response_raw = ssh.tako_command(&json).await?;
        let response: Response = serde_json::from_str(&response_raw)?;
        parse_delete_response(response)
            .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { e.into() })?;

        let remote_app_root = format!("/opt/tako/apps/{}", app_name);
        let cleanup_cmd = format!("rm -rf {}", shell_single_quote(&remote_app_root));
        ssh.exec_checked(&cleanup_cmd).await?;

        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
    }
    .await;

    let _ = ssh.disconnect().await;
    result
}

fn shell_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn parse_delete_response(response: Response) -> Result<(), String> {
    match response {
        Response::Ok { .. } => Ok(()),
        Response::Error { message } => Err(format!("tako-server error (delete): {}", message)),
    }
}

fn format_delete_confirm_prompt(app_name: &str, env: &str, server_count: usize) -> String {
    format!(
        "Please confirm you want to remove application {} from {} on {} server(s).",
        output::emphasized(app_name),
        output::emphasized(env),
        server_count
    )
}

fn format_delete_confirm_hint(app_name: &str, server_names: &[String]) -> String {
    let app = output::emphasized(app_name);
    if server_names.len() == 1 {
        return format!(
            "This removes application {} from {}.",
            app,
            output::emphasized(&server_names[0])
        );
    }

    let servers = server_names
        .iter()
        .map(|name| output::emphasized(name))
        .collect::<Vec<_>>()
        .join(", ");
    format!("This removes application {} from {}.", app, servers)
}

fn format_delete_summary_warning(success_count: usize, total_servers: usize) -> String {
    if success_count == 0 {
        return format!(
            "Deletion failed: {}/{} servers succeeded",
            success_count, total_servers
        );
    }

    format!(
        "Deletion partially failed: {}/{} servers succeeded",
        success_count, total_servers
    )
}

fn format_server_delete_target(name: &str, entry: &ServerEntry) -> String {
    format!("{name} (tako@{}:{})", entry.host, entry.port)
}

fn format_server_delete_success(name: &str, entry: &ServerEntry) -> String {
    format!(
        "{} deleted successfully",
        format_server_delete_target(name, entry)
    )
}

fn format_server_delete_success_for_output(
    name: &str,
    entry: &ServerEntry,
    verbose: bool,
) -> String {
    if verbose {
        return format_server_delete_success(name, entry);
    }
    format!("{name} deleted")
}

fn format_server_delete_failure(name: &str, entry: &ServerEntry, error: &str) -> String {
    format!(
        "{} delete failed: {}",
        format_server_delete_target(name, entry),
        error
    )
}

fn format_server_delete_failure_for_output(
    name: &str,
    entry: &ServerEntry,
    error: &str,
    verbose: bool,
) -> String {
    if verbose {
        return format_server_delete_failure(name, entry, error);
    }
    format!("{name} delete failed: {error}")
}

fn format_server_not_found_error(server_name: &str) -> String {
    format!(
        "Server '{}' not found in ~/.tako/config.toml [[servers]]. Run 'tako servers add --name {} <host>'.",
        server_name, server_name
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ServerConfig;

    fn server_entry(host: &str) -> ServerEntry {
        ServerEntry {
            host: host.to_string(),
            port: 22,
            description: None,
        }
    }

    fn deployment(app: &str, env: &str, server: &str) -> RemoteDeployment {
        RemoteDeployment {
            app: app.to_string(),
            env: env.to_string(),
            server_name: server.to_string(),
        }
    }

    #[test]
    fn app_env_options_filters_by_app_and_counts_servers() {
        let deployments = vec![
            deployment("web", "production", "s1"),
            deployment("web", "production", "s2"),
            deployment("web", "staging", "s1"),
            deployment("api", "production", "s1"),
        ];

        let options = app_env_options(&deployments, Some("web"));
        assert_eq!(
            options,
            vec![
                AppEnvOption {
                    app: "web".to_string(),
                    env: "production".to_string(),
                    server_count: 2,
                },
                AppEnvOption {
                    app: "web".to_string(),
                    env: "staging".to_string(),
                    server_count: 1,
                },
            ]
        );
    }

    #[test]
    fn resolve_app_for_env_without_project_uses_single_app_automatically() {
        let deployments = vec![
            deployment("web", "production", "s1"),
            deployment("web", "production", "s2"),
        ];

        let app = resolve_app_for_env_without_project(&deployments, "production", false).unwrap();
        assert_eq!(app, "web");
    }

    #[test]
    fn should_use_per_server_delete_spinner_only_for_single_interactive_target() {
        assert!(should_use_per_server_delete_spinner(1, true));
        assert!(!should_use_per_server_delete_spinner(2, true));
        assert!(!should_use_per_server_delete_spinner(1, false));
    }

    #[test]
    fn server_delete_success_message_is_compact_by_default() {
        let entry = server_entry("example.com");
        let message = format_server_delete_success_for_output("prod", &entry, false);
        assert_eq!(message, "prod deleted");
    }

    #[test]
    fn server_delete_success_message_includes_target_in_verbose_mode() {
        let entry = server_entry("example.com");
        let message = format_server_delete_success_for_output("prod", &entry, true);
        assert_eq!(message, "prod (tako@example.com:22) deleted successfully");
    }

    #[test]
    fn server_delete_failure_message_is_compact_by_default() {
        let entry = server_entry("example.com");
        let message = format_server_delete_failure_for_output("prod", &entry, "boom", false);
        assert_eq!(message, "prod delete failed: boom");
    }

    #[test]
    fn resolve_app_for_env_without_project_requires_interactive_when_multiple_apps() {
        let deployments = vec![
            deployment("web", "production", "s1"),
            deployment("api", "production", "s2"),
        ];

        let err = resolve_app_for_env_without_project(&deployments, "production", false)
            .unwrap_err()
            .to_string();
        assert!(err.contains("Multiple apps are deployed"));
    }

    #[test]
    fn resolve_delete_server_names_from_deployments_filters_by_app_and_env() {
        let deployments = vec![
            deployment("web", "production", "s1"),
            deployment("web", "production", "s2"),
            deployment("web", "staging", "s3"),
            deployment("api", "production", "s4"),
        ];

        let servers =
            resolve_delete_server_names_from_deployments(&deployments, "web", "production")
                .unwrap();
        assert_eq!(servers, vec!["s1".to_string(), "s2".to_string()]);
    }

    #[test]
    fn validate_project_delete_env_rejects_development() {
        let config = TakoToml::default();
        let err = validate_project_delete_env("development", &config).unwrap_err();
        assert!(err.contains("reserved"));
    }

    #[test]
    fn validate_project_delete_env_requires_known_env() {
        let mut config = TakoToml::default();
        config
            .envs
            .insert("production".to_string(), Default::default());

        let err = validate_project_delete_env("staging", &config).unwrap_err();
        assert!(err.contains("Environment 'staging' not found"));
    }

    #[test]
    fn resolve_delete_server_names_prefers_env_mapping() {
        let mut config = TakoToml::default();
        config
            .envs
            .insert("production".to_string(), Default::default());
        config.servers.insert(
            "prod-a".to_string(),
            ServerConfig {
                env: "production".to_string(),
                ..Default::default()
            },
        );

        let mut servers = ServersToml::default();
        servers
            .servers
            .insert("prod-a".to_string(), server_entry("1.2.3.4"));
        servers
            .servers
            .insert("fallback".to_string(), server_entry("5.6.7.8"));

        let names = resolve_delete_server_names(&config, &servers, "production").unwrap();
        assert_eq!(names, vec!["prod-a".to_string()]);
    }

    #[test]
    fn resolve_delete_server_names_uses_single_production_fallback() {
        let mut config = TakoToml::default();
        config
            .envs
            .insert("production".to_string(), Default::default());

        let mut servers = ServersToml::default();
        servers
            .servers
            .insert("solo".to_string(), server_entry("127.0.0.1"));

        let names = resolve_delete_server_names(&config, &servers, "production").unwrap();
        assert_eq!(names, vec!["solo".to_string()]);
    }

    #[test]
    fn resolve_delete_server_names_errors_without_mapping_for_non_production() {
        let mut config = TakoToml::default();
        config
            .envs
            .insert("staging".to_string(), Default::default());

        let mut servers = ServersToml::default();
        servers
            .servers
            .insert("solo".to_string(), server_entry("127.0.0.1"));

        let err = resolve_delete_server_names(&config, &servers, "staging").unwrap_err();
        assert!(err.contains("No servers configured for environment 'staging'"));
    }

    #[test]
    fn resolve_delete_server_names_errors_when_no_servers_exist() {
        let mut config = TakoToml::default();
        config
            .envs
            .insert("staging".to_string(), Default::default());

        let servers = ServersToml::default();
        let err = resolve_delete_server_names(&config, &servers, "staging").unwrap_err();
        assert!(err.contains("No servers have been added"));
        assert!(err.contains("env = \"staging\""));
    }

    #[test]
    fn parse_delete_response_converts_error_response() {
        let err = parse_delete_response(Response::error("boom")).unwrap_err();
        assert!(err.contains("boom"));
    }

    #[test]
    fn format_delete_confirm_hint_uses_app_and_single_server() {
        let hint = format_delete_confirm_hint("bun-example", &[String::from("prod-1")]);
        assert_eq!(
            hint,
            "This removes application 'bun-example' from 'prod-1'."
        );
    }

    #[test]
    fn format_delete_confirm_hint_lists_multiple_servers() {
        let hint = format_delete_confirm_hint(
            "bun-example",
            &[String::from("prod-1"), String::from("prod-2")],
        );
        assert_eq!(
            hint,
            "This removes application 'bun-example' from 'prod-1', 'prod-2'."
        );
    }

    #[test]
    fn format_delete_confirm_prompt_uses_confirm_wording() {
        let prompt = format_delete_confirm_prompt("bun-example", "production", 2);
        assert_eq!(
            prompt,
            "Please confirm you want to remove application 'bun-example' from 'production' on 2 server(s)."
        );
    }

    #[test]
    fn format_delete_summary_warning_uses_all_servers_failed_when_none_succeed() {
        let warning = format_delete_summary_warning(0, 1);
        assert_eq!(warning, "Deletion failed: 0/1 servers succeeded");
    }

    #[test]
    fn format_delete_summary_warning_uses_partial_when_some_succeed() {
        let warning = format_delete_summary_warning(1, 2);
        assert_eq!(warning, "Deletion partially failed: 1/2 servers succeeded");
    }

    #[test]
    fn validate_confirmation_mode_requires_yes_for_non_interactive() {
        let err = validate_confirmation_mode(false, false).unwrap_err();
        assert!(err.contains("--yes"));
    }

    #[test]
    fn parse_server_env_from_tako_toml_matches_named_server() {
        let content = r#"
[servers.prod]
env = "production"

[servers.staging]
env = "staging"
"#;

        let env = parse_server_env_from_tako_toml(content, "prod");
        assert_eq!(env.as_deref(), Some("production"));
    }

    #[test]
    fn parse_server_env_from_tako_toml_falls_back_to_single_env() {
        let content = r#"
[servers.a]
env = "production"

[servers.b]
env = "production"
"#;

        let env = parse_server_env_from_tako_toml(content, "missing");
        assert_eq!(env.as_deref(), Some("production"));
    }
}

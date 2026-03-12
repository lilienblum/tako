use std::env::current_dir;

use crate::app::require_app_name_from_config;
use crate::commands::helpers::{resolve_servers_for_env, validate_server_names};
use crate::config::{ServerEntry, ServersToml, TakoToml};
use crate::output;
use crate::ssh::SshClient;
use tako_core::{Command, Response};

pub fn run(
    instances: u8,
    env: Option<&str>,
    server: Option<&str>,
    app: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(run_async(instances, env, server, app))
}

async fn run_async(
    instances: u8,
    env: Option<&str>,
    server: Option<&str>,
    app: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    let project_dir = current_dir()?;
    let project_tako = if TakoToml::exists_in_dir(&project_dir) {
        Some(TakoToml::load_from_dir(&project_dir)?)
    } else {
        None
    };

    let app_name = resolve_app_name(project_tako.as_ref(), &project_dir, app)?;
    let servers = ServersToml::load()?;
    let server_names = resolve_scale_server_names(project_tako.as_ref(), &servers, env, server)?;

    output::section("Scale");
    output::step(&format!(
        "{} -> {} instance(s)",
        output::strong(&app_name),
        output::strong(&instances.to_string())
    ));

    let mut tasks = Vec::new();
    for server_name in &server_names {
        let Some(entry) = servers.get(server_name) else {
            return Err(format!("Server '{}' not found in ~/.tako/config.toml", server_name).into());
        };
        let server_name = server_name.clone();
        let entry = entry.clone();
        let app_name = app_name.clone();
        tasks.push(tokio::spawn(async move {
            let result = scale_server(&app_name, &entry, instances).await;
            (server_name, result)
        }));
    }

    let results = if output::is_interactive() && tasks.len() > 1 {
        output::with_spinner_async_simple(
            &format!("Scaling {} server(s)", tasks.len()),
            async {
                let mut results = Vec::new();
                for task in tasks {
                    results.push(task.await);
                }
                results
            },
        )
        .await
    } else {
        let mut results = Vec::new();
        for task in tasks {
            results.push(task.await);
        }
        results
    };

    let mut failures = Vec::new();
    for result in results {
        match result {
            Ok((server_name, Ok(scale_result))) => {
                output::bullet(&format!(
                    "{}: {} instance(s)",
                    output::strong(&server_name),
                    scale_result.instances
                ));
                if scale_result.worker_limited {
                    output::warning(&format!(
                        "{}: worker mode limited scale to {} instance(s)",
                        output::strong(&server_name),
                        scale_result.instances
                    ));
                }
            }
            Ok((server_name, Err(error))) => {
                output::error(&format!("{}: {}", output::strong(&server_name), error));
                failures.push(server_name);
            }
            Err(error) => {
                output::error(&format!("Scale task failed: {}", error));
                failures.push("<task>".to_string());
            }
        }
    }

    if failures.is_empty() {
        output::success("Scale");
        Ok(())
    } else {
        Err(format!("Failed to scale {} server(s)", failures.len()).into())
    }
}

fn resolve_app_name(
    project_tako: Option<&TakoToml>,
    project_dir: &std::path::Path,
    explicit_app: Option<&str>,
) -> Result<String, Box<dyn std::error::Error>> {
    if let Some(tako_config) = project_tako {
        let resolved = require_app_name_from_config(project_dir)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidInput, error.to_string()))?;
        if let Some(app_name) = explicit_app
            && app_name != resolved
        {
            return Err(format!(
                "--app '{}' does not match project app '{}'",
                app_name, resolved
            )
            .into());
        }
        let _ = tako_config;
        return Ok(resolved);
    }

    explicit_app
        .map(str::to_string)
        .ok_or_else(|| "Run `tako scale` from a project directory or pass --app.".into())
}

fn resolve_scale_server_names(
    project_tako: Option<&TakoToml>,
    servers: &ServersToml,
    env: Option<&str>,
    server: Option<&str>,
) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    if let Some(server_name) = server {
        if !servers.contains(server_name) {
            return Err(format!("Server '{}' not found in ~/.tako/config.toml", server_name).into());
        }
        if let Some(env_name) = env {
            let tako_config = project_tako.ok_or_else(|| {
                "Using --env requires project context because environment mappings live in tako.toml."
                    .to_string()
            })?;
            if !tako_config.envs.contains_key(env_name) {
                return Err(format!("Environment '{}' not found in tako.toml.", env_name).into());
            }
            if !tako_config.get_servers_for_env(env_name).contains(&server_name) {
                return Err(format!(
                    "Server '{}' is not configured for environment '{}'.",
                    server_name, env_name
                )
                .into());
            }
        }
        return Ok(vec![server_name.to_string()]);
    }

    let env_name = env.ok_or_else(|| {
        "Pass --env when --server is omitted.".to_string()
    })?;
    let tako_config = project_tako.ok_or_else(|| {
        "Scaling by environment requires project context because environment mappings live in tako.toml."
            .to_string()
    })?;
    if !tako_config.envs.contains_key(env_name) {
        return Err(format!("Environment '{}' not found in tako.toml.", env_name).into());
    }

    let mut names = resolve_servers_for_env(tako_config, servers, env_name)?;
    names.sort();
    names.dedup();
    validate_server_names(&names, servers)?;
    Ok(names)
}

#[derive(Debug)]
struct ScaleResult {
    instances: u8,
    worker_limited: bool,
}

async fn scale_server(
    app_name: &str,
    server: &ServerEntry,
    instances: u8,
) -> Result<ScaleResult, Box<dyn std::error::Error + Send + Sync>> {
    let mut ssh = SshClient::connect_to(&server.host, server.port).await?;
    let command = serde_json::to_string(&Command::Scale {
        app: app_name.to_string(),
        instances,
    })
    .map_err(|error| format!("Failed to serialize scale command: {error}"))?;

    let response_raw = ssh.tako_command(&command).await?;
    ssh.disconnect().await?;

    match serde_json::from_str::<Response>(&response_raw)
        .map_err(|error| format!("Invalid response from tako-server: {error}"))?
    {
        Response::Ok { data } => Ok(ScaleResult {
            instances: data
                .get("instances")
                .and_then(|value| value.as_u64())
                .and_then(|value| u8::try_from(value).ok())
                .unwrap_or(instances),
            worker_limited: data
                .get("worker_limited")
                .and_then(|value| value.as_bool())
                .unwrap_or(false),
        }),
        Response::Error { message } => Err(message.into()),
    }
}

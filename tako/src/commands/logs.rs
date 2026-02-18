use std::env::current_dir;
use std::io::{Write, stdout};
use std::sync::Arc;

use crate::app::resolve_app_name;
use crate::commands::server;
use crate::config::{ServersToml, TakoToml};
use crate::output;
use crate::ssh::{SshClient, SshConfig};

pub fn run(env: &str) -> Result<(), Box<dyn std::error::Error>> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(run_async(env))
}

async fn run_async(env: &str) -> Result<(), Box<dyn std::error::Error>> {
    let project_dir = current_dir()?;

    // Load configuration
    let tako_config = TakoToml::load_from_dir(&project_dir)?;
    let mut servers = ServersToml::load()?;

    // Resolve app name from config or project directory fallback.
    let app_name = resolve_app_name(&project_dir)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e.to_string()))?;

    // Check environment exists
    if !tako_config.envs.contains_key(env) {
        let available: Vec<_> = tako_config.envs.keys().collect();
        return Err(format!(
            "Environment '{}' not found. Available: {}",
            env,
            if available.is_empty() {
                "(none)".to_string()
            } else {
                available
                    .into_iter()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            }
        )
        .into());
    }

    let server_names = resolve_log_server_names(&tako_config, &mut servers, env).await?;

    output::section("Logs");
    output::step(&format!(
        "Streaming logs for {} in {} (Ctrl+c to stop)",
        output::emphasized(&app_name),
        output::emphasized(env)
    ));

    // Stream logs from all servers in parallel.
    let write_lock = Arc::new(std::sync::Mutex::new(()));
    let mut tasks = Vec::new();
    for server_name in server_names {
        let Some(server) = servers.get(&server_name) else {
            return Err(format!(
                "Server '{}' not found in ~/.tako/config.toml [[servers]]. Run 'tako servers add --name {} <host>'.",
                server_name, server_name
            )
            .into());
        };

        let server_name = server_name.to_string();
        let host = server.host.clone();
        let port = server.port;
        let app_name = app_name.clone();
        let write_lock = write_lock.clone();

        tasks.push(tokio::spawn(async move {
            let ssh_config = SshConfig::from_server(&host, port);
            let mut ssh = SshClient::new(ssh_config);
            ssh.connect().await?;

            // Prefix chunks so multi-server output is readable.
            let prefix = format!("[{}] ", server_name);

            let log_cmd = format!(
                "sudo journalctl -u tako-server -f --no-pager -o cat 2>/dev/null | grep -E '(\\[{}\\]|app={})' || tail -f /opt/tako/apps/{}/shared/logs/*.log 2>/dev/null || echo 'No logs available'",
                app_name, app_name, app_name
            );

            let exit_code = ssh
                .exec_streaming(
                    &log_cmd,
                    |data| {
                        let _guard = write_lock.lock();
                        let _ = stdout().write_all(prefix.as_bytes());
                        let _ = stdout().write_all(data);
                        let _ = stdout().flush();
                    },
                    |data| {
                        let _guard = write_lock.lock();
                        let _ = stdout().write_all(prefix.as_bytes());
                        let _ = stdout().write_all(data);
                        let _ = stdout().flush();
                    },
                )
                .await?;

            let _ = exit_code;
            ssh.disconnect().await?;
            Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
        }));
    }

    // Wait for any task to finish; Ctrl+c usually stops all.
    for t in tasks {
        let _ = t.await;
    }

    Ok(())
}

async fn resolve_log_server_names(
    tako_config: &TakoToml,
    servers: &mut ServersToml,
    env: &str,
) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let mapped: Vec<String> = tako_config
        .get_servers_for_env(env)
        .into_iter()
        .map(|s| s.to_string())
        .collect();
    if !mapped.is_empty() {
        return Ok(mapped);
    }

    if env == "production" && servers.len() == 1 {
        let only = servers.names().into_iter().next().unwrap_or("<server>");
        let confirmed = output::confirm(
            &format!(
                "No [servers.*] mapping for 'production'. Stream logs from the only configured server ('{}')?",
                only
            ),
            true,
        )?;
        if confirmed {
            return Ok(vec![only.to_string()]);
        }
        return Err(
            "Logs cancelled. Add [servers.<name>] with env = \"production\" to tako.toml.".into(),
        );
    }

    if env == "production" && servers.is_empty() {
        if server::prompt_to_add_server(
            "No servers have been added. Logs need at least one server.",
        )
        .await?
        .is_some()
        {
            *servers = ServersToml::load()?;
            if servers.len() == 1 {
                let only = servers.names().into_iter().next().unwrap_or("<server>");
                return Ok(vec![only.to_string()]);
            }
        }
        return Err(
            "No servers have been added. Run 'tako servers add <host>' first, then map it in tako.toml with [servers.<name>] env = \"production\"."
                .into(),
        );
    }

    Err(format!(
        "No servers configured for environment '{}'. Add [servers.<name>] with env = \"{}\" to tako.toml.",
        env, env
    )
    .into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ServerEntry;

    #[tokio::test]
    async fn resolve_log_server_names_uses_single_production_server_fallback() {
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

        let names = resolve_log_server_names(&tako_config, &mut servers, "production")
            .await
            .expect("should resolve with fallback");
        assert_eq!(names, vec!["solo".to_string()]);
    }

    #[tokio::test]
    async fn resolve_log_server_names_errors_for_non_production_without_mapping() {
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

        let err = resolve_log_server_names(&tako_config, &mut servers, "staging")
            .await
            .expect_err("should fail for non-production");
        assert!(
            err.to_string()
                .contains("No servers configured for environment 'staging'")
        );
    }
}

use crate::output;
use clap::Subcommand;

#[derive(Subcommand)]
pub enum ServerCommands {
    /// Add a new server
    Add {
        /// Server host (IP or hostname). Omit to use the interactive setup wizard.
        host: Option<String>,

        /// Server name
        #[arg(long)]
        name: Option<String>,

        /// Optional description shown in server lists (e.g. "Primary EU region")
        #[arg(long)]
        description: Option<String>,

        /// SSH port
        #[arg(long, default_value_t = 22)]
        port: u16,

        /// Skip SSH connection test
        #[arg(long)]
        no_test: bool,
    },

    /// Remove a server
    #[command(visible_aliases = ["remove", "delete"])]
    Rm {
        /// Server name (omit to choose interactively)
        name: Option<String>,
    },

    /// List all servers
    #[command(visible_alias = "list")]
    Ls,

    /// Restart tako-server on a server
    Restart {
        /// Server name
        name: String,
    },

    /// Reload configuration on a server
    Reload {
        /// Server name
        name: String,
    },

    /// Show status of tako-server on a server
    Status {
        /// Server name
        name: String,
    },
}

pub fn run(cmd: ServerCommands) -> Result<(), Box<dyn std::error::Error>> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(run_async(cmd))
}

async fn run_async(cmd: ServerCommands) -> Result<(), Box<dyn std::error::Error>> {
    match cmd {
        ServerCommands::Add {
            host,
            name,
            description,
            port,
            no_test,
        } => {
            if let Some(host) = host {
                let Some(name) = name.as_deref() else {
                    return Err(
                        "Server name is required when adding with a host. Use --name <name>, or run 'tako servers add' to use the interactive wizard."
                            .into(),
                    );
                };
                let _ =
                    add_server(&host, Some(name), description.as_deref(), port, no_test).await?;
                Ok(())
            } else {
                let _ =
                    run_add_server_wizard(name.as_deref(), description.as_deref(), port, !no_test)
                        .await?;
                Ok(())
            }
        }
        ServerCommands::Rm { name } => remove_server(name.as_deref()).await,
        ServerCommands::Ls => list_servers().await,
        ServerCommands::Restart { name } => restart_server(&name).await,
        ServerCommands::Reload { name } => reload_server(&name).await,
        ServerCommands::Status { name } => status_server(&name).await,
    }
}

pub async fn prompt_to_add_server(
    reason: &str,
) -> Result<Option<String>, Box<dyn std::error::Error>> {
    if !output::is_interactive() {
        return Ok(None);
    }

    output::warning(reason);

    let should_add = output::confirm("Add a server now?", true)?;
    if !should_add {
        output::warning("Cancelled.");
        return Ok(None);
    }

    run_add_server_wizard(None, None, 22, true).await
}

fn append_unique_suggestions(target: &mut Vec<String>, source: &[String]) {
    for value in source {
        push_unique_suggestion(target, value.clone());
    }
}

fn push_unique_suggestion(values: &mut Vec<String>, value: String) {
    if value.is_empty() {
        return;
    }
    if values.iter().any(|existing| existing == &value) {
        return;
    }
    values.push(value);
}

async fn run_add_server_wizard(
    initial_name: Option<&str>,
    initial_description: Option<&str>,
    initial_port: u16,
    default_test_ssh: bool,
) -> Result<Option<String>, Box<dyn std::error::Error>> {
    use crate::config::{CliHistoryToml, ServersToml};

    if !output::is_interactive() {
        return Err(
            "Interactive server setup requires a terminal. Run: tako servers add <host>".into(),
        );
    }

    let existing_servers = ServersToml::load()?;
    let suggestion_history = CliHistoryToml::load().unwrap_or_default();

    let host_suggestions = suggestion_history.server_host_suggestions();
    let mut name_suggestions = suggestion_history.server_name_suggestions();
    let mut port_suggestions = suggestion_history.server_port_suggestions();
    let mut host_suggestions = host_suggestions;

    for server_name in existing_servers.names() {
        if let Some(server) = existing_servers.get(server_name) {
            push_unique_suggestion(&mut host_suggestions, server.host.clone());
            push_unique_suggestion(&mut name_suggestions, server_name.to_string());
            push_unique_suggestion(&mut port_suggestions, server.port.to_string());
        }
    }

    push_unique_suggestion(&mut port_suggestions, String::from("22"));
    push_unique_suggestion(&mut port_suggestions, initial_port.to_string());

    if let Some(initial_name) = initial_name
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        push_unique_suggestion(&mut name_suggestions, initial_name.to_string());
    }

    output::muted("Tip: press Tab for autocomplete suggestions.");

    loop {
        let host = output::prompt_input_with_suggestions(
            "Server host (IP or hostname)",
            false,
            None,
            &host_suggestions,
        )?;
        let host = host.trim().to_string();
        if host.is_empty() {
            return Err("Server host cannot be empty".into());
        }

        let mut name_prompt_suggestions =
            suggestion_history.server_name_suggestions_for_host(&host);
        for server_name in existing_servers.names() {
            if let Some(server) = existing_servers.get(server_name)
                && server.host == host
            {
                push_unique_suggestion(&mut name_prompt_suggestions, server_name.to_string());
            }
        }
        append_unique_suggestions(&mut name_prompt_suggestions, &name_suggestions);
        push_unique_suggestion(&mut name_prompt_suggestions, host.clone());
        let name = output::prompt_input_with_suggestions(
            "Server name",
            false,
            initial_name,
            &name_prompt_suggestions,
        )?;
        let name = name.trim().to_string();

        let description =
            output::prompt_input("Description (optional)", true, initial_description)?;
        let description = description.trim().to_string();

        let default_port = initial_port.to_string();
        let mut port_prompt_suggestions =
            suggestion_history.server_port_suggestions_for(&host, &name);
        for server_name in existing_servers.names() {
            if let Some(server) = existing_servers.get(server_name)
                && server.host == host
                && server_name == name
            {
                push_unique_suggestion(&mut port_prompt_suggestions, server.port.to_string());
            }
        }
        for server_name in existing_servers.names() {
            if let Some(server) = existing_servers.get(server_name)
                && server.host == host
            {
                push_unique_suggestion(&mut port_prompt_suggestions, server.port.to_string());
            }
        }
        for server_name in existing_servers.names() {
            if let Some(server) = existing_servers.get(server_name)
                && server_name == name
            {
                push_unique_suggestion(&mut port_prompt_suggestions, server.port.to_string());
            }
        }
        append_unique_suggestions(&mut port_prompt_suggestions, &port_suggestions);
        let port_raw = output::prompt_input_with_suggestions(
            "SSH port",
            false,
            Some(&default_port),
            &port_prompt_suggestions,
        )?;
        let port: u16 = port_raw
            .trim()
            .parse()
            .map_err(|_| format!("Invalid SSH port '{}'", port_raw.trim()))?;

        output::step(&format!("Host: {}", host));
        output::step(&format!("Server name: {}", &name));
        if !description.is_empty() {
            output::step(&format!("Description: {}", description));
        }
        output::step(&format!("SSH port: {}", port));
        if default_test_ssh {
            output::muted("SSH connection test will run before saving.");
        } else {
            output::muted("SSH connection test is disabled.");
        }

        if !output::confirm("Looks good?", true)? {
            output::warning("Okay, let's try that again.");
            continue;
        }

        let name_ref = Some(name.as_str());
        let description_ref = if description.is_empty() {
            None
        } else {
            Some(description.as_str())
        };

        return add_server(&host, name_ref, description_ref, port, !default_test_ssh).await;
    }
}

pub async fn add_server(
    host: &str,
    name: Option<&str>,
    description: Option<&str>,
    port: u16,
    no_test: bool,
) -> Result<Option<String>, Box<dyn std::error::Error>> {
    use crate::config::{ServerEntry, ServersToml};
    use crate::ssh::{SshClient, SshConfig};

    let mut servers = ServersToml::load()?;
    let normalized_description = description.and_then(|d| {
        let trimmed = d.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    });

    let server_name = name
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or(
            "Server name is required when adding with a host. Use --name <name>, or run 'tako servers add' to use the interactive wizard.",
        )?
        .to_string();

    // Check if host already exists
    if let Some(existing_name) = servers.find_by_host(host) {
        let existing_name = existing_name.to_string();
        let existing = servers
            .get(&existing_name)
            .cloned()
            .ok_or_else(|| format!("Server '{}' vanished during lookup", existing_name))?;

        if existing_name == server_name && existing.port == port {
            if normalized_description.is_some()
                && existing.description.as_deref() != normalized_description.as_deref()
            {
                servers.update(
                    &existing_name,
                    ServerEntry {
                        host: existing.host,
                        port: existing.port,
                        description: normalized_description.clone(),
                    },
                )?;
                servers.save()?;
                output::success(&format!(
                    "Updated description for server {} (tako@{}:{})",
                    output::emphasized(&server_name),
                    host,
                    port
                ));
                record_server_history(host, &server_name, port);
                return Ok(Some(server_name));
            }

            output::success(&format!(
                "Server {} is already configured (tako@{}:{})",
                output::emphasized(&server_name),
                host,
                port
            ));
            record_server_history(host, &server_name, port);
            return Ok(Some(server_name));
        }

        let confirm = output::confirm(
            &format!(
                "Host {} already exists as {}. Override?",
                output::emphasized(host),
                output::emphasized(&existing_name)
            ),
            false,
        )?;

        if !confirm {
            output::warning("Cancelled");
            return Ok(None);
        }

        servers.remove(&existing_name)?;
    }

    // Check if name already exists (with different host)
    if servers.contains(&server_name) {
        return Err(format!(
            "Server name '{}' already exists. Use --name to specify a different name.",
            server_name
        )
        .into());
    }

    // Test SSH connection unless skipped
    if !no_test {
        let ssh_config = SshConfig::from_server(host, port);
        let mut ssh = SshClient::new(ssh_config);

        let connect_result = output::with_spinner_async(
            format!("Testing SSH connection to tako@{}:{}", host, port),
            ssh.connect(),
        )
        .await
        .map_err(|e| format!("SSH connection failed: {}", e))?;

        match connect_result {
            Ok(()) => {
                output::success("SSH connection successful");

                // Check if tako-server is installed
                match ssh.is_tako_installed().await {
                    Ok(true) => {
                        if let Ok(Some(version)) = ssh.tako_version().await {
                            output::success(&format!("tako-server installed ({})", version));
                        } else {
                            output::success("tako-server installed");
                        }
                    }
                    Ok(false) => {
                        output::warning("tako-server not installed");
                        output::muted(
                            "Install it on the server as root (see scripts/install-tako-server.sh), then re-run deploy.",
                        );
                    }
                    Err(e) => output::warning(&format!("Could not check tako-server: {}", e)),
                }

                ssh.disconnect().await?;
            }
            Err(e) => {
                return Err(format!("SSH connection failed: {}", e).into());
            }
        }
    }

    // Add the server
    let entry = ServerEntry {
        host: host.to_string(),
        port,
        description: normalized_description.clone(),
    };

    servers.add(server_name.clone(), entry)?;
    servers.save()?;

    output::success(&format!(
        "Added server {} (tako@{}:{})",
        output::emphasized(&server_name),
        host,
        port
    ));
    if let Some(desc) = normalized_description
        && !desc.trim().is_empty()
    {
        output::muted(&format!("Description: {}", desc));
    }
    record_server_history(host, &server_name, port);

    Ok(Some(server_name))
}

fn record_server_history(host: &str, name: &str, port: u16) {
    let mut history = crate::config::CliHistoryToml::load().unwrap_or_default();
    history.record_server_prompt_values(host, name, port);
    if let Err(e) = history.save()
        && output::is_verbose()
    {
        output::warning(&format!("Could not save CLI history: {}", e));
    }
}

fn removal_option_label(name: &str, entry: &crate::config::ServerEntry) -> String {
    match entry.description.as_deref().map(str::trim) {
        Some(description) if !description.is_empty() => {
            format!("{name} ({description})  tako@{}:{}", entry.host, entry.port)
        }
        _ => format!("{name}  tako@{}:{}", entry.host, entry.port),
    }
}

async fn remove_server(name: Option<&str>) -> Result<(), Box<dyn std::error::Error>> {
    use crate::config::ServersToml;

    let mut servers = ServersToml::load()?;

    if servers.is_empty() {
        return Err("No servers configured. Run 'tako servers add <host>' first.".into());
    }

    let name = match name {
        Some(name) => {
            if !servers.contains(name) {
                return Err(format!("Server '{}' not found.", name).into());
            }
            name.to_string()
        }
        None => {
            if !output::is_interactive() {
                return Err(
                    "No server name provided and selection requires an interactive terminal. Run 'tako servers rm <name>'."
                        .into(),
                );
            }

            let mut names = servers.names();
            names.sort_unstable();
            let options: Vec<(String, String)> = names
                .into_iter()
                .filter_map(|server_name| {
                    servers.get(server_name).map(|entry| {
                        (
                            removal_option_label(server_name, entry),
                            server_name.to_string(),
                        )
                    })
                })
                .collect();

            output::select(
                "Select server to remove",
                Some("Choose a server and press Enter."),
                options,
            )?
        }
    };

    let confirm = output::confirm(
        &format!("Remove server {}?", output::emphasized(&name)),
        false,
    )?;

    if !confirm {
        output::warning("Cancelled");
        return Ok(());
    }

    servers.remove(&name)?;
    servers.save()?;

    output::success(&format!("Removed server {}", output::emphasized(&name)));

    Ok(())
}

async fn list_servers() -> Result<(), Box<dyn std::error::Error>> {
    use crate::config::ServersToml;

    let servers = ServersToml::load()?;

    if servers.is_empty() {
        output::warning("No servers configured");
        output::muted(&format!(
            "Run {} to add a server.",
            output::emphasized("tako servers add")
        ));
        return Ok(());
    }

    print_servers_table(&servers);
    Ok(())
}

fn print_servers_table(servers: &crate::config::ServersToml) {
    println!("{:<20} {:<30} {:<6} DESCRIPTION", "NAME", "HOST", "PORT");
    println!("{}", "-".repeat(92));

    for name in servers.names() {
        if let Some(entry) = servers.get(name) {
            println!(
                "{:<20} {:<30} {:<6} {}",
                name,
                entry.host,
                entry.port,
                entry.description.as_deref().unwrap_or("")
            );
        }
    }
}

async fn restart_server(name: &str) -> Result<(), Box<dyn std::error::Error>> {
    use crate::config::ServersToml;
    use crate::ssh::{SshClient, SshConfig};

    let servers = ServersToml::load()?;

    let server = servers
        .get(name)
        .ok_or_else(|| format!("Server '{}' not found.", name))?;

    let ssh_config = SshConfig::from_server(&server.host, server.port);
    let mut ssh = SshClient::new(ssh_config);
    let connect_result = output::with_spinner_async(
        format!("Connecting to {}", output::emphasized(name)),
        ssh.connect(),
    )
    .await?;
    connect_result?;

    let restart_result =
        output::with_spinner_async("Restarting tako-server...", ssh.tako_restart()).await?;

    match restart_result {
        Ok(()) => {
            output::success("tako-server restarted");

            // Wait a moment for it to come back up
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;

            // Check status
            match ssh.tako_status().await {
                Ok(status) => {
                    if status == "active" {
                        output::success("tako-server is running");
                    } else {
                        output::warning(&format!("tako-server status: {}", status));
                    }
                }
                Err(e) => {
                    output::warning(&format!("Could not check status: {}", e));
                }
            }
        }
        Err(e) => {
            output::error(&format!("Restart failed: {}", e));
            ssh.disconnect().await?;
            return Err(format!("Failed to restart tako-server: {}", e).into());
        }
    }

    ssh.disconnect().await?;

    Ok(())
}

async fn reload_server(name: &str) -> Result<(), Box<dyn std::error::Error>> {
    use crate::config::ServersToml;
    use crate::ssh::{SshClient, SshConfig};

    let servers = ServersToml::load()?;

    let server = servers
        .get(name)
        .ok_or_else(|| format!("Server '{}' not found.", name))?;

    let ssh_config = SshConfig::from_server(&server.host, server.port);
    let mut ssh = SshClient::new(ssh_config);
    let connect_result = output::with_spinner_async(
        format!("Connecting to {}", output::emphasized(name)),
        ssh.connect(),
    )
    .await?;
    connect_result?;

    let reload_result =
        output::with_spinner_async("Reloading configuration...", ssh.tako_reload(None)).await?;

    match reload_result {
        Ok(()) => {
            output::success("Configuration reloaded");
        }
        Err(e) => {
            output::error(&format!("Reload failed: {}", e));
            ssh.disconnect().await?;
            return Err(format!("Failed to reload configuration: {}", e).into());
        }
    }

    ssh.disconnect().await?;

    Ok(())
}

async fn status_server(name: &str) -> Result<(), Box<dyn std::error::Error>> {
    use crate::config::ServersToml;
    use crate::ssh::{SshClient, SshConfig};

    let servers = ServersToml::load()?;

    let server = servers
        .get(name)
        .ok_or_else(|| format!("Server '{}' not found.", name))?;

    let ssh_config = SshConfig::from_server(&server.host, server.port);
    let mut ssh = SshClient::new(ssh_config);
    let connect_result = output::with_spinner_async(
        format!("Connecting to {}", output::emphasized(name)),
        ssh.connect(),
    )
    .await?;
    connect_result?;

    output::step(&format!(
        "Server: {} (tako@{}:{})",
        name, server.host, server.port
    ));

    // Check tako-server installation
    match ssh.is_tako_installed().await {
        Ok(true) => {
            if let Ok(Some(version)) = ssh.tako_version().await {
                output::success(&format!("tako-server: {} (installed)", version));
            } else {
                output::success("tako-server: installed");
            }
        }
        Ok(false) => {
            output::warning("tako-server: not installed");
            ssh.disconnect().await?;
            return Ok(());
        }
        Err(e) => {
            output::warning(&format!("tako-server: error checking ({})", e));
        }
    }

    // Check service status
    match ssh.tako_status().await {
        Ok(status) => {
            let status_display = match status.as_str() {
                "active" => "active (running)",
                "inactive" => "inactive (stopped)",
                "failed" => "failed",
                other => other,
            };
            output::step(&format!("Service status: {}", status_display));
        }
        Err(e) => {
            output::warning(&format!("Service status: unknown ({})", e));
        }
    }

    // Get list of apps
    let list_cmd = serde_json::json!({"List": {}}).to_string();
    let apps_result =
        output::with_spinner_async("Querying apps...", ssh.tako_command(&list_cmd)).await?;

    match apps_result {
        Ok(response) => {
            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&response) {
                if let Some(apps) = parsed
                    .get("Ok")
                    .and_then(|v| v.get("apps"))
                    .and_then(|v| v.as_array())
                {
                    if apps.is_empty() {
                        output::muted("No apps deployed");
                    } else {
                        output::section("Apps");
                        println!(
                            "{:<20} {:<15} {:<10} {:<10}",
                            "APP", "VERSION", "STATE", "INSTANCES"
                        );
                        println!("{}", "-".repeat(55));
                        for app in apps {
                            let name = app.get("name").and_then(|v| v.as_str()).unwrap_or("-");
                            let version =
                                app.get("version").and_then(|v| v.as_str()).unwrap_or("-");
                            let state = app.get("state").and_then(|v| v.as_str()).unwrap_or("-");
                            let instances =
                                app.get("instances").and_then(|v| v.as_u64()).unwrap_or(0);
                            println!(
                                "{:<20} {:<15} {:<10} {:<10}",
                                name, version, state, instances
                            );
                        }
                    }
                }
            } else {
                output::warning("Could not parse app list response");
            }
        }
        Err(e) => {
            output::warning(&format!("Could not query apps: {}", e));
        }
    }

    ssh.disconnect().await?;

    Ok(())
}

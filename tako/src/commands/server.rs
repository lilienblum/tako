use crate::output;
use clap::Subcommand;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tako_core::ServerRuntimeInfo;

use crate::config::{UpgradeChannel, resolve_upgrade_channel};

const UPGRADE_SOCKET_WAIT_TIMEOUT: Duration = Duration::from_secs(120);
const UPGRADE_POLL_INTERVAL: Duration = Duration::from_millis(500);
const SERVER_INSTALL_REFRESH_HELPER: &str = "/usr/local/bin/tako-server-install-refresh";

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

    /// Upgrade tako-server with a temporary candidate process and promotion handoff
    Upgrade {
        /// Server name (omit to upgrade all servers)
        name: Option<String>,

        /// Install latest canary build instead of stable release
        #[arg(long, conflicts_with = "stable")]
        canary: bool,

        /// Install latest stable build and set default channel to stable
        #[arg(long, conflicts_with = "canary")]
        stable: bool,
    },

    /// Show global deployment status across configured servers
    #[command(visible_alias = "info")]
    Status,

    /// Configure DNS provider for wildcard TLS certificates
    DnsSetup {
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
        ServerCommands::Upgrade {
            name,
            canary,
            stable,
        } => upgrade_servers(name.as_deref(), canary, stable).await,
        ServerCommands::Status => crate::commands::status::run().await,
        ServerCommands::DnsSetup { name } => dns_setup(&name).await,
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

    // Collect existing hosts/names for filtering placeholders
    let existing_hosts: Vec<String> = existing_servers
        .names()
        .iter()
        .filter_map(|n| existing_servers.get(n).map(|s| s.host.clone()))
        .collect();

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

    // Placeholder: most recent history entry not already in servers
    let host_placeholder = host_suggestions
        .iter()
        .find(|h| !existing_hosts.contains(h))
        .cloned();

    let mut wizard = output::Wizard::new();
    let mut step = 0usize;
    let mut host = String::new();
    let mut name = String::new();
    let mut description = String::new();
    let mut port: u16 = initial_port;

    loop {
        match step {
            // Step 0: Server host
            0 => {
                let mut builder =
                    output::TextField::new("Server IP or hostname").suggestions(&host_suggestions);
                if !host.is_empty() {
                    builder = builder.with_default(&host);
                } else if let Some(ref ph) = host_placeholder {
                    builder = builder.with_placeholder(ph);
                }
                match wizard.text_field(builder) {
                    Ok(v) => {
                        let v = v.trim().to_string();
                        if v.is_empty() {
                            return Err("Server host cannot be empty".into());
                        }
                        host = v;
                        step = 1;
                    }
                    Err(e) if output::is_wizard_back(&e) => return Ok(None),
                    Err(e) => return Err(e.into()),
                }
            }
            // Step 1: Server name
            1 => {
                let mut name_prompt_suggestions =
                    suggestion_history.server_name_suggestions_for_host(&host);
                for server_name in existing_servers.names() {
                    if let Some(server) = existing_servers.get(server_name)
                        && server.host == host
                    {
                        push_unique_suggestion(
                            &mut name_prompt_suggestions,
                            server_name.to_string(),
                        );
                    }
                }
                append_unique_suggestions(&mut name_prompt_suggestions, &name_suggestions);
                push_unique_suggestion(&mut name_prompt_suggestions, host.clone());

                let default_name = if !name.is_empty() {
                    Some(name.as_str())
                } else if let Some(n) = initial_name {
                    Some(n)
                } else if let Some(ref n) = name_prompt_suggestions.first() {
                    // History/existing server matched this host
                    Some(n.as_str())
                } else if !host.chars().next().is_some_and(|c| c.is_ascii_digit())
                    && !host.contains(':')
                {
                    // Hostname (not IP) — suggest as name
                    Some(host.as_str())
                } else {
                    None
                };
                match wizard.text_field(
                    output::TextField::new("Server name")
                        .default_opt(default_name)
                        .suggestions(&name_prompt_suggestions),
                ) {
                    Ok(v) => {
                        name = v.trim().to_string();
                        step = 2;
                    }
                    Err(e) if output::is_wizard_back(&e) => step = 0,
                    Err(e) => return Err(e.into()),
                }
            }
            // Step 2: Description
            2 => {
                let default_desc = if !description.is_empty() {
                    Some(description.as_str())
                } else {
                    initial_description
                };
                match wizard.text_field(
                    output::TextField::new("Description")
                        .optional()
                        .default_opt(default_desc),
                ) {
                    Ok(v) => {
                        description = v.trim().to_string();
                        step = 3;
                    }
                    Err(e) if output::is_wizard_back(&e) => step = 1,
                    Err(e) => return Err(e.into()),
                }
            }
            // Step 3: SSH port
            3 => {
                let port_str = port.to_string();
                let mut port_prompt_suggestions =
                    suggestion_history.server_port_suggestions_for(&host, &name);
                for server_name in existing_servers.names() {
                    if let Some(server) = existing_servers.get(server_name)
                        && server.host == host
                        && server_name == name
                    {
                        push_unique_suggestion(
                            &mut port_prompt_suggestions,
                            server.port.to_string(),
                        );
                    }
                }
                for server_name in existing_servers.names() {
                    if let Some(server) = existing_servers.get(server_name)
                        && server.host == host
                    {
                        push_unique_suggestion(
                            &mut port_prompt_suggestions,
                            server.port.to_string(),
                        );
                    }
                }
                for server_name in existing_servers.names() {
                    if let Some(server) = existing_servers.get(server_name)
                        && server_name == name
                    {
                        push_unique_suggestion(
                            &mut port_prompt_suggestions,
                            server.port.to_string(),
                        );
                    }
                }
                append_unique_suggestions(&mut port_prompt_suggestions, &port_suggestions);
                match wizard.text_field(
                    output::TextField::new("SSH port")
                        .with_default(&port_str)
                        .suggestions(&port_prompt_suggestions),
                ) {
                    Ok(v) => match v.trim().parse::<u16>() {
                        Ok(p) => {
                            port = p;
                            break;
                        }
                        Err(_) => {
                            output::warning(&format!("Invalid SSH port '{}'", v.trim()));
                            // Stay on step 3 — undo the answer the wizard just set
                            wizard.undo_last();
                        }
                    },
                    Err(e) if output::is_wizard_back(&e) => step = 2,
                    Err(e) => return Err(e.into()),
                }
            }
            _ => break,
        }
    }

    let name_ref = Some(name.as_str());
    let description_ref = if description.is_empty() {
        None
    } else {
        Some(description.as_str())
    };

    add_server(&host, name_ref, description_ref, port, !default_test_ssh).await
}

pub async fn add_server(
    host: &str,
    name: Option<&str>,
    description: Option<&str>,
    port: u16,
    no_test: bool,
) -> Result<Option<String>, Box<dyn std::error::Error>> {
    use crate::config::{ServerEntry, ServerTarget, ServersToml};
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
                    output::highlight(&server_name),
                    host,
                    port
                ));
                record_server_history(host, &server_name, port);
                return Ok(Some(server_name));
            }

            output::success(&format!(
                "Server {} is already configured (tako@{}:{})",
                output::highlight(&server_name),
                host,
                port
            ));
            record_server_history(host, &server_name, port);
            return Ok(Some(server_name));
        }

        let confirm = output::confirm(
            &format!(
                "Host {} already exists as {}. Override?",
                output::highlight(host),
                output::highlight(&existing_name)
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

    struct ConnectionResult {
        target: ServerTarget,
        version: Option<String>,
        installed: bool,
    }

    let mut detected_target: Option<ServerTarget> = None;
    // Test SSH connection unless skipped
    if !no_test {
        let ssh_config = SshConfig::from_server(host, port);
        let mut ssh = SshClient::new(ssh_config);

        let result: Result<ConnectionResult, String> = output::with_spinner_async_err(
            "Connecting",
            "Connection successful",
            "Connection failed",
            async {
                ssh.connect().await.map_err(|e| e.to_string())?;

                let target = detect_server_target(&ssh)
                    .await
                    .map_err(|e| format!("Target detection failed: {e}"))?;

                let (installed, version) = match ssh.is_tako_installed().await {
                    Ok(true) => {
                        let ver = ssh.tako_version().await.ok().flatten();
                        (true, ver)
                    }
                    Ok(false) => (false, None),
                    Err(_) => (false, None),
                };

                ssh.disconnect().await.map_err(|e| e.to_string())?;

                Ok(ConnectionResult {
                    target,
                    version,
                    installed,
                })
            },
        )
        .await;

        match result {
            Ok(info) => {
                if output::is_verbose() {
                    output::muted(&format!("Target: {}", info.target.label()));
                }
                if output::is_verbose() {
                    if let Some(ref ver) = info.version {
                        let ver = ver.strip_prefix("tako-server ").unwrap_or(ver);
                        output::muted(&format!("Server version: {ver}"));
                    }
                }
                if !info.installed {
                    output::warning("tako-server not installed");
                    output::muted(
                        "Install it on the server as root (see scripts/install-tako-server.sh), then re-run deploy.",
                    );
                }
                detected_target = Some(info.target);
            }
            Err(e) => {
                return Err(e.into());
            }
        }
    } else {
        output::warning(
            "Skipped SSH test. Target metadata was not detected; deploy will fail for this server until it is re-added with SSH checks enabled.",
        );
    }

    // Add the server
    let entry = ServerEntry {
        host: host.to_string(),
        port,
        description: normalized_description.clone(),
    };

    servers.add(server_name.clone(), entry)?;
    if let Some(target) = detected_target {
        servers.set_target(&server_name, target)?;
    }
    servers.save()?;

    output::success(&format!("Added server {}", output::highlight(&server_name),));
    record_server_history(host, &server_name, port);

    Ok(Some(server_name))
}

const DETECT_LIBC_COMMAND: &str = "if command -v ldd >/dev/null 2>&1 && ldd --version 2>&1 | grep -qi musl; then echo musl; \
elif command -v ldd >/dev/null 2>&1 && ldd --version 2>&1 | grep -Eqi 'glibc|gnu libc|gnu c library'; then echo glibc; \
elif command -v getconf >/dev/null 2>&1 && getconf GNU_LIBC_VERSION >/dev/null 2>&1; then echo glibc; \
elif ls /lib/ld-musl-*.so.1 /usr/lib/ld-musl-*.so.1 >/dev/null 2>&1; then echo musl; \
else echo unknown; fi";

async fn detect_server_target(
    ssh: &crate::ssh::SshClient,
) -> Result<crate::config::ServerTarget, String> {
    let arch_out = ssh
        .exec("uname -m 2>/dev/null || true")
        .await
        .map_err(|e| format!("Failed to query architecture: {}", e))?;
    let arch = parse_detected_arch(&arch_out.stdout)?;

    let libc_out = ssh
        .exec(DETECT_LIBC_COMMAND)
        .await
        .map_err(|e| format!("Failed to query libc: {}", e))?;
    let libc = parse_detected_libc(&libc_out.stdout)?;

    crate::config::ServerTarget::normalized(&arch, &libc)
        .map_err(|e| format!("Unsupported target metadata: {}", e))
}

fn parse_detected_arch(stdout: &str) -> Result<String, String> {
    let raw = stdout.lines().map(str::trim).find(|line| !line.is_empty());
    let Some(raw_arch) = raw else {
        return Err("Could not detect server architecture from `uname -m` output".to_string());
    };

    crate::config::ServerTarget::normalize_arch(raw_arch).ok_or_else(|| {
        format!(
            "Unsupported server architecture '{}'. Supported architectures: x86_64, aarch64.",
            raw_arch
        )
    })
}

fn parse_detected_libc(stdout: &str) -> Result<String, String> {
    let raw = stdout.lines().map(str::trim).find(|line| !line.is_empty());
    let Some(raw_libc) = raw else {
        return Err("Could not detect server libc".to_string());
    };

    crate::config::ServerTarget::normalize_libc(raw_libc).ok_or_else(|| {
        format!(
            "Unsupported server libc '{}'. Supported libc families: glibc, musl.",
            raw_libc
        )
    })
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

    if let Some(name) = name {
        if !servers.contains(name) {
            return Err(format!("Server '{}' not found.", name).into());
        }

        let confirm = output::confirm(
            &format!("Remove server {}?", output::highlight(name)),
            false,
        )?;

        if !confirm {
            output::warning("Cancelled");
            return Ok(());
        }

        servers.remove(name)?;
        servers.save()?;

        output::success(&format!("Removed {}", output::highlight(name)));
        return Ok(());
    }

    if !output::is_interactive() {
        return Err(
            "No server name provided and selection requires an interactive terminal. Run 'tako servers rm <name>'."
                .into(),
        );
    }

    let mut step = 0;
    let mut selected_name = String::new();

    loop {
        match step {
            // Step 0: Select server
            0 => {
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

                match output::select("Select server to remove", None, options) {
                    Ok(name) => {
                        selected_name = name;
                        step = 1;
                    }
                    Err(e) if output::is_wizard_back(&e) => return Ok(()),
                    Err(e) => return Err(e.into()),
                }
            }
            // Step 1: Confirm
            1 => {
                match output::confirm(
                    &format!("Remove server {}?", output::highlight(&selected_name)),
                    false,
                ) {
                    Ok(true) => {
                        servers.remove(&selected_name)?;
                        servers.save()?;
                        output::success(&format!("Removed {}", output::highlight(&selected_name)));
                        return Ok(());
                    }
                    Ok(false) => {
                        output::warning("Cancelled");
                        return Ok(());
                    }
                    Err(e) if output::is_wizard_back(&e) => {
                        step = 0;
                    }
                    Err(e) => return Err(e.into()),
                }
            }
            _ => unreachable!(),
        }
    }
}

async fn list_servers() -> Result<(), Box<dyn std::error::Error>> {
    use crate::config::ServersToml;

    let servers = ServersToml::load()?;

    if servers.is_empty() {
        output::warning("No servers configured");
        output::muted(&format!(
            "Run {} to add a server.",
            output::highlight("tako servers add")
        ));
        return Ok(());
    }

    let mut names = servers.names();
    names.sort_unstable();

    for name in &names {
        let entry = match servers.get(name) {
            Some(e) => e,
            None => continue,
        };

        let header = if entry.port != 22 {
            format!(
                "{} ({}:{})",
                output::highlight(name),
                entry.host,
                entry.port
            )
        } else {
            format!("{} ({})", output::highlight(name), entry.host)
        };
        println!("{}", header);

        if let Some(desc) = entry
            .description
            .as_deref()
            .filter(|d| !d.trim().is_empty())
        {
            println!(
                "{} {}  {}",
                output::brand_muted("└"),
                output::brand_muted("Description"),
                desc,
            );
        }
    }
    Ok(())
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
    output::with_spinner_async(
        &format!("Connecting to {}", output::highlight(name)),
        "Connected",
        ssh.connect(),
    )
    .await?;

    match output::with_spinner_async(
        "Restarting tako-server",
        "tako-server restarted",
        ssh.tako_restart(),
    )
    .await
    {
        Ok(()) => {
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

fn build_upgrade_owner(server_name: &str) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let raw = format!("upgrade-{server_name}-{now}-{}", std::process::id());
    raw.chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
        .collect()
}

fn first_non_empty_line(value: &str) -> Option<&str> {
    value.lines().map(str::trim).find(|line| !line.is_empty())
}

fn run_with_root_or_sudo(shell_script: &str) -> String {
    format!(
        "if [ \"$(id -u)\" -eq 0 ]; then sh -c '{0}'; elif command -v sudo >/dev/null 2>&1; then sudo sh -c '{0}'; else echo \"error: this operation requires root privileges (run as root or install/configure sudo)\" >&2; exit 1; fi",
        shell_script
    )
}

fn remote_installer_command(channel: UpgradeChannel) -> String {
    let channel_arg = if channel == UpgradeChannel::Canary {
        "canary"
    } else {
        "stable"
    };

    run_with_root_or_sudo(&format!(
        "{} {}",
        SERVER_INSTALL_REFRESH_HELPER, channel_arg
    ))
}

fn format_installer_failure(output: &crate::ssh::CommandOutput) -> String {
    let combined = output.combined();
    let message = first_non_empty_line(combined.trim()).unwrap_or("remote installer failed");
    let lower = message.to_ascii_lowercase();
    if lower.contains("tako-server-install-refresh") && lower.contains("not found") {
        return "Remote host is missing the tako-server upgrade helper. Re-run https://tako.sh/install-server as root, then retry upgrade.".to_string();
    }
    if lower.contains("password")
        || lower.contains("not allowed")
        || lower.contains("sorry")
        || lower.contains("requires root privileges")
        || lower.contains("sudo:")
    {
        return "Remote upgrade requires root privileges. Connect as root or use an SSH user with sudo access on the server.".to_string();
    }
    format!(
        "Remote installer failed with exit code {}: {}",
        output.exit_code, message
    )
}

async fn wait_for_primary_ready(
    ssh: &mut crate::ssh::SshClient,
    timeout: Duration,
    old_pid: u32,
) -> Result<ServerRuntimeInfo, String> {
    let start = std::time::Instant::now();
    let mut last_err = String::new();
    let mut last_seen_pid: Option<u32> = None;
    while start.elapsed() < timeout {
        ssh.clear_tako_hello_cache();
        match ssh.tako_server_info().await {
            Ok(info) if info.pid != old_pid => return Ok(info),
            Ok(info) => {
                last_seen_pid = Some(info.pid);
                tokio::time::sleep(UPGRADE_POLL_INTERVAL).await;
            }
            Err(e) => {
                last_err = e.to_string();
                tokio::time::sleep(UPGRADE_POLL_INTERVAL).await;
            }
        }
    }

    // Gather diagnostics for a more actionable error message
    let service_status = match ssh.tako_status().await {
        Ok(s) => s,
        Err(_) => "unknown".to_string(),
    };

    let detail = if !last_err.is_empty() {
        format!("last socket error: {last_err}")
    } else if let Some(pid) = last_seen_pid {
        format!("socket still reports old pid {pid}")
    } else {
        "no response received".to_string()
    };

    Err(format!(
        "timed out after {:.0}s waiting for new server process (old pid {old_pid}): {detail}; service status: {service_status}",
        timeout.as_secs_f64(),
    ))
}

async fn upgrade_servers(
    name: Option<&str>,
    canary: bool,
    stable: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    use crate::config::ServersToml;

    let Some(name) = name else {
        let servers = ServersToml::load()?;
        if servers.is_empty() {
            return Err("No servers configured. Run 'tako servers add <host>' first.".into());
        }
        let mut names: Vec<String> = servers.names().iter().map(|s| s.to_string()).collect();
        names.sort_unstable();
        for server_name in &names {
            upgrade_server(server_name, canary, stable).await?;
        }
        return Ok(());
    };

    upgrade_server(name, canary, stable).await
}

pub(crate) async fn upgrade_server(
    name: &str,
    canary: bool,
    stable: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    use crate::config::ServersToml;
    use crate::ssh::{SshClient, SshConfig};

    let channel = resolve_upgrade_channel(canary, stable)?;
    output::step(&format!(
        "You're on {} channel",
        output::highlight(channel.as_str())
    ));

    let servers = ServersToml::load()?;
    let server = servers
        .get(name)
        .ok_or_else(|| format!("Server '{}' not found.", name))?;

    let ssh_config = SshConfig::from_server(&server.host, server.port);
    let mut ssh = SshClient::new(ssh_config);
    output::with_spinner_async(
        &format!("Connecting to {}", output::highlight(name)),
        "Connected",
        ssh.connect(),
    )
    .await
    .map_err(|e| format!("SSH connection failed: {}", e))?;

    let owner = build_upgrade_owner(name);
    let mut upgrade_mode_entered = false;

    let upgrade_result: Result<(), String> = async {
        let status = ssh
            .tako_status()
            .await
            .map_err(|e| format!("Failed to query tako-server status: {}", e))?;
        if status != "active" {
            return Err(format!(
                "tako-server must be active before upgrade (status: {}).",
                status
            ));
        }

        // Install/update binary without restarting the service
        let install_message = if channel == UpgradeChannel::Canary {
            "Installing updated canary tako-server binary"
        } else {
            "Installing updated tako-server binary"
        };
        let install_output = output::with_spinner_async_simple(
            install_message,
            ssh.exec(&remote_installer_command(channel)),
        )
        .await
        .map_err(|e| format!("Failed to run installer: {}", e))?;
        if !install_output.success() {
            return Err(format_installer_failure(&install_output));
        }
        output::success("Binary updated");

        output::with_spinner_async(
            "Entering upgrading mode",
            "Upgrading mode enabled",
            ssh.tako_enter_upgrading(&owner),
        )
        .await
        .map_err(|e| format!("Failed to enter upgrading mode: {}", e))?;
        upgrade_mode_entered = true;

        // Capture the current (old) PID before reload so we can detect when
        // the new process has taken over the management socket.
        let old_info = output::with_spinner_async(
            "Reading active runtime config",
            "Runtime config loaded",
            ssh.tako_server_info(),
        )
        .await
        .map_err(|e| format!("Failed to read runtime config: {}", e))?;
        let old_pid = old_info.pid;

        // Trigger zero-downtime reload: SIGHUP → new binary spawns → SIGUSR1 → old drains
        output::with_spinner_async(
            "Reloading tako-server (zero-downtime)",
            "Reload triggered",
            ssh.tako_reload(),
        )
        .await
        .map_err(|e| format!("Reload failed: {}", e))?;

        // Poll until a *new* process (different PID) responds on the socket
        let new_info = output::with_spinner_async(
            "Waiting for new server to be ready",
            "New server is ready",
            wait_for_primary_ready(&mut ssh, UPGRADE_SOCKET_WAIT_TIMEOUT, old_pid),
        )
        .await
        .map_err(|e| format!("Readiness check failed: {}", e))?;
        output::muted(&format!(
            "pid: {} → {}, socket: {}",
            old_pid, new_info.pid, new_info.socket
        ));

        output::with_spinner_async(
            "Exiting upgrading mode",
            "Upgrade complete",
            ssh.tako_exit_upgrading(&owner),
        )
        .await
        .map_err(|e| format!("Failed to exit upgrading mode: {}", e))?;
        upgrade_mode_entered = false;

        Ok(())
    }
    .await;

    if upgrade_result.is_err() && upgrade_mode_entered {
        // Best-effort: wait for *any* server to respond, then exit upgrading mode.
        // Use pid 0 so any responding process satisfies the check.
        let _ = wait_for_primary_ready(&mut ssh, Duration::from_secs(30), 0).await;
        let _ = ssh.tako_exit_upgrading(&owner).await;
    }

    let _ = ssh.disconnect().await;
    upgrade_result.map_err(|e| e.into())
}

const DNS_PROVIDER_CONF: &str = "/opt/tako/dns-provider.conf";
const DNS_CREDENTIALS_ENV: &str = "/opt/tako/dns-credentials.env";
const LEGO_VERSION: &str = "4.21.0";

/// Well-known DNS providers and their required environment variables.
fn dns_provider_env_vars(provider: &str) -> &'static [(&'static str, &'static str)] {
    match provider {
        "cloudflare" => &[(
            "CF_DNS_API_TOKEN",
            "Cloudflare API token (DNS edit permission)",
        )],
        "route53" => &[
            ("AWS_ACCESS_KEY_ID", "AWS access key ID"),
            ("AWS_SECRET_ACCESS_KEY", "AWS secret access key"),
            ("AWS_REGION", "AWS region (e.g. us-east-1)"),
        ],
        "digitalocean" => &[("DO_AUTH_TOKEN", "DigitalOcean API token")],
        "hetzner" => &[("HETZNER_API_KEY", "Hetzner DNS API token")],
        "vultr" => &[("VULTR_API_KEY", "Vultr API key")],
        "linode" => &[("LINODE_TOKEN", "Linode API token")],
        "namecheap" => &[
            ("NAMECHEAP_API_USER", "Namecheap API user"),
            ("NAMECHEAP_API_KEY", "Namecheap API key"),
        ],
        "gcloud" => &[
            ("GCE_PROJECT", "Google Cloud project ID"),
            (
                "GCE_SERVICE_ACCOUNT_FILE",
                "Path to service account JSON key file",
            ),
        ],
        _ => &[],
    }
}

fn lego_download_url(arch: &str) -> String {
    let go_arch = match arch {
        "aarch64" | "arm64" => "arm64",
        _ => "amd64",
    };
    format!(
        "https://github.com/go-acme/lego/releases/download/v{version}/lego_v{version}_linux_{arch}.tar.gz",
        version = LEGO_VERSION,
        arch = go_arch,
    )
}

fn install_lego_command(arch: &str) -> String {
    let url = lego_download_url(arch);
    format!(
        "set -e; \
         if command -v lego >/dev/null 2>&1 && lego --version 2>&1 | grep -q '{version}'; then \
           echo 'lego {version} already installed'; \
         else \
           tmp=$(mktemp -d); \
           cd \"$tmp\"; \
           curl -fsSL '{url}' -o lego.tar.gz; \
           tar -xzf lego.tar.gz lego; \
           install -m 0755 lego /usr/local/bin/lego; \
           rm -rf \"$tmp\"; \
           echo 'lego {version} installed'; \
         fi",
        version = LEGO_VERSION,
        url = url,
    )
}

async fn dns_setup(name: &str) -> Result<(), Box<dyn std::error::Error>> {
    use crate::config::ServersToml;
    use crate::ssh::{SshClient, SshConfig};

    let servers = ServersToml::load()?;
    let server = servers
        .get(name)
        .ok_or_else(|| format!("Server '{}' not found.", name))?;

    output::step(&format!(
        "Setting up DNS provider for wildcard certificates on {}",
        output::highlight(name),
    ));

    let ssh_config = SshConfig::from_server(&server.host, server.port);
    let mut ssh = SshClient::new(ssh_config);
    output::with_spinner_async(
        &format!("Connecting to {}", output::highlight(name)),
        "Connected",
        ssh.connect(),
    )
    .await?;

    // Check if DNS provider is already configured
    let existing_provider = ssh
        .exec(&format!("cat {} 2>/dev/null || true", DNS_PROVIDER_CONF))
        .await?
        .stdout
        .trim()
        .to_string();

    if !existing_provider.is_empty() {
        output::step(&format!(
            "Current DNS provider: {}",
            output::highlight(&existing_provider),
        ));

        let keep = output::confirm("Keep existing DNS provider?", true)?;
        if keep {
            output::success("DNS provider configuration unchanged");
            let _ = ssh.disconnect().await;
            return Ok(());
        }
    }

    // Select DNS provider
    let provider_options = vec![
        ("cloudflare".to_string(), "cloudflare"),
        ("route53 (AWS)".to_string(), "route53"),
        ("digitalocean".to_string(), "digitalocean"),
        ("hetzner".to_string(), "hetzner"),
        ("vultr".to_string(), "vultr"),
        ("linode".to_string(), "linode"),
        ("namecheap".to_string(), "namecheap"),
        ("gcloud (Google Cloud DNS)".to_string(), "gcloud"),
        ("other (enter manually)".to_string(), "other"),
    ];

    let provider = output::select(
        "DNS provider",
        Some("Choose your DNS provider for Let's Encrypt DNS-01 challenges"),
        provider_options,
    )?;

    let provider = if provider == "other" {
        output::TextField::new("DNS provider name")
            .with_hint("lego provider code")
            .prompt()?
    } else {
        provider.to_string()
    };

    // Collect credentials
    let known_vars = dns_provider_env_vars(&provider);
    let mut credentials: Vec<(String, String)> = Vec::new();

    if known_vars.is_empty() {
        output::muted(&format!(
            "Provider '{}' — enter environment variables required by lego.",
            provider,
        ));
        output::muted("See https://go-acme.github.io/lego/dns/ for provider docs.");
        output::muted("Enter variables as KEY=VALUE, one per line. Empty line to finish.");

        loop {
            let line = output::TextField::new("ENV_VAR=value")
                .optional()
                .prompt()?;
            let line = line.trim().to_string();
            if line.is_empty() {
                break;
            }
            if let Some((key, value)) = line.split_once('=') {
                credentials.push((key.trim().to_string(), value.trim().to_string()));
            } else {
                output::warning(&format!("Invalid format '{}' — expected KEY=VALUE", line));
            }
        }
    } else {
        for (var_name, description) in known_vars {
            let value = output::text_field(&format!("{} ({})", var_name, description), None)?;
            credentials.push((var_name.to_string(), value));
        }
    }

    if credentials.is_empty() {
        return Err("No credentials provided. DNS provider setup cancelled.".into());
    }

    // Detect server architecture for lego download
    let arch_output = ssh.exec("uname -m").await?;
    let arch = arch_output.stdout.trim();
    let arch = match arch {
        "x86_64" | "amd64" => "x86_64",
        "aarch64" | "arm64" => "aarch64",
        other => return Err(format!("Unsupported server architecture: {}", other).into()),
    };

    // Install lego binary
    let install_cmd = install_lego_command(arch);
    let install_cmd = SshClient::run_with_root_or_sudo(&install_cmd);
    output::with_spinner_async(
        "Installing lego",
        "lego installed",
        ssh.exec_checked(&install_cmd),
    )
    .await?;

    // Write credentials env file
    let mut env_content = String::new();
    for (key, value) in &credentials {
        env_content.push_str(&format!("{}={}\n", key, value));
    }

    // Write via sudo to /opt/tako/dns-credentials.env
    let escaped_content = env_content.replace('\'', "'\\''");
    let write_creds_cmd = SshClient::run_with_root_or_sudo(&format!(
        "printf '%s' '{}' > {} && chmod 0600 {} && chown tako:tako {}",
        escaped_content, DNS_CREDENTIALS_ENV, DNS_CREDENTIALS_ENV, DNS_CREDENTIALS_ENV,
    ));
    output::with_spinner_async(
        "Writing DNS credentials",
        "Credentials saved",
        ssh.exec_checked(&write_creds_cmd),
    )
    .await?;

    // Write provider conf
    let write_provider_cmd = SshClient::run_with_root_or_sudo(&format!(
        "printf '%s' '{}' > {} && chmod 0644 {} && chown tako:tako {}",
        provider, DNS_PROVIDER_CONF, DNS_PROVIDER_CONF, DNS_PROVIDER_CONF,
    ));
    ssh.exec_checked(&write_provider_cmd).await?;

    // Read existing ExecStart to reconstruct with --dns-provider
    let existing_exec = ssh
        .exec("systemctl show tako-server -p ExecStart --value 2>/dev/null | head -1 || true")
        .await?
        .stdout
        .trim()
        .to_string();

    // Build ExecStart override: strip any old --dns-provider, append new one
    let base_exec = if existing_exec.contains("--dns-provider") {
        // Remove old --dns-provider and its argument
        let re_parts: Vec<&str> = existing_exec.split("--dns-provider").collect();
        if re_parts.len() >= 2 {
            let before = re_parts[0].trim_end();
            // Skip the provider argument after --dns-provider
            let after = re_parts[1].trim_start();
            let after = after
                .split_once(|c: char| c.is_whitespace())
                .map(|(_, rest)| rest.trim_start())
                .unwrap_or("");
            if after.is_empty() {
                before.to_string()
            } else {
                format!("{} {}", before, after)
            }
        } else {
            existing_exec.clone()
        }
    } else if !existing_exec.is_empty() {
        existing_exec.clone()
    } else {
        // Fallback: construct default ExecStart
        "/usr/local/bin/tako-server --socket /var/run/tako/tako.sock --data-dir /opt/tako"
            .to_string()
    };

    let exec_start = format!("{} --dns-provider {}", base_exec, provider);
    let dropin_full = format!(
        "[Service]\nEnvironmentFile={}\nExecStart=\nExecStart={}\n",
        DNS_CREDENTIALS_ENV, exec_start,
    );
    let escaped_dropin_full = dropin_full.replace('\'', "'\\''");

    let write_dropin_cmd = SshClient::run_with_root_or_sudo(&format!(
        "mkdir -p /etc/systemd/system/tako-server.service.d && \
         printf '%s' '{}' > /etc/systemd/system/tako-server.service.d/dns.conf && \
         systemctl daemon-reload",
        escaped_dropin_full,
    ));
    output::with_spinner_async(
        "Configuring systemd service",
        "Service configured",
        ssh.exec_checked(&write_dropin_cmd),
    )
    .await?;

    // Reload service gracefully
    output::with_spinner_async(
        "Reloading tako-server",
        "tako-server reloaded",
        ssh.tako_reload(),
    )
    .await?;

    // Verify the new config took effect
    tokio::time::sleep(Duration::from_secs(2)).await;
    match ssh.tako_server_info().await {
        Ok(info) => {
            if info.dns_provider.as_deref() == Some(&provider) {
                output::success(&format!(
                    "DNS provider '{}' configured on {}",
                    output::highlight(&provider),
                    output::highlight(name),
                ));
            } else {
                output::warning(&format!(
                    "Service reloaded but dns_provider is {:?} (expected '{}').\n\
                     A full restart may be needed: tako servers restart {}",
                    info.dns_provider, provider, name,
                ));
            }
        }
        Err(e) => {
            output::warning(&format!(
                "Could not verify server config after reload: {}.\n\
                 Check: tako servers restart {}",
                e, name,
            ));
        }
    }

    let _ = ssh.disconnect().await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_upgrade_owner_is_shell_safe() {
        let owner = build_upgrade_owner("prod-1");
        assert!(owner.contains("upgrade-prod-1-"));
        assert!(owner.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'));
    }

    #[test]
    fn parse_detected_arch_normalizes_supported_aliases() {
        assert_eq!(parse_detected_arch("x86_64\n").unwrap(), "x86_64");
        assert_eq!(parse_detected_arch("amd64\n").unwrap(), "x86_64");
        assert_eq!(parse_detected_arch("arm64\n").unwrap(), "aarch64");
    }

    #[test]
    fn parse_detected_arch_rejects_unknown_values() {
        let err = parse_detected_arch("sparc\n").unwrap_err();
        assert!(err.contains("Unsupported server architecture"));
    }

    #[test]
    fn parse_detected_libc_normalizes_supported_aliases() {
        assert_eq!(parse_detected_libc("glibc\n").unwrap(), "glibc");
        assert_eq!(parse_detected_libc("GNU libc\n").unwrap(), "glibc");
        assert_eq!(parse_detected_libc("musl\n").unwrap(), "musl");
    }

    #[test]
    fn parse_detected_libc_rejects_unknown_values() {
        let err = parse_detected_libc("uclibc\n").unwrap_err();
        assert!(err.contains("Unsupported server libc"));
    }

    #[test]
    fn remote_installer_command_downloads_script_before_execution() {
        let command = remote_installer_command(UpgradeChannel::Stable);
        assert!(command.contains("if [ \"$(id -u)\" -eq 0 ]"));
        assert!(command.contains("command -v sudo"));
        assert!(command.contains("/usr/local/bin/tako-server-install-refresh stable"));
    }

    #[test]
    fn remote_installer_command_uses_canary_channel_arg_when_requested() {
        let command = remote_installer_command(UpgradeChannel::Canary);
        assert!(command.contains("/usr/local/bin/tako-server-install-refresh canary"));
    }

    #[test]
    fn dns_provider_env_vars_returns_cloudflare_vars() {
        let vars = dns_provider_env_vars("cloudflare");
        assert_eq!(vars.len(), 1);
        assert_eq!(vars[0].0, "CF_DNS_API_TOKEN");
    }

    #[test]
    fn dns_provider_env_vars_returns_empty_for_unknown() {
        let vars = dns_provider_env_vars("some-obscure-provider");
        assert!(vars.is_empty());
    }

    #[test]
    fn lego_download_url_uses_correct_arch() {
        let url = lego_download_url("x86_64");
        assert!(url.contains("amd64"));
        assert!(url.contains(LEGO_VERSION));

        let url = lego_download_url("aarch64");
        assert!(url.contains("arm64"));
    }

    #[test]
    fn install_lego_command_checks_existing_version() {
        let cmd = install_lego_command("x86_64");
        assert!(cmd.contains("lego --version"));
        assert!(cmd.contains(LEGO_VERSION));
        assert!(cmd.contains("/usr/local/bin/lego"));
    }
}

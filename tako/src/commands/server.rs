use crate::output;
use crate::ssh::SshClient;
use clap::Subcommand;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tako_core::ServerRuntimeInfo;
use tracing::Instrument;

use crate::config::{UpgradeChannel, resolve_upgrade_channel};

const UPGRADE_SOCKET_WAIT_TIMEOUT: Duration = Duration::from_secs(120);
const UPGRADE_POLL_INTERVAL: Duration = Duration::from_millis(500);

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

    /// Remove tako-server and all data from a server
    #[command(visible_alias = "uninstall")]
    Implode {
        /// Server name (omit to choose interactively)
        name: Option<String>,

        /// Skip confirmation prompts
        #[arg(short = 'y', long = "yes")]
        yes: bool,
    },

    /// Show global deployment status across configured servers
    #[command(visible_alias = "info")]
    Status,
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
                let _ = add_server(
                    &host,
                    Some(name),
                    description.as_deref(),
                    port,
                    no_test,
                    None,
                )
                .await?;
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
        ServerCommands::Implode { name, yes } => implode_server_cmd(name.as_deref(), yes).await,
        ServerCommands::Status => crate::commands::status::run().await,
    }
}

async fn implode_server_cmd(
    name: Option<&str>,
    assume_yes: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    use crate::config::ServersToml;

    let servers = ServersToml::load()?;

    if servers.is_empty() {
        output::error("No servers configured.");
        return Ok(());
    }

    let server_name = match name {
        Some(n) => {
            if !servers.contains(n) {
                return Err(format!("Server '{}' not found.", n).into());
            }
            n.to_string()
        }
        None => {
            if !output::is_interactive() {
                return Err(
                    "No server name provided and selection requires an interactive terminal. Run 'tako servers implode <name>'."
                        .into(),
                );
            }
            let mut names = servers.names();
            names.sort_unstable();
            let options: Vec<(String, String)> = names
                .into_iter()
                .map(|n| (n.to_string(), n.to_string()))
                .collect();
            output::select("Select server to remove", None, options)?
        }
    };

    let server = servers
        .get(&server_name)
        .ok_or_else(|| format!("Server '{}' not found.", server_name))?
        .clone();

    crate::commands::implode::implode_server(&server_name, &server, assume_yes).await
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

struct WizardConnectionResult {
    target: crate::config::ServerTarget,
    version: Option<String>,
    installed: bool,
    server_name: Option<String>,
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

    // --- Wizard 1: Connection details ---
    let mut conn_wizard =
        output::Wizard::new().with_fields(&[("Server IP or hostname", false), ("SSH port", false)]);
    let mut step = 0usize;
    let mut host = String::new();
    let mut port: u16 = initial_port;

    loop {
        match step {
            0 => {
                let mut builder =
                    output::TextField::new("Server IP or hostname").suggestions(&host_suggestions);
                if !host.is_empty() {
                    builder = builder.with_default(&host);
                } else if let Some(ref ph) = host_placeholder {
                    builder = builder.with_placeholder(ph);
                }
                match conn_wizard.text_field(builder) {
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
            1 => {
                let port_str = port.to_string();
                let mut port_prompt_suggestions =
                    suggestion_history.server_port_suggestions_for(&host, "");
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
                append_unique_suggestions(&mut port_prompt_suggestions, &port_suggestions);
                match conn_wizard.text_field(
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
                            conn_wizard.undo_last();
                        }
                    },
                    Err(e) if output::is_wizard_back(&e) => step = 0,
                    Err(e) => return Err(e.into()),
                }
            }
            _ => break,
        }
    }

    // --- SSH connection test ---
    let mut remote_server_name: Option<String> = None;
    let mut detected_target: Option<crate::config::ServerTarget> = None;

    if default_test_ssh {
        use crate::ssh::{SshClient, SshConfig};

        let ssh_config = SshConfig::from_server(&host, port);
        let mut ssh = SshClient::new(ssh_config);

        let host_span = output::scope(&host);
        let _t = output::timed("SSH connected");
        let result: Result<WizardConnectionResult, String> = output::with_spinner_async_err(
            "Connecting",
            "Connection successful",
            "Connection failed",
            async {
                tracing::debug!("Testing SSH connection to {host}:{port}…");
                ssh.connect().await.map_err(|e| e.to_string())?;

                let target = detect_server_target(&ssh)
                    .await
                    .map_err(|e| format!("Target detection failed: {e}"))?;
                tracing::debug!("Detected target: {}", target.label());

                let (installed, version, server_name_from_info) =
                    match ssh.is_tako_installed().await {
                        Ok(true) => {
                            let ver = ssh.tako_version().await.ok().flatten();
                            let sn = ssh
                                .tako_server_info()
                                .await
                                .ok()
                                .and_then(|info| info.server_name);
                            (true, ver, sn)
                        }
                        Ok(false) => (false, None, None),
                        Err(_) => (false, None, None),
                    };

                ssh.disconnect().await.map_err(|e| e.to_string())?;

                Ok(WizardConnectionResult {
                    target,
                    version,
                    installed,
                    server_name: server_name_from_info,
                })
            }
            .instrument(host_span),
        )
        .await;
        drop(_t);

        let _host_scope = output::scope(&host).entered();
        match result {
            Ok(info) => {
                tracing::debug!("Target: {}", info.target.label());
                if let Some(ref ver) = info.version {
                    let ver = ver.strip_prefix("tako-server ").unwrap_or(ver);
                    tracing::debug!("Server version: {ver}");
                }
                if !info.installed {
                    output::warning("tako-server not installed");
                    output::muted(
                        "Install it on the server as root (see scripts/install-tako-server.sh), then re-run deploy.",
                    );
                }
                remote_server_name = info.server_name;
                detected_target = Some(info.target);
            }
            Err(e) => {
                return Err(e.into());
            }
        }
    }

    if output::is_pretty() {
        eprintln!();
    }

    // --- Wizard 2: Server identity ---
    let mut id_wizard =
        output::Wizard::new().with_fields(&[("Server name", false), ("Description", false)]);
    let mut step = 0usize;
    let mut name = String::new();
    let mut description = String::new();

    loop {
        match step {
            0 => {
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
                } else if let Some(ref rsn) = remote_server_name {
                    Some(rsn.as_str())
                } else if let Some(n) = name_prompt_suggestions.first() {
                    Some(n.as_str())
                } else if !host.chars().next().is_some_and(|c| c.is_ascii_digit())
                    && !host.contains(':')
                {
                    Some(host.as_str())
                } else {
                    None
                };
                match id_wizard.text_field(
                    output::TextField::new("Server name")
                        .default_opt(default_name)
                        .suggestions(&name_prompt_suggestions),
                ) {
                    Ok(v) => {
                        name = v.trim().to_string();
                        step = 1;
                    }
                    Err(e) if output::is_wizard_back(&e) => return Ok(None),
                    Err(e) => return Err(e.into()),
                }
            }
            1 => {
                let default_desc = if !description.is_empty() {
                    Some(description.as_str())
                } else {
                    initial_description
                };
                match id_wizard.text_field(
                    output::TextField::new("Description")
                        .optional()
                        .default_opt(default_desc),
                ) {
                    Ok(v) => {
                        description = v.trim().to_string();
                        break;
                    }
                    Err(e) if output::is_wizard_back(&e) => step = 0,
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

    // SSH was already tested above; skip re-testing in add_server
    add_server(
        &host,
        name_ref,
        description_ref,
        port,
        true,
        detected_target,
    )
    .await
}

pub async fn add_server(
    host: &str,
    name: Option<&str>,
    description: Option<&str>,
    port: u16,
    no_test: bool,
    pre_detected_target: Option<crate::config::ServerTarget>,
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
                    output::strong(&server_name),
                    host,
                    port
                ));
                record_server_history(host, &server_name, port);
                return Ok(Some(server_name));
            }

            output::success(&format!(
                "Server {} is already configured (tako@{}:{})",
                output::strong(&server_name),
                host,
                port
            ));
            record_server_history(host, &server_name, port);
            return Ok(Some(server_name));
        }

        let confirm = output::confirm(
            &format!(
                "Host {} already exists as {}. Override?",
                output::strong(host),
                output::strong(&existing_name)
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

    if output::is_dry_run() {
        output::dry_run_skip(&format!(
            "Add server {} (tako@{}:{})",
            output::strong(&server_name),
            host,
            port
        ));
        return Ok(Some(server_name));
    }

    let mut detected_target: Option<ServerTarget> = pre_detected_target;
    // Test SSH connection unless skipped or already tested
    if !no_test && detected_target.is_none() {
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

        {
            let _host_scope = output::scope(host).entered();
            match result {
                Ok(info) => {
                    tracing::debug!("Target: {}", info.target.label());
                    if let Some(ref ver) = info.version {
                        let ver = ver.strip_prefix("tako-server ").unwrap_or(ver);
                        tracing::debug!("Server version: {ver}");
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
        }
    } else if detected_target.is_none() {
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

    output::success(&format!("Added server {}", output::strong(&server_name),));
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
    let combined = format!(
        "echo ARCH:$(uname -m 2>/dev/null || echo unknown); echo LIBC:$({})",
        DETECT_LIBC_COMMAND
    );
    let output = ssh
        .exec(&combined)
        .await
        .map_err(|e| format!("Failed to detect server target: {}", e))?;

    let mut arch_str = String::new();
    let mut libc_str = String::new();
    for line in output.stdout.lines() {
        let line = line.trim();
        if let Some(val) = line.strip_prefix("ARCH:") {
            arch_str = val.trim().to_string();
        } else if let Some(val) = line.strip_prefix("LIBC:") {
            libc_str = val.trim().to_string();
        }
    }

    let arch = parse_detected_arch(&arch_str)?;
    let libc = parse_detected_libc(&libc_str)?;

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
    if let Err(e) = history.save() {
        tracing::warn!("Could not save CLI history: {e}");
    }
}

fn removal_option_label(name: &str, entry: &crate::config::ServerEntry) -> String {
    match entry.description.as_deref().map(str::trim) {
        Some(description) if !description.is_empty() => {
            format!("{name} ({description})  {}:{}", entry.host, entry.port)
        }
        _ => format!("{name}  {}:{}", entry.host, entry.port),
    }
}

async fn remove_server(name: Option<&str>) -> Result<(), Box<dyn std::error::Error>> {
    use crate::config::ServersToml;

    let mut servers = ServersToml::load()?;

    if servers.is_empty() {
        output::error("No servers configured.");
        output::hint(&format!(
            "Run {} to add a server.",
            output::strong("tako servers add")
        ));
        return Ok(());
    }

    if let Some(name) = name {
        if !servers.contains(name) {
            return Err(format!("Server '{}' not found.", name).into());
        }

        if output::is_dry_run() {
            output::dry_run_skip(&format!("Remove server {}", output::strong(name)));
            return Ok(());
        }

        let confirm = output::confirm(&format!("Remove {}?", output::strong(name)), false)?;

        if !confirm {
            output::warning("Cancelled");
            return Ok(());
        }

        servers.remove(name)?;
        servers.save()?;

        output::success(&format!("Removed {}", output::strong(name)));
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
                        output::muted(&format!("Server: {name}"));
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
                    &format!("Remove {}?", output::strong(&selected_name)),
                    false,
                ) {
                    Ok(true) => {
                        servers.remove(&selected_name)?;
                        servers.save()?;
                        output::success(&format!("Removed {}", output::strong(&selected_name)));
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
        tracing::warn!("No servers configured");
        output::warning("No servers configured");
        output::hint(&format!(
            "Run {} to add a server.",
            output::strong("tako servers add")
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
            format!("{} ({}:{})", output::strong(name), entry.host, entry.port)
        } else {
            format!("{} ({})", output::strong(name), entry.host)
        };
        let _scope = output::scope(name).entered();
        tracing::info!("Server listed ({}:{})", entry.host, entry.port);
        output::info(&header);

        if let Some(desc) = entry
            .description
            .as_deref()
            .filter(|d| !d.trim().is_empty())
        {
            output::bullet(&format!("{} {desc}", output::brand_muted("Description")));
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

    let _scope = output::scope(name).entered();
    let ssh_config = SshConfig::from_server(&server.host, server.port);
    let mut ssh = SshClient::new(ssh_config);
    let _t = output::timed("SSH connected");
    output::with_spinner_async(&format!("Connecting to {name}"), "Connected", ssh.connect())
        .await?;
    drop(_t);

    tracing::debug!("Sending restart command…");
    let _t = output::timed("Restart tako-server");
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
            drop(_t);
            output::error(&format!("Restart failed: {}", e));
            ssh.disconnect().await?;
            return Err(format!("Failed to restart tako-server: {}", e).into());
        }
    }
    drop(_t);

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

const REPO_OWNER: &str = "lilienblum";
const REPO_NAME: &str = "tako";
const SERVER_TAG_PREFIX: &str = "tako-server-v";
const SERVER_TAGS_API: &str = "https://api.github.com/repos/lilienblum/tako/tags?per_page=100";
const SERVER_CHECKSUM_MANIFEST_ASSET: &str = "tako-server-sha256s.txt";
const SERVER_CHECKSUM_SIGNATURE_ASSET: &str = "tako-server-sha256s.txt.sig";
const ALLOW_INSECURE_DOWNLOAD_BASE_ENV: &str = "TAKO_ALLOW_INSECURE_DOWNLOAD_BASE";
const SERVER_RELEASE_SIGNING_PUBLIC_KEY_PEM: &str = "-----BEGIN PUBLIC KEY-----\n\
MIIBojANBgkqhkiG9w0BAQEFAAOCAY8AMIIBigKCAYEAuSti08sNCTG7S1oGDSB3\n\
vThbzAfQQzGq+wQjVkjN1VEPFk21eWqYMEAN2jU3FhTZDrsfl5iEMv1NsE6bimjd\n\
LN3UtdvqnxdF08wlCmbu4tO7thJE4CNY1uY4qHjI1aqBSozJ92x8vkel1DZKUxG0\n\
aK1YdrP0bqbuikK8f5wFgMGPO0sfSH5FKH7N0SseEoMZt1bGh7bL8G2EEDo91uEb\n\
w0OcbZGhZ/G3Kbv9dBQAS16eEgH/d0ssruPjdsQbFD+hnywgiqC8lOro1cmr1bBN\n\
d+Q7l60r6e3Y4kmH3OCqRzmIcKnv+6Piot9YHqMxptd6BuiE6x72w9j2loOLnB5j\n\
ytknLq3YykchWrbwLYqVspjN6FcqPZgI6bIEhsaFLRD6tjTqYBmEHcpLk//26p7a\n\
1/r22DyKdHO3/GS0L2sYVKkD/7R9N5QfnRd3erbx7je0pzDDe/x31h4X7vGgjCTy\n\
xm4tDiIHBg92bd3+ag9qnvulBH1uEb2i+grxFYefUkKpAgMBAAE=\n\
-----END PUBLIC KEY-----\n";

#[derive(Debug, Clone, PartialEq, Eq)]
struct VerifiedReleaseAsset {
    download_url: String,
    expected_sha256: String,
}

/// Fetch the latest canary server version from the GitHub release body.
/// The release body contains "master (SHA)" — we extract the SHA and construct
/// the version string like "canary-<sha>".
async fn fetch_canary_server_version() -> Result<String, String> {
    let url = format!(
        "https://api.github.com/repos/{REPO_OWNER}/{REPO_NAME}/releases/tags/canary-latest"
    );
    let client = reqwest::Client::new();
    let resp = client
        .get(&url)
        .header("User-Agent", "tako-cli")
        .send()
        .await
        .map_err(|e| format!("Failed to fetch canary release: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("GitHub API returned {}", resp.status()));
    }
    let text = resp
        .text()
        .await
        .map_err(|e| format!("Failed to read response: {e}"))?;
    let raw: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("Failed to parse release: {e}"))?;

    // Body format: "Latest canary build from master (SHA) on DATE."
    let body = raw["body"].as_str().unwrap_or("");
    if let Some(start) = body.find('(')
        && let Some(end) = body[start..].find(')')
    {
        let sha = &body[start + 1..start + end];
        if !sha.is_empty() && sha.len() <= 40 {
            let short = &sha[..sha.len().min(7)];
            return Ok(format!("canary-{short}"));
        }
    }

    Err("Could not parse canary version from release".to_string())
}

fn server_binary_archive_name(target: &crate::config::ServerTarget) -> String {
    format!("tako-server-linux-{}-{}.tar.zst", target.arch, target.libc)
}

fn parse_boolish_env(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

fn allow_insecure_download_base() -> bool {
    std::env::var(ALLOW_INSECURE_DOWNLOAD_BASE_ENV)
        .map(|value| parse_boolish_env(&value))
        .unwrap_or(false)
}

fn validate_download_base(base: &str, allow_insecure: bool) -> Result<(), String> {
    if base.starts_with("https://") {
        return Ok(());
    }
    if allow_insecure {
        output::warning(&format!(
            "Using insecure download base '{}'; this is intended only for local testing.",
            base
        ));
        return Ok(());
    }
    Err(format!(
        "TAKO_DOWNLOAD_BASE_URL must use https://. Set {ALLOW_INSECURE_DOWNLOAD_BASE_ENV}=1 to allow an insecure override for local testing."
    ))
}

fn server_download_base(
    channel: UpgradeChannel,
    tag: Option<&str>,
    custom_base: Option<&str>,
    allow_insecure: bool,
) -> Result<String, String> {
    let base = if let Some(raw) = custom_base {
        let trimmed = raw.trim().trim_end_matches('/');
        if trimmed.is_empty() {
            default_download_base(channel, tag)
        } else {
            validate_download_base(trimmed, allow_insecure)?;
            trimmed.to_string()
        }
    } else if let Ok(env_base) = std::env::var("TAKO_DOWNLOAD_BASE_URL") {
        let trimmed = env_base.trim().trim_end_matches('/');
        if trimmed.is_empty() {
            default_download_base(channel, tag)
        } else {
            validate_download_base(trimmed, allow_insecure)?;
            trimmed.to_string()
        }
    } else {
        default_download_base(channel, tag)
    };
    Ok(base)
}

fn server_binary_download_url(
    channel: UpgradeChannel,
    tag: Option<&str>,
    target: &crate::config::ServerTarget,
    custom_base: Option<&str>,
    allow_insecure: bool,
) -> Result<String, String> {
    let base = server_download_base(channel, tag, custom_base, allow_insecure)?;
    Ok(format!("{}/{}", base, server_binary_archive_name(target)))
}

fn default_download_base(channel: UpgradeChannel, tag: Option<&str>) -> String {
    let release_tag = if channel == UpgradeChannel::Canary {
        "canary-latest".to_string()
    } else {
        tag.unwrap_or("canary-latest").to_string()
    };
    format!("https://github.com/{REPO_OWNER}/{REPO_NAME}/releases/download/{release_tag}")
}

fn parse_sha256_manifest_value(manifest: &str, filename: &str) -> Result<String, String> {
    for line in manifest
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        let mut parts = line.split_whitespace();
        let Some(hash) = parts.next() else {
            continue;
        };
        let Some(name) = parts.next() else {
            continue;
        };
        let normalized_name = name.trim_start_matches('*').trim_start_matches("./");
        if normalized_name == filename {
            if hash.len() == 64 && hash.chars().all(|ch| ch.is_ascii_hexdigit()) {
                return Ok(hash.to_ascii_lowercase());
            }
            return Err(format!(
                "checksum manifest entry for '{filename}' contains an invalid SHA-256 value"
            ));
        }
    }
    Err(format!("checksum manifest missing entry for '{filename}'"))
}

fn verify_signed_server_checksum_manifest(manifest: &[u8], signature: &[u8]) -> Result<(), String> {
    let key =
        openssl::pkey::PKey::public_key_from_pem(SERVER_RELEASE_SIGNING_PUBLIC_KEY_PEM.as_bytes())
            .map_err(|e| format!("failed to load embedded server release public key: {e}"))?;
    let mut verifier =
        openssl::sign::Verifier::new(openssl::hash::MessageDigest::sha256(), &key)
            .map_err(|e| format!("failed to initialize server release signature verifier: {e}"))?;
    verifier
        .update(manifest)
        .map_err(|e| format!("failed to hash server release checksum manifest: {e}"))?;
    let verified = verifier
        .verify(signature)
        .map_err(|e| format!("failed to verify server checksum signature: {e}"))?;
    if verified {
        Ok(())
    } else {
        Err("server checksum signature verification failed".to_string())
    }
}

async fn fetch_release_bytes(url: &str) -> Result<Vec<u8>, String> {
    let client = reqwest::Client::new();
    let response = client
        .get(url)
        .header("User-Agent", "tako-cli")
        .send()
        .await
        .map_err(|e| format!("request failed for {url}: {e}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "download failed for {url}: HTTP {}",
            response.status()
        ));
    }
    response
        .bytes()
        .await
        .map(|bytes| bytes.to_vec())
        .map_err(|e| format!("failed to read response body from {url}: {e}"))
}

async fn fetch_release_text(url: &str) -> Result<String, String> {
    let bytes = fetch_release_bytes(url).await?;
    String::from_utf8(bytes).map_err(|e| format!("response from {url} was not valid UTF-8: {e}"))
}

async fn resolve_verified_server_release_asset(
    channel: UpgradeChannel,
    tag: Option<&str>,
    target: &crate::config::ServerTarget,
) -> Result<VerifiedReleaseAsset, String> {
    let allow_insecure = allow_insecure_download_base();
    let custom_base = std::env::var("TAKO_DOWNLOAD_BASE_URL").ok();
    let custom_base_ref = custom_base.as_deref();
    let base = server_download_base(channel, tag, custom_base_ref, allow_insecure)?;
    let is_custom_source = custom_base_ref
        .map(|b| !b.trim().is_empty())
        .unwrap_or(false);
    let archive_name = server_binary_archive_name(target);
    let download_url =
        server_binary_download_url(channel, tag, target, custom_base_ref, allow_insecure)?;
    let manifest_url = format!("{base}/{SERVER_CHECKSUM_MANIFEST_ASSET}");
    let manifest = fetch_release_bytes(&manifest_url).await?;
    if is_custom_source {
        // Custom download source: skip signature verification since the embedded
        // public key only matches the upstream signing key. Checksum verification
        // on the remote host still protects against corrupt downloads.
        output::warning(
            "Skipping release signature verification because TAKO_DOWNLOAD_BASE_URL is set. \
             Checksums will still be verified after download.",
        );
    } else {
        let signature_url = format!("{base}/{SERVER_CHECKSUM_SIGNATURE_ASSET}");
        let signature = fetch_release_bytes(&signature_url).await?;
        verify_signed_server_checksum_manifest(&manifest, &signature)?;
    }
    let manifest_text = std::str::from_utf8(&manifest)
        .map_err(|e| format!("signed checksum manifest was not valid UTF-8: {e}"))?;
    let expected_sha256 = parse_sha256_manifest_value(manifest_text, &archive_name)?;
    Ok(VerifiedReleaseAsset {
        download_url,
        expected_sha256,
    })
}

fn verify_downloaded_sha256_script(path_expr: &str, expected_sha256: &str) -> String {
    let expected_sha256 = crate::shell::shell_single_quote(expected_sha256);
    format!(
        "expected_sha={expected_sha256}; \
         actual_sha=''; \
         if command -v sha256sum >/dev/null 2>&1; then \
           actual_sha=$(sha256sum {path_expr} | awk '{{print $1}}'); \
         elif command -v shasum >/dev/null 2>&1; then \
           actual_sha=$(shasum -a 256 {path_expr} | awk '{{print $1}}'); \
         elif command -v openssl >/dev/null 2>&1; then \
           actual_sha=$(openssl dgst -sha256 {path_expr} | awk '{{print $NF}}'); \
         else \
           echo 'error: sha256 tool not found' >&2; exit 1; \
         fi; \
         if [ \"$actual_sha\" != \"$expected_sha\" ]; then \
           echo \"error: sha256 mismatch (expected=$expected_sha actual=$actual_sha)\" >&2; exit 1; \
         fi"
    )
}

/// Build a remote command that downloads and replaces the tako-server binary.
fn remote_binary_replace_command(url: &str, expected_sha256: &str) -> String {
    use crate::shell::shell_single_quote;
    let url_q = shell_single_quote(url);
    let sha_check = verify_downloaded_sha256_script("\"$archive\"", expected_sha256);
    // Download tar.zst, extract the binary, install it, set capabilities.
    let script = format!(
        "set -eu; \
         tmp=$(mktemp -d); \
         archive=\"$tmp/tako-server.tar.zst\"; \
         trap 'rm -rf \"$tmp\"' EXIT; \
         curl -fsSL {url_q} -o \"$archive\"; \
         {sha_check}; \
         zstd -d \"$archive\" --stdout | tar -x -C \"$tmp\"; \
         bin=$(find \"$tmp\" -type f -name tako-server | head -n 1); \
         if [ -z \"$bin\" ]; then echo 'error: archive did not contain tako-server binary' >&2; exit 1; fi; \
         install -m 0755 \"$bin\" /usr/local/bin/tako-server; \
         if command -v setcap >/dev/null 2>&1; then setcap cap_net_bind_service=+ep /usr/local/bin/tako-server 2>/dev/null || true; fi"
    );
    SshClient::run_with_root_or_sudo(&script)
}

/// Resolve the latest stable server tag from the GitHub API.
async fn resolve_latest_server_tag() -> Result<String, String> {
    let client = reqwest::Client::new();
    let resp = client
        .get(SERVER_TAGS_API)
        .header("User-Agent", "tako-cli")
        .send()
        .await
        .map_err(|e| format!("request failed: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("GitHub API returned {}", resp.status()));
    }
    let text = resp
        .text()
        .await
        .map_err(|e| format!("failed to read response: {e}"))?;
    let raw: Vec<serde_json::Value> =
        serde_json::from_str(&text).map_err(|e| format!("failed to parse tags: {e}"))?;
    for entry in &raw {
        if let Some(name) = entry.get("name").and_then(|n| n.as_str())
            && name.starts_with(SERVER_TAG_PREFIX)
        {
            return Ok(name.to_string());
        }
    }
    Err(format!(
        "no release found with prefix '{SERVER_TAG_PREFIX}'"
    ))
}

async fn wait_for_primary_ready(
    ssh: &mut crate::ssh::SshClient,
    timeout: Duration,
    old_pid: u32,
    server_name: &str,
) -> Result<ServerRuntimeInfo, String> {
    let start = std::time::Instant::now();
    let mut last_err = String::new();
    let mut last_seen_pid: Option<u32> = None;
    let mut poll_count = 0u32;
    while start.elapsed() < timeout {
        ssh.clear_tako_hello_cache();
        poll_count += 1;
        match ssh.tako_server_info().await {
            Ok(info) if info.pid != old_pid => {
                tracing::debug!(
                    server = server_name,
                    new_pid = info.pid,
                    old_pid,
                    polls = poll_count,
                    elapsed_ms = start.elapsed().as_millis() as u64,
                    "new server process detected"
                );
                return Ok(info);
            }
            Ok(info) => {
                last_seen_pid = Some(info.pid);
                tracing::debug!(
                    server = server_name,
                    pid = info.pid,
                    polls = poll_count,
                    "still seeing old PID, waiting"
                );
                tokio::time::sleep(UPGRADE_POLL_INTERVAL).await;
            }
            Err(e) => {
                last_err = e.to_string();
                tracing::debug!(
                    server = server_name,
                    error = %e,
                    polls = poll_count,
                    "socket probe failed, waiting"
                );
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

    let channel = resolve_upgrade_channel(canary, stable)?;

    let servers = ServersToml::load()?;
    if servers.is_empty() {
        output::error("No servers configured.");
        output::hint(&format!(
            "Run {} to add a server.",
            output::strong("tako servers add")
        ));
        return Ok(());
    }

    let names: Vec<String> = if let Some(name) = name {
        if !servers.contains(name) {
            return Err(format!("Server '{}' not found.", name).into());
        }
        vec![name.to_string()]
    } else {
        let mut names: Vec<String> = servers.names().iter().map(|s| s.to_string()).collect();
        names.sort_unstable();
        names
    };

    output::ContextBlock::new()
        .channel(channel.as_str())
        .print();

    let interactive = output::is_pretty() && output::is_interactive();

    // Resolve latest version. For stable the CLI version is the latest
    // (CLI and server are released together). For canary, fetch from GitHub.
    let latest_version: Option<String> = if channel == UpgradeChannel::Stable {
        let ver = crate::cli::display_version();
        tracing::info!("Latest version: {ver}");
        output::info(&format!("Latest version: {}", output::strong(&ver)));
        Some(ver)
    } else {
        let start = std::time::Instant::now();
        let ver = output::with_spinner_async_simple(
            "Getting latest version…",
            fetch_canary_server_version(),
        )
        .await
        .ok();
        if let Some(ref ver) = ver {
            let time = output::format_elapsed_trace(start.elapsed());
            tracing::info!("Latest version: {ver} {time}");
            output::success(&format!("Latest version: {}", output::strong(ver)));
        }
        ver
    };

    // ── Phase 1: Get current versions from all servers ──────────────
    let total = names.len();
    let done = Arc::new(std::sync::atomic::AtomicUsize::new(0));

    struct VersionCheck {
        name: String,
        ssh: Option<SshClient>,
        version: Option<String>,
        target: Option<crate::config::ServerTarget>,
        error: Option<String>,
        elapsed: Duration,
    }

    let mut version_set = tokio::task::JoinSet::new();
    for server_name in &names {
        let server = servers
            .get(server_name)
            .ok_or_else(|| format!("Server '{}' not found.", server_name))?
            .clone();
        let name = server_name.clone();
        let done = Arc::clone(&done);
        let span = output::scope(&name);
        version_set.spawn(
            async move {
                let start = std::time::Instant::now();
                let ssh = match SshClient::connect_to(&server.host, server.port).await {
                    Ok(ssh) => ssh,
                    Err(e) => {
                        done.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        return VersionCheck {
                            name,
                            ssh: None,
                            version: None,
                            target: None,
                            error: Some(e.to_string()),
                            elapsed: start.elapsed(),
                        };
                    }
                };
                let version = ssh.tako_version().await.ok().flatten();
                let target = detect_server_target(&ssh).await.ok();
                done.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                VersionCheck {
                    name,
                    ssh: Some(ssh),
                    version,
                    target,
                    error: None,
                    elapsed: start.elapsed(),
                }
            }
            .instrument(span),
        );
    }

    let pb = if interactive {
        output::hide_cursor();
        let pb = indicatif::ProgressBar::new_spinner();
        pb.set_style(output::phase_spinner_style());
        let msg = if total == 1 {
            format!("Getting current version for {}…", output::strong(&names[0]))
        } else {
            format!(
                "Getting current versions… {}",
                output::muted_progress(0, total)
            )
        };
        pb.set_message(msg);
        pb.enable_steady_tick(Duration::from_millis(80));
        Some(pb)
    } else {
        if total == 1 {
            tracing::info!("Getting current version for {}…", &names[0]);
        } else {
            tracing::info!("Getting current versions for {} servers…", total);
        }
        None
    };

    let channel_label = channel.as_str();

    let mut checks: Vec<VersionCheck> = Vec::new();
    while let Some(join_result) = version_set.join_next().await {
        let check = match join_result {
            Ok(v) => v,
            Err(e) => {
                if let Some(ref pb) = pb {
                    pb.finish_and_clear();
                }
                if interactive {
                    output::show_cursor();
                }
                return Err(e.to_string().into());
            }
        };
        if let Some(ref pb) = pb {
            let finished = done.load(std::sync::atomic::Ordering::Relaxed);
            if total > 1 {
                pb.set_message(format!(
                    "Getting current versions… {}",
                    output::muted_progress(finished, total)
                ));
            }
        }
        if let Some(ref v) = check.version {
            let _scope = output::scope(&check.name).entered();
            let time = output::format_elapsed_trace(check.elapsed);
            tracing::debug!("Current version: {v} {time}");
        }
        checks.push(check);
    }

    if let Some(ref pb) = pb {
        pb.finish_and_clear();
    }
    if interactive {
        output::show_cursor();
    }

    // Sort to match input order.
    checks.sort_by(|a, b| {
        let pos_a = names
            .iter()
            .position(|n| n == &a.name)
            .unwrap_or(usize::MAX);
        let pos_b = names
            .iter()
            .position(|n| n == &b.name)
            .unwrap_or(usize::MAX);
        pos_a.cmp(&pos_b)
    });

    // ── Phase 2: Per-server upgrade ─────────────────────────────────
    let mut has_error = false;
    for (i, mut check) in checks.into_iter().enumerate() {
        // First heading uses no leading blank (ContextBlock already printed one).
        if i > 0 {
            output::heading(&format!("Server {}", output::strong(&check.name)));
        } else {
            output::heading_no_gap(&format!("Server {}", output::strong(&check.name)));
        }

        let _upgrade_scope = output::scope(&check.name).entered();
        let current_ver = check.version.as_deref().unwrap_or("unknown");

        // Connection error — nothing else to do.
        if let Some(ref err) = check.error {
            output::error(err);
            has_error = true;
            continue;
        }

        // Already on the latest version — skip the download entirely.
        if let Some(ref latest) = latest_version {
            let matches = if channel == UpgradeChannel::Canary {
                // For canary, compare the "canary-<sha>" suffix (base versions differ).
                check
                    .version
                    .as_deref()
                    .and_then(|v| v.find("-canary-").map(|pos| &v[pos + 1..]))
                    == Some(latest.as_str())
            } else {
                check.version.as_deref() == Some(latest.as_str())
            };
            if matches {
                output::success(&format!(
                    "Already on latest {channel_label} build ({current_ver})"
                ));
                if let Some(mut ssh) = check.ssh.take() {
                    let _ = ssh.disconnect().await;
                }
                continue;
            }
        }

        // Run upgrade with a spinner.
        let mut ssh = check.ssh.take().unwrap();
        let spinner = output::PhaseSpinner::start_indented(&format!("Upgrading to {current_ver}…"));

        let target = match check.target {
            Some(t) => t,
            None => {
                has_error = true;
                spinner.finish_err_indented("Could not detect server target");
                let _ = ssh.disconnect().await;
                continue;
            }
        };
        match run_server_upgrade(
            &check.name,
            &mut ssh,
            channel,
            check.version.as_deref(),
            &target,
        )
        .await
        {
            Ok(version_after) => {
                let ver = version_after.as_deref().unwrap_or("unknown");
                if ver == current_ver {
                    spinner.finish_ok_indented(&format!("Already on the latest version ({ver})"));
                } else {
                    spinner.finish_ok_indented(&format!("{current_ver} -> {ver}"));
                }
            }
            Err(e) => {
                has_error = true;
                let clean_err = if let Some(pos) = e.find(" (owner:") {
                    &e[..pos]
                } else {
                    e.as_str()
                };
                spinner.finish_err_indented(clean_err);
            }
        }

        let _ = ssh.disconnect().await;
    }

    if has_error {
        std::process::exit(1);
    }
    Ok(())
}

/// Run the install → reload → verify cycle on an already-connected server.
/// Returns the version string after upgrade, or an error message.
async fn run_server_upgrade(
    name: &str,
    ssh: &mut SshClient,
    channel: UpgradeChannel,
    running_version: Option<&str>,
    target: &crate::config::ServerTarget,
) -> Result<Option<String>, String> {
    let owner = build_upgrade_owner(name);
    let mut upgrade_mode_entered = false;

    let result: Result<Option<String>, String> = async {
        let status = ssh
            .tako_status()
            .await
            .map_err(|e| format!("Failed to query status: {e}"))?;
        if status != "active" {
            return Err(format!("tako-server not active (status: {status})"));
        }

        // Resolve download URL
        let stable_tag = if channel == UpgradeChannel::Stable {
            let tag = resolve_latest_server_tag()
                .await
                .map_err(|e| format!("Failed to resolve latest server tag: {e}"))?;
            tracing::debug!("Resolved tag: {tag}");
            Some(tag)
        } else {
            None
        };
        let verified_release =
            resolve_verified_server_release_asset(channel, stable_tag.as_deref(), target)
                .await
                .map_err(|e| format!("Failed to verify release metadata: {e}"))?;

        // Download and replace binary
        tracing::debug!("Downloading {} binary…", channel.as_str());
        let _t = output::timed("Binary download");
        let install_output = ssh
            .exec(&remote_binary_replace_command(
                &verified_release.download_url,
                &verified_release.expected_sha256,
            ))
            .await
            .map_err(|e| format!("Binary download failed: {e}"))?;
        drop(_t);
        if !install_output.success() {
            tracing::debug!("Binary replace failed: {}", install_output.stderr.trim());
            let combined = install_output.combined();
            let message =
                first_non_empty_line(combined.trim()).unwrap_or("binary download/install failed");
            return Err(message.to_string());
        }

        // Check if the on-disk binary actually changed. `tako-server --version`
        // reads the binary, not the running process, so this detects installer
        // no-ops and skips the expensive reload+wait cycle.
        let version_after_install = ssh.tako_version().await.ok().flatten();
        if version_after_install.as_deref() == running_version {
            tracing::debug!("Binary unchanged, skipping reload");
            return Ok(version_after_install);
        }

        // Enter upgrading mode
        let _t = output::timed("Enter upgrade mode");
        ssh.tako_enter_upgrading(&owner)
            .await
            .map_err(|e| match &e {
                crate::ssh::SshError::CommandFailed(m) => m.clone(),
                other => other.to_string(),
            })?;
        drop(_t);
        upgrade_mode_entered = true;

        // Get old PID, reload, wait for new process
        let old_pid = ssh
            .tako_server_info()
            .await
            .map_err(|e| format!("Failed to read runtime config: {e}"))?
            .pid;

        tracing::debug!("Reloading server (pid: {old_pid})…");
        let _t = output::timed("Reload + wait for new process");
        ssh.tako_reload()
            .await
            .map_err(|e| format!("Reload failed: {e}"))?;

        let info = wait_for_primary_ready(ssh, UPGRADE_SOCKET_WAIT_TIMEOUT, old_pid, name).await?;
        drop(_t);
        tracing::debug!("New server process ready (pid: {})", info.pid);

        // Exit upgrading mode. After a SIGHUP reload the new server process
        // starts fresh in Normal mode and clears the orphaned upgrade lock, so
        // "owner does not hold the upgrade lock" is expected and harmless.
        match ssh.tako_exit_upgrading(&owner).await {
            Ok(()) => {}
            Err(e) => {
                let msg = e.to_string();
                if msg.contains("does not hold the upgrade lock") {
                    tracing::debug!("Upgrade lock already cleared by new server process");
                } else {
                    return Err(format!("Failed to exit upgrading mode: {e}"));
                }
            }
        }
        upgrade_mode_entered = false;

        // Get new version
        let version = ssh.tako_version().await.ok().flatten();
        tracing::debug!("Upgrade complete (version: {version:?})");
        Ok(version)
    }
    .await;

    if result.is_err() && upgrade_mode_entered {
        tracing::debug!("Upgrade failed, attempting to release upgrade lock (owner: {owner})");
        for attempt in 0..5 {
            if attempt > 0 {
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
            match ssh.tako_exit_upgrading(&owner).await {
                Ok(()) => {
                    tracing::debug!("Upgrade lock released (attempt {attempt})");
                    break;
                }
                Err(e) => {
                    tracing::debug!(
                        "Failed to release upgrade lock, retrying (attempt {attempt}): {e}"
                    );
                }
            }
        }
    }

    result
}

const SERVER_CONFIG_JSON: &str = "/opt/tako/config.json";
const DNS_CREDENTIALS_ENV: &str = "/opt/tako/dns-credentials.env";
const LEGO_VERSION: &str = "4.21.0";

/// Return a shell command to quickly verify credentials for a provider, if
/// supported. The command should exit 0 on success, non-zero on failure.
fn credential_verify_command(provider: &str, credentials: &[(String, String)]) -> Option<String> {
    match provider {
        "cloudflare" => {
            let token = credentials.iter().find(|(k, _)| k == "CF_DNS_API_TOKEN")?;
            let escaped = token.1.replace('\'', "'\\''");
            Some(format!(
                "curl -sf -H 'Authorization: Bearer {}' \
                 https://api.cloudflare.com/client/v4/user/tokens/verify \
                 | grep -q '\"active\"'",
                escaped,
            ))
        }
        "digitalocean" => {
            let token = credentials.iter().find(|(k, _)| k == "DO_AUTH_TOKEN")?;
            let escaped = token.1.replace('\'', "'\\''");
            Some(format!(
                "curl -sf -H 'Authorization: Bearer {}' \
                 https://api.digitalocean.com/v2/account \
                 | grep -q '\"account\"'",
                escaped,
            ))
        }
        "hetzner" => {
            let token = credentials.iter().find(|(k, _)| k == "HETZNER_API_KEY")?;
            let escaped = token.1.replace('\'', "'\\''");
            Some(format!(
                "curl -sf -H 'Auth-API-Token: {}' \
                 https://dns.hetzner.com/api/v1/zones >/dev/null",
                escaped,
            ))
        }
        "vultr" => {
            let token = credentials.iter().find(|(k, _)| k == "VULTR_API_KEY")?;
            let escaped = token.1.replace('\'', "'\\''");
            Some(format!(
                "curl -sf -H 'Authorization: Bearer {}' \
                 https://api.vultr.com/v2/account >/dev/null",
                escaped,
            ))
        }
        "linode" => {
            let token = credentials.iter().find(|(k, _)| k == "LINODE_TOKEN")?;
            let escaped = token.1.replace('\'', "'\\''");
            Some(format!(
                "curl -sf -H 'Authorization: Bearer {}' \
                 https://api.linode.com/v4/profile >/dev/null",
                escaped,
            ))
        }
        _ => None,
    }
}

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
    let archive_name = lego_archive_name(arch);
    format!(
        "https://github.com/go-acme/lego/releases/download/v{version}/{archive_name}",
        version = LEGO_VERSION,
    )
}

fn lego_archive_name(arch: &str) -> String {
    let go_arch = match arch {
        "aarch64" | "arm64" => "arm64",
        _ => "amd64",
    };
    format!(
        "lego_v{version}_linux_{arch}.tar.gz",
        version = LEGO_VERSION,
        arch = go_arch,
    )
}

fn lego_checksums_url() -> String {
    format!(
        "https://github.com/go-acme/lego/releases/download/v{version}/lego_{version}_checksums.txt",
        version = LEGO_VERSION,
    )
}

async fn resolve_verified_lego_release_asset(arch: &str) -> Result<VerifiedReleaseAsset, String> {
    let archive_name = lego_archive_name(arch);
    let checksum_text = fetch_release_text(&lego_checksums_url()).await?;
    let expected_sha256 = parse_sha256_manifest_value(&checksum_text, &archive_name)?;
    Ok(VerifiedReleaseAsset {
        download_url: lego_download_url(arch),
        expected_sha256,
    })
}

fn install_lego_command(arch: &str, expected_sha256: &str) -> String {
    let url = lego_download_url(arch);
    let sha_check = verify_downloaded_sha256_script("\"$archive\"", expected_sha256);
    format!(
        "set -e; \
         if command -v lego >/dev/null 2>&1 && lego --version 2>&1 | grep -q '{version}'; then \
           echo 'lego {version} already installed'; \
         else \
           tmp=$(mktemp -d); \
           archive=\"$tmp/lego.tar.gz\"; \
           curl -fsSL '{url}' -o \"$archive\"; \
           {sha_check}; \
           tar -xzf \"$archive\" -C \"$tmp\" lego; \
           install -m 0755 \"$tmp/lego\" /usr/local/bin/lego; \
           rm -rf \"$tmp\"; \
           echo 'lego {version} installed'; \
         fi",
        version = LEGO_VERSION,
        url = url,
    )
}

/// DNS provider + credentials pair, used to copy config between servers.
pub struct DnsConfig {
    pub provider: String,
    pub credentials_env: String, // KEY=VALUE\n content
}

/// Read DNS provider config and credentials from a server that already has them.
pub async fn fetch_dns_config(
    ssh: &SshClient,
) -> Result<Option<DnsConfig>, Box<dyn std::error::Error>> {
    let config_json = ssh
        .exec(&format!("cat {} 2>/dev/null || true", SERVER_CONFIG_JSON))
        .await?
        .stdout
        .trim()
        .to_string();
    let provider = if !config_json.is_empty() {
        serde_json::from_str::<serde_json::Value>(&config_json)
            .ok()
            .and_then(|v| v.get("dns")?.get("provider")?.as_str().map(String::from))
            .unwrap_or_default()
    } else {
        String::new()
    };
    if provider.is_empty() {
        return Ok(None);
    }
    let creds_cmd = SshClient::run_with_root_or_sudo(&format!(
        "cat {} 2>/dev/null || true",
        DNS_CREDENTIALS_ENV,
    ));
    let credentials_env = ssh.exec(&creds_cmd).await?.stdout.trim().to_string();
    if credentials_env.is_empty() {
        return Ok(None);
    }
    Ok(Some(DnsConfig {
        provider,
        credentials_env: format!("{}\n", credentials_env),
    }))
}

/// Interactively prompt for DNS provider and credentials, verify them, and
/// return the config. Does not write anything to the server yet.
pub async fn prompt_dns_setup(ssh: &SshClient) -> Result<DnsConfig, Box<dyn std::error::Error>> {
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
        "Choose your DNS provider for Let's Encrypt DNS-01 challenges",
        None,
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
            let value = output::text_field(description, None)?;
            credentials.push((var_name.to_string(), value));
        }
    }

    if credentials.is_empty() {
        return Err("No credentials provided. DNS provider setup cancelled.".into());
    }

    // Quick credential validation via provider API before touching the server.
    if let Some(verify_cmd) = credential_verify_command(&provider, &credentials) {
        tracing::debug!("Verifying credentials for DNS provider {provider}…");
        let _t = output::timed(&format!("DNS credential verification ({provider})"));
        output::with_spinner_async_err(
            "Verifying credentials",
            "Credentials valid",
            "Verifying credentials",
            ssh.exec_checked(&verify_cmd),
        )
        .await
        .map_err(|_| -> Box<dyn std::error::Error> {
            "Credentials invalid. Check your API token and try again.".into()
        })?;
    }

    let mut env_content = String::new();
    for (key, value) in &credentials {
        env_content.push_str(&format!("{}={}\n", key, value));
    }

    Ok(DnsConfig {
        provider,
        credentials_env: env_content,
    })
}

/// Apply a DNS config to a server: install lego, write credentials, configure
/// systemd, and reload tako-server. The server must already be connected.
/// All intermediate output is suppressed — shows a single spinner line.
pub async fn apply_dns_config(
    ssh: &SshClient,
    name: &str,
    config: &DnsConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    let msg = format!("Configuring DNS on {name}");
    output::with_spinner_async_err(
        &msg,
        &format!("DNS configured on {name}"),
        &msg,
        apply_dns_config_inner(ssh, name, config),
    )
    .await
}

async fn apply_dns_config_inner(
    ssh: &SshClient,
    name: &str,
    config: &DnsConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    let provider = &config.provider;
    let _scope = output::scope(name).entered();
    tracing::debug!("Configuring DNS provider {provider}…");

    // Detect server architecture for lego download
    let _t = output::timed("Arch detection");
    let arch_output = ssh.exec("uname -m").await?;
    drop(_t);
    let arch = arch_output.stdout.trim();
    let arch = match arch {
        "x86_64" | "amd64" => "x86_64",
        "aarch64" | "arm64" => "aarch64",
        other => return Err(format!("Unsupported server architecture: {}", other).into()),
    };
    tracing::debug!("Server architecture: {arch}");

    // Install lego binary
    let _t = output::timed("Lego install");
    let verified_lego = resolve_verified_lego_release_asset(arch)
        .await
        .map_err(|e| format!("Failed to verify lego release metadata: {e}"))?;
    let install_cmd = install_lego_command(arch, &verified_lego.expected_sha256);
    let install_cmd = SshClient::run_with_root_or_sudo(&install_cmd);
    ssh.exec_checked(&install_cmd).await?;
    drop(_t);
    tracing::debug!("Lego binary installed");

    // Write credentials env file
    let _t = output::timed("Credentials write");
    let escaped_content = config.credentials_env.replace('\'', "'\\''");
    let write_creds_cmd = SshClient::run_with_root_or_sudo(&format!(
        "printf '%s' '{}' > {} && chmod 0600 {} && chown tako:tako {}",
        escaped_content, DNS_CREDENTIALS_ENV, DNS_CREDENTIALS_ENV, DNS_CREDENTIALS_ENV,
    ));
    ssh.exec_checked(&write_creds_cmd).await?;
    drop(_t);

    // Merge dns.provider into config.json
    let escaped_provider = provider.replace('\\', "\\\\").replace('"', "\\\"");
    let merge_config_cmd = SshClient::run_with_root_or_sudo(&format!(
        r#"CONFIG="{path}"; \
         EXISTING="$(cat "$CONFIG" 2>/dev/null || echo '{{}}')"; \
         if command -v jq >/dev/null 2>&1; then \
           echo "$EXISTING" | jq --arg p '{provider}' '.dns.provider = $p' > "$CONFIG.tmp"; \
         elif command -v python3 >/dev/null 2>&1; then \
           python3 -c "import json,sys; d=json.loads(sys.argv[1]); d.setdefault('dns',{{}}); d['dns']['provider']=sys.argv[2]; json.dump(d,open(sys.argv[3],'w'))" "$EXISTING" '{provider}' "$CONFIG.tmp"; \
         else \
           echo "error: jq or python3 required" >&2 && exit 1; \
         fi && \
         mv "$CONFIG.tmp" "$CONFIG" && chmod 0644 "$CONFIG" && chown tako:tako "$CONFIG""#,
        path = SERVER_CONFIG_JSON,
        provider = escaped_provider,
    ));
    ssh.exec_checked(&merge_config_cmd).await?;

    // Write systemd drop-in for EnvironmentFile (credentials only), reload, and restart
    let _t = output::timed("Systemd reload + restart");
    let dropin = format!("[Service]\nEnvironmentFile={}\n", DNS_CREDENTIALS_ENV,);
    let escaped_dropin = dropin.replace('\'', "'\\''");
    let write_dropin_cmd = SshClient::run_with_root_or_sudo(&format!(
        "mkdir -p /etc/systemd/system/tako-server.service.d && \
         printf '%s' '{}' > /etc/systemd/system/tako-server.service.d/dns.conf && \
         systemctl daemon-reload && \
         systemctl restart tako-server",
        escaped_dropin,
    ));
    ssh.exec_checked(&write_dropin_cmd).await?;
    drop(_t);
    tracing::debug!("DNS configured, tako-server restarted");

    // Verify the new config took effect (retry up to 5 times)
    for attempt in 0..5 {
        tokio::time::sleep(Duration::from_secs(if attempt == 0 { 2 } else { 3 })).await;
        match ssh.tako_server_info().await {
            Ok(info) if info.dns_provider.as_deref() == Some(provider.as_str()) => {
                return Ok(());
            }
            Ok(_) if attempt < 4 => continue,
            Ok(info) => {
                return Err(format!(
                    "DNS provider on {} is {:?} after restart (expected '{}').\n\
                     Try: tako servers restart {}",
                    name, info.dns_provider, provider, name,
                )
                .into());
            }
            Err(_) if attempt < 4 => continue,
            Err(e) => {
                return Err(format!(
                    "Could not verify DNS config on {} after restart: {}",
                    name, e,
                )
                .into());
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;

    const TEST_SERVER_CHECKSUM_MANIFEST: &str = "1111111111111111111111111111111111111111111111111111111111111111  tako-server-linux-x86_64-glibc.tar.zst\n\
         2222222222222222222222222222222222222222222222222222222222222222  tako-server-linux-aarch64-musl.tar.zst\n";
    const TEST_SERVER_CHECKSUM_MANIFEST_SIG_BASE64: &str = "nZdPJ9zO2xgD3KYpdDWovNaMNko8XtBjcqSJVdNZs0aIwKKfc4pG8g0paADEUHIjwabW80jfj35n5qmEH1ko111qsUUsNwdB0ewUAckN5fvO+tprTmhWsFV9653I7q36LzFT3E3ORNI5JUHLQKqgn15DoOloPR7pi1sU/r4y2FFXJcfBIir0LR5jrR9eXuyPAqDDJSX2QJX19WtEnWNXZsAZUaTsHUtXrlHdqtQDb9fA+pr3w+dVUjg12mYRBi1CJbnxTbrZUyy7+LMDQwXWagTjivHXCaSiZVGz4JGuEMds838wNsy8nfwCqXhffrMXuIb3sOZ6sfPVLZgeUnr12ZpkDjYEiDAz0HEekNQUIIQqjvlcIkgxZYByZLRap0Vvi4NMfPkRI7K7FDtY1hhs7CurJ7Xcag784cx5V+pFEPIbCfMnEjK/beP+V36UbSbjnbOtbw4WUKQZH+knspw+MUBmy3ZdqGsgYDSyVQ6dE5u7lvl4V9/ai8f5pue5uWgL";

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
        let cmd = install_lego_command(
            "x86_64",
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        );
        assert!(cmd.contains("lego --version"));
        assert!(cmd.contains(LEGO_VERSION));
        assert!(cmd.contains("/usr/local/bin/lego"));
        assert!(cmd.contains("sha256 mismatch"));
    }

    #[test]
    fn lego_checksums_url_uses_release_checksums_asset() {
        assert_eq!(
            lego_checksums_url(),
            format!(
                "https://github.com/go-acme/lego/releases/download/v{version}/lego_{version}_checksums.txt",
                version = LEGO_VERSION
            )
        );
    }

    #[test]
    fn server_binary_download_url_canary() {
        let target = crate::config::ServerTarget {
            arch: "x86_64".to_string(),
            libc: "glibc".to_string(),
        };
        let url =
            server_binary_download_url(UpgradeChannel::Canary, None, &target, None, false).unwrap();
        assert_eq!(
            url,
            "https://github.com/lilienblum/tako/releases/download/canary-latest/tako-server-linux-x86_64-glibc.tar.zst"
        );
    }

    #[test]
    fn server_binary_download_url_stable_with_tag() {
        let target = crate::config::ServerTarget {
            arch: "aarch64".to_string(),
            libc: "musl".to_string(),
        };
        let url = server_binary_download_url(
            UpgradeChannel::Stable,
            Some("tako-server-v0.1.0"),
            &target,
            None,
            false,
        )
        .unwrap();
        assert_eq!(
            url,
            "https://github.com/lilienblum/tako/releases/download/tako-server-v0.1.0/tako-server-linux-aarch64-musl.tar.zst"
        );
    }

    #[test]
    fn server_binary_download_url_rejects_insecure_custom_base_without_override() {
        let target = crate::config::ServerTarget {
            arch: "x86_64".to_string(),
            libc: "glibc".to_string(),
        };
        let err = server_binary_download_url(
            UpgradeChannel::Stable,
            Some("tako-server-v0.1.0"),
            &target,
            Some("http://example.test/releases"),
            false,
        )
        .unwrap_err();
        assert!(err.contains("must use https://"));
    }

    #[test]
    fn server_binary_download_url_allows_insecure_custom_base_with_explicit_override() {
        let target = crate::config::ServerTarget {
            arch: "x86_64".to_string(),
            libc: "glibc".to_string(),
        };
        let url = server_binary_download_url(
            UpgradeChannel::Stable,
            Some("tako-server-v0.1.0"),
            &target,
            Some("http://example.test/releases"),
            true,
        )
        .unwrap();
        assert_eq!(
            url,
            "http://example.test/releases/tako-server-linux-x86_64-glibc.tar.zst"
        );
    }

    #[test]
    fn parse_sha256_manifest_value_finds_named_asset() {
        let sha = parse_sha256_manifest_value(
            TEST_SERVER_CHECKSUM_MANIFEST,
            "tako-server-linux-aarch64-musl.tar.zst",
        )
        .unwrap();
        assert_eq!(
            sha,
            "2222222222222222222222222222222222222222222222222222222222222222"
        );
    }

    #[test]
    fn verify_signed_server_checksum_manifest_accepts_valid_signature() {
        let signature = base64::engine::general_purpose::STANDARD
            .decode(TEST_SERVER_CHECKSUM_MANIFEST_SIG_BASE64)
            .unwrap();
        verify_signed_server_checksum_manifest(
            TEST_SERVER_CHECKSUM_MANIFEST.as_bytes(),
            &signature,
        )
        .unwrap();
    }

    #[test]
    fn verify_signed_server_checksum_manifest_rejects_tampering() {
        let signature = base64::engine::general_purpose::STANDARD
            .decode(TEST_SERVER_CHECKSUM_MANIFEST_SIG_BASE64)
            .unwrap();
        let err = verify_signed_server_checksum_manifest(
            b"1111111111111111111111111111111111111111111111111111111111111111  tako-server-linux-x86_64-glibc.tar.zst\n",
            &signature,
        )
        .unwrap_err();
        assert!(err.contains("signature verification failed"));
    }

    #[test]
    fn remote_binary_replace_command_uses_root_shell_wrapper_and_verifies_sha256() {
        let cmd = remote_binary_replace_command(
            "https://example.com/tako-server.tar.zst",
            "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789",
        );
        assert!(cmd.contains("then sh -c '"));
        assert!(cmd.contains("sudo sh -c '"));
        assert!(cmd.contains("curl -fsSL"));
        assert!(cmd.contains("sha256 mismatch"));
        assert!(cmd.contains("abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789"));
        assert!(cmd.contains("install -m 0755"));
        assert!(cmd.contains("/usr/local/bin/tako-server"));
    }

    #[test]
    fn build_upgrade_owner_differs_by_server_name() {
        let a = build_upgrade_owner("prod-1");
        let b = build_upgrade_owner("prod-2");
        assert_ne!(a, b, "different servers should produce different owner IDs");
        assert!(a.contains("prod-1"));
        assert!(b.contains("prod-2"));
    }

    #[test]
    fn first_non_empty_line_skips_blanks() {
        assert_eq!(first_non_empty_line("\n\n  hello\nworld"), Some("hello"));
        assert_eq!(first_non_empty_line(""), None);
        assert_eq!(first_non_empty_line("\n\n"), None);
        assert_eq!(first_non_empty_line("first"), Some("first"));
    }
}

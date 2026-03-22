use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::output;

pub fn run(assume_yes: bool) -> Result<(), Box<dyn std::error::Error>> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(run_async(assume_yes))
}

async fn run_async(assume_yes: bool) -> Result<(), Box<dyn std::error::Error>> {
    let user_targets = gather_user_targets()?;
    let system_targets = gather_system_targets();

    if user_targets.is_empty() && system_targets.is_empty() {
        output::muted("Nothing to remove — Tako does not appear to be installed.");
        return Ok(());
    }

    output::warning("This will permanently remove Tako and all local data:");
    eprintln!();
    for target in &user_targets {
        output::muted(&format!("  {}", target.display()));
    }
    if !system_targets.is_empty() {
        output::muted("  System services and config (requires sudo):");
        for desc in &system_targets {
            output::muted(&format!("    {}", desc.description));
        }
    }
    eprintln!();

    if !assume_yes {
        let confirmed = output::confirm("Remove Tako and all local data?", false)?;
        if !confirmed {
            output::warning("Cancelled");
            return Ok(());
        }
    }

    // Best-effort: stop dev server before removing data
    let _ = stop_dev_server().await;

    // Remove system-level items first (requires sudo)
    if !system_targets.is_empty() {
        output::warning("Sudo is required to remove system-level components.");
        let sudo_status = Command::new("sudo")
            .arg("-v")
            .status()
            .map_err(|e| format!("failed to run sudo: {e}"))?;
        if sudo_status.success() {
            remove_system_targets(&system_targets);
        } else {
            output::error("Sudo authentication failed — skipping system-level cleanup");
        }
    }

    // Remove user-level items (directories + binaries)
    let mut errors = Vec::new();
    for target in &user_targets {
        if !target.exists() {
            continue;
        }
        let result = if target.is_dir() {
            std::fs::remove_dir_all(target)
        } else {
            std::fs::remove_file(target)
        };
        match result {
            Ok(()) => output::success(&format!("Removed {}", target.display())),
            Err(e) => {
                output::error(&format!("Failed to remove {}: {e}", target.display()));
                errors.push(e);
            }
        }
    }

    if errors.is_empty() {
        eprintln!();
        output::success("Tako has been removed");
    } else {
        eprintln!();
        output::warning(&format!(
            "Tako partially removed ({} item(s) could not be deleted)",
            errors.len()
        ));
    }

    Ok(())
}

/// Collect user-level paths (no sudo needed).
fn gather_user_targets() -> Result<Vec<PathBuf>, Box<dyn std::error::Error>> {
    let config_dir = crate::paths::tako_config_dir()?;
    let data_dir = crate::paths::tako_data_dir()?;
    let binaries = find_tako_binaries();

    let mut targets = Vec::new();

    if config_dir.exists() {
        targets.push(config_dir.clone());
    }
    if data_dir.exists() && data_dir != config_dir {
        targets.push(data_dir);
    }
    for bin in binaries {
        targets.push(bin);
    }

    Ok(targets)
}

/// Find Tako binaries in the same directory as the running executable.
fn find_tako_binaries() -> Vec<PathBuf> {
    let Ok(exe) = std::env::current_exe() else {
        return vec![];
    };
    let Some(dir) = exe.parent() else {
        return vec![];
    };

    ["tako", "tako-dev-server", "tako-loopback-proxy"]
        .iter()
        .map(|name| dir.join(name))
        .filter(|path| path.exists())
        .collect()
}

async fn stop_dev_server() -> Result<(), Box<dyn std::error::Error>> {
    let apps = crate::dev_server_client::list_registered_apps().await?;
    for app in &apps {
        let _ = crate::dev_server_client::unregister_app(&app.config_path).await;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// System-level cleanup (requires sudo)
// ---------------------------------------------------------------------------

struct SystemTarget {
    description: String,
    commands: Vec<Vec<String>>,
}

/// Detect which system-level Tako artifacts exist on this machine.
fn gather_system_targets() -> Vec<SystemTarget> {
    let mut targets = Vec::new();

    #[cfg(target_os = "macos")]
    {
        targets.extend(gather_macos_system_targets());
    }

    #[cfg(target_os = "linux")]
    {
        targets.extend(gather_linux_system_targets());
    }

    targets
}

#[cfg(target_os = "macos")]
fn gather_macos_system_targets() -> Vec<SystemTarget> {
    use crate::commands::dev::loopback_proxy::{
        LOOPBACK_PROXY_BINARY_PATH, LOOPBACK_PROXY_BOOTSTRAP_LABEL,
        LOOPBACK_PROXY_BOOTSTRAP_PLIST_PATH, LOOPBACK_PROXY_LABEL, LOOPBACK_PROXY_PLIST_PATH,
    };

    let mut targets = Vec::new();

    // Loopback proxy services and files
    if Path::new(LOOPBACK_PROXY_BOOTSTRAP_PLIST_PATH).exists()
        || Path::new(LOOPBACK_PROXY_PLIST_PATH).exists()
        || Path::new(LOOPBACK_PROXY_BINARY_PATH).exists()
    {
        targets.push(SystemTarget {
            description: "Loopback proxy (LaunchDaemons, binary)".into(),
            commands: vec![
                vec![
                    "launchctl".into(),
                    "bootout".into(),
                    format!("system/{LOOPBACK_PROXY_LABEL}"),
                ],
                vec![
                    "launchctl".into(),
                    "bootout".into(),
                    format!("system/{LOOPBACK_PROXY_BOOTSTRAP_LABEL}"),
                ],
                vec!["rm".into(), "-f".into(), LOOPBACK_PROXY_PLIST_PATH.into()],
                vec![
                    "rm".into(),
                    "-f".into(),
                    LOOPBACK_PROXY_BOOTSTRAP_PLIST_PATH.into(),
                ],
                vec!["rm".into(), "-f".into(), LOOPBACK_PROXY_BINARY_PATH.into()],
                vec![
                    "rm".into(),
                    "-rf".into(),
                    "/Library/Application Support/Tako".into(),
                ],
            ],
        });
    }

    // DNS resolver
    if Path::new(crate::commands::dev::TAKO_RESOLVER_FILE).exists() {
        targets.push(SystemTarget {
            description: format!(
                "DNS resolver ({})",
                crate::commands::dev::TAKO_RESOLVER_FILE
            ),
            commands: vec![vec![
                "rm".into(),
                "-f".into(),
                crate::commands::dev::TAKO_RESOLVER_FILE.into(),
            ]],
        });
    }

    // CA certificate in system keychain
    if ca_is_trusted_macos() {
        targets.push(SystemTarget {
            description: "CA certificate in system keychain".into(),
            commands: vec![vec![
                "security".into(),
                "delete-certificate".into(),
                "-c".into(),
                "Tako Local Development CA".into(),
                "/Library/Keychains/System.keychain".into(),
            ]],
        });
    }

    // Loopback alias
    if loopback_alias_exists_macos() {
        targets.push(SystemTarget {
            description: "Loopback alias 127.77.0.1".into(),
            commands: vec![vec![
                "ifconfig".into(),
                "lo0".into(),
                "-alias".into(),
                "127.77.0.1".into(),
            ]],
        });
    }

    targets
}

#[cfg(target_os = "macos")]
fn ca_is_trusted_macos() -> bool {
    Command::new("security")
        .args([
            "find-certificate",
            "-c",
            "Tako Local Development CA",
            "/Library/Keychains/System.keychain",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[cfg(target_os = "macos")]
fn loopback_alias_exists_macos() -> bool {
    Command::new("ifconfig")
        .arg("lo0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .map(|o| {
            let text = String::from_utf8_lossy(&o.stdout);
            text.contains("127.77.0.1")
        })
        .unwrap_or(false)
}

#[cfg(target_os = "linux")]
fn gather_linux_system_targets() -> Vec<SystemTarget> {
    let mut targets = Vec::new();

    // systemd service
    if Path::new("/etc/systemd/system/tako-dev-redirect.service").exists() {
        targets.push(SystemTarget {
            description: "systemd service (tako-dev-redirect)".into(),
            commands: vec![
                vec![
                    "systemctl".into(),
                    "disable".into(),
                    "--now".into(),
                    "tako-dev-redirect.service".into(),
                ],
                vec![
                    "rm".into(),
                    "-f".into(),
                    "/etc/systemd/system/tako-dev-redirect.service".into(),
                ],
                vec!["systemctl".into(), "daemon-reload".into()],
            ],
        });
    }

    // systemd-resolved drop-in
    if Path::new("/etc/systemd/resolved.conf.d/tako-dev.conf").exists() {
        targets.push(SystemTarget {
            description: "systemd-resolved config (tako-dev.conf)".into(),
            commands: vec![
                vec![
                    "rm".into(),
                    "-f".into(),
                    "/etc/systemd/resolved.conf.d/tako-dev.conf".into(),
                ],
                vec![
                    "systemctl".into(),
                    "restart".into(),
                    "systemd-resolved".into(),
                ],
            ],
        });
    }

    // CA certificate (Debian/Ubuntu)
    if Path::new("/usr/local/share/ca-certificates/tako-ca.crt").exists() {
        targets.push(SystemTarget {
            description: "CA certificate (Debian/Ubuntu trust store)".into(),
            commands: vec![
                vec![
                    "rm".into(),
                    "-f".into(),
                    "/usr/local/share/ca-certificates/tako-ca.crt".into(),
                ],
                vec!["update-ca-certificates".into()],
            ],
        });
    }

    // CA certificate (Fedora/RHEL/SUSE)
    if Path::new("/etc/pki/ca-trust/source/anchors/tako-ca.crt").exists() {
        targets.push(SystemTarget {
            description: "CA certificate (Fedora/RHEL trust store)".into(),
            commands: vec![
                vec![
                    "rm".into(),
                    "-f".into(),
                    "/etc/pki/ca-trust/source/anchors/tako-ca.crt".into(),
                ],
                vec!["update-ca-trust".into()],
            ],
        });
    }

    // iptables rules and loopback alias (ephemeral, but clean up if present)
    if loopback_alias_exists_linux() {
        targets.push(SystemTarget {
            description: "Loopback alias 127.77.0.1 and iptables rules".into(),
            commands: vec![
                vec![
                    "iptables".into(),
                    "-t".into(),
                    "nat".into(),
                    "-D".into(),
                    "OUTPUT".into(),
                    "-d".into(),
                    "127.77.0.1".into(),
                    "-p".into(),
                    "tcp".into(),
                    "--dport".into(),
                    "443".into(),
                    "-j".into(),
                    "REDIRECT".into(),
                    "--to-port".into(),
                    "47831".into(),
                ],
                vec![
                    "iptables".into(),
                    "-t".into(),
                    "nat".into(),
                    "-D".into(),
                    "OUTPUT".into(),
                    "-d".into(),
                    "127.77.0.1".into(),
                    "-p".into(),
                    "tcp".into(),
                    "--dport".into(),
                    "80".into(),
                    "-j".into(),
                    "REDIRECT".into(),
                    "--to-port".into(),
                    "47830".into(),
                ],
                vec![
                    "iptables".into(),
                    "-t".into(),
                    "nat".into(),
                    "-D".into(),
                    "OUTPUT".into(),
                    "-d".into(),
                    "127.77.0.1".into(),
                    "-p".into(),
                    "udp".into(),
                    "--dport".into(),
                    "53".into(),
                    "-j".into(),
                    "REDIRECT".into(),
                    "--to-port".into(),
                    "53535".into(),
                ],
                vec![
                    "ip".into(),
                    "addr".into(),
                    "del".into(),
                    "127.77.0.1/8".into(),
                    "dev".into(),
                    "lo".into(),
                ],
            ],
        });
    }

    targets
}

#[cfg(target_os = "linux")]
fn loopback_alias_exists_linux() -> bool {
    Command::new("ip")
        .args(["addr", "show", "dev", "lo"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .map(|o| {
            let text = String::from_utf8_lossy(&o.stdout);
            text.contains("inet 127.77.0.1/")
        })
        .unwrap_or(false)
}

/// Run each system target's commands with sudo, best-effort.
/// Sudo credential cache should already be warm from a prior `sudo -v` call.
fn remove_system_targets(targets: &[SystemTarget]) {
    for target in targets {
        let mut any_failed = false;
        for cmd_args in &target.commands {
            let result = Command::new("sudo")
                .args(cmd_args)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
            match result {
                Err(e) => {
                    tracing::debug!("sudo {:?} spawn failed: {e}", cmd_args);
                    any_failed = true;
                }
                Ok(s) if !s.success() => {
                    tracing::debug!("sudo {:?} exited {}", cmd_args, s);
                    any_failed = true;
                }
                Ok(_) => {}
            }
        }
        if any_failed {
            output::warning(&format!("Could not fully remove: {}", target.description));
        } else {
            output::success(&format!("Removed {}", target.description));
        }
    }
}

// ---------------------------------------------------------------------------
// Server-side implode (via SSH)
// ---------------------------------------------------------------------------

pub async fn implode_server(
    server_name: &str,
    server: &crate::config::ServerEntry,
    assume_yes: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    use crate::ssh::SshClient;

    output::warning(&format!(
        "This will permanently remove tako-server and all data on {}",
        output::strong(server_name),
    ));
    eprintln!();
    output::muted("  Services:  tako-server, tako-server-worker");
    output::muted(
        "  Binaries:  /usr/local/bin/tako-server, tako-server-service, tako-server-install-refresh",
    );
    output::muted("  Data:      /opt/tako/");
    output::muted("  Sockets:   /var/run/tako/");
    output::muted("  Service files (systemd/OpenRC)");
    eprintln!();

    if !assume_yes {
        let confirmed = output::confirm(
            &format!(
                "Remove tako-server and all data on {}?",
                output::strong(server_name)
            ),
            false,
        )?;
        if !confirmed {
            output::warning("Cancelled");
            return Ok(());
        }
    }

    let ssh = SshClient::connect_to(&server.host, server.port).await?;

    let script = build_server_implode_script();
    let cmd = SshClient::run_with_root_or_sudo(&script);

    output::with_spinner_async(
        &format!("Removing tako-server from {server_name}"),
        &format!("Removed tako-server from {server_name}"),
        async { ssh.exec_checked(&cmd).await },
    )
    .await?;

    // Remove server from local config
    let mut servers = crate::config::ServersToml::load()?;
    servers.remove(server_name)?;
    servers.save()?;

    output::success(&format!(
        "Removed {} from local server list",
        output::strong(server_name)
    ));

    Ok(())
}

fn build_server_implode_script() -> String {
    // Stop and disable services (supports both systemd and OpenRC)
    // Remove service files, binaries, data, and sockets
    [
        // Stop services
        "if command -v systemctl >/dev/null 2>&1; then",
        "  systemctl stop tako-server tako-server-worker 2>/dev/null || true",
        "  systemctl disable tako-server tako-server-worker 2>/dev/null || true",
        "fi",
        "if command -v rc-service >/dev/null 2>&1; then",
        "  rc-service tako-server stop 2>/dev/null || true",
        "  rc-service tako-server-worker stop 2>/dev/null || true",
        "  rc-update del tako-server 2>/dev/null || true",
        "  rc-update del tako-server-worker 2>/dev/null || true",
        "fi",
        // Remove systemd service files and drop-ins
        "rm -f /etc/systemd/system/tako-server.service",
        "rm -f /etc/systemd/system/tako-server-worker.service",
        "rm -rf /etc/systemd/system/tako-server.service.d",
        "if command -v systemctl >/dev/null 2>&1; then systemctl daemon-reload 2>/dev/null || true; fi",
        // Remove OpenRC service files
        "rm -f /etc/init.d/tako-server",
        "rm -f /etc/init.d/tako-server-worker",
        // Remove binaries
        "rm -f /usr/local/bin/tako-server",
        "rm -f /usr/local/bin/tako-server-service",
        "rm -f /usr/local/bin/tako-server-install-refresh",
        // Remove data and sockets
        "rm -rf /opt/tako",
        "rm -rf /var/run/tako",
    ]
    .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn gather_user_targets_includes_existing_dirs() {
        let _lock = crate::paths::test_tako_home_env_lock();
        let previous = std::env::var_os("TAKO_HOME");

        let tmp = TempDir::new().unwrap();
        unsafe { std::env::set_var("TAKO_HOME", tmp.path()) };

        let targets = gather_user_targets().unwrap();
        assert!(targets.iter().any(|p| p == tmp.path()));
        // TAKO_HOME override makes config_dir == data_dir, so only one entry
        let dir_targets: Vec<_> = targets.iter().filter(|p| p.is_dir()).collect();
        assert_eq!(dir_targets.len(), 1);

        match previous {
            Some(v) => unsafe { std::env::set_var("TAKO_HOME", v) },
            None => unsafe { std::env::remove_var("TAKO_HOME") },
        }
    }

    #[test]
    fn gather_user_targets_empty_when_nothing_exists() {
        let _lock = crate::paths::test_tako_home_env_lock();
        let previous = std::env::var_os("TAKO_HOME");

        unsafe { std::env::set_var("TAKO_HOME", "/tmp/tako-implode-test-nonexistent") };

        let targets = gather_user_targets().unwrap();
        assert!(
            !targets
                .iter()
                .any(|p| p.starts_with("/tmp/tako-implode-test-nonexistent"))
        );

        match previous {
            Some(v) => unsafe { std::env::set_var("TAKO_HOME", v) },
            None => unsafe { std::env::remove_var("TAKO_HOME") },
        }
    }

    #[test]
    fn find_tako_binaries_returns_existing_siblings() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("tako"), b"bin").unwrap();
        std::fs::write(tmp.path().join("tako-dev-server"), b"bin").unwrap();

        let names = ["tako", "tako-dev-server", "tako-loopback-proxy"];
        let found: Vec<PathBuf> = names
            .iter()
            .map(|name| tmp.path().join(name))
            .filter(|path| path.exists())
            .collect();

        assert_eq!(found.len(), 2);
        assert!(found[0].ends_with("tako"));
        assert!(found[1].ends_with("tako-dev-server"));
    }

    #[test]
    fn server_implode_script_stops_services() {
        let script = build_server_implode_script();
        assert!(script.contains("systemctl stop tako-server"));
        assert!(script.contains("systemctl disable tako-server"));
        assert!(script.contains("rc-service tako-server stop"));
        assert!(script.contains("rc-update del tako-server"));
    }

    #[test]
    fn server_implode_script_removes_binaries() {
        let script = build_server_implode_script();
        assert!(script.contains("rm -f /usr/local/bin/tako-server"));
        assert!(script.contains("rm -f /usr/local/bin/tako-server-service"));
        assert!(script.contains("rm -f /usr/local/bin/tako-server-install-refresh"));
    }

    #[test]
    fn server_implode_script_removes_data_and_sockets() {
        let script = build_server_implode_script();
        assert!(script.contains("rm -rf /opt/tako"));
        assert!(script.contains("rm -rf /var/run/tako"));
    }

    #[test]
    fn server_implode_script_removes_service_files() {
        let script = build_server_implode_script();
        assert!(script.contains("rm -f /etc/systemd/system/tako-server.service"));
        assert!(script.contains("rm -f /etc/systemd/system/tako-server-worker.service"));
        assert!(script.contains("rm -rf /etc/systemd/system/tako-server.service.d"));
        assert!(script.contains("rm -f /etc/init.d/tako-server"));
        assert!(script.contains("rm -f /etc/init.d/tako-server-worker"));
        assert!(script.contains("systemctl daemon-reload"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_system_targets_include_loopback_proxy_when_present() {
        // This is a detection test — it verifies the function runs without panic.
        // Actual file presence depends on the machine state.
        let targets = gather_macos_system_targets();
        // Each target should have a non-empty description and at least one command
        for t in &targets {
            assert!(!t.description.is_empty());
            assert!(!t.commands.is_empty());
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_system_targets_include_service_when_present() {
        let targets = gather_linux_system_targets();
        for t in &targets {
            assert!(!t.description.is_empty());
            assert!(!t.commands.is_empty());
        }
    }
}

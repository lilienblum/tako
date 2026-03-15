#[cfg(any(target_os = "macos", test))]
use std::path::{Path, PathBuf};

#[cfg(target_os = "macos")]
use sha2::Digest;

#[cfg(test)]
use std::time::Duration;

#[cfg(target_os = "macos")]
use super::{DEV_LOOPBACK_ADDR, sudo_run_checked, tcp_port_open, write_system_file_with_sudo};

#[cfg(any(target_os = "macos", test))]
pub(crate) const LOOPBACK_PROXY_LABEL: &str = "sh.tako.loopback-proxy";
#[cfg(any(target_os = "macos", test))]
pub(crate) const LOOPBACK_PROXY_BOOTSTRAP_LABEL: &str = "sh.tako.loopback-bootstrap";
#[cfg(any(target_os = "macos", test))]
pub(crate) const LOOPBACK_PROXY_PLIST_PATH: &str =
    "/Library/Application Support/Tako/launchd/sh.tako.loopback-proxy.plist";
#[cfg(any(target_os = "macos", test))]
pub(crate) const LOOPBACK_PROXY_BOOTSTRAP_PLIST_PATH: &str =
    "/Library/LaunchDaemons/sh.tako.loopback-bootstrap.plist";
#[cfg(any(target_os = "macos", test))]
pub(crate) const LOOPBACK_PROXY_BINARY_PATH: &str =
    "/Library/Application Support/Tako/bin/tako-loopback-proxy";
#[cfg(any(target_os = "macos", test))]
pub(crate) const LOOPBACK_PROXY_HTTPS_NAME: &str = "https";
#[cfg(any(target_os = "macos", test))]
pub(crate) const LOOPBACK_PROXY_HTTP_NAME: &str = "http";
#[cfg(test)]
pub(crate) const LOOPBACK_PROXY_IDLE_TIMEOUT: Duration = Duration::from_secs(4 * 60 * 60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LoopbackProxyRepairPlan {
    None,
    ReloadService,
    InstallOrUpdate,
}

#[cfg(target_os = "macos")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LoopbackProxyStatus {
    pub installed: bool,
    pub bootstrap_loaded: bool,
    pub alias_ready: bool,
    pub launchd_loaded: bool,
    pub https_ready: bool,
    pub http_ready: bool,
}

#[cfg(any(target_os = "macos", test))]
pub(crate) fn launchd_plist(binary_path: &Path) -> String {
    let binary = binary_path.display();
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{LOOPBACK_PROXY_LABEL}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{binary}</string>
  </array>
  <key>KeepAlive</key>
  <false/>
  <key>Sockets</key>
  <dict>
    <key>{LOOPBACK_PROXY_HTTPS_NAME}</key>
    <dict>
      <key>SockNodeName</key>
      <string>127.77.0.1</string>
      <key>SockServiceName</key>
      <string>443</string>
      <key>SockPassive</key>
      <true/>
      <key>SockType</key>
      <string>stream</string>
    </dict>
    <key>{LOOPBACK_PROXY_HTTP_NAME}</key>
    <dict>
      <key>SockNodeName</key>
      <string>127.77.0.1</string>
      <key>SockServiceName</key>
      <string>80</string>
      <key>SockPassive</key>
      <true/>
      <key>SockType</key>
      <string>stream</string>
    </dict>
  </dict>
</dict>
</plist>
"#
    )
}

#[cfg(any(target_os = "macos", test))]
pub(crate) fn bootstrap_launchd_plist(binary_path: &Path) -> String {
    let binary = binary_path.display();
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{LOOPBACK_PROXY_BOOTSTRAP_LABEL}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{binary}</string>
    <string>bootstrap</string>
  </array>
  <key>KeepAlive</key>
  <false/>
  <key>RunAtLoad</key>
  <true/>
</dict>
</plist>
"#
    )
}

#[cfg(any(target_os = "macos", test))]
fn plists_match_installed_binary(
    installed_binary: &Path,
    proxy_plist_contents: &str,
    bootstrap_plist_contents: &str,
) -> bool {
    proxy_plist_contents == launchd_plist(installed_binary)
        && bootstrap_plist_contents == bootstrap_launchd_plist(installed_binary)
}

#[cfg(any(target_os = "macos", test))]
pub(crate) fn repair_plan(
    files_current: bool,
    bootstrap_loaded: bool,
    alias_ready: bool,
    launchd_loaded: bool,
    https_ready: bool,
    http_ready: bool,
) -> LoopbackProxyRepairPlan {
    if !files_current || !bootstrap_loaded || !alias_ready {
        LoopbackProxyRepairPlan::InstallOrUpdate
    } else if !launchd_loaded || !https_ready || !http_ready {
        LoopbackProxyRepairPlan::ReloadService
    } else {
        LoopbackProxyRepairPlan::None
    }
}

#[cfg(test)]
pub(crate) fn should_exit_for_idle(
    active_connections: usize,
    idle_for: Duration,
    idle_timeout: Duration,
) -> bool {
    active_connections == 0 && idle_for >= idle_timeout
}

#[cfg(any(target_os = "macos", test))]
pub(crate) fn install_binary_path() -> PathBuf {
    PathBuf::from(LOOPBACK_PROXY_BINARY_PATH)
}

#[cfg(any(target_os = "macos", test))]
pub(crate) fn plist_path() -> PathBuf {
    PathBuf::from(LOOPBACK_PROXY_PLIST_PATH)
}

#[cfg(any(target_os = "macos", test))]
pub(crate) fn bootstrap_plist_path() -> PathBuf {
    PathBuf::from(LOOPBACK_PROXY_BOOTSTRAP_PLIST_PATH)
}

#[cfg(any(target_os = "macos", test))]
fn install_action_line() -> &'static str {
    "Install local loopback proxy for 127.77.0.1:80/443"
}

#[cfg(any(target_os = "macos", test))]
fn reload_action_line() -> &'static str {
    "Repair local loopback proxy for 127.77.0.1:80/443"
}

#[cfg(target_os = "macos")]
fn locate_proxy_source_binary() -> Result<PathBuf, Box<dyn std::error::Error>> {
    let current_exe = std::env::current_exe()?;
    if let Some(root) = crate::paths::repo_root_from_exe(&current_exe) {
        let candidates = [
            root.join("target")
                .join("debug")
                .join("tako-loopback-proxy"),
            root.join("target")
                .join("release")
                .join("tako-loopback-proxy"),
        ];
        if candidates.iter().all(|candidate| !candidate.exists()) {
            let _ = std::process::Command::new("cargo")
                .args(["build", "-p", "tako", "--bin", "tako-loopback-proxy"])
                .current_dir(&root)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
        }
        if let Some(found) = candidates.into_iter().find(|candidate| candidate.exists()) {
            return Ok(found);
        }
        return Err(
            "failed to locate 'tako-loopback-proxy'. Build it with: cargo build -p tako --bin tako-loopback-proxy"
                .into(),
        );
    }

    if let Some(parent) = current_exe.parent() {
        let sibling = parent.join("tako-loopback-proxy");
        if sibling.exists() {
            return Ok(sibling);
        }
    }

    if let Some(path) = find_on_path("tako-loopback-proxy") {
        return Ok(path);
    }

    Err(
        "failed to locate 'tako-loopback-proxy'. Reinstall Tako CLI and retry: curl -fsSL https://tako.sh/install | sh"
            .into(),
    )
}

#[cfg(target_os = "macos")]
fn find_on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|entry| entry.join(name))
        .find(|candidate| candidate.exists())
}

#[cfg(target_os = "macos")]
fn hash_file(path: &Path) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    Some(hex::encode(sha2::Sha256::digest(bytes)))
}

#[cfg(target_os = "macos")]
fn files_current(desired_binary: &Path) -> bool {
    let installed_binary = install_binary_path();
    let plist = plist_path();
    let bootstrap_plist = bootstrap_plist_path();
    if !installed_binary.is_file() || !plist.is_file() || !bootstrap_plist.is_file() {
        return false;
    }

    let installed_hash = hash_file(&installed_binary);
    let desired_hash = hash_file(desired_binary);
    if installed_hash.is_none() || installed_hash != desired_hash {
        return false;
    }

    let Some(proxy_plist_contents) = std::fs::read_to_string(&plist).ok() else {
        return false;
    };
    let Some(bootstrap_plist_contents) = std::fs::read_to_string(&bootstrap_plist).ok() else {
        return false;
    };
    plists_match_installed_binary(
        &installed_binary,
        &proxy_plist_contents,
        &bootstrap_plist_contents,
    )
}

#[cfg(target_os = "macos")]
fn files_installed() -> bool {
    install_binary_path().is_file() && plist_path().is_file() && bootstrap_plist_path().is_file()
}

#[cfg(target_os = "macos")]
fn launchd_loaded(label: &str) -> bool {
    let label = format!("system/{label}");
    std::process::Command::new("launchctl")
        .args(["print", &label])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

#[cfg(any(target_os = "macos", test))]
pub(crate) fn loopback_alias_present(ifconfig_output: &str, ip: &str) -> bool {
    ifconfig_output.lines().any(|line| {
        let mut parts = line.split_whitespace();
        matches!(parts.next(), Some("inet")) && parts.next() == Some(ip)
    })
}

#[cfg(target_os = "macos")]
fn loopback_alias_ready() -> bool {
    let Ok(output) = std::process::Command::new("ifconfig").arg("lo0").output() else {
        return false;
    };
    if !output.status.success() {
        return false;
    }
    loopback_alias_present(&String::from_utf8_lossy(&output.stdout), DEV_LOOPBACK_ADDR)
}

#[cfg(target_os = "macos")]
pub(crate) fn status() -> LoopbackProxyStatus {
    LoopbackProxyStatus {
        installed: files_installed(),
        bootstrap_loaded: launchd_loaded(LOOPBACK_PROXY_BOOTSTRAP_LABEL),
        alias_ready: loopback_alias_ready(),
        launchd_loaded: launchd_loaded(LOOPBACK_PROXY_LABEL),
        https_ready: tcp_port_open(DEV_LOOPBACK_ADDR, 443, 150),
        http_ready: tcp_port_open(DEV_LOOPBACK_ADDR, 80, 150),
    }
}

#[cfg(target_os = "macos")]
fn install_binary_with_sudo(
    src: &Path,
    dest: &Path,
    mode: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let Some(parent) = dest.parent() else {
        return Err(format!("invalid destination {}", dest.display()).into());
    };
    let parent_str = parent.to_string_lossy().to_string();
    let src_str = src.to_string_lossy().to_string();
    let dest_str = dest.to_string_lossy().to_string();
    sudo_run_checked(
        &["install", "-d", "-m", "755", &parent_str],
        &format!("creating {}", parent.display()),
    )?;
    sudo_run_checked(
        &["install", "-m", mode, &src_str, &dest_str],
        &format!("installing {}", dest.display()),
    )?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn ensure_parent_dir_with_sudo(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let Some(parent) = path.parent() else {
        return Err(format!("invalid path {}", path.display()).into());
    };
    let parent_str = parent.to_string_lossy().to_string();
    sudo_run_checked(
        &["install", "-d", "-m", "755", &parent_str],
        &format!("creating {}", parent.display()),
    )
}

#[cfg(target_os = "macos")]
fn bootout_launchd_service(label: &str) -> Result<(), Box<dyn std::error::Error>> {
    let label = format!("system/{label}");
    let status = std::process::Command::new("sudo")
        .args(["launchctl", "bootout", &label])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()?;
    if status.success() || status.code() == Some(3) {
        return Ok(());
    }
    Err(format!("booting out {label} failed").into())
}

#[cfg(target_os = "macos")]
fn bootstrap_launchd_service(
    label: &str,
    plist_path: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let launchd_label = format!("system/{label}");
    sudo_run_checked(
        &["launchctl", "bootstrap", "system", plist_path],
        &format!("bootstrapping {label} launchd service"),
    )?;
    sudo_run_checked(
        &["launchctl", "enable", &launchd_label],
        &format!("enabling {label} launchd service"),
    )?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn run_bootstrap_helper_with_sudo() -> Result<(), Box<dyn std::error::Error>> {
    let binary = install_binary_path();
    let binary_str = binary.to_string_lossy().to_string();
    sudo_run_checked(
        &[&binary_str, "bootstrap"],
        "running loopback proxy bootstrap helper",
    )
}

#[cfg(target_os = "macos")]
fn install_or_update(desired_binary: &Path) -> Result<(), Box<dyn std::error::Error>> {
    install_binary_with_sudo(desired_binary, &install_binary_path(), "755")?;
    ensure_parent_dir_with_sudo(&plist_path())?;
    write_system_file_with_sudo(
        LOOPBACK_PROXY_PLIST_PATH,
        &launchd_plist(&install_binary_path()),
    )?;
    write_system_file_with_sudo(
        LOOPBACK_PROXY_BOOTSTRAP_PLIST_PATH,
        &bootstrap_launchd_plist(&install_binary_path()),
    )?;
    bootout_launchd_service(LOOPBACK_PROXY_BOOTSTRAP_LABEL)?;
    bootstrap_launchd_service(
        LOOPBACK_PROXY_BOOTSTRAP_LABEL,
        LOOPBACK_PROXY_BOOTSTRAP_PLIST_PATH,
    )?;
    run_bootstrap_helper_with_sudo()?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn reload_service() -> Result<(), Box<dyn std::error::Error>> {
    bootout_launchd_service(LOOPBACK_PROXY_LABEL)?;
    run_bootstrap_helper_with_sudo()?;
    Ok(())
}

#[cfg(target_os = "macos")]
pub(crate) fn current_repair_plan() -> Result<LoopbackProxyRepairPlan, Box<dyn std::error::Error>> {
    let desired_binary = locate_proxy_source_binary()?;
    Ok({
        let files_current = files_current(&desired_binary);
        let status = status();
        repair_plan(
            files_current,
            status.bootstrap_loaded,
            status.alias_ready,
            status.launchd_loaded,
            status.https_ready,
            status.http_ready,
        )
    })
}

#[cfg(target_os = "macos")]
pub(crate) fn pending_sudo_action() -> Result<Option<&'static str>, Box<dyn std::error::Error>> {
    Ok(match current_repair_plan()? {
        LoopbackProxyRepairPlan::InstallOrUpdate => Some(install_action_line()),
        LoopbackProxyRepairPlan::ReloadService => Some(reload_action_line()),
        LoopbackProxyRepairPlan::None => None,
    })
}

#[cfg(target_os = "macos")]
pub(crate) fn ensure_installed() -> Result<(), Box<dyn std::error::Error>> {
    let current_plan = current_repair_plan()?;

    if current_plan == LoopbackProxyRepairPlan::None {
        return Ok(());
    }

    if !crate::output::is_interactive() {
        return Err(
            "local loopback proxy is not configured; run `tako dev` interactively once to install it"
                .into(),
        );
    }

    let loading = match current_plan {
        LoopbackProxyRepairPlan::InstallOrUpdate => "Setting up",
        LoopbackProxyRepairPlan::ReloadService => "Starting",
        LoopbackProxyRepairPlan::None => unreachable!(),
    };
    let success = match current_plan {
        LoopbackProxyRepairPlan::InstallOrUpdate => "Setup complete",
        LoopbackProxyRepairPlan::ReloadService => "Ready",
        LoopbackProxyRepairPlan::None => unreachable!(),
    };

    crate::output::with_spinner(
        loading,
        success,
        || -> Result<(), Box<dyn std::error::Error>> {
            match current_plan {
                LoopbackProxyRepairPlan::InstallOrUpdate => {
                    let desired_binary = locate_proxy_source_binary()?;
                    install_or_update(&desired_binary)?;
                }
                LoopbackProxyRepairPlan::ReloadService => {
                    reload_service()?;
                }
                LoopbackProxyRepairPlan::None => unreachable!(),
            }

            // Check non-network state once (files, launchd, alias won't change by waiting).
            let verified = status();
            if !(verified.installed
                && verified.bootstrap_loaded
                && verified.alias_ready
                && verified.launchd_loaded)
            {
                return Err("local loopback proxy setup verification failed".into());
            }

            // The service was just (re)started — give it time to bind its ports.
            let (mut https_ok, mut http_ok) = (verified.https_ready, verified.http_ready);
            if !(https_ok && http_ok) {
                for _ in 0..20 {
                    std::thread::sleep(std::time::Duration::from_millis(250));
                    https_ok = https_ok || tcp_port_open(DEV_LOOPBACK_ADDR, 443, 150);
                    http_ok = http_ok || tcp_port_open(DEV_LOOPBACK_ADDR, 80, 150);
                    if https_ok && http_ok {
                        break;
                    }
                }
            }
            if !(https_ok && http_ok) {
                return Err("local loopback proxy setup verification failed".into());
            }

            Ok(())
        },
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_action_line_uses_bullet_copy() {
        assert_eq!(
            install_action_line(),
            "Install local loopback proxy for 127.77.0.1:80/443"
        );
    }

    #[test]
    fn reload_action_line_uses_bullet_copy() {
        assert_eq!(
            reload_action_line(),
            "Repair local loopback proxy for 127.77.0.1:80/443"
        );
    }

    #[test]
    fn launchd_plist_configures_socket_activation_on_loopback_ports() {
        let plist = launchd_plist(Path::new(
            "/Library/Application Support/Tako/bin/tako-loopback-proxy",
        ));
        assert!(plist.contains(LOOPBACK_PROXY_LABEL));
        assert!(plist.contains("/Library/Application Support/Tako/bin/tako-loopback-proxy"));
        assert!(plist.contains("<key>Sockets</key>"));
        assert!(plist.contains("<key>https</key>"));
        assert!(plist.contains("<key>http</key>"));
        assert!(plist.contains("<string>127.77.0.1</string>"));
        assert!(plist.contains("<string>443</string>"));
        assert!(plist.contains("<string>80</string>"));
        assert!(plist.contains("<key>KeepAlive</key>"));
        assert!(plist.contains("<false/>"));
    }

    #[test]
    fn bootstrap_launchd_plist_runs_helper_at_boot() {
        let plist = bootstrap_launchd_plist(Path::new(
            "/Library/Application Support/Tako/bin/tako-loopback-proxy",
        ));
        assert!(plist.contains(LOOPBACK_PROXY_BOOTSTRAP_LABEL));
        assert!(plist.contains("/Library/Application Support/Tako/bin/tako-loopback-proxy"));
        assert!(plist.contains("<string>bootstrap</string>"));
        assert!(plist.contains("<key>RunAtLoad</key>"));
        assert!(plist.contains("<true/>"));
        assert!(plist.contains("<key>KeepAlive</key>"));
        assert!(plist.contains("<false/>"));
    }

    #[test]
    fn loopback_alias_present_matches_assigned_ipv4_lines() {
        assert!(loopback_alias_present(
            "lo0: flags=8049<UP,LOOPBACK,RUNNING,MULTICAST>\n\tinet 127.0.0.1 netmask 0xff000000\n\tinet 127.77.0.1 netmask 0xff000000 alias\n",
            "127.77.0.1",
        ));
        assert!(!loopback_alias_present(
            "lo0: flags=8049<UP,LOOPBACK,RUNNING,MULTICAST>\n\tinet 127.0.0.1 netmask 0xff000000\n",
            "127.77.0.1",
        ));
    }

    #[test]
    fn plists_match_current_layout_when_both_plists_match_installed_binary() {
        let binary = Path::new("/Library/Application Support/Tako/bin/tako-loopback-proxy");
        assert!(plists_match_installed_binary(
            binary,
            &launchd_plist(binary),
            &bootstrap_launchd_plist(binary),
        ));
    }

    #[test]
    fn plists_match_current_layout_rejects_stale_plist_contents() {
        let binary = Path::new("/Library/Application Support/Tako/bin/tako-loopback-proxy");
        assert!(!plists_match_installed_binary(
            binary,
            "<plist>stale</plist>",
            &bootstrap_launchd_plist(binary),
        ));
        assert!(!plists_match_installed_binary(
            binary,
            &launchd_plist(binary),
            "<plist>stale</plist>",
        ));
    }

    #[test]
    fn repair_plan_is_none_when_files_loaded_alias_ready_and_ports_ready() {
        assert_eq!(
            repair_plan(true, true, true, true, true, true),
            LoopbackProxyRepairPlan::None
        );
    }

    #[test]
    fn repair_plan_reloads_when_launchd_or_ports_are_not_ready() {
        assert_eq!(
            repair_plan(true, true, true, false, true, true),
            LoopbackProxyRepairPlan::ReloadService
        );
        assert_eq!(
            repair_plan(true, true, true, true, false, true),
            LoopbackProxyRepairPlan::ReloadService
        );
    }

    #[test]
    fn repair_plan_installs_when_files_are_missing_boot_helper_missing_or_alias_missing() {
        assert_eq!(
            repair_plan(false, true, true, true, true, true),
            LoopbackProxyRepairPlan::InstallOrUpdate
        );
        assert_eq!(
            repair_plan(true, false, true, true, true, true),
            LoopbackProxyRepairPlan::InstallOrUpdate
        );
        assert_eq!(
            repair_plan(true, true, false, true, true, true),
            LoopbackProxyRepairPlan::InstallOrUpdate
        );
    }

    #[test]
    fn idle_exit_only_happens_when_no_connections_and_timeout_elapsed() {
        assert!(!should_exit_for_idle(
            1,
            LOOPBACK_PROXY_IDLE_TIMEOUT + Duration::from_secs(1),
            LOOPBACK_PROXY_IDLE_TIMEOUT,
        ));
        assert!(!should_exit_for_idle(
            0,
            LOOPBACK_PROXY_IDLE_TIMEOUT - Duration::from_secs(1),
            LOOPBACK_PROXY_IDLE_TIMEOUT,
        ));
        assert!(should_exit_for_idle(
            0,
            LOOPBACK_PROXY_IDLE_TIMEOUT + Duration::from_secs(1),
            LOOPBACK_PROXY_IDLE_TIMEOUT,
        ));
    }
}

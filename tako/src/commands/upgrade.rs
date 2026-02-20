use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::commands::server;
use crate::config::{ServerEntry, ServersToml};
use crate::output;
use crate::ssh::{CommandOutput, SshClient, SshConfig};

const DEFAULT_INSTALL_URL: &str = "https://tako.sh/install";
const INSTALL_URL_ENV: &str = "TAKO_INSTALL_URL";
const REMOTE_UPGRADE_HELPER: &str = "/usr/local/bin/tako-server-upgrade";

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UpgradeArgs {
    pub cli_only: bool,
    pub servers_only: bool,
    pub servers: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UpgradeScope {
    CliAndServers,
    CliOnly,
    ServersOnly,
}

impl UpgradeScope {
    fn upgrades_cli(self) -> bool {
        matches!(self, UpgradeScope::CliAndServers | UpgradeScope::CliOnly)
    }

    fn upgrades_servers(self) -> bool {
        matches!(
            self,
            UpgradeScope::CliAndServers | UpgradeScope::ServersOnly
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Downloader {
    Curl,
    Wget,
}

impl Downloader {
    fn binary(self) -> &'static str {
        match self {
            Downloader::Curl => "curl",
            Downloader::Wget => "wget",
        }
    }

    fn apply_args(self, cmd: &mut Command, install_url: &str) {
        match self {
            Downloader::Curl => {
                cmd.args(["-fsSL", install_url]);
            }
            Downloader::Wget => {
                cmd.args(["-qO-", install_url]);
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CliUpgradeMethod {
    Installer,
    Homebrew,
    Cargo,
}

#[derive(Debug, Clone)]
struct CliUpgradeDetectionContext {
    current_exe: PathBuf,
    home_dir: Option<PathBuf>,
    has_brew: bool,
    has_cargo: bool,
    brew_formula_installed: bool,
}

pub fn run(args: UpgradeArgs) -> Result<(), Box<dyn std::error::Error>> {
    let scope = resolve_scope(args.cli_only, args.servers_only);

    output::section("Upgrade");

    if scope.upgrades_cli() {
        run_cli_upgrade()?;
    }

    if scope.upgrades_servers() {
        let rt = tokio::runtime::Runtime::new()?;
        rt.block_on(run_servers_upgrade(&args.servers))?;
    }

    output::success("Upgrade complete");
    Ok(())
}

fn resolve_scope(cli_only: bool, servers_only: bool) -> UpgradeScope {
    if cli_only {
        return UpgradeScope::CliOnly;
    }
    if servers_only {
        return UpgradeScope::ServersOnly;
    }
    UpgradeScope::CliAndServers
}

fn run_cli_upgrade() -> Result<(), Box<dyn std::error::Error>> {
    let method = detect_cli_upgrade_method_runtime();
    match method {
        CliUpgradeMethod::Installer => run_cli_upgrade_with_installer(),
        CliUpgradeMethod::Homebrew => run_cli_upgrade_with_homebrew(),
        CliUpgradeMethod::Cargo => run_cli_upgrade_with_cargo(),
    }
}

fn run_cli_upgrade_with_installer() -> Result<(), Box<dyn std::error::Error>> {
    let install_url = resolve_install_url();
    output::step(&format_upgrade_start_message(
        &install_url,
        output::is_verbose(),
    ));

    let downloader = select_downloader(command_exists("curl"), command_exists("wget"))
        .map_err(|e| format!("{e}. Install curl or wget and retry."))?;

    output::with_spinner("Running installer...", || {
        run_installer(downloader, &install_url)
    })?
    .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;

    output::success("Local CLI upgraded");
    Ok(())
}

fn run_cli_upgrade_with_homebrew() -> Result<(), Box<dyn std::error::Error>> {
    output::step("Upgrading local tako CLI via Homebrew");
    output::with_spinner("Running brew upgrade tako...", || {
        run_local_upgrade_command("brew", &["upgrade", "tako"])
    })?
    .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;
    output::success("Local CLI upgraded");
    Ok(())
}

fn run_cli_upgrade_with_cargo() -> Result<(), Box<dyn std::error::Error>> {
    output::step("Upgrading local tako CLI via cargo");
    output::with_spinner("Running cargo install tako --locked...", || {
        run_local_upgrade_command("cargo", &["install", "tako", "--locked"])
    })?
    .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;
    output::success("Local CLI upgraded");
    Ok(())
}

fn run_local_upgrade_command(binary: &str, args: &[&str]) -> Result<(), String> {
    let status = Command::new(binary)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .map_err(|e| format!("failed to start {}: {}", binary, e))?;

    if status.success() {
        return Ok(());
    }

    Err(format!(
        "{} exited with a non-zero status while upgrading",
        binary
    ))
}

fn build_cli_upgrade_detection_context() -> CliUpgradeDetectionContext {
    let has_brew = command_exists("brew");
    let has_cargo = command_exists("cargo");

    CliUpgradeDetectionContext {
        current_exe: std::env::current_exe().unwrap_or_else(|_| PathBuf::from("tako")),
        home_dir: dirs::home_dir(),
        has_brew,
        has_cargo,
        brew_formula_installed: if has_brew {
            homebrew_formula_installed("tako")
        } else {
            false
        },
    }
}

fn detect_cli_upgrade_method_runtime() -> CliUpgradeMethod {
    let ctx = build_cli_upgrade_detection_context();
    detect_cli_upgrade_method(&ctx)
}

fn detect_cli_upgrade_method(ctx: &CliUpgradeDetectionContext) -> CliUpgradeMethod {
    if ctx.has_brew && is_homebrew_path(&ctx.current_exe) {
        return CliUpgradeMethod::Homebrew;
    }

    if ctx.has_cargo && is_cargo_install_path(&ctx.current_exe, ctx.home_dir.as_deref()) {
        return CliUpgradeMethod::Cargo;
    }

    if ctx.has_brew && ctx.brew_formula_installed {
        return CliUpgradeMethod::Homebrew;
    }

    CliUpgradeMethod::Installer
}

fn is_homebrew_path(path: &Path) -> bool {
    let value = path.to_string_lossy();
    value.starts_with("/opt/homebrew/")
        || value.starts_with("/usr/local/Homebrew/")
        || value.starts_with("/home/linuxbrew/.linuxbrew/")
        || value.contains("/Cellar/tako/")
}

fn is_cargo_install_path(path: &Path, home_dir: Option<&Path>) -> bool {
    if let Some(home_dir) = home_dir {
        let cargo_bin = home_dir.join(".cargo").join("bin");
        if path.starts_with(cargo_bin) {
            return true;
        }
    }

    path.to_string_lossy().contains("/.cargo/bin/")
}

fn homebrew_formula_installed(formula: &str) -> bool {
    let output = Command::new("brew")
        .args(["list", "--formula", "--versions", formula])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output();

    match output {
        Ok(output) if output.status.success() => {
            !String::from_utf8_lossy(&output.stdout).trim().is_empty()
        }
        _ => false,
    }
}

async fn run_servers_upgrade(
    requested_servers: &[String],
) -> Result<(), Box<dyn std::error::Error>> {
    let servers = ServersToml::load()?;
    let target_servers = resolve_server_targets(&servers, requested_servers)
        .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;

    if target_servers.is_empty() {
        output::warning("No servers configured. Skipping remote upgrades.");
        return Ok(());
    }

    output::step(&format!(
        "Upgrading remote servers on {} host{}",
        target_servers.len(),
        if target_servers.len() == 1 { "" } else { "s" }
    ));

    let mut failures: Vec<(String, String)> = Vec::new();

    for server_name in target_servers {
        output::section(&format!("Server {}", output::emphasized(&server_name)));

        let Some(entry) = servers.get(&server_name) else {
            failures.push((
                server_name,
                "Server was missing from config during upgrade execution.".to_string(),
            ));
            continue;
        };

        if let Err(err) = refresh_and_handoff_server(&server_name, entry).await {
            output::error(&err);
            failures.push((server_name, err));
        }
    }

    if failures.is_empty() {
        output::success("Remote server upgrades complete");
        return Ok(());
    }

    output::section("Summary");
    for (server_name, error) in &failures {
        output::error(&format!("{}: {}", output::emphasized(server_name), error));
    }

    Err(format!(
        "Remote server upgrade failed on {} host{}",
        failures.len(),
        if failures.len() == 1 { "" } else { "s" }
    )
    .into())
}

fn resolve_server_targets(
    servers: &ServersToml,
    requested_servers: &[String],
) -> Result<Vec<String>, String> {
    if requested_servers.is_empty() {
        let mut names: Vec<String> = servers.names().into_iter().map(str::to_string).collect();
        names.sort();
        return Ok(names);
    }

    let mut selected = Vec::new();

    for raw in requested_servers {
        let name = raw.trim();
        if name.is_empty() {
            continue;
        }

        if !servers.contains(name) {
            return Err(format!(
                "Server '{}' is not configured. Run `tako servers ls` to inspect configured servers.",
                name
            ));
        }

        if !selected.iter().any(|existing| existing == name) {
            selected.push(name.to_string());
        }
    }

    if selected.is_empty() {
        return Err("No server names were provided for remote upgrade.".to_string());
    }

    Ok(selected)
}

async fn refresh_and_handoff_server(server_name: &str, entry: &ServerEntry) -> Result<(), String> {
    refresh_remote_server_install(server_name, entry).await?;
    server::upgrade_server(server_name)
        .await
        .map_err(|e| format!("Upgrade handoff failed: {}", e))
}

async fn refresh_remote_server_install(
    server_name: &str,
    entry: &ServerEntry,
) -> Result<(), String> {
    let ssh_config = SshConfig::from_server(&entry.host, entry.port);
    let mut ssh = SshClient::new(ssh_config);

    output::with_spinner_async(
        format!(
            "Connecting to {} for installer refresh...",
            output::emphasized(server_name)
        ),
        ssh.connect(),
    )
    .await
    .map_err(|e| format!("Failed to connect: {}", e))?
    .map_err(|e| format!("Failed to connect: {}", e))?;

    let helper_available = output::with_spinner_async(
        "Checking remote upgrade helper...",
        remote_upgrade_helper_available(&ssh),
    )
    .await
    .map_err(|e| format!("Failed to check remote upgrade helper: {}", e))?
    .map_err(|e| format!("Failed to check remote upgrade helper: {}", e))?;

    if !helper_available {
        let _ = ssh.disconnect().await;
        return Err(format_missing_remote_helper_message());
    }

    let cmd = format!("sudo -n {}", shell_single_quote(REMOTE_UPGRADE_HELPER));
    let output = output::with_spinner_async("Refreshing remote server install...", ssh.exec(&cmd))
        .await
        .map_err(|e| format!("Failed to run remote installer helper: {}", e))?
        .map_err(|e| format!("Failed to run remote installer helper: {}", e))?;

    if !output.success() {
        let _ = ssh.disconnect().await;
        return Err(format_remote_helper_failure(&output));
    }

    let _ = ssh.disconnect().await;
    Ok(())
}

async fn remote_upgrade_helper_available(ssh: &SshClient) -> crate::ssh::SshResult<bool> {
    let cmd = format!(
        "test -x {} && echo yes || echo no",
        shell_single_quote(REMOTE_UPGRADE_HELPER)
    );
    let output = ssh.exec(&cmd).await?;
    Ok(output.stdout.trim() == "yes")
}

fn format_missing_remote_helper_message() -> String {
    format!(
        "Remote upgrade helper '{}' is missing. Run `curl -fsSL https://tako.sh/install-server | sh` as root on that host once, then retry.",
        REMOTE_UPGRADE_HELPER
    )
}

fn format_remote_helper_failure(output: &CommandOutput) -> String {
    let combined = output.combined();
    let message = first_non_empty_line(combined.trim()).unwrap_or("remote installer helper failed");
    let lower = message.to_ascii_lowercase();

    if lower.contains("command not found") || lower.contains("no such file") {
        return format_missing_remote_helper_message();
    }

    if lower.contains("password") || lower.contains("not allowed") || lower.contains("sorry") {
        return format!(
            "Remote user does not have permission to run '{}'. Re-run `curl -fsSL https://tako.sh/install-server | sh` as root to install/update sudoers policy.",
            REMOTE_UPGRADE_HELPER
        );
    }

    format!(
        "Remote installer helper failed with exit code {}: {}",
        output.exit_code, message
    )
}

fn first_non_empty_line(value: &str) -> Option<&str> {
    value.lines().map(str::trim).find(|line| !line.is_empty())
}

fn shell_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn format_upgrade_start_message(install_url: &str, verbose: bool) -> String {
    if verbose {
        return format!(
            "Installing latest tako CLI from {}",
            output::emphasized(install_url)
        );
    }
    "Installing latest tako CLI".to_string()
}

fn run_installer(downloader: Downloader, install_url: &str) -> Result<(), String> {
    let mut download = Command::new(downloader.binary());
    downloader.apply_args(&mut download, install_url);
    let mut download = download
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|e| format!("failed to start {}: {}", downloader.binary(), e))?;

    let Some(download_stdout) = download.stdout.take() else {
        return Err("failed to capture installer download output".to_string());
    };

    let mut install = Command::new("sh")
        .stdin(download_stdout)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|e| format!("failed to start installer shell: {}", e))?;

    let install_status = install
        .wait()
        .map_err(|e| format!("failed waiting for installer shell: {}", e))?;
    let download_status = download
        .wait()
        .map_err(|e| format!("failed waiting for {}: {}", downloader.binary(), e))?;

    if !download_status.success() {
        return Err(format!("failed to download installer from {}", install_url));
    }
    if !install_status.success() {
        return Err("installer script exited with a non-zero status".to_string());
    }

    Ok(())
}

fn resolve_install_url() -> String {
    let env_value = std::env::var(INSTALL_URL_ENV).ok();
    install_url_from_env(env_value.as_deref())
}

fn install_url_from_env(env_value: Option<&str>) -> String {
    match env_value.map(str::trim).filter(|v| !v.is_empty()) {
        Some(value) => value.to_string(),
        None => DEFAULT_INSTALL_URL.to_string(),
    }
}

fn select_downloader(has_curl: bool, has_wget: bool) -> Result<Downloader, String> {
    if has_curl {
        return Ok(Downloader::Curl);
    }
    if has_wget {
        return Ok(Downloader::Wget);
    }
    Err("no installer downloader available".to_string())
}

fn command_exists(name: &str) -> bool {
    Command::new(name)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ServersToml;

    #[test]
    fn install_url_from_env_defaults_when_missing() {
        assert_eq!(install_url_from_env(None), DEFAULT_INSTALL_URL);
    }

    #[test]
    fn install_url_from_env_defaults_when_blank() {
        assert_eq!(install_url_from_env(Some("   ")), DEFAULT_INSTALL_URL);
    }

    #[test]
    fn install_url_from_env_uses_override() {
        assert_eq!(
            install_url_from_env(Some("https://example.com/custom-install")),
            "https://example.com/custom-install"
        );
    }

    #[test]
    fn select_downloader_prefers_curl_when_available() {
        assert_eq!(select_downloader(true, true).unwrap(), Downloader::Curl);
        assert_eq!(select_downloader(true, false).unwrap(), Downloader::Curl);
    }

    #[test]
    fn select_downloader_falls_back_to_wget() {
        assert_eq!(select_downloader(false, true).unwrap(), Downloader::Wget);
    }

    #[test]
    fn select_downloader_errors_when_none_available() {
        let err = select_downloader(false, false).unwrap_err();
        assert!(err.contains("no installer downloader available"));
    }

    #[test]
    fn format_upgrade_start_message_is_compact_by_default() {
        let message = format_upgrade_start_message("https://tako.sh/install", false);
        assert_eq!(message, "Installing latest tako CLI");
    }

    #[test]
    fn format_upgrade_start_message_includes_url_in_verbose_mode() {
        let message = format_upgrade_start_message("https://tako.sh/install", true);
        assert!(message.contains("Installing latest tako CLI from"));
        assert!(message.contains("tako.sh/install"));
    }

    #[test]
    fn resolve_scope_defaults_to_cli_and_servers() {
        assert_eq!(resolve_scope(false, false), UpgradeScope::CliAndServers);
    }

    #[test]
    fn resolve_scope_honors_cli_only() {
        assert_eq!(resolve_scope(true, false), UpgradeScope::CliOnly);
    }

    #[test]
    fn resolve_scope_honors_servers_only() {
        assert_eq!(resolve_scope(false, true), UpgradeScope::ServersOnly);
    }

    #[test]
    fn detect_cli_upgrade_method_prefers_homebrew_path() {
        let ctx = CliUpgradeDetectionContext {
            current_exe: PathBuf::from("/opt/homebrew/bin/tako"),
            home_dir: Some(PathBuf::from("/Users/alice")),
            has_brew: true,
            has_cargo: true,
            brew_formula_installed: true,
        };
        assert_eq!(detect_cli_upgrade_method(&ctx), CliUpgradeMethod::Homebrew);
    }

    #[test]
    fn detect_cli_upgrade_method_prefers_cargo_path() {
        let ctx = CliUpgradeDetectionContext {
            current_exe: PathBuf::from("/Users/alice/.cargo/bin/tako"),
            home_dir: Some(PathBuf::from("/Users/alice")),
            has_brew: true,
            has_cargo: true,
            brew_formula_installed: true,
        };
        assert_eq!(detect_cli_upgrade_method(&ctx), CliUpgradeMethod::Cargo);
    }

    #[test]
    fn detect_cli_upgrade_method_uses_formula_presence_when_path_is_generic() {
        let ctx = CliUpgradeDetectionContext {
            current_exe: PathBuf::from("/usr/local/bin/tako"),
            home_dir: Some(PathBuf::from("/Users/alice")),
            has_brew: true,
            has_cargo: true,
            brew_formula_installed: true,
        };
        assert_eq!(detect_cli_upgrade_method(&ctx), CliUpgradeMethod::Homebrew);
    }

    #[test]
    fn detect_cli_upgrade_method_falls_back_to_installer() {
        let ctx = CliUpgradeDetectionContext {
            current_exe: PathBuf::from("/usr/local/bin/tako"),
            home_dir: Some(PathBuf::from("/Users/alice")),
            has_brew: false,
            has_cargo: false,
            brew_formula_installed: false,
        };
        assert_eq!(detect_cli_upgrade_method(&ctx), CliUpgradeMethod::Installer);
    }

    #[test]
    fn resolve_server_targets_defaults_to_sorted_all_servers() {
        let servers = sample_servers();
        let targets = resolve_server_targets(&servers, &[]).unwrap();
        assert_eq!(targets, vec!["prod".to_string(), "staging".to_string()]);
    }

    #[test]
    fn resolve_server_targets_preserves_requested_order_and_deduplicates() {
        let servers = sample_servers();
        let requested = vec![
            "staging".to_string(),
            "prod".to_string(),
            "staging".to_string(),
        ];
        let targets = resolve_server_targets(&servers, &requested).unwrap();
        assert_eq!(targets, vec!["staging".to_string(), "prod".to_string()]);
    }

    #[test]
    fn resolve_server_targets_rejects_unknown_server() {
        let servers = sample_servers();
        let err = resolve_server_targets(&servers, &["missing".to_string()]).unwrap_err();
        assert!(err.contains("is not configured"));
    }

    #[test]
    fn format_missing_remote_helper_message_mentions_install_script() {
        let message = format_missing_remote_helper_message();
        assert!(message.contains("tako-server-upgrade"));
        assert!(message.contains("install-server"));
    }

    #[test]
    fn format_remote_helper_failure_reports_permission_issue() {
        let output = CommandOutput {
            exit_code: 1,
            stdout: String::new(),
            stderr: "sudo: a password is required".to_string(),
        };
        let message = format_remote_helper_failure(&output);
        assert!(message.contains("does not have permission"));
    }

    fn sample_servers() -> ServersToml {
        ServersToml::parse(
            r#"
[[servers]]
name = "prod"
host = "1.2.3.4"

[[servers]]
name = "staging"
host = "5.6.7.8"
"#,
        )
        .expect("sample servers")
    }
}

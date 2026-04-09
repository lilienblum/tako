mod install;
mod task_tree;
mod version;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::config::{UpgradeChannel, resolve_upgrade_channel};
use crate::output;

use install::{
    detect_platform, download_and_install, download_and_install_pretty, resolve_install_dir,
};
#[cfg(test)]
use task_tree::should_use_upgrade_task_tree_for_mode;
use task_tree::{UpgradeTaskId, UpgradeTaskTreeController, should_use_upgrade_task_tree};
use version::{
    UpdateCheck, check_for_updates, check_for_updates_inner, current_version, fetch_canary_version,
    fetch_latest_stable_tag, tarball_url_for_tag,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CliUpgradeMethod {
    Installer,
    Homebrew,
}

#[derive(Debug, Clone)]
struct CliUpgradeDetectionContext {
    current_exe: PathBuf,
    has_brew: bool,
    brew_formula_installed: bool,
}

impl CliUpgradeMethod {
    #[allow(dead_code)]
    fn display_name(self) -> &'static str {
        match self {
            Self::Installer => "Installer",
            Self::Homebrew => "Homebrew",
        }
    }
}

impl UpgradeChannel {
    #[allow(dead_code)]
    fn display_name(self) -> &'static str {
        match self {
            Self::Stable => "Stable",
            Self::Canary => "Canary",
        }
    }
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

pub fn run(canary: bool, stable: bool) -> Result<(), Box<dyn std::error::Error>> {
    let channel = resolve_upgrade_channel(canary, stable)?;

    if channel == UpgradeChannel::Canary {
        output::muted("You're on canary channel");
        let ver = current_version();
        output::info(&format!("Current version: {}", output::strong(&ver)));
        tracing::info!("Current version: {ver}");
    }

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(run_upgrade(channel))
}

// ---------------------------------------------------------------------------
// Upgrade orchestration
// ---------------------------------------------------------------------------

async fn run_upgrade(channel: UpgradeChannel) -> Result<(), Box<dyn std::error::Error>> {
    let method = if channel == UpgradeChannel::Canary {
        CliUpgradeMethod::Installer
    } else {
        detect_cli_upgrade_method_runtime()
    };

    if should_use_upgrade_task_tree() {
        return run_upgrade_with_task_tree(channel, method).await;
    }

    match method {
        CliUpgradeMethod::Installer => run_installer_upgrade(channel).await,
        CliUpgradeMethod::Homebrew => run_brew_upgrade(),
    }
}

async fn run_upgrade_with_task_tree(
    channel: UpgradeChannel,
    method: CliUpgradeMethod,
) -> Result<(), Box<dyn std::error::Error>> {
    let controller = UpgradeTaskTreeController::new(channel, method);

    match method {
        CliUpgradeMethod::Installer => run_installer_upgrade_pretty(channel, &controller).await,
        CliUpgradeMethod::Homebrew => {
            run_local_upgrade_pretty(&controller, UpgradeTaskId::UpgradeViaHomebrew, || {
                run_local_upgrade_command("brew", &["upgrade", "tako"])
            })
        }
    }
}

// ---------------------------------------------------------------------------
// Installer upgrade (version check + progress bar download)
// ---------------------------------------------------------------------------

async fn run_installer_upgrade_pretty(
    channel: UpgradeChannel,
    controller: &UpgradeTaskTreeController,
) -> Result<(), Box<dyn std::error::Error>> {
    let (os, arch) = detect_platform()?;
    let install_dir = resolve_install_dir();

    if channel == UpgradeChannel::Canary {
        return run_canary_upgrade_pretty(os, arch, &install_dir, controller).await;
    }

    controller.mark_running(UpgradeTaskId::CheckForUpdates);
    match check_for_updates_inner(channel).await {
        Ok(UpdateCheck::AlreadyCurrent) => {
            controller.succeed(
                UpgradeTaskId::CheckForUpdates,
                Some("Already current".to_string()),
            );
            controller.skip_pending("Skipped");
            return Ok(());
        }
        Ok(UpdateCheck::Available { tag, version }) => {
            controller.succeed(UpgradeTaskId::CheckForUpdates, Some(version.clone()));
            let url = tarball_url_for_tag(&tag, os, arch);
            download_and_install_pretty(&url, &install_dir, controller, Some(version)).await?;
            return Ok(());
        }
        Err(error) => {
            tracing::warn!("Could not check for updates: {error}");
            controller.warn(
                UpgradeTaskId::CheckForUpdates,
                Some("Fallback to latest release".to_string()),
            );
        }
    }

    let tag = fetch_latest_stable_tag()
        .await
        .map_err(|e| format!("Failed to resolve latest release: {e}"))?;
    let url = tarball_url_for_tag(&tag, os, arch);
    download_and_install_pretty(&url, &install_dir, controller, None).await
}

async fn run_canary_upgrade_pretty(
    os: &str,
    arch: &str,
    install_dir: &Path,
    controller: &UpgradeTaskTreeController,
) -> Result<(), Box<dyn std::error::Error>> {
    let local_version = current_version();
    controller.mark_running(UpgradeTaskId::CheckForUpdates);

    match fetch_canary_version().await {
        Ok(remote_version) => {
            if remote_version == local_version {
                controller.succeed(
                    UpgradeTaskId::CheckForUpdates,
                    Some("Already current".to_string()),
                );
                controller.skip_pending("Skipped");
                return Ok(());
            }

            controller.succeed(UpgradeTaskId::CheckForUpdates, Some(remote_version.clone()));
            let url = tarball_url_for_tag("canary-latest", os, arch);
            download_and_install_pretty(&url, install_dir, controller, Some(remote_version)).await
        }
        Err(error) => {
            controller.fail(UpgradeTaskId::CheckForUpdates, Some(error.to_string()));
            Err(error)
        }
    }
}

fn run_local_upgrade_pretty<F>(
    controller: &UpgradeTaskTreeController,
    task_id: UpgradeTaskId,
    work: F,
) -> Result<(), Box<dyn std::error::Error>>
where
    F: FnOnce() -> Result<(), String>,
{
    controller.mark_running(task_id);
    match work() {
        Ok(()) => {
            controller.succeed(task_id, None);
            Ok(())
        }
        Err(error) => {
            controller.fail(task_id, Some(error.clone()));
            Err(error.into())
        }
    }
}

async fn run_installer_upgrade(channel: UpgradeChannel) -> Result<(), Box<dyn std::error::Error>> {
    let (os, arch) = detect_platform()?;
    let install_dir = resolve_install_dir();

    if channel == UpgradeChannel::Canary {
        return run_canary_upgrade(os, arch, &install_dir).await;
    }

    let check = check_for_updates(channel).await;

    match check {
        Ok(UpdateCheck::AlreadyCurrent) => {
            output::success("Already on the latest version");
            return Ok(());
        }
        Ok(UpdateCheck::Available { tag, version }) => {
            let url = tarball_url_for_tag(&tag, os, arch);
            download_and_install(&url, &install_dir).await?;

            output::success(&format!("Upgraded to {}", output::strong(&version)));
            return Ok(());
        }
        Err(_) => {
            output::warning("Could not check for updates");
        }
    }

    // Version check failed: download latest anyway
    let tag = fetch_latest_stable_tag()
        .await
        .map_err(|e| format!("Failed to resolve latest release: {e}"))?;

    let url = tarball_url_for_tag(&tag, os, arch);
    download_and_install(&url, &install_dir).await?;

    output::success("Upgraded");
    Ok(())
}

/// Canary upgrade: resolve the canary-latest tag to its commit SHA, compare
/// with the currently running version, and only download when they differ.
async fn run_canary_upgrade(
    os: &str,
    arch: &str,
    install_dir: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let remote_version =
        output::with_spinner_async_simple("Checking for updates", fetch_canary_version()).await?;
    let local_version = current_version();

    if remote_version == local_version {
        tracing::info!("Already on the latest canary build");
        output::success("Already on the latest canary build");
        return Ok(());
    }

    let url = tarball_url_for_tag("canary-latest", os, arch);
    download_and_install(&url, install_dir).await?;

    tracing::info!("Upgraded to {remote_version}");
    output::success(&format!("Upgraded to {}", output::strong(&remote_version)));
    Ok(())
}

// ---------------------------------------------------------------------------
// Homebrew upgrades
// ---------------------------------------------------------------------------

fn run_brew_upgrade() -> Result<(), Box<dyn std::error::Error>> {
    output::with_spinner("Upgrading via Homebrew", "Upgraded via Homebrew", || {
        run_local_upgrade_command("brew", &["upgrade", "tako"])
    })
    .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;
    Ok(())
}

fn run_local_upgrade_command(binary: &str, args: &[&str]) -> Result<(), String> {
    let result = Command::new(binary)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("failed to start {}: {}", binary, e))?;

    if result.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&result.stderr);
    let detail = stderr.trim();
    if detail.is_empty() {
        Err(format!("{binary} exited with a non-zero status"))
    } else {
        Err(format!("{binary}: {detail}"))
    }
}

// ---------------------------------------------------------------------------
// Download with progress bar + install
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Upgrade method detection
// ---------------------------------------------------------------------------

fn build_cli_upgrade_detection_context() -> CliUpgradeDetectionContext {
    let has_brew = command_exists("brew");

    CliUpgradeDetectionContext {
        current_exe: std::env::current_exe().unwrap_or_else(|_| PathBuf::from("tako")),
        has_brew,
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
    use crate::ui::TaskState;

    #[test]
    fn tarball_url_for_tag_constructs_github_url() {
        let url = tarball_url_for_tag("tako-v0.1.0", "darwin", "aarch64");
        assert_eq!(
            url,
            "https://github.com/lilienblum/tako/releases/download/tako-v0.1.0/tako-darwin-aarch64.tar.gz"
        );
    }

    #[test]
    fn tarball_url_for_tag_canary() {
        let url = tarball_url_for_tag("canary-latest", "linux", "x86_64");
        assert_eq!(
            url,
            "https://github.com/lilienblum/tako/releases/download/canary-latest/tako-linux-x86_64.tar.gz"
        );
    }

    #[test]
    fn detect_cli_upgrade_method_prefers_homebrew_path() {
        let ctx = CliUpgradeDetectionContext {
            current_exe: PathBuf::from("/opt/homebrew/bin/tako"),

            has_brew: true,
            brew_formula_installed: true,
        };
        assert_eq!(detect_cli_upgrade_method(&ctx), CliUpgradeMethod::Homebrew);
    }

    #[test]
    fn detect_cli_upgrade_method_uses_formula_presence_when_path_is_generic() {
        let ctx = CliUpgradeDetectionContext {
            current_exe: PathBuf::from("/usr/local/bin/tako"),

            has_brew: true,
            brew_formula_installed: true,
        };
        assert_eq!(detect_cli_upgrade_method(&ctx), CliUpgradeMethod::Homebrew);
    }

    #[test]
    fn detect_cli_upgrade_method_falls_back_to_installer() {
        let ctx = CliUpgradeDetectionContext {
            current_exe: PathBuf::from("/usr/local/bin/tako"),

            has_brew: false,
            brew_formula_installed: false,
        };
        assert_eq!(detect_cli_upgrade_method(&ctx), CliUpgradeMethod::Installer);
    }

    #[test]
    fn upgrade_task_tree_requires_pretty_interactive_mode() {
        assert!(should_use_upgrade_task_tree_for_mode(true, true));
        assert!(!should_use_upgrade_task_tree_for_mode(true, false));
        assert!(!should_use_upgrade_task_tree_for_mode(false, true));
    }

    #[test]
    fn stable_installer_task_tree_happy_path_marks_each_step_complete() {
        let controller =
            UpgradeTaskTreeController::new(UpgradeChannel::Stable, CliUpgradeMethod::Installer);

        controller.mark_running(UpgradeTaskId::CheckForUpdates);
        controller.succeed(UpgradeTaskId::CheckForUpdates, Some("1.2.3".to_string()));
        controller.mark_running(UpgradeTaskId::DownloadArchive);
        controller.succeed(UpgradeTaskId::DownloadArchive, None);
        controller.mark_running(UpgradeTaskId::VerifySha256);
        controller.succeed(UpgradeTaskId::VerifySha256, None);
        controller.mark_running(UpgradeTaskId::ExtractArchive);
        controller.succeed(UpgradeTaskId::ExtractArchive, None);
        controller.mark_running(UpgradeTaskId::InstallBinaries);
        controller.succeed(UpgradeTaskId::InstallBinaries, Some("1.2.3".to_string()));

        let snapshot = controller.snapshot();
        assert_eq!(snapshot.tasks.len(), 5);
        assert!(
            snapshot
                .tasks
                .iter()
                .all(|task| matches!(task.state, TaskState::Succeeded { .. }))
        );
        assert_eq!(
            snapshot
                .tasks
                .iter()
                .find(|task| task.id == UpgradeTaskId::InstallBinaries.key())
                .and_then(|task| task.detail.clone())
                .as_deref(),
            Some("1.2.3")
        );
    }

    #[test]
    fn stable_installer_task_tree_fallback_marks_update_check_warning() {
        let controller =
            UpgradeTaskTreeController::new(UpgradeChannel::Stable, CliUpgradeMethod::Installer);

        controller.warn(
            UpgradeTaskId::CheckForUpdates,
            Some("Fallback to latest release".to_string()),
        );
        controller.mark_running(UpgradeTaskId::DownloadArchive);
        controller.succeed(UpgradeTaskId::DownloadArchive, None);
        controller.mark_running(UpgradeTaskId::VerifySha256);
        controller.succeed(UpgradeTaskId::VerifySha256, None);
        controller.mark_running(UpgradeTaskId::ExtractArchive);
        controller.succeed(UpgradeTaskId::ExtractArchive, None);
        controller.mark_running(UpgradeTaskId::InstallBinaries);
        controller.succeed(UpgradeTaskId::InstallBinaries, None);

        let snapshot = controller.snapshot();
        let check = snapshot
            .tasks
            .iter()
            .find(|task| task.id == UpgradeTaskId::CheckForUpdates.key())
            .unwrap();
        assert!(matches!(check.state, TaskState::Cancelled { .. }));
        assert_eq!(check.detail.as_deref(), Some("Fallback to latest release"));
        assert!(
            snapshot.tasks[1..]
                .iter()
                .all(|task| matches!(task.state, TaskState::Succeeded { .. }))
        );
    }

    #[test]
    fn canary_task_tree_already_current_skips_remaining_steps() {
        let controller =
            UpgradeTaskTreeController::new(UpgradeChannel::Canary, CliUpgradeMethod::Installer);

        controller.succeed(
            UpgradeTaskId::CheckForUpdates,
            Some("Already current".to_string()),
        );
        controller.skip_pending("Skipped");

        let snapshot = controller.snapshot();
        let check = snapshot
            .tasks
            .iter()
            .find(|task| task.id == UpgradeTaskId::CheckForUpdates.key())
            .unwrap();
        assert!(matches!(check.state, TaskState::Succeeded { .. }));
        assert!(
            snapshot.tasks[1..]
                .iter()
                .all(|task| matches!(task.state, TaskState::Cancelled { .. }))
        );
        assert!(
            snapshot.tasks[1..]
                .iter()
                .all(|task| task.detail.as_deref() == Some("Skipped"))
        );
    }

    #[test]
    fn canary_task_tree_upgrade_path_keeps_full_sequence_visible() {
        let controller =
            UpgradeTaskTreeController::new(UpgradeChannel::Canary, CliUpgradeMethod::Installer);

        controller.succeed(
            UpgradeTaskId::CheckForUpdates,
            Some("canary-abcdef0".to_string()),
        );
        controller.mark_running(UpgradeTaskId::DownloadArchive);
        controller.succeed(UpgradeTaskId::DownloadArchive, None);
        controller.mark_running(UpgradeTaskId::VerifySha256);
        controller.succeed(UpgradeTaskId::VerifySha256, None);
        controller.mark_running(UpgradeTaskId::ExtractArchive);
        controller.succeed(UpgradeTaskId::ExtractArchive, None);
        controller.mark_running(UpgradeTaskId::InstallBinaries);
        controller.succeed(
            UpgradeTaskId::InstallBinaries,
            Some("canary-abcdef0".to_string()),
        );

        let snapshot = controller.snapshot();
        assert_eq!(snapshot.tasks.len(), 5);
        assert!(
            snapshot
                .tasks
                .iter()
                .all(|task| !matches!(task.state, TaskState::Pending))
        );
    }

    #[test]
    fn homebrew_task_tree_uses_single_method_task() {
        let homebrew =
            UpgradeTaskTreeController::new(UpgradeChannel::Stable, CliUpgradeMethod::Homebrew)
                .snapshot();
        assert_eq!(homebrew.tasks.len(), 1);
        assert_eq!(homebrew.tasks[0].label, "Upgrade via Homebrew");
    }
}

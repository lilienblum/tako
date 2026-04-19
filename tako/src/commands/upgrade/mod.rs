mod install;
mod task_tree;
mod version;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::output;

use install::{
    detect_platform, download_and_install, download_and_install_pretty, resolve_install_dir,
};
#[cfg(test)]
use task_tree::should_use_upgrade_task_tree_for_mode;
use task_tree::{UpgradeTaskId, UpgradeTaskTreeController, should_use_upgrade_task_tree};
use version::{
    UpdateCheck, check_for_updates, check_for_updates_inner, current_version, tarball_url,
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

pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    let ver = current_version();
    output::info(&format!("Current version: {}", output::strong(&ver)));
    tracing::info!("Current version: {ver}");

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(run_upgrade())
}

async fn run_upgrade() -> Result<(), Box<dyn std::error::Error>> {
    let method = detect_cli_upgrade_method_runtime();

    if should_use_upgrade_task_tree() {
        return run_upgrade_with_task_tree(method).await;
    }

    match method {
        CliUpgradeMethod::Installer => run_installer_upgrade().await,
        CliUpgradeMethod::Homebrew => run_brew_upgrade(),
    }
}

async fn run_upgrade_with_task_tree(
    method: CliUpgradeMethod,
) -> Result<(), Box<dyn std::error::Error>> {
    let controller = UpgradeTaskTreeController::new(method);

    match method {
        CliUpgradeMethod::Installer => run_installer_upgrade_pretty(&controller).await,
        CliUpgradeMethod::Homebrew => {
            run_local_upgrade_pretty(&controller, UpgradeTaskId::UpgradeViaHomebrew, || {
                run_local_upgrade_command("brew", &["upgrade", "tako"])
            })
        }
    }
}

async fn run_installer_upgrade_pretty(
    controller: &UpgradeTaskTreeController,
) -> Result<(), Box<dyn std::error::Error>> {
    let (os, arch) = detect_platform()?;
    let install_dir = resolve_install_dir();

    controller.mark_running(UpgradeTaskId::CheckForUpdates);
    match check_for_updates_inner().await {
        Ok(UpdateCheck::AlreadyCurrent) => {
            controller.succeed(
                UpgradeTaskId::CheckForUpdates,
                Some("Already current".to_string()),
            );
            controller.skip_pending("Skipped");
            Ok(())
        }
        Ok(UpdateCheck::Available { version }) => {
            controller.succeed(UpgradeTaskId::CheckForUpdates, Some(version.clone()));
            let url = tarball_url(os, arch);
            download_and_install_pretty(&url, &install_dir, controller, Some(version)).await
        }
        Err(error) => {
            controller.fail(UpgradeTaskId::CheckForUpdates, Some(error.clone()));
            Err(error.into())
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

async fn run_installer_upgrade() -> Result<(), Box<dyn std::error::Error>> {
    let (os, arch) = detect_platform()?;
    let install_dir = resolve_install_dir();

    let check = check_for_updates().await;

    match check {
        Ok(UpdateCheck::AlreadyCurrent) => {
            output::success("Already on the latest version");
            Ok(())
        }
        Ok(UpdateCheck::Available { version }) => {
            let url = tarball_url(os, arch);
            download_and_install(&url, &install_dir).await?;
            output::success(&format!("Upgraded to {}", output::strong(&version)));
            Ok(())
        }
        Err(error) => Err(error.into()),
    }
}

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
    fn tarball_url_constructs_github_url() {
        let url = tarball_url("darwin", "aarch64");
        assert_eq!(
            url,
            "https://github.com/lilienblum/tako/releases/download/latest/tako-darwin-aarch64.tar.gz"
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
    fn installer_task_tree_happy_path_marks_each_step_complete() {
        let controller = UpgradeTaskTreeController::new(CliUpgradeMethod::Installer);

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
    fn installer_task_tree_already_current_skips_remaining_steps() {
        let controller = UpgradeTaskTreeController::new(CliUpgradeMethod::Installer);

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
                .all(|task| matches!(task.state, TaskState::Skipped { .. }))
        );
        assert!(
            snapshot.tasks[1..]
                .iter()
                .all(|task| task.detail.as_deref() == Some("Skipped"))
        );
    }

    #[test]
    fn homebrew_task_tree_uses_single_method_task() {
        let homebrew = UpgradeTaskTreeController::new(CliUpgradeMethod::Homebrew).snapshot();
        assert_eq!(homebrew.tasks.len(), 1);
        assert_eq!(homebrew.tasks[0].label, "Upgrade via Homebrew");
    }
}

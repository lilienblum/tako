use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

use sha2::{Digest, Sha256};

use crate::config::{UpgradeChannel, resolve_upgrade_channel};
use crate::output;
use crate::ui::{TaskItemState, TaskState, TaskTreeSession, TreeNode};

const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");
const CANARY_SHA: Option<&str> = option_env!("TAKO_CANARY_SHA");

const REPO_OWNER: &str = "lilienblum";
const REPO_NAME: &str = "tako";
const TAGS_API: &str = "https://api.github.com/repos/lilienblum/tako/tags?per_page=100";
const TAG_PREFIX: &str = "tako-v";

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

/// Result of checking for updates.
enum UpdateCheck {
    /// Current version matches latest.
    AlreadyCurrent,
    /// A newer version is available. Contains the tag name for download and a display version string.
    Available { tag: String, version: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UpgradeTaskId {
    CheckForUpdates,
    DownloadArchive,
    VerifySha256,
    ExtractArchive,
    InstallBinaries,
    UpgradeViaHomebrew,
}

#[derive(Debug, Clone)]
struct UpgradeTaskTreeState {
    tasks: Vec<TaskItemState>,
}

#[derive(Clone)]
struct UpgradeTaskTreeController {
    state: Arc<Mutex<UpgradeTaskTreeState>>,
    session: Option<TaskTreeSession>,
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

impl UpgradeTaskId {
    fn key(self) -> &'static str {
        match self {
            Self::CheckForUpdates => "check-for-updates",
            Self::DownloadArchive => "download-archive",
            Self::VerifySha256 => "verify-sha256",
            Self::ExtractArchive => "extract-archive",
            Self::InstallBinaries => "install-binaries",
            Self::UpgradeViaHomebrew => "upgrade-via-homebrew",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::CheckForUpdates => "Check for updates",
            Self::DownloadArchive => "Download archive",
            Self::VerifySha256 => "Verify SHA256",
            Self::ExtractArchive => "Extract archive",
            Self::InstallBinaries => "Install binaries",
            Self::UpgradeViaHomebrew => "Upgrade via Homebrew",
        }
    }
}

impl UpgradeTaskTreeController {
    fn new(channel: UpgradeChannel, method: CliUpgradeMethod) -> Self {
        let state = UpgradeTaskTreeState {
            tasks: build_upgrade_tasks(channel, method),
        };
        let tree = build_upgrade_tree(&state);
        let session = should_use_upgrade_task_tree().then(|| TaskTreeSession::new(tree));
        Self {
            state: Arc::new(Mutex::new(state)),
            session,
        }
    }

    fn mark_running(&self, id: UpgradeTaskId) {
        self.update(id, |task| {
            task.state = TaskState::Running {
                started_at: std::time::Instant::now(),
            };
            task.detail = None;
        });
    }

    fn succeed(&self, id: UpgradeTaskId, detail: Option<String>) {
        self.complete(id, detail, CompletionKind::Succeeded);
    }

    fn warn(&self, id: UpgradeTaskId, detail: Option<String>) {
        self.complete(id, detail, CompletionKind::Cancelled);
    }

    fn fail(&self, id: UpgradeTaskId, detail: Option<String>) {
        self.complete(id, detail, CompletionKind::Failed);
    }

    fn skip_pending(&self, reason: &str) {
        let mut guard = self.state.lock().unwrap();
        for task in &mut guard.tasks {
            if matches!(task.state, TaskState::Pending) {
                task.state = TaskState::Cancelled { elapsed: None };
                task.detail = Some(reason.to_string());
            }
        }
        self.refresh_locked(&guard);
    }

    #[cfg(test)]
    fn snapshot(&self) -> UpgradeTaskTreeState {
        self.state.lock().unwrap().clone()
    }

    fn update<F>(&self, id: UpgradeTaskId, update: F)
    where
        F: FnOnce(&mut TaskItemState),
    {
        let mut guard = self.state.lock().unwrap();
        let task = guard
            .tasks
            .iter_mut()
            .find_map(|task| task.find_mut(id.key()))
            .unwrap_or_else(|| panic!("missing upgrade task {}", id.key()));
        update(task);
        self.refresh_locked(&guard);
    }

    fn complete(&self, id: UpgradeTaskId, detail: Option<String>, kind: CompletionKind) {
        self.update(id, |task| {
            let elapsed = match task.state {
                TaskState::Running { started_at } => Some(started_at.elapsed()),
                _ => None,
            };
            task.state = match kind {
                CompletionKind::Succeeded => TaskState::Succeeded { elapsed },
                CompletionKind::Cancelled => TaskState::Cancelled { elapsed },
                CompletionKind::Failed => TaskState::Failed { elapsed },
            };
            task.detail = detail;
        });
    }

    fn refresh_locked(&self, state: &UpgradeTaskTreeState) {
        if let Some(session) = &self.session {
            session.set_tree(build_upgrade_tree(state));
        }
    }
}

#[derive(Clone, Copy)]
enum CompletionKind {
    Succeeded,
    Cancelled,
    Failed,
}

fn build_upgrade_tasks(_channel: UpgradeChannel, method: CliUpgradeMethod) -> Vec<TaskItemState> {
    match method {
        CliUpgradeMethod::Installer => {
            vec![
                TaskItemState::pending(
                    UpgradeTaskId::CheckForUpdates.key(),
                    UpgradeTaskId::CheckForUpdates.label(),
                ),
                TaskItemState::pending(
                    UpgradeTaskId::DownloadArchive.key(),
                    UpgradeTaskId::DownloadArchive.label(),
                ),
                TaskItemState::pending(
                    UpgradeTaskId::VerifySha256.key(),
                    UpgradeTaskId::VerifySha256.label(),
                ),
                TaskItemState::pending(
                    UpgradeTaskId::ExtractArchive.key(),
                    UpgradeTaskId::ExtractArchive.label(),
                ),
                TaskItemState::pending(
                    UpgradeTaskId::InstallBinaries.key(),
                    UpgradeTaskId::InstallBinaries.label(),
                ),
            ]
        }
        CliUpgradeMethod::Homebrew => vec![TaskItemState::pending(
            UpgradeTaskId::UpgradeViaHomebrew.key(),
            UpgradeTaskId::UpgradeViaHomebrew.label(),
        )],
    }
}

fn build_upgrade_tree(state: &UpgradeTaskTreeState) -> Vec<TreeNode> {
    // Render tasks as a flat list (no summary rows in the live tree —
    // those are printed separately before the session starts).
    state
        .tasks
        .iter()
        .map(|task| TreeNode::Task(task.clone()))
        .collect()
}

fn should_use_upgrade_task_tree() -> bool {
    should_use_upgrade_task_tree_for_mode(output::is_pretty(), output::is_interactive())
}

fn should_use_upgrade_task_tree_for_mode(pretty: bool, interactive: bool) -> bool {
    pretty && interactive
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

fn current_version() -> String {
    match CANARY_SHA {
        Some(sha) if !sha.trim().is_empty() => {
            let short = &sha.trim()[..sha.trim().len().min(7)];
            format!("canary-{short}")
        }
        _ => CURRENT_VERSION.to_string(),
    }
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

async fn download_and_install_pretty(
    url: &str,
    install_dir: &Path,
    controller: &UpgradeTaskTreeController,
    install_detail: Option<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    let tmp_base = std::env::temp_dir();
    let tmp_dir = tmp_base.join(format!("tako-upgrade-{}", std::process::id()));
    std::fs::create_dir_all(&tmp_dir)?;

    let result =
        download_and_install_pretty_inner(url, install_dir, &tmp_dir, controller, install_detail)
            .await;

    let _ = std::fs::remove_dir_all(&tmp_dir);
    result
}

async fn download_and_install_pretty_inner(
    url: &str,
    install_dir: &Path,
    tmp_dir: &Path,
    controller: &UpgradeTaskTreeController,
    install_detail: Option<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    let archive_path = tmp_dir.join("tako.tar.gz");

    run_upgrade_task_step(controller, UpgradeTaskId::DownloadArchive, None, async {
        download_without_progress(url, &archive_path)
            .await
            .map_err(|error| error.to_string())
    })
    .await?;

    let sha_url = format!("{url}.sha256");
    run_upgrade_task_step(controller, UpgradeTaskId::VerifySha256, None, async {
        let expected = fetch_sha256(&sha_url)
            .await
            .map_err(|e| format!("SHA256 checksum unavailable for {sha_url}: {e}"))?;
        verify_sha256(&archive_path, &expected)?;
        Ok::<(), String>(())
    })
    .await?;

    let extract_dir = tmp_dir.join("extract");
    run_upgrade_task_step(controller, UpgradeTaskId::ExtractArchive, None, async {
        std::fs::create_dir_all(&extract_dir)
            .map_err(|e| format!("failed to create extract directory: {e}"))?;
        extract_tarball(&archive_path, &extract_dir)?;
        Ok::<(), String>(())
    })
    .await?;

    run_upgrade_task_step(
        controller,
        UpgradeTaskId::InstallBinaries,
        install_detail,
        async {
            let new_tako = find_binary(&extract_dir, "tako")
                .ok_or_else(|| "archive did not contain a tako binary".to_string())?;
            let new_dev_server = find_binary(&extract_dir, "tako-dev-server")
                .ok_or_else(|| "archive did not contain a tako-dev-server binary".to_string())?;
            let new_loopback_proxy =
                find_binary(&extract_dir, "tako-loopback-proxy").ok_or_else(|| {
                    "archive did not contain a tako-loopback-proxy binary".to_string()
                })?;
            std::fs::create_dir_all(install_dir)
                .map_err(|e| format!("failed to create install dir: {e}"))?;
            install_binary(&new_tako, install_dir, "tako")?;
            install_binary(&new_dev_server, install_dir, "tako-dev-server")?;
            install_binary(&new_loopback_proxy, install_dir, "tako-loopback-proxy")?;
            Ok::<(), String>(())
        },
    )
    .await
}

async fn run_upgrade_task_step<T, Fut>(
    controller: &UpgradeTaskTreeController,
    task_id: UpgradeTaskId,
    success_detail: Option<String>,
    work: Fut,
) -> Result<T, Box<dyn std::error::Error>>
where
    Fut: std::future::Future<Output = Result<T, String>>,
{
    controller.mark_running(task_id);
    match work.await {
        Ok(value) => {
            controller.succeed(task_id, success_detail);
            Ok(value)
        }
        Err(error) => {
            controller.fail(task_id, Some(error.to_string()));
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
    // Resolve canary-latest tag → commit SHA → version string
    let remote_version =
        output::with_spinner_async_simple("Checking for updates", fetch_canary_version()).await?;
    let local_version = current_version();

    if remote_version == local_version {
        tracing::info!("Already on the latest canary build");
        output::success("Already on the latest canary build");
        return Ok(());
    }

    // Download and extract to temp dir
    let url = tarball_url_for_tag("canary-latest", os, arch);
    let tmp_base = std::env::temp_dir();
    let tmp_dir = tmp_base.join(format!("tako-upgrade-{}", std::process::id()));
    std::fs::create_dir_all(&tmp_dir)?;

    let result = async {
        let archive_path = tmp_dir.join("tako.tar.gz");
        download_with_progress(&url, &archive_path).await?;

        // Verify SHA256 — fail if checksum is unavailable
        let sha_url = format!("{url}.sha256");
        let expected = fetch_sha256(&sha_url)
            .await
            .map_err(|e| format!("SHA256 checksum unavailable for {sha_url}: {e}"))?;
        verify_sha256(&archive_path, &expected)?;

        let extract_dir = tmp_dir.join("extract");
        std::fs::create_dir_all(&extract_dir)?;
        extract_tarball(&archive_path, &extract_dir)?;

        // Install
        let new_tako =
            find_binary(&extract_dir, "tako").ok_or("archive did not contain a tako binary")?;
        let new_dev_server = find_binary(&extract_dir, "tako-dev-server")
            .ok_or("archive did not contain a tako-dev-server binary")?;
        let new_loopback_proxy = find_binary(&extract_dir, "tako-loopback-proxy")
            .ok_or("archive did not contain a tako-loopback-proxy binary")?;
        std::fs::create_dir_all(install_dir)?;
        install_binary(&new_tako, install_dir, "tako")?;
        install_binary(&new_dev_server, install_dir, "tako-dev-server")?;
        install_binary(&new_loopback_proxy, install_dir, "tako-loopback-proxy")?;

        tracing::info!("Upgraded to {remote_version}");
        output::success(&format!("Upgraded to {}", output::strong(&remote_version)));
        Ok(())
    }
    .await;

    let _ = std::fs::remove_dir_all(&tmp_dir);
    result
}

/// Fetch the commit SHA that the canary-latest tag points to and format as a version string.
async fn fetch_canary_version() -> Result<String, Box<dyn std::error::Error>> {
    let url =
        format!("https://api.github.com/repos/{REPO_OWNER}/{REPO_NAME}/git/ref/tags/canary-latest");
    let client = reqwest::Client::new();
    let resp = client
        .get(&url)
        .header("User-Agent", "tako-cli")
        .send()
        .await?
        .error_for_status()
        .map_err(|e| format!("failed to resolve canary-latest tag: {e}"))?;
    let body: serde_json::Value = resp.json().await?;
    let sha = body["object"]["sha"]
        .as_str()
        .ok_or("canary-latest tag response missing object.sha")?;
    let short = &sha[..sha.len().min(7)];
    Ok(format!("canary-{short}"))
}

fn tarball_url_for_tag(tag: &str, os: &str, arch: &str) -> String {
    // Support TAKO_DOWNLOAD_BASE_URL override (used by canary builds and testing)
    if let Ok(base) = std::env::var("TAKO_DOWNLOAD_BASE_URL") {
        let base = base.trim().trim_end_matches('/');
        if !base.is_empty() {
            if !base.starts_with("https://") {
                crate::output::warning(&format!(
                    "TAKO_DOWNLOAD_BASE_URL uses non-HTTPS scheme — binary will be downloaded over an insecure connection: {base}"
                ));
            }
            return format!("{base}/tako-{os}-{arch}.tar.gz");
        }
    }
    format!(
        "https://github.com/{REPO_OWNER}/{REPO_NAME}/releases/download/{tag}/tako-{os}-{arch}.tar.gz"
    )
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
// Version resolution
// ---------------------------------------------------------------------------

async fn check_for_updates(channel: UpgradeChannel) -> Result<UpdateCheck, String> {
    output::with_spinner_async_simple("Checking for updates", check_for_updates_inner(channel))
        .await
}

async fn check_for_updates_inner(channel: UpgradeChannel) -> Result<UpdateCheck, String> {
    tracing::debug!("Fetching tags from {}…", TAGS_API);
    let _t = output::timed("Fetch version tags");
    let tags = fetch_tags().await?;
    tracing::debug!("Fetched {} tag(s)", tags.len());

    // Canary upgrades are handled by run_canary_upgrade (hash-based comparison)
    assert!(channel == UpgradeChannel::Stable);

    let tag = tags
        .iter()
        .find(|t| t.name.starts_with(TAG_PREFIX))
        .ok_or_else(|| format!("no release found with prefix '{TAG_PREFIX}'"))?;

    let version = tag.name.strip_prefix(TAG_PREFIX).unwrap_or(&tag.name);
    tracing::debug!("Current: {}, latest: {}", CURRENT_VERSION, version);
    if version == CURRENT_VERSION {
        Ok(UpdateCheck::AlreadyCurrent)
    } else {
        Ok(UpdateCheck::Available {
            tag: tag.name.clone(),
            version: version.to_string(),
        })
    }
}

#[derive(Debug)]
struct TagInfo {
    name: String,
}

async fn fetch_tags() -> Result<Vec<TagInfo>, String> {
    let client = reqwest::Client::new();
    let resp = client
        .get(TAGS_API)
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

    let mut tags = Vec::new();
    for entry in &raw {
        if let Some(name) = entry["name"].as_str() {
            tags.push(TagInfo {
                name: name.to_string(),
            });
        }
    }

    Ok(tags)
}

/// Fetch latest stable tag name only (used as fallback when full check failed).
async fn fetch_latest_stable_tag() -> Result<String, String> {
    let tags = fetch_tags().await?;
    tags.iter()
        .find(|t| t.name.starts_with(TAG_PREFIX))
        .map(|t| t.name.clone())
        .ok_or_else(|| format!("no release found with prefix '{TAG_PREFIX}'"))
}

// ---------------------------------------------------------------------------
// Download with progress bar + install
// ---------------------------------------------------------------------------

async fn download_and_install(
    url: &str,
    install_dir: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    // Create temp directory
    let tmp_base = std::env::temp_dir();
    let tmp_dir = tmp_base.join(format!("tako-upgrade-{}", std::process::id()));
    std::fs::create_dir_all(&tmp_dir)?;

    let result = download_and_install_inner(url, install_dir, &tmp_dir).await;

    // Clean up temp directory
    let _ = std::fs::remove_dir_all(&tmp_dir);

    result
}

async fn download_and_install_inner(
    url: &str,
    install_dir: &Path,
    tmp_dir: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let archive_path = tmp_dir.join("tako.tar.gz");

    // Download with progress bar
    download_with_progress(url, &archive_path).await?;

    // Verify SHA256 — fail if checksum is unavailable
    let sha_url = format!("{url}.sha256");
    let expected = fetch_sha256(&sha_url)
        .await
        .map_err(|e| format!("SHA256 checksum unavailable for {sha_url}: {e}"))?;
    verify_sha256(&archive_path, &expected)?;

    // Extract
    tracing::debug!("Extracting archive…");
    {
        let _t = output::timed("Extract archive");
        let extract_dir_tmp = tmp_dir.join("extract");
        std::fs::create_dir_all(&extract_dir_tmp)?;
        extract_tarball(&archive_path, &extract_dir_tmp)?;
    }

    let extract_dir = tmp_dir.join("extract");

    // Install binaries
    tracing::debug!("Installing binaries to {}…", install_dir.display());
    let _t = output::timed("Install binaries");
    let tako_bin =
        find_binary(&extract_dir, "tako").ok_or("archive did not contain a tako binary")?;
    let dev_server_bin = find_binary(&extract_dir, "tako-dev-server")
        .ok_or("archive did not contain a tako-dev-server binary")?;
    let loopback_proxy_bin = find_binary(&extract_dir, "tako-loopback-proxy")
        .ok_or("archive did not contain a tako-loopback-proxy binary")?;

    std::fs::create_dir_all(install_dir)?;
    install_binary(&tako_bin, install_dir, "tako")?;
    install_binary(&dev_server_bin, install_dir, "tako-dev-server")?;
    install_binary(&loopback_proxy_bin, install_dir, "tako-loopback-proxy")?;

    Ok(())
}

async fn download_with_progress(url: &str, dest: &Path) -> Result<(), Box<dyn std::error::Error>> {
    download_archive(url, dest, true).await
}

async fn download_without_progress(
    url: &str,
    dest: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    download_archive(url, dest, false).await
}

async fn download_archive(
    url: &str,
    dest: &Path,
    show_progress: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    tracing::debug!("Downloading {}…", url);
    let _t = output::timed("Download release archive");
    let client = reqwest::Client::new();
    let mut resp = client
        .get(url)
        .header("User-Agent", "tako-cli")
        .send()
        .await?
        .error_for_status()
        .map_err(|e| format!("download failed: {e}"))?;

    let total = resp.content_length().unwrap_or(0);
    tracing::debug!(
        "Download started, content_length={}",
        if total > 0 {
            format!("{total} bytes")
        } else {
            "unknown".to_string()
        }
    );
    let mut file = std::fs::File::create(dest)?;
    let mut downloaded = 0u64;

    let transfer_progress =
        show_progress.then(|| output::TransferProgress::new("Downloading", "Downloaded", total));

    while let Some(chunk) = resp.chunk().await? {
        file.write_all(&chunk)?;
        downloaded += chunk.len() as u64;
        if let Some(tp) = &transfer_progress {
            tp.set_position(downloaded);
        }
    }

    if let Some(tp) = transfer_progress {
        tp.finish();
    }
    tracing::debug!("Downloaded {} bytes to {}", downloaded, dest.display());
    Ok(())
}

// ---------------------------------------------------------------------------
// SHA256 verification
// ---------------------------------------------------------------------------

async fn fetch_sha256(url: &str) -> Result<String, String> {
    let client = reqwest::Client::new();
    let resp = client
        .get(url)
        .header("User-Agent", "tako-cli")
        .send()
        .await
        .map_err(|e| format!("{e}"))?
        .error_for_status()
        .map_err(|e| format!("{e}"))?;
    let text = resp.text().await.map_err(|e| format!("{e}"))?;
    // SHA file format: "hash  filename" or just "hash"
    Ok(text.split_whitespace().next().unwrap_or("").to_string())
}

fn verify_sha256(path: &Path, expected: &str) -> Result<(), String> {
    let data = std::fs::read(path).map_err(|e| format!("failed to read archive: {e}"))?;
    let hash = Sha256::digest(&data);
    let actual = hex::encode(hash);

    if actual != expected {
        return Err(format!(
            "SHA256 mismatch: expected {expected}, got {actual}"
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Archive extraction and binary installation
// ---------------------------------------------------------------------------

fn extract_tarball(archive: &Path, dest: &Path) -> Result<(), String> {
    let status = Command::new("tar")
        .arg("-xzf")
        .arg(archive)
        .arg("-C")
        .arg(dest)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .status()
        .map_err(|e| format!("failed to run tar: {e}"))?;

    if !status.success() {
        return Err("failed to extract archive".to_string());
    }
    Ok(())
}

fn find_binary(dir: &Path, name: &str) -> Option<PathBuf> {
    for entry in std::fs::read_dir(dir).ok()? {
        let entry = entry.ok()?;
        let path = entry.path();
        if path.is_file() && path.file_name().map(|n| n == name).unwrap_or(false) {
            return Some(path);
        }
        if path.is_dir()
            && let Some(found) = find_binary(&path, name)
        {
            return Some(found);
        }
    }
    None
}

fn install_binary(src: &Path, dest_dir: &Path, name: &str) -> Result<(), String> {
    let dest = dest_dir.join(name);
    std::fs::copy(src, &dest)
        .map_err(|e| format!("failed to install {name} to {}: {e}", dest.display()))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("failed to set permissions on {name}: {e}"))?;
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Platform detection
// ---------------------------------------------------------------------------

fn detect_platform() -> Result<(&'static str, &'static str), String> {
    let os = if cfg!(target_os = "macos") {
        "darwin"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else {
        return Err("unsupported OS".to_string());
    };

    let arch = if cfg!(target_arch = "x86_64") {
        "x86_64"
    } else if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else {
        return Err("unsupported architecture".to_string());
    };

    Ok((os, arch))
}

fn resolve_install_dir() -> PathBuf {
    // Install to the same directory as the running binary
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        return dir.to_path_buf();
    }
    // Fallback to default install location
    dirs::home_dir()
        .map(|h| h.join(".local").join("bin"))
        .unwrap_or_else(|| PathBuf::from("/usr/local/bin"))
}

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
    fn detect_platform_returns_valid_pair() {
        let (os, arch) = detect_platform().unwrap();
        assert!(os == "darwin" || os == "linux");
        assert!(arch == "x86_64" || arch == "aarch64");
    }

    #[test]
    fn verify_sha256_rejects_mismatch() {
        let dir = std::env::temp_dir().join("tako-test-sha");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("test.bin");
        std::fs::write(&path, b"hello").unwrap();

        let err = verify_sha256(
            &path,
            "0000000000000000000000000000000000000000000000000000000000000000",
        )
        .unwrap_err();
        assert!(err.contains("SHA256 mismatch"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn verify_sha256_accepts_correct_hash() {
        let dir = std::env::temp_dir().join("tako-test-sha-ok");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("test.bin");
        std::fs::write(&path, b"hello").unwrap();

        // SHA256 of "hello"
        let expected = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";
        verify_sha256(&path, expected).unwrap();

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn find_binary_locates_file_in_subdirectory() {
        let dir = std::env::temp_dir().join("tako-test-find");
        let sub = dir.join("subdir");
        let _ = std::fs::create_dir_all(&sub);
        std::fs::write(sub.join("tako"), b"binary").unwrap();

        let found = find_binary(&dir, "tako");
        assert!(found.is_some());
        assert!(found.unwrap().ends_with("tako"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn find_binary_returns_none_when_missing() {
        let dir = std::env::temp_dir().join("tako-test-find-none");
        let _ = std::fs::create_dir_all(&dir);

        let found = find_binary(&dir, "nonexistent");
        assert!(found.is_none());

        let _ = std::fs::remove_dir_all(&dir);
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

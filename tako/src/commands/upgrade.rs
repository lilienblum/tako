use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use indicatif::{ProgressBar, ProgressStyle};
use sha2::{Digest, Sha256};

use crate::config::{UpgradeChannel, resolve_upgrade_channel};
use crate::output;

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

/// Result of checking for updates.
enum UpdateCheck {
    /// Current version matches latest.
    AlreadyCurrent,
    /// A newer version is available. Contains the tag name for download.
    Available { tag: String },
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

pub fn run(canary: bool, stable: bool) -> Result<(), Box<dyn std::error::Error>> {
    let channel = resolve_upgrade_channel(canary, stable)?;

    println!();
    output::step(&format!(
        "Current version: {}",
        output::highlight(&format_current_version())
    ));
    println!();

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(run_upgrade(channel))
}

fn format_current_version() -> String {
    match CANARY_SHA {
        Some(sha) if !sha.trim().is_empty() => {
            let short = &sha.trim()[..sha.trim().len().min(7)];
            format!("{CURRENT_VERSION}-canary-{short}")
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

    match method {
        CliUpgradeMethod::Installer => run_installer_upgrade(channel).await,
        CliUpgradeMethod::Homebrew => run_brew_upgrade(),
        CliUpgradeMethod::Cargo => run_cargo_upgrade(),
    }
}

// ---------------------------------------------------------------------------
// Installer upgrade (version check + progress bar download)
// ---------------------------------------------------------------------------

async fn run_installer_upgrade(channel: UpgradeChannel) -> Result<(), Box<dyn std::error::Error>> {
    let (os, arch) = detect_platform()?;
    let install_dir = resolve_install_dir();

    // Check for updates (both stable and canary)
    let check = check_for_updates(channel).await;

    match check {
        Ok(UpdateCheck::AlreadyCurrent) => {
            // Spinner already showed "Already on the latest version"
            return Ok(());
        }
        Ok(UpdateCheck::Available { tag }) => {
            println!();
            let url = tarball_url_for_tag(&tag, os, arch);
            download_and_install(&url, &install_dir).await?;

            let new_version = get_installed_version(&install_dir);
            output::success(&format!("Upgraded to {}", output::highlight(&new_version)));
            return Ok(());
        }
        Err(_) => {
            // Spinner already showed error; fall through to download anyway
        }
    }

    // Version check failed: download latest anyway
    let tag = match channel {
        UpgradeChannel::Canary => "canary-latest".to_string(),
        UpgradeChannel::Stable => fetch_latest_stable_tag()
            .await
            .map_err(|e| format!("Failed to resolve latest release: {e}"))?,
    };

    let url = tarball_url_for_tag(&tag, os, arch);
    download_and_install(&url, &install_dir).await?;

    let new_version = get_installed_version(&install_dir);
    output::success(&format!("Upgraded to {}", output::highlight(&new_version)));
    Ok(())
}

fn tarball_url_for_tag(tag: &str, os: &str, arch: &str) -> String {
    // Support TAKO_DOWNLOAD_BASE_URL override (used by canary builds and testing)
    if let Ok(base) = std::env::var("TAKO_DOWNLOAD_BASE_URL") {
        let base = base.trim().trim_end_matches('/');
        if !base.is_empty() {
            return format!("{base}/tako-{os}-{arch}.tar.gz");
        }
    }
    format!(
        "https://github.com/{REPO_OWNER}/{REPO_NAME}/releases/download/{tag}/tako-{os}-{arch}.tar.gz"
    )
}

// ---------------------------------------------------------------------------
// Homebrew / Cargo upgrades
// ---------------------------------------------------------------------------

fn run_brew_upgrade() -> Result<(), Box<dyn std::error::Error>> {
    output::with_spinner("Upgrading via Homebrew", "Upgraded via Homebrew", || {
        run_local_upgrade_command("brew", &["upgrade", "tako"])
    })
    .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;
    Ok(())
}

fn run_cargo_upgrade() -> Result<(), Box<dyn std::error::Error>> {
    output::with_spinner("Upgrading via cargo", "Upgraded via cargo", || {
        run_local_upgrade_command("cargo", &["install", "tako", "--locked"])
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

/// Check for updates with a transforming spinner.
///
/// Stable:
///   - new version:     `✓ Latest version: 0.0.3`
///   - already current: `✓ Already on the latest version`
///
/// Canary:
///   - new build:       `✓ New canary build available`
///   - already current: `✓ Already on the latest canary build`
///
/// Error: `✗ Could not check for updates`
async fn check_for_updates(channel: UpgradeChannel) -> Result<UpdateCheck, String> {
    if !output::is_interactive() {
        return check_for_updates_inner(channel).await;
    }

    let start = std::time::Instant::now();
    let pb = ProgressBar::new_spinner();
    pb.set_style(output::spinner_style());
    pb.set_message("Checking for updates...");
    pb.enable_steady_tick(Duration::from_millis(80));

    let result = check_for_updates_inner(channel).await;
    let elapsed = output::format_elapsed(start.elapsed());

    match &result {
        Ok(update) => {
            let check = output::bold(&output::brand_success("✓"));
            let msg = match (channel, update) {
                (UpgradeChannel::Stable, UpdateCheck::AlreadyCurrent) => {
                    "Already on the latest version".to_string()
                }
                (UpgradeChannel::Stable, UpdateCheck::Available { tag }) => {
                    let version = tag.strip_prefix(TAG_PREFIX).unwrap_or(tag);
                    format!("Latest version: {}", output::highlight(version))
                }
                (UpgradeChannel::Canary, UpdateCheck::AlreadyCurrent) => {
                    "Already on the latest canary build".to_string()
                }
                (UpgradeChannel::Canary, UpdateCheck::Available { .. }) => {
                    "New canary build available".to_string()
                }
            };
            if elapsed.is_empty() {
                pb.finish_with_message(format!("{check} {msg}"));
            } else {
                pb.finish_with_message(format!("{check} {msg} {}", output::brand_muted(&elapsed)));
            }
        }
        Err(_) => {
            let x = output::bold(&output::brand_error("✗"));
            pb.finish_with_message(format!("{x} Could not check for updates"));
        }
    }

    result
}

async fn check_for_updates_inner(channel: UpgradeChannel) -> Result<UpdateCheck, String> {
    let tags = fetch_tags().await?;

    match channel {
        UpgradeChannel::Stable => {
            let tag = tags
                .iter()
                .find(|t| t.name.starts_with(TAG_PREFIX))
                .ok_or_else(|| format!("no release found with prefix '{TAG_PREFIX}'"))?;

            let version = tag.name.strip_prefix(TAG_PREFIX).unwrap_or(&tag.name);
            if version == CURRENT_VERSION {
                Ok(UpdateCheck::AlreadyCurrent)
            } else {
                Ok(UpdateCheck::Available {
                    tag: tag.name.clone(),
                })
            }
        }
        UpgradeChannel::Canary => {
            let tag = tags
                .iter()
                .find(|t| t.name == "canary-latest")
                .ok_or("no canary release found")?;

            let current_sha = CANARY_SHA.map(|s| s.trim()).unwrap_or("");
            if !current_sha.is_empty() && sha_prefix_matches(current_sha, &tag.commit_sha) {
                Ok(UpdateCheck::AlreadyCurrent)
            } else {
                Ok(UpdateCheck::Available {
                    tag: tag.name.clone(),
                })
            }
        }
    }
}

fn sha_prefix_matches(a: &str, b: &str) -> bool {
    let len = a.len().min(b.len());
    len > 0 && a[..len].eq_ignore_ascii_case(&b[..len])
}

fn get_installed_version(install_dir: &Path) -> String {
    let tako_bin = install_dir.join("tako");
    let result = Command::new(&tako_bin)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output();

    match result {
        Ok(out) if out.status.success() => {
            let v = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if v.is_empty() {
                "latest".to_string()
            } else {
                v
            }
        }
        _ => "latest".to_string(),
    }
}

#[derive(Debug)]
struct TagInfo {
    name: String,
    commit_sha: String,
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
        if let (Some(name), Some(sha)) = (entry["name"].as_str(), entry["commit"]["sha"].as_str()) {
            tags.push(TagInfo {
                name: name.to_string(),
                commit_sha: sha.to_string(),
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

    // Verify SHA256 if checksum file is available
    let sha_url = format!("{url}.sha256");
    if let Ok(expected) = fetch_sha256(&sha_url).await {
        verify_sha256(&archive_path, &expected)?;
    }

    // Extract
    let extract_dir = tmp_dir.join("extract");
    std::fs::create_dir_all(&extract_dir)?;
    extract_tarball(&archive_path, &extract_dir)?;

    // Install binaries
    let tako_bin =
        find_binary(&extract_dir, "tako").ok_or("archive did not contain a tako binary")?;
    let dev_server_bin = find_binary(&extract_dir, "tako-dev-server")
        .ok_or("archive did not contain a tako-dev-server binary")?;

    std::fs::create_dir_all(install_dir)?;
    install_binary(&tako_bin, install_dir, "tako")?;
    install_binary(&dev_server_bin, install_dir, "tako-dev-server")?;

    Ok(())
}

async fn download_with_progress(url: &str, dest: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let client = reqwest::Client::new();
    let mut resp = client
        .get(url)
        .header("User-Agent", "tako-cli")
        .send()
        .await?
        .error_for_status()
        .map_err(|e| format!("download failed: {e}"))?;

    let total = resp.content_length();
    let mut file = std::fs::File::create(dest)?;
    let mut downloaded = 0u64;

    let pb = if output::is_interactive() {
        let pb = if let Some(total) = total {
            let pb = ProgressBar::new(total);
            pb.set_style(download_bar_style());
            pb
        } else {
            let pb = ProgressBar::new_spinner();
            pb.set_style(download_spinner_style());
            pb.enable_steady_tick(Duration::from_millis(80));
            pb
        };
        pb.set_message("Downloading");
        Some(pb)
    } else {
        None
    };

    while let Some(chunk) = resp.chunk().await? {
        file.write_all(&chunk)?;
        downloaded += chunk.len() as u64;
        if let Some(pb) = &pb {
            pb.set_position(downloaded);
        }
    }

    if let Some(pb) = &pb {
        pb.finish_and_clear();
    }

    Ok(())
}

fn download_bar_style() -> ProgressStyle {
    ProgressStyle::with_template("  {msg}\n  {bar:30.cyan} {percent}%")
        .unwrap()
        .progress_chars("━╸─")
}

fn download_spinner_style() -> ProgressStyle {
    ProgressStyle::with_template("  {spinner} {msg}  ({bytes})")
        .unwrap()
        .tick_strings(&["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏", " "])
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
        if path.is_dir() {
            if let Some(found) = find_binary(&path, name) {
                return Some(found);
            }
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
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            return dir.to_path_buf();
        }
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
    fn format_current_version_stable() {
        // In tests, CANARY_SHA is None (not set at build time)
        let version = format_current_version();
        assert_eq!(version, CURRENT_VERSION);
    }

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
    fn sha_prefix_matches_full_match() {
        assert!(sha_prefix_matches("abc1234", "abc1234"));
    }

    #[test]
    fn sha_prefix_matches_short_vs_long() {
        assert!(sha_prefix_matches("abc1234", "abc1234567890abcdef"));
        assert!(sha_prefix_matches("abc1234567890abcdef", "abc1234"));
    }

    #[test]
    fn sha_prefix_matches_rejects_mismatch() {
        assert!(!sha_prefix_matches("abc1234", "def5678"));
    }

    #[test]
    fn sha_prefix_matches_rejects_empty() {
        assert!(!sha_prefix_matches("", "abc1234"));
        assert!(!sha_prefix_matches("abc1234", ""));
    }

    #[test]
    fn sha_prefix_matches_case_insensitive() {
        assert!(sha_prefix_matches("ABC1234", "abc1234"));
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
}

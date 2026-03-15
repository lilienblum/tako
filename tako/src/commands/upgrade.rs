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
    /// A newer version is available. Contains the tag name for download and a display version string.
    Available { tag: String, version: String },
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

pub fn run(canary: bool, stable: bool) -> Result<(), Box<dyn std::error::Error>> {
    let channel = resolve_upgrade_channel(canary, stable)?;

    if channel == UpgradeChannel::Canary {
        output::ContextBlock::new().channel("canary").print();
        let ver = current_version();
        output::info(&format!("Your current version: {}", output::strong(&ver)));
        tracing::info!("Current version: {ver}");
    }

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(run_upgrade(channel))
}

fn current_version() -> String {
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

/// Canary upgrade: fetch the remote tarball checksum first to skip download
/// when already current, otherwise download, verify, and install.
async fn run_canary_upgrade(
    os: &str,
    arch: &str,
    install_dir: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let url = tarball_url_for_tag("canary-latest", os, arch);
    let sha_url = format!("{url}.sha256");

    // Quick check: compare remote tarball hash with locally saved hash from last upgrade
    let saved_hash_path = canary_hash_path();
    if let Ok(remote_hash) = fetch_sha256(&sha_url).await {
        if let Some(ref path) = saved_hash_path {
            if let Ok(saved_hash) = std::fs::read_to_string(path) {
                if saved_hash.trim() == remote_hash {
                    tracing::info!("Already on the latest canary build");
                    output::success("Already on the latest canary build");
                    return Ok(());
                }
            }
        }
    }

    // Download and extract to temp dir
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

        let new_tako =
            find_binary(&extract_dir, "tako").ok_or("archive did not contain a tako binary")?;

        // Compare hashes of current and downloaded binary
        let current_exe = install_dir.join("tako");
        if current_exe.exists() {
            let current_hash = hash_file(&current_exe)?;
            let new_hash = hash_file(&new_tako)?;
            if current_hash == new_hash {
                // Save tarball hash so next time we can skip the download
                save_canary_hash(saved_hash_path.as_deref(), &archive_path);
                tracing::info!("Already on the latest canary build");
                output::success("Already on the latest canary build");
                return Ok(());
            }
        }

        // Get version from the new binary before installing
        let version = get_binary_version(&new_tako).unwrap_or_else(|| "canary".to_string());

        // Install
        let new_dev_server = find_binary(&extract_dir, "tako-dev-server")
            .ok_or("archive did not contain a tako-dev-server binary")?;
        let new_loopback_proxy = find_binary(&extract_dir, "tako-loopback-proxy")
            .ok_or("archive did not contain a tako-loopback-proxy binary")?;
        std::fs::create_dir_all(install_dir)?;
        install_binary(&new_tako, install_dir, "tako")?;
        install_binary(&new_dev_server, install_dir, "tako-dev-server")?;
        install_binary(&new_loopback_proxy, install_dir, "tako-loopback-proxy")?;

        // Save tarball hash so next time we can skip the download
        save_canary_hash(saved_hash_path.as_deref(), &archive_path);

        tracing::info!("Upgraded to {version}");
        output::success(&format!("Upgraded to {}", output::strong(&version)));
        Ok(())
    }
    .await;

    let _ = std::fs::remove_dir_all(&tmp_dir);
    result
}

fn canary_hash_path() -> Option<PathBuf> {
    crate::paths::tako_data_dir()
        .ok()
        .map(|d| d.join("canary-tarball-sha256"))
}

fn save_canary_hash(path: Option<&Path>, archive: &Path) {
    let Some(path) = path else { return };
    if let Ok(hash) = hash_file(archive) {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(path, hash);
    }
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

    let total = resp.content_length();
    tracing::debug!(
        "Download started, content_length={}",
        total.map_or("unknown".to_string(), |n| format!("{n} bytes"))
    );
    let mut file = std::fs::File::create(dest)?;
    let mut downloaded = 0u64;

    let pb = if output::is_pretty() && output::is_interactive() {
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

    tracing::debug!("Downloaded {} bytes to {}", downloaded, dest.display());
    Ok(())
}

fn download_bar_style() -> ProgressStyle {
    ProgressStyle::with_template("{msg}\n{bar:40} {pos}/{len}")
        .unwrap()
        .progress_chars("██░")
}

fn download_spinner_style() -> ProgressStyle {
    ProgressStyle::with_template("{spinner} {msg} ({bytes})")
        .unwrap()
        .tick_strings(crate::output::SPINNER_TICKS)
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

fn hash_file(path: &Path) -> Result<String, String> {
    let data =
        std::fs::read(path).map_err(|e| format!("failed to read {}: {e}", path.display()))?;
    Ok(hex::encode(Sha256::digest(&data)))
}

fn get_binary_version(path: &Path) -> Option<String> {
    let output = Command::new(path)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
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

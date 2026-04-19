use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use sha2::{Digest, Sha256};

use super::task_tree::{UpgradeTaskId, UpgradeTaskTreeController};
use crate::output;

pub(super) async fn download_and_install_pretty(
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
            let new_dev_proxy = find_binary(&extract_dir, "tako-dev-proxy")
                .ok_or_else(|| "archive did not contain a tako-dev-proxy binary".to_string())?;
            std::fs::create_dir_all(install_dir)
                .map_err(|e| format!("failed to create install dir: {e}"))?;
            install_binary(&new_tako, install_dir, "tako")?;
            install_binary(&new_dev_server, install_dir, "tako-dev-server")?;
            install_binary(&new_dev_proxy, install_dir, "tako-dev-proxy")?;
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

pub(super) async fn download_and_install(
    url: &str,
    install_dir: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let tmp_base = std::env::temp_dir();
    let tmp_dir = tmp_base.join(format!("tako-upgrade-{}", std::process::id()));
    std::fs::create_dir_all(&tmp_dir)?;

    let result = download_and_install_inner(url, install_dir, &tmp_dir).await;

    let _ = std::fs::remove_dir_all(&tmp_dir);

    result
}

async fn download_and_install_inner(
    url: &str,
    install_dir: &Path,
    tmp_dir: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let archive_path = tmp_dir.join("tako.tar.gz");

    download_with_progress(url, &archive_path).await?;

    let sha_url = format!("{url}.sha256");
    let expected = fetch_sha256(&sha_url)
        .await
        .map_err(|e| format!("SHA256 checksum unavailable for {sha_url}: {e}"))?;
    verify_sha256(&archive_path, &expected)?;

    {
        let _t = output::timed("Extract archive");
        let extract_dir_tmp = tmp_dir.join("extract");
        std::fs::create_dir_all(&extract_dir_tmp)?;
        extract_tarball(&archive_path, &extract_dir_tmp)?;
    }

    let extract_dir = tmp_dir.join("extract");

    let _t = output::timed(&format!("Install binaries to {}", install_dir.display()));
    let tako_bin =
        find_binary(&extract_dir, "tako").ok_or("archive did not contain a tako binary")?;
    let dev_server_bin = find_binary(&extract_dir, "tako-dev-server")
        .ok_or("archive did not contain a tako-dev-server binary")?;
    let dev_proxy_bin = find_binary(&extract_dir, "tako-dev-proxy")
        .ok_or("archive did not contain a tako-dev-proxy binary")?;

    std::fs::create_dir_all(install_dir)?;
    install_binary(&tako_bin, install_dir, "tako")?;
    install_binary(&dev_server_bin, install_dir, "tako-dev-server")?;
    install_binary(&dev_proxy_bin, install_dir, "tako-dev-proxy")?;

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
    let _t = output::timed(&format!("Download release archive from {url}"));
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

pub(super) fn detect_platform() -> Result<(&'static str, &'static str), String> {
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

pub(super) fn resolve_install_dir() -> PathBuf {
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        return dir.to_path_buf();
    }

    dirs::home_dir()
        .map(|h| h.join(".local").join("bin"))
        .unwrap_or_else(|| PathBuf::from("/usr/local/bin"))
}

#[cfg(test)]
mod tests {
    use super::*;

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
}

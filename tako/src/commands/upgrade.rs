use std::process::{Command, Stdio};

use crate::output;

const DEFAULT_INSTALL_URL: &str = "https://tako.sh/install";
const INSTALL_URL_ENV: &str = "TAKO_INSTALL_URL";

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

pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    let install_url = resolve_install_url();
    output::section("Upgrade");
    output::step(&format!(
        "Installing latest tako CLI from {}",
        output::emphasized(&install_url)
    ));

    let downloader = select_downloader(command_exists("curl"), command_exists("wget"))
        .map_err(|e| format!("{e}. Install curl or wget and retry."))?;

    run_installer(downloader, &install_url)?;

    output::success("Upgrade complete");
    Ok(())
}

fn run_installer(
    downloader: Downloader,
    install_url: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut download = Command::new(downloader.binary());
    downloader.apply_args(&mut download, install_url);
    let mut download = download
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|e| format!("failed to start {}: {}", downloader.binary(), e))?;

    let Some(download_stdout) = download.stdout.take() else {
        return Err("failed to capture installer download output".into());
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
        return Err(format!("failed to download installer from {}", install_url).into());
    }
    if !install_status.success() {
        return Err("installer script exited with a non-zero status".into());
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
}

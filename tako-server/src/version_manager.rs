//! Runtime version manager abstraction.
//!
//! Provides a trait for installing runtime versions and resolving binary paths,
//! with a proto implementation. The abstraction allows swapping the backing
//! tool (proto or a custom downloader) without changing call sites.

use std::path::Path;
use tokio::process::Command as TokioCommand;

/// A runtime version manager that can install tools and resolve binary paths.
#[async_trait::async_trait]
pub(crate) trait VersionManager: Send + Sync {
    /// Install a specific version of a runtime tool. Should be a no-op if
    /// already installed.
    async fn install(&self, tool: &str, version: &str) -> Result<(), String>;

    /// Return the absolute path to the binary for a specific tool version.
    async fn bin(&self, tool: &str, version: &str) -> Result<String, String>;
}

/// Proto-backed version manager. Shells out to the `proto` CLI.
pub(crate) struct Proto;

/// Drop privileges on a proto command to the `tako` service user if running as
/// root. Proto stores its toolchain cache in `~/.proto/`; when tako-server runs
/// as root (outside the usual systemd unit), this ensures proto operates on the
/// service user's store rather than `/root/.proto`.
#[cfg(unix)]
fn drop_to_service_user(cmd: &mut TokioCommand) {
    if unsafe { libc::geteuid() } != 0 {
        return;
    }
    use std::ffi::CString;
    let Ok(name) = CString::new("tako") else {
        return;
    };
    // SAFETY: getpwnam is safe when not modifying the passwd db concurrently.
    let pw = unsafe { libc::getpwnam(name.as_ptr()) };
    if pw.is_null() {
        return;
    }
    unsafe {
        cmd.uid((*pw).pw_uid);
        cmd.gid((*pw).pw_gid);
        let home = std::ffi::CStr::from_ptr((*pw).pw_dir);
        cmd.env("HOME", home.to_string_lossy().as_ref());
    }
}

#[async_trait::async_trait]
impl VersionManager for Proto {
    async fn install(&self, tool: &str, version: &str) -> Result<(), String> {
        let mut cmd = TokioCommand::new("proto");
        cmd.args(["install", tool, version])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());

        #[cfg(unix)]
        drop_to_service_user(&mut cmd);

        let output = cmd
            .output()
            .await
            .map_err(|e| format!("Failed to run 'proto install {} {}': {}", tool, version, e))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!(
                "proto install {} {} failed (exit {}): {}",
                tool,
                version,
                output.status.code().unwrap_or(-1),
                stderr.trim()
            ));
        }

        Ok(())
    }

    async fn bin(&self, tool: &str, version: &str) -> Result<String, String> {
        let mut cmd = TokioCommand::new("proto");
        cmd.args(["bin", tool, version])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());

        #[cfg(unix)]
        drop_to_service_user(&mut cmd);

        let output = cmd
            .output()
            .await
            .map_err(|e| format!("Failed to run 'proto bin {} {}': {}", tool, version, e))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!(
                "proto bin {} {} failed (exit {}): {}",
                tool,
                version,
                output.status.code().unwrap_or(-1),
                stderr.trim()
            ));
        }

        let bin_path = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if bin_path.is_empty() {
            return Err(format!(
                "proto bin {} {} returned empty path",
                tool, version
            ));
        }

        Ok(bin_path)
    }
}

/// Detect which version manager is available. Returns `None` if none found.
pub(crate) async fn detect() -> Option<Box<dyn VersionManager>> {
    if is_proto_available().await {
        return Some(Box::new(Proto));
    }
    None
}

async fn is_proto_available() -> bool {
    TokioCommand::new("proto")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Install a runtime and return the absolute binary path. Returns `None` if no
/// version manager is available or no version is specified.
pub(crate) async fn install_and_resolve(tool: &str, version: Option<&str>) -> Option<String> {
    let version = version?;
    let vm = detect().await?;

    if let Err(e) = vm.install(tool, version).await {
        tracing::warn!(tool, version, error = %e, "Version manager install failed");
        return None;
    }

    match vm.bin(tool, version).await {
        Ok(bin) => {
            if Path::new(&bin).is_file() {
                tracing::info!(tool, version, bin = %bin, "Resolved runtime binary");
                Some(bin)
            } else {
                tracing::warn!(tool, version, bin = %bin, "Resolved binary path does not exist");
                None
            }
        }
        Err(e) => {
            tracing::warn!(tool, version, error = %e, "Version manager bin resolution failed");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn install_and_resolve_returns_none_without_version() {
        assert!(install_and_resolve("bun", None).await.is_none());
    }

    #[tokio::test]
    async fn install_and_resolve_returns_none_when_no_vm_available() {
        // In CI/test environments without proto, this should gracefully return None
        // rather than error.
        let result = install_and_resolve("bun", Some("99.99.99")).await;
        assert!(result.is_none());
    }
}

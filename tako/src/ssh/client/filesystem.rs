use std::path::Path;

use russh::ChannelMsg;

use super::super::error::{SshError, SshResult};
use super::{SshClient, shell_quote};

impl SshClient {
    /// Upload file contents to a remote path by piping bytes through `cat`.
    /// Uses the existing SSH connection (no SFTP subsystem needed).
    pub async fn upload(&self, local_path: &Path, remote_path: &str) -> SshResult<()> {
        self.upload_with_progress(local_path, remote_path, None)
            .await
    }

    /// Upload a file with optional progress callback `(transferred, total)`.
    pub async fn upload_with_progress(
        &self,
        local_path: &Path,
        remote_path: &str,
        progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> SshResult<()> {
        use tokio::io::AsyncReadExt;

        let mut file = tokio::fs::File::open(local_path)
            .await
            .map_err(|_| SshError::FileNotFound(local_path.to_path_buf()))?;
        let total = file
            .metadata()
            .await
            .map_err(|_| SshError::FileNotFound(local_path.to_path_buf()))?
            .len();

        let handle = self.handle.as_ref().ok_or(SshError::NotConnected)?;

        let mut channel = handle
            .channel_open_session()
            .await
            .map_err(|e| SshError::Channel(e.to_string()))?;

        channel
            .exec(true, format!("cat > {}", shell_quote(remote_path)))
            .await
            .map_err(|e| SshError::CommandFailed(e.to_string()))?;

        const CHUNK: usize = 64 * 1024;
        let mut buf = vec![0u8; CHUNK];
        let mut sent = 0u64;
        loop {
            let n = file
                .read(&mut buf)
                .await
                .map_err(|_| SshError::FileNotFound(local_path.to_path_buf()))?;
            if n == 0 {
                break;
            }
            channel
                .data(&buf[..n])
                .await
                .map_err(|e| SshError::CommandFailed(e.to_string()))?;
            sent += n as u64;
            if let Some(ref cb) = progress {
                cb(sent, total);
            }
        }

        channel
            .eof()
            .await
            .map_err(|e| SshError::CommandFailed(e.to_string()))?;

        let mut exit_code = 0u32;
        let mut stderr_buf = Vec::new();
        loop {
            match channel.wait().await {
                Some(ChannelMsg::ExitStatus { exit_status }) => {
                    exit_code = exit_status;
                }
                Some(ChannelMsg::ExtendedData { data, ext: 1 }) => {
                    stderr_buf.extend_from_slice(&data);
                }
                Some(ChannelMsg::Eof) => {}
                None => break,
                _ => {}
            }
        }

        if exit_code != 0 {
            return Err(SshError::NonZeroExit {
                code: exit_code,
                stderr: String::from_utf8_lossy(&stderr_buf).to_string(),
            });
        }

        Ok(())
    }

    /// Check if a file or directory exists
    pub async fn exists(&self, path: &str) -> SshResult<bool> {
        let output = self
            .exec(&format!(
                "test -e {} && echo yes || echo no",
                shell_quote(path)
            ))
            .await?;
        Ok(output.stdout.trim() == "yes")
    }

    /// Check if tako-server is installed
    pub async fn is_tako_installed(&self) -> SshResult<bool> {
        let output = self
            .exec("command -v tako-server 2>/dev/null || echo not_found")
            .await?;
        Ok(!output.stdout.contains("not_found"))
    }

    /// Get tako-server version (just the version number, not the binary name prefix).
    pub async fn tako_version(&self) -> SshResult<Option<String>> {
        let output = self
            .exec("tako-server --version 2>/dev/null || true")
            .await?;
        let raw = output.stdout.trim();
        if raw.is_empty() {
            Ok(None)
        } else {
            Ok(Some(
                raw.strip_prefix("tako-server ").unwrap_or(raw).to_string(),
            ))
        }
    }

    /// Create a directory (with parents)
    pub async fn mkdir(&self, path: &str) -> SshResult<()> {
        self.exec_checked(&format!("mkdir -p {}", shell_quote(path)))
            .await?;
        Ok(())
    }

    /// Remove a file or directory
    pub async fn rm(&self, path: &str, recursive: bool) -> SshResult<()> {
        let quoted = shell_quote(path);
        let cmd = if recursive {
            format!("rm -rf {}", quoted)
        } else {
            format!("rm -f {}", quoted)
        };
        self.exec_checked(&cmd).await?;
        Ok(())
    }

    /// Create a symlink
    pub async fn symlink(&self, target: &str, link: &str) -> SshResult<()> {
        self.exec_checked(&format!(
            "ln -sfn {} {}",
            shell_quote(target),
            shell_quote(link)
        ))
        .await?;
        Ok(())
    }

    /// Read a remote file's contents
    pub async fn read_file(&self, path: &str) -> SshResult<String> {
        let output = self
            .exec_checked(&format!("cat {}", shell_quote(path)))
            .await?;
        Ok(output.stdout)
    }

    /// Write content to a remote file
    pub async fn write_file(&self, path: &str, content: &str) -> SshResult<()> {
        self.exec_checked(&format!(
            "printf '%s' {} > {}",
            shell_quote(content),
            shell_quote(path)
        ))
        .await?;
        Ok(())
    }

    /// List directory contents
    pub async fn ls(&self, path: &str) -> SshResult<Vec<String>> {
        let output = self
            .exec(&format!("ls -1 {} 2>/dev/null || true", shell_quote(path)))
            .await?;
        Ok(output
            .stdout
            .lines()
            .filter(|l| !l.is_empty())
            .map(String::from)
            .collect())
    }
}

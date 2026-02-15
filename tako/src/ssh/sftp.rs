//! SFTP client for file transfers

use super::client::SshClient;
use super::error::{SshError, SshResult};
use russh_sftp::client::SftpSession;
use std::ffi::OsString;
use std::path::Path;
use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Progress callback for file transfers
pub type ProgressCallback = Box<dyn Fn(u64, u64) + Send>;

/// SFTP client for file operations
pub struct SftpClient {
    session: Option<SftpSession>,
}

impl SftpClient {
    /// Create SFTP client from SSH client
    pub async fn new(ssh: &SshClient) -> SshResult<Self> {
        let handle = ssh.handle.as_ref().ok_or(SshError::NotConnected)?;

        let channel = handle
            .channel_open_session()
            .await
            .map_err(|e| SshError::Channel(e.to_string()))?;

        channel
            .request_subsystem(true, "sftp")
            .await
            .map_err(|e| SshError::Sftp(format!("Failed to request SFTP subsystem: {}", e)))?;

        let session = SftpSession::new(channel.into_stream())
            .await
            .map_err(|e| SshError::Sftp(format!("Failed to create SFTP session: {}", e)))?;

        Ok(Self {
            session: Some(session),
        })
    }

    /// Upload a file to the remote server
    pub async fn upload(&self, local_path: &Path, remote_path: &str) -> SshResult<()> {
        self.upload_with_progress(local_path, remote_path, None)
            .await
    }

    /// Upload a file with progress reporting
    pub async fn upload_with_progress(
        &self,
        local_path: &Path,
        remote_path: &str,
        progress: Option<ProgressCallback>,
    ) -> SshResult<()> {
        let session = self.session.as_ref().ok_or(SshError::NotConnected)?;

        // Read local file
        let mut file = File::open(local_path)
            .await
            .map_err(|_e| SshError::FileNotFound(local_path.to_path_buf()))?;

        let metadata = file.metadata().await?;
        let total_size = metadata.len();

        // Create remote file
        let mut remote_file = session
            .create(remote_path)
            .await
            .map_err(|e| SshError::Sftp(format!("Failed to create remote file: {}", e)))?;

        // Transfer in chunks
        let mut buffer = vec![0u8; 64 * 1024]; // 64KB chunks
        let mut transferred = 0u64;

        loop {
            let n = file.read(&mut buffer).await?;
            if n == 0 {
                break;
            }

            remote_file
                .write_all(&buffer[..n])
                .await
                .map_err(|e| SshError::Sftp(format!("Failed to write to remote file: {}", e)))?;

            transferred += n as u64;

            if let Some(ref cb) = progress {
                cb(transferred, total_size);
            }
        }

        remote_file
            .shutdown()
            .await
            .map_err(|e| SshError::Sftp(format!("Failed to close remote file: {}", e)))?;

        tracing::debug!(
            local = %local_path.display(),
            remote = %remote_path,
            size = total_size,
            "File uploaded"
        );

        Ok(())
    }

    /// Download a file from the remote server
    pub async fn download(&self, remote_path: &str, local_path: &Path) -> SshResult<()> {
        self.download_with_progress(remote_path, local_path, None)
            .await
    }

    /// Download a file with progress reporting
    pub async fn download_with_progress(
        &self,
        remote_path: &str,
        local_path: &Path,
        progress: Option<ProgressCallback>,
    ) -> SshResult<()> {
        let session = self.session.as_ref().ok_or(SshError::NotConnected)?;

        // Get remote file size
        let metadata = session
            .metadata(remote_path)
            .await
            .map_err(|e| SshError::Sftp(format!("Failed to get remote file metadata: {}", e)))?;

        let total_size = metadata.size.unwrap_or(0);

        // Open remote file
        let mut remote_file = session
            .open(remote_path)
            .await
            .map_err(|e| SshError::Sftp(format!("Failed to open remote file: {}", e)))?;

        // Create local file
        let mut local_file = File::create(local_path).await?;

        // Transfer in chunks
        let mut buffer = vec![0u8; 64 * 1024];
        let mut transferred = 0u64;

        loop {
            let n = remote_file
                .read(&mut buffer)
                .await
                .map_err(|e| SshError::Sftp(format!("Failed to read remote file: {}", e)))?;

            if n == 0 {
                break;
            }

            local_file.write_all(&buffer[..n]).await?;
            transferred += n as u64;

            if let Some(ref cb) = progress {
                cb(transferred, total_size);
            }
        }

        tracing::debug!(
            remote = %remote_path,
            local = %local_path.display(),
            size = transferred,
            "File downloaded"
        );

        Ok(())
    }

    /// Create a remote directory
    pub async fn mkdir(&self, path: &str) -> SshResult<()> {
        let session = self.session.as_ref().ok_or(SshError::NotConnected)?;

        session
            .create_dir(path)
            .await
            .map_err(|e| SshError::Sftp(format!("Failed to create directory: {}", e)))?;

        Ok(())
    }

    /// Remove a remote file
    pub async fn remove(&self, path: &str) -> SshResult<()> {
        let session = self.session.as_ref().ok_or(SshError::NotConnected)?;

        session
            .remove_file(path)
            .await
            .map_err(|e| SshError::Sftp(format!("Failed to remove file: {}", e)))?;

        Ok(())
    }

    /// Remove a remote directory
    pub async fn rmdir(&self, path: &str) -> SshResult<()> {
        let session = self.session.as_ref().ok_or(SshError::NotConnected)?;

        session
            .remove_dir(path)
            .await
            .map_err(|e| SshError::Sftp(format!("Failed to remove directory: {}", e)))?;

        Ok(())
    }

    /// List directory contents
    pub async fn readdir(&self, path: &str) -> SshResult<Vec<String>> {
        let session = self.session.as_ref().ok_or(SshError::NotConnected)?;

        let entries = session
            .read_dir(path)
            .await
            .map_err(|e| SshError::Sftp(format!("Failed to read directory: {}", e)))?;

        Ok(entries.into_iter().map(|e| e.file_name()).collect())
    }

    /// Check if a remote path exists
    pub async fn exists(&self, path: &str) -> SshResult<bool> {
        let session = self.session.as_ref().ok_or(SshError::NotConnected)?;

        match session.metadata(path).await {
            Ok(_) => Ok(true),
            Err(_) => Ok(false),
        }
    }

    /// Get file size
    pub async fn file_size(&self, path: &str) -> SshResult<u64> {
        let session = self.session.as_ref().ok_or(SshError::NotConnected)?;

        let metadata = session
            .metadata(path)
            .await
            .map_err(|e| SshError::Sftp(format!("Failed to get metadata: {}", e)))?;

        Ok(metadata.size.unwrap_or(0))
    }
}

/// Upload a file using SSH (convenience function that uses cat)
/// This is a fallback when SFTP is not available
pub async fn upload_via_ssh(
    ssh: &SshClient,
    local_path: &Path,
    remote_path: &str,
) -> SshResult<()> {
    let content = tokio::fs::read(local_path)
        .await
        .map_err(|_| SshError::FileNotFound(local_path.to_path_buf()))?;

    // For binary files, we use base64 encoding
    let encoded = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &content);

    let cmd = format!("echo '{}' | base64 -d > {}", encoded, remote_path);

    ssh.exec_checked(&cmd).await?;

    tracing::debug!(
        local = %local_path.display(),
        remote = %remote_path,
        "File uploaded via SSH"
    );

    Ok(())
}

/// Upload a file using SCP command (most compatible)
pub async fn upload_via_scp(
    local_path: &Path,
    remote_host: &str,
    remote_port: u16,
    remote_path: &str,
    keys_dir: &Path,
) -> SshResult<()> {
    use tokio::process::Command;

    let remote_target = format!("tako@{}:{}", remote_host, remote_path);
    let identity_file = find_ssh_identity_file(keys_dir);
    let args = build_scp_args(local_path, remote_port, &remote_target, identity_file.as_deref());

    let output = Command::new("scp").args(args).output().await?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(SshError::Sftp(format!("SCP failed: {}", stderr)));
    }

    tracing::debug!(
        local = %local_path.display(),
        remote = %remote_target,
        "File uploaded via SCP"
    );

    Ok(())
}

fn find_ssh_identity_file(keys_dir: &Path) -> Option<std::path::PathBuf> {
    let key_names = ["id_ed25519", "id_rsa", "id_ecdsa", "id_dsa"];
    key_names
        .iter()
        .map(|name| keys_dir.join(name))
        .find(|path| path.exists())
}

fn build_scp_args(
    local_path: &Path,
    remote_port: u16,
    remote_target: &str,
    identity_file: Option<&Path>,
) -> Vec<OsString> {
    let mut args = vec![
        OsString::from("-P"),
        OsString::from(remote_port.to_string()),
        OsString::from("-o"),
        OsString::from("StrictHostKeyChecking=no"),
        OsString::from("-o"),
        OsString::from("BatchMode=yes"),
    ];

    if let Some(identity) = identity_file {
        args.extend([
            OsString::from("-o"),
            OsString::from("IdentitiesOnly=yes"),
            OsString::from("-i"),
            identity.as_os_str().to_os_string(),
        ]);
    }

    args.extend([
        local_path.as_os_str().to_os_string(),
        OsString::from(remote_target),
    ]);

    args
}

#[cfg(test)]
mod tests {
    use super::{build_scp_args, find_ssh_identity_file};
    use std::path::Path;
    use tempfile::TempDir;

    #[test]
    fn finds_identity_file_in_keys_directory() {
        let temp = TempDir::new().expect("create temp dir");
        let key_path = temp.path().join("id_ed25519");
        std::fs::write(&key_path, "test-key").expect("write key");

        let found = find_ssh_identity_file(temp.path()).expect("expected key path");
        assert_eq!(found, key_path);
    }

    #[test]
    fn prefers_ed25519_when_multiple_keys_exist() {
        let temp = TempDir::new().expect("create temp dir");
        let rsa_path = temp.path().join("id_rsa");
        let ed25519_path = temp.path().join("id_ed25519");
        std::fs::write(&rsa_path, "rsa").expect("write rsa");
        std::fs::write(&ed25519_path, "ed25519").expect("write ed25519");

        let found = find_ssh_identity_file(temp.path()).expect("expected key path");
        assert_eq!(found, ed25519_path);
    }

    #[test]
    fn includes_identity_args_when_identity_file_is_present() {
        let local = Path::new("/tmp/archive.tar.gz");
        let identity = Path::new("/tmp/.ssh/id_ed25519");
        let args = build_scp_args(local, 22, "tako@server1:/remote/release.tar.gz", Some(identity));

        let args_as_strings: Vec<String> = args
            .iter()
            .map(|s| s.to_string_lossy().to_string())
            .collect();

        assert!(args_as_strings.contains(&"-i".to_string()));
        assert!(args_as_strings.contains(&"/tmp/.ssh/id_ed25519".to_string()));
        assert!(args_as_strings.contains(&"IdentitiesOnly=yes".to_string()));
    }

    #[test]
    fn omits_identity_args_when_identity_file_is_absent() {
        let local = Path::new("/tmp/archive.tar.gz");
        let args = build_scp_args(local, 22, "tako@server1:/remote/release.tar.gz", None);

        let args_as_strings: Vec<String> = args
            .iter()
            .map(|s| s.to_string_lossy().to_string())
            .collect();

        assert!(!args_as_strings.contains(&"-i".to_string()));
        assert!(!args_as_strings.contains(&"IdentitiesOnly=yes".to_string()));
    }
}

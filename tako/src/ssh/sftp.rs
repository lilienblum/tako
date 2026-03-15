//! SFTP client for file transfers

use super::client::SshClient;
use super::error::{SshError, SshResult};
use russh_sftp::client::SftpSession;
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


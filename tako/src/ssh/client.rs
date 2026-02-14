//! SSH client implementation using russh

use super::error::{SshError, SshResult};
use russh::client::{self, Config, Handle, Handler};
use russh::keys::{Algorithm, PrivateKeyWithHashAlg, PublicKey, load_secret_key};
use russh::{ChannelMsg, Disconnect};
use std::future::Future;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tako_core::{Command, Response, ServerRuntimeInfo};

/// SSH connection configuration
#[derive(Debug, Clone)]
pub struct SshConfig {
    /// Remote hostname or IP
    pub host: String,
    /// SSH port (default 22)
    pub port: u16,
    /// Connection timeout
    pub timeout: Duration,
    /// Path to SSH keys directory (default ~/.ssh)
    pub keys_dir: Option<PathBuf>,
}

impl SshConfig {
    /// Create config from server entry
    pub fn from_server(host: &str, port: u16) -> Self {
        Self {
            host: host.to_string(),
            port,
            timeout: Duration::from_secs(30),
            keys_dir: None,
        }
    }

    /// Get the SSH keys directory
    pub fn keys_directory(&self) -> PathBuf {
        self.keys_dir.clone().unwrap_or_else(|| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".ssh")
        })
    }
}

/// Output from a command execution
#[derive(Debug, Clone)]
pub struct CommandOutput {
    /// Exit code (0 = success)
    pub exit_code: u32,
    /// Standard output
    pub stdout: String,
    /// Standard error
    pub stderr: String,
}

impl CommandOutput {
    /// Check if command succeeded
    pub fn success(&self) -> bool {
        self.exit_code == 0
    }

    /// Get combined output (stdout + stderr)
    pub fn combined(&self) -> String {
        if self.stderr.is_empty() {
            self.stdout.clone()
        } else if self.stdout.is_empty() {
            self.stderr.clone()
        } else {
            format!("{}\n{}", self.stdout, self.stderr)
        }
    }
}

/// Handler for SSH client events
pub struct SshHandler {
    /// Whether to accept any host key (for known hosts we'd verify)
    accept_any_host_key: bool,
}

impl Handler for SshHandler {
    type Error = SshError;

    fn check_server_key(
        &mut self,
        _server_public_key: &PublicKey,
    ) -> impl Future<Output = std::result::Result<bool, Self::Error>> + Send {
        // In production, we should verify against known_hosts
        // For now, accept all keys (like ssh -o StrictHostKeyChecking=no)
        let accept = self.accept_any_host_key;
        async move { Ok(accept) }
    }
}

/// SSH client for remote operations
pub struct SshClient {
    config: SshConfig,
    /// SSH session handle (public for SFTP access)
    pub handle: Option<Handle<SshHandler>>,

    tako_hello_checked: bool,
}

impl SshClient {
    /// Create a new SSH client
    pub fn new(config: SshConfig) -> Self {
        Self {
            config,
            handle: None,
            tako_hello_checked: false,
        }
    }

    fn interpret_hello_response(resp: &Response) -> Result<(), String> {
        match resp {
            Response::Ok { .. } => Ok(()),
            Response::Error { message } => {
                // Old servers will fail to deserialize `hello` and respond with "Invalid command".
                // Also catch serde's unknown-variant wording.
                let m = message.to_lowercase();
                if m.contains("invalid command") && m.contains("hello") {
                    return Err(
                        "Remote tako-server is too old (no protocol handshake support). Upgrade tako-server.".to_string(),
                    );
                }
                if m.contains("unknown variant") && m.contains("hello") {
                    return Err(
                        "Remote tako-server is too old (no protocol handshake support). Upgrade tako-server.".to_string(),
                    );
                }
                if m.contains("protocol version mismatch") {
                    return Err(format!("Remote tako-server protocol mismatch: {message}"));
                }
                Err(format!("tako-server handshake failed: {message}"))
            }
        }
    }

    /// Connect to the remote server
    pub async fn connect(&mut self) -> SshResult<()> {
        let ssh_config = Config {
            inactivity_timeout: Some(self.config.timeout),
            keepalive_interval: Some(Duration::from_secs(15)),
            keepalive_max: 3,
            ..Default::default()
        };

        let handler = SshHandler {
            accept_any_host_key: true,
        };

        let addr = format!("{}:{}", self.config.host, self.config.port);

        tracing::debug!(host = %self.config.host, port = self.config.port, "Connecting to SSH server");

        let mut handle = tokio::time::timeout(self.config.timeout, async {
            client::connect(Arc::new(ssh_config), addr, handler).await
        })
        .await
        .map_err(|_| SshError::Timeout("Connection timed out".to_string()))?
        .map_err(|e| SshError::Connection(e.to_string()))?;

        // Authenticate with SSH keys
        self.authenticate(&mut handle).await?;

        self.handle = Some(handle);
        tracing::info!(host = %self.config.host, "SSH connection established");

        Ok(())
    }

    /// Authenticate using SSH keys
    async fn authenticate(&self, handle: &mut Handle<SshHandler>) -> SshResult<()> {
        let keys_dir = self.config.keys_directory();

        // Try common key names in order
        let key_names = ["id_ed25519", "id_rsa", "id_ecdsa", "id_dsa"];

        let mut last_error = None;
        let mut found_any_key_file = false;

        for key_name in &key_names {
            let key_path = keys_dir.join(key_name);

            if !key_path.exists() {
                continue;
            }

            found_any_key_file = true;
            tracing::debug!(key = %key_path.display(), "Trying SSH key");

            match self.try_key_auth(handle, &key_path).await {
                Ok(true) => {
                    tracing::debug!(key = %key_path.display(), "Authentication successful");
                    return Ok(());
                }
                Ok(false) => {
                    tracing::debug!(key = %key_path.display(), "Key not accepted");
                }
                Err(e) => {
                    tracing::debug!(key = %key_path.display(), error = %e, "Key auth failed");
                    last_error = Some(e);
                }
            }
        }

        // Fall back to ssh-agent if available. This supports setups where keys live in the agent
        // (e.g. macOS Keychain, 1Password, etc.) or when private keys are passphrase-protected.
        match self.try_agent_auth(handle).await {
            Ok(true) => return Ok(()),
            Ok(false) => {}
            Err(e) => last_error = Some(e),
        }

        if found_any_key_file {
            Err(last_error.unwrap_or_else(|| {
                SshError::Authentication("No SSH keys were accepted by the server".to_string())
            }))
        } else {
            Err(last_error.unwrap_or(SshError::NoKeysFound(keys_dir)))
        }
    }

    async fn try_agent_auth(&self, handle: &mut Handle<SshHandler>) -> SshResult<bool> {
        #[cfg(unix)]
        {
            use russh::keys::agent::client::AgentClient;

            let mut agent = match AgentClient::connect_env().await {
                Ok(agent) => agent,
                Err(_) => return Ok(false),
            };

            let keys = agent.request_identities().await.map_err(|e| {
                SshError::Authentication(format!("ssh-agent identities failed: {e}"))
            })?;

            for key in keys {
                match handle
                    .authenticate_publickey_with(Self::ssh_user(), key.clone(), None, &mut agent)
                    .await
                {
                    Ok(result) if result.success() => return Ok(true),
                    Ok(_) => continue,
                    Err(e) => {
                        return Err(SshError::Authentication(format!(
                            "ssh-agent authentication failed: {e}"
                        )));
                    }
                }
            }

            Ok(false)
        }

        #[cfg(not(unix))]
        {
            let _ = handle;
            Ok(false)
        }
    }

    /// Try authenticating with a specific key
    async fn try_key_auth(
        &self,
        handle: &mut Handle<SshHandler>,
        key_path: &Path,
    ) -> SshResult<bool> {
        // Load the key. Prefer non-interactive sources of passphrases (env var) and only prompt
        // when we're attached to a terminal.
        let key = match load_secret_key(key_path, None) {
            Ok(k) => k,
            Err(e) => {
                let pass = std::env::var("TAKO_SSH_KEY_PASSPHRASE").ok().or_else(|| {
                    if std::io::stdin().is_terminal() {
                        crate::output::prompt_password(
                            &format!("SSH key passphrase for {}", key_path.display()),
                            true,
                        )
                        .ok()
                    } else {
                        None
                    }
                });

                match pass {
                    Some(pass) => {
                        load_secret_key(key_path, Some(&pass)).map_err(|e| SshError::KeyLoad {
                            path: key_path.to_path_buf(),
                            reason: e.to_string(),
                        })?
                    }
                    None => {
                        return Err(SshError::KeyLoad {
                            path: key_path.to_path_buf(),
                            reason: e.to_string(),
                        });
                    }
                }
            }
        };

        let hash_alg = if matches!(key.algorithm(), Algorithm::Rsa { .. }) {
            handle
                .best_supported_rsa_hash()
                .await
                .map_err(|e| SshError::Authentication(e.to_string()))?
                .flatten()
        } else {
            None
        };

        let auth_result = handle
            .authenticate_publickey(
                Self::ssh_user(),
                PrivateKeyWithHashAlg::new(Arc::new(key), hash_alg),
            )
            .await
            .map_err(|e| SshError::Authentication(e.to_string()))?;

        Ok(auth_result.success())
    }

    fn ssh_user() -> &'static str {
        // v1 intentionally has a fixed SSH user for all servers.
        "tako"
    }

    /// Execute a command on the remote server
    pub async fn exec(&self, command: &str) -> SshResult<CommandOutput> {
        let handle = self.handle.as_ref().ok_or(SshError::NotConnected)?;

        tracing::debug!(command = %command, "Executing remote command");

        let mut channel = handle
            .channel_open_session()
            .await
            .map_err(|e| SshError::Channel(e.to_string()))?;

        channel
            .exec(true, command)
            .await
            .map_err(|e| SshError::CommandFailed(e.to_string()))?;

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut exit_code = 0u32;

        loop {
            match channel.wait().await {
                Some(ChannelMsg::Data { data }) => {
                    stdout.extend_from_slice(&data);
                }
                Some(ChannelMsg::ExtendedData { data, ext }) => {
                    if ext == 1 {
                        // stderr
                        stderr.extend_from_slice(&data);
                    }
                }
                Some(ChannelMsg::ExitStatus { exit_status }) => {
                    exit_code = exit_status;
                }
                Some(ChannelMsg::Eof) | None => break,
                _ => {}
            }
        }

        let output = CommandOutput {
            exit_code,
            stdout: String::from_utf8_lossy(&stdout).to_string(),
            stderr: String::from_utf8_lossy(&stderr).to_string(),
        };

        tracing::debug!(
            exit_code = output.exit_code,
            stdout_len = output.stdout.len(),
            stderr_len = output.stderr.len(),
            "Command completed"
        );

        Ok(output)
    }

    /// Execute a command and return error if it fails
    pub async fn exec_checked(&self, command: &str) -> SshResult<CommandOutput> {
        let output = self.exec(command).await?;

        if !output.success() {
            return Err(SshError::NonZeroExit {
                code: output.exit_code,
                stderr: output.stderr.clone(),
            });
        }

        Ok(output)
    }

    /// Execute a command and stream output to callbacks
    pub async fn exec_streaming<F, G>(
        &self,
        command: &str,
        mut on_stdout: F,
        mut on_stderr: G,
    ) -> SshResult<u32>
    where
        F: FnMut(&[u8]),
        G: FnMut(&[u8]),
    {
        let handle = self.handle.as_ref().ok_or(SshError::NotConnected)?;

        let mut channel = handle
            .channel_open_session()
            .await
            .map_err(|e| SshError::Channel(e.to_string()))?;

        channel
            .exec(true, command)
            .await
            .map_err(|e| SshError::CommandFailed(e.to_string()))?;

        let mut exit_code = 0u32;

        loop {
            match channel.wait().await {
                Some(ChannelMsg::Data { data }) => {
                    on_stdout(&data);
                }
                Some(ChannelMsg::ExtendedData { data, ext }) => {
                    if ext == 1 {
                        on_stderr(&data);
                    }
                }
                Some(ChannelMsg::ExitStatus { exit_status }) => {
                    exit_code = exit_status;
                }
                Some(ChannelMsg::Eof) | None => break,
                _ => {}
            }
        }

        Ok(exit_code)
    }

    /// Check if a file or directory exists
    pub async fn exists(&self, path: &str) -> SshResult<bool> {
        let output = self
            .exec(&format!("test -e {} && echo yes || echo no", path))
            .await?;
        Ok(output.stdout.trim() == "yes")
    }

    /// Check if tako-server is installed
    pub async fn is_tako_installed(&self) -> SshResult<bool> {
        let output = self
            .exec("which tako-server 2>/dev/null || echo not_found")
            .await?;
        Ok(!output.stdout.contains("not_found"))
    }

    /// Get tako-server version
    pub async fn tako_version(&self) -> SshResult<Option<String>> {
        let output = self
            .exec("tako-server --version 2>/dev/null || true")
            .await?;
        if output.stdout.trim().is_empty() {
            Ok(None)
        } else {
            Ok(Some(output.stdout.trim().to_string()))
        }
    }

    /// Create a directory (with parents)
    pub async fn mkdir(&self, path: &str) -> SshResult<()> {
        self.exec_checked(&format!("mkdir -p {}", path)).await?;
        Ok(())
    }

    /// Remove a file or directory
    pub async fn rm(&self, path: &str, recursive: bool) -> SshResult<()> {
        let cmd = if recursive {
            format!("rm -rf {}", path)
        } else {
            format!("rm -f {}", path)
        };
        self.exec_checked(&cmd).await?;
        Ok(())
    }

    /// Create a symlink
    pub async fn symlink(&self, target: &str, link: &str) -> SshResult<()> {
        self.exec_checked(&format!("ln -sfn {} {}", target, link))
            .await?;
        Ok(())
    }

    /// Read a remote file's contents
    pub async fn read_file(&self, path: &str) -> SshResult<String> {
        let output = self.exec_checked(&format!("cat {}", path)).await?;
        Ok(output.stdout)
    }

    /// Write content to a remote file
    pub async fn write_file(&self, path: &str, content: &str) -> SshResult<()> {
        // Escape content for shell
        let escaped = content.replace("'", "'\\''");
        self.exec_checked(&format!("echo '{}' > {}", escaped, path))
            .await?;
        Ok(())
    }

    /// List directory contents
    pub async fn ls(&self, path: &str) -> SshResult<Vec<String>> {
        let output = self
            .exec(&format!("ls -1 {} 2>/dev/null || true", path))
            .await?;
        Ok(output
            .stdout
            .lines()
            .filter(|l| !l.is_empty())
            .map(String::from)
            .collect())
    }

    /// Send a signal to tako-server (via unix socket or systemctl)
    pub async fn tako_reload(&self, app: Option<&str>) -> SshResult<()> {
        let cmd = match app {
            Some(app_name) => {
                let payload = serde_json::to_string(&Command::Reload {
                    app: app_name.to_string(),
                })
                .map_err(|e| SshError::CommandFailed(e.to_string()))?;
                Self::socket_request_command(&payload)
            }
            None => "sudo systemctl reload tako-server".to_string(),
        };
        self.exec_checked(&cmd).await?;
        Ok(())
    }

    /// Restart tako-server
    pub async fn tako_restart(&self) -> SshResult<()> {
        self.exec_checked("sudo systemctl restart tako-server")
            .await?;
        Ok(())
    }

    /// Get tako-server status
    pub async fn tako_status(&self) -> SshResult<String> {
        // Prefer probing the management unix socket directly. This works on
        // non-systemd hosts (e.g. minimal containers) and avoids sudo for
        // read-only status checks.
        let list_probe = r#"{"command":"list"}"#;
        if self.tako_command_raw(list_probe).await.is_ok() {
            return Ok("active".to_string());
        }

        // Fall back to service-manager status if socket probe fails.
        let output = self
            .exec(
                "(command -v systemctl >/dev/null 2>&1 && systemctl is-active tako-server 2>/dev/null) || echo unknown",
            )
            .await?;
        Ok(output.stdout.trim().to_string())
    }

    /// Send a command to tako-server via unix socket
    pub async fn tako_command(&mut self, json_command: &str) -> SshResult<String> {
        self.ensure_tako_hello().await?;
        self.tako_command_raw(json_command).await
    }

    async fn tako_command_raw(&self, json_command: &str) -> SshResult<String> {
        let output = self
            .exec_checked(&Self::socket_request_command(json_command))
            .await?;
        Self::extract_socket_stdout(output)
    }

    fn socket_request_command(json_command: &str) -> String {
        // `nc` implementations can keep the connection open after writing stdin.
        // Read one JSONL line and terminate the pipeline deterministically.
        Self::socket_request_command_on_path("/var/run/tako/tako.sock", json_command)
    }

    fn socket_request_command_on_path(socket_path: &str, json_command: &str) -> String {
        format!(
            "printf '%s\\n' '{}' | nc -U '{}' | head -n 1",
            json_command.replace("'", "'\\''"),
            socket_path.replace("'", "'\\''")
        )
    }

    fn extract_socket_stdout(output: CommandOutput) -> SshResult<String> {
        if output.stdout.trim().is_empty() {
            let stderr = output.stderr.trim();
            if stderr.is_empty() {
                return Err(SshError::CommandFailed(
                    "tako-server socket returned an empty response".to_string(),
                ));
            }
            return Err(SshError::CommandFailed(stderr.to_string()));
        }
        Ok(output.stdout)
    }

    async fn ensure_tako_hello(&mut self) -> SshResult<()> {
        if self.tako_hello_checked {
            return Ok(());
        }

        let cmd = Command::Hello {
            protocol_version: tako_core::PROTOCOL_VERSION,
        };
        let json =
            serde_json::to_string(&cmd).map_err(|e| SshError::CommandFailed(e.to_string()))?;
        let mut last_error: Option<SshError> = None;

        // A just-started service can transiently accept the socket but not answer yet.
        for attempt in 0..5 {
            let response_str = match self.tako_command_raw(&json).await {
                Ok(v) => v,
                Err(e) => {
                    last_error = Some(e);
                    if attempt < 4 {
                        tokio::time::sleep(Duration::from_millis(200)).await;
                        continue;
                    }
                    return Err(last_error.unwrap_or_else(|| {
                        SshError::CommandFailed("tako-server handshake failed".to_string())
                    }));
                }
            };

            let response: Response = match serde_json::from_str(&response_str) {
                Ok(value) => value,
                Err(e) if e.is_eof() && attempt < 4 => {
                    last_error = Some(SshError::CommandFailed(e.to_string()));
                    tokio::time::sleep(Duration::from_millis(200)).await;
                    continue;
                }
                Err(e) => return Err(SshError::CommandFailed(e.to_string())),
            };

            Self::interpret_hello_response(&response).map_err(SshError::CommandFailed)?;
            self.tako_hello_checked = true;
            return Ok(());
        }

        Err(last_error
            .unwrap_or_else(|| SshError::CommandFailed("tako-server handshake failed".to_string())))
    }

    /// Get status of a specific app from tako-server
    pub async fn tako_app_status(&mut self, app_name: &str) -> SshResult<Response> {
        let cmd = Command::Status {
            app: app_name.to_string(),
        };
        let json =
            serde_json::to_string(&cmd).map_err(|e| SshError::CommandFailed(e.to_string()))?;
        let response_str = self.tako_command(&json).await?;
        let response: Response = serde_json::from_str(&response_str)
            .map_err(|e| SshError::CommandFailed(e.to_string()))?;
        Ok(response)
    }

    /// List all apps from tako-server
    pub async fn tako_list_apps(&mut self) -> SshResult<Response> {
        let cmd = Command::List;
        let json =
            serde_json::to_string(&cmd).map_err(|e| SshError::CommandFailed(e.to_string()))?;
        let response_str = self.tako_command(&json).await?;
        let response: Response = serde_json::from_str(&response_str)
            .map_err(|e| SshError::CommandFailed(e.to_string()))?;
        Ok(response)
    }

    /// List all configured routes from tako-server
    pub async fn tako_routes(&mut self) -> SshResult<Response> {
        let cmd = Command::Routes;
        let json =
            serde_json::to_string(&cmd).map_err(|e| SshError::CommandFailed(e.to_string()))?;
        let response_str = self.tako_command(&json).await?;
        let response: Response = serde_json::from_str(&response_str)
            .map_err(|e| SshError::CommandFailed(e.to_string()))?;
        Ok(response)
    }

    /// Get runtime information from tako-server.
    pub async fn tako_server_info(&mut self) -> SshResult<ServerRuntimeInfo> {
        let cmd = Command::ServerInfo;
        let json =
            serde_json::to_string(&cmd).map_err(|e| SshError::CommandFailed(e.to_string()))?;
        let response_str = self.tako_command(&json).await?;
        parse_ok_data_response(response_str)
    }

    /// Enter upgrading mode using a durable owner lock.
    pub async fn tako_enter_upgrading(&mut self, owner: &str) -> SshResult<()> {
        let cmd = Command::EnterUpgrading {
            owner: owner.to_string(),
        };
        let json =
            serde_json::to_string(&cmd).map_err(|e| SshError::CommandFailed(e.to_string()))?;
        let response_str = self.tako_command(&json).await?;
        parse_ok_unit_response(response_str)
    }

    /// Exit upgrading mode using the lock owner.
    pub async fn tako_exit_upgrading(&mut self, owner: &str) -> SshResult<()> {
        let cmd = Command::ExitUpgrading {
            owner: owner.to_string(),
        };
        let json =
            serde_json::to_string(&cmd).map_err(|e| SshError::CommandFailed(e.to_string()))?;
        let response_str = self.tako_command(&json).await?;
        parse_ok_unit_response(response_str)
    }

    /// Send a command to a specific unix socket path.
    pub async fn tako_command_on_socket(
        &self,
        socket_path: &str,
        json_command: &str,
    ) -> SshResult<String> {
        let output = self
            .exec_checked(&Self::socket_request_command_on_path(
                socket_path,
                json_command,
            ))
            .await?;
        Self::extract_socket_stdout(output)
    }

    /// Run protocol hello against a specific unix socket path.
    pub async fn tako_hello_on_socket(&self, socket_path: &str) -> SshResult<()> {
        let cmd = Command::Hello {
            protocol_version: tako_core::PROTOCOL_VERSION,
        };
        let json =
            serde_json::to_string(&cmd).map_err(|e| SshError::CommandFailed(e.to_string()))?;
        let response_str = self.tako_command_on_socket(socket_path, &json).await?;
        let response: Response = serde_json::from_str(&response_str)
            .map_err(|e| SshError::CommandFailed(e.to_string()))?;
        Self::interpret_hello_response(&response).map_err(SshError::CommandFailed)
    }

    pub fn clear_tako_hello_cache(&mut self) {
        self.tako_hello_checked = false;
    }

    /// Disconnect from the server
    pub async fn disconnect(&mut self) -> SshResult<()> {
        if let Some(handle) = self.handle.take() {
            handle
                .disconnect(Disconnect::ByApplication, "", "en")
                .await
                .map_err(|e| SshError::Connection(e.to_string()))?;
        }
        Ok(())
    }

    /// Check if connected
    pub fn is_connected(&self) -> bool {
        self.handle.is_some()
    }

    /// Get the config
    pub fn config(&self) -> &SshConfig {
        &self.config
    }
}

impl Drop for SshClient {
    fn drop(&mut self) {
        // Connection will be closed when handle is dropped
    }
}

fn parse_ok_unit_response(response_str: String) -> SshResult<()> {
    let response: Response =
        serde_json::from_str(&response_str).map_err(|e| SshError::CommandFailed(e.to_string()))?;
    match response {
        Response::Ok { .. } => Ok(()),
        Response::Error { message } => Err(SshError::CommandFailed(message)),
    }
}

fn parse_ok_data_response<T: serde::de::DeserializeOwned>(response_str: String) -> SshResult<T> {
    let response: Response =
        serde_json::from_str(&response_str).map_err(|e| SshError::CommandFailed(e.to_string()))?;
    match response {
        Response::Ok { data } => {
            serde_json::from_value(data).map_err(|e| SshError::CommandFailed(e.to_string()))
        }
        Response::Error { message } => Err(SshError::CommandFailed(message)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ENCRYPTED_ED25519_KEY: &str = "-----BEGIN OPENSSH PRIVATE KEY-----\n\
b3BlbnNzaC1rZXktdjEAAAAACmFlczI1Ni1jdHIAAAAGYmNyeXB0AAAAGAAAABCRv2KPnI\n\
IRphE01i7dWiijAAAAGAAAAAEAAAAzAAAAC3NzaC1lZDI1NTE5AAAAIBS7MYzXocRVMCqK\n\
uxD+2gS1Q9ZtX7zYh74IFWEKRZ4OAAAAkEa8z/fYTNnkt7g2yLcFM8IQFw67+aUeTzC6V2\n\
g+KleH6OSa4Q3cbBSMhWFkNY/IjTKNNg7P2XszrFMJblBkWokMvKgh3oGfJV4Axh3RZUsS\n\
ep5Su4gT/9WhaF3n32sxVB3BhK8IDBQBfsXh+YLhP0bZFdN+jLffuAQlINtoFYY8/4vvsn\n\
l4QMs5cmnWfrM0GQ==\n\
-----END OPENSSH PRIVATE KEY-----\n";

    #[cfg(unix)]
    fn can_bind_localhost() -> bool {
        std::net::TcpListener::bind(("127.0.0.1", 0)).is_ok()
    }

    #[cfg(unix)]
    fn can_bind_unix_socket() -> bool {
        let Ok(dir) = tempfile::TempDir::new() else {
            return false;
        };
        let socket_path = dir.path().join("agent.sock");
        std::os::unix::net::UnixListener::bind(socket_path).is_ok()
    }

    #[test]
    fn test_ssh_config_creation() {
        let config = SshConfig::from_server("example.com", 22);
        assert_eq!(config.host, "example.com");
        assert_eq!(config.port, 22);
    }

    #[test]
    fn test_ssh_config_keys_directory() {
        let config = SshConfig::from_server("example.com", 22);
        let keys_dir = config.keys_directory();
        assert!(keys_dir.ends_with(".ssh"));
    }

    #[test]
    fn test_command_output_success() {
        let output = CommandOutput {
            exit_code: 0,
            stdout: "hello".to_string(),
            stderr: String::new(),
        };
        assert!(output.success());
    }

    #[test]
    fn test_command_output_failure() {
        let output = CommandOutput {
            exit_code: 1,
            stdout: String::new(),
            stderr: "error".to_string(),
        };
        assert!(!output.success());
    }

    #[test]
    fn test_command_output_combined() {
        let output = CommandOutput {
            exit_code: 0,
            stdout: "out".to_string(),
            stderr: "err".to_string(),
        };
        assert_eq!(output.combined(), "out\nerr");
    }

    #[test]
    fn test_ssh_client_creation() {
        let config = SshConfig::from_server("example.com", 22);
        let client = SshClient::new(config);
        assert!(!client.is_connected());
    }

    #[test]
    fn hello_response_interpretation_rejects_old_server() {
        let resp = Response::Error {
            message: "Invalid command: unknown variant `hello`, expected one of ...".to_string(),
        };
        let err = SshClient::interpret_hello_response(&resp).unwrap_err();
        assert!(err.to_lowercase().contains("too old"));
    }

    #[test]
    fn hello_response_interpretation_accepts_ok() {
        let resp = Response::Ok {
            data: serde_json::json!({"protocol_version": tako_core::PROTOCOL_VERSION}),
        };
        SshClient::interpret_hello_response(&resp).unwrap();
    }

    #[test]
    fn extract_socket_stdout_returns_stdout() {
        let output = CommandOutput {
            exit_code: 0,
            stdout: "{\"status\":\"ok\"}\n".to_string(),
            stderr: String::new(),
        };
        let value = SshClient::extract_socket_stdout(output).unwrap();
        assert!(value.contains("\"status\":\"ok\""));
    }

    #[test]
    fn extract_socket_stdout_surfaces_stderr_when_stdout_is_empty() {
        let output = CommandOutput {
            exit_code: 0,
            stdout: String::new(),
            stderr: "sh: nc: command not found".to_string(),
        };
        let err = SshClient::extract_socket_stdout(output).unwrap_err();
        assert!(err.to_string().contains("nc: command not found"));
    }

    #[test]
    fn extract_socket_stdout_errors_on_empty_output() {
        let output = CommandOutput {
            exit_code: 0,
            stdout: "\n".to_string(),
            stderr: String::new(),
        };
        let err = SshClient::extract_socket_stdout(output).unwrap_err();
        assert!(err.to_string().contains("empty response"));
    }

    #[test]
    fn socket_request_command_reads_one_line_and_escapes_payload() {
        let command = SshClient::socket_request_command("{\"k\":\"it's\"}");
        assert!(command.contains("| head -n 1"));
        assert!(command.contains("it'\\''s"));
        assert!(command.starts_with("printf '%s\\n'"));
    }

    #[test]
    fn socket_request_command_on_path_uses_custom_socket() {
        let command = SshClient::socket_request_command_on_path(
            "/tmp/tako-next.sock",
            "{\"command\":\"list\"}",
        );
        assert!(command.contains("nc -U '/tmp/tako-next.sock'"));
        assert!(command.contains("| head -n 1"));
    }

    #[tokio::test]
    async fn connect_to_unreachable_host_fails_quickly() {
        let mut cfg = SshConfig::from_server("10.255.255.1", 22);
        cfg.timeout = Duration::from_millis(200);
        let mut client = SshClient::new(cfg);

        let start = std::time::Instant::now();
        let err = client.connect().await.unwrap_err();
        assert!(start.elapsed() < Duration::from_secs(2));

        // Depending on platform/network, this can be a timeout or immediate connect failure.
        match err {
            SshError::Timeout(_) | SshError::Connection(_) => {}
            other => panic!("unexpected error: {}", other),
        }
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn encrypted_keyfile_authenticates_with_passphrase_env_var() {
        use russh::Channel;
        use russh::keys::{Algorithm, PrivateKey};
        use russh::server::{Server as _, Session};
        use std::sync::Arc;
        use tempfile::TempDir;

        if !can_bind_localhost() {
            return;
        }

        let keys_dir = TempDir::new().expect("temp keys dir");
        let key_path = keys_dir.path().join("id_ed25519");
        std::fs::write(&key_path, ENCRYPTED_ED25519_KEY).expect("write key file");
        // This should be private to satisfy OpenSSH conventions (and some parsers).
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600))
                .expect("chmod key file");
        }

        let client_key = load_secret_key(&key_path, Some("testpass")).expect("load encrypted key");
        let allowed_key = client_key.public_key().clone();

        #[derive(Clone)]
        struct TestServer {
            allowed_key: russh::keys::PublicKey,
        }

        impl russh::server::Server for TestServer {
            type Handler = Self;

            fn new_client(&mut self, _: Option<std::net::SocketAddr>) -> Self::Handler {
                self.clone()
            }
        }

        impl russh::server::Handler for TestServer {
            type Error = russh::Error;

            fn auth_publickey(
                &mut self,
                _user: &str,
                key: &russh::keys::PublicKey,
            ) -> impl Future<Output = Result<russh::server::Auth, Self::Error>> + Send {
                let accepted = key.key_data() == self.allowed_key.key_data();
                async move {
                    if accepted {
                        Ok(russh::server::Auth::Accept)
                    } else {
                        Ok(russh::server::Auth::reject())
                    }
                }
            }

            fn channel_open_session(
                &mut self,
                channel: Channel<russh::server::Msg>,
                _session: &mut Session,
            ) -> impl Future<Output = Result<bool, Self::Error>> + Send {
                let _ = channel.id();
                async { Ok(true) }
            }
        }

        let mut rng = russh::keys::ssh_key::rand_core::OsRng;
        let host_key = PrivateKey::random(&mut rng, Algorithm::Ed25519).expect("host key");

        let server_config = russh::server::Config {
            auth_rejection_time: Duration::from_millis(0),
            auth_rejection_time_initial: Some(Duration::from_millis(0)),
            inactivity_timeout: Some(Duration::from_secs(5)),
            keys: vec![host_key],
            ..Default::default()
        };
        let server_config = Arc::new(server_config);

        let Ok(listener) = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await else {
            return;
        };
        let port = listener.local_addr().expect("local addr").port();

        let mut server = TestServer { allowed_key };
        let server_task = tokio::spawn(async move {
            server
                .run_on_socket(server_config, &listener)
                .await
                .expect("server failed");
        });

        let prev_pass = std::env::var("TAKO_SSH_KEY_PASSPHRASE").ok();
        let prev_sock = std::env::var("SSH_AUTH_SOCK").ok();
        // SAFETY: tests in this crate are not expected to rely on concurrent env var mutation.
        unsafe { std::env::set_var("TAKO_SSH_KEY_PASSPHRASE", "testpass") };
        // Ensure we don't accidentally use an agent in this test.
        unsafe { std::env::remove_var("SSH_AUTH_SOCK") };

        let mut ssh_config = SshConfig::from_server("127.0.0.1", port);
        ssh_config.timeout = Duration::from_secs(5);
        ssh_config.keys_dir = Some(keys_dir.path().to_path_buf());

        let mut ssh = SshClient::new(ssh_config);
        tokio::time::timeout(Duration::from_secs(10), ssh.connect())
            .await
            .expect("connect timed out")
            .expect("encrypted key auth should work");
        ssh.disconnect().await.expect("disconnect");

        // Cleanup.
        server_task.abort();
        match prev_pass {
            Some(v) => unsafe { std::env::set_var("TAKO_SSH_KEY_PASSPHRASE", v) },
            None => unsafe { std::env::remove_var("TAKO_SSH_KEY_PASSPHRASE") },
        }
        match prev_sock {
            Some(v) => unsafe { std::env::set_var("SSH_AUTH_SOCK", v) },
            None => unsafe { std::env::remove_var("SSH_AUTH_SOCK") },
        }
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn ssh_agent_authenticates_when_no_key_files_exist() {
        use russh::Channel;
        use russh::keys::agent::client::AgentClient;
        use russh::keys::{Algorithm, PrivateKey};
        use russh::server::{Server as _, Session};
        use std::process::Stdio;
        use std::sync::Arc;
        use tempfile::TempDir;

        if !can_bind_localhost() || !can_bind_unix_socket() {
            return;
        }

        #[derive(Clone)]
        struct TestServer {
            allowed_key: russh::keys::PublicKey,
        }

        impl russh::server::Server for TestServer {
            type Handler = Self;

            fn new_client(&mut self, _: Option<std::net::SocketAddr>) -> Self::Handler {
                self.clone()
            }
        }

        impl russh::server::Handler for TestServer {
            type Error = russh::Error;

            fn auth_publickey(
                &mut self,
                _user: &str,
                key: &russh::keys::PublicKey,
            ) -> impl Future<Output = Result<russh::server::Auth, Self::Error>> + Send {
                let accepted = key.key_data() == self.allowed_key.key_data();
                async move {
                    if accepted {
                        Ok(russh::server::Auth::Accept)
                    } else {
                        Ok(russh::server::Auth::reject())
                    }
                }
            }

            fn channel_open_session(
                &mut self,
                channel: Channel<russh::server::Msg>,
                _session: &mut Session,
            ) -> impl Future<Output = Result<bool, Self::Error>> + Send {
                // We don't need to run any commands for this test; just allow opening.
                let _ = channel.id();
                async { Ok(true) }
            }
        }

        // Start a private ssh-agent with a temporary socket (daemonized by ssh-agent).
        let agent_dir = TempDir::new().expect("tempdir");
        let agent_path = agent_dir.path().join("agent.sock");
        let agent_out = tokio::process::Command::new("ssh-agent")
            .arg("-a")
            .arg(&agent_path)
            .arg("-s")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await
            .expect("start ssh-agent");
        if !agent_out.status.success() {
            return;
        }
        let agent_stdout = String::from_utf8_lossy(&agent_out.stdout);
        let Some(pid) = agent_stdout
            .split(';')
            .find_map(|part| part.trim().strip_prefix("SSH_AGENT_PID="))
            .and_then(|v| v.parse::<u32>().ok())
        else {
            return;
        };

        // Generate a client key and load it into the agent.
        let mut rng = russh::keys::ssh_key::rand_core::OsRng;
        let client_key = PrivateKey::random(&mut rng, Algorithm::Ed25519).expect("client key");
        let client_pub = client_key.public_key().clone();

        let stream = tokio::net::UnixStream::connect(&agent_path)
            .await
            .expect("connect to agent");
        let mut agent = AgentClient::connect(stream);
        agent
            .add_identity(&client_key, &[])
            .await
            .expect("add identity");

        // Point SSH_AUTH_SOCK at the test agent so SshClient can find it.
        let prev_sock = std::env::var("SSH_AUTH_SOCK").ok();
        // SAFETY: tests in this crate are not expected to rely on concurrent env var mutation.
        unsafe { std::env::set_var("SSH_AUTH_SOCK", &agent_path) };

        // Start an SSH server that accepts only the agent-loaded public key.
        let mut rng = russh::keys::ssh_key::rand_core::OsRng;
        let host_key = PrivateKey::random(&mut rng, Algorithm::Ed25519).expect("host key");

        let server_config = russh::server::Config {
            auth_rejection_time: Duration::from_millis(0),
            auth_rejection_time_initial: Some(Duration::from_millis(0)),
            inactivity_timeout: Some(Duration::from_secs(5)),
            keys: vec![host_key],
            ..Default::default()
        };
        let server_config = Arc::new(server_config);

        let Ok(listener) = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await else {
            let _ = tokio::process::Command::new("kill")
                .arg(pid.to_string())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .await;
            return;
        };
        let port = listener.local_addr().expect("local addr").port();

        let mut server = TestServer {
            allowed_key: client_pub,
        };

        let server_task = tokio::spawn(async move {
            server
                .run_on_socket(server_config, &listener)
                .await
                .expect("server failed");
        });

        // Ensure we don't find any key files on disk.
        let keys_dir = TempDir::new().expect("temp keys dir");
        let mut ssh_config = SshConfig::from_server("127.0.0.1", port);
        ssh_config.timeout = Duration::from_secs(5);
        ssh_config.keys_dir = Some(keys_dir.path().to_path_buf());

        let mut ssh = SshClient::new(ssh_config);
        tokio::time::timeout(Duration::from_secs(10), ssh.connect())
            .await
            .expect("connect timed out")
            .expect("agent auth should work");
        ssh.disconnect().await.expect("disconnect");

        // Cleanup.
        server_task.abort();
        let _ = tokio::process::Command::new("kill")
            .arg(pid.to_string())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await;

        if let Some(prev) = prev_sock {
            // SAFETY: see note above.
            unsafe { std::env::set_var("SSH_AUTH_SOCK", prev) };
        } else {
            // SAFETY: see note above.
            unsafe { std::env::remove_var("SSH_AUTH_SOCK") };
        }
    }
}

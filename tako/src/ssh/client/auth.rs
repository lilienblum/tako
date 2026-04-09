use std::io::IsTerminal;
use std::path::Path;
use std::sync::Arc;

use russh::keys::{Algorithm, PrivateKeyWithHashAlg, load_secret_key};

use super::*;

impl SshClient {
    pub(super) async fn authenticate(&self, handle: &mut Handle<SshHandler>) -> SshResult<()> {
        let keys_dir = self.config.keys_directory();

        let key_names = ["id_ed25519", "id_rsa", "id_ecdsa", "id_dsa"];

        let mut last_error = None;
        let mut found_any_key_file = false;

        for key_name in &key_names {
            let key_path = keys_dir.join(key_name);

            if !key_path.exists() {
                continue;
            }

            found_any_key_file = true;

            match self.try_key_auth(handle, &key_path).await {
                Ok(true) => {
                    return Ok(());
                }
                Ok(false) => {
                    tracing::trace!("Key not accepted ({key_name})");
                }
                Err(e) => {
                    tracing::trace!("Key auth failed ({key_name}): {e}");
                    last_error = Some(e);
                }
            }
        }

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

            for identity in keys {
                match handle
                    .authenticate_publickey_with(
                        Self::ssh_user(),
                        identity.public_key().into_owned(),
                        None,
                        &mut agent,
                    )
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

    async fn try_key_auth(
        &self,
        handle: &mut Handle<SshHandler>,
        key_path: &Path,
    ) -> SshResult<bool> {
        let key = match load_secret_key(key_path, None) {
            Ok(k) => k,
            Err(e) => {
                let pass = std::env::var("TAKO_SSH_KEY_PASSPHRASE").ok().or_else(|| {
                    if std::io::stdin().is_terminal() {
                        crate::output::TextField::new(&format!(
                            "SSH key passphrase for {}",
                            key_path.display()
                        ))
                        .password()
                        .optional()
                        .prompt()
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
        "tako"
    }
}

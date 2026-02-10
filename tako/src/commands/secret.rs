use crate::output;
use clap::Subcommand;

#[derive(Subcommand)]
pub enum SecretCommands {
    /// Set a secret (creates or overwrites)
    Set {
        /// Secret name (uppercase, underscores)
        name: String,

        /// Environment to set the secret for
        #[arg(long, default_value = "production")]
        env: String,
    },

    /// Remove a secret
    #[command(visible_aliases = ["remove", "delete"])]
    Rm {
        /// Secret name
        name: String,

        /// Environment to remove from (or all if not specified)
        #[arg(long)]
        env: Option<String>,
    },

    /// List all secrets
    #[command(visible_alias = "list")]
    Ls,

    /// Sync secrets to servers
    Sync {
        /// Only sync to specific environment
        #[arg(long)]
        env: Option<String>,
    },

    /// Manage encryption keys used for secrets
    #[command(subcommand)]
    Key(SecretKeyCommands),
}

#[derive(Subcommand)]
pub enum SecretKeyCommands {
    /// Import a base64 key from masked terminal input
    Import {
        /// Target environment key (defaults to production when omitted)
        #[arg(long)]
        env: Option<String>,
    },

    /// Export a base64 key and copy it to clipboard
    Export {
        /// Target environment key (defaults to production when omitted)
        #[arg(long)]
        env: Option<String>,
    },
}

pub fn run(cmd: SecretCommands) -> Result<(), Box<dyn std::error::Error>> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(run_async(cmd))
}

fn read_secret_value(prompt: &str) -> Result<String, Box<dyn std::error::Error>> {
    use std::io::IsTerminal;

    if std::io::stdin().is_terminal() {
        return Ok(crate::output::prompt_password(prompt, false)?);
    }

    // Non-interactive fallback for CI/piped input.
    let mut value = String::new();
    let bytes = std::io::stdin().read_line(&mut value)?;
    if bytes == 0 {
        return Err("No secret value provided on stdin".into());
    }
    let value = value.trim_end_matches(['\r', '\n']).to_string();
    if value.is_empty() {
        return Err("Secret value cannot be empty".into());
    }

    Ok(value)
}

async fn run_async(cmd: SecretCommands) -> Result<(), Box<dyn std::error::Error>> {
    match cmd {
        SecretCommands::Set { name, env } => set_secret(&name, &env).await,
        SecretCommands::Rm { name, env } => remove_secret(&name, env.as_deref()).await,
        SecretCommands::Ls => list_secrets().await,
        SecretCommands::Sync { env } => sync_secrets(env.as_deref()).await,
        SecretCommands::Key(SecretKeyCommands::Import { env }) => import_key(env.as_deref()).await,
        SecretCommands::Key(SecretKeyCommands::Export { env }) => export_key(env.as_deref()).await,
    }
}

async fn set_secret(name: &str, env: &str) -> Result<(), Box<dyn std::error::Error>> {
    use crate::config::SecretsStore;
    use crate::crypto::encrypt;
    use std::env::current_dir;

    // Get an environment-scoped key.
    let key = load_or_create_key_for_env(env)?;

    // Load secrets from project directory
    let project_dir = current_dir()?;
    let mut secrets = SecretsStore::load_from_dir(&project_dir)?;

    // Check if secret exists
    let exists = secrets.contains(env, name);

    // Prompt for value
    let prompt = if exists {
        format!("Enter new value for {} ({})", name, env)
    } else {
        format!("Enter value for {} ({})", name, env)
    };

    let value = read_secret_value(&prompt)?;

    // Encrypt and store
    let encrypted = encrypt(&value, &key)?;
    secrets.set(env, name, encrypted)?;
    secrets.save_to_dir(&project_dir)?;

    if exists {
        output::success(&format!(
            "Updated secret {} for environment {}",
            output::emphasized(name),
            output::emphasized(env)
        ));
    } else {
        output::success(&format!(
            "Set secret {} for environment {}",
            output::emphasized(name),
            output::emphasized(env)
        ));
    }

    output::muted(&format!(
        "Note: Run {} to push secrets to servers.",
        output::emphasized("tako secrets sync")
    ));

    Ok(())
}

async fn remove_secret(name: &str, env: Option<&str>) -> Result<(), Box<dyn std::error::Error>> {
    use crate::config::SecretsStore;
    use std::env::current_dir;

    let project_dir = current_dir()?;
    let mut secrets = SecretsStore::load_from_dir(&project_dir)?;

    if let Some(env) = env {
        // Remove from specific environment
        if !secrets.contains(env, name) {
            return Err(format!("Secret '{}' not found in environment '{}'", name, env).into());
        }

        let confirm = crate::output::confirm(
            &format!(
                "Remove secret {} from {}?",
                output::emphasized(name),
                output::emphasized(env)
            ),
            false,
        )?;

        if !confirm {
            output::warning("Cancelled.");
            return Ok(());
        }

        secrets.remove(env, name)?;
        output::success(&format!(
            "Removed secret {} from environment {}",
            output::emphasized(name),
            output::emphasized(env)
        ));
    } else {
        // Remove from all environments
        let confirm = crate::output::confirm(
            &format!(
                "Remove secret {} from ALL environments?",
                output::emphasized(name)
            ),
            false,
        )?;

        if !confirm {
            output::warning("Cancelled.");
            return Ok(());
        }

        let removed_from = secrets.remove_all(name)?;
        output::success(&format!(
            "Removed secret {} from environments: {}",
            output::emphasized(name),
            removed_from.join(", ")
        ));
    }

    secrets.save_to_dir(&project_dir)?;

    output::muted(&format!(
        "Note: Run {} to update servers.",
        output::emphasized("tako secrets sync")
    ));

    Ok(())
}

async fn list_secrets() -> Result<(), Box<dyn std::error::Error>> {
    use crate::config::SecretsStore;
    use std::env::current_dir;

    let project_dir = current_dir()?;
    let secrets = SecretsStore::load_from_dir(&project_dir)?;

    if secrets.is_empty() {
        output::warning("No secrets configured.");
        output::muted(&format!(
            "Run {} to add a secret.",
            output::emphasized("tako secrets set --env <env> <NAME>")
        ));
        return Ok(());
    }

    output::section("Secrets");

    let all_names = secrets.all_secret_names();
    let all_envs = secrets.environment_names();

    // Print header
    print!("{:<30}", "SECRET");
    for env in &all_envs {
        print!(" {:<15}", env.to_uppercase());
    }
    println!();

    print!("{}", "-".repeat(30));
    for _ in &all_envs {
        print!(" {}", "-".repeat(15));
    }
    println!();

    // Print each secret
    let discrepancies = secrets.find_discrepancies();
    let discrepancy_names: Vec<&str> = discrepancies.iter().map(|d| d.name.as_str()).collect();

    for name in &all_names {
        print!("{:<30}", name);
        for env in &all_envs {
            if secrets.contains(env, name) {
                print!(" {:<15}", "[set]");
            } else {
                print!(" {:<15}", "-");
            }
        }

        // Show warning if this secret has discrepancies
        if discrepancy_names.contains(&name.as_str()) {
            print!(" (missing in some envs)");
        }

        println!();
    }

    // Summary
    if !discrepancies.is_empty() {
        output::warning(&format!(
            "{} secret(s) have discrepancies across environments.",
            discrepancies.len()
        ));
        output::muted(&format!(
            "Run {} to sync secrets to servers.",
            output::emphasized("tako secrets sync")
        ));
    }

    Ok(())
}

async fn sync_secrets(target_env: Option<&str>) -> Result<(), Box<dyn std::error::Error>> {
    use crate::app::resolve_app_name;
    use crate::commands::server;
    use crate::config::{SecretsStore, ServersToml, TakoToml};
    use crate::crypto::decrypt;
    use std::env::current_dir;

    let project_dir = current_dir()?;
    let secrets = SecretsStore::load_from_dir(&project_dir)?;
    let tako_config = TakoToml::load_from_dir(&project_dir)?;
    let mut servers = ServersToml::load()?;

    if secrets.is_empty() {
        output::warning("No secrets to sync.");
        return Ok(());
    }

    if servers.is_empty()
        && server::prompt_to_add_server("No servers configured yet. Add one now to sync secrets.")
            .await?
            .is_some()
    {
        servers = ServersToml::load()?;
    }

    // Resolve app name
    let app_name = resolve_app_name(&project_dir).unwrap_or_else(|_| "app".to_string());

    // Check for discrepancies first
    let discrepancies = secrets.find_discrepancies();
    if !discrepancies.is_empty() {
        output::warning("Some secrets are missing in certain environments:");
        for d in &discrepancies {
            output::warning(&format!(
                "{} missing in: {}",
                d.name,
                d.missing_in.join(", ")
            ));
        }
    }

    // Determine which environments to sync
    let envs_to_sync: Vec<String> = if let Some(env) = target_env {
        if !tako_config.envs.contains_key(env) {
            return Err(format!("Environment '{}' not found in tako.toml", env).into());
        }
        vec![env.to_string()]
    } else {
        tako_config.get_environment_names()
    };

    let mut success_count = 0;
    let mut error_count = 0;

    // Sync to each environment
    for env_name in &envs_to_sync {
        let server_names = resolve_secret_sync_server_names(env_name, &tako_config, &servers)
            .map_err(|e| {
                format!(
                    "Failed to resolve target servers for environment '{}': {}",
                    env_name, e
                )
            })?;

        if server_names.is_empty() {
            output::warning(&format!(
                "Skipping {} - no servers configured",
                output::emphasized(env_name)
            ));
            continue;
        }

        output::section(&format!("Sync {}", output::emphasized(env_name)));

        // Get decrypted secrets for this environment
        let env_secrets = match secrets.get_env(env_name) {
            Some(encrypted_secrets) => {
                let key = load_key_for_env(env_name)?;
                let mut decrypted = std::collections::HashMap::new();
                for (name, encrypted_value) in encrypted_secrets {
                    match decrypt(encrypted_value, &key) {
                        Ok(value) => {
                            decrypted.insert(name.clone(), value);
                        }
                        Err(e) => {
                            output::warning(&format!(
                                "Failed to decrypt {}: {}",
                                output::emphasized(name),
                                e
                            ));
                        }
                    }
                }
                decrypted
            }
            None => {
                output::warning("No secrets for this environment");
                continue;
            }
        };

        if env_secrets.is_empty() {
            output::warning("No secrets to sync.");
            continue;
        }

        for server_name in server_names {
            let server = match servers.get(server_name.as_str()) {
                Some(s) => s,
                None => {
                    output::error(&format!(
                        "{} - server not found in ~/.tako/config.toml [[servers]]",
                        server_name
                    ));
                    error_count += 1;
                    continue;
                }
            };

            let sync_result = output::with_spinner_async(
                format!("Syncing to {}", output::emphasized(&server_name)),
                sync_to_server(&app_name, server, &env_secrets),
            )
            .await?;

            match sync_result {
                Ok(()) => {
                    output::success(&format!("Synced {}", output::emphasized(&server_name)));
                    success_count += 1;
                }
                Err(e) => {
                    output::error(&format!("FAILED: {} ({})", e, server_name));
                    error_count += 1;
                }
            }
        }
    }

    if error_count == 0 {
        output::success(&format!(
            "Synced secrets to {} server(s) successfully.",
            success_count
        ));
    } else {
        output::warning(&format!(
            "Synced to {} server(s), {} failed.",
            success_count, error_count
        ));
    }

    Ok(())
}

fn resolve_secret_sync_server_names(
    env_name: &str,
    tako_config: &crate::config::TakoToml,
    servers: &crate::config::ServersToml,
) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let mapped: Vec<String> = tako_config
        .get_servers_for_env(env_name)
        .into_iter()
        .map(|name| name.to_string())
        .collect();
    if !mapped.is_empty() {
        return Ok(mapped);
    }

    if env_name == "production" && servers.len() == 1 {
        let only = servers.names().into_iter().next().unwrap_or("<server>");
        if output::confirm(
            &format!(
                "No [servers.*] mapping for 'production'. Sync secrets to the only configured server ('{}')?",
                only
            ),
            true,
        )? {
            return Ok(vec![only.to_string()]);
        }
    }

    Ok(Vec::new())
}

async fn import_key(target_env: Option<&str>) -> Result<(), Box<dyn std::error::Error>> {
    use crate::crypto::EncryptionKey;

    let env = target_env.unwrap_or("production");
    let key_store = crate::crypto::KeyStore::for_env(env)?;

    let prompt = format!("Enter base64 key for environment '{}'", env);

    let encoded = crate::output::prompt_password(&prompt, false)?;
    let confirm = crate::output::prompt_password("Confirm key", false)?;
    if encoded != confirm {
        return Err("Keys do not match".into());
    }

    let key = EncryptionKey::from_base64(encoded.trim())?;
    key_store.save_key(&key)?;

    output::success(&format!(
        "Imported key for environment {}.",
        output::emphasized(env)
    ));

    Ok(())
}

async fn export_key(target_env: Option<&str>) -> Result<(), Box<dyn std::error::Error>> {
    let env = target_env.unwrap_or("production");
    let key_store = crate::crypto::KeyStore::for_env(env)?;

    if !key_store.key_exists() {
        return Err(format!("No key found for environment '{}'.", env).into());
    }

    let key = key_store.load_key()?;
    copy_to_clipboard(&key.to_base64())?;

    output::success(&format!(
        "Copied key for environment {} to clipboard.",
        output::emphasized(env)
    ));

    Ok(())
}

fn load_or_create_key_for_env(
    env: &str,
) -> Result<crate::crypto::EncryptionKey, Box<dyn std::error::Error>> {
    let env_store = crate::crypto::KeyStore::for_env(env)?;
    if env_store.key_exists() {
        return Ok(env_store.load_key()?);
    }

    Ok(env_store.get_or_create_key()?)
}

fn load_key_for_env(env: &str) -> Result<crate::crypto::EncryptionKey, Box<dyn std::error::Error>> {
    let env_store = crate::crypto::KeyStore::for_env(env)?;
    if env_store.key_exists() {
        return Ok(env_store.load_key()?);
    }

    Err(format!(
        "No key found for environment '{}'. Import one with 'tako secrets key import --env {}'",
        env, env
    )
    .into())
}

fn copy_to_clipboard(text: &str) -> Result<(), Box<dyn std::error::Error>> {
    if text.is_empty() {
        return Err("Cannot copy empty key".into());
    }

    #[cfg(target_os = "macos")]
    {
        copy_to_clipboard_command("pbcopy", &[], text)
    }

    #[cfg(target_os = "linux")]
    {
        for (cmd, args) in [
            ("wl-copy", &[][..]),
            ("xclip", &["-selection", "clipboard"][..]),
            ("xsel", &["--clipboard", "--input"][..]),
        ] {
            if copy_to_clipboard_command(cmd, args, text).is_ok() {
                return Ok(());
            }
        }

        Err("Failed to copy key to clipboard (tried wl-copy, xclip, xsel).".into())
    }

    #[cfg(target_os = "windows")]
    {
        copy_to_clipboard_command("clip", &[], text)
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        let _ = text;
        return Err("Clipboard export is not supported on this platform".into());
    }
}

fn copy_to_clipboard_command(
    cmd: &str,
    args: &[&str],
    text: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    use std::io::Write;
    use std::process::{Command, Stdio};

    let mut child = Command::new(cmd).args(args).stdin(Stdio::piped()).spawn()?;

    let mut stdin = child
        .stdin
        .take()
        .ok_or("Failed to open clipboard process stdin")?;
    stdin.write_all(text.as_bytes())?;
    drop(stdin);

    let status = child.wait()?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("Clipboard command '{}' failed", cmd).into())
    }
}

async fn sync_to_server(
    app_name: &str,
    server: &crate::config::ServerEntry,
    secrets: &std::collections::HashMap<String, String>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use crate::ssh::{SshClient, SshConfig};

    // Connect
    let ssh_config = SshConfig::from_server(&server.host, server.port);
    let mut ssh = SshClient::new(ssh_config);
    ssh.connect().await?;

    // Build .env content
    let mut env_content = String::new();
    for (key, value) in secrets {
        // Escape values for .env format
        let escaped = value.replace("\\", "\\\\").replace("\"", "\\\"");
        env_content.push_str(&format!("{}=\"{}\"\n", key, escaped));
    }

    // Write to server
    let env_file = format!("/opt/tako/apps/{}/shared/.env", app_name);

    // Ensure directory exists
    ssh.mkdir(&format!("/opt/tako/apps/{}/shared", app_name))
        .await?;

    // Write using base64 to safely transfer content
    let encoded = base64::Engine::encode(
        &base64::engine::general_purpose::STANDARD,
        env_content.as_bytes(),
    );

    ssh.exec_checked(&format!(
        "echo '{}' | base64 -d > {} && chmod 600 {}",
        encoded, env_file, env_file
    ))
    .await?;

    // Reload app if running
    let reload_cmd = serde_json::json!({
        "Reload": {
            "app": app_name
        }
    });

    // Try to reload, but don't fail if app isn't running
    let _ = ssh.tako_command(&reload_cmd.to_string()).await;

    ssh.disconnect().await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ServerEntry, ServersToml, TakoToml};
    use std::sync::{Mutex, OnceLock};
    use tempfile::TempDir;

    fn with_temp_tako_home<T>(f: impl FnOnce(&std::path::Path) -> T) -> T {
        static TAKO_HOME_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        let _lock = TAKO_HOME_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .expect("lock poisoned");

        let temp = TempDir::new().expect("temp dir");
        unsafe {
            std::env::set_var("TAKO_HOME", temp.path());
        }

        struct ResetEnv;
        impl Drop for ResetEnv {
            fn drop(&mut self) {
                unsafe {
                    std::env::remove_var("TAKO_HOME");
                }
            }
        }
        let _reset = ResetEnv;

        f(temp.path())
    }

    #[test]
    fn resolve_secret_sync_server_names_uses_single_production_server_fallback() {
        let tako_config = TakoToml::default();
        let mut servers = ServersToml::default();
        servers.servers.insert(
            "solo".to_string(),
            ServerEntry {
                host: "127.0.0.1".to_string(),
                port: 22,
                description: None,
            },
        );

        let names = resolve_secret_sync_server_names("production", &tako_config, &servers)
            .expect("should resolve");
        assert_eq!(names, vec!["solo".to_string()]);
    }

    #[test]
    fn resolve_secret_sync_server_names_returns_empty_for_unmapped_non_production() {
        let tako_config = TakoToml::default();
        let mut servers = ServersToml::default();
        servers.servers.insert(
            "solo".to_string(),
            ServerEntry {
                host: "127.0.0.1".to_string(),
                port: 22,
                description: None,
            },
        );

        let names = resolve_secret_sync_server_names("staging", &tako_config, &servers)
            .expect("should work");
        assert!(names.is_empty());
    }

    #[test]
    fn load_or_create_key_for_env_ignores_legacy_global_key() {
        with_temp_tako_home(|home| {
            let legacy_store = crate::crypto::KeyStore::with_path(home.join("key"));
            let legacy_key = crate::crypto::EncryptionKey::generate();
            legacy_store
                .save_key(&legacy_key)
                .expect("save legacy global key");

            let loaded = load_or_create_key_for_env("production").expect("load env key");
            let env_store = crate::crypto::KeyStore::for_env("production").expect("env key store");
            assert!(env_store.key_exists(), "env key should be created");

            let env_key = env_store.load_key().expect("load env key");
            assert_eq!(loaded.as_bytes(), env_key.as_bytes());
            assert_ne!(loaded.as_bytes(), legacy_key.as_bytes());
        });
    }

    #[test]
    fn load_key_for_env_fails_when_only_legacy_global_key_exists() {
        with_temp_tako_home(|home| {
            let legacy_store = crate::crypto::KeyStore::with_path(home.join("key"));
            legacy_store
                .save_key(&crate::crypto::EncryptionKey::generate())
                .expect("save legacy global key");

            match load_key_for_env("production") {
                Ok(_) => panic!("should require env key"),
                Err(err) => assert!(
                    err.to_string()
                        .contains("No key found for environment 'production'"),
                    "unexpected error: {err}"
                ),
            }
        });
    }
}

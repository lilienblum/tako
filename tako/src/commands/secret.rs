use crate::output;
use clap::Subcommand;
use tako_core::Command;

#[derive(Subcommand)]
pub enum SecretCommands {
    /// Set a secret (creates or overwrites)
    #[command(visible_alias = "add")]
    Set {
        /// Secret name (uppercase, underscores)
        name: String,

        /// Environment to set the secret for (defaults to production)
        #[arg(long)]
        env: Option<String>,

        /// Sync secrets to servers after setting
        #[arg(long)]
        sync: bool,
    },

    /// Remove a secret
    #[command(visible_aliases = ["remove", "delete", "del"])]
    Rm {
        /// Secret name
        name: String,

        /// Environment to remove from (or all if not specified)
        #[arg(long)]
        env: Option<String>,

        /// Sync secrets to servers after removing
        #[arg(long)]
        sync: bool,
    },

    /// List all secrets
    #[command(visible_aliases = ["list", "show"])]
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
    /// Derive a key from a passphrase (for sharing with teammates)
    Derive {
        /// Target environment (defaults to production when omitted)
        #[arg(long)]
        env: Option<String>,
    },

    /// Export the derived key as base64 and copy it to clipboard
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
        return Ok(crate::output::password_field(prompt)?);
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
        SecretCommands::Set { name, env, sync } => {
            let env = super::helpers::resolve_env(env.as_deref());
            set_secret(&name, &env, sync).await
        }
        SecretCommands::Rm { name, env, sync } => remove_secret(&name, env.as_deref(), sync).await,
        SecretCommands::Ls => list_secrets().await,
        SecretCommands::Sync { env } => sync_secrets(env.as_deref()).await,
        SecretCommands::Key(SecretKeyCommands::Derive { env }) => {
            derive_key(env.as_deref()).await
        }
        SecretCommands::Key(SecretKeyCommands::Export { env }) => export_key(env.as_deref()).await,
    }
}

async fn set_secret(
    name: &str,
    env: &str,
    do_sync: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    use crate::config::SecretsStore;
    use crate::crypto::encrypt;
    use std::env::current_dir;

    let project_dir = current_dir()?;
    let app_name = resolve_app_name(&project_dir)?;

    // Load secrets and ensure environment has a salt
    let mut secrets = SecretsStore::load_from_dir(&project_dir)?;
    secrets.ensure_env_salt(env)?;
    secrets.save_to_dir(&project_dir)?;

    // Get the encryption key (prompts for passphrase if no local key)
    let key = load_or_derive_key(&app_name, env, &secrets)?;

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
            output::strong(name),
            output::strong(env)
        ));
    } else {
        output::success(&format!(
            "Set secret {} for environment {}",
            output::strong(name),
            output::strong(env)
        ));
    }

    if do_sync {
        sync_secrets(Some(env)).await?;
    }

    Ok(())
}

async fn remove_secret(
    name: &str,
    env: Option<&str>,
    do_sync: bool,
) -> Result<(), Box<dyn std::error::Error>> {
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
                output::strong(name),
                output::strong(env)
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
            output::strong(name),
            output::strong(env)
        ));
    } else {
        // Remove from all environments
        let confirm = crate::output::confirm(
            &format!(
                "Remove secret {} from ALL environments?",
                output::strong(name)
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
            output::strong(name),
            removed_from.join(", ")
        ));
    }

    secrets.save_to_dir(&project_dir)?;

    if do_sync {
        // Sync to the specific env if provided, otherwise all environments
        sync_secrets(env).await?;
    }

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
            output::strong("tako secrets set")
        ));
        return Ok(());
    }

    output::section("Secrets");

    let all_names = secrets.all_secret_names();
    let all_envs = secrets.environment_names();

    let discrepancies = secrets.find_discrepancies();

    if output::is_pretty() {
        // Print header
        eprint!("{:<30}", "SECRET");
        for env in &all_envs {
            eprint!(" {:<15}", env.to_uppercase());
        }
        eprintln!();

        eprint!("{}", "-".repeat(30));
        for _ in &all_envs {
            eprint!(" {}", "-".repeat(15));
        }
        eprintln!();

        // Print each secret
        let discrepancy_names: Vec<&str> = discrepancies.iter().map(|d| d.name.as_str()).collect();

        for name in &all_names {
            eprint!("{:<30}", name);
            for env in &all_envs {
                if secrets.contains(env, name) {
                    eprint!(" {:<15}", "[set]");
                } else {
                    eprint!(" {:<15}", "-");
                }
            }

            // Show warning if this secret has discrepancies
            if discrepancy_names.contains(&name.as_str()) {
                eprint!(" (missing in some envs)");
            }

            eprintln!();
        }
    } else {
        for name in &all_names {
            let envs_with_secret: Vec<&str> = all_envs
                .iter()
                .filter(|env| secrets.contains(env, name))
                .map(|s| s.as_str())
                .collect();
            tracing::info!("{name}: set in {}", envs_with_secret.join(", "));
        }
    }

    // Summary
    if !discrepancies.is_empty() {
        output::warning(&format!(
            "{} secret(s) have discrepancies across environments.",
            output::strong(&discrepancies.len().to_string())
        ));
        output::muted(&format!(
            "Run {} to sync secrets to servers.",
            output::strong("tako secrets sync")
        ));
    }

    Ok(())
}

async fn sync_secrets(target_env: Option<&str>) -> Result<(), Box<dyn std::error::Error>> {
    use crate::config::{SecretsStore, ServersToml, TakoToml};
    use crate::crypto::decrypt;
    use std::env::current_dir;

    let project_dir = current_dir()?;
    let app_name = resolve_app_name(&project_dir)?;
    let secrets = SecretsStore::load_from_dir(&project_dir)?;
    let tako_config = TakoToml::load_from_dir(&project_dir)?;
    let mut servers = ServersToml::load()?;

    if secrets.is_empty() {
        output::warning("No secrets to sync.");
        return Ok(());
    }

    if servers.is_empty()
        && super::server::prompt_to_add_server(
            "No servers configured yet. Add one now to sync secrets.",
        )
        .await?
        .is_some()
    {
        servers = ServersToml::load()?;
    }

    // Check for discrepancies first
    let discrepancies = secrets.find_discrepancies();
    if !discrepancies.is_empty() {
        output::warning("Some secrets are missing in certain environments:");
        for d in &discrepancies {
            output::warning(&format!(
                "{} missing in: {}",
                output::strong(&d.name),
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

    // Collect all (env, server_name, server_entry) targets first
    let mut sync_targets: Vec<(String, String, crate::config::ServerEntry)> = Vec::new();
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
                "Skipping {} — no servers configured",
                output::strong(env_name)
            ));
            continue;
        }

        for server_name in server_names {
            let server = match servers.get(server_name.as_str()) {
                Some(s) => s.clone(),
                None => {
                    output::error(&format!(
                        "{} — server not found",
                        output::strong(&server_name)
                    ));
                    continue;
                }
            };
            sync_targets.push((env_name.clone(), server_name, server));
        }
    }

    if sync_targets.is_empty() {
        output::warning("No servers to sync to.");
        return Ok(());
    }

    let total_servers = sync_targets.len();
    let spinner =
        output::TrackedSpinner::start(&format!("Syncing secrets to {total_servers} server(s)…"));
    let sync_start = std::time::Instant::now();

    let mut success_count = 0;
    let mut error_count = 0;

    for (env_name, server_name, server) in &sync_targets {
        let _scope = output::scope(server_name).entered();
        let _t = output::timed(&format!("Sync secrets ({env_name})"));
        // Get decrypted secrets for this environment
        let env_secrets = match secrets.get_env(env_name) {
            Some(encrypted_secrets) => {
                let key = load_or_derive_key(&app_name, env_name, &secrets)?;
                let mut decrypted = std::collections::HashMap::new();
                for (name, encrypted_value) in encrypted_secrets {
                    match decrypt(encrypted_value, &key) {
                        Ok(value) => {
                            decrypted.insert(name.clone(), value);
                        }
                        Err(e) => {
                            output::warning(&format!(
                                "Failed to decrypt {}: {}",
                                output::strong(name),
                                e
                            ));
                        }
                    }
                }
                decrypted
            }
            None => {
                output::warning(&format!(
                    "No secrets for environment {}",
                    output::strong(env_name)
                ));
                continue;
            }
        };

        if env_secrets.is_empty() {
            continue;
        }

        let remote_app_name = tako_core::deployment_app_id(&app_name, env_name);
        match sync_to_server(&remote_app_name, server, &env_secrets).await {
            Ok(()) => {
                tracing::debug!("Synced {} secret(s) for {env_name}", env_secrets.len());
                success_count += 1;
            }
            Err(e) => {
                output::error(&format!("{} ({})", e, output::strong(server_name)));
                error_count += 1;
            }
        }
    }

    let elapsed = sync_start.elapsed();
    spinner.finish();

    if error_count == 0 {
        output::success(&format!(
            "Synced secrets to {} server(s) ({:.1}s)",
            output::strong(&success_count.to_string()),
            elapsed.as_secs_f64()
        ));
    } else {
        output::warning(&format!(
            "Synced to {} server(s), {} failed ({:.1}s)",
            output::strong(&success_count.to_string()),
            output::strong(&error_count.to_string()),
            elapsed.as_secs_f64()
        ));
    }

    Ok(())
}

fn resolve_secret_sync_server_names(
    env_name: &str,
    tako_config: &crate::config::TakoToml,
    servers: &crate::config::ServersToml,
) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let mut resolved = match super::helpers::resolve_servers_for_env(tako_config, servers, env_name)
    {
        Ok(r) => r,
        Err(_) => return Ok(Vec::new()),
    };
    resolved.sort();
    resolved.dedup();
    Ok(resolved)
}

async fn derive_key(target_env: Option<&str>) -> Result<(), Box<dyn std::error::Error>> {
    use crate::config::SecretsStore;
    use std::env::current_dir;

    let project_dir = current_dir()?;
    let app_name = resolve_app_name(&project_dir)?;
    let env = super::helpers::resolve_env(target_env);

    // Load secrets and ensure salt exists
    let mut secrets = SecretsStore::load_from_dir(&project_dir)?;
    let salt_b64 = secrets.ensure_env_salt(&env)?;
    secrets.save_to_dir(&project_dir)?;

    // Prompt for passphrase
    let passphrase = prompt_for_passphrase(&format!("Enter passphrase for '{}' ({})", app_name, env))?;

    // Derive and store the key
    let salt = crate::crypto::decode_salt(&salt_b64)?;
    let key = crate::crypto::EncryptionKey::derive(&passphrase, &salt)?;
    let key_store = crate::crypto::KeyStore::for_salt(&salt_b64)?;
    key_store.save_key(&key)?;

    output::success(&format!(
        "Derived and stored key for {} ({}).",
        output::strong(&app_name),
        output::strong(&env),
    ));

    Ok(())
}

async fn export_key(target_env: Option<&str>) -> Result<(), Box<dyn std::error::Error>> {
    let project_dir = std::env::current_dir()?;
    let app_name = resolve_app_name(&project_dir)?;
    let env = super::helpers::resolve_env(target_env);
    let secrets = crate::config::SecretsStore::load_from_dir(&project_dir)?;
    let salt_b64 = secrets.get_salt(&env).ok_or_else(|| {
        format!("No secrets configured for environment '{}'.", env)
    })?;
    let key_store = crate::crypto::KeyStore::for_salt(salt_b64)?;

    if !key_store.key_exists() {
        return Err(format!(
            "No key found for '{}' ({}). Run 'tako secrets key derive --env {}' first.",
            app_name, env, env
        )
        .into());
    }

    let key = key_store.load_key()?;
    copy_to_clipboard(&key.to_base64())?;

    output::success(&format!(
        "Copied key for {} ({}) to clipboard.",
        output::strong(&app_name),
        output::strong(&env),
    ));

    Ok(())
}

/// Prompt for a passphrase with confirmation
fn prompt_for_passphrase(prompt: &str) -> Result<String, Box<dyn std::error::Error>> {
    let passphrase = read_passphrase(prompt)?;
    let confirm = crate::output::password_field("Confirm passphrase")?;
    if passphrase != confirm {
        return Err("Passphrases do not match".into());
    }
    Ok(passphrase)
}

/// Read a passphrase from interactive prompt or TAKO_PASSPHRASE env var.
fn read_passphrase(prompt: &str) -> Result<String, Box<dyn std::error::Error>> {
    // CI / non-interactive: accept passphrase from environment variable.
    if let Ok(passphrase) = std::env::var("TAKO_PASSPHRASE") {
        if passphrase.is_empty() {
            return Err("TAKO_PASSPHRASE is set but empty".into());
        }
        return Ok(passphrase);
    }

    let passphrase = crate::output::password_field(prompt)?;
    if passphrase.is_empty() {
        return Err("Passphrase cannot be empty".into());
    }
    Ok(passphrase)
}

/// Load a locally cached key, or prompt for passphrase and derive it.
///
/// This is the main key resolution function used by set, sync, deploy, and dev.
/// The cache path is derived from the salt in `secrets.json`, so it's stable
/// across app renames, `--name` overrides, and git worktrees.
pub fn load_or_derive_key(
    app_name: &str,
    env: &str,
    secrets: &crate::config::SecretsStore,
) -> Result<crate::crypto::EncryptionKey, Box<dyn std::error::Error>> {
    // No local key — need to derive from passphrase
    let salt_b64 = secrets.get_salt(env).ok_or_else(|| {
        format!(
            "No salt found for environment '{}'. This shouldn't happen — file a bug.",
            env
        )
    })?;

    let key_store = crate::crypto::KeyStore::for_salt(salt_b64)?;

    // Return cached key if available
    if key_store.key_exists() {
        return Ok(key_store.load_key()?);
    }

    let salt = crate::crypto::decode_salt(salt_b64)?;

    output::muted(&format!(
        "No local key for {} ({}). Deriving from passphrase…",
        output::strong(app_name),
        output::strong(env),
    ));

    let passphrase = read_passphrase(&format!(
        "Enter passphrase for '{}' ({})",
        app_name, env
    ))?;

    let key = crate::crypto::EncryptionKey::derive(&passphrase, &salt)?;

    // Cache locally for future use
    key_store.save_key(&key)?;

    Ok(key)
}

fn resolve_app_name(
    project_dir: &std::path::Path,
) -> Result<String, Box<dyn std::error::Error>> {
    crate::app::require_app_name_from_config(project_dir)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e.to_string()).into())
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
    use crate::ssh::SshClient;

    let mut ssh = SshClient::connect_to(&server.host, server.port).await?;

    // Push secrets through the management protocol; no remote .env file writes.
    let update_cmd = build_update_secrets_command(app_name, secrets)?;
    let response = ssh.tako_command(&update_cmd).await?;
    if tako_response_has_error(&response) {
        return Err(format!("tako-server error (update-secrets): {response}").into());
    }

    ssh.disconnect().await?;

    Ok(())
}

fn build_update_secrets_command(
    app_name: &str,
    secrets: &std::collections::HashMap<String, String>,
) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    serde_json::to_string(&Command::UpdateSecrets {
        app: app_name.to_string(),
        secrets: secrets.clone(),
    })
    .map_err(|e| format!("Failed to serialize update-secrets command: {e}").into())
}

fn tako_response_has_error(response: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(response)
        .ok()
        .and_then(|value| {
            value
                .get("status")
                .and_then(|status| status.as_str())
                .map(|status| status == "error")
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ServerEntry, ServersToml, TakoToml};
    use std::collections::HashMap;

    #[test]
    fn resolve_secret_sync_server_names_uses_explicit_mapping() {
        let tako_config = TakoToml::parse(
            r#"
[envs.production]
route = "app.example.com"
servers = ["solo"]
"#,
        )
        .unwrap();
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
    fn build_update_secrets_command_uses_protocol_payload_not_env_file_writes() {
        let secrets = HashMap::from([("API_KEY".to_string(), "secret".to_string())]);
        let command = build_update_secrets_command("my-app", &secrets).expect("serialize command");
        let value: serde_json::Value =
            serde_json::from_str(&command).expect("parse serialized command");

        assert_eq!(
            value.get("command").and_then(|v| v.as_str()),
            Some("update_secrets")
        );
        assert_eq!(value.get("app").and_then(|v| v.as_str()), Some("my-app"));
        assert_eq!(
            value
                .get("secrets")
                .and_then(|v| v.get("API_KEY"))
                .and_then(|v| v.as_str()),
            Some("secret")
        );
        assert!(!command.contains(".env"));
    }

    #[test]
    fn tako_response_has_error_only_accepts_structured_status_errors() {
        let json_err = r#"{"status":"error","message":"nope"}"#;
        let json_ok = r#"{"status":"ok","data":{}}"#;
        let old_error_shape = r#"{"error":"old-shape"}"#;
        let plain_text = "all good";

        assert!(tako_response_has_error(json_err));
        assert!(!tako_response_has_error(json_ok));
        assert!(!tako_response_has_error(old_error_shape));
        assert!(!tako_response_has_error(plain_text));
    }
}

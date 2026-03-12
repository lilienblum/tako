use crate::config::{ServersToml, TakoToml};
use crate::output;

/// Result of server resolution, indicating whether an explicit mapping was
/// found or the single-production-server fallback was used.
#[derive(Debug)]
pub struct ResolvedServers {
    pub names: Vec<String>,
    /// True when no explicit env mapping existed and the single production
    /// server fallback was applied.
    pub used_fallback: bool,
}

/// Resolve which servers to target for a given environment.
///
/// 1. Returns explicitly mapped servers from `[servers.*]` env config.
/// 2. If none mapped and `env == "production"` with exactly one global server,
///    falls back to that server (sets `used_fallback = true`).
/// 3. Otherwise returns an error.
///
/// Commands that need user confirmation before accepting the fallback should
/// check `used_fallback` and prompt accordingly.
pub fn resolve_servers_for_env(
    tako_config: &TakoToml,
    servers: &ServersToml,
    env: &str,
) -> Result<ResolvedServers, String> {
    let mapped: Vec<String> = tako_config
        .get_servers_for_env(env)
        .into_iter()
        .map(|s| s.to_string())
        .collect();
    if !mapped.is_empty() {
        return Ok(ResolvedServers {
            names: mapped,
            used_fallback: false,
        });
    }

    if env == "production" && servers.len() == 1 {
        if let Some(name) = servers.names().into_iter().next() {
            return Ok(ResolvedServers {
                names: vec![name.to_string()],
                used_fallback: true,
            });
        }
    }

    if servers.is_empty() {
        return Err(format!(
            "No servers have been added. Run 'tako servers add <host>' first, \
             then map it in tako.toml with [servers.<name>] env = \"{}\".",
            env
        ));
    }

    Err(format!(
        "No servers configured for environment '{}'. \
         Add [servers.<name>] with env = \"{}\" to tako.toml.",
        env, env
    ))
}

/// Resolve the target environment name, defaulting to "production".
/// When the default is used (no explicit `--env`), prints a muted hint line.
pub fn resolve_env(requested: Option<&str>) -> String {
    if let Some(env) = requested {
        env.to_string()
    } else {
        let env = "production";
        output::muted(&format!(
            "Using {} environment",
            output::highlight_muted(env)
        ));
        env.to_string()
    }
}

/// Validate that all resolved server names exist in the global servers config.
pub fn validate_server_names(
    names: &[String],
    servers: &ServersToml,
) -> Result<(), String> {
    for name in names {
        if !servers.contains(name) {
            return Err(format!(
                "Server '{}' not found in ~/.tako/config.toml",
                name
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ServerConfig, ServerEntry};

    fn one_server_config() -> ServersToml {
        let mut servers = ServersToml::default();
        servers.servers.insert(
            "solo".to_string(),
            ServerEntry {
                host: "127.0.0.1".to_string(),
                port: 22,
                description: None,
            },
        );
        servers
    }

    #[test]
    fn explicit_mapping_returns_mapped_servers() {
        let mut tako_config = TakoToml::default();
        tako_config.servers.insert(
            "web1".to_string(),
            ServerConfig {
                env: "staging".to_string(),
                ..Default::default()
            },
        );
        let servers = one_server_config();

        let resolved = resolve_servers_for_env(&tako_config, &servers, "staging").unwrap();
        assert_eq!(resolved.names, vec!["web1"]);
        assert!(!resolved.used_fallback);
    }

    #[test]
    fn production_single_server_fallback() {
        let tako_config = TakoToml::default();
        let servers = one_server_config();

        let resolved =
            resolve_servers_for_env(&tako_config, &servers, "production").unwrap();
        assert_eq!(resolved.names, vec!["solo"]);
        assert!(resolved.used_fallback);
    }

    #[test]
    fn non_production_without_mapping_errors() {
        let tako_config = TakoToml::default();
        let servers = one_server_config();

        let err = resolve_servers_for_env(&tako_config, &servers, "staging")
            .expect_err("should fail");
        assert!(err.contains("No servers configured for environment 'staging'"));
    }

    #[test]
    fn no_servers_at_all_errors() {
        let tako_config = TakoToml::default();
        let servers = ServersToml::default();

        let err = resolve_servers_for_env(&tako_config, &servers, "production")
            .expect_err("should fail");
        assert!(err.contains("No servers have been added"));
    }

    #[test]
    fn validate_server_names_passes_for_known_servers() {
        let servers = one_server_config();
        assert!(validate_server_names(&["solo".to_string()], &servers).is_ok());
    }

    #[test]
    fn validate_server_names_fails_for_unknown_server() {
        let servers = one_server_config();
        let err = validate_server_names(&["missing".to_string()], &servers)
            .expect_err("should fail");
        assert!(err.contains("missing"));
    }
}

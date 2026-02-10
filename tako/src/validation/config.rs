use crate::config::{ConfigError, Result, ServersToml, TakoToml};

/// Validation result with warnings
#[derive(Debug, Default)]
pub struct ValidationResult {
    /// Critical errors that prevent operation
    pub errors: Vec<String>,
    /// Warnings that should be shown to user
    pub warnings: Vec<String>,
}

impl ValidationResult {
    /// Create a new empty result
    pub fn new() -> Self {
        Self::default()
    }

    /// Add an error
    pub fn error(&mut self, msg: impl Into<String>) {
        self.errors.push(msg.into());
    }

    /// Add a warning
    pub fn warn(&mut self, msg: impl Into<String>) {
        self.warnings.push(msg.into());
    }

    /// Check if there are any errors
    pub fn has_errors(&self) -> bool {
        !self.errors.is_empty()
    }

    /// Check if there are any warnings
    pub fn has_warnings(&self) -> bool {
        !self.warnings.is_empty()
    }

    /// Convert to Result, failing if there are errors
    pub fn into_result(self) -> Result<Vec<String>> {
        if self.has_errors() {
            Err(ConfigError::Validation(self.errors.join("\n")))
        } else {
            Ok(self.warnings)
        }
    }

    /// Merge another result into this one
    pub fn merge(&mut self, other: ValidationResult) {
        self.errors.extend(other.errors);
        self.warnings.extend(other.warnings);
    }
}

/// Validate tako.toml configuration
pub fn validate_tako_toml(config: &TakoToml) -> ValidationResult {
    let mut result = ValidationResult::new();

    // Check for environments without routes
    for (env_name, env_config) in &config.envs {
        if env_config.route.is_none() && env_config.routes.is_none() {
            result.warn(format!(
                "Environment '{}' has no routes configured",
                env_name
            ));
        }
    }

    // Check for server configs without matching environments
    for (server_name, server_config) in &config.servers {
        if !config.envs.contains_key(&server_config.env)
            && !config.envs.is_empty()
            && server_config.env != "production"
        {
            result.error(format!(
                "Server '{}' references unknown environment '{}'",
                server_name, server_config.env
            ));
        }
    }

    // Check for environments without servers
    for env_name in config.envs.keys() {
        let servers = config.get_servers_for_env(env_name);
        if servers.is_empty() {
            result.warn(format!(
                "Environment '{}' has no servers configured",
                env_name
            ));
        }
    }

    // Check port range
    if config.server_defaults.port == 0 {
        result.error("Default port cannot be 0".to_string());
    }

    for (server_name, server_config) in &config.servers {
        if let Some(port) = server_config.port
            && port == 0
        {
            result.error(format!("Server '{}' has invalid port 0", server_name));
        }
    }

    result
}

/// Validate global server inventory configuration.
pub fn validate_servers_toml(config: &ServersToml) -> ValidationResult {
    let mut result = ValidationResult::new();

    // Check for empty host
    for (name, entry) in &config.servers {
        if entry.host.is_empty() {
            result.error(format!("Server '{}' has empty host", name));
        }

        if entry.port == 0 {
            result.error(format!("Server '{}' has invalid SSH port 0", name));
        }
    }

    result
}

/// Validate that tako.toml servers reference existing global servers.
pub fn validate_server_references(
    tako_config: &TakoToml,
    servers_config: &ServersToml,
) -> ValidationResult {
    let mut result = ValidationResult::new();

    for server_name in tako_config.servers.keys() {
        if !servers_config.contains(server_name) {
            result.error(format!(
                "Server '{}' is configured in tako.toml but not found in ~/.tako/config.toml [[servers]]. \
                  Run 'tako servers add --name {} <host>' to add it.",
                server_name, server_name
            ));
        }
    }

    result
}

/// Full configuration validation
pub fn validate_full_config(
    tako_config: &TakoToml,
    servers_config: &ServersToml,
) -> ValidationResult {
    let mut result = ValidationResult::new();

    result.merge(validate_tako_toml(tako_config));
    result.merge(validate_servers_toml(servers_config));
    result.merge(validate_server_references(tako_config, servers_config));

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{EnvConfig, ServerConfig, TakoToml};

    #[test]
    fn validate_tako_toml_allows_implicit_production_server_env() {
        let mut config = TakoToml::default();
        config
            .envs
            .insert("staging".to_string(), EnvConfig::default());
        config.servers.insert(
            "prod-1".to_string(),
            ServerConfig {
                env: "production".to_string(),
                instances: None,
                port: None,
                idle_timeout: None,
            },
        );

        let result = validate_tako_toml(&config);
        assert!(
            !result
                .errors
                .iter()
                .any(|e| e.contains("unknown environment"))
        );
    }

    #[test]
    fn validate_tako_toml_rejects_unknown_non_production_server_env() {
        let mut config = TakoToml::default();
        config
            .envs
            .insert("staging".to_string(), EnvConfig::default());
        config.servers.insert(
            "bad-1".to_string(),
            ServerConfig {
                env: "qa".to_string(),
                instances: None,
                port: None,
                idle_timeout: None,
            },
        );

        let result = validate_tako_toml(&config);
        assert!(
            result
                .errors
                .iter()
                .any(|e| e.contains("unknown environment 'qa'"))
        );
    }
}

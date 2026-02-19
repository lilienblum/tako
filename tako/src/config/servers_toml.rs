use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use super::error::{ConfigError, Result};

/// Server inventory from ~/.tako/config.toml `[[servers]]`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ServersToml {
    /// Map of server name to server entry
    #[serde(default)]
    pub servers: HashMap<String, ServerEntry>,

    /// Optional detected runtime build targets by server name.
    #[serde(default)]
    pub server_targets: HashMap<String, ServerTarget>,
}

/// Detected server target metadata used for runtime-specific build planning.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServerTarget {
    /// CPU architecture (for example: x86_64, aarch64).
    pub arch: String,
    /// C library family (for example: glibc, musl).
    pub libc: String,
}

impl ServerTarget {
    /// Canonicalize target metadata values.
    pub fn normalized(arch: &str, libc: &str) -> std::result::Result<Self, String> {
        let arch = Self::normalize_arch(arch)
            .ok_or_else(|| format!("Unsupported server target architecture '{}'", arch.trim()))?;
        let libc = Self::normalize_libc(libc)
            .ok_or_else(|| format!("Unsupported server target libc '{}'", libc.trim()))?;
        Ok(Self { arch, libc })
    }

    /// Normalize architecture aliases.
    pub fn normalize_arch(value: &str) -> Option<String> {
        let normalized = value.trim().to_ascii_lowercase();
        match normalized.as_str() {
            "x86_64" | "amd64" => Some("x86_64".to_string()),
            "aarch64" | "arm64" => Some("aarch64".to_string()),
            _ => None,
        }
    }

    /// Normalize libc aliases.
    pub fn normalize_libc(value: &str) -> Option<String> {
        let normalized = value.trim().to_ascii_lowercase();
        match normalized.as_str() {
            "glibc" | "gnu" | "gnu-libc" | "gnu_libc" | "gnu libc" | "gnu c library" => {
                Some("glibc".to_string())
            }
            "musl" => Some("musl".to_string()),
            _ => None,
        }
    }

    /// Stable human-readable target label.
    pub fn label(&self) -> String {
        format!("linux-{}-{}", self.arch, self.libc)
    }
}

/// Single server entry with SSH connection details
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ServerEntry {
    /// Server hostname or IP address
    pub host: String,

    /// SSH port (default: 22)
    #[serde(default = "default_ssh_port")]
    pub port: u16,

    /// Optional human-readable server description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

fn default_ssh_port() -> u16 {
    22
}

impl Default for ServerEntry {
    fn default() -> Self {
        Self {
            host: String::new(),
            port: default_ssh_port(),
            description: None,
        }
    }
}

impl ServersToml {
    /// Get the default path for global config (~/.tako/config.toml).
    pub fn default_path() -> Result<PathBuf> {
        let home = crate::paths::tako_home_dir().map_err(|e| {
            ConfigError::Validation(format!("Could not determine tako home directory: {}", e))
        })?;
        Ok(home.join("config.toml"))
    }

    fn load_from_paths(config_path: &Path) -> Result<Self> {
        if config_path.exists() {
            return Self::load_from_file(config_path);
        }

        Ok(Self::default())
    }

    /// Load server inventory from the default location.
    pub fn load() -> Result<Self> {
        let config_path = Self::default_path()?;
        Self::load_from_paths(&config_path)
    }

    /// Load server inventory from a specific file.
    pub fn load_from_file<P: AsRef<Path>>(path: P) -> Result<Self> {
        let content = fs::read_to_string(path.as_ref())
            .map_err(|e| ConfigError::FileRead(path.as_ref().to_path_buf(), e))?;
        Self::parse(&content)
    }

    /// Parse server inventory TOML (`[[servers]]` array).
    pub fn parse(content: &str) -> Result<Self> {
        if content.trim().is_empty() {
            return Ok(Self::default());
        }

        // Parse only the [[servers]] array and ignore unrelated top-level config.
        let raw: toml::Value = toml::from_str(content)?;

        let mut config = ServersToml::default();

        if raw.get("server_targets").is_some() {
            return Err(ConfigError::Validation(
                "Global server config no longer supports [server_targets.<name>]. Set `arch` and `libc` directly inside each [[servers]] entry.".to_string(),
            ));
        }

        if let Some(servers_array) = raw.get("servers")
            && let Some(array) = servers_array.as_array()
        {
            for server_value in array {
                // Each server must have a name field
                let name = server_value
                    .get("name")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        ConfigError::Validation("Server entry must have a 'name' field".to_string())
                    })?;

                let host = server_value
                    .get("host")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        ConfigError::Validation(format!(
                            "Server '{}' must have a 'host' field",
                            name
                        ))
                    })?;

                let port = server_value
                    .get("port")
                    .and_then(|v| v.as_integer())
                    .map(|p| p as u16)
                    .unwrap_or_else(default_ssh_port);

                let description = server_value
                    .get("description")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());

                let arch_field = server_value.get("arch");
                let libc_field = server_value.get("libc");
                let arch = match arch_field {
                    Some(value) => Some(value.as_str().ok_or_else(|| {
                        ConfigError::Validation(format!(
                            "Server '{}' field 'arch' must be a string",
                            name
                        ))
                    })?),
                    None => None,
                };
                let libc = match libc_field {
                    Some(value) => Some(value.as_str().ok_or_else(|| {
                        ConfigError::Validation(format!(
                            "Server '{}' field 'libc' must be a string",
                            name
                        ))
                    })?),
                    None => None,
                };

                let entry = ServerEntry {
                    host: host.to_string(),
                    port,
                    description,
                };

                // Check for duplicate names
                if config.servers.contains_key(name) {
                    return Err(ConfigError::DuplicateServerName(name.to_string()));
                }

                // Check for duplicate hosts
                if config.servers.values().any(|e| e.host == host) {
                    return Err(ConfigError::DuplicateServerHost(host.to_string()));
                }

                config.servers.insert(name.to_string(), entry);

                match (arch, libc) {
                    (Some(arch), Some(libc)) => {
                        let normalized = ServerTarget::normalized(arch, libc).map_err(|e| {
                            ConfigError::Validation(format!(
                                "Invalid target metadata for server '{}': {}",
                                name, e
                            ))
                        })?;
                        config.server_targets.insert(name.to_string(), normalized);
                    }
                    (None, None) => {}
                    _ => {
                        return Err(ConfigError::Validation(format!(
                            "Server '{}' must set both `arch` and `libc` together inside [[servers]]",
                            name
                        )));
                    }
                }
            }
        }

        config.validate()?;
        Ok(config)
    }

    /// Validate the configuration
    pub fn validate(&self) -> Result<()> {
        for (name, entry) in &self.servers {
            // Validate server name
            validate_server_name(name)?;

            // Validate host
            if entry.host.is_empty() {
                return Err(ConfigError::Validation(format!(
                    "Server '{}' has empty host",
                    name
                )));
            }

            // Validate port
            if entry.port == 0 {
                return Err(ConfigError::Validation(format!(
                    "Server '{}' has invalid port 0",
                    name
                )));
            }
        }

        for name in self.server_targets.keys() {
            if !self.servers.contains_key(name) {
                return Err(ConfigError::Validation(format!(
                    "Target metadata references unknown server '{}'",
                    name
                )));
            }
        }

        Ok(())
    }

    /// Save server inventory to the default global config path.
    pub fn save(&self) -> Result<()> {
        let path = Self::default_path()?;
        self.save_to_file(&path)
    }

    /// Save server inventory to a specific TOML file, preserving unrelated sections.
    pub fn save_to_file<P: AsRef<Path>>(&self, path: P) -> Result<()> {
        let path = path.as_ref();

        // Ensure parent directory exists
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| ConfigError::FileWrite(parent.to_path_buf(), e))?;
        }

        let mut doc = if path.exists() {
            let existing = fs::read_to_string(path)
                .map_err(|e| ConfigError::FileRead(path.to_path_buf(), e))?;
            if existing.trim().is_empty() {
                toml::Value::Table(toml::map::Map::new())
            } else {
                toml::from_str::<toml::Value>(&existing)?
            }
        } else {
            toml::Value::Table(toml::map::Map::new())
        };

        let root = doc.as_table_mut().ok_or_else(|| {
            ConfigError::Validation("Global config must be a TOML table".to_string())
        })?;

        let mut names: Vec<&str> = self.servers.keys().map(|k| k.as_str()).collect();
        names.sort_unstable();

        let mut servers_array = Vec::with_capacity(names.len());
        for name in names {
            let entry = self.servers.get(name).ok_or_else(|| {
                ConfigError::Validation(format!("Missing server entry '{}'", name))
            })?;

            let mut table = toml::map::Map::new();
            table.insert("name".to_string(), toml::Value::String(name.to_string()));
            table.insert("host".to_string(), toml::Value::String(entry.host.clone()));
            if entry.port != default_ssh_port() {
                table.insert("port".to_string(), toml::Value::Integer(entry.port as i64));
            }
            if let Some(description) = &entry.description
                && !description.trim().is_empty()
            {
                table.insert(
                    "description".to_string(),
                    toml::Value::String(description.clone()),
                );
            }
            if let Some(target) = self.server_targets.get(name) {
                table.insert("arch".to_string(), toml::Value::String(target.arch.clone()));
                table.insert("libc".to_string(), toml::Value::String(target.libc.clone()));
            }
            servers_array.push(toml::Value::Table(table));
        }

        if servers_array.is_empty() {
            root.remove("servers");
        } else {
            root.insert("servers".to_string(), toml::Value::Array(servers_array));
        }
        root.remove("server_targets");

        let content = toml::to_string_pretty(&doc)?;
        fs::write(path, content).map_err(|e| ConfigError::FileWrite(path.to_path_buf(), e))?;

        Ok(())
    }

    /// Get a server by name
    pub fn get(&self, name: &str) -> Option<&ServerEntry> {
        self.servers.get(name)
    }

    /// Check if a server exists by name
    pub fn contains(&self, name: &str) -> bool {
        self.servers.contains_key(name)
    }

    /// Check if a host already exists
    pub fn contains_host(&self, host: &str) -> bool {
        self.servers.values().any(|e| e.host == host)
    }

    /// Find server name by host
    pub fn find_by_host(&self, host: &str) -> Option<&str> {
        self.servers
            .iter()
            .find(|(_, e)| e.host == host)
            .map(|(name, _)| name.as_str())
    }

    /// Add a new server
    pub fn add(&mut self, name: String, entry: ServerEntry) -> Result<()> {
        if self.servers.contains_key(&name) {
            return Err(ConfigError::DuplicateServerName(name));
        }
        if self.contains_host(&entry.host) {
            return Err(ConfigError::DuplicateServerHost(entry.host.clone()));
        }
        validate_server_name(&name)?;
        self.servers.insert(name, entry);
        Ok(())
    }

    /// Remove a server by name
    pub fn remove(&mut self, name: &str) -> Result<ServerEntry> {
        let removed = self
            .servers
            .remove(name)
            .ok_or_else(|| ConfigError::ServerNotFound(name.to_string()))?;
        self.server_targets.remove(name);
        Ok(removed)
    }

    /// Update an existing server (by name, allows changing host)
    pub fn update(&mut self, name: &str, entry: ServerEntry) -> Result<()> {
        if !self.servers.contains_key(name) {
            return Err(ConfigError::ServerNotFound(name.to_string()));
        }

        // Check if new host conflicts with another server
        if let Some(existing_name) = self.find_by_host(&entry.host)
            && existing_name != name
        {
            return Err(ConfigError::DuplicateServerHost(entry.host.clone()));
        }

        self.servers.insert(name.to_string(), entry);
        Ok(())
    }

    /// Read detected target metadata for a server.
    pub fn get_target(&self, name: &str) -> Option<&ServerTarget> {
        self.server_targets.get(name)
    }

    /// Set detected target metadata for a server.
    pub fn set_target(&mut self, name: &str, target: ServerTarget) -> Result<()> {
        if !self.servers.contains_key(name) {
            return Err(ConfigError::ServerNotFound(name.to_string()));
        }
        let normalized = ServerTarget::normalized(&target.arch, &target.libc).map_err(|e| {
            ConfigError::Validation(format!(
                "Invalid target metadata for server '{}': {}",
                name, e
            ))
        })?;
        self.server_targets.insert(name.to_string(), normalized);
        Ok(())
    }

    /// Get all server names
    pub fn names(&self) -> Vec<&str> {
        self.servers.keys().map(|s| s.as_str()).collect()
    }

    /// Get number of servers
    pub fn len(&self) -> usize {
        self.servers.len()
    }

    /// Check if empty
    pub fn is_empty(&self) -> bool {
        self.servers.is_empty()
    }
}

/// Validate server name format
fn validate_server_name(name: &str) -> Result<()> {
    if name.is_empty() {
        return Err(ConfigError::Validation(
            "Server name cannot be empty".to_string(),
        ));
    }

    if name.len() > 63 {
        return Err(ConfigError::Validation(
            "Server name cannot exceed 63 characters".to_string(),
        ));
    }

    // Must start with lowercase letter
    if !name
        .chars()
        .next()
        .map(|c| c.is_ascii_lowercase())
        .unwrap_or(false)
    {
        return Err(ConfigError::Validation(
            "Server name must start with a lowercase letter".to_string(),
        ));
    }

    // Only lowercase letters, numbers, and hyphens
    for c in name.chars() {
        if !c.is_ascii_lowercase() && !c.is_ascii_digit() && c != '-' {
            return Err(ConfigError::Validation(format!(
                "Server name can only contain lowercase letters, numbers, and hyphens. Found: '{}'",
                c
            )));
        }
    }

    // Cannot end with hyphen
    if name.ends_with('-') {
        return Err(ConfigError::Validation(
            "Server name cannot end with a hyphen".to_string(),
        ));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    // ==================== Parsing Tests ====================

    #[test]
    fn test_parse_empty_file() {
        let config = ServersToml::parse("").unwrap();
        assert!(config.servers.is_empty());
    }

    #[test]
    fn test_parse_single_server() {
        let toml = r#"
[[servers]]
name = "la"
host = "1.2.3.4"
"#;
        let config = ServersToml::parse(toml).unwrap();
        assert_eq!(config.servers.len(), 1);

        let server = config.get("la").unwrap();
        assert_eq!(server.host, "1.2.3.4");
        assert_eq!(server.port, 22);
    }

    #[test]
    fn test_parse_server_with_all_fields() {
        let toml = r#"
[[servers]]
name = "production"
host = "prod.example.com"
port = 2222
description = "Primary production server"
"#;
        let config = ServersToml::parse(toml).unwrap();
        let server = config.get("production").unwrap();

        assert_eq!(server.host, "prod.example.com");
        assert_eq!(server.port, 2222);
        assert_eq!(
            server.description.as_deref(),
            Some("Primary production server")
        );
    }

    #[test]
    fn test_parse_multiple_servers() {
        let toml = r#"
[[servers]]
name = "la"
host = "1.2.3.4"

[[servers]]
name = "nyc"
host = "5.6.7.8"

[[servers]]
name = "kyoto"
host = "9.10.11.12"
port = 2222
"#;
        let config = ServersToml::parse(toml).unwrap();
        assert_eq!(config.servers.len(), 3);

        assert!(config.contains("la"));
        assert!(config.contains("nyc"));
        assert!(config.contains("kyoto"));

        assert_eq!(config.get("kyoto").unwrap().port, 2222);
    }

    #[test]
    fn test_parse_server_entry_target_fields() {
        let toml = r#"
[[servers]]
name = "la"
host = "1.2.3.4"
arch = "x86_64"
libc = "glibc"
"#;
        let config = ServersToml::parse(toml).unwrap();
        let target = config.get_target("la").unwrap();
        assert_eq!(target.arch, "x86_64");
        assert_eq!(target.libc, "glibc");
    }

    #[test]
    fn test_parse_rejects_partial_server_target_fields() {
        let toml = r#"
[[servers]]
name = "la"
host = "1.2.3.4"
arch = "x86_64"
"#;
        let err = ServersToml::parse(toml).unwrap_err();
        assert!(err.to_string().contains("both `arch` and `libc`"));
    }

    #[test]
    fn test_target_normalization_accepts_common_aliases() {
        assert_eq!(
            ServerTarget::normalize_arch("amd64").as_deref(),
            Some("x86_64")
        );
        assert_eq!(
            ServerTarget::normalize_arch("arm64").as_deref(),
            Some("aarch64")
        );
        assert_eq!(
            ServerTarget::normalize_libc("GNU libc").as_deref(),
            Some("glibc")
        );
        assert_eq!(
            ServerTarget::normalize_libc("musl").as_deref(),
            Some("musl")
        );
    }

    #[test]
    fn test_target_normalization_rejects_unknown_values() {
        assert!(ServerTarget::normalize_arch("sparc").is_none());
        assert!(ServerTarget::normalize_libc("uclibc").is_none());
    }

    #[test]
    fn test_parse_rejects_legacy_server_targets_table() {
        let toml = r#"
[server_targets.ghost]
arch = "x86_64"
libc = "glibc"
"#;
        let err = ServersToml::parse(toml).unwrap_err();
        assert!(err.to_string().contains("no longer supports [server_targets"));
    }

    #[test]
    fn test_parse_from_config_toml_with_dev_section() {
        let toml = r#"
[dev]
port = 55555

[[servers]]
name = "la"
host = "1.2.3.4"
"#;
        let config = ServersToml::parse(toml).unwrap();
        assert_eq!(config.len(), 1);
        assert!(config.contains("la"));
    }

    #[test]
    fn test_default_values() {
        let entry = ServerEntry::default();
        assert_eq!(entry.port, 22);
    }

    // ==================== Validation Tests ====================

    #[test]
    fn test_validate_server_name_valid() {
        assert!(validate_server_name("la").is_ok());
        assert!(validate_server_name("prod-server").is_ok());
        assert!(validate_server_name("server1").is_ok());
        assert!(validate_server_name("my-prod-server-1").is_ok());
    }

    #[test]
    fn test_validate_server_name_empty() {
        assert!(validate_server_name("").is_err());
    }

    #[test]
    fn test_validate_server_name_too_long() {
        let long_name = "a".repeat(64);
        assert!(validate_server_name(&long_name).is_err());
    }

    #[test]
    fn test_validate_server_name_invalid_start() {
        assert!(validate_server_name("1server").is_err());
        assert!(validate_server_name("-server").is_err());
        assert!(validate_server_name("Server").is_err());
    }

    #[test]
    fn test_validate_server_name_invalid_chars() {
        assert!(validate_server_name("my_server").is_err());
        assert!(validate_server_name("my.server").is_err());
        assert!(validate_server_name("MY-SERVER").is_err());
    }

    #[test]
    fn test_duplicate_server_names() {
        let toml = r#"
[[servers]]
name = "la"
host = "1.2.3.4"

[[servers]]
name = "la"
host = "5.6.7.8"
"#;
        let result = ServersToml::parse(toml);
        assert!(matches!(result, Err(ConfigError::DuplicateServerName(_))));
    }

    #[test]
    fn test_duplicate_hosts() {
        let toml = r#"
[[servers]]
name = "la"
host = "1.2.3.4"

[[servers]]
name = "nyc"
host = "1.2.3.4"
"#;
        let result = ServersToml::parse(toml);
        assert!(matches!(result, Err(ConfigError::DuplicateServerHost(_))));
    }

    #[test]
    fn test_missing_name_field() {
        let toml = r#"
[[servers]]
host = "1.2.3.4"
"#;
        let result = ServersToml::parse(toml);
        assert!(result.is_err());
    }

    #[test]
    fn test_missing_host_field() {
        let toml = r#"
[[servers]]
name = "la"
"#;
        let result = ServersToml::parse(toml);
        assert!(result.is_err());
    }

    // ==================== CRUD Operation Tests ====================

    #[test]
    fn test_add_server() {
        let mut config = ServersToml::default();

        config
            .add(
                "la".to_string(),
                ServerEntry {
                    host: "1.2.3.4".to_string(),
                    port: 22,
                    description: None,
                },
            )
            .unwrap();

        assert_eq!(config.len(), 1);
        assert!(config.contains("la"));
    }

    #[test]
    fn test_add_duplicate_name_fails() {
        let mut config = ServersToml::default();

        config
            .add(
                "la".to_string(),
                ServerEntry {
                    host: "1.2.3.4".to_string(),
                    ..Default::default()
                },
            )
            .unwrap();

        let result = config.add(
            "la".to_string(),
            ServerEntry {
                host: "5.6.7.8".to_string(),
                ..Default::default()
            },
        );

        assert!(matches!(result, Err(ConfigError::DuplicateServerName(_))));
    }

    #[test]
    fn test_add_duplicate_host_fails() {
        let mut config = ServersToml::default();

        config
            .add(
                "la".to_string(),
                ServerEntry {
                    host: "1.2.3.4".to_string(),
                    ..Default::default()
                },
            )
            .unwrap();

        let result = config.add(
            "nyc".to_string(),
            ServerEntry {
                host: "1.2.3.4".to_string(),
                ..Default::default()
            },
        );

        assert!(matches!(result, Err(ConfigError::DuplicateServerHost(_))));
    }

    #[test]
    fn test_remove_server() {
        let mut config = ServersToml::default();

        config
            .add(
                "la".to_string(),
                ServerEntry {
                    host: "1.2.3.4".to_string(),
                    ..Default::default()
                },
            )
            .unwrap();
        config
            .set_target(
                "la",
                ServerTarget {
                    arch: "x86_64".to_string(),
                    libc: "glibc".to_string(),
                },
            )
            .unwrap();

        let removed = config.remove("la").unwrap();
        assert_eq!(removed.host, "1.2.3.4");
        assert!(config.is_empty());
        assert!(config.get_target("la").is_none());
    }

    #[test]
    fn test_set_target_normalizes_arch_and_libc_aliases() {
        let mut config = ServersToml::default();
        config
            .add(
                "la".to_string(),
                ServerEntry {
                    host: "1.2.3.4".to_string(),
                    ..Default::default()
                },
            )
            .unwrap();

        config
            .set_target(
                "la",
                ServerTarget {
                    arch: "amd64".to_string(),
                    libc: "gnu libc".to_string(),
                },
            )
            .unwrap();

        let target = config.get_target("la").unwrap();
        assert_eq!(target.arch, "x86_64");
        assert_eq!(target.libc, "glibc");
        assert_eq!(target.label(), "linux-x86_64-glibc");
    }

    #[test]
    fn test_set_target_rejects_unknown_metadata_values() {
        let mut config = ServersToml::default();
        config
            .add(
                "la".to_string(),
                ServerEntry {
                    host: "1.2.3.4".to_string(),
                    ..Default::default()
                },
            )
            .unwrap();

        let err = config
            .set_target(
                "la",
                ServerTarget {
                    arch: "sparc".to_string(),
                    libc: "glibc".to_string(),
                },
            )
            .unwrap_err();
        assert!(err.to_string().contains("Invalid target metadata"));
    }

    #[test]
    fn test_remove_nonexistent_fails() {
        let mut config = ServersToml::default();
        let result = config.remove("la");
        assert!(matches!(result, Err(ConfigError::ServerNotFound(_))));
    }

    #[test]
    fn test_update_server() {
        let mut config = ServersToml::default();

        config
            .add(
                "la".to_string(),
                ServerEntry {
                    host: "1.2.3.4".to_string(),
                    port: 22,
                    description: None,
                },
            )
            .unwrap();

        config
            .update(
                "la",
                ServerEntry {
                    host: "5.6.7.8".to_string(),
                    port: 2222,
                    description: None,
                },
            )
            .unwrap();

        let server = config.get("la").unwrap();
        assert_eq!(server.host, "5.6.7.8");
        assert_eq!(server.port, 2222);
    }

    #[test]
    fn test_find_by_host() {
        let mut config = ServersToml::default();

        config
            .add(
                "la".to_string(),
                ServerEntry {
                    host: "1.2.3.4".to_string(),
                    ..Default::default()
                },
            )
            .unwrap();

        assert_eq!(config.find_by_host("1.2.3.4"), Some("la"));
        assert_eq!(config.find_by_host("5.6.7.8"), None);
    }

    // ==================== File I/O Tests ====================

    #[test]
    fn test_save_and_load() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("config.toml");

        let mut config = ServersToml::default();
        config
            .add(
                "la".to_string(),
                ServerEntry {
                    host: "1.2.3.4".to_string(),
                    port: 2222,
                    description: Some("west coast".to_string()),
                },
            )
            .unwrap();
        config
            .add(
                "nyc".to_string(),
                ServerEntry {
                    host: "5.6.7.8".to_string(),
                    ..Default::default()
                },
            )
            .unwrap();
        config
            .set_target(
                "la",
                ServerTarget {
                    arch: "x86_64".to_string(),
                    libc: "glibc".to_string(),
                },
            )
            .unwrap();

        config.save_to_file(&path).unwrap();
        let written = fs::read_to_string(&path).unwrap();
        assert!(written.contains("arch = \"x86_64\""));
        assert!(written.contains("libc = \"glibc\""));
        assert!(!written.contains("[server_targets."));

        let loaded = ServersToml::load_from_file(&path).unwrap();
        assert_eq!(loaded.len(), 2);

        let la = loaded.get("la").unwrap();
        assert_eq!(la.host, "1.2.3.4");
        assert_eq!(la.port, 2222);
        assert_eq!(la.description.as_deref(), Some("west coast"));
        let la_target = loaded.get_target("la").unwrap();
        assert_eq!(la_target.arch, "x86_64");
        assert_eq!(la_target.libc, "glibc");

        let nyc = loaded.get("nyc").unwrap();
        assert_eq!(nyc.host, "5.6.7.8");
        assert_eq!(nyc.port, 22); // default
        assert!(loaded.get_target("nyc").is_none());
    }

    #[test]
    fn test_load_nonexistent_returns_default() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("nonexistent.toml");

        // load_from_file should fail for nonexistent
        assert!(ServersToml::load_from_file(&path).is_err());
    }

    #[test]
    fn test_creates_parent_directory() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("subdir").join("config.toml");

        let mut config = ServersToml::default();
        config
            .add(
                "la".to_string(),
                ServerEntry {
                    host: "1.2.3.4".to_string(),
                    ..Default::default()
                },
            )
            .unwrap();

        config.save_to_file(&path).unwrap();
        assert!(path.exists());
    }

    #[test]
    fn test_save_preserves_dev_section() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("config.toml");
        fs::write(
            &path,
            r#"
[dev]
port = 61234
"#,
        )
        .unwrap();

        let mut config = ServersToml::default();
        config
            .add(
                "la".to_string(),
                ServerEntry {
                    host: "1.2.3.4".to_string(),
                    ..Default::default()
                },
            )
            .unwrap();

        config.save_to_file(&path).unwrap();
        let written = fs::read_to_string(&path).unwrap();
        assert!(written.contains("[dev]"));
        assert!(written.contains("port = 61234"));
        assert!(written.contains("[[servers]]"));
    }

    #[test]
    fn test_load_prefers_config_over_legacy_when_present() {
        let temp_dir = TempDir::new().unwrap();
        let config_path = temp_dir.path().join("config.toml");
        let legacy_path = temp_dir.path().join("servers.toml");

        fs::write(
            &config_path,
            r#"
[[servers]]
name = "from-config"
host = "1.1.1.1"
"#,
        )
        .unwrap();
        fs::write(
            &legacy_path,
            r#"
[[servers]]
name = "from-legacy"
host = "2.2.2.2"
"#,
        )
        .unwrap();

        let loaded = ServersToml::load_from_paths(&config_path).unwrap();
        assert!(loaded.contains("from-config"));
        assert!(!loaded.contains("from-legacy"));
    }

    #[test]
    fn test_load_does_not_fallback_to_legacy_when_config_has_no_servers() {
        let temp_dir = TempDir::new().unwrap();
        let config_path = temp_dir.path().join("config.toml");
        let legacy_path = temp_dir.path().join("servers.toml");

        fs::write(
            &config_path,
            r#"
[dev]
port = 55555
"#,
        )
        .unwrap();
        fs::write(
            &legacy_path,
            r#"
[[servers]]
name = "from-legacy"
host = "2.2.2.2"
"#,
        )
        .unwrap();

        let loaded = ServersToml::load_from_paths(&config_path).unwrap();
        assert!(loaded.is_empty());
    }

    #[test]
    fn test_load_does_not_fallback_to_legacy_when_config_missing() {
        let temp_dir = TempDir::new().unwrap();
        let config_path = temp_dir.path().join("config.toml");
        let legacy_path = temp_dir.path().join("servers.toml");

        fs::write(
            &legacy_path,
            r#"
[[servers]]
name = "from-legacy"
host = "2.2.2.2"
"#,
        )
        .unwrap();

        let loaded = ServersToml::load_from_paths(&config_path).unwrap();
        assert!(loaded.is_empty());
    }
}

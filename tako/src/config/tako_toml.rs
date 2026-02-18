use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Component, Path};

use super::error::{ConfigError, Result};

/// Root configuration from tako.toml
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct TakoToml {
    /// Application name (required; stable identity)
    pub name: Option<String>,

    /// Build settings for deploy artifact generation.
    #[serde(default)]
    pub build: BuildConfig,

    /// Runtime entrypoint override relative to project root
    pub main: Option<String>,

    /// [vars] section - global environment variables
    #[serde(default)]
    pub vars: HashMap<String, String>,

    /// [vars.*] sections - per-environment variables
    #[serde(default)]
    pub vars_per_env: HashMap<String, HashMap<String, String>>,

    /// [envs.*] sections - environment configurations
    #[serde(default)]
    pub envs: HashMap<String, EnvConfig>,

    /// [servers] section - default server settings
    #[serde(default)]
    pub server_defaults: ServerDefaults,

    /// [servers.*] sections - per-server configurations
    #[serde(default)]
    pub servers: HashMap<String, ServerConfig>,
}

/// Build configuration from [build].
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct BuildConfig {
    /// Build preset reference (for example: "bun", "bun/<commit-hash>", or "github:owner/repo/path.toml@<sha>").
    pub preset: Option<String>,

    /// Additional file globs to include in the deploy artifact.
    #[serde(default)]
    pub include: Vec<String>,

    /// File globs to exclude from the deploy artifact.
    #[serde(default)]
    pub exclude: Vec<String>,

    /// Additional asset directories merged into app public/ after container build.
    #[serde(default)]
    pub assets: Vec<String>,
}

/// Environment configuration from [envs.*]
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct EnvConfig {
    /// Single route (mutually exclusive with routes)
    pub route: Option<String>,

    /// Multiple routes (mutually exclusive with route)
    pub routes: Option<Vec<String>>,

    /// Environment-specific variables (merged with global vars)
    #[serde(flatten)]
    pub vars: HashMap<String, String>,
}

/// Default server settings from [servers]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ServerDefaults {
    /// Default number of instances (0 = on-demand)
    #[serde(default)]
    pub instances: u8,

    /// Default port (80)
    #[serde(default = "default_port")]
    pub port: u16,

    /// Idle timeout in seconds (300 = 5 minutes)
    #[serde(default = "default_idle_timeout")]
    pub idle_timeout: u32,
}

/// Per-server configuration from [servers.*]
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ServerConfig {
    /// Environment this server belongs to (required)
    pub env: String,

    /// Override number of instances for this server
    pub instances: Option<u8>,

    /// Override port for this server
    pub port: Option<u16>,

    /// Override idle timeout for this server
    pub idle_timeout: Option<u32>,
}

fn default_port() -> u16 {
    80
}

fn default_idle_timeout() -> u32 {
    300
}

impl Default for ServerDefaults {
    fn default() -> Self {
        Self {
            instances: 0,
            port: default_port(),
            idle_timeout: default_idle_timeout(),
        }
    }
}

impl TakoToml {
    /// Load tako.toml from a directory
    pub fn load_from_dir<P: AsRef<Path>>(dir: P) -> Result<Self> {
        let path = dir.as_ref().join("tako.toml");
        if !path.exists() {
            return Err(ConfigError::Validation(format!(
                "Missing tako.toml at {}. Run 'tako init' first.",
                path.display()
            )));
        }

        Self::load_from_file(&path)
    }

    /// Load tako.toml from a specific file
    pub fn load_from_file<P: AsRef<Path>>(path: P) -> Result<Self> {
        let content = fs::read_to_string(path.as_ref())
            .map_err(|e| ConfigError::FileRead(path.as_ref().to_path_buf(), e))?;
        let config = Self::parse(&content)?;
        config.validate_required_fields()?;
        Ok(config)
    }

    /// Parse tako.toml content
    pub fn parse(content: &str) -> Result<Self> {
        if content.trim().is_empty() {
            return Ok(Self::default());
        }

        // First parse into a raw Value to handle the dynamic server configs
        let raw: toml::Value = toml::from_str(content)?;

        if raw.get("dist").is_some() {
            return Err(ConfigError::Validation(
                "'dist' is no longer supported; set `main` and [build] settings instead"
                    .to_string(),
            ));
        }
        if raw.get("assets").is_some() {
            return Err(ConfigError::Validation(
                "'assets' is no longer supported at the top level; use [build].assets instead"
                    .to_string(),
            ));
        }

        let mut config = TakoToml::default();

        // Parse top-level metadata
        config.name = parse_optional_string(&raw, "name")?;
        config.main = parse_optional_string(&raw, "main")?;
        config.build = parse_build_config(&raw)?;

        // Parse [vars] section (global) and [vars.*] sections (per-environment)
        if let Some(vars) = raw.get("vars")
            && let Some(table) = vars.as_table()
        {
            for (key, value) in table {
                if let Some(s) = value.as_str() {
                    // Direct string value - global var
                    config.vars.insert(key.clone(), s.to_string());
                } else if let Some(nested_table) = value.as_table() {
                    // Nested table - per-environment vars [vars.production], etc.
                    let mut env_vars = HashMap::new();
                    for (var_name, var_value) in nested_table {
                        if let Some(s) = var_value.as_str() {
                            env_vars.insert(var_name.clone(), s.to_string());
                        }
                    }
                    config.vars_per_env.insert(key.clone(), env_vars);
                }
            }
        }

        // Parse [envs.*] sections
        if let Some(envs) = raw.get("envs")
            && let Some(table) = envs.as_table()
        {
            for (env_name, env_value) in table {
                let env_config: EnvConfig = toml::from_str(&toml::to_string(env_value)?)?;
                config.envs.insert(env_name.clone(), env_config);
            }
        }

        // Parse [servers] section - both defaults and per-server configs
        if let Some(servers) = raw.get("servers")
            && let Some(table) = servers.as_table()
        {
            for (key, value) in table {
                match key.as_str() {
                    // These are default values in [servers]
                    "instances" => {
                        if let Some(v) = value.as_integer() {
                            config.server_defaults.instances = v as u8;
                        }
                    }
                    "port" => {
                        if let Some(v) = value.as_integer() {
                            config.server_defaults.port = v as u16;
                        }
                    }
                    "idle_timeout" => {
                        if let Some(v) = value.as_integer() {
                            config.server_defaults.idle_timeout = v as u32;
                        }
                    }
                    // Anything else is a [servers.{name}] section
                    server_name => {
                        if value.is_table() {
                            let server_config: ServerConfig =
                                toml::from_str(&toml::to_string(value)?)?;
                            config
                                .servers
                                .insert(server_name.to_string(), server_config);
                        }
                    }
                }
            }
        }

        config.validate()?;
        Ok(config)
    }

    /// Validate the configuration
    pub fn validate(&self) -> Result<()> {
        // Validate app name if specified
        if let Some(name) = &self.name {
            validate_app_name(name)?;
        }

        if let Some(main) = &self.main
            && main.trim().is_empty()
        {
            return Err(ConfigError::Validation("main cannot be empty".to_string()));
        }

        if let Some(preset) = &self.build.preset
            && preset.trim().is_empty()
        {
            return Err(ConfigError::Validation(
                "build.preset cannot be empty".to_string(),
            ));
        }

        for include in &self.build.include {
            validate_build_glob(include, "build.include")?;
        }
        for exclude in &self.build.exclude {
            validate_build_glob(exclude, "build.exclude")?;
        }
        for asset_path in &self.build.assets {
            validate_asset_path(asset_path)?;
        }

        // Validate each environment
        for (env_name, env_config) in &self.envs {
            let is_development = env_name == "development";

            // Cannot have both route and routes
            if env_config.route.is_some() && env_config.routes.is_some() {
                return Err(ConfigError::Validation(format!(
                    "Environment '{}' cannot have both 'route' and 'routes'",
                    env_name
                )));
            }

            if !is_development && env_config.route.is_none() && env_config.routes.is_none() {
                return Err(ConfigError::Validation(format!(
                    "Environment '{}' must define either 'route' or 'routes'",
                    env_name
                )));
            }

            if let Some(routes) = &env_config.routes
                && routes.is_empty()
                && !is_development
            {
                return Err(ConfigError::Validation(format!(
                    "Environment '{}' has empty 'routes'; define at least one route",
                    env_name
                )));
            }

            // Validate route patterns
            if let Some(route) = &env_config.route {
                validate_route_pattern(route)?;
            }
            if let Some(routes) = &env_config.routes {
                for route in routes {
                    validate_route_pattern(route)?;
                }
            }
        }

        // Validate default port
        if self.server_defaults.port == 0 {
            return Err(ConfigError::Validation("Port cannot be 0".to_string()));
        }

        // Validate each server config
        for (server_name, server_config) in &self.servers {
            // Validate server name format
            validate_server_name(server_name)?;

            // Validate that referenced environment exists (if we have envs defined)
            if !self.envs.is_empty()
                && !self.envs.contains_key(&server_config.env)
                && server_config.env != "production"
            {
                return Err(ConfigError::Validation(format!(
                    "Server '{}' references unknown environment '{}'",
                    server_name, server_config.env
                )));
            }

            // Validate port if overridden
            if let Some(port) = server_config.port
                && port == 0
            {
                return Err(ConfigError::Validation(format!(
                    "Server '{}' has invalid port 0",
                    server_name
                )));
            }
        }

        Ok(())
    }

    fn validate_required_fields(&self) -> Result<()> {
        let Some(name) = self.name.as_deref() else {
            return Err(ConfigError::Validation(
                "Missing top-level `name` in tako.toml. Run `tako init` and set `name`."
                    .to_string(),
            ));
        };
        if name.trim().is_empty() {
            return Err(ConfigError::Validation("name cannot be empty".to_string()));
        }
        validate_app_name(name)?;
        Ok(())
    }

    /// Get servers for a specific environment
    pub fn get_servers_for_env(&self, env_name: &str) -> Vec<&str> {
        self.servers
            .iter()
            .filter(|(_, config)| config.env == env_name)
            .map(|(name, _)| name.as_str())
            .collect()
    }

    /// Get effective instances for a server (with defaults applied)
    pub fn get_effective_instances(&self, server_name: &str) -> u8 {
        self.servers
            .get(server_name)
            .and_then(|s| s.instances)
            .unwrap_or(self.server_defaults.instances)
    }

    /// Get effective port for a server (with defaults applied)
    pub fn get_effective_port(&self, server_name: &str) -> u16 {
        self.servers
            .get(server_name)
            .and_then(|s| s.port)
            .unwrap_or(self.server_defaults.port)
    }

    /// Get effective idle_timeout for a server (with defaults applied)
    pub fn get_effective_idle_timeout(&self, server_name: &str) -> u32 {
        self.servers
            .get(server_name)
            .and_then(|s| s.idle_timeout)
            .unwrap_or(self.server_defaults.idle_timeout)
    }

    /// Get merged vars for an environment (global + per-env)
    pub fn get_merged_vars(&self, env_name: &str) -> HashMap<String, String> {
        let mut merged = self.vars.clone();
        if let Some(env_vars) = self.vars_per_env.get(env_name) {
            merged.extend(env_vars.iter().map(|(k, v)| (k.clone(), v.clone())));
        }
        merged
    }

    /// Check if tako.toml exists in a directory
    pub fn exists_in_dir<P: AsRef<Path>>(dir: P) -> bool {
        dir.as_ref().join("tako.toml").exists()
    }

    /// Get routes for an environment
    pub fn get_routes(&self, env_name: &str) -> Option<Vec<String>> {
        self.envs.get(env_name).and_then(|env| {
            if let Some(route) = &env.route {
                Some(vec![route.clone()])
            } else {
                env.routes.clone()
            }
        })
    }

    /// Get all environment names
    pub fn get_environment_names(&self) -> Vec<String> {
        self.envs.keys().cloned().collect()
    }

    /// Upsert `[servers.<name>] env = "<env>"` in `tako.toml` under the given directory.
    pub fn upsert_server_env_in_dir<P: AsRef<Path>>(
        dir: P,
        server_name: &str,
        env: &str,
    ) -> Result<()> {
        let path = dir.as_ref().join("tako.toml");
        Self::upsert_server_env_in_file(path, server_name, env)
    }

    fn upsert_server_env_in_file<P: AsRef<Path>>(
        path: P,
        server_name: &str,
        env: &str,
    ) -> Result<()> {
        let path = path.as_ref();
        let mut doc = load_or_create_toml_document(path)?;
        let root = doc
            .as_table_mut()
            .ok_or_else(|| ConfigError::Validation("tako.toml must be a TOML table".to_string()))?;

        let servers = root
            .entry("servers")
            .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
            .as_table_mut()
            .ok_or_else(|| {
                ConfigError::Validation(
                    "Invalid [servers] section: expected table structure".to_string(),
                )
            })?;

        match servers.get_mut(server_name) {
            Some(existing) => {
                let Some(server_table) = existing.as_table_mut() else {
                    return Err(ConfigError::Validation(format!(
                        "Cannot map server '{}': [servers.{}] is not a table",
                        server_name, server_name
                    )));
                };
                server_table.insert("env".to_string(), toml::Value::String(env.to_string()));
            }
            None => {
                let mut server_table = toml::map::Map::new();
                server_table.insert("env".to_string(), toml::Value::String(env.to_string()));
                servers.insert(server_name.to_string(), toml::Value::Table(server_table));
            }
        }

        let rendered = toml::to_string_pretty(&doc)
            .map_err(|e| ConfigError::Validation(format!("Failed to render tako.toml: {}", e)))?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| ConfigError::FileWrite(parent.to_path_buf(), e))?;
        }
        fs::write(path, rendered).map_err(|e| ConfigError::FileWrite(path.to_path_buf(), e))?;
        Ok(())
    }
}

fn load_or_create_toml_document(path: &Path) -> Result<toml::Value> {
    if !path.exists() {
        return Ok(toml::Value::Table(toml::map::Map::new()));
    }

    let content =
        fs::read_to_string(path).map_err(|e| ConfigError::FileRead(path.to_path_buf(), e))?;
    if content.trim().is_empty() {
        return Ok(toml::Value::Table(toml::map::Map::new()));
    }

    toml::from_str::<toml::Value>(&content).map_err(ConfigError::TomlParse)
}

fn parse_optional_string(raw: &toml::Value, key: &str) -> Result<Option<String>> {
    let Some(value) = raw.get(key) else {
        return Ok(None);
    };
    value
        .as_str()
        .map(|s| Some(s.to_string()))
        .ok_or_else(|| ConfigError::Validation(format!("'{}' must be a string", key)))
}

fn parse_build_config(raw: &toml::Value) -> Result<BuildConfig> {
    let Some(value) = raw.get("build") else {
        return Ok(BuildConfig::default());
    };

    let table = value
        .as_table()
        .ok_or_else(|| ConfigError::Validation("'build' must be a table ([build])".to_string()))?;
    let table_value = toml::Value::Table(table.clone());

    let preset = parse_optional_string(&table_value, "preset")?;
    let include = parse_string_array(&table_value, "include")?.unwrap_or_default();
    let exclude = parse_string_array(&table_value, "exclude")?.unwrap_or_default();
    let assets = parse_string_array(&table_value, "assets")?.unwrap_or_default();

    Ok(BuildConfig {
        preset,
        include,
        exclude,
        assets,
    })
}

fn parse_string_array(raw: &toml::Value, key: &str) -> Result<Option<Vec<String>>> {
    let Some(value) = raw.get(key) else {
        return Ok(None);
    };
    let arr = value
        .as_array()
        .ok_or_else(|| ConfigError::Validation(format!("'{}' must be an array of strings", key)))?;
    let mut out = Vec::with_capacity(arr.len());
    for item in arr {
        let Some(s) = item.as_str() else {
            return Err(ConfigError::Validation(format!(
                "'{}' must be an array of strings",
                key
            )));
        };
        out.push(s.to_string());
    }
    Ok(Some(out))
}

fn validate_build_glob(pattern: &str, field: &str) -> Result<()> {
    let trimmed = pattern.trim();
    if trimmed.is_empty() {
        return Err(ConfigError::Validation(format!(
            "{field} entries cannot be empty"
        )));
    }

    let path = Path::new(trimmed);
    if path.is_absolute() {
        return Err(ConfigError::Validation(format!(
            "{field} entry '{}' must be relative to project root",
            pattern
        )));
    }
    if path
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(ConfigError::Validation(format!(
            "{field} entry '{}' must not contain '..'",
            pattern
        )));
    }

    Ok(())
}

fn validate_asset_path(asset_path: &str) -> Result<()> {
    let trimmed = asset_path.trim();
    if trimmed.is_empty() {
        return Err(ConfigError::Validation(
            "build.assets entry cannot be empty".to_string(),
        ));
    }

    let path = Path::new(trimmed);
    if path.is_absolute() {
        return Err(ConfigError::Validation(format!(
            "build.assets entry '{}' must be relative to project root",
            asset_path
        )));
    }

    if path
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(ConfigError::Validation(format!(
            "build.assets entry '{}' must not contain '..'",
            asset_path
        )));
    }

    Ok(())
}

/// Validate app name format
fn validate_app_name(name: &str) -> Result<()> {
    if name.is_empty() {
        return Err(ConfigError::Validation(
            "App name cannot be empty".to_string(),
        ));
    }

    if name.len() > 63 {
        return Err(ConfigError::Validation(
            "App name cannot exceed 63 characters".to_string(),
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
            "App name must start with a lowercase letter".to_string(),
        ));
    }

    // Only lowercase letters, numbers, and hyphens
    for c in name.chars() {
        if !c.is_ascii_lowercase() && !c.is_ascii_digit() && c != '-' {
            return Err(ConfigError::Validation(format!(
                "App name can only contain lowercase letters, numbers, and hyphens. Found: '{}'",
                c
            )));
        }
    }

    // Cannot end with hyphen
    if name.ends_with('-') {
        return Err(ConfigError::Validation(
            "App name cannot end with a hyphen".to_string(),
        ));
    }

    Ok(())
}

/// Validate server name format (same rules as app name)
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

/// Validate route pattern format
fn validate_route_pattern(pattern: &str) -> Result<()> {
    if pattern.is_empty() {
        return Err(ConfigError::InvalidRoutePattern(
            "Route pattern cannot be empty".to_string(),
        ));
    }

    // Basic validation - routes can be:
    // - Exact hostname: api.example.com
    // - Wildcard subdomain: *.example.com
    // - Path-based: example.com/api/*
    // - Combined: *.example.com/admin/*

    // Check for invalid characters
    for c in pattern.chars() {
        if !c.is_ascii_alphanumeric() && c != '.' && c != '-' && c != '*' && c != '/' {
            return Err(ConfigError::InvalidRoutePattern(format!(
                "Invalid character in route pattern: '{}'",
                c
            )));
        }
    }

    // Wildcard must be at start of a segment
    if pattern.contains('*') {
        let parts: Vec<&str> = pattern.split('/').collect();
        let hostname = parts[0];

        // Check hostname wildcards
        if hostname.contains('*') && !hostname.starts_with("*.") {
            return Err(ConfigError::InvalidRoutePattern(
                "Wildcard in hostname must be at the start (e.g., *.example.com)".to_string(),
            ));
        }

        // Check path wildcards
        for part in parts.iter().skip(1) {
            if part.contains('*') && *part != "*" {
                return Err(ConfigError::InvalidRoutePattern(
                    "Wildcard in path must be a complete segment (e.g., /api/*)".to_string(),
                ));
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // ==================== Parsing Tests ====================

    #[test]
    fn test_parse_empty_file() {
        let config = TakoToml::parse("").unwrap();
        assert_eq!(config, TakoToml::default());
    }

    #[test]
    fn test_parse_top_level_metadata_fields() {
        let toml = r#"
name = "my-app"
main = "server/index.mjs"
[build]
preset = "bun"
"#;
        let config = TakoToml::parse(toml).unwrap();
        assert_eq!(config.name, Some("my-app".to_string()));
        assert_eq!(config.main, Some("server/index.mjs".to_string()));
        assert_eq!(config.build.preset, Some("bun".to_string()));
    }

    #[test]
    fn test_parse_build_arrays() {
        let toml = r#"
[build]
include = [".output/**", "dist/**"]
exclude = ["**/*.map"]
assets = ["public-assets", "shared/images"]
"#;
        let config = TakoToml::parse(toml).unwrap();
        assert_eq!(
            config.build.include,
            vec![".output/**".to_string(), "dist/**".to_string()]
        );
        assert_eq!(config.build.exclude, vec!["**/*.map".to_string()]);
        assert_eq!(
            config.build.assets,
            vec!["public-assets".to_string(), "shared/images".to_string()]
        );
    }

    #[test]
    fn test_parse_global_vars() {
        let toml = r#"
[vars]
LOG_LEVEL = "info"
API_URL = "https://api.example.com"
"#;
        let config = TakoToml::parse(toml).unwrap();
        assert_eq!(config.vars.get("LOG_LEVEL"), Some(&"info".to_string()));
        assert_eq!(
            config.vars.get("API_URL"),
            Some(&"https://api.example.com".to_string())
        );
    }

    #[test]
    fn test_parse_single_route() {
        let toml = r#"
[envs.production]
route = "api.example.com"
"#;
        let config = TakoToml::parse(toml).unwrap();
        let env = config.envs.get("production").unwrap();
        assert_eq!(env.route, Some("api.example.com".to_string()));
        assert_eq!(env.routes, None);
    }

    #[test]
    fn test_parse_env_without_routes_is_rejected() {
        let toml = r#"
[envs.production]
LOG_LEVEL = "info"
"#;
        let err = TakoToml::parse(&toml).unwrap_err();
        assert!(
            err.to_string()
                .contains("must define either 'route' or 'routes'")
        );
    }

    #[test]
    fn test_parse_development_env_without_routes_is_allowed() {
        let toml = r#"
[envs.development]
LOG_LEVEL = "debug"
"#;
        let config = TakoToml::parse(toml).unwrap();
        let env = config.envs.get("development").unwrap();
        assert_eq!(env.route, None);
        assert_eq!(env.routes, None);
    }

    #[test]
    fn test_parse_env_with_empty_routes_is_rejected() {
        let toml = r#"
[envs.production]
routes = []
"#;
        let err = TakoToml::parse(&toml).unwrap_err();
        assert!(err.to_string().contains("routes"));
    }

    #[test]
    fn test_parse_development_env_with_empty_routes_is_allowed() {
        let toml = r#"
[envs.development]
routes = []
"#;
        let config = TakoToml::parse(toml).unwrap();
        let env = config.envs.get("development").unwrap();
        assert_eq!(env.route, None);
        assert_eq!(env.routes, Some(Vec::new()));
    }

    #[test]
    fn test_parse_multiple_routes() {
        let toml = r#"
[envs.production]
routes = ["api.example.com", "*.api.example.com", "example.com/api/*"]
"#;
        let config = TakoToml::parse(toml).unwrap();
        let env = config.envs.get("production").unwrap();
        assert_eq!(env.route, None);
        assert_eq!(
            env.routes,
            Some(vec![
                "api.example.com".to_string(),
                "*.api.example.com".to_string(),
                "example.com/api/*".to_string(),
            ])
        );
    }

    #[test]
    fn test_parse_server_defaults() {
        let toml = r#"
[servers]
instances = 3
port = 8080
idle_timeout = 600
"#;
        let config = TakoToml::parse(toml).unwrap();
        assert_eq!(config.server_defaults.instances, 3);
        assert_eq!(config.server_defaults.port, 8080);
        assert_eq!(config.server_defaults.idle_timeout, 600);
    }

    #[test]
    fn test_default_server_values() {
        let config = TakoToml::default();
        assert_eq!(config.server_defaults.instances, 0);
        assert_eq!(config.server_defaults.port, 80);
        assert_eq!(config.server_defaults.idle_timeout, 300);
    }

    #[test]
    fn test_parse_complete_config() {
        let toml = r#"
name = "my-api"
main = "server/index.mjs"

[build]
preset = "bun"
include = ["dist/**"]
exclude = ["**/*.map"]
assets = ["public", ".output/public"]

[vars]
LOG_LEVEL = "info"

[envs.production]
route = "api.example.com"

[envs.staging]
routes = ["staging.example.com", "*.staging.example.com"]

[servers]
instances = 2
port = 80
"#;
        let config = TakoToml::parse(toml).unwrap();

        assert_eq!(config.name, Some("my-api".to_string()));
        assert_eq!(config.main, Some("server/index.mjs".to_string()));
        assert_eq!(config.build.preset, Some("bun".to_string()));
        assert_eq!(config.build.include, vec!["dist/**".to_string()]);
        assert_eq!(config.build.exclude, vec!["**/*.map".to_string()]);
        assert_eq!(
            config.build.assets,
            vec!["public".to_string(), ".output/public".to_string()]
        );
        assert_eq!(config.vars.get("LOG_LEVEL"), Some(&"info".to_string()));

        let prod = config.envs.get("production").unwrap();
        assert_eq!(prod.route, Some("api.example.com".to_string()));

        let staging = config.envs.get("staging").unwrap();
        assert_eq!(staging.routes.as_ref().unwrap().len(), 2);

        assert_eq!(config.server_defaults.instances, 2);
    }

    // ==================== Validation Tests ====================

    #[test]
    fn test_validate_app_name_valid() {
        assert!(validate_app_name("my-app").is_ok());
        assert!(validate_app_name("api").is_ok());
        assert!(validate_app_name("my-app-123").is_ok());
        assert!(validate_app_name("a").is_ok());
    }

    #[test]
    fn test_validate_app_name_empty() {
        assert!(validate_app_name("").is_err());
    }

    #[test]
    fn test_validate_app_name_too_long() {
        let long_name = "a".repeat(64);
        assert!(validate_app_name(&long_name).is_err());
    }

    #[test]
    fn test_validate_app_name_must_start_lowercase() {
        assert!(validate_app_name("My-app").is_err());
        assert!(validate_app_name("1app").is_err());
        assert!(validate_app_name("-app").is_err());
    }

    #[test]
    fn test_validate_app_name_invalid_chars() {
        assert!(validate_app_name("my_app").is_err());
        assert!(validate_app_name("my.app").is_err());
        assert!(validate_app_name("my app").is_err());
        assert!(validate_app_name("MY-APP").is_err());
    }

    #[test]
    fn test_validate_app_name_cannot_end_with_hyphen() {
        assert!(validate_app_name("my-app-").is_err());
    }

    #[test]
    fn test_validate_route_pattern_valid() {
        assert!(validate_route_pattern("api.example.com").is_ok());
        assert!(validate_route_pattern("*.example.com").is_ok());
        assert!(validate_route_pattern("example.com/api/*").is_ok());
        assert!(validate_route_pattern("*.example.com/admin/*").is_ok());
    }

    #[test]
    fn test_validate_route_pattern_empty() {
        assert!(validate_route_pattern("").is_err());
    }

    #[test]
    fn test_validate_route_pattern_invalid_wildcard() {
        assert!(validate_route_pattern("api*.example.com").is_err());
        assert!(validate_route_pattern("example.com/api*").is_err());
    }

    #[test]
    fn test_validate_route_pattern_invalid_chars() {
        assert!(validate_route_pattern("api@example.com").is_err());
        assert!(validate_route_pattern("api example.com").is_err());
    }

    #[test]
    fn test_cannot_have_both_route_and_routes() {
        let toml = r#"
[envs.production]
route = "api.example.com"
routes = ["staging.example.com"]
"#;
        assert!(TakoToml::parse(toml).is_err());
    }

    #[test]
    fn test_validate_port_cannot_be_zero() {
        let toml = r#"
[servers]
port = 0
"#;
        assert!(TakoToml::parse(toml).is_err());
    }

    #[test]
    fn test_validate_tako_build_assets_rejects_absolute_path() {
        let toml = r#"
[build]
assets = ["/tmp/assets"]
"#;
        let err = TakoToml::parse(toml).unwrap_err();
        assert!(
            err.to_string()
                .contains("build.assets entry '/tmp/assets' must be relative to project root")
        );
    }

    #[test]
    fn test_validate_tako_build_assets_rejects_parent_directory_reference() {
        let toml = r#"
[build]
assets = ["../shared-assets"]
"#;
        let err = TakoToml::parse(toml).unwrap_err();
        assert!(
            err.to_string()
                .contains("build.assets entry '../shared-assets' must not contain '..'")
        );
    }

    #[test]
    fn test_validate_build_globs_reject_invalid_paths() {
        let absolute = r#"
[build]
include = ["/tmp/out/**"]
"#;
        let err = TakoToml::parse(absolute).unwrap_err();
        assert!(
            err.to_string()
                .contains("build.include entry '/tmp/out/**' must be relative to project root")
        );

        let parent = r#"
[build]
exclude = ["../secret/**"]
"#;
        let err = TakoToml::parse(parent).unwrap_err();
        assert!(
            err.to_string()
                .contains("build.exclude entry '../secret/**' must not contain '..'")
        );
    }

    #[test]
    fn test_parse_rejects_legacy_build_string_property() {
        let toml = r#"
build = "bun run build"
"#;
        let err = TakoToml::parse(toml).unwrap_err();
        assert!(err.to_string().contains("'build' must be a table"));
    }

    #[test]
    fn test_parse_rejects_legacy_dist_property() {
        let legacy_key = ["di", "st"].join("");
        let toml = format!(r#"{legacy_key} = ".tako/dist""#);
        let err = TakoToml::parse(&toml).unwrap_err();
        assert!(err.to_string().contains("'dist' is no longer supported"));
    }

    #[test]
    fn test_parse_rejects_legacy_top_level_assets_property() {
        let toml = r#"
assets = ["dist/client"]
"#;
        let err = TakoToml::parse(toml).unwrap_err();
        assert!(
            err.to_string()
                .contains("'assets' is no longer supported at the top level")
        );
    }

    #[test]
    fn test_validate_main_rejects_empty_value() {
        let toml = r#"
main = "   "
"#;
        let err = TakoToml::parse(toml).unwrap_err();
        assert!(err.to_string().contains("main cannot be empty"));
    }

    // ==================== Helper Method Tests ====================

    #[test]
    fn test_get_routes_single() {
        let toml = r#"
[envs.production]
route = "api.example.com"
"#;
        let config = TakoToml::parse(toml).unwrap();
        let routes = config.get_routes("production").unwrap();
        assert_eq!(routes, vec!["api.example.com"]);
    }

    #[test]
    fn test_get_routes_multiple() {
        let toml = r#"
[envs.production]
routes = ["api.example.com", "www.example.com"]
"#;
        let config = TakoToml::parse(toml).unwrap();
        let routes = config.get_routes("production").unwrap();
        assert_eq!(routes, vec!["api.example.com", "www.example.com"]);
    }

    #[test]
    fn test_get_routes_nonexistent_env() {
        let config = TakoToml::default();
        assert!(config.get_routes("production").is_none());
    }

    #[test]
    fn test_load_from_dir_requires_tako_toml() {
        let temp = tempfile::TempDir::new().unwrap();
        let err = TakoToml::load_from_dir(temp.path()).unwrap_err();
        assert!(err.to_string().contains("tako.toml"));
    }

    #[test]
    fn test_load_from_dir_requires_name() {
        let temp = tempfile::TempDir::new().unwrap();
        fs::write(
            temp.path().join("tako.toml"),
            r#"
[envs.production]
route = "prod.example.com"
"#,
        )
        .unwrap();

        let err = TakoToml::load_from_dir(temp.path()).unwrap_err();
        assert!(
            err.to_string()
                .contains("Missing top-level `name` in tako.toml")
        );
    }

    #[test]
    fn test_get_environment_names() {
        let toml = r#"
[envs.production]
route = "prod.example.com"

[envs.staging]
route = "staging.example.com"
"#;
        let config = TakoToml::parse(toml).unwrap();
        let mut names = config.get_environment_names();
        names.sort();
        assert_eq!(names, vec!["production", "staging"]);
    }

    // ==================== Error Handling Tests ====================

    #[test]
    fn test_invalid_toml_syntax() {
        let toml = r#"
[tako
name = "broken"
"#;
        assert!(TakoToml::parse(toml).is_err());
    }

    #[test]
    fn test_wrong_type() {
        let toml = r#"
name = 123
"#;
        assert!(TakoToml::parse(toml).is_err());
    }

    // ==================== Per-Environment Vars Tests ====================

    #[test]
    fn test_parse_per_env_vars() {
        let toml = r#"
[vars]
LOG_LEVEL = "info"

[vars.production]
LOG_LEVEL = "warn"
DATABASE_URL = "postgres://prod"

[vars.staging]
DATABASE_URL = "postgres://staging"
"#;
        let config = TakoToml::parse(toml).unwrap();

        // Global var
        assert_eq!(config.vars.get("LOG_LEVEL"), Some(&"info".to_string()));

        // Per-env vars
        let prod_vars = config.vars_per_env.get("production").unwrap();
        assert_eq!(prod_vars.get("LOG_LEVEL"), Some(&"warn".to_string()));
        assert_eq!(
            prod_vars.get("DATABASE_URL"),
            Some(&"postgres://prod".to_string())
        );

        let staging_vars = config.vars_per_env.get("staging").unwrap();
        assert_eq!(
            staging_vars.get("DATABASE_URL"),
            Some(&"postgres://staging".to_string())
        );
    }

    #[test]
    fn test_get_merged_vars() {
        let toml = r#"
[vars]
LOG_LEVEL = "info"
API_URL = "https://api.example.com"

[vars.production]
LOG_LEVEL = "warn"
DATABASE_URL = "postgres://prod"
"#;
        let config = TakoToml::parse(toml).unwrap();

        let merged = config.get_merged_vars("production");
        assert_eq!(merged.get("LOG_LEVEL"), Some(&"warn".to_string())); // overridden
        assert_eq!(
            merged.get("API_URL"),
            Some(&"https://api.example.com".to_string())
        ); // inherited
        assert_eq!(
            merged.get("DATABASE_URL"),
            Some(&"postgres://prod".to_string())
        ); // env-specific
    }

    #[test]
    fn test_get_merged_vars_nonexistent_env() {
        let toml = r#"
[vars]
LOG_LEVEL = "info"
"#;
        let config = TakoToml::parse(toml).unwrap();

        let merged = config.get_merged_vars("nonexistent");
        assert_eq!(merged.get("LOG_LEVEL"), Some(&"info".to_string()));
        assert_eq!(merged.len(), 1);
    }

    // ==================== Per-Server Config Tests ====================

    #[test]
    fn test_parse_per_server_configs() {
        let toml = r#"
[envs.production]
route = "api.example.com"

[servers]
instances = 2
port = 80

[servers.la-prod]
env = "production"
instances = 4

[servers.nyc-prod]
env = "production"
port = 8080
"#;
        let config = TakoToml::parse(toml).unwrap();

        // Check defaults
        assert_eq!(config.server_defaults.instances, 2);
        assert_eq!(config.server_defaults.port, 80);

        // Check per-server configs
        let la = config.servers.get("la-prod").unwrap();
        assert_eq!(la.env, "production");
        assert_eq!(la.instances, Some(4));
        assert_eq!(la.port, None);

        let nyc = config.servers.get("nyc-prod").unwrap();
        assert_eq!(nyc.env, "production");
        assert_eq!(nyc.instances, None);
        assert_eq!(nyc.port, Some(8080));
    }

    #[test]
    fn test_get_servers_for_env() {
        let toml = r#"
[envs.production]
route = "prod.example.com"

[envs.staging]
route = "staging.example.com"

[servers.la-prod]
env = "production"

[servers.nyc-prod]
env = "production"

[servers.staging-server]
env = "staging"
"#;
        let config = TakoToml::parse(toml).unwrap();

        let prod_servers = config.get_servers_for_env("production");
        assert_eq!(prod_servers.len(), 2);
        assert!(prod_servers.contains(&"la-prod"));
        assert!(prod_servers.contains(&"nyc-prod"));

        let staging_servers = config.get_servers_for_env("staging");
        assert_eq!(staging_servers.len(), 1);
        assert!(staging_servers.contains(&"staging-server"));

        let dev_servers = config.get_servers_for_env("development");
        assert!(dev_servers.is_empty());
    }

    #[test]
    fn test_get_effective_values() {
        let toml = r#"
[envs.production]
route = "api.example.com"

[servers]
instances = 2
port = 80
idle_timeout = 300

[servers.la-prod]
env = "production"
instances = 4
port = 8080
idle_timeout = 600
"#;
        let config = TakoToml::parse(toml).unwrap();

        // Server with overrides
        assert_eq!(config.get_effective_instances("la-prod"), 4);
        assert_eq!(config.get_effective_port("la-prod"), 8080);
        assert_eq!(config.get_effective_idle_timeout("la-prod"), 600);

        // Non-existent server falls back to defaults
        assert_eq!(config.get_effective_instances("unknown"), 2);
        assert_eq!(config.get_effective_port("unknown"), 80);
        assert_eq!(config.get_effective_idle_timeout("unknown"), 300);
    }

    #[test]
    fn test_server_config_partial_overrides() {
        let toml = r#"
[envs.production]
route = "api.example.com"

[servers]
instances = 2
port = 80
idle_timeout = 300

[servers.la-prod]
env = "production"
instances = 4
"#;
        let config = TakoToml::parse(toml).unwrap();

        // Only instances is overridden
        assert_eq!(config.get_effective_instances("la-prod"), 4);
        assert_eq!(config.get_effective_port("la-prod"), 80); // default
        assert_eq!(config.get_effective_idle_timeout("la-prod"), 300); // default
    }

    #[test]
    fn test_server_config_invalid_name() {
        let toml = r#"
[envs.production]
route = "api.example.com"

[servers.INVALID_NAME]
env = "production"
"#;
        assert!(TakoToml::parse(toml).is_err());
    }

    #[test]
    fn test_server_config_unknown_env() {
        let toml = r#"
[envs.production]
route = "api.example.com"

[servers.la-prod]
env = "nonexistent"
"#;
        assert!(TakoToml::parse(toml).is_err());
    }

    #[test]
    fn test_server_config_implicit_production_env_allowed() {
        let toml = r#"
[envs.staging]
route = "staging.example.com"

[servers.la-prod]
env = "production"
"#;
        assert!(TakoToml::parse(toml).is_ok());
    }

    #[test]
    fn test_server_config_invalid_port() {
        let toml = r#"
[envs.production]
route = "api.example.com"

[servers.la-prod]
env = "production"
port = 0
"#;
        assert!(TakoToml::parse(toml).is_err());
    }

    // ==================== Server Name Validation Tests ====================

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
}

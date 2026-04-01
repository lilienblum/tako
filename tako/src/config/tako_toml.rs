use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Component, Path};

use crate::build::BuildAdapter;

use super::error::{ConfigError, Result};

/// Root configuration from tako.toml
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Config {
    /// Application name (required; stable identity for deploy paths and hostnames)
    pub name: Option<String>,

    /// Build runtime override used for default preset selection when `preset` is omitted.
    pub runtime: Option<String>,

    /// Pinned runtime version (for example: "1.2.3"). Used by deploy instead of auto-detecting.
    pub runtime_version: Option<String>,

    /// Package manager override (e.g. "npm", "pnpm", "yarn", "bun").
    /// Auto-detected from package.json `packageManager` field or lockfiles if omitted.
    pub package_manager: Option<String>,

    /// App preset reference (e.g. "tanstack-start"). Provides `main` and `assets` defaults.
    pub preset: Option<String>,

    /// Custom dev command override (e.g. `["vite", "dev"]`).
    #[serde(default)]
    pub dev: Vec<String>,

    /// Runtime entrypoint override relative to project root
    pub main: Option<String>,

    /// Asset directories to include in the deploy artifact (e.g. ["dist/client"]).
    #[serde(default)]
    pub assets: Vec<String>,

    /// Build settings for deploy artifact generation.
    #[serde(default)]
    pub build: BuildConfig,

    /// Multi-stage build (mutually exclusive with `build.run`).
    #[serde(default)]
    pub build_stages: Vec<BuildStage>,

    /// [vars] section - global environment variables
    #[serde(default)]
    pub vars: HashMap<String, String>,

    /// [vars.*] sections - per-environment variables
    #[serde(default)]
    pub vars_per_env: HashMap<String, HashMap<String, String>>,

    /// [envs.*] sections - environment configurations
    #[serde(default)]
    pub envs: HashMap<String, EnvConfig>,
}

/// Backward-compatible alias.
pub type TakoToml = Config;

/// Build configuration from [build].
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct BuildConfig {
    /// Build command (e.g. "vinxi build", "bun run build").
    pub run: Option<String>,

    /// Optional pre-build install command (e.g. "bun install").
    pub install: Option<String>,

    /// Working directory for build commands, relative to the project root.
    pub cwd: Option<String>,

    /// Additional file globs to include in the deploy artifact.
    #[serde(default)]
    pub include: Vec<String>,

    /// File globs to exclude from the deploy artifact.
    #[serde(default)]
    pub exclude: Vec<String>,
}

/// Custom build stage from [[build_stages]].
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct BuildStage {
    /// Optional display label.
    #[serde(default)]
    pub name: Option<String>,

    /// Optional working directory relative to tako.toml location.
    /// Allows ".." for monorepo traversal (guarded against escaping workspace root).
    #[serde(default)]
    pub cwd: Option<String>,

    /// Optional preparatory command run before `run`.
    #[serde(default)]
    pub install: Option<String>,

    /// Required stage command.
    pub run: String,

    /// File globs to include in the deploy artifact, relative to this stage's `cwd`.
    /// Stages without `include` are intermediate (contribute nothing to the artifact).
    #[serde(default)]
    pub include: Vec<String>,
}

/// Environment configuration from [envs.*]
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct EnvConfig {
    /// Single route (mutually exclusive with routes)
    pub route: Option<String>,

    /// Multiple routes (mutually exclusive with route)
    pub routes: Option<Vec<String>>,

    /// Servers assigned to this environment.
    #[serde(default)]
    pub servers: Vec<String>,

    /// Idle timeout in seconds (300 = 5 minutes).
    #[serde(default = "default_idle_timeout")]
    pub idle_timeout: u32,

    /// Application log level for this environment.
    /// Allowed values: "debug", "info", "warn", "error".
    /// Default: "debug" for development, "info" for all others.
    pub log_level: Option<String>,
}

fn default_idle_timeout() -> u32 {
    300
}

/// Allowed values for the `log_level` config field.
const ALLOWED_LOG_LEVELS: &[&str] = &["debug", "info", "warn", "error"];

/// Resolve the effective app log level for an environment.
/// Explicit `log_level` in config takes precedence; otherwise:
/// - "development" => "debug"
/// - everything else => "info"
pub fn resolve_app_log_level<'a>(env_config: Option<&'a EnvConfig>, env_name: &'a str) -> &'a str {
    if let Some(config) = env_config
        && let Some(ref level) = config.log_level
    {
        return level;
    }
    if env_name == "development" {
        "debug"
    } else {
        "info"
    }
}

impl Config {
    /// Load tako.toml from a directory
    pub fn load_from_dir<P: AsRef<Path>>(dir: P) -> Result<Self> {
        let path = dir.as_ref().join("tako.toml");
        if !path.exists() {
            return Err(ConfigError::Validation(format!(
                "tako.toml not found at {}",
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
        Ok(config)
    }

    /// Parse tako.toml content
    pub fn parse(content: &str) -> Result<Self> {
        if content.trim().is_empty() {
            return Ok(Self::default());
        }

        // First parse into a raw Value so the current schema can be validated.
        let raw: toml::Value = toml::from_str(content)?;
        validate_top_level_keys(&raw)?;

        // Parse top-level metadata
        let name = parse_optional_string(&raw, "name")?;
        let main = parse_optional_string(&raw, "main")?;
        let runtime = parse_optional_string(&raw, "runtime")?;
        let runtime_version = parse_optional_string(&raw, "runtime_version")?;
        let package_manager = parse_optional_string(&raw, "package_manager")?;
        let preset = parse_optional_string(&raw, "preset")?;
        let assets = parse_string_array(&raw, "assets")?.unwrap_or_default();
        let build = parse_build_config(&raw)?;
        let build_stages = parse_build_stages(&raw)?;
        let mut config = Config {
            name,
            main,
            runtime,
            runtime_version,
            package_manager,
            preset,
            assets,
            build,
            build_stages,
            ..Config::default()
        };

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

        if let Some(preset) = &self.preset
            && preset.trim().is_empty()
        {
            return Err(ConfigError::Validation(
                "preset cannot be empty".to_string(),
            ));
        }
        if let Some(preset) = &self.preset {
            let trimmed = preset.trim();
            if trimmed.starts_with("github:") {
                return Err(ConfigError::Validation(
                    "github preset references are not supported; use official aliases only."
                        .to_string(),
                ));
            }
            if trimmed.contains(':') {
                return Err(ConfigError::Validation(
                    "preset must be an official alias (for example `tanstack-start`); ':' references are not supported."
                        .to_string(),
                ));
            }
            if !trimmed.is_empty() && trimmed.contains('/') {
                return Err(ConfigError::Validation(
                    "preset must not include runtime namespace; set top-level `runtime` and use a local preset name (for example `preset = \"tanstack-start\"`).".to_string(),
                ));
            }
        }
        if let Some(runtime) = &self.runtime {
            let trimmed = runtime.trim();
            if trimmed.is_empty() {
                return Err(ConfigError::Validation(
                    "runtime cannot be empty".to_string(),
                ));
            }
            if BuildAdapter::from_id(trimmed).is_none() {
                return Err(ConfigError::Validation(
                    "runtime must be one of: bun, node, deno, go".to_string(),
                ));
            }
        }
        for asset_path in &self.assets {
            validate_asset_path(asset_path)?;
        }
        if let Some(cwd) = &self.build.cwd {
            validate_relative_dir(cwd, "build.cwd")?;
        }
        for include in &self.build.include {
            validate_build_glob(include, "build.include")?;
        }
        for exclude in &self.build.exclude {
            validate_build_glob(exclude, "build.exclude")?;
        }
        // Mutual exclusion: [build] and [[build_stages]] cannot both be set
        let has_build_run = self
            .build
            .run
            .as_deref()
            .is_some_and(|r| !r.trim().is_empty());
        if has_build_run && !self.build_stages.is_empty() {
            return Err(ConfigError::Validation(
                "Cannot use both [build] with 'run' and [[build_stages]]; they are mutually exclusive."
                    .to_string(),
            ));
        }
        if !self.build_stages.is_empty()
            && (!self.build.include.is_empty() || !self.build.exclude.is_empty())
        {
            return Err(ConfigError::Validation(
                "Cannot use [build] include/exclude with [[build_stages]]; use per-stage include instead."
                    .to_string(),
            ));
        }
        for (index, stage) in self.build_stages.iter().enumerate() {
            validate_build_stage(stage, index)?;
            for include in &stage.include {
                validate_build_glob(include, &format!("build_stages[{index}].include"))?;
            }
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
            if env_config.idle_timeout == 0 {
                return Err(ConfigError::Validation(format!(
                    "Environment '{}' has invalid idle_timeout 0",
                    env_name
                )));
            }
            for server_name in &env_config.servers {
                validate_server_name(server_name)?;
            }
            if let Some(ref log_level) = env_config.log_level
                && !ALLOWED_LOG_LEVELS.contains(&log_level.as_str())
            {
                return Err(ConfigError::Validation(format!(
                    "Invalid log_level \"{}\" in [envs.{}]. Allowed values: {}",
                    log_level,
                    env_name,
                    ALLOWED_LOG_LEVELS.join(", ")
                )));
            }
        }

        Ok(())
    }

    /// Get servers for a specific environment
    pub fn get_servers_for_env(&self, env_name: &str) -> Vec<&str> {
        self.envs
            .get(env_name)
            .map(|env| env.servers.iter().map(String::as_str).collect())
            .unwrap_or_default()
    }

    /// Get effective idle timeout for an environment.
    pub fn get_idle_timeout(&self, env_name: &str) -> u32 {
        self.envs
            .get(env_name)
            .map(|env| env.idle_timeout)
            .unwrap_or_else(default_idle_timeout)
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

    /// Check if a config file exists at an explicit path.
    pub fn exists_in_file<P: AsRef<Path>>(path: P) -> bool {
        path.as_ref().is_file()
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

    /// Add a server to `[envs.<name>].servers` in `tako.toml` under the given directory.
    pub fn upsert_server_env_in_dir<P: AsRef<Path>>(
        dir: P,
        server_name: &str,
        env: &str,
    ) -> Result<()> {
        let path = dir.as_ref().join("tako.toml");
        Self::upsert_server_env_in_file(path, server_name, env)
    }

    pub fn upsert_server_env_in_file<P: AsRef<Path>>(
        path: P,
        server_name: &str,
        env: &str,
    ) -> Result<()> {
        let path = path.as_ref();
        let mut doc = load_or_create_toml_document(path)?;
        let root = doc
            .as_table_mut()
            .ok_or_else(|| ConfigError::Validation("tako.toml must be a TOML table".to_string()))?;

        let envs = root
            .entry("envs")
            .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
            .as_table_mut()
            .ok_or_else(|| {
                ConfigError::Validation(
                    "Invalid [envs] section: expected table structure".to_string(),
                )
            })?;

        for (env_name, env_value) in envs.iter_mut() {
            if env_name == "development" || env_name == env {
                continue;
            }
            let Some(env_table) = env_value.as_table_mut() else {
                return Err(ConfigError::Validation(format!(
                    "Cannot update env '{}': [envs.{}] is not a table",
                    env_name, env_name
                )));
            };
            if let Some(existing_servers) = env_table.get_mut("servers") {
                let Some(array) = existing_servers.as_array_mut() else {
                    return Err(ConfigError::Validation(format!(
                        "Cannot update env '{}': [envs.{}].servers must be an array",
                        env_name, env_name
                    )));
                };
                array.retain(|value| value.as_str() != Some(server_name));
            }
        }

        let env_entry = envs
            .entry(env.to_string())
            .or_insert_with(|| toml::Value::Table(toml::map::Map::new()));
        let Some(env_table) = env_entry.as_table_mut() else {
            return Err(ConfigError::Validation(format!(
                "Cannot map server '{}': [envs.{}] is not a table",
                server_name, env
            )));
        };

        match env_table.get_mut("servers") {
            Some(existing_servers) => {
                let Some(array) = existing_servers.as_array_mut() else {
                    return Err(ConfigError::Validation(format!(
                        "Cannot map server '{}': [envs.{}].servers must be an array",
                        server_name, env
                    )));
                };
                if !array
                    .iter()
                    .any(|value| value.as_str() == Some(server_name))
                {
                    array.push(toml::Value::String(server_name.to_string()));
                }
            }
            None => {
                env_table.insert(
                    "servers".to_string(),
                    toml::Value::Array(vec![toml::Value::String(server_name.to_string())]),
                );
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

fn validate_top_level_keys(raw: &toml::Value) -> Result<()> {
    let Some(table) = raw.as_table() else {
        return Err(ConfigError::Validation(
            "tako.toml must be a TOML table".to_string(),
        ));
    };

    for key in table.keys() {
        if !matches!(
            key.as_str(),
            "name"
                | "runtime"
                | "runtime_version"
                | "package_manager"
                | "preset"
                | "main"
                | "assets"
                | "build"
                | "build_stages"
                | "vars"
                | "envs"
        ) {
            return Err(ConfigError::Validation(format!("Unknown key '{}'", key)));
        }
    }

    Ok(())
}

fn parse_build_config(raw: &toml::Value) -> Result<BuildConfig> {
    let Some(value) = raw.get("build") else {
        return Ok(BuildConfig::default());
    };

    let table = value
        .as_table()
        .ok_or_else(|| ConfigError::Validation("'build' must be a table ([build])".to_string()))?;
    validate_build_keys(table)?;
    let table_value = toml::Value::Table(table.clone());

    let run = parse_optional_string(&table_value, "run")?;
    let install = parse_optional_string(&table_value, "install")?;
    let cwd = parse_optional_string(&table_value, "cwd")?;
    let include = parse_string_array(&table_value, "include")?.unwrap_or_default();
    let exclude = parse_string_array(&table_value, "exclude")?.unwrap_or_default();

    Ok(BuildConfig {
        run,
        install,
        cwd,
        include,
        exclude,
    })
}

fn validate_build_keys(table: &toml::value::Table) -> Result<()> {
    for key in table.keys() {
        if !matches!(
            key.as_str(),
            "run" | "install" | "cwd" | "include" | "exclude"
        ) {
            return Err(ConfigError::Validation(format!(
                "Unknown key 'build.{key}'"
            )));
        }
    }

    Ok(())
}

fn parse_build_stages(raw: &toml::Value) -> Result<Vec<BuildStage>> {
    let Some(value) = raw.get("build_stages") else {
        return Ok(Vec::new());
    };
    let Some(stages) = value.as_array() else {
        return Err(ConfigError::Validation(
            "'build_stages' must be an array of tables ([[build_stages]])".to_string(),
        ));
    };

    let mut parsed = Vec::with_capacity(stages.len());
    for (index, stage_value) in stages.iter().enumerate() {
        let Some(stage_table) = stage_value.as_table() else {
            return Err(ConfigError::Validation(format!(
                "'build_stages[{index}]' must be a table"
            )));
        };

        for key in stage_table.keys() {
            if !matches!(key.as_str(), "name" | "cwd" | "install" | "run" | "include") {
                return Err(ConfigError::Validation(format!(
                    "Unknown key 'build_stages[{index}].{key}'"
                )));
            }
        }

        let name = parse_build_stage_optional_string(stage_table, index, "name")?;
        let cwd = parse_build_stage_optional_string(stage_table, index, "cwd")?;
        let install = parse_build_stage_optional_string(stage_table, index, "install")?;
        let run = parse_build_stage_required_string(stage_table, index, "run")?;
        let include =
            parse_build_stage_string_array(stage_table, index, "include")?.unwrap_or_default();

        parsed.push(BuildStage {
            name,
            cwd,
            install,
            run,
            include,
        });
    }

    Ok(parsed)
}

fn parse_build_stage_optional_string(
    stage_table: &toml::value::Table,
    index: usize,
    key: &str,
) -> Result<Option<String>> {
    let Some(value) = stage_table.get(key) else {
        return Ok(None);
    };
    let Some(value) = value.as_str() else {
        return Err(ConfigError::Validation(format!(
            "'build_stages[{index}].{key}' must be a string"
        )));
    };
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(ConfigError::Validation(format!(
            "'build_stages[{index}].{key}' cannot be empty"
        )));
    }
    Ok(Some(trimmed.to_string()))
}

fn parse_build_stage_required_string(
    stage_table: &toml::value::Table,
    index: usize,
    key: &str,
) -> Result<String> {
    let Some(value) = stage_table.get(key) else {
        return Err(ConfigError::Validation(format!(
            "'build_stages[{index}].{key}' is required"
        )));
    };
    let Some(value) = value.as_str() else {
        return Err(ConfigError::Validation(format!(
            "'build_stages[{index}].{key}' must be a string"
        )));
    };
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(ConfigError::Validation(format!(
            "'build_stages[{index}].{key}' cannot be empty"
        )));
    }
    Ok(trimmed.to_string())
}

fn parse_build_stage_string_array(
    stage_table: &toml::value::Table,
    index: usize,
    key: &str,
) -> Result<Option<Vec<String>>> {
    let Some(value) = stage_table.get(key) else {
        return Ok(None);
    };
    let Some(arr) = value.as_array() else {
        return Err(ConfigError::Validation(format!(
            "'build_stages[{index}].{key}' must be an array of strings"
        )));
    };
    let mut out = Vec::with_capacity(arr.len());
    for item in arr {
        let Some(s) = item.as_str() else {
            return Err(ConfigError::Validation(format!(
                "'build_stages[{index}].{key}' must be an array of strings"
            )));
        };
        out.push(s.to_string());
    }
    Ok(Some(out))
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

fn validate_relative_dir(value: &str, field: &str) -> Result<()> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(ConfigError::Validation(format!(
            "'{field}' cannot be empty"
        )));
    }
    let path = Path::new(trimmed);
    if path.is_absolute() {
        return Err(ConfigError::Validation(format!(
            "'{field}' must be a relative path"
        )));
    }
    if path
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(ConfigError::Validation(format!(
            "'{field}' must not contain '..'"
        )));
    }
    Ok(())
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
            "assets entry cannot be empty".to_string(),
        ));
    }

    let path = Path::new(trimmed);
    if path.is_absolute() {
        return Err(ConfigError::Validation(format!(
            "assets entry '{}' must be relative to project root",
            asset_path
        )));
    }

    if path
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(ConfigError::Validation(format!(
            "assets entry '{}' must not contain '..'",
            asset_path
        )));
    }

    Ok(())
}

fn validate_build_stage(stage: &BuildStage, index: usize) -> Result<()> {
    if let Some(cwd) = &stage.cwd {
        validate_build_stage_cwd(cwd, index)?;
    }
    if stage.run.trim().is_empty() {
        return Err(ConfigError::Validation(format!(
            "'build_stages[{index}].run' cannot be empty"
        )));
    }
    Ok(())
}

fn validate_build_stage_cwd(cwd: &str, index: usize) -> Result<()> {
    let trimmed = cwd.trim();
    if trimmed.is_empty() {
        return Err(ConfigError::Validation(format!(
            "'build_stages[{index}].cwd' cannot be empty"
        )));
    }
    let path = Path::new(trimmed);
    if path.is_absolute() {
        return Err(ConfigError::Validation(format!(
            "'build_stages[{index}].cwd' must be relative"
        )));
    }
    // Allow ".." for monorepo traversal, but guard against escaping the workspace root.
    // After normalizing, the resolved path must not start with ".." (i.e., escape the root).
    let mut depth: i32 = 0;
    for component in path.components() {
        match component {
            Component::ParentDir => depth -= 1,
            Component::Normal(_) => depth += 1,
            _ => {}
        }
        if depth < 0 {
            return Err(ConfigError::Validation(format!(
                "'build_stages[{index}].cwd' must not escape the project root"
            )));
        }
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
        let config = Config::parse("").unwrap();
        assert_eq!(config, Config::default());
    }

    #[test]
    fn test_parse_top_level_metadata_fields() {
        let toml = r#"
name = "my-app"
main = "server/index.mjs"
preset = "bun"
"#;
        let config = Config::parse(toml).unwrap();
        assert_eq!(config.name, Some("my-app".to_string()));
        assert_eq!(config.main, Some("server/index.mjs".to_string()));
        assert_eq!(config.preset, Some("bun".to_string()));
    }

    #[test]
    fn test_parse_build_arrays() {
        let toml = r#"
assets = ["public-assets", "shared/images"]

[build]
include = [".output/**", "dist/**"]
exclude = ["**/*.map"]
"#;
        let config = Config::parse(toml).unwrap();
        assert_eq!(
            config.build.include,
            vec![".output/**".to_string(), "dist/**".to_string()]
        );
        assert_eq!(config.build.exclude, vec!["**/*.map".to_string()]);
        assert_eq!(
            config.assets,
            vec!["public-assets".to_string(), "shared/images".to_string()]
        );
        assert!(config.build_stages.is_empty());
    }

    #[test]
    fn test_parse_build_stages() {
        let toml = r#"
[[build_stages]]
run = "bun run build"

[[build_stages]]
name = "frontend-assets"
cwd = "frontend"
install = "bun install"
run = "bun run build"
"#;
        let config = Config::parse(toml).unwrap();
        assert_eq!(config.build_stages.len(), 2);
        assert_eq!(config.build_stages[0].name, None);
        assert_eq!(config.build_stages[0].cwd, None);
        assert_eq!(config.build_stages[0].install, None);
        assert_eq!(config.build_stages[0].run, "bun run build");
        assert!(config.build_stages[0].include.is_empty());
        assert_eq!(
            config.build_stages[1],
            BuildStage {
                name: Some("frontend-assets".to_string()),
                cwd: Some("frontend".to_string()),
                install: Some("bun install".to_string()),
                run: "bun run build".to_string(),
                include: Vec::new(),
            }
        );
    }

    #[test]
    fn test_parse_build_stages_with_include() {
        let toml = r#"
[[build_stages]]
name = "rust-service"
cwd = "rust-service"
run = "cargo build --release"
include = ["target/release/my-service"]

[[build_stages]]
name = "frontend"
cwd = "apps/web"
install = "bun install"
run = "bun run build"
include = ["dist/**", "package.json"]
"#;
        let config = Config::parse(toml).unwrap();
        assert_eq!(config.build_stages.len(), 2);
        assert_eq!(
            config.build_stages[0].include,
            vec!["target/release/my-service".to_string()]
        );
        assert_eq!(
            config.build_stages[1].include,
            vec!["dist/**".to_string(), "package.json".to_string()]
        );
    }

    #[test]
    fn test_build_stages_include_rejects_absolute_paths() {
        let toml = r#"
[[build_stages]]
run = "cargo build"
include = ["/tmp/out/**"]
"#;
        let err = Config::parse(toml).unwrap_err();
        assert!(
            err.to_string()
                .contains("build_stages[0].include entry '/tmp/out/**' must be relative")
        );
    }

    #[test]
    fn test_build_stages_include_rejects_parent_traversal() {
        let toml = r#"
[[build_stages]]
run = "cargo build"
include = ["../secret/**"]
"#;
        let err = Config::parse(toml).unwrap_err();
        assert!(
            err.to_string()
                .contains("build_stages[0].include entry '../secret/**' must not contain '..'")
        );
    }

    #[test]
    fn test_build_include_mutually_exclusive_with_stages() {
        let toml = r#"
[build]
include = ["dist/**"]

[[build_stages]]
run = "bun run build"
"#;
        let err = Config::parse(toml).unwrap_err();
        assert!(err.to_string().contains("per-stage include"));
    }

    #[test]
    fn test_build_exclude_mutually_exclusive_with_stages() {
        let toml = r#"
[build]
exclude = ["**/*.map"]

[[build_stages]]
run = "bun run build"
"#;
        let err = Config::parse(toml).unwrap_err();
        assert!(err.to_string().contains("per-stage include"));
    }

    #[test]
    fn test_parse_build_stages_requires_run() {
        let toml = r#"
[[build_stages]]
name = "frontend-assets"
"#;
        let err = Config::parse(toml).unwrap_err();
        assert!(
            err.to_string()
                .contains("'build_stages[0].run' is required")
        );
    }

    #[test]
    fn test_parse_build_stages_rejects_empty_run() {
        let toml = r#"
[[build_stages]]
run = "   "
"#;
        let err = Config::parse(toml).unwrap_err();
        assert!(
            err.to_string()
                .contains("'build_stages[0].run' cannot be empty")
        );
    }

    #[test]
    fn test_parse_build_stages_rejects_non_table_entries() {
        let toml = r#"
build_stages = ["bun run build"]
"#;
        let err = Config::parse(toml).unwrap_err();
        assert!(
            err.to_string()
                .contains("'build_stages[0]' must be a table")
        );
    }

    #[test]
    fn test_parse_build_stages_rejects_unknown_keys() {
        let toml = r#"
[[build_stages]]
command = "bun run build"
run = "bun run build"
"#;
        let err = Config::parse(toml).unwrap_err();
        assert!(
            err.to_string()
                .contains("Unknown key 'build_stages[0].command'")
        );
    }

    #[test]
    fn test_build_stages_mutually_exclusive_with_build_run() {
        let toml = r#"
[build]
run = "bun run build"

[[build_stages]]
run = "bun run other"
"#;
        let err = Config::parse(toml).unwrap_err();
        assert!(err.to_string().contains("mutually exclusive"));
    }

    #[test]
    fn test_parse_runtime() {
        let toml = r#"
runtime = "deno"
"#;
        let config = Config::parse(toml).unwrap();
        assert_eq!(config.runtime, Some("deno".to_string()));
    }

    #[test]
    fn test_parse_runtime_version() {
        let toml = r#"
runtime = "bun"
runtime_version = "1.2.3"
"#;
        let config = Config::parse(toml).unwrap();
        assert_eq!(config.runtime_version, Some("1.2.3".to_string()));
    }

    #[test]
    fn test_parse_runtime_version_defaults_to_none() {
        let toml = r#"
runtime = "bun"
"#;
        let config = Config::parse(toml).unwrap();
        assert!(config.runtime_version.is_none());
    }

    #[test]
    fn test_parse_rejects_unknown_top_level_keys() {
        let top_level_adapter = r#"
adapter = "node"
"#;
        let err = Config::parse(top_level_adapter).unwrap_err();
        assert!(err.to_string().contains("Unknown key 'adapter'"));

        let top_level_dist = r#"
dist = ".tako/dist"
"#;
        let err = Config::parse(top_level_dist).unwrap_err();
        assert!(err.to_string().contains("Unknown key 'dist'"));

        let top_level_servers = r#"
[servers]
production = ["prod-1"]
"#;
        let err = Config::parse(top_level_servers).unwrap_err();
        assert!(err.to_string().contains("Unknown key 'servers'"));
    }

    #[test]
    fn test_parse_accepts_top_level_assets() {
        let toml = r#"
assets = ["dist/client"]
"#;
        let config = Config::parse(toml).unwrap();
        assert_eq!(config.assets, vec!["dist/client".to_string()]);
    }

    #[test]
    fn test_parse_accepts_top_level_preset() {
        let toml = r#"
preset = "tanstack-start"
"#;
        let config = Config::parse(toml).unwrap();
        assert_eq!(config.preset, Some("tanstack-start".to_string()));
    }

    #[test]
    fn test_parse_rejects_unknown_build_keys() {
        let build_adapter = r#"
[build]
adapter = "bun"
"#;
        let err = Config::parse(build_adapter).unwrap_err();
        assert!(err.to_string().contains("Unknown key 'build.adapter'"));

        // preset is now top-level, not under [build]
        let build_preset = r#"
[build]
preset = "bun"
"#;
        let err = Config::parse(build_preset).unwrap_err();
        assert!(err.to_string().contains("Unknown key 'build.preset'"));
    }

    #[test]
    fn test_parse_global_vars() {
        let toml = r#"
[vars]
TAKO_APP_LOG_LEVEL = "info"
API_URL = "https://api.example.com"
"#;
        let config = Config::parse(toml).unwrap();
        assert_eq!(
            config.vars.get("TAKO_APP_LOG_LEVEL"),
            Some(&"info".to_string())
        );
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
        let config = Config::parse(toml).unwrap();
        let env = config.envs.get("production").unwrap();
        assert_eq!(env.route, Some("api.example.com".to_string()));
        assert_eq!(env.routes, None);
    }

    #[test]
    fn test_parse_env_without_routes_is_rejected() {
        let toml = r#"
[envs.production]
"#;
        let err = Config::parse(toml).unwrap_err();
        assert!(
            err.to_string()
                .contains("must define either 'route' or 'routes'")
        );
    }

    #[test]
    fn test_parse_development_env_without_routes_is_allowed() {
        let toml = r#"
[envs.development]
"#;
        let config = Config::parse(toml).unwrap();
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
        let err = Config::parse(toml).unwrap_err();
        assert!(err.to_string().contains("routes"));
    }

    #[test]
    fn test_parse_development_env_with_empty_routes_is_allowed() {
        let toml = r#"
[envs.development]
routes = []
"#;
        let config = Config::parse(toml).unwrap();
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
        let config = Config::parse(toml).unwrap();
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
    fn test_parse_env_rejects_additional_keys() {
        let toml = r#"
[envs.production]
route = "api.example.com"
replicas = 3
"#;
        let err = Config::parse(toml).unwrap_err();
        assert!(err.to_string().contains("unknown field"));
    }

    #[test]
    fn test_parse_env_servers_and_idle_timeout() {
        let toml = r#"
[envs.production]
route = "api.example.com"
servers = ["la-prod", "nyc-prod"]
idle_timeout = 600
"#;
        let config = Config::parse(toml).unwrap();
        let env = config.envs.get("production").unwrap();
        assert_eq!(
            env.servers,
            vec!["la-prod".to_string(), "nyc-prod".to_string()]
        );
        assert_eq!(env.idle_timeout, 600);
    }

    #[test]
    fn test_default_env_idle_timeout_is_five_minutes() {
        let config = Config::default();
        assert_eq!(config.get_idle_timeout("production"), 300);
    }

    #[test]
    fn test_parse_complete_config() {
        let toml = r#"
name = "my-api"
main = "server/index.mjs"
preset = "bun"
assets = ["public", ".output/public"]

[build]
run = "bun run build"
include = ["dist/**"]
exclude = ["**/*.map"]

[vars]
TAKO_APP_LOG_LEVEL = "info"

[envs.production]
route = "api.example.com"
servers = ["prod-1"]

[envs.staging]
routes = ["staging.example.com", "*.staging.example.com"]
"#;
        let config = Config::parse(toml).unwrap();

        assert_eq!(config.name, Some("my-api".to_string()));
        assert_eq!(config.main, Some("server/index.mjs".to_string()));
        assert_eq!(config.preset, Some("bun".to_string()));
        assert_eq!(config.build.run, Some("bun run build".to_string()));
        assert_eq!(config.build.include, vec!["dist/**".to_string()]);
        assert_eq!(config.build.exclude, vec!["**/*.map".to_string()]);
        assert_eq!(
            config.assets,
            vec!["public".to_string(), ".output/public".to_string()]
        );
        assert_eq!(
            config.vars.get("TAKO_APP_LOG_LEVEL"),
            Some(&"info".to_string())
        );

        let prod = config.envs.get("production").unwrap();
        assert_eq!(prod.route, Some("api.example.com".to_string()));

        let staging = config.envs.get("staging").unwrap();
        assert_eq!(staging.routes.as_ref().unwrap().len(), 2);
        let prod = config.envs.get("production").unwrap();
        assert_eq!(prod.servers, vec!["prod-1".to_string()]);
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
        assert!(Config::parse(toml).is_err());
    }

    #[test]
    fn test_validate_idle_timeout_cannot_be_zero() {
        let toml = r#"
[envs.production]
route = "api.example.com"
idle_timeout = 0
"#;
        assert!(Config::parse(toml).is_err());
    }

    #[test]
    fn test_validate_assets_rejects_absolute_path() {
        let toml = r#"
assets = ["/tmp/assets"]
"#;
        let err = Config::parse(toml).unwrap_err();
        assert!(
            err.to_string()
                .contains("assets entry '/tmp/assets' must be relative to project root")
        );
    }

    #[test]
    fn test_validate_assets_rejects_parent_directory_reference() {
        let toml = r#"
assets = ["../shared-assets"]
"#;
        let err = Config::parse(toml).unwrap_err();
        assert!(
            err.to_string()
                .contains("assets entry '../shared-assets' must not contain '..'")
        );
    }

    #[test]
    fn test_validate_build_globs_reject_invalid_paths() {
        let absolute = r#"
[build]
include = ["/tmp/out/**"]
"#;
        let err = Config::parse(absolute).unwrap_err();
        assert!(
            err.to_string()
                .contains("build.include entry '/tmp/out/**' must be relative to project root")
        );

        let parent = r#"
[build]
exclude = ["../secret/**"]
"#;
        let err = Config::parse(parent).unwrap_err();
        assert!(
            err.to_string()
                .contains("build.exclude entry '../secret/**' must not contain '..'")
        );
    }

    #[test]
    fn test_validate_build_stage_cwd_rejects_absolute_paths() {
        let absolute = r#"
[[build_stages]]
cwd = "/tmp"
run = "bun run build"
"#;
        let err = Config::parse(absolute).unwrap_err();
        assert!(
            err.to_string()
                .contains("'build_stages[0].cwd' must be relative")
        );
    }

    #[test]
    fn test_validate_build_stage_cwd_allows_parent_within_root() {
        // cwd = "packages/../packages/ui" stays within root
        let toml = r#"
[[build_stages]]
cwd = "packages/../packages/ui"
run = "bun run build"
"#;
        let config = Config::parse(toml).unwrap();
        assert_eq!(
            config.build_stages[0].cwd,
            Some("packages/../packages/ui".to_string())
        );
    }

    #[test]
    fn test_validate_build_stage_cwd_rejects_escaping_root() {
        let toml = r#"
[[build_stages]]
cwd = "../outside"
run = "bun run build"
"#;
        let err = Config::parse(toml).unwrap_err();
        assert!(err.to_string().contains("must not escape the project root"));
    }

    #[test]
    fn test_validate_runtime_rejects_empty_and_unknown_values() {
        let empty = r#"
runtime = ""
"#;
        let err = Config::parse(empty).unwrap_err();
        assert!(err.to_string().contains("runtime cannot be empty"));

        let unknown = r#"
runtime = "python"
"#;
        let err = Config::parse(unknown).unwrap_err();
        assert!(
            err.to_string()
                .contains("runtime must be one of: bun, node, deno, go")
        );
    }

    #[test]
    fn test_validate_preset_rejects_namespaced_alias_in_tako_toml() {
        let raw = r#"
preset = "js/tanstack-start"
"#;
        let err = Config::parse(raw).unwrap_err();
        assert!(
            err.to_string()
                .contains("preset must not include runtime namespace")
        );
    }

    #[test]
    fn test_validate_preset_rejects_github_reference() {
        let raw = r#"
preset = "github:owner/repo/presets/custom.toml"
"#;
        let err = Config::parse(raw).unwrap_err();
        assert!(
            err.to_string()
                .contains("github preset references are not supported")
        );
    }

    #[test]
    fn test_validate_preset_rejects_colon_references() {
        let raw = r#"
preset = "custom:tanstack-start"
"#;
        let err = Config::parse(raw).unwrap_err();
        assert!(err.to_string().contains("':' references are not supported"));
    }

    #[test]
    fn test_parse_rejects_non_table_build_property() {
        let toml = r#"
build = "bun run build"
"#;
        let err = Config::parse(toml).unwrap_err();
        assert!(err.to_string().contains("'build' must be a table"));
    }

    #[test]
    fn test_validate_main_rejects_empty_value() {
        let toml = r#"
main = "   "
"#;
        let err = Config::parse(toml).unwrap_err();
        assert!(err.to_string().contains("main cannot be empty"));
    }

    // ==================== Helper Method Tests ====================

    #[test]
    fn test_get_routes_single() {
        let toml = r#"
[envs.production]
route = "api.example.com"
"#;
        let config = Config::parse(toml).unwrap();
        let routes = config.get_routes("production").unwrap();
        assert_eq!(routes, vec!["api.example.com"]);
    }

    #[test]
    fn test_get_routes_multiple() {
        let toml = r#"
[envs.production]
routes = ["api.example.com", "www.example.com"]
"#;
        let config = Config::parse(toml).unwrap();
        let routes = config.get_routes("production").unwrap();
        assert_eq!(routes, vec!["api.example.com", "www.example.com"]);
    }

    #[test]
    fn test_get_routes_nonexistent_env() {
        let config = Config::default();
        assert!(config.get_routes("production").is_none());
    }

    #[test]
    fn test_load_from_dir_requires_tako_toml() {
        let temp = tempfile::TempDir::new().unwrap();
        let err = Config::load_from_dir(temp.path()).unwrap_err();
        assert!(err.to_string().contains("tako.toml"));
    }

    #[test]
    fn test_load_from_dir_allows_missing_name() {
        let temp = tempfile::TempDir::new().unwrap();
        fs::write(
            temp.path().join("tako.toml"),
            r#"
[envs.production]
route = "prod.example.com"
"#,
        )
        .unwrap();

        let config = Config::load_from_dir(temp.path()).unwrap();
        assert!(config.name.is_none());
        assert_eq!(
            config
                .get_routes("production")
                .expect("production routes should exist"),
            vec!["prod.example.com".to_string()]
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
        let config = Config::parse(toml).unwrap();
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
        assert!(Config::parse(toml).is_err());
    }

    #[test]
    fn test_wrong_type() {
        let toml = r#"
name = 123
"#;
        assert!(Config::parse(toml).is_err());
    }

    // ==================== Per-Environment Vars Tests ====================

    #[test]
    fn test_parse_per_env_vars() {
        let toml = r#"
[vars]
TAKO_APP_LOG_LEVEL = "info"

[vars.production]
TAKO_APP_LOG_LEVEL = "warn"
DATABASE_URL = "postgres://prod"

[vars.staging]
DATABASE_URL = "postgres://staging"
"#;
        let config = Config::parse(toml).unwrap();

        // Global var
        assert_eq!(
            config.vars.get("TAKO_APP_LOG_LEVEL"),
            Some(&"info".to_string())
        );

        // Per-env vars
        let prod_vars = config.vars_per_env.get("production").unwrap();
        assert_eq!(
            prod_vars.get("TAKO_APP_LOG_LEVEL"),
            Some(&"warn".to_string())
        );
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
TAKO_APP_LOG_LEVEL = "info"
API_URL = "https://api.example.com"

[vars.production]
TAKO_APP_LOG_LEVEL = "warn"
DATABASE_URL = "postgres://prod"
"#;
        let config = Config::parse(toml).unwrap();

        let merged = config.get_merged_vars("production");
        assert_eq!(merged.get("TAKO_APP_LOG_LEVEL"), Some(&"warn".to_string())); // overridden
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
TAKO_APP_LOG_LEVEL = "info"
"#;
        let config = Config::parse(toml).unwrap();

        let merged = config.get_merged_vars("nonexistent");
        assert_eq!(merged.get("TAKO_APP_LOG_LEVEL"), Some(&"info".to_string()));
        assert_eq!(merged.len(), 1);
    }

    // ==================== Environment Server Mapping Tests ====================

    #[test]
    fn test_get_servers_for_env() {
        let toml = r#"
[envs.production]
route = "api.example.com"
servers = ["la-prod", "nyc-prod"]

[envs.staging]
route = "staging.example.com"
servers = ["staging-server"]
"#;
        let config = Config::parse(toml).unwrap();

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
    fn test_get_idle_timeout() {
        let toml = r#"
[envs.production]
route = "api.example.com"
idle_timeout = 300

[envs.staging]
route = "staging.example.com"
idle_timeout = 600
"#;
        let config = Config::parse(toml).unwrap();

        assert_eq!(config.get_idle_timeout("production"), 300);
        assert_eq!(config.get_idle_timeout("staging"), 600);
        assert_eq!(config.get_idle_timeout("unknown"), 300);
    }

    #[test]
    fn test_duplicate_non_development_server_membership_is_allowed() {
        let toml = r#"
[envs.production]
route = "api.example.com"
servers = ["shared"]

[envs.staging]
route = "staging.example.com"
servers = ["shared"]
"#;
        let config = Config::parse(toml).unwrap();
        assert_eq!(config.get_servers_for_env("production"), vec!["shared"]);
        assert_eq!(config.get_servers_for_env("staging"), vec!["shared"]);
    }

    #[test]
    fn test_duplicate_server_membership_with_development_is_allowed() {
        let toml = r#"
[envs.production]
route = "api.example.com"
servers = ["shared"]

[envs.development]
servers = ["shared"]
"#;
        assert!(Config::parse(toml).is_ok());
    }

    #[test]
    fn test_env_servers_reject_invalid_server_name() {
        let toml = r#"
[envs.production]
route = "api.example.com"
servers = ["INVALID_NAME"]
"#;
        assert!(Config::parse(toml).is_err());
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

    #[test]
    fn test_log_level_parses_valid_values() {
        for level in ["debug", "info", "warn", "error"] {
            let toml = format!(
                r#"
[envs.production]
route = "example.com"
log_level = "{level}"
"#
            );
            let config = Config::parse(&toml).unwrap();
            assert_eq!(config.envs["production"].log_level.as_deref(), Some(level));
        }
    }

    #[test]
    fn test_log_level_rejects_invalid_value() {
        let toml = r#"
[envs.production]
route = "example.com"
log_level = "verbose"
"#;
        let err = Config::parse(toml).unwrap_err();
        assert!(err.to_string().contains("Invalid log_level"));
        assert!(err.to_string().contains("verbose"));
    }

    #[test]
    fn test_log_level_defaults_omitted() {
        let toml = r#"
[envs.production]
route = "example.com"
"#;
        let config = Config::parse(toml).unwrap();
        assert_eq!(config.envs["production"].log_level, None);
    }

    #[test]
    fn test_resolve_app_log_level_explicit() {
        let config = EnvConfig {
            log_level: Some("warn".to_string()),
            ..Default::default()
        };
        assert_eq!(resolve_app_log_level(Some(&config), "production"), "warn");
        assert_eq!(resolve_app_log_level(Some(&config), "development"), "warn");
    }

    #[test]
    fn test_resolve_app_log_level_default_for_development() {
        assert_eq!(resolve_app_log_level(None, "development"), "debug");
        let config = EnvConfig::default();
        assert_eq!(resolve_app_log_level(Some(&config), "development"), "debug");
    }

    #[test]
    fn test_resolve_app_log_level_default_for_other_envs() {
        assert_eq!(resolve_app_log_level(None, "production"), "info");
        assert_eq!(resolve_app_log_level(None, "staging"), "info");
        let config = EnvConfig::default();
        assert_eq!(resolve_app_log_level(Some(&config), "production"), "info");
    }

    // ==================== build.cwd Tests ====================

    #[test]
    fn test_parse_build_cwd() {
        let toml = r#"
[build]
cwd = "."
"#;
        let config = Config::parse(toml).unwrap();
        assert_eq!(config.build.cwd, Some(".".to_string()));
    }

    #[test]
    fn test_build_cwd_accepts_subdirectory() {
        let toml = r#"
[build]
cwd = "packages/web"
"#;
        let config = Config::parse(toml).unwrap();
        assert_eq!(config.build.cwd, Some("packages/web".to_string()));
    }

    #[test]
    fn test_build_cwd_rejects_empty() {
        let toml = r#"
[build]
cwd = ""
"#;
        let err = Config::parse(toml).unwrap_err();
        assert!(err.to_string().contains("'build.cwd' cannot be empty"));
    }

    #[test]
    fn test_build_cwd_rejects_absolute_path() {
        let toml = r#"
[build]
cwd = "/tmp/build"
"#;
        let err = Config::parse(toml).unwrap_err();
        assert!(
            err.to_string()
                .contains("'build.cwd' must be a relative path")
        );
    }

    #[test]
    fn test_build_cwd_rejects_parent_dir() {
        let toml = r#"
[build]
cwd = "../parent"
"#;
        let err = Config::parse(toml).unwrap_err();
        assert!(
            err.to_string()
                .contains("'build.cwd' must not contain '..'")
        );
    }

    #[test]
    fn test_parse_build_with_run_and_install() {
        let toml = r#"
[build]
run = "vinxi build"
install = "bun install"
cwd = "."
include = ["dist/**"]
"#;
        let config = Config::parse(toml).unwrap();
        assert_eq!(config.build.run, Some("vinxi build".to_string()));
        assert_eq!(config.build.install, Some("bun install".to_string()));
        assert_eq!(config.build.cwd, Some(".".to_string()));
        assert_eq!(config.build.include, vec!["dist/**".to_string()]);
    }
}

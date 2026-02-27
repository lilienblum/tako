use std::fs;
use std::path::{Path, PathBuf};

use super::error::{ConfigError, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpgradeChannel {
    Stable,
    Canary,
}

impl UpgradeChannel {
    pub fn as_str(self) -> &'static str {
        match self {
            UpgradeChannel::Stable => "stable",
            UpgradeChannel::Canary => "canary",
        }
    }

    fn parse(value: &str) -> Result<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "stable" => Ok(UpgradeChannel::Stable),
            "canary" => Ok(UpgradeChannel::Canary),
            other => Err(ConfigError::Validation(format!(
                "Invalid upgrade channel '{}'. Expected 'stable' or 'canary'.",
                other
            ))),
        }
    }
}

pub fn load_upgrade_channel() -> Result<UpgradeChannel> {
    let config_path = default_path()?;
    load_upgrade_channel_from_file(&config_path)
}

fn build_default_channel() -> UpgradeChannel {
    if option_env!("TAKO_CANARY_SHA").is_some() {
        UpgradeChannel::Canary
    } else {
        UpgradeChannel::Stable
    }
}

pub fn save_upgrade_channel(channel: UpgradeChannel) -> Result<()> {
    let config_path = default_path()?;
    save_upgrade_channel_to_file(&config_path, channel)
}

pub fn resolve_upgrade_channel(canary: bool, stable: bool) -> Result<UpgradeChannel> {
    let config_path = default_path()?;
    resolve_upgrade_channel_with_path(&config_path, canary, stable)
}

fn default_path() -> Result<PathBuf> {
    let home = crate::paths::tako_home_dir().map_err(|e| {
        ConfigError::Validation(format!("Could not determine tako home directory: {}", e))
    })?;
    Ok(home.join("config.toml"))
}

fn load_upgrade_channel_from_file(path: &Path) -> Result<UpgradeChannel> {
    load_upgrade_channel_from_file_with_default(path, build_default_channel())
}

fn load_upgrade_channel_from_file_with_default(
    path: &Path,
    default: UpgradeChannel,
) -> Result<UpgradeChannel> {
    if !path.exists() {
        return Ok(default);
    }

    let content =
        fs::read_to_string(path).map_err(|e| ConfigError::FileRead(path.to_path_buf(), e))?;
    if content.trim().is_empty() {
        return Ok(default);
    }

    let doc: toml::Value = toml::from_str(&content)?;
    let Some(root) = doc.as_table() else {
        return Err(ConfigError::Validation(
            "Global config must be a TOML table".to_string(),
        ));
    };

    let Some(channel_value) = root.get("upgrade_channel") else {
        return Ok(default);
    };
    let Some(channel) = channel_value.as_str() else {
        return Err(ConfigError::Validation(
            "Global config key upgrade_channel must be a string".to_string(),
        ));
    };

    UpgradeChannel::parse(channel)
}

fn save_upgrade_channel_to_file(path: &Path, channel: UpgradeChannel) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| ConfigError::FileWrite(parent.to_path_buf(), e))?;
    }

    let mut doc = if path.exists() {
        let existing =
            fs::read_to_string(path).map_err(|e| ConfigError::FileRead(path.to_path_buf(), e))?;
        if existing.trim().is_empty() {
            toml::Value::Table(toml::map::Map::new())
        } else {
            toml::from_str::<toml::Value>(&existing)?
        }
    } else {
        toml::Value::Table(toml::map::Map::new())
    };

    let Some(root) = doc.as_table_mut() else {
        return Err(ConfigError::Validation(
            "Global config must be a TOML table".to_string(),
        ));
    };

    root.insert(
        "upgrade_channel".to_string(),
        toml::Value::String(channel.as_str().to_string()),
    );

    let content = toml::to_string_pretty(&doc)?;
    fs::write(path, content).map_err(|e| ConfigError::FileWrite(path.to_path_buf(), e))?;
    Ok(())
}

fn resolve_upgrade_channel_with_path(
    path: &Path,
    canary: bool,
    stable: bool,
) -> Result<UpgradeChannel> {
    if canary {
        save_upgrade_channel_to_file(path, UpgradeChannel::Canary)?;
        return Ok(UpgradeChannel::Canary);
    }
    if stable {
        save_upgrade_channel_to_file(path, UpgradeChannel::Stable)?;
        return Ok(UpgradeChannel::Stable);
    }
    load_upgrade_channel_from_file(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn load_upgrade_channel_defaults_to_stable_when_config_is_missing() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("config.toml");
        let channel =
            load_upgrade_channel_from_file_with_default(&path, UpgradeChannel::Stable).unwrap();
        assert_eq!(channel, UpgradeChannel::Stable);
    }

    #[test]
    fn load_upgrade_channel_defaults_to_canary_when_canary_is_build_default() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("config.toml");
        let channel =
            load_upgrade_channel_from_file_with_default(&path, UpgradeChannel::Canary).unwrap();
        assert_eq!(channel, UpgradeChannel::Canary);
    }

    #[test]
    fn load_upgrade_channel_build_default_overridden_by_saved_channel() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("config.toml");
        fs::write(&path, "upgrade_channel = \"stable\"\n").unwrap();
        let channel =
            load_upgrade_channel_from_file_with_default(&path, UpgradeChannel::Canary).unwrap();
        assert_eq!(channel, UpgradeChannel::Stable);
    }

    #[test]
    fn load_upgrade_channel_rejects_invalid_channel_value() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("config.toml");
        fs::write(
            &path,
            r#"
upgrade_channel = "beta"
"#,
        )
        .unwrap();

        let err =
            load_upgrade_channel_from_file_with_default(&path, UpgradeChannel::Stable).unwrap_err();
        assert!(err.to_string().contains("Invalid upgrade channel"));
    }

    #[test]
    fn save_upgrade_channel_preserves_unrelated_sections() {
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

        save_upgrade_channel_to_file(&path, UpgradeChannel::Canary).unwrap();
        let written = fs::read_to_string(&path).unwrap();
        assert!(written.contains("[dev]"));
        assert!(written.contains("port = 61234"));
        assert!(written.contains("upgrade_channel = \"canary\""));
    }

    #[test]
    fn resolve_upgrade_channel_with_path_persists_explicit_channel() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("config.toml");

        let channel = resolve_upgrade_channel_with_path(&path, true, false).unwrap();
        assert_eq!(channel, UpgradeChannel::Canary);

        let channel = resolve_upgrade_channel_with_path(&path, false, false).unwrap();
        assert_eq!(channel, UpgradeChannel::Canary);

        let channel = resolve_upgrade_channel_with_path(&path, false, true).unwrap();
        assert_eq!(channel, UpgradeChannel::Stable);

        let channel = resolve_upgrade_channel_with_path(&path, false, false).unwrap();
        assert_eq!(channel, UpgradeChannel::Stable);
    }
}

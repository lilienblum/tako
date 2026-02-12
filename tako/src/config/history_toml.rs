use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

use super::error::{ConfigError, Result};

const HISTORY_MAX_ENTRIES: usize = 100;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct CliHistoryToml {
    #[serde(default)]
    pub servers: ServerPromptHistory,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServerPromptHistory {
    #[serde(default)]
    pub hosts: Vec<String>,
    #[serde(default)]
    pub names: Vec<String>,
    #[serde(default)]
    pub ports: Vec<String>,
    #[serde(
        default,
        serialize_with = "serialize_server_prompt_history_entries",
        deserialize_with = "deserialize_server_prompt_history_entries"
    )]
    pub entries: Vec<ServerPromptHistoryEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServerPromptHistoryEntry {
    pub host: String,
    pub name: String,
    pub port: String,
}

fn serialize_server_prompt_history_entries<S>(
    entries: &[ServerPromptHistoryEntry],
    serializer: S,
) -> std::result::Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    let compact_entries: Vec<[&str; 3]> = entries
        .iter()
        .map(|entry| {
            [
                entry.host.as_str(),
                entry.name.as_str(),
                entry.port.as_str(),
            ]
        })
        .collect();
    compact_entries.serialize(serializer)
}

fn deserialize_server_prompt_history_entries<'de, D>(
    deserializer: D,
) -> std::result::Result<Vec<ServerPromptHistoryEntry>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let entries = Vec::<[String; 3]>::deserialize(deserializer)?;
    Ok(entries
        .into_iter()
        .map(|[host, name, port]| ServerPromptHistoryEntry { host, name, port })
        .collect())
}

impl CliHistoryToml {
    pub fn default_path() -> Result<PathBuf> {
        let home = crate::paths::tako_home_dir().map_err(|e| {
            ConfigError::Validation(format!("Could not determine tako home directory: {}", e))
        })?;
        Ok(home.join("history.toml"))
    }

    fn legacy_path() -> Result<PathBuf> {
        let home = crate::paths::tako_home_dir().map_err(|e| {
            ConfigError::Validation(format!("Could not determine tako home directory: {}", e))
        })?;
        Ok(home.join("history").join("cli.toml"))
    }

    pub fn load() -> Result<Self> {
        let path = Self::default_path()?;
        let legacy_path = Self::legacy_path()?;
        Self::load_from_paths(&path, &legacy_path)
    }

    fn load_from_paths(path: &Path, legacy_path: &Path) -> Result<Self> {
        if path.exists() {
            return Self::load_from_file(path);
        }
        if legacy_path.exists() {
            return Self::load_from_file(legacy_path);
        }
        Ok(Self::default())
    }

    pub fn load_from_file<P: AsRef<Path>>(path: P) -> Result<Self> {
        let content = fs::read_to_string(path.as_ref())
            .map_err(|e| ConfigError::FileRead(path.as_ref().to_path_buf(), e))?;
        Self::parse(&content)
    }

    pub fn parse(content: &str) -> Result<Self> {
        if content.trim().is_empty() {
            return Ok(Self::default());
        }
        let parsed: Self = toml::from_str(content)?;
        Ok(parsed)
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::default_path()?;
        self.save_to_file(path)
    }

    pub fn save_to_file<P: AsRef<Path>>(&self, path: P) -> Result<()> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| ConfigError::FileWrite(parent.to_path_buf(), e))?;
        }
        let content = toml::to_string_pretty(self)?;
        fs::write(path, content).map_err(|e| ConfigError::FileWrite(path.to_path_buf(), e))?;
        Ok(())
    }

    pub fn record_server_prompt_values(&mut self, host: &str, name: &str, port: u16) {
        let host = host.trim().to_string();
        let name = name.trim().to_string();
        let port = port.to_string();

        push_history_value(&mut self.servers.hosts, host.clone(), HISTORY_MAX_ENTRIES);
        push_history_value(&mut self.servers.names, name.clone(), HISTORY_MAX_ENTRIES);
        push_history_value(&mut self.servers.ports, port.clone(), HISTORY_MAX_ENTRIES);

        push_history_entry(
            &mut self.servers.entries,
            ServerPromptHistoryEntry { host, name, port },
            HISTORY_MAX_ENTRIES,
        );
    }

    pub fn server_host_suggestions(&self) -> Vec<String> {
        let mut suggestions = Vec::new();

        for entry in self.servers.entries.iter().rev() {
            push_unique_value(&mut suggestions, &entry.host);
        }
        for host in self.servers.hosts.iter().rev() {
            push_unique_value(&mut suggestions, host);
        }

        suggestions
    }

    pub fn server_name_suggestions(&self) -> Vec<String> {
        let mut suggestions = Vec::new();

        for entry in self.servers.entries.iter().rev() {
            push_unique_value(&mut suggestions, &entry.name);
        }
        for name in self.servers.names.iter().rev() {
            push_unique_value(&mut suggestions, name);
        }

        suggestions
    }

    pub fn server_port_suggestions(&self) -> Vec<String> {
        let mut suggestions = Vec::new();

        for entry in self.servers.entries.iter().rev() {
            push_unique_value(&mut suggestions, &entry.port);
        }
        for port in self.servers.ports.iter().rev() {
            push_unique_value(&mut suggestions, port);
        }

        suggestions
    }

    pub fn server_name_suggestions_for_host(&self, host: &str) -> Vec<String> {
        let host = host.trim();
        if host.is_empty() {
            return Vec::new();
        }

        let mut suggestions = Vec::new();
        for entry in self.servers.entries.iter().rev() {
            if entry.host == host {
                push_unique_value(&mut suggestions, &entry.name);
            }
        }

        suggestions
    }

    pub fn server_port_suggestions_for(&self, host: &str, name: &str) -> Vec<String> {
        let host = host.trim();
        let name = name.trim();
        let mut suggestions = Vec::new();

        if !host.is_empty() && !name.is_empty() {
            for entry in self.servers.entries.iter().rev() {
                if entry.host == host && entry.name == name {
                    push_unique_value(&mut suggestions, &entry.port);
                }
            }
        }

        if !host.is_empty() {
            for entry in self.servers.entries.iter().rev() {
                if entry.host == host {
                    push_unique_value(&mut suggestions, &entry.port);
                }
            }
        }

        if !name.is_empty() {
            for entry in self.servers.entries.iter().rev() {
                if entry.name == name {
                    push_unique_value(&mut suggestions, &entry.port);
                }
            }
        }

        for port in self.server_port_suggestions() {
            push_unique_value(&mut suggestions, &port);
        }

        suggestions
    }
}

fn push_history_value(values: &mut Vec<String>, value: String, max_entries: usize) {
    if value.is_empty() {
        return;
    }

    if let Some(existing_idx) = values.iter().position(|existing| existing == &value) {
        values.remove(existing_idx);
    }
    values.push(value);

    if values.len() > max_entries {
        let remove_count = values.len() - max_entries;
        values.drain(0..remove_count);
    }
}

fn push_history_entry(
    entries: &mut Vec<ServerPromptHistoryEntry>,
    entry: ServerPromptHistoryEntry,
    max_entries: usize,
) {
    if entry.host.is_empty() || entry.name.is_empty() || entry.port.is_empty() {
        return;
    }

    if let Some(existing_idx) = entries.iter().position(|existing| existing == &entry) {
        entries.remove(existing_idx);
    }
    entries.push(entry);

    if entries.len() > max_entries {
        let remove_count = entries.len() - max_entries;
        entries.drain(0..remove_count);
    }
}

fn push_unique_value(values: &mut Vec<String>, value: &str) {
    if value.is_empty() {
        return;
    }
    if values.iter().any(|existing| existing == value) {
        return;
    }
    values.push(value.to_string());
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn parse_empty_defaults() {
        let parsed = CliHistoryToml::parse("").unwrap();
        assert_eq!(parsed, CliHistoryToml::default());
    }

    #[test]
    fn parse_rejects_legacy_servers_entries_array_header() {
        let parsed = CliHistoryToml::parse(
            r#"[servers]
hosts = ["203.0.113.10"]
names = ["prod"]
ports = ["2222"]

[[servers.entries]]
host = "203.0.113.10"
name = "prod"
port = "2222"
"#,
        );

        assert!(
            parsed.is_err(),
            "legacy [[servers.entries]] format should not be accepted"
        );
    }

    #[test]
    fn save_and_load_round_trip() {
        let temp_dir = TempDir::new().unwrap();
        let file = temp_dir.path().join("history").join("cli.toml");

        let mut history = CliHistoryToml::default();
        history.record_server_prompt_values("203.0.113.10", "prod", 2222);
        history.save_to_file(&file).unwrap();

        let loaded = CliHistoryToml::load_from_file(&file).unwrap();
        assert_eq!(loaded.servers.hosts, vec!["203.0.113.10"]);
        assert_eq!(loaded.servers.names, vec!["prod"]);
        assert_eq!(loaded.servers.ports, vec!["2222"]);
        assert_eq!(
            loaded.servers.entries,
            vec![ServerPromptHistoryEntry {
                host: "203.0.113.10".to_string(),
                name: "prod".to_string(),
                port: "2222".to_string()
            }]
        );
    }

    #[test]
    fn record_server_prompt_values_dedupes_and_updates_recency() {
        let mut history = CliHistoryToml::default();
        history.record_server_prompt_values("localhost", "local", 22);
        history.record_server_prompt_values("1.2.3.4", "prod", 2222);
        history.record_server_prompt_values("localhost", "local", 22);

        assert_eq!(history.servers.hosts, vec!["1.2.3.4", "localhost"]);
        assert_eq!(history.servers.names, vec!["prod", "local"]);
        assert_eq!(history.servers.ports, vec!["2222", "22"]);
        assert_eq!(
            history
                .servers
                .entries
                .iter()
                .map(|entry| format!("{}:{}:{}", entry.host, entry.name, entry.port))
                .collect::<Vec<_>>(),
            vec!["1.2.3.4:prod:2222", "localhost:local:22"]
        );
    }

    #[test]
    fn related_suggestions_prioritize_same_server_context() {
        let mut history = CliHistoryToml::default();
        history.record_server_prompt_values("api.internal", "api", 22);
        history.record_server_prompt_values("api.internal", "api-admin", 2222);
        history.record_server_prompt_values("db.internal", "db", 22);
        history.record_server_prompt_values("api.internal", "api", 2200);

        assert_eq!(
            history.server_name_suggestions_for_host("api.internal"),
            vec!["api".to_string(), "api-admin".to_string()]
        );

        assert_eq!(
            history.server_port_suggestions_for("api.internal", "api"),
            vec!["2200".to_string(), "22".to_string(), "2222".to_string()]
        );
    }

    #[test]
    fn load_uses_legacy_path_when_primary_missing() {
        let temp_dir = TempDir::new().unwrap();
        let primary = temp_dir.path().join("history.toml");
        let legacy = temp_dir.path().join("history").join("cli.toml");
        fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        fs::write(
            &legacy,
            r#"[servers]
hosts = ["legacy-host"]
names = ["legacy-name"]
ports = ["22"]
"#,
        )
        .unwrap();

        let loaded = CliHistoryToml::load_from_paths(&primary, &legacy).unwrap();
        assert_eq!(loaded.servers.hosts, vec!["legacy-host".to_string()]);
    }

    #[test]
    fn load_prefers_primary_over_legacy() {
        let temp_dir = TempDir::new().unwrap();
        let primary = temp_dir.path().join("history.toml");
        let legacy = temp_dir.path().join("history").join("cli.toml");
        fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        fs::write(
            &primary,
            r#"[servers]
hosts = ["primary-host"]
names = ["primary-name"]
ports = ["2200"]
"#,
        )
        .unwrap();
        fs::write(
            &legacy,
            r#"[servers]
hosts = ["legacy-host"]
names = ["legacy-name"]
ports = ["22"]
"#,
        )
        .unwrap();

        let loaded = CliHistoryToml::load_from_paths(&primary, &legacy).unwrap();
        assert_eq!(loaded.servers.hosts, vec!["primary-host".to_string()]);
    }

    #[test]
    fn save_uses_servers_table_without_servers_entries_array_header() {
        let temp_dir = TempDir::new().unwrap();
        let file = temp_dir.path().join("history.toml");

        let mut history = CliHistoryToml::default();
        history.record_server_prompt_values("203.0.113.10", "prod", 2222);
        history.record_server_prompt_values("203.0.113.11", "staging", 2200);
        history.save_to_file(&file).unwrap();

        let raw = fs::read_to_string(&file).unwrap();
        assert!(
            raw.contains("[servers]"),
            "history should include [servers]: {}",
            raw
        );
        assert!(
            !raw.contains("[[servers.entries]]"),
            "history should not include nested servers.entries header: {}",
            raw
        );
        assert!(
            raw.contains("entries = ["),
            "history should keep entries data in [servers]: {}",
            raw
        );
    }
}

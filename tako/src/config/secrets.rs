use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use super::error::{ConfigError, Result};

/// Secrets storage from .tako/secrets
///
/// Format:
/// ```json
/// {
///   "production": {
///     "DATABASE_URL": "encrypted_base64_value",
///     "API_KEY": "encrypted_base64_value"
///   },
///   "staging": {
///     "DATABASE_URL": "encrypted_base64_value"
///   }
/// }
/// ```
///
/// Secret names are plaintext (allows listing without decryption).
/// Secret values are encrypted with AES-256-GCM.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct SecretsStore {
    /// Map of environment name to secrets map
    #[serde(flatten)]
    pub environments: HashMap<String, HashMap<String, String>>,
}

impl SecretsStore {
    /// Get the default path for secrets (.tako/secrets in project root)
    pub fn default_path<P: AsRef<Path>>(project_dir: P) -> PathBuf {
        project_dir.as_ref().join(".tako").join("secrets")
    }

    /// Load secrets from a project directory
    pub fn load_from_dir<P: AsRef<Path>>(project_dir: P) -> Result<Self> {
        let path = Self::default_path(&project_dir);
        if path.exists() {
            Self::load_from_file(&path)
        } else {
            Ok(Self::default())
        }
    }

    /// Load secrets from a specific file
    pub fn load_from_file<P: AsRef<Path>>(path: P) -> Result<Self> {
        let content = fs::read_to_string(path.as_ref())
            .map_err(|e| ConfigError::FileRead(path.as_ref().to_path_buf(), e))?;
        Self::parse(&content)
    }

    /// Parse secrets from JSON content
    pub fn parse(content: &str) -> Result<Self> {
        if content.trim().is_empty() {
            return Ok(Self::default());
        }

        let environments: HashMap<String, HashMap<String, String>> = serde_json::from_str(content)?;

        let store = Self { environments };
        store.validate()?;
        Ok(store)
    }

    /// Validate secrets
    pub fn validate(&self) -> Result<()> {
        for (env_name, secrets) in &self.environments {
            // Validate environment name
            validate_environment_name(env_name)?;

            // Validate secret names
            for secret_name in secrets.keys() {
                validate_secret_name(secret_name)?;
            }
        }
        Ok(())
    }

    /// Save secrets to a project directory
    pub fn save_to_dir<P: AsRef<Path>>(&self, project_dir: P) -> Result<()> {
        let path = Self::default_path(&project_dir);
        self.save_to_file(&path)
    }

    /// Save secrets to a specific file
    pub fn save_to_file<P: AsRef<Path>>(&self, path: P) -> Result<()> {
        // Ensure parent directory exists
        if let Some(parent) = path.as_ref().parent() {
            fs::create_dir_all(parent)
                .map_err(|e| ConfigError::FileWrite(parent.to_path_buf(), e))?;
        }

        let content = serde_json::to_string_pretty(&self.environments)?;
        fs::write(path.as_ref(), content)
            .map_err(|e| ConfigError::FileWrite(path.as_ref().to_path_buf(), e))?;

        Ok(())
    }

    /// Get a secret value for an environment
    pub fn get(&self, env: &str, name: &str) -> Option<&String> {
        self.environments
            .get(env)
            .and_then(|secrets| secrets.get(name))
    }

    /// Set a secret value for an environment
    pub fn set(&mut self, env: &str, name: &str, value: String) -> Result<()> {
        validate_environment_name(env)?;
        validate_secret_name(name)?;

        self.environments
            .entry(env.to_string())
            .or_default()
            .insert(name.to_string(), value);

        Ok(())
    }

    /// Remove a secret from an environment
    pub fn remove(&mut self, env: &str, name: &str) -> Result<()> {
        let secrets = self
            .environments
            .get_mut(env)
            .ok_or_else(|| ConfigError::EnvironmentNotFound(env.to_string()))?;

        if secrets.remove(name).is_none() {
            return Err(ConfigError::SecretNotFound(name.to_string()));
        }

        // Remove environment if empty
        if secrets.is_empty() {
            self.environments.remove(env);
        }

        Ok(())
    }

    /// Remove a secret from all environments
    pub fn remove_all(&mut self, name: &str) -> Result<Vec<String>> {
        let mut removed_from = Vec::new();

        for (env_name, secrets) in &mut self.environments {
            if secrets.remove(name).is_some() {
                removed_from.push(env_name.clone());
            }
        }

        // Remove empty environments
        self.environments.retain(|_, secrets| !secrets.is_empty());

        if removed_from.is_empty() {
            return Err(ConfigError::SecretNotFound(name.to_string()));
        }

        Ok(removed_from)
    }

    /// Check if a secret exists in an environment
    pub fn contains(&self, env: &str, name: &str) -> bool {
        self.environments
            .get(env)
            .map(|secrets| secrets.contains_key(name))
            .unwrap_or(false)
    }

    /// Get all secret names across all environments
    pub fn all_secret_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .environments
            .values()
            .flat_map(|secrets| secrets.keys().cloned())
            .collect();
        names.sort();
        names.dedup();
        names
    }

    /// Get all environment names
    pub fn environment_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.environments.keys().cloned().collect();
        names.sort();
        names
    }

    /// Get secrets for an environment
    pub fn get_env(&self, env: &str) -> Option<&HashMap<String, String>> {
        self.environments.get(env)
    }

    /// Check for discrepancies (secrets missing in some environments)
    pub fn find_discrepancies(&self) -> Vec<SecretDiscrepancy> {
        let all_names = self.all_secret_names();
        let all_envs = self.environment_names();

        let mut discrepancies = Vec::new();

        for name in &all_names {
            let mut present_in = Vec::new();
            let mut missing_in = Vec::new();

            for env in &all_envs {
                if self.contains(env, name) {
                    present_in.push(env.clone());
                } else {
                    missing_in.push(env.clone());
                }
            }

            if !missing_in.is_empty() {
                discrepancies.push(SecretDiscrepancy {
                    name: name.clone(),
                    present_in,
                    missing_in,
                });
            }
        }

        discrepancies
    }

    /// Check if all secrets are present in all environments
    pub fn is_consistent(&self) -> bool {
        self.find_discrepancies().is_empty()
    }

    /// Get secrets count per environment
    pub fn count_by_env(&self) -> HashMap<String, usize> {
        self.environments
            .iter()
            .map(|(env, secrets)| (env.clone(), secrets.len()))
            .collect()
    }

    /// Check if empty
    pub fn is_empty(&self) -> bool {
        self.environments.is_empty()
    }

    /// Total number of secrets (across all environments)
    pub fn total_count(&self) -> usize {
        self.environments.values().map(|s| s.len()).sum()
    }
}

/// Represents a secret that is missing in some environments
#[derive(Debug, Clone, PartialEq)]
pub struct SecretDiscrepancy {
    pub name: String,
    pub present_in: Vec<String>,
    pub missing_in: Vec<String>,
}

/// Validate environment name format
fn validate_environment_name(name: &str) -> Result<()> {
    if name.is_empty() {
        return Err(ConfigError::Validation(
            "Environment name cannot be empty".to_string(),
        ));
    }

    // Only lowercase letters, numbers, and hyphens
    for c in name.chars() {
        if !c.is_ascii_lowercase() && !c.is_ascii_digit() && c != '-' {
            return Err(ConfigError::Validation(format!(
                "Environment name can only contain lowercase letters, numbers, and hyphens. Found: '{}'",
                c
            )));
        }
    }

    Ok(())
}

/// Validate secret name format (uppercase, underscores, numbers)
fn validate_secret_name(name: &str) -> Result<()> {
    if name.is_empty() {
        return Err(ConfigError::Validation(
            "Secret name cannot be empty".to_string(),
        ));
    }

    // Must start with uppercase letter
    if !name
        .chars()
        .next()
        .map(|c| c.is_ascii_uppercase())
        .unwrap_or(false)
    {
        return Err(ConfigError::Validation(
            "Secret name must start with an uppercase letter".to_string(),
        ));
    }

    // Only uppercase letters, numbers, and underscores
    for c in name.chars() {
        if !c.is_ascii_uppercase() && !c.is_ascii_digit() && c != '_' {
            return Err(ConfigError::Validation(format!(
                "Secret name can only contain uppercase letters, numbers, and underscores. Found: '{}'",
                c
            )));
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    // ==================== Parsing Tests ====================

    #[test]
    fn test_parse_empty() {
        let store = SecretsStore::parse("").unwrap();
        assert!(store.is_empty());
    }

    #[test]
    fn test_parse_empty_object() {
        let store = SecretsStore::parse("{}").unwrap();
        assert!(store.is_empty());
    }

    #[test]
    fn test_parse_single_environment() {
        let json = r#"{
            "production": {
                "DATABASE_URL": "encrypted_value_1",
                "API_KEY": "encrypted_value_2"
            }
        }"#;

        let store = SecretsStore::parse(json).unwrap();
        assert_eq!(store.environment_names(), vec!["production"]);
        assert_eq!(
            store.get("production", "DATABASE_URL"),
            Some(&"encrypted_value_1".to_string())
        );
        assert_eq!(
            store.get("production", "API_KEY"),
            Some(&"encrypted_value_2".to_string())
        );
    }

    #[test]
    fn test_parse_multiple_environments() {
        let json = r#"{
            "production": {
                "DATABASE_URL": "prod_db"
            },
            "staging": {
                "DATABASE_URL": "staging_db",
                "DEBUG": "true"
            }
        }"#;

        let store = SecretsStore::parse(json).unwrap();

        let mut envs = store.environment_names();
        envs.sort();
        assert_eq!(envs, vec!["production", "staging"]);

        assert_eq!(
            store.get("production", "DATABASE_URL"),
            Some(&"prod_db".to_string())
        );
        assert_eq!(
            store.get("staging", "DATABASE_URL"),
            Some(&"staging_db".to_string())
        );
        assert_eq!(store.get("staging", "DEBUG"), Some(&"true".to_string()));
    }

    // ==================== Validation Tests ====================

    #[test]
    fn test_validate_secret_name_valid() {
        assert!(validate_secret_name("DATABASE_URL").is_ok());
        assert!(validate_secret_name("API_KEY").is_ok());
        assert!(validate_secret_name("SECRET123").is_ok());
        assert!(validate_secret_name("A").is_ok());
        assert!(validate_secret_name("MY_SECRET_KEY_123").is_ok());
    }

    #[test]
    fn test_validate_secret_name_empty() {
        assert!(validate_secret_name("").is_err());
    }

    #[test]
    fn test_validate_secret_name_must_start_uppercase() {
        assert!(validate_secret_name("database_url").is_err());
        assert!(validate_secret_name("1SECRET").is_err());
        assert!(validate_secret_name("_SECRET").is_err());
    }

    #[test]
    fn test_validate_secret_name_invalid_chars() {
        assert!(validate_secret_name("DATABASE-URL").is_err());
        assert!(validate_secret_name("DATABASE.URL").is_err());
        assert!(validate_secret_name("database_url").is_err());
    }

    #[test]
    fn test_validate_environment_name_valid() {
        assert!(validate_environment_name("production").is_ok());
        assert!(validate_environment_name("staging").is_ok());
        assert!(validate_environment_name("prod-1").is_ok());
    }

    #[test]
    fn test_validate_environment_name_invalid() {
        assert!(validate_environment_name("").is_err());
        assert!(validate_environment_name("Production").is_err());
        assert!(validate_environment_name("prod_1").is_err());
    }

    // ==================== CRUD Operation Tests ====================

    #[test]
    fn test_set_secret() {
        let mut store = SecretsStore::default();

        store
            .set("production", "API_KEY", "secret123".to_string())
            .unwrap();

        assert_eq!(
            store.get("production", "API_KEY"),
            Some(&"secret123".to_string())
        );
    }

    #[test]
    fn test_set_secret_creates_environment() {
        let mut store = SecretsStore::default();

        store
            .set("production", "API_KEY", "secret123".to_string())
            .unwrap();
        store
            .set("staging", "API_KEY", "secret456".to_string())
            .unwrap();

        assert_eq!(store.environment_names().len(), 2);
    }

    #[test]
    fn test_set_overwrites_existing() {
        let mut store = SecretsStore::default();

        store
            .set("production", "API_KEY", "old_value".to_string())
            .unwrap();
        store
            .set("production", "API_KEY", "new_value".to_string())
            .unwrap();

        assert_eq!(
            store.get("production", "API_KEY"),
            Some(&"new_value".to_string())
        );
    }

    #[test]
    fn test_remove_secret() {
        let mut store = SecretsStore::default();

        store
            .set("production", "API_KEY", "secret".to_string())
            .unwrap();
        store
            .set("production", "DATABASE_URL", "db".to_string())
            .unwrap();

        store.remove("production", "API_KEY").unwrap();

        assert!(!store.contains("production", "API_KEY"));
        assert!(store.contains("production", "DATABASE_URL"));
    }

    #[test]
    fn test_remove_last_secret_removes_environment() {
        let mut store = SecretsStore::default();

        store
            .set("production", "API_KEY", "secret".to_string())
            .unwrap();
        store.remove("production", "API_KEY").unwrap();

        assert!(!store.environments.contains_key("production"));
    }

    #[test]
    fn test_remove_nonexistent_fails() {
        let mut store = SecretsStore::default();
        store
            .set("production", "API_KEY", "secret".to_string())
            .unwrap();

        let result = store.remove("production", "NONEXISTENT");
        assert!(matches!(result, Err(ConfigError::SecretNotFound(_))));
    }

    #[test]
    fn test_remove_from_nonexistent_env_fails() {
        let mut store = SecretsStore::default();

        let result = store.remove("production", "API_KEY");
        assert!(matches!(result, Err(ConfigError::EnvironmentNotFound(_))));
    }

    #[test]
    fn test_remove_all() {
        let mut store = SecretsStore::default();

        store
            .set("production", "API_KEY", "prod".to_string())
            .unwrap();
        store
            .set("staging", "API_KEY", "staging".to_string())
            .unwrap();
        store
            .set("staging", "DATABASE_URL", "db".to_string())
            .unwrap();

        let removed_from = store.remove_all("API_KEY").unwrap();

        assert_eq!(removed_from.len(), 2);
        assert!(!store.contains("production", "API_KEY"));
        assert!(!store.contains("staging", "API_KEY"));
        assert!(store.contains("staging", "DATABASE_URL"));

        // production environment should be removed (was only API_KEY)
        assert!(!store.environments.contains_key("production"));
    }

    // ==================== Discrepancy Tests ====================

    #[test]
    fn test_find_discrepancies_none() {
        let mut store = SecretsStore::default();

        store
            .set("production", "API_KEY", "prod".to_string())
            .unwrap();
        store
            .set("production", "DATABASE_URL", "prod_db".to_string())
            .unwrap();
        store
            .set("staging", "API_KEY", "staging".to_string())
            .unwrap();
        store
            .set("staging", "DATABASE_URL", "staging_db".to_string())
            .unwrap();

        assert!(store.is_consistent());
        assert!(store.find_discrepancies().is_empty());
    }

    #[test]
    fn test_find_discrepancies_some() {
        let mut store = SecretsStore::default();

        store
            .set("production", "API_KEY", "prod".to_string())
            .unwrap();
        store
            .set("production", "DATABASE_URL", "prod_db".to_string())
            .unwrap();
        store
            .set("staging", "API_KEY", "staging".to_string())
            .unwrap();
        // DATABASE_URL missing in staging

        let discrepancies = store.find_discrepancies();
        assert_eq!(discrepancies.len(), 1);
        assert_eq!(discrepancies[0].name, "DATABASE_URL");
        assert_eq!(discrepancies[0].missing_in, vec!["staging"]);
    }

    #[test]
    fn test_all_secret_names() {
        let mut store = SecretsStore::default();

        store.set("production", "API_KEY", "1".to_string()).unwrap();
        store
            .set("production", "DATABASE_URL", "2".to_string())
            .unwrap();
        store.set("staging", "API_KEY", "3".to_string()).unwrap();
        store.set("staging", "REDIS_URL", "4".to_string()).unwrap();

        let names = store.all_secret_names();
        assert_eq!(names, vec!["API_KEY", "DATABASE_URL", "REDIS_URL"]);
    }

    // ==================== File I/O Tests ====================

    #[test]
    fn test_save_and_load() {
        let temp_dir = TempDir::new().unwrap();

        let mut store = SecretsStore::default();
        store
            .set("production", "API_KEY", "secret123".to_string())
            .unwrap();
        store
            .set("staging", "API_KEY", "secret456".to_string())
            .unwrap();

        store.save_to_dir(&temp_dir).unwrap();

        let loaded = SecretsStore::load_from_dir(&temp_dir).unwrap();

        assert_eq!(
            loaded.get("production", "API_KEY"),
            Some(&"secret123".to_string())
        );
        assert_eq!(
            loaded.get("staging", "API_KEY"),
            Some(&"secret456".to_string())
        );
    }

    #[test]
    fn test_load_nonexistent_returns_default() {
        let temp_dir = TempDir::new().unwrap();
        let store = SecretsStore::load_from_dir(&temp_dir).unwrap();
        assert!(store.is_empty());
    }

    #[test]
    fn test_creates_parent_directory() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("subdir").join(".tako").join("secrets");

        let mut store = SecretsStore::default();
        store
            .set("production", "API_KEY", "secret".to_string())
            .unwrap();
        store.save_to_file(&path).unwrap();

        assert!(path.exists());
    }

    // ==================== Utility Tests ====================

    #[test]
    fn test_count_by_env() {
        let mut store = SecretsStore::default();

        store.set("production", "API_KEY", "1".to_string()).unwrap();
        store
            .set("production", "DATABASE_URL", "2".to_string())
            .unwrap();
        store.set("staging", "API_KEY", "3".to_string()).unwrap();

        let counts = store.count_by_env();
        assert_eq!(counts.get("production"), Some(&2));
        assert_eq!(counts.get("staging"), Some(&1));
    }

    #[test]
    fn test_total_count() {
        let mut store = SecretsStore::default();

        store.set("production", "API_KEY", "1".to_string()).unwrap();
        store
            .set("production", "DATABASE_URL", "2".to_string())
            .unwrap();
        store.set("staging", "API_KEY", "3".to_string()).unwrap();

        assert_eq!(store.total_count(), 3);
    }
}

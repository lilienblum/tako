use super::schema::*;
use std::collections::HashMap;
use std::path::Path;

const RESERVED_DERIVED_ENV_VARS: &[&str] = &["ENV"];

impl Config {
    /// Return the effective workflows config for a given server name.
    ///
    /// Precedence: `[servers.<name>.workflows]` > `[servers.workflows]` > defaults
    /// (`workers = 0`, `concurrency = 10`).
    pub fn workflows_for_server(&self, name: &str) -> WorkflowsConfig {
        if let Some(server) = self.servers.per_server.get(name)
            && let Some(wf) = &server.workflows
        {
            return wf.clone();
        }
        if let Some(wf) = &self.servers.workflows {
            return wf.clone();
        }
        WorkflowsConfig::default()
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
        for reserved in RESERVED_DERIVED_ENV_VARS {
            merged.remove(*reserved);
        }
        merged
    }

    pub fn ignored_reserved_var_warnings(&self) -> Vec<String> {
        let mut warnings = Vec::new();

        for reserved in RESERVED_DERIVED_ENV_VARS {
            if self.vars.contains_key(*reserved) {
                warnings.push(format!(
                    "[vars].{reserved} is ignored. Tako derives {reserved} automatically."
                ));
            }

            for env_name in self.vars_per_env.keys() {
                if self
                    .vars_per_env
                    .get(env_name)
                    .is_some_and(|vars| vars.contains_key(*reserved))
                {
                    warnings.push(format!(
                        "[vars.{env_name}].{reserved} is ignored. Tako derives {reserved} automatically."
                    ));
                }
            }
        }

        warnings
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
}

use std::collections::HashMap;
use std::path::Path;

#[derive(Debug, Clone, serde::Deserialize)]
pub(crate) struct ReleaseManifest {
    pub runtime: String,
    pub main: String,
    pub idle_timeout: u32,
    #[serde(default)]
    pub env_vars: HashMap<String, String>,
    #[serde(default)]
    pub runtime_version: Option<String>,
    #[serde(default)]
    pub package_manager: Option<String>,
}

pub(crate) fn load_release_manifest(release_dir: &Path) -> Result<ReleaseManifest, String> {
    let manifest_path = release_dir.join("app.json");
    let content = std::fs::read_to_string(&manifest_path).map_err(|e| {
        format!(
            "failed to read deploy manifest {}: {}",
            manifest_path.display(),
            e
        )
    })?;
    serde_json::from_str(&content).map_err(|e| {
        format!(
            "failed to parse deploy manifest {}: {}",
            manifest_path.display(),
            e
        )
    })
}

pub fn env_vars_from_release_dir(release_dir: &Path) -> Result<HashMap<String, String>, String> {
    Ok(load_release_manifest(release_dir)?.env_vars)
}

pub fn idle_timeout_secs_from_release_dir(release_dir: &Path) -> Result<u32, String> {
    Ok(load_release_manifest(release_dir)?.idle_timeout)
}

pub fn runtime_from_release_dir(release_dir: &Path) -> Result<String, String> {
    let manifest = load_release_manifest(release_dir)?;
    if manifest.runtime.trim().is_empty() {
        return Err(format!(
            "deploy manifest {} has empty runtime field",
            release_dir.join("app.json").display()
        ));
    }
    Ok(manifest.runtime)
}

/// Build the launch command from a manifest using the plugin system.
///
/// The manifest is declarative (runtime, main, package_manager). The plugin
/// provides the actual launch args and entrypoint path.
pub(crate) fn command_from_manifest(
    manifest: &ReleaseManifest,
    release_dir: &Path,
    runtime_bin: Option<&str>,
) -> Result<Vec<String>, String> {
    let manifest_path = release_dir.join("app.json");
    if manifest.main.trim().is_empty() {
        return Err(format!(
            "deploy manifest {} has empty main field",
            manifest_path.display()
        ));
    }

    let ctx = manifest
        .package_manager
        .as_ref()
        .map(|pm| tako_runtime::PluginContext {
            project_dir: release_dir,
            package_manager: Some(pm.as_str()),
        });
    let def = tako_runtime::runtime_def_for(&manifest.runtime, ctx.as_ref()).ok_or_else(|| {
        format!(
            "unsupported runtime '{}' in deploy manifest {}",
            manifest.runtime,
            manifest_path.display()
        )
    })?;

    let bin = runtime_bin
        .map(str::to_string)
        .unwrap_or_else(|| manifest.runtime.clone());
    let resolved_main = resolve_main_path(release_dir, &manifest.main);

    let cmd: Vec<String> = def
        .server
        .launch_args
        .iter()
        .map(|arg| match arg.as_str() {
            "{bin}" => bin.clone(),
            "{main}" => resolved_main.clone(),
            other => other.to_string(),
        })
        .collect();

    Ok(cmd)
}

/// Determine the command to launch an app from its release directory.
///
/// Release launch behavior is derived from deploy manifest (`app.json`) only.
pub fn command_for_release_dir(release_dir: &Path) -> Result<Vec<String>, String> {
    let manifest = load_release_manifest(release_dir)?;
    command_from_manifest(&manifest, release_dir, None)
}

/// Resolve the main entrypoint for the launch command.
/// - If the file exists on disk, return the absolute path.
/// - Otherwise pass through as-is (bare module specifier).
fn resolve_main_path(release_dir: &Path, main: &str) -> String {
    let candidate = release_dir.join(main);
    if candidate.is_file() {
        return candidate.to_string_lossy().to_string();
    }
    main.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn env_vars_from_release_dir_reads_env_vars_field() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("app.json"),
            r#"{"runtime":"bun","main":"index.ts","idle_timeout":300,"env_vars":{"NODE_ENV":"production","TAKO_BUILD":"v1"}}"#,
        )
        .unwrap();
        let vars = env_vars_from_release_dir(dir.path()).unwrap();
        assert_eq!(vars.get("NODE_ENV"), Some(&"production".to_string()));
        assert_eq!(vars.get("TAKO_BUILD"), Some(&"v1".to_string()));
    }

    #[test]
    fn env_vars_from_release_dir_returns_empty_when_field_missing() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("app.json"),
            r#"{"runtime":"bun","main":"index.ts","idle_timeout":300}"#,
        )
        .unwrap();
        let vars = env_vars_from_release_dir(dir.path()).unwrap();
        assert!(vars.is_empty());
    }

    #[test]
    fn env_vars_from_release_dir_errors_when_manifest_is_missing() {
        let dir = TempDir::new().unwrap();
        let err = env_vars_from_release_dir(dir.path()).unwrap_err();
        assert!(err.contains("failed to read deploy manifest"));
    }

    #[test]
    fn env_vars_from_release_dir_errors_on_invalid_json() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("app.json"), r#"not json"#).unwrap();
        let err = env_vars_from_release_dir(dir.path()).unwrap_err();
        assert!(err.contains("parse"));
    }

    #[test]
    fn idle_timeout_secs_from_release_dir_reads_required_field() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("app.json"),
            r#"{"runtime":"bun","main":"index.ts","idle_timeout":42}"#,
        )
        .unwrap();
        assert_eq!(idle_timeout_secs_from_release_dir(dir.path()).unwrap(), 42);
    }

    #[test]
    fn bun_command_uses_entrypoint_path() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("app.json"),
            r#"{"runtime":"bun","main":"server/entry.js","idle_timeout":300}"#,
        )
        .unwrap();

        let cmd = command_for_release_dir(dir.path()).unwrap();
        assert_eq!(cmd[0], "bun");
        assert_eq!(cmd[1], "run");
        assert!(cmd[2].contains("tako.sh/dist/entrypoints/bun.mjs"));
        assert_eq!(cmd.last().unwrap(), "server/entry.js");
    }

    #[test]
    fn errors_when_manifest_is_missing() {
        let dir = TempDir::new().unwrap();
        let err = command_for_release_dir(dir.path()).unwrap_err();
        assert!(err.contains("failed to read deploy manifest"));
    }

    #[test]
    fn errors_when_manifest_runtime_is_unknown() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("app.json"),
            r#"{"runtime":"python","main":"server/index.js","idle_timeout":300}"#,
        )
        .unwrap();
        let err = command_for_release_dir(dir.path()).unwrap_err();
        assert!(err.contains("unsupported runtime"));
    }

    #[test]
    fn node_command_uses_entrypoint_path() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("app.json"),
            r#"{"runtime":"node","main":"server/index.mjs","idle_timeout":300}"#,
        )
        .unwrap();

        let cmd = command_for_release_dir(dir.path()).unwrap();
        assert_eq!(cmd[0], "node");
        assert!(cmd.iter().any(|a| a.contains("entrypoints/node.mjs")));
        assert_eq!(cmd.last().unwrap(), "server/index.mjs");
    }

    #[test]
    fn deno_command_uses_entrypoint_path() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("app.json"),
            r#"{"runtime":"deno","main":"server/main.ts","idle_timeout":300}"#,
        )
        .unwrap();

        let cmd = command_for_release_dir(dir.path()).unwrap();
        assert_eq!(cmd[0], "deno");
        assert!(cmd.iter().any(|a| a.contains("entrypoints/deno.mjs")));
        assert_eq!(cmd.last().unwrap(), "server/main.ts");
    }

    #[test]
    fn errors_when_manifest_main_is_empty() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("app.json"),
            r#"{"runtime":"bun","main":"  ","idle_timeout":300}"#,
        )
        .unwrap();

        let err = command_for_release_dir(dir.path()).unwrap_err();
        assert!(err.contains("empty main"));
    }

    #[test]
    fn main_resolved_to_absolute_when_file_exists() {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/app.ts"), "export default {};\n").unwrap();
        std::fs::write(
            dir.path().join("app.json"),
            r#"{"runtime":"bun","main":"src/app.ts","idle_timeout":300}"#,
        )
        .unwrap();

        let cmd = command_for_release_dir(dir.path()).unwrap();
        assert_eq!(
            cmd.last().unwrap(),
            &dir.path().join("src/app.ts").to_string_lossy().to_string()
        );
    }

    #[test]
    fn bare_specifier_main_passed_through() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("app.json"),
            r#"{"runtime":"bun","main":"@tanstack/react-start/server-entry","idle_timeout":300}"#,
        )
        .unwrap();

        let cmd = command_for_release_dir(dir.path()).unwrap();
        assert_eq!(cmd.last().unwrap(), "@tanstack/react-start/server-entry");
    }

    #[test]
    fn runtime_version_deserialized_from_manifest() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("app.json"),
            r#"{"runtime":"bun","main":"index.ts","idle_timeout":300,"runtime_version":"1.2.0"}"#,
        )
        .unwrap();

        let manifest = load_release_manifest(dir.path()).unwrap();
        assert_eq!(manifest.runtime_version.as_deref(), Some("1.2.0"));
    }

    #[test]
    fn runtime_version_defaults_to_none() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("app.json"),
            r#"{"runtime":"bun","main":"index.ts","idle_timeout":300}"#,
        )
        .unwrap();

        let manifest = load_release_manifest(dir.path()).unwrap();
        assert!(manifest.runtime_version.is_none());
    }

    #[test]
    fn go_command_runs_binary_directly() {
        let dir = TempDir::new().unwrap();
        // Create the binary file so main resolves to absolute path
        std::fs::write(dir.path().join("app"), "").unwrap();
        std::fs::write(
            dir.path().join("app.json"),
            r#"{"runtime":"go","main":"app","idle_timeout":300}"#,
        )
        .unwrap();

        let cmd = command_for_release_dir(dir.path()).unwrap();
        // Go launch_args is ["{main}"] — binary runs directly, no runtime prefix
        assert_eq!(cmd.len(), 1);
        assert!(
            cmd[0].ends_with("/app"),
            "expected absolute path to binary, got: {}",
            cmd[0]
        );
    }

    #[test]
    fn go_command_no_bin_placeholder() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("app.json"),
            r#"{"runtime":"go","main":"my-server","idle_timeout":300}"#,
        )
        .unwrap();

        let cmd = command_for_release_dir(dir.path()).unwrap();
        assert_eq!(cmd.len(), 1);
        // When binary doesn't exist on disk, main is passed through as-is
        assert_eq!(cmd[0], "my-server");
    }
}

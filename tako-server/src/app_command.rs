use std::collections::HashMap;
use std::path::Path;

pub(crate) fn entrypoint_relative_path(runtime: &str) -> Option<&'static str> {
    match runtime {
        "bun" => Some("node_modules/tako.sh/src/entrypoints/bun.ts"),
        "node" => Some("node_modules/tako.sh/src/entrypoints/node.ts"),
        "deno" => Some("node_modules/tako.sh/src/entrypoints/deno.ts"),
        _ => None,
    }
}

#[derive(Debug, Clone, serde::Deserialize)]
struct ReleaseManifest {
    runtime: String,
    main: String,
    idle_timeout: u32,
    #[serde(default)]
    start: Vec<String>,
    #[serde(default)]
    env_vars: HashMap<String, String>,
    #[serde(default)]
    install: Option<String>,
}

fn load_release_manifest(release_dir: &Path) -> Result<ReleaseManifest, String> {
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

pub fn install_command_from_release_dir(release_dir: &Path) -> Result<Option<String>, String> {
    Ok(load_release_manifest(release_dir)?.install)
}

/// Determine the command to launch an app from its release directory.
///
/// Release launch behavior is derived from deploy manifest (`app.json`) only.
pub fn command_for_release_dir(release_dir: &Path) -> Result<Vec<String>, String> {
    let manifest_path = release_dir.join("app.json");
    let manifest = load_release_manifest(release_dir)?;
    if manifest.main.trim().is_empty() {
        return Err(format!(
            "deploy manifest {} has empty main field",
            manifest_path.display()
        ));
    }

    if !manifest.start.is_empty() {
        return Ok(
            manifest
                .start
                .into_iter()
                .map(|arg| {
                    if arg == "{main}" {
                        manifest.main.clone()
                    } else {
                        arg
                    }
                })
                .collect(),
        );
    }

    let rel_path = entrypoint_relative_path(&manifest.runtime).ok_or_else(|| {
        format!(
            "unsupported runtime '{}' in deploy manifest {}",
            manifest.runtime,
            manifest_path.display()
        )
    })?;
    let entrypoint = resolve_entrypoint_path(release_dir, rel_path);

    match manifest.runtime.as_str() {
        "bun" => Ok(vec![
            "bun".to_string(),
            "run".to_string(),
            entrypoint,
            manifest.main,
        ]),
        "node" => Ok(vec![
            "node".to_string(),
            "--experimental-strip-types".to_string(),
            entrypoint,
            manifest.main,
        ]),
        "deno" => Ok(vec![
            "deno".to_string(),
            "run".to_string(),
            "--allow-net".to_string(),
            "--allow-env".to_string(),
            "--allow-read".to_string(),
            entrypoint,
            manifest.main,
        ]),
        _ => unreachable!(),
    }
}

fn resolve_entrypoint_path(release_dir: &Path, relative_path: &str) -> String {
    let mut current = Some(release_dir);
    while let Some(dir) = current {
        let candidate = dir.join(relative_path);
        if candidate.is_file() {
            return candidate.to_string_lossy().to_string();
        }
        current = dir.parent();
    }
    release_dir
        .join(relative_path)
        .to_string_lossy()
        .to_string()
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
    fn uses_manifest_main_when_present() {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join("node_modules/tako.sh/src/entrypoints")).unwrap();
        std::fs::write(
            dir.path()
                .join("node_modules/tako.sh/src/entrypoints/bun.ts"),
            "export {};",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("app.json"),
            r#"{"runtime":"bun","main":"server/entry.js","idle_timeout":300}"#,
        )
        .unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/index.ts"), "export {};\n").unwrap();

        let cmd = command_for_release_dir(dir.path()).unwrap();
        assert_eq!(
            cmd,
            vec![
                "bun",
                "run",
                &dir.path()
                    .join("node_modules/tako.sh/src/entrypoints/bun.ts")
                    .to_string_lossy(),
                "server/entry.js"
            ]
        );
    }

    #[test]
    fn uses_manifest_start_command_when_present() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("app.json"),
            r#"{"runtime":"bun","main":"server/entry.js","idle_timeout":300,"start":["bun","run","node_modules/tako.sh/src/entrypoints/bun.ts","{main}"]}"#,
        )
        .unwrap();

        let cmd = command_for_release_dir(dir.path()).unwrap();
        assert_eq!(
            cmd,
            vec![
                "bun",
                "run",
                "node_modules/tako.sh/src/entrypoints/bun.ts",
                "server/entry.js"
            ]
        );
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
    fn falls_back_to_node_runtime_command_when_start_is_missing() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("app.json"),
            r#"{"runtime":"node","main":"server/index.mjs","idle_timeout":300}"#,
        )
        .unwrap();

        let cmd = command_for_release_dir(dir.path()).unwrap();
        assert_eq!(
            cmd,
            vec![
                "node",
                "--experimental-strip-types",
                &dir.path()
                    .join("node_modules/tako.sh/src/entrypoints/node.ts")
                    .to_string_lossy(),
                "server/index.mjs",
            ]
        );
    }

    #[test]
    fn falls_back_to_deno_runtime_command_when_start_is_missing() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("app.json"),
            r#"{"runtime":"deno","main":"server/main.ts","idle_timeout":300}"#,
        )
        .unwrap();

        let cmd = command_for_release_dir(dir.path()).unwrap();
        assert_eq!(
            cmd,
            vec![
                "deno",
                "run",
                "--allow-net",
                "--allow-env",
                "--allow-read",
                &dir.path()
                    .join("node_modules/tako.sh/src/entrypoints/deno.ts")
                    .to_string_lossy(),
                "server/main.ts",
            ]
        );
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
    fn resolves_bun_entrypoint_from_parent_node_modules() {
        let dir = TempDir::new().unwrap();
        let release_root = dir.path().join("releases/v1");
        let app_dir = release_root.join("apps/web");
        std::fs::create_dir_all(app_dir.join("src")).unwrap();
        std::fs::create_dir_all(release_root.join("node_modules/tako.sh/src/entrypoints")).unwrap();
        std::fs::write(
            release_root.join("node_modules/tako.sh/src/entrypoints/bun.ts"),
            "export {};",
        )
        .unwrap();
        std::fs::write(app_dir.join("src/app.ts"), "export default {};\n").unwrap();
        std::fs::write(
            app_dir.join("app.json"),
            r#"{"runtime":"bun","main":"src/app.ts","idle_timeout":300}"#,
        )
        .unwrap();

        let cmd = command_for_release_dir(&app_dir).unwrap();
        assert_eq!(cmd[0], "bun");
        assert_eq!(cmd[1], "run");
        assert_eq!(
            cmd[2],
            release_root
                .join("node_modules/tako.sh/src/entrypoints/bun.ts")
                .to_string_lossy()
        );
        assert_eq!(cmd[3], "src/app.ts");
    }

    #[test]
    fn uses_default_entrypoint_path_when_entrypoint_is_missing() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("app.json"),
            r#"{"runtime":"bun","main":"src/app.ts","idle_timeout":300}"#,
        )
        .unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/app.ts"), "export default {};\n").unwrap();

        let cmd = command_for_release_dir(dir.path()).unwrap();
        assert_eq!(cmd[0], "bun");
        assert_eq!(cmd[1], "run");
        assert_eq!(
            cmd[2],
            dir.path()
                .join("node_modules/tako.sh/src/entrypoints/bun.ts")
                .to_string_lossy()
        );
        assert_eq!(cmd[3], "src/app.ts");
    }
}

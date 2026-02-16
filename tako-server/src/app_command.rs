use std::path::{Path, PathBuf};

const BUN_WRAPPER_PATH: &str = "node_modules/tako.sh/src/wrapper.ts";

#[derive(Debug, serde::Deserialize)]
struct DeployArchiveManifest {
    runtime: String,
    main: String,
}

/// Determine the command to launch an app from its release directory.
///
/// For now this is intentionally minimal:
/// - If `package.json` has a `scripts.dev`, we run `bun run dev`.
/// - Otherwise we run `bun run <entry>` using a small set of conventional entry paths.
pub fn command_for_release_dir(release_dir: &Path) -> Result<Vec<String>, String> {
    if let Some(cmd) = command_from_archive_manifest(release_dir)? {
        return Ok(cmd);
    }

    let pkg_json = release_dir.join("package.json");
    if pkg_json.exists() {
        if has_dev_script(&pkg_json) {
            return Ok(vec![
                "bun".to_string(),
                "run".to_string(),
                "dev".to_string(),
            ]);
        }

        let entry = default_entry(release_dir).ok_or_else(|| {
            "could not find an entry point (expected src/index.ts, index.ts, server/index.mjs, or server/server.js)".to_string()
        })?;
        return Ok(vec![
            "bun".to_string(),
            "run".to_string(),
            entry.to_string_lossy().to_string(),
        ]);
    }

    // No package.json: still allow a plain entry file.
    let entry = default_entry(release_dir).ok_or_else(|| {
        "could not find an entry point (expected src/index.ts, index.ts, server/index.mjs, or server/server.js)".to_string()
    })?;
    Ok(vec![
        "bun".to_string(),
        "run".to_string(),
        entry.to_string_lossy().to_string(),
    ])
}

fn command_from_archive_manifest(release_dir: &Path) -> Result<Option<Vec<String>>, String> {
    let manifest_path = release_dir.join("app.json");
    if !manifest_path.exists() {
        return Ok(None);
    }

    let content = std::fs::read_to_string(&manifest_path).map_err(|e| {
        format!(
            "failed to read deploy manifest {}: {}",
            manifest_path.display(),
            e
        )
    })?;
    let manifest: DeployArchiveManifest = serde_json::from_str(&content).map_err(|e| {
        format!(
            "failed to parse deploy manifest {}: {}",
            manifest_path.display(),
            e
        )
    })?;
    if manifest.main.trim().is_empty() {
        return Err(format!(
            "deploy manifest {} has empty main field",
            manifest_path.display()
        ));
    }

    match manifest.runtime.as_str() {
        "bun" => Ok(Some(vec![
            "bun".to_string(),
            "run".to_string(),
            BUN_WRAPPER_PATH.to_string(),
            manifest.main,
        ])),
        other => Err(format!(
            "unsupported runtime '{}' in deploy manifest {}",
            other,
            manifest_path.display()
        )),
    }
}

fn has_dev_script(package_json_path: &Path) -> bool {
    let Ok(contents) = std::fs::read_to_string(package_json_path) else {
        return false;
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&contents) else {
        return false;
    };

    v.get("scripts")
        .and_then(|s| s.get("dev"))
        .and_then(|d| d.as_str())
        .is_some()
}

fn default_entry(release_dir: &Path) -> Option<PathBuf> {
    let candidates = [
        PathBuf::from("src/index.ts"),
        PathBuf::from("index.ts"),
        PathBuf::from("src/index.js"),
        PathBuf::from("index.js"),
        PathBuf::from("server/index.mjs"),
        PathBuf::from("server/index.js"),
        PathBuf::from("server/server.mjs"),
        PathBuf::from("server/server.js"),
    ];

    for rel in candidates {
        let p = release_dir.join(&rel);
        if p.exists() {
            return Some(rel);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn prefers_bun_run_dev_when_dev_script_exists() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("app.json"),
            r#"{"runtime":"bun","main":"server/index.mjs"}"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"name":"x","scripts":{"dev":"bun src/index.ts"}}"#,
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
                "node_modules/tako.sh/src/wrapper.ts",
                "server/index.mjs"
            ]
        );
    }

    #[test]
    fn uses_manifest_main_when_present() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("app.json"),
            r#"{"runtime":"bun","main":"server/entry.js"}"#,
        )
        .unwrap();
        std::fs::write(dir.path().join("package.json"), r#"{"name":"x"}"#).unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/index.ts"), "export {};\n").unwrap();

        let cmd = command_for_release_dir(dir.path()).unwrap();
        assert_eq!(
            cmd,
            vec![
                "bun",
                "run",
                "node_modules/tako.sh/src/wrapper.ts",
                "server/entry.js"
            ]
        );
    }

    #[test]
    fn falls_back_to_entry_guessing_when_manifest_missing() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("package.json"), r#"{"name":"x"}"#).unwrap();
        std::fs::write(dir.path().join("index.ts"), "export {};\n").unwrap();

        let cmd = command_for_release_dir(dir.path()).unwrap();
        assert_eq!(cmd, vec!["bun", "run", "index.ts"]);
    }

    #[test]
    fn errors_when_manifest_runtime_is_unknown() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("app.json"),
            r#"{"runtime":"node","main":"server/index.js"}"#,
        )
        .unwrap();
        let err = command_for_release_dir(dir.path()).unwrap_err();
        assert!(err.contains("unsupported runtime"));
    }

    #[test]
    fn errors_when_no_entry_found() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("package.json"), r#"{"name":"x"}"#).unwrap();

        let err = command_for_release_dir(dir.path()).unwrap_err();
        assert!(err.contains("entry"));
    }
}

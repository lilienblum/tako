use std::path::Path;

const BUN_WRAPPER_RELATIVE_PATH: &str = "node_modules/tako.sh/src/wrapper.ts";

#[derive(Debug, serde::Deserialize)]
struct DeployArchiveManifest {
    runtime: String,
    main: String,
    #[serde(default)]
    start: Option<Vec<String>>,
}

/// Determine the command to launch an app from its release directory.
///
/// Release launch behavior is derived from deploy manifest (`app.json`) only.
pub fn command_for_release_dir(release_dir: &Path) -> Result<Vec<String>, String> {
    command_from_archive_manifest(release_dir)?.ok_or_else(|| {
        format!(
            "missing deploy manifest {}",
            release_dir.join("app.json").display()
        )
    })
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

    if let Some(start) = manifest.start {
        if !start.is_empty() {
            return Ok(Some(
                start
                    .into_iter()
                    .map(|arg| {
                        if arg == "{main}" {
                            manifest.main.clone()
                        } else {
                            arg
                        }
                    })
                    .collect(),
            ));
        }
    }

    match manifest.runtime.as_str() {
        "bun" => {
            let wrapper = resolve_bun_wrapper_path(release_dir);
            Ok(Some(vec![
                "bun".to_string(),
                "run".to_string(),
                wrapper,
                manifest.main,
            ]))
        }
        other => Err(format!(
            "unsupported runtime '{}' in deploy manifest {}",
            other,
            manifest_path.display()
        )),
    }
}

fn resolve_bun_wrapper_path(release_dir: &Path) -> String {
    let mut current = Some(release_dir);
    while let Some(dir) = current {
        let candidate = dir.join(BUN_WRAPPER_RELATIVE_PATH);
        if candidate.is_file() {
            return candidate.to_string_lossy().to_string();
        }
        current = dir.parent();
    }
    release_dir
        .join(BUN_WRAPPER_RELATIVE_PATH)
        .to_string_lossy()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn uses_manifest_main_when_present() {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join("node_modules/tako.sh/src")).unwrap();
        std::fs::write(
            dir.path().join("node_modules/tako.sh/src/wrapper.ts"),
            "export {};",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("app.json"),
            r#"{"runtime":"bun","main":"server/entry.js"}"#,
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
                    .join("node_modules/tako.sh/src/wrapper.ts")
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
            r#"{"runtime":"bun","main":"server/entry.js","start":["bun","run","node_modules/tako.sh/src/wrapper.ts","{main}"]}"#,
        )
        .unwrap();

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
    fn errors_when_manifest_is_missing() {
        let dir = TempDir::new().unwrap();
        let err = command_for_release_dir(dir.path()).unwrap_err();
        assert!(err.contains("missing deploy manifest"));
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
    fn errors_when_manifest_main_is_empty() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("app.json"),
            r#"{"runtime":"bun","main":"  "}"#,
        )
        .unwrap();

        let err = command_for_release_dir(dir.path()).unwrap_err();
        assert!(err.contains("empty main"));
    }

    #[test]
    fn resolves_bun_wrapper_from_parent_node_modules() {
        let dir = TempDir::new().unwrap();
        let release_root = dir.path().join("releases/v1");
        let app_dir = release_root.join("apps/web");
        std::fs::create_dir_all(app_dir.join("src")).unwrap();
        std::fs::create_dir_all(release_root.join("node_modules/tako.sh/src")).unwrap();
        std::fs::write(
            release_root.join("node_modules/tako.sh/src/wrapper.ts"),
            "export {};",
        )
        .unwrap();
        std::fs::write(app_dir.join("src/app.ts"), "export default {};\n").unwrap();
        std::fs::write(
            app_dir.join("app.json"),
            r#"{"runtime":"bun","main":"src/app.ts"}"#,
        )
        .unwrap();

        let cmd = command_for_release_dir(&app_dir).unwrap();
        assert_eq!(cmd[0], "bun");
        assert_eq!(cmd[1], "run");
        assert_eq!(
            cmd[2],
            release_root
                .join("node_modules/tako.sh/src/wrapper.ts")
                .to_string_lossy()
        );
        assert_eq!(cmd[3], "src/app.ts");
    }

    #[test]
    fn uses_default_wrapper_path_when_manifest_wrapper_is_missing() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("app.json"),
            r#"{"runtime":"bun","main":"src/app.ts"}"#,
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
                .join("node_modules/tako.sh/src/wrapper.ts")
                .to_string_lossy()
        );
        assert_eq!(cmd[3], "src/app.ts");
    }
}

use std::path::{Path, PathBuf};

/// Determine the command to launch an app from its release directory.
///
/// For now this is intentionally minimal:
/// - If `package.json` has a `scripts.dev`, we run `bun run dev`.
/// - Otherwise we run `bun run <entry>` using a small set of conventional entry paths.
pub fn command_for_release_dir(release_dir: &Path) -> Result<Vec<String>, String> {
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
            "could not find an entry point (expected src/index.ts or index.ts)".to_string()
        })?;
        return Ok(vec![
            "bun".to_string(),
            "run".to_string(),
            entry.to_string_lossy().to_string(),
        ]);
    }

    // No package.json: still allow a plain entry file.
    let entry = default_entry(release_dir).ok_or_else(|| {
        "could not find an entry point (expected src/index.ts or index.ts)".to_string()
    })?;
    Ok(vec![
        "bun".to_string(),
        "run".to_string(),
        entry.to_string_lossy().to_string(),
    ])
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
            dir.path().join("package.json"),
            r#"{"name":"x","scripts":{"dev":"bun src/index.ts"}}"#,
        )
        .unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/index.ts"), "export {};\n").unwrap();

        let cmd = command_for_release_dir(dir.path()).unwrap();
        assert_eq!(cmd, vec!["bun", "run", "dev"]);
    }

    #[test]
    fn falls_back_to_bun_run_entry_when_no_dev_script() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("package.json"), r#"{"name":"x"}"#).unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/index.ts"), "export {};\n").unwrap();

        let cmd = command_for_release_dir(dir.path()).unwrap();
        assert_eq!(cmd, vec!["bun", "run", "src/index.ts"]);
    }

    #[test]
    fn errors_when_no_entry_found() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("package.json"), r#"{"name":"x"}"#).unwrap();

        let err = command_for_release_dir(dir.path()).unwrap_err();
        assert!(err.contains("entry"));
    }
}

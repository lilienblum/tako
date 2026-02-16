use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::{RuntimeAdapter, RuntimeMode};

/// Bun runtime adapter
#[derive(Debug, Clone)]
pub struct BunRuntime {
    /// Project directory
    dir: PathBuf,

    /// Detected entry point
    entry_point: PathBuf,
}

impl BunRuntime {
    fn git_repo_root(dir: &Path) -> Option<PathBuf> {
        let output = Command::new("git")
            .args(["rev-parse", "--show-toplevel"])
            .current_dir(dir)
            .output()
            .ok()?;

        if !output.status.success() {
            return None;
        }

        let s = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if s.is_empty() {
            return None;
        }

        Some(PathBuf::from(s))
    }

    fn root_looks_like_bun_project(root: &Path) -> bool {
        root.join("bun.lockb").exists()
            || root.join("bun.lock").exists()
            || root.join("bunfig.toml").exists()
            || root.join("package.json").exists()
    }

    /// Detect Bun runtime in a directory
    ///
    /// Heuristic:
    /// - Find git repo root (if available), otherwise use `dir`
    /// - If repo root has a Bun lockfile or package.json, treat as Bun
    /// - Use `dir` to detect the entry point (index.ts, src/index.ts, etc.)
    pub fn detect<P: AsRef<Path>>(dir: P) -> Option<Self> {
        let dir = dir.as_ref();

        let root = Self::git_repo_root(dir).unwrap_or_else(|| dir.to_path_buf());
        // Support both:
        // - a Bun repo where the git root has bun.lockb / bunfig / package.json
        // - a Bun app living inside a non-Bun monorepo (bun files present in `dir`)
        if !Self::root_looks_like_bun_project(&root) && !Self::root_looks_like_bun_project(dir) {
            return None;
        }

        // Detect entry point
        let entry_point = Self::detect_entry_point(dir)?;

        Some(Self {
            dir: dir.to_path_buf(),
            entry_point,
        })
    }

    /// Detect entry point from project
    ///
    /// Detection order:
    /// 1. package.json "main" field
    /// 2. src/index.tsx
    /// 3. src/index.ts
    /// 4. index.tsx
    /// 5. index.ts
    /// 6. src/index.js
    /// 7. index.js
    fn detect_entry_point<P: AsRef<Path>>(dir: P) -> Option<PathBuf> {
        let dir = dir.as_ref();

        // Check package.json main field
        if let Some(main) = Self::get_main_from_package_json(dir) {
            let main_path = dir.join(&main);
            if main_path.exists() {
                return Some(main_path);
            }
        }

        // Try common entry points in order
        let candidates = [
            "src/index.tsx",
            "src/index.ts",
            "index.tsx",
            "index.ts",
            "src/index.js",
            "index.js",
            "src/main.ts",
            "main.ts",
            "src/main.js",
            "main.js",
        ];

        for candidate in candidates {
            let path = dir.join(candidate);
            if path.exists() {
                return Some(path);
            }
        }

        None
    }

    /// Get "main" field from package.json
    fn get_main_from_package_json<P: AsRef<Path>>(dir: P) -> Option<String> {
        let package_json_path = dir.as_ref().join("package.json");
        let content = fs::read_to_string(&package_json_path).ok()?;
        let json: serde_json::Value = serde_json::from_str(&content).ok()?;
        json.get("main")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    }

    /// Get Bun version from command
    fn get_bun_version() -> Option<String> {
        let output = Command::new("bun").arg("--version").output().ok()?;

        if output.status.success() {
            let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
            Some(version)
        } else {
            None
        }
    }
}

impl RuntimeAdapter for BunRuntime {
    fn name(&self) -> &str {
        "bun"
    }

    fn version(&self) -> Option<String> {
        Self::get_bun_version()
    }

    fn entry_point(&self) -> &Path {
        &self.entry_point
    }

    fn build_command(&self) -> Option<Vec<String>> {
        // Check package.json for build script
        let package_json_path = self.dir.join("package.json");
        if let Ok(content) = fs::read_to_string(&package_json_path)
            && let Ok(json) = serde_json::from_str::<serde_json::Value>(&content)
            && let Some(scripts) = json.get("scripts")
            && scripts.get("build").is_some()
        {
            return Some(vec![
                "bun".to_string(),
                "run".to_string(),
                "build".to_string(),
            ]);
        }
        None
    }

    fn install_command(&self) -> Option<Vec<String>> {
        Some(vec![
            "bun".to_string(),
            "install".to_string(),
            "--frozen-lockfile".to_string(),
        ])
    }

    fn run_command(&self, _port: u16) -> Vec<String> {
        // Run the app via tako.sh wrapper so user entry points can be a simple
        // default export `{ fetch() { ... } }` without calling Bun.serve().
        let wrapper = self
            .dir
            .join("node_modules")
            .join("tako.sh")
            .join("src")
            .join("wrapper.ts");

        vec![
            "bun".to_string(),
            "--watch".to_string(),
            "run".to_string(),
            wrapper.to_string_lossy().to_string(),
            self.entry_point.to_string_lossy().to_string(),
        ]
    }

    fn env_vars(&self, mode: RuntimeMode) -> HashMap<String, String> {
        let mut vars = HashMap::new();

        match mode {
            RuntimeMode::Development => {
                vars.insert("NODE_ENV".to_string(), "development".to_string());
                vars.insert("BUN_ENV".to_string(), "development".to_string());
            }
            RuntimeMode::Production => {
                vars.insert("NODE_ENV".to_string(), "production".to_string());
                vars.insert("BUN_ENV".to_string(), "production".to_string());
            }
        }

        vars
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{self, File};
    use tempfile::TempDir;

    fn create_bun_project(temp_dir: &TempDir) {
        // Create bun.lockb
        File::create(temp_dir.path().join("bun.lockb")).unwrap();

        // Create package.json
        let package_json = r#"{
            "name": "test-app",
            "main": "src/index.ts"
        }"#;
        fs::write(temp_dir.path().join("package.json"), package_json).unwrap();

        // Create src/index.ts
        fs::create_dir_all(temp_dir.path().join("src")).unwrap();
        fs::write(temp_dir.path().join("src/index.ts"), "export default {}").unwrap();
    }

    #[test]
    fn test_detect_bun_from_lockb() {
        let temp_dir = TempDir::new().unwrap();

        // Create bun.lockb
        File::create(temp_dir.path().join("bun.lockb")).unwrap();
        fs::write(temp_dir.path().join("index.ts"), "export default {}").unwrap();

        let runtime = BunRuntime::detect(temp_dir.path());
        assert!(runtime.is_some());
        assert_eq!(runtime.unwrap().name(), "bun");
    }

    #[test]
    fn test_detect_bun_from_bunfig() {
        let temp_dir = TempDir::new().unwrap();

        // Create bunfig.toml
        fs::write(temp_dir.path().join("bunfig.toml"), "[install]\n").unwrap();
        fs::write(temp_dir.path().join("index.ts"), "export default {}").unwrap();

        let runtime = BunRuntime::detect(temp_dir.path());
        assert!(runtime.is_some());
    }

    #[test]
    fn test_detect_bun_from_package_json_bun_field() {
        let temp_dir = TempDir::new().unwrap();

        let package_json = r#"{"name": "test", "bun": {}}"#;
        fs::write(temp_dir.path().join("package.json"), package_json).unwrap();
        fs::write(temp_dir.path().join("index.ts"), "export default {}").unwrap();

        let runtime = BunRuntime::detect(temp_dir.path());
        assert!(runtime.is_some());
    }

    #[test]
    fn test_detect_bun_from_package_manager() {
        let temp_dir = TempDir::new().unwrap();

        let package_json = r#"{"name": "test", "packageManager": "bun@1.0.0"}"#;
        fs::write(temp_dir.path().join("package.json"), package_json).unwrap();
        fs::write(temp_dir.path().join("index.ts"), "export default {}").unwrap();

        let runtime = BunRuntime::detect(temp_dir.path());
        assert!(runtime.is_some());
    }

    #[test]
    fn test_detect_entry_point_from_package_json_main() {
        let temp_dir = TempDir::new().unwrap();
        create_bun_project(&temp_dir);

        let runtime = BunRuntime::detect(temp_dir.path()).unwrap();
        assert!(runtime.entry_point().ends_with("src/index.ts"));
    }

    #[test]
    fn test_detect_entry_point_fallback_src_index_ts() {
        let temp_dir = TempDir::new().unwrap();

        File::create(temp_dir.path().join("bun.lockb")).unwrap();
        fs::create_dir_all(temp_dir.path().join("src")).unwrap();
        fs::write(temp_dir.path().join("src/index.ts"), "").unwrap();

        let runtime = BunRuntime::detect(temp_dir.path()).unwrap();
        assert!(runtime.entry_point().ends_with("src/index.ts"));
    }

    #[test]
    fn test_detect_entry_point_fallback_src_index_tsx() {
        let temp_dir = TempDir::new().unwrap();

        File::create(temp_dir.path().join("bun.lockb")).unwrap();
        fs::create_dir_all(temp_dir.path().join("src")).unwrap();
        fs::write(temp_dir.path().join("src/index.tsx"), "").unwrap();

        let runtime = BunRuntime::detect(temp_dir.path()).unwrap();
        assert!(runtime.entry_point().ends_with("src/index.tsx"));
    }

    #[test]
    fn test_detect_entry_point_fallback_index_ts() {
        let temp_dir = TempDir::new().unwrap();

        File::create(temp_dir.path().join("bun.lockb")).unwrap();
        fs::write(temp_dir.path().join("index.ts"), "").unwrap();

        let runtime = BunRuntime::detect(temp_dir.path()).unwrap();
        assert!(runtime.entry_point().ends_with("index.ts"));
    }

    #[test]
    fn test_detect_entry_point_fallback_index_tsx() {
        let temp_dir = TempDir::new().unwrap();

        File::create(temp_dir.path().join("bun.lockb")).unwrap();
        fs::write(temp_dir.path().join("index.tsx"), "").unwrap();

        let runtime = BunRuntime::detect(temp_dir.path()).unwrap();
        assert!(runtime.entry_point().ends_with("index.tsx"));
    }

    #[test]
    fn test_detect_entry_point_fallback_package_json_without_main_uses_index_candidates() {
        let temp_dir = TempDir::new().unwrap();

        fs::write(temp_dir.path().join("package.json"), r#"{"name":"test"}"#).unwrap();
        fs::create_dir_all(temp_dir.path().join("src")).unwrap();
        fs::write(temp_dir.path().join("src/index.tsx"), "").unwrap();

        let runtime = BunRuntime::detect(temp_dir.path()).unwrap();
        assert!(runtime.entry_point().ends_with("src/index.tsx"));
    }

    #[test]
    fn test_detect_entry_point_fallback_order() {
        let temp_dir = TempDir::new().unwrap();

        File::create(temp_dir.path().join("bun.lockb")).unwrap();

        // Create both index.ts and src/index.ts
        fs::write(temp_dir.path().join("index.ts"), "").unwrap();
        fs::create_dir_all(temp_dir.path().join("src")).unwrap();
        fs::write(temp_dir.path().join("src/index.ts"), "").unwrap();

        let runtime = BunRuntime::detect(temp_dir.path()).unwrap();
        // src/index.ts should be preferred
        assert!(runtime.entry_point().ends_with("src/index.ts"));
    }

    #[test]
    fn test_return_none_when_not_bun_project() {
        let temp_dir = TempDir::new().unwrap();

        // Just a package.json without bun indicators
        fs::write(temp_dir.path().join("index.js"), "").unwrap();

        let runtime = BunRuntime::detect(temp_dir.path());
        assert!(runtime.is_none());
    }

    #[test]
    fn test_detect_allows_minimal_project_with_package_json() {
        let temp_dir = TempDir::new().unwrap();
        fs::write(temp_dir.path().join("package.json"), r#"{"name":"x"}"#).unwrap();
        fs::write(temp_dir.path().join("index.ts"), "export default {}").unwrap();

        let runtime = BunRuntime::detect(temp_dir.path());
        assert!(runtime.is_some());
        assert_eq!(
            runtime.unwrap().entry_point.file_name().unwrap(),
            "index.ts"
        );
    }

    #[test]
    fn test_detect_uses_git_root_for_bun_heuristic() {
        let temp_dir = TempDir::new().unwrap();

        // init git repo so `git rev-parse --show-toplevel` works
        let status = Command::new("git")
            .args(["init", "-q"])
            .current_dir(temp_dir.path())
            .status()
            .unwrap();
        assert!(status.success());

        // root package.json => bun by heuristic
        fs::write(temp_dir.path().join("package.json"), r#"{"name":"root"}"#).unwrap();

        // nested app dir with entry point
        let app_dir = temp_dir.path().join("apps").join("a");
        fs::create_dir_all(&app_dir).unwrap();
        fs::write(app_dir.join("index.ts"), "export default {}").unwrap();

        let runtime = BunRuntime::detect(&app_dir);
        assert!(runtime.is_some());
        assert_eq!(
            runtime.unwrap().entry_point.file_name().unwrap(),
            "index.ts"
        );
    }

    #[test]
    fn test_detect_works_for_bun_app_inside_non_bun_git_repo() {
        let temp_dir = TempDir::new().unwrap();

        // init git repo so `git rev-parse --show-toplevel` works
        let status = Command::new("git")
            .args(["init", "-q"])
            .current_dir(temp_dir.path())
            .status()
            .unwrap();
        assert!(status.success());

        // no package.json / bun.lockb at git root

        // bun app in subdir
        let app_dir = temp_dir.path().join("apps").join("a");
        fs::create_dir_all(&app_dir).unwrap();
        File::create(app_dir.join("bun.lockb")).unwrap();
        fs::write(app_dir.join("package.json"), r#"{"name":"a"}"#).unwrap();
        fs::write(app_dir.join("index.ts"), "export default {}\n").unwrap();

        let runtime = BunRuntime::detect(&app_dir);
        assert!(runtime.is_some());
        assert_eq!(runtime.unwrap().name(), "bun");
    }

    #[test]
    fn test_return_none_when_no_entry_point() {
        let temp_dir = TempDir::new().unwrap();

        // Bun indicator but no entry point
        File::create(temp_dir.path().join("bun.lockb")).unwrap();

        let runtime = BunRuntime::detect(temp_dir.path());
        assert!(runtime.is_none());
    }

    #[test]
    fn test_run_command_includes_entry_point() {
        let temp_dir = TempDir::new().unwrap();
        create_bun_project(&temp_dir);

        // Minimal tako.sh wrapper location expected by Bun runtime
        let wrapper = temp_dir
            .path()
            .join("node_modules")
            .join("tako.sh")
            .join("src");
        fs::create_dir_all(&wrapper).unwrap();
        fs::write(wrapper.join("wrapper.ts"), "// test wrapper").unwrap();

        let runtime = BunRuntime::detect(temp_dir.path()).unwrap();
        let cmd = runtime.run_command(3000);

        assert_eq!(cmd[0], "bun");
        assert_eq!(cmd[1], "--watch");
        assert_eq!(cmd[2], "run");
        assert!(cmd[3].contains("tako.sh"));
        assert!(cmd[3].contains("wrapper.ts"));
        assert!(cmd[4].contains("index.ts"));
    }

    #[test]
    fn test_env_vars_development() {
        let temp_dir = TempDir::new().unwrap();
        create_bun_project(&temp_dir);

        let runtime = BunRuntime::detect(temp_dir.path()).unwrap();
        let vars = runtime.env_vars(RuntimeMode::Development);

        assert_eq!(vars.get("NODE_ENV"), Some(&"development".to_string()));
        assert_eq!(vars.get("BUN_ENV"), Some(&"development".to_string()));
    }

    #[test]
    fn test_env_vars_production() {
        let temp_dir = TempDir::new().unwrap();
        create_bun_project(&temp_dir);

        let runtime = BunRuntime::detect(temp_dir.path()).unwrap();
        let vars = runtime.env_vars(RuntimeMode::Production);

        assert_eq!(vars.get("NODE_ENV"), Some(&"production".to_string()));
        assert_eq!(vars.get("BUN_ENV"), Some(&"production".to_string()));
    }

    #[test]
    fn test_build_command_when_script_exists() {
        let temp_dir = TempDir::new().unwrap();

        File::create(temp_dir.path().join("bun.lockb")).unwrap();
        fs::write(temp_dir.path().join("index.ts"), "").unwrap();

        let package_json = r#"{
            "name": "test",
            "scripts": {
                "build": "bun build ./src/index.ts"
            }
        }"#;
        fs::write(temp_dir.path().join("package.json"), package_json).unwrap();

        let runtime = BunRuntime::detect(temp_dir.path()).unwrap();
        let build_cmd = runtime.build_command();

        assert!(build_cmd.is_some());
        let cmd = build_cmd.unwrap();
        assert_eq!(cmd, vec!["bun", "run", "build"]);
    }

    #[test]
    fn test_build_command_when_no_script() {
        let temp_dir = TempDir::new().unwrap();

        File::create(temp_dir.path().join("bun.lockb")).unwrap();
        fs::write(temp_dir.path().join("index.ts"), "").unwrap();

        let package_json = r#"{"name": "test"}"#;
        fs::write(temp_dir.path().join("package.json"), package_json).unwrap();

        let runtime = BunRuntime::detect(temp_dir.path()).unwrap();
        let build_cmd = runtime.build_command();

        assert!(build_cmd.is_none());
    }

    #[test]
    fn test_install_command_uses_frozen_lockfile() {
        let temp_dir = TempDir::new().unwrap();

        File::create(temp_dir.path().join("bun.lockb")).unwrap();
        fs::write(temp_dir.path().join("index.ts"), "").unwrap();
        fs::write(temp_dir.path().join("package.json"), r#"{"name":"test"}"#).unwrap();

        let runtime = BunRuntime::detect(temp_dir.path()).unwrap();
        let install_cmd = runtime.install_command();

        assert_eq!(
            install_cmd,
            Some(vec![
                "bun".to_string(),
                "install".to_string(),
                "--frozen-lockfile".to_string(),
            ])
        );
    }

    #[test]
    fn test_runtime_name() {
        let temp_dir = TempDir::new().unwrap();
        create_bun_project(&temp_dir);

        let runtime = BunRuntime::detect(temp_dir.path()).unwrap();
        assert_eq!(runtime.name(), "bun");
    }
}

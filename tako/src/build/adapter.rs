use std::fs;
use std::path::Path;

const EMBEDDED_BUN_PRESET_CONTENT: &str = include_str!("../../../presets/bun/bun.toml");
const EMBEDDED_NODE_PRESET_CONTENT: &str = include_str!("../../../presets/node/node.toml");
const EMBEDDED_DENO_PRESET_CONTENT: &str = include_str!("../../../presets/deno/deno.toml");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildAdapter {
    Bun,
    Node,
    Deno,
    Unknown,
}

impl BuildAdapter {
    pub fn id(self) -> &'static str {
        match self {
            BuildAdapter::Bun => "bun",
            BuildAdapter::Node => "node",
            BuildAdapter::Deno => "deno",
            BuildAdapter::Unknown => "unknown",
        }
    }

    pub fn default_preset(self) -> &'static str {
        match self {
            BuildAdapter::Bun => "bun",
            BuildAdapter::Node => "node",
            BuildAdapter::Deno => "deno",
            BuildAdapter::Unknown => "bun",
        }
    }

    pub fn infer_main_entrypoint(self, project_dir: &Path) -> Option<String> {
        match self {
            BuildAdapter::Bun | BuildAdapter::Node => infer_javascript_main_entrypoint(project_dir),
            BuildAdapter::Deno => infer_deno_main_entrypoint(project_dir),
            BuildAdapter::Unknown => None,
        }
    }

    pub fn embedded_preset_default_main(self) -> Option<String> {
        match self {
            BuildAdapter::Bun => parse_embedded_preset_default_main(EMBEDDED_BUN_PRESET_CONTENT),
            BuildAdapter::Node => parse_embedded_preset_default_main(EMBEDDED_NODE_PRESET_CONTENT),
            BuildAdapter::Deno => parse_embedded_preset_default_main(EMBEDDED_DENO_PRESET_CONTENT),
            BuildAdapter::Unknown => None,
        }
    }
}

pub fn detect_build_adapter(project_dir: &Path) -> BuildAdapter {
    if project_dir.join("deno.json").is_file()
        || project_dir.join("deno.jsonc").is_file()
        || project_dir.join("deno.lock").is_file()
    {
        return BuildAdapter::Deno;
    }

    if project_dir.join("bun.lockb").is_file() || project_dir.join("bun.lock").is_file() {
        return BuildAdapter::Bun;
    }

    if project_dir.join("package.json").is_file() {
        return BuildAdapter::Node;
    }
    BuildAdapter::Unknown
}

fn infer_javascript_main_entrypoint(project_dir: &Path) -> Option<String> {
    if let Some(main) = infer_package_json_main(project_dir) {
        return Some(main);
    }

    const CANDIDATES: &[&str] = &[
        "index.ts",
        "index.tsx",
        "index.js",
        "index.jsx",
        "src/index.ts",
        "src/index.tsx",
        "src/index.js",
        "src/index.jsx",
    ];

    for candidate in CANDIDATES {
        if project_dir.join(candidate).is_file() {
            return Some((*candidate).to_string());
        }
    }

    None
}

fn infer_deno_main_entrypoint(project_dir: &Path) -> Option<String> {
    const CANDIDATES: &[&str] = &[
        "main.ts",
        "mod.ts",
        "index.ts",
        "src/main.ts",
        "src/mod.ts",
        "src/index.ts",
    ];
    for candidate in CANDIDATES {
        if project_dir.join(candidate).is_file() {
            return Some((*candidate).to_string());
        }
    }
    None
}

fn infer_package_json_main(project_dir: &Path) -> Option<String> {
    let package_json_path = project_dir.join("package.json");
    if !package_json_path.is_file() {
        return None;
    }

    let raw = fs::read_to_string(package_json_path).ok()?;
    let parsed: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let main = parsed.get("main")?.as_str()?.trim();
    if main.is_empty() {
        return None;
    }

    let normalized = main.replace('\\', "/");
    let normalized = normalized.trim_start_matches("./").to_string();
    if normalized.is_empty() || normalized.starts_with('/') || normalized.contains("..") {
        return None;
    }
    if project_dir.join(&normalized).is_file() {
        Some(normalized)
    } else {
        None
    }
}

fn parse_embedded_preset_default_main(content: &str) -> Option<String> {
    let parsed: toml::Value = toml::from_str(content).ok()?;
    let main = parsed.get("main")?.as_str()?.trim();
    if main.is_empty() {
        None
    } else {
        Some(main.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::{BuildAdapter, detect_build_adapter};
    use tempfile::TempDir;

    #[test]
    fn detect_build_adapter_prefers_deno_markers_over_package_json() {
        let temp = TempDir::new().unwrap();
        std::fs::write(temp.path().join("package.json"), r#"{"name":"demo"}"#).unwrap();
        std::fs::write(temp.path().join("deno.json"), "{}").unwrap();
        assert_eq!(detect_build_adapter(temp.path()), BuildAdapter::Deno);
    }

    #[test]
    fn detect_build_adapter_uses_bun_lock_for_bun() {
        let temp = TempDir::new().unwrap();
        std::fs::write(temp.path().join("bun.lock"), "").unwrap();
        assert_eq!(detect_build_adapter(temp.path()), BuildAdapter::Bun);
    }

    #[test]
    fn detect_build_adapter_uses_package_json_for_node() {
        let temp = TempDir::new().unwrap();
        std::fs::write(temp.path().join("package.json"), r#"{"name":"demo"}"#).unwrap();
        assert_eq!(detect_build_adapter(temp.path()), BuildAdapter::Node);
    }

    #[test]
    fn node_main_inference_prioritizes_package_json_main() {
        let temp = TempDir::new().unwrap();
        std::fs::create_dir_all(temp.path().join("src")).unwrap();
        std::fs::write(temp.path().join("src/index.ts"), "export {};").unwrap();
        std::fs::write(temp.path().join("custom-entry.js"), "export default {};").unwrap();
        std::fs::write(
            temp.path().join("package.json"),
            r#"{"name":"demo","main":"custom-entry.js"}"#,
        )
        .unwrap();

        let inferred = BuildAdapter::Node.infer_main_entrypoint(temp.path());
        assert_eq!(inferred.as_deref(), Some("custom-entry.js"));
    }

    #[test]
    fn bun_main_inference_uses_requested_candidate_priority() {
        let temp = TempDir::new().unwrap();
        std::fs::create_dir_all(temp.path().join("src")).unwrap();
        std::fs::write(temp.path().join("index.jsx"), "export default {};").unwrap();
        std::fs::write(temp.path().join("src/index.ts"), "export {};").unwrap();

        let inferred = BuildAdapter::Bun.infer_main_entrypoint(temp.path());
        assert_eq!(inferred.as_deref(), Some("index.jsx"));
    }

    #[test]
    fn deno_main_inference_uses_deno_candidates() {
        let temp = TempDir::new().unwrap();
        std::fs::create_dir_all(temp.path().join("src")).unwrap();
        std::fs::write(temp.path().join("src/mod.ts"), "export {};").unwrap();
        assert_eq!(
            BuildAdapter::Deno.infer_main_entrypoint(temp.path()),
            Some("src/mod.ts".to_string())
        );
    }

    #[test]
    fn bun_adapter_embedded_preset_has_default_main() {
        assert_eq!(
            BuildAdapter::Bun.embedded_preset_default_main(),
            Some("src/index.ts".to_string())
        );
    }
}

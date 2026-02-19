use std::fs;
use std::path::Path;

pub const BUILTIN_BUN_PRESET_PATH: &str = "presets/bun/bun.toml";
pub const BUILTIN_NODE_PRESET_PATH: &str = "presets/node/node.toml";
pub const BUILTIN_DENO_PRESET_PATH: &str = "presets/deno/deno.toml";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresetFamily {
    Js,
    Unknown,
}

impl PresetFamily {
    pub fn id(self) -> &'static str {
        match self {
            PresetFamily::Js => "js",
            PresetFamily::Unknown => "unknown",
        }
    }
}

const BUILTIN_BUN_PRESET_CONTENT: &str = r#"main = "src/index.ts"
dev = ["bun", "run", "dev"]
install = '''
if command -v mise >/dev/null 2>&1; then
  mise install >/dev/null 2>&1 || true
  if [ -f bun.lockb ] || [ -f bun.lock ]; then
    mise exec -- bun install --production --frozen-lockfile
  else
    mise exec -- bun install --production
  fi
else
  if [ -f bun.lockb ] || [ -f bun.lock ]; then
    bun install --production --frozen-lockfile
  else
    bun install --production
  fi
fi
'''
start = ["mise", "exec", "--", "bun", "run", "node_modules/tako.sh/src/wrapper.ts", "{main}"]

[build]
exclude = ["node_modules/"]
install = '''
if command -v mise >/dev/null 2>&1; then
  mise install >/dev/null 2>&1 || true
  if [ -f bun.lockb ] || [ -f bun.lock ]; then
    mise exec -- bun install --frozen-lockfile
  else
    mise exec -- bun install
  fi
else
  if [ -f bun.lockb ] || [ -f bun.lock ]; then
    bun install --frozen-lockfile
  else
    bun install
  fi
fi
'''
build = '''
cd "$TAKO_APP_DIR"
if command -v mise >/dev/null 2>&1; then
  mise install >/dev/null 2>&1 || true
  mise exec -- bun run --if-present build
else
  bun run --if-present build
fi
'''
targets = ["linux-x86_64-glibc", "linux-aarch64-glibc", "linux-x86_64-musl", "linux-aarch64-musl"]
"#;

const BUILTIN_NODE_PRESET_CONTENT: &str = r#"main = "index.js"
dev = ["node", "{main}"]
install = '''
if command -v mise >/dev/null 2>&1; then
  mise install >/dev/null 2>&1 || true
  if [ -f package-lock.json ]; then
    mise exec -- npm ci --omit=dev
  else
    mise exec -- npm install --omit=dev
  fi
else
  if [ -f package-lock.json ]; then
    npm ci --omit=dev
  else
    npm install --omit=dev
  fi
fi
'''
start = ["mise", "exec", "--", "node", "{main}"]

[build]
exclude = ["node_modules/"]
install = '''
if command -v mise >/dev/null 2>&1; then
  mise install >/dev/null 2>&1 || true
  if [ -f package-lock.json ]; then
    mise exec -- npm ci
  else
    mise exec -- npm install
  fi
else
  if [ -f package-lock.json ]; then
    npm ci
  else
    npm install
  fi
fi
'''
build = '''
cd "$TAKO_APP_DIR"
if command -v mise >/dev/null 2>&1; then
  mise install >/dev/null 2>&1 || true
  mise exec -- npm run --if-present build
else
  npm run --if-present build
fi
'''
targets = ["linux-x86_64-glibc", "linux-aarch64-glibc", "linux-x86_64-musl", "linux-aarch64-musl"]
"#;

const BUILTIN_DENO_PRESET_CONTENT: &str = r#"main = "main.ts"
dev = [
  "deno",
  "run",
  "--watch",
  "--allow-net",
  "--allow-env",
  "--allow-read",
  "{main}",
]
install = '''
if command -v mise >/dev/null 2>&1; then
  mise install >/dev/null 2>&1 || true
fi
'''
start = [
  "mise",
  "exec",
  "--",
  "deno",
  "run",
  "--allow-net",
  "--allow-env",
  "--allow-read",
  "{main}",
]

[build]
install = '''
if command -v mise >/dev/null 2>&1; then
  mise install >/dev/null 2>&1 || true
fi
'''
build = "true"
targets = ["linux-x86_64-glibc", "linux-aarch64-glibc", "linux-x86_64-musl", "linux-aarch64-musl"]
"#;

pub fn builtin_base_preset_content_for_alias(alias: &str) -> Option<&'static str> {
    match alias {
        "bun" => Some(BUILTIN_BUN_PRESET_CONTENT),
        "node" => Some(BUILTIN_NODE_PRESET_CONTENT),
        "deno" => Some(BUILTIN_DENO_PRESET_CONTENT),
        _ => None,
    }
}

pub fn builtin_base_preset_content_for_path(path: &str) -> Option<&'static str> {
    match path {
        BUILTIN_BUN_PRESET_PATH => Some(BUILTIN_BUN_PRESET_CONTENT),
        BUILTIN_NODE_PRESET_PATH => Some(BUILTIN_NODE_PRESET_CONTENT),
        BUILTIN_DENO_PRESET_PATH => Some(BUILTIN_DENO_PRESET_CONTENT),
        _ => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildAdapter {
    Bun,
    Node,
    Deno,
    Unknown,
}

impl BuildAdapter {
    pub fn from_id(value: &str) -> Option<Self> {
        match value {
            "bun" => Some(BuildAdapter::Bun),
            "node" => Some(BuildAdapter::Node),
            "deno" => Some(BuildAdapter::Deno),
            _ => None,
        }
    }

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

    pub fn preset_family(self) -> PresetFamily {
        match self {
            BuildAdapter::Bun | BuildAdapter::Node | BuildAdapter::Deno => PresetFamily::Js,
            BuildAdapter::Unknown => PresetFamily::Unknown,
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
            BuildAdapter::Bun => parse_embedded_preset_default_main(BUILTIN_BUN_PRESET_CONTENT),
            BuildAdapter::Node => parse_embedded_preset_default_main(BUILTIN_NODE_PRESET_CONTENT),
            BuildAdapter::Deno => parse_embedded_preset_default_main(BUILTIN_DENO_PRESET_CONTENT),
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
    use super::{
        BUILTIN_BUN_PRESET_PATH, BUILTIN_DENO_PRESET_PATH, BUILTIN_NODE_PRESET_PATH, BuildAdapter,
        PresetFamily, builtin_base_preset_content_for_alias, builtin_base_preset_content_for_path,
        detect_build_adapter,
    };
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

    #[test]
    fn builtin_base_preset_content_is_available_by_alias() {
        let bun = builtin_base_preset_content_for_alias("bun").expect("bun preset");
        let node = builtin_base_preset_content_for_alias("node").expect("node preset");
        let deno = builtin_base_preset_content_for_alias("deno").expect("deno preset");

        assert!(bun.contains("main = \"src/index.ts\""));
        assert!(node.contains("main = \"index.js\""));
        assert!(deno.contains("main = \"main.ts\""));
    }

    #[test]
    fn builtin_base_preset_content_is_available_by_path() {
        assert!(builtin_base_preset_content_for_path(BUILTIN_BUN_PRESET_PATH).is_some());
        assert!(builtin_base_preset_content_for_path(BUILTIN_NODE_PRESET_PATH).is_some());
        assert!(builtin_base_preset_content_for_path(BUILTIN_DENO_PRESET_PATH).is_some());
    }

    #[test]
    fn build_adapter_from_id_parses_known_values() {
        assert_eq!(BuildAdapter::from_id("bun"), Some(BuildAdapter::Bun));
        assert_eq!(BuildAdapter::from_id("node"), Some(BuildAdapter::Node));
        assert_eq!(BuildAdapter::from_id("deno"), Some(BuildAdapter::Deno));
        assert_eq!(BuildAdapter::from_id("python"), None);
    }

    #[test]
    fn build_adapter_maps_to_preset_family() {
        assert_eq!(BuildAdapter::Bun.preset_family(), PresetFamily::Js);
        assert_eq!(BuildAdapter::Node.preset_family(), PresetFamily::Js);
        assert_eq!(BuildAdapter::Deno.preset_family(), PresetFamily::Js);
        assert_eq!(BuildAdapter::Unknown.preset_family(), PresetFamily::Unknown);
        assert_eq!(PresetFamily::Js.id(), "js");
    }
}

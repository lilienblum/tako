mod cache;
pub mod download;
mod fetch;
mod registry;
mod types;

pub use cache::RuntimeCache;
pub use download::{DownloadManager, resolve_latest_version};
pub use fetch::{OFFICIAL_BRANCH, official_repo};
pub use registry::{load_runtime, parse_runtime};
pub use types::{
    DownloadDef, EntrypointDef, EnvsDef, ExtractDef, ManifestMainDef, PackageManagerDef, PresetDef,
    RuntimeDef, ServerDef, SymlinkDef, VersionSourceDef,
};

/// Known runtime IDs.
pub const KNOWN_RUNTIME_IDS: &[&str] = &["bun", "node", "deno"];

/// Known package manager IDs for the JavaScript family.
pub const KNOWN_JS_PACKAGE_MANAGER_IDS: &[&str] = &["bun", "npm", "pnpm", "yarn", "deno"];

/// Parse a runtime definition from the embedded TOML file for a known runtime.
/// The `id` is set from the filename, not the TOML content.
pub fn builtin_runtime(id: &str) -> Option<RuntimeDef> {
    let content = match id {
        "bun" => include_str!("../../registry/javascript/runtimes/bun.toml"),
        "node" => include_str!("../../registry/javascript/runtimes/node.toml"),
        "deno" => include_str!("../../registry/javascript/runtimes/deno.toml"),
        _ => return None,
    };
    let mut def = parse_runtime(content).ok()?;
    def.id = id.to_string();
    Some(def)
}

/// The embedded JS package managers manifest (single file with all PMs as sections).
const JS_PACKAGE_MANAGERS_CONTENT: &str =
    include_str!("../../registry/package_managers/javascript.toml");

/// Parse a package manager definition from the embedded manifest.
/// The `id` is set from the section name.
pub fn builtin_package_manager(id: &str) -> Option<PackageManagerDef> {
    let parsed: toml::Value = toml::from_str(JS_PACKAGE_MANAGERS_CONTENT).ok()?;
    let section = parsed.get(id)?.as_table()?;
    let content = toml::to_string(section).ok()?;
    let mut pm: PackageManagerDef = toml::from_str(&content).ok()?;
    pm.id = id.to_string();
    Some(pm)
}

/// Parse a package manager definition from a TOML string.
pub fn parse_package_manager(content: &str) -> Result<PackageManagerDef, String> {
    toml::from_str::<PackageManagerDef>(content)
        .map_err(|e| format!("failed to parse package manager TOML: {e}"))
}

/// Detect the package manager for a project directory.
/// 1. Check lockfiles (first match wins).
/// 2. Fall back to first PM whose binary is on PATH.
pub fn detect_package_manager(project_dir: &std::path::Path) -> Option<PackageManagerDef> {
    // Lockfile detection
    for &pm_id in KNOWN_JS_PACKAGE_MANAGER_IDS {
        if let Some(pm) = builtin_package_manager(pm_id) {
            for lockfile in &pm.lockfiles {
                if project_dir.join(lockfile).exists() {
                    return Some(pm);
                }
            }
        }
    }

    // Binary-on-PATH fallback
    for &pm_id in KNOWN_JS_PACKAGE_MANAGER_IDS {
        if is_binary_on_path(pm_id) {
            return builtin_package_manager(pm_id);
        }
    }

    None
}

fn is_binary_on_path(name: &str) -> bool {
    std::process::Command::new(name)
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_runtime_loads_all_known_runtimes() {
        for &id in KNOWN_RUNTIME_IDS {
            let def = builtin_runtime(id).unwrap_or_else(|| panic!("missing builtin for {id}"));
            assert_eq!(def.id, id);
        }
    }

    #[test]
    fn builtin_runtime_returns_none_for_unknown() {
        assert!(builtin_runtime("python").is_none());
    }

    #[test]
    fn builtin_package_manager_loads_all_known_pms() {
        for &id in KNOWN_JS_PACKAGE_MANAGER_IDS {
            let pm =
                builtin_package_manager(id).unwrap_or_else(|| panic!("missing builtin PM {id}"));
            assert_eq!(pm.id, id);
            assert!(pm.add.is_some());
        }
    }

    #[test]
    fn detect_package_manager_uses_lockfile() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join("pnpm-lock.yaml"), "").unwrap();
        let pm = detect_package_manager(dir.path()).unwrap();
        assert_eq!(pm.id, "pnpm");
    }

    #[test]
    fn detect_package_manager_prefers_lockfile_over_binary() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join("yarn.lock"), "").unwrap();
        let pm = detect_package_manager(dir.path()).unwrap();
        assert_eq!(pm.id, "yarn");
    }
}

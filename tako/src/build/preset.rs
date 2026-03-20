use std::fs;
use std::path::{Path, PathBuf};

use base64::Engine;
use serde::{Deserialize, Serialize};

use crate::build::adapter::{BuildAdapter, PresetGroup, builtin_base_preset_content_for_alias};

pub const BUILD_LOCK_RELATIVE_PATH: &str = ".tako/build.lock.json";
const FALLBACK_OFFICIAL_PRESET_REPO: &str = "tako-sh/presets";
const PACKAGE_REPOSITORY_URL: &str = env!("CARGO_PKG_REPOSITORY");
const OFFICIAL_PRESET_BRANCH: &str = "master";
const EMBEDDED_JS_GROUP_PRESETS_PATH: &str = "presets/javascript/javascript.toml";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PresetReference {
    OfficialAlias {
        name: String,
        commit: Option<String>,
    },
}

/// Lightweight preset metadata: just name, main, and assets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresetDefinition {
    pub name: String,
    pub main: Option<String>,
}

/// App preset providing entrypoint and asset defaults.
/// Loaded from `presets/<group>/<group>.toml` or embedded.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AppPreset {
    pub name: String,
    #[serde(default)]
    pub main: Option<String>,
    #[serde(default)]
    pub assets: Vec<String>,
}

/// Backward-compatible alias.
pub type BuildPreset = AppPreset;

const KNOWN_PRESET_FIELDS: &[&str] = &["name", "main", "assets"];

#[derive(Debug, Clone, Deserialize)]
struct AppPresetRaw {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    main: Option<String>,
    #[serde(default)]
    assets: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResolvedPresetSource {
    pub preset_ref: String,
    pub repo: String,
    pub path: String,
    pub commit: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct BuildLockFile {
    schema_version: u32,
    preset: ResolvedPresetSource,
}

pub fn parse_preset_reference(value: &str) -> Result<PresetReference, String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err("preset cannot be empty".to_string());
    }

    if trimmed.contains(':') {
        return Err(format!(
            "Invalid preset reference '{}'. GitHub preset references are not supported. Use an official alias like 'bun', 'js/tanstack-start', or 'js/tanstack-start@<commit-hash>'.",
            trimmed
        ));
    }

    let (without_at_commit, explicit_commit) = match trimmed.rsplit_once('@') {
        Some((name, commit)) => {
            let commit = commit.trim();
            if commit.is_empty() {
                return Err(format!(
                    "Invalid preset reference '{}': commit hash cannot be empty after '@'.",
                    trimmed
                ));
            }
            validate_commit_hash(trimmed, commit)?;
            (name.trim(), Some(commit.to_string()))
        }
        None => (trimmed, None),
    };

    let (name, commit) = match explicit_commit {
        Some(commit) => (without_at_commit.to_string(), Some(commit)),
        None => (without_at_commit.to_string(), None),
    };

    validate_official_alias(trimmed, &name)?;
    Ok(PresetReference::OfficialAlias { name, commit })
}

pub fn qualify_runtime_local_preset_ref(
    runtime: BuildAdapter,
    preset_ref: &str,
) -> Result<String, String> {
    let trimmed = preset_ref.trim();
    if trimmed.is_empty() {
        return Err("preset cannot be empty".to_string());
    }
    if trimmed.contains('/') {
        return Err(
            "preset must not include namespace (for example `js/tanstack-start`); set top-level `runtime` and use local preset name only."
                .to_string(),
        );
    }

    let preset_group = runtime.preset_group();
    if preset_group == PresetGroup::Unknown {
        return Err(format!(
            "Cannot resolve preset '{}' without a known runtime. Set top-level `runtime` explicitly.",
            trimmed
        ));
    }

    let (name, commit) = match trimmed.rsplit_once('@') {
        Some((name, commit)) if !name.trim().is_empty() && !commit.trim().is_empty() => {
            (name.trim(), Some(commit.trim()))
        }
        Some((_, commit)) if commit.trim().is_empty() => {
            return Err(format!(
                "Invalid preset reference '{}': commit hash cannot be empty after '@'.",
                trimmed
            ));
        }
        _ => (trimmed, None),
    };

    Ok(match commit {
        Some(commit) => format!("{}/{}@{}", preset_group.id(), name, commit),
        None => format!("{}/{}", preset_group.id(), name),
    })
}

fn validate_official_alias(raw_value: &str, alias: &str) -> Result<(), String> {
    if alias.is_empty() {
        return Err(format!(
            "Invalid preset alias '{}'. Alias is empty.",
            raw_value
        ));
    }
    let segments: Vec<&str> = alias.split('/').collect();
    if segments.len() > 2 {
        return Err(format!(
            "Invalid preset alias '{}'. Expected '<name>' or '<group>/<name>'.",
            raw_value
        ));
    }
    if segments.iter().any(|segment| segment.is_empty()) {
        return Err(format!(
            "Invalid preset alias '{}'. Alias segments cannot be empty.",
            raw_value
        ));
    }
    for segment in segments {
        if !segment
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-' || ch == '_')
        {
            return Err(format!(
                "Invalid preset alias '{}'. Alias must use lowercase letters, digits, '-' or '_' (with optional one '/').",
                raw_value
            ));
        }
    }
    Ok(())
}

fn validate_commit_hash(raw_value: &str, commit: &str) -> Result<(), String> {
    if commit.len() < 7 || commit.len() > 64 {
        return Err(format!(
            "Invalid preset reference '{}': commit hash '{}' must be 7-64 hexadecimal characters.",
            raw_value, commit
        ));
    }
    if !commit.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return Err(format!(
            "Invalid preset reference '{}': commit hash '{}' must be hexadecimal.",
            raw_value, commit
        ));
    }
    Ok(())
}

fn parse_github_repo_slug(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }

    let without_prefix = if let Some(rest) = trimmed.strip_prefix("https://github.com/") {
        rest
    } else if let Some(rest) = trimmed.strip_prefix("http://github.com/") {
        rest
    } else if let Some(rest) = trimmed.strip_prefix("git@github.com:") {
        rest
    } else {
        trimmed
    };

    let mut parts = without_prefix.trim_matches('/').split('/');
    let owner = parts.next()?.trim();
    let repo = parts.next()?.trim();
    if owner.is_empty() || repo.is_empty() {
        return None;
    }

    let normalized_repo = repo.strip_suffix(".git").unwrap_or(repo).trim();
    if normalized_repo.is_empty() {
        return None;
    }

    Some(format!("{owner}/{normalized_repo}"))
}

fn official_preset_repo() -> String {
    parse_github_repo_slug(PACKAGE_REPOSITORY_URL)
        .unwrap_or_else(|| FALLBACK_OFFICIAL_PRESET_REPO.to_string())
}

pub async fn load_build_preset(
    project_dir: &Path,
    preset_ref: &str,
) -> Result<(BuildPreset, ResolvedPresetSource), String> {
    let parsed_ref = parse_preset_reference(preset_ref)?;

    let (alias, commit_override) = match &parsed_ref {
        PresetReference::OfficialAlias { name, commit } => (name.as_str(), commit.clone()),
    };
    let path = official_alias_to_path(alias);
    let official_repo = official_preset_repo();
    let fetch_result = if let Some(commit) = commit_override {
        fetch_preset_content_by_commit(&official_repo, &path, &commit)
            .await
            .map(|content| (official_repo.clone(), commit, content))
    } else {
        fetch_preset_content_from_master_branch(&official_repo, &path)
            .await
            .map(|(resolved_commit, content)| (official_repo.clone(), resolved_commit, content))
    };

    let (repo, commit, preset) = match fetch_result {
        Ok((repo, commit, content)) => {
            let preset = parse_resolved_preset_from_content(&parsed_ref, &path, &content)?;
            (repo, commit, preset)
        }
        Err(fetch_error) => {
            // Fall back to embedded content for known runtime aliases and group presets.
            if BuildAdapter::from_id(alias).is_some() {
                tracing::debug!(
                    "Preset fetch failed, using embedded preset: {}",
                    fetch_error
                );
                let preset = parse_embedded_runtime_base_preset(alias)?;
                (official_repo.clone(), "embedded".to_string(), preset)
            } else if let Some(embedded_path) = official_group_manifest_path(PresetGroup::Js) {
                tracing::debug!(
                    "Preset fetch failed, trying embedded group manifest: {}",
                    fetch_error
                );
                let embedded_content = embedded_group_manifest_content(embedded_path);
                match parse_resolved_preset_from_content(&parsed_ref, &path, &embedded_content) {
                    Ok(preset) => (official_repo.clone(), "embedded".to_string(), preset),
                    Err(_) => return Err(fetch_error),
                }
            } else {
                return Err(fetch_error);
            }
        }
    };
    let resolved = ResolvedPresetSource {
        preset_ref: preset_ref.to_string(),
        repo,
        path,
        commit: commit.clone(),
    };
    if commit != "embedded" {
        write_locked_preset(project_dir, &resolved)?;
    }
    Ok((preset, resolved))
}

fn official_alias_to_path(alias: &str) -> String {
    match alias.split_once('/') {
        Some((group, _)) => format!("presets/{group}/{group}.toml"),
        None => {
            if let Some(adapter) = BuildAdapter::from_id(alias) {
                let group = adapter.preset_group().id();
                format!("presets/{group}/{group}.toml")
            } else {
                format!("presets/{alias}/{alias}.toml")
            }
        }
    }
}

fn official_group_manifest_path(group: PresetGroup) -> Option<&'static str> {
    match group {
        PresetGroup::Js => Some(EMBEDDED_JS_GROUP_PRESETS_PATH),
        PresetGroup::Unknown => None,
    }
}

fn embedded_group_manifest_content(path: &str) -> String {
    match path {
        "presets/javascript/javascript.toml" => {
            include_str!("../../presets/javascript/javascript.toml").to_string()
        }
        _ => String::new(),
    }
}

fn parse_group_manifest_preset_definitions(
    path: &str,
    content: &str,
) -> Result<Vec<PresetDefinition>, String> {
    let parsed: toml::Value = toml::from_str(content)
        .map_err(|e| format!("Failed to parse preset group manifest '{}': {e}", path))?;
    let manifest = parsed.as_table().ok_or_else(|| {
        format!(
            "Preset group manifest '{}' must be a TOML table with [preset-name] sections.",
            path
        )
    })?;

    let mut definitions = Vec::new();
    for (name, value) in manifest {
        let Some(preset_table) = value.as_table() else {
            continue;
        };
        let trimmed = name.trim();
        if trimmed.is_empty() {
            continue;
        }
        let main = preset_table
            .get("main")
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string);
        definitions.push(PresetDefinition {
            name: trimmed.to_string(),
            main,
        });
    }
    definitions.sort_by(|left, right| left.name.cmp(&right.name));
    definitions.dedup_by(|left, right| left.name == right.name);
    Ok(definitions)
}

fn parse_group_manifest_preset_names(path: &str, content: &str) -> Result<Vec<String>, String> {
    Ok(parse_group_manifest_preset_definitions(path, content)?
        .into_iter()
        .map(|definition| definition.name)
        .collect())
}

pub async fn load_available_group_preset_definitions(
    group: PresetGroup,
) -> Result<Vec<PresetDefinition>, String> {
    let Some(path) = official_group_manifest_path(group) else {
        return Err(format!(
            "Preset group '{}' is not supported for preset listing.",
            group.id()
        ));
    };

    let official_repo = official_preset_repo();
    let (_commit, content) = fetch_preset_content_from_master_branch(&official_repo, path).await?;
    parse_group_manifest_preset_definitions(path, &content)
}

pub async fn load_available_group_presets(group: PresetGroup) -> Result<Vec<String>, String> {
    let Some(path) = official_group_manifest_path(group) else {
        return Err(format!(
            "Preset group '{}' is not supported for preset listing.",
            group.id()
        ));
    };
    let official_repo = official_preset_repo();
    let (_commit, content) = fetch_preset_content_from_master_branch(&official_repo, path).await?;
    parse_group_manifest_preset_names(path, &content)
}

fn parse_resolved_preset_from_content(
    parsed_ref: &PresetReference,
    path: &str,
    content: &str,
) -> Result<BuildPreset, String> {
    match parsed_ref {
        PresetReference::OfficialAlias { name, .. } => {
            parse_official_alias_preset_content(name, path, content)
        }
    }
}

fn parse_official_alias_preset_content(
    alias: &str,
    path: &str,
    content: &str,
) -> Result<BuildPreset, String> {
    if let Some((_group, preset_name)) = alias.split_once('/') {
        return parse_group_preset_content(path, content, preset_name);
    }
    if BuildAdapter::from_id(alias).is_some() {
        return parse_group_preset_content(path, content, alias).or_else(|error| {
            if error == missing_group_preset_error(alias, path) {
                parse_embedded_runtime_base_preset(alias)
            } else {
                Err(error)
            }
        });
    }
    parse_and_validate_preset(content, alias)
}

fn missing_group_preset_error(preset_name: &str, path: &str) -> String {
    format!("Preset '{}' was not found in '{}'.", preset_name, path)
}

fn parse_embedded_runtime_base_preset(alias: &str) -> Result<BuildPreset, String> {
    let content = builtin_base_preset_content_for_alias(alias).ok_or_else(|| {
        format!(
            "Missing built-in base preset content for runtime '{}'.",
            alias
        )
    })?;
    parse_and_validate_preset(&content, alias)
}

fn parse_group_preset_content(
    path: &str,
    content: &str,
    preset_name: &str,
) -> Result<BuildPreset, String> {
    let parsed: toml::Value = toml::from_str(content)
        .map_err(|e| format!("Failed to parse preset group manifest '{}': {e}", path))?;
    let manifest = parsed.as_table().ok_or_else(|| {
        format!(
            "Preset group manifest '{}' must be a TOML table with [preset-name] sections.",
            path
        )
    })?;
    let preset = manifest
        .get(preset_name)
        .ok_or_else(|| format!("Preset '{}' was not found in '{}'.", preset_name, path))?;
    let preset_table = preset.as_table().ok_or_else(|| {
        format!(
            "Preset '{}' in '{}' must be a table section ([{}]).",
            preset_name, path, preset_name
        )
    })?;
    let preset_content = toml::to_string(preset_table).map_err(|e| {
        format!(
            "Failed to parse preset '{}' in '{}': {}",
            preset_name, path, e
        )
    })?;
    parse_and_validate_preset(&preset_content, preset_name)
}

pub fn parse_and_validate_preset(
    content: &str,
    inferred_name: &str,
) -> Result<AppPreset, String> {
    // Warn on unknown fields (legacy preset fields like dev, build, install, start).
    if let Ok(value) = toml::from_str::<toml::Value>(content) {
        if let Some(table) = value.as_table() {
            for key in table.keys() {
                if !KNOWN_PRESET_FIELDS.contains(&key.as_str()) {
                    tracing::warn!("Preset has unknown field '{}' — only name, main, assets are supported", key);
                }
            }
        }
    }

    let raw: AppPresetRaw = toml::from_str(content)
        .map_err(|e| format!("Failed to parse preset TOML: {e}"))?;

    let name = raw
        .name
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| inferred_name.to_string());
    if name.is_empty() {
        return Err(
            "Preset name is empty. Set top-level `name` or use a .toml file name with a non-empty stem."
                .to_string(),
        );
    }

    Ok(AppPreset {
        name,
        main: raw.main,
        assets: raw.assets,
    })
}

pub fn apply_adapter_base_runtime_defaults(
    preset: &mut AppPreset,
    adapter: BuildAdapter,
    plugin_ctx: Option<&tako_runtime::PluginContext>,
) -> Result<(), String> {
    if adapter == BuildAdapter::Unknown {
        return Ok(());
    }

    let def = tako_runtime::runtime_def_for(adapter.id(), plugin_ctx).ok_or_else(|| {
        format!(
            "Missing built-in runtime definition for '{}'.",
            adapter.id()
        )
    })?;

    if preset.main.is_none() {
        preset.main = def.preset.main;
    }

    Ok(())
}

pub fn infer_adapter_from_preset_reference(preset_ref: &str) -> BuildAdapter {
    let Ok(reference) = parse_preset_reference(preset_ref) else {
        return BuildAdapter::Unknown;
    };
    match reference {
        PresetReference::OfficialAlias { name, .. } => {
            infer_adapter_from_official_alias_name(&name)
        }
    }
}

fn infer_adapter_from_official_alias_name(alias: &str) -> BuildAdapter {
    let group_or_name = alias.split('/').next().unwrap_or(alias);
    BuildAdapter::from_id(group_or_name).unwrap_or(BuildAdapter::Unknown)
}

#[cfg(test)]
fn read_locked_preset(project_dir: &Path) -> Result<Option<ResolvedPresetSource>, String> {
    let lock_path = project_dir.join(BUILD_LOCK_RELATIVE_PATH);
    if !lock_path.exists() {
        return Ok(None);
    }

    let raw = fs::read_to_string(&lock_path)
        .map_err(|e| format!("Failed to read {}: {e}", lock_path.display()))?;
    let lock: BuildLockFile = serde_json::from_str(&raw)
        .map_err(|e| format!("Failed to parse {}: {e}", lock_path.display()))?;
    Ok(Some(lock.preset))
}

fn write_locked_preset(project_dir: &Path, resolved: &ResolvedPresetSource) -> Result<(), String> {
    let lock_path = project_dir.join(BUILD_LOCK_RELATIVE_PATH);
    if let Some(parent) = lock_path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create {}: {e}", parent.display()))?;
    }
    let lock = BuildLockFile {
        schema_version: 1,
        preset: resolved.clone(),
    };
    let json = serde_json::to_string_pretty(&lock)
        .map_err(|e| format!("Failed to serialize build lock: {e}"))?;
    fs::write(&lock_path, json).map_err(|e| format!("Failed to write {}: {e}", lock_path.display()))
}

async fn fetch_preset_content_by_commit(
    repo: &str,
    path: &str,
    commit: &str,
) -> Result<String, String> {
    let url = format!("https://raw.githubusercontent.com/{repo}/{commit}/{path}");
    let client = reqwest::Client::new();
    let response = client
        .get(url)
        .header("User-Agent", "tako-cli")
        .send()
        .await
        .map_err(|_e| "Failed to fetch preset".to_string())?;
    if !response.status().is_success() {
        return Err("Failed to fetch preset".to_string());
    }
    response
        .text()
        .await
        .map_err(|_e| "Failed to fetch preset".to_string())
}

async fn fetch_preset_content_from_master_branch(
    repo: &str,
    path: &str,
) -> Result<(String, String), String> {
    let Some((owner, repository)) = repo.split_once('/') else {
        return Err("Failed to fetch preset".to_string());
    };
    let url = format!(
        "https://api.github.com/repos/{owner}/{repository}/contents/{path}?ref={OFFICIAL_PRESET_BRANCH}"
    );
    let client = reqwest::Client::new();
    let response = client
        .get(url)
        .header("User-Agent", "tako-cli")
        .send()
        .await
        .map_err(|_e| "Failed to fetch preset".to_string())?;
    if !response.status().is_success() {
        return Err("Failed to fetch preset".to_string());
    }
    let raw = response
        .text()
        .await
        .map_err(|_e| "Failed to fetch preset".to_string())?;
    let json: serde_json::Value =
        serde_json::from_str(&raw).map_err(|_e| "Failed to fetch preset".to_string())?;

    let sha = json
        .get("sha")
        .and_then(|v| v.as_str())
        .map(ToOwned::to_owned)
        .ok_or_else(|| "Failed to fetch preset".to_string())?;
    let content_b64 = json
        .get("content")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "Failed to fetch preset".to_string())?;
    let normalized = content_b64.replace('\n', "");
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(normalized)
        .map_err(|_e| "Failed to fetch preset".to_string())?;
    let content = String::from_utf8(bytes).map_err(|_e| "Failed to fetch preset".to_string())?;
    Ok((sha, content))
}

pub fn lock_file_path(project_dir: &Path) -> PathBuf {
    project_dir.join(BUILD_LOCK_RELATIVE_PATH)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::build::adapter::builtin_base_preset_content_for_alias;

    fn parse_preset(raw: &str) -> Result<AppPreset, String> {
        parse_and_validate_preset(raw, "bun")
    }

    #[test]
    fn parse_github_repo_slug_accepts_https_ssh_and_slug_formats() {
        assert_eq!(
            parse_github_repo_slug("https://github.com/lilienblum/tako"),
            Some("lilienblum/tako".to_string())
        );
        assert_eq!(
            parse_github_repo_slug("git@github.com:lilienblum/tako.git"),
            Some("lilienblum/tako".to_string())
        );
        assert_eq!(
            parse_github_repo_slug("lilienblum/tako"),
            Some("lilienblum/tako".to_string())
        );
    }

    #[test]
    fn parse_github_repo_slug_rejects_invalid_values() {
        assert_eq!(parse_github_repo_slug(""), None);
        assert_eq!(parse_github_repo_slug("lilienblum"), None);
        assert_eq!(
            parse_github_repo_slug("https://example.com/lilienblum/tako"),
            None
        );
    }

    #[test]
    fn official_preset_repo_uses_package_repository_slug() {
        let expected = parse_github_repo_slug(PACKAGE_REPOSITORY_URL).unwrap();
        assert_eq!(official_preset_repo(), expected);
    }

    #[test]
    fn parse_preset_reference_accepts_official_alias() {
        let parsed = parse_preset_reference("bun").unwrap();
        assert_eq!(
            parsed,
            PresetReference::OfficialAlias {
                name: "bun".to_string(),
                commit: None,
            }
        );
    }

    #[test]
    fn parse_preset_reference_accepts_official_alias_with_commit() {
        let parsed = parse_preset_reference("bun@abc1234").unwrap();
        assert_eq!(
            parsed,
            PresetReference::OfficialAlias {
                name: "bun".to_string(),
                commit: Some("abc1234".to_string()),
            }
        );
    }

    #[test]
    fn parse_preset_reference_accepts_namespaced_official_alias() {
        let parsed = parse_preset_reference("javascript/tanstack-start").unwrap();
        assert_eq!(
            parsed,
            PresetReference::OfficialAlias {
                name: "javascript/tanstack-start".to_string(),
                commit: None,
            }
        );
    }

    #[test]
    fn parse_preset_reference_accepts_namespaced_official_alias_with_commit() {
        let parsed = parse_preset_reference("js/tanstack-start@abc1234").unwrap();
        assert_eq!(
            parsed,
            PresetReference::OfficialAlias {
                name: "js/tanstack-start".to_string(),
                commit: Some("abc1234".to_string()),
            }
        );
    }

    #[test]
    fn parse_preset_reference_rejects_invalid_values() {
        assert!(parse_preset_reference("").is_err());
        assert!(parse_preset_reference("github:owner/repo").is_err());
        assert!(parse_preset_reference("github:owner/repo/path.jsonc").is_err());
        assert!(parse_preset_reference("github:owner/repo/path.toml").is_err());
        assert!(parse_preset_reference("bun/abc12345/extra").is_err());
        assert!(parse_preset_reference("bun@").is_err());
        assert!(parse_preset_reference("js/tanstack-start@").is_err());
        assert!(parse_preset_reference("Bun").is_err());
    }

    #[test]
    fn official_alias_to_path_maps_group_layout() {
        assert_eq!(
            official_alias_to_path("bun"),
            "presets/javascript/javascript.toml"
        );
        assert_eq!(
            official_alias_to_path("javascript/tanstack-start"),
            "presets/javascript/javascript.toml"
        );
        assert_eq!(
            official_alias_to_path("node"),
            "presets/javascript/javascript.toml"
        );
        assert_eq!(
            official_alias_to_path("deno"),
            "presets/javascript/javascript.toml"
        );
    }

    #[test]
    fn official_group_manifest_path_supports_known_families() {
        assert_eq!(
            official_group_manifest_path(PresetGroup::Js),
            Some("presets/javascript/javascript.toml")
        );
        assert_eq!(official_group_manifest_path(PresetGroup::Unknown), None);
    }

    #[test]
    fn parse_group_manifest_preset_names_collects_sorted_sections() {
        let names = parse_group_manifest_preset_names(
            "presets/javascript/javascript.toml",
            r#"
[zeta]
main = "z.ts"

foo = "bar"

[alpha]
main = "a.ts"
"#,
        )
        .unwrap();
        assert_eq!(names, vec!["alpha".to_string(), "zeta".to_string()]);
    }

    #[test]
    fn parse_group_manifest_preset_definitions_reads_optional_main() {
        let definitions = parse_group_manifest_preset_definitions(
            "presets/javascript/javascript.toml",
            r#"
[tanstack-start]
main = "dist/server/tako-entry.mjs"

[no-main]
foo = "bar"
"#,
        )
        .unwrap();
        assert_eq!(
            definitions,
            vec![
                PresetDefinition {
                    name: "no-main".to_string(),
                    main: None,
                },
                PresetDefinition {
                    name: "tanstack-start".to_string(),
                    main: Some("dist/server/tako-entry.mjs".to_string()),
                },
            ]
        );
    }

    #[test]
    fn load_available_group_presets_rejects_unknown_group() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let err = runtime
            .block_on(load_available_group_presets(PresetGroup::Unknown))
            .unwrap_err();
        assert!(err.contains("not supported"));
    }

    #[test]
    fn embedded_bun_preset_parses() {
        let content = builtin_base_preset_content_for_alias("bun").expect("embedded bun preset");
        let preset = parse_and_validate_preset(&content, "bun").unwrap();
        assert_eq!(preset.name, "bun");
        // The embedded bun preset only provides main (and possibly assets).
        assert!(preset.main.is_some());
    }

    #[test]
    fn embedded_bun_tanstack_start_preset_parses() {
        let content = r#"
[tanstack-start]
main = "dist/server/tako-entry.mjs"
assets = ["dist/client"]
"#;
        let preset = parse_official_alias_preset_content(
            "javascript/tanstack-start",
            "presets/javascript/javascript.toml",
            content,
        )
        .unwrap();
        assert_eq!(preset.name, "tanstack-start");
        assert_eq!(preset.main.as_deref(), Some("dist/server/tako-entry.mjs"));
        assert_eq!(preset.assets, vec!["dist/client"]);
    }

    #[test]
    fn runtime_alias_uses_embedded_base_when_missing_from_manifest() {
        let content = r#"
[tanstack-start]
main = "dist/server/tako-entry.mjs"
"#;
        let preset = parse_official_alias_preset_content(
            "bun",
            "presets/javascript/javascript.toml",
            content,
        )
        .expect("runtime alias should use built-in preset fallback");
        assert_eq!(preset.name, "bun");
        // Embedded base preset provides a default main for the bun runtime.
        assert!(preset.main.is_some());
    }

    #[test]
    fn parse_official_alias_preset_content_rejects_missing_non_runtime_group_alias() {
        let content = r#"
[tanstack-start]
main = "dist/server/tako-entry.mjs"
"#;
        let err = parse_official_alias_preset_content(
            "javascript/missing",
            "presets/javascript/javascript.toml",
            content,
        )
        .expect_err("non-runtime group alias should still require manifest section");
        assert!(err.contains("Preset 'missing' was not found"));
    }

    #[test]
    fn infer_adapter_from_preset_reference_supports_official_aliases() {
        assert_eq!(
            infer_adapter_from_preset_reference("bun"),
            BuildAdapter::Bun
        );
        assert_eq!(
            infer_adapter_from_preset_reference("javascript/tanstack-start"),
            BuildAdapter::Unknown
        );
        assert_eq!(
            infer_adapter_from_preset_reference("node"),
            BuildAdapter::Node
        );
        assert_eq!(
            infer_adapter_from_preset_reference("deno"),
            BuildAdapter::Deno
        );
        assert_eq!(
            infer_adapter_from_preset_reference("github:owner/repo/presets/custom.toml"),
            BuildAdapter::Unknown
        );
        assert_eq!(
            infer_adapter_from_preset_reference("bun-tanstack-start"),
            BuildAdapter::Unknown
        );
    }

    #[test]
    fn apply_adapter_base_runtime_defaults_fills_missing_main_from_runtime() {
        let raw = r#"
name = "tanstack-start"
assets = ["dist/client"]
"#;
        let mut preset = parse_preset(raw).unwrap();
        apply_adapter_base_runtime_defaults(&mut preset, BuildAdapter::Bun, None).unwrap();

        // main was not set, so it gets filled from the bun runtime default.
        assert!(preset.main.is_some());
        // assets are untouched by the runtime defaults.
        assert_eq!(preset.assets, vec!["dist/client".to_string()]);
    }

    #[test]
    fn apply_adapter_base_runtime_defaults_keeps_explicit_main() {
        let raw = r#"
name = "custom-bun"
main = "custom-main.ts"
"#;
        let mut preset = parse_preset(raw).unwrap();
        apply_adapter_base_runtime_defaults(&mut preset, BuildAdapter::Bun, None).unwrap();

        assert_eq!(preset.main.as_deref(), Some("custom-main.ts"));
    }

    #[test]
    fn apply_adapter_base_runtime_defaults_skips_unknown_adapter() {
        let raw = r#"
name = "custom"
"#;
        let mut preset = parse_preset(raw).unwrap();
        apply_adapter_base_runtime_defaults(&mut preset, BuildAdapter::Unknown, None).unwrap();

        // Unknown adapter does nothing — main stays None.
        assert!(preset.main.is_none());
    }

    #[test]
    fn parse_and_validate_preset_accepts_name_main_assets() {
        let raw = r#"
name = "bun"
main = "index.ts"
assets = ["dist/client", "public"]
"#;
        let preset = parse_preset(raw).unwrap();
        assert_eq!(preset.name, "bun");
        assert_eq!(preset.main.as_deref(), Some("index.ts"));
        assert_eq!(
            preset.assets,
            vec!["dist/client".to_string(), "public".to_string()]
        );
    }

    #[test]
    fn parse_and_validate_preset_uses_inferred_name_when_missing() {
        let raw = r#"
main = "index.ts"
"#;
        let preset = parse_preset(raw).unwrap();
        assert_eq!(preset.name, "bun");
    }

    #[test]
    fn parse_and_validate_preset_defaults_to_empty_assets() {
        let raw = r#"
name = "bun"
main = "index.ts"
"#;
        let preset = parse_preset(raw).unwrap();
        assert!(preset.assets.is_empty());
    }

    #[test]
    fn parse_and_validate_preset_accepts_unknown_fields_with_warning() {
        // Unknown fields are accepted (parsed successfully) but logged as warnings.
        let raw = r#"
name = "bun"
main = "index.ts"
extra_field = "ignored"
"#;
        let preset = parse_preset(raw).unwrap();
        assert_eq!(preset.name, "bun");
        assert_eq!(preset.main.as_deref(), Some("index.ts"));
    }

    #[test]
    fn parse_and_validate_preset_accepts_top_level_assets() {
        let raw = r#"
name = "bun"
assets = ["dist/client"]
"#;
        let preset = parse_preset(raw).unwrap();
        assert_eq!(preset.assets, vec!["dist/client".to_string()]);
    }

    #[test]
    fn lock_round_trip_writes_and_reads_build_lock_file() {
        let temp = tempfile::TempDir::new().unwrap();
        let resolved = ResolvedPresetSource {
            preset_ref: "bun".to_string(),
            repo: "tako-sh/presets".to_string(),
            path: "presets/javascript/javascript.toml".to_string(),
            commit: "abc123".to_string(),
        };
        write_locked_preset(temp.path(), &resolved).unwrap();
        let loaded = read_locked_preset(temp.path()).unwrap().unwrap();
        assert_eq!(loaded, resolved);
        assert!(lock_file_path(temp.path()).exists());
    }

    #[test]
    fn fetch_preset_content_from_master_branch_returns_generic_fetch_error() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let err = runtime
            .block_on(fetch_preset_content_from_master_branch(
                "invalid-repo-slug",
                "presets/javascript/javascript.toml",
            ))
            .unwrap_err();
        assert_eq!(err, "Failed to fetch preset");
    }
}

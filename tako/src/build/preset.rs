use std::fs;
use std::path::{Path, PathBuf};

use base64::Engine;
use serde::{Deserialize, Serialize};

use crate::build::adapter::{
    BUILTIN_BUN_PRESET_PATH, BUILTIN_DENO_PRESET_PATH, BUILTIN_NODE_PRESET_PATH, BuildAdapter,
    PresetFamily, builtin_base_preset_content_for_alias, builtin_base_preset_content_for_path,
};

pub const BUILD_LOCK_RELATIVE_PATH: &str = ".tako/build.lock.json";
const FALLBACK_OFFICIAL_PRESET_REPO: &str = "tako-sh/presets";
const PACKAGE_REPOSITORY_URL: &str = env!("CARGO_PKG_REPOSITORY");
const OFFICIAL_PRESET_BRANCH: &str = "master";
const EMBEDDED_PRESET_REPO: &str = "embedded";
const EMBEDDED_JS_FAMILY_PRESETS_PATH: &str = "presets/js.toml";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PresetReference {
    OfficialAlias {
        name: String,
        commit: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FamilyPresetDefinition {
    pub name: String,
    pub main: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BuildPreset {
    pub name: String,
    #[serde(default)]
    pub main: Option<String>,
    #[serde(default)]
    pub builder_image: Option<String>,
    #[serde(default)]
    pub build: BuildPresetBuild,
    #[serde(default)]
    pub dev: Vec<String>,
    #[serde(default)]
    pub install: Option<String>,
    #[serde(default)]
    pub start: Vec<String>,
    #[serde(default)]
    pub targets: std::collections::HashMap<String, BuildPresetTarget>,
    #[serde(default)]
    pub target_defaults: BuildPresetTargetDefaults,
    #[serde(default)]
    pub assets: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct BuildPresetBuild {
    #[serde(default)]
    pub exclude: Vec<String>,
    #[serde(default)]
    pub install: Option<String>,
    #[serde(default)]
    pub build: Option<String>,
    #[serde(default)]
    pub targets: Vec<String>,
    #[serde(default)]
    pub container: bool,
    #[serde(skip)]
    pub targets_explicit: bool,
    #[serde(skip)]
    pub container_explicit: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BuildPresetTarget {
    #[serde(default)]
    pub builder_image: Option<String>,
    #[serde(default)]
    pub install: Option<String>,
    #[serde(default)]
    pub build: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct BuildPresetTargetDefaults {
    #[serde(default)]
    pub builder_image: Option<String>,
    #[serde(default)]
    pub install: Option<String>,
    #[serde(default)]
    pub build: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct BuildPresetRaw {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    main: Option<String>,
    #[serde(default)]
    build: BuildPresetRawBuild,
    #[serde(default)]
    dev: Vec<String>,
    #[serde(default)]
    install: Option<String>,
    #[serde(default)]
    start: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct BuildPresetRawBuild {
    #[serde(default)]
    exclude: Vec<String>,
    #[serde(default)]
    assets: Vec<String>,
    #[serde(default)]
    install: Option<String>,
    #[serde(default)]
    build: Option<String>,
    #[serde(default)]
    targets: Option<toml::Value>,
    #[serde(default)]
    container: Option<bool>,
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

    let preset_family = runtime.preset_family();
    if preset_family == PresetFamily::Unknown {
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
        Some(commit) => format!("{}/{}@{}", preset_family.id(), name, commit),
        None => format!("{}/{}", preset_family.id(), name),
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
            "Invalid preset alias '{}'. Expected '<name>' or '<family>/<name>'.",
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
    let (repo, commit, content) = if let Some(commit) = commit_override {
        let content = fetch_preset_content_by_commit(&official_repo, &path, &commit).await?;
        (official_repo.clone(), commit, content)
    } else {
        match fetch_preset_content_from_master_branch(&official_repo, &path).await {
            Ok((resolved_commit, content)) => (official_repo.clone(), resolved_commit, content),
            Err(default_branch_error) => {
                if let Some(content) = embedded_official_preset_content(&path) {
                    (
                        EMBEDDED_PRESET_REPO.to_string(),
                        embedded_content_hash(content),
                        content.to_string(),
                    )
                } else {
                    return Err(default_branch_error);
                }
            }
        }
    };

    let preset = parse_resolved_preset_from_content(&parsed_ref, &path, &content)?;
    let resolved = ResolvedPresetSource {
        preset_ref: preset_ref.to_string(),
        repo,
        path,
        commit,
    };
    write_locked_preset(project_dir, &resolved)?;
    Ok((preset, resolved))
}

fn official_alias_to_path(alias: &str) -> String {
    match alias {
        "bun" => BUILTIN_BUN_PRESET_PATH.to_string(),
        "node" => BUILTIN_NODE_PRESET_PATH.to_string(),
        "deno" => BUILTIN_DENO_PRESET_PATH.to_string(),
        _ => match alias.split_once('/') {
            Some((family, _)) => format!("presets/{family}.toml"),
            None => format!("presets/{alias}.toml"),
        },
    }
}

fn embedded_official_preset_content(path: &str) -> Option<&'static str> {
    builtin_base_preset_content_for_path(path)
}

fn official_family_manifest_path(family: PresetFamily) -> Option<&'static str> {
    match family {
        PresetFamily::Js => Some(EMBEDDED_JS_FAMILY_PRESETS_PATH),
        PresetFamily::Unknown => None,
    }
}

fn parse_family_manifest_preset_definitions(
    path: &str,
    content: &str,
) -> Result<Vec<FamilyPresetDefinition>, String> {
    let parsed: toml::Value = toml::from_str(content)
        .map_err(|e| format!("Failed to parse preset family manifest '{}': {e}", path))?;
    let manifest = parsed.as_table().ok_or_else(|| {
        format!(
            "Preset family manifest '{}' must be a TOML table with [preset-name] sections.",
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
        definitions.push(FamilyPresetDefinition {
            name: trimmed.to_string(),
            main,
        });
    }
    definitions.sort_by(|left, right| left.name.cmp(&right.name));
    definitions.dedup_by(|left, right| left.name == right.name);
    Ok(definitions)
}

fn parse_family_manifest_preset_names(path: &str, content: &str) -> Result<Vec<String>, String> {
    Ok(parse_family_manifest_preset_definitions(path, content)?
        .into_iter()
        .map(|definition| definition.name)
        .collect())
}

pub async fn load_available_family_preset_definitions(
    family: PresetFamily,
) -> Result<Vec<FamilyPresetDefinition>, String> {
    let Some(path) = official_family_manifest_path(family) else {
        return Err(format!(
            "Preset family '{}' is not supported for preset listing.",
            family.id()
        ));
    };

    let official_repo = official_preset_repo();
    let (_commit, content) = fetch_preset_content_from_master_branch(&official_repo, path).await?;
    parse_family_manifest_preset_definitions(path, &content)
}

pub async fn load_available_family_presets(family: PresetFamily) -> Result<Vec<String>, String> {
    let Some(path) = official_family_manifest_path(family) else {
        return Err(format!(
            "Preset family '{}' is not supported for preset listing.",
            family.id()
        ));
    };
    let official_repo = official_preset_repo();
    let (_commit, content) = fetch_preset_content_from_master_branch(&official_repo, path).await?;
    parse_family_manifest_preset_names(path, &content)
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
    if let Some((_family, preset_name)) = alias.split_once('/') {
        return parse_family_preset_content(path, content, preset_name);
    }
    parse_and_validate_preset(content, alias)
}

fn parse_family_preset_content(
    path: &str,
    content: &str,
    preset_name: &str,
) -> Result<BuildPreset, String> {
    let parsed: toml::Value = toml::from_str(content)
        .map_err(|e| format!("Failed to parse preset family manifest '{}': {e}", path))?;
    let manifest = parsed.as_table().ok_or_else(|| {
        format!(
            "Preset family manifest '{}' must be a TOML table with [preset-name] sections.",
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

fn embedded_content_hash(content: &str) -> String {
    use sha2::{Digest, Sha256};

    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    hex::encode(hasher.finalize())
}

pub fn parse_and_validate_preset(
    content: &str,
    inferred_name: &str,
) -> Result<BuildPreset, String> {
    let value: toml::Value =
        toml::from_str(content).map_err(|e| format!("Failed to parse build preset TOML: {e}"))?;
    if value.get("artifact").is_some() {
        return Err(
            "Build preset no longer supports [artifact]. Move exclude to [build].exclude and remove include."
                .to_string(),
        );
    }
    if value.get("include").is_some() {
        return Err(
            "Build preset no longer supports top-level include. Use app [build].include when needed."
                .to_string(),
        );
    }
    if value.get("targets").is_some() {
        return Err(
            "Build preset no longer supports top-level [targets]. Use [build].targets = [\"linux-x86_64-glibc\", ...].".to_string(),
        );
    }
    if value.get("exclude").is_some() {
        return Err(
            "Build preset no longer supports top-level exclude. Use [build].exclude.".to_string(),
        );
    }
    if value.get("assets").is_some() {
        return Err(
            "Build preset no longer supports top-level assets. Use [build].assets.".to_string(),
        );
    }
    if value.get("builder_image").is_some() {
        return Err(
            "Build preset no longer supports top-level `builder_image`. Builder image overrides are no longer supported."
                .to_string(),
        );
    }
    if value.get("runtime").is_some() {
        return Err(
            "Build preset no longer supports top-level `runtime`. Use top-level `name` (or omit it to use the preset file name)."
                .to_string(),
        );
    }
    if value.get("id").is_some() {
        return Err(
            "Build preset no longer supports top-level `id`. Use top-level `name`.".to_string(),
        );
    }
    if value.get("dev").and_then(toml::Value::as_table).is_some() {
        return Err("Build preset no longer supports [dev]. Use top-level `dev`.".to_string());
    }
    if value.get("development").is_some() {
        return Err(
            "Build preset no longer supports [development]. Use top-level `dev`.".to_string(),
        );
    }
    if value.get("deploy").is_some() {
        return Err(
            "Build preset no longer supports [deploy]. Use top-level `install` and `start`."
                .to_string(),
        );
    }
    if value.get("dev_cmd").is_some() {
        return Err(
            "Build preset no longer supports top-level `dev_cmd`. Use top-level `dev`.".to_string(),
        );
    }
    if value
        .get("build")
        .and_then(|build| build.get("builder_image"))
        .is_some()
    {
        return Err(
            "Build preset no longer supports [build].builder_image. Builder image overrides are no longer supported."
                .to_string(),
        );
    }
    if value
        .get("build")
        .and_then(|build| build.get("stages"))
        .is_some()
    {
        return Err(
            "Build preset does not support [build].stages. Define custom stages in app tako.toml under [[build.stages]].".to_string(),
        );
    }
    if value
        .get("build")
        .and_then(|build| build.get("docker"))
        .is_some()
    {
        return Err(
            "Build preset no longer supports [build].docker. Use [build].container.".to_string(),
        );
    }

    let build_table = value.get("build").and_then(toml::Value::as_table);
    let targets_explicit = build_table.and_then(|table| table.get("targets")).is_some();
    let container_explicit = build_table
        .and_then(|table| table.get("container"))
        .is_some();

    let raw: BuildPresetRaw = value
        .try_into()
        .map_err(|e| format!("Failed to parse build preset TOML: {e}"))?;

    let name = raw
        .name
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| inferred_name.to_string());
    if name.is_empty() {
        return Err(
            "Build preset name is empty. Set top-level `name` or use a .toml file name with a non-empty stem."
                .to_string(),
        );
    }

    let target_labels = parse_build_target_labels(raw.build.targets.clone())?;
    let use_container_build = resolve_container_build_toggle(raw.build.container, &target_labels)?;

    let preset = BuildPreset {
        name,
        main: raw.main,
        builder_image: None,
        build: BuildPresetBuild {
            exclude: raw.build.exclude,
            install: raw.build.install.clone(),
            build: raw.build.build.clone(),
            targets: target_labels,
            container: use_container_build,
            targets_explicit,
            container_explicit,
        },
        dev: raw.dev,
        install: raw.install,
        start: raw.start,
        targets: std::collections::HashMap::new(),
        target_defaults: BuildPresetTargetDefaults::default(),
        assets: raw.build.assets,
    };

    Ok(preset)
}

pub fn apply_adapter_base_runtime_defaults(
    preset: &mut BuildPreset,
    adapter: BuildAdapter,
) -> Result<(), String> {
    if adapter == BuildAdapter::Unknown {
        return Ok(());
    }

    let base_content = builtin_base_preset_content_for_alias(adapter.id()).ok_or_else(|| {
        format!(
            "Missing built-in base preset content for runtime '{}'.",
            adapter.id()
        )
    })?;
    let base_preset = parse_and_validate_preset(base_content, adapter.id())?;

    if preset.main.is_none() {
        preset.main = base_preset.main;
    }
    if preset.dev.is_empty() {
        preset.dev = base_preset.dev;
    }
    if preset.install.is_none() {
        preset.install = base_preset.install;
    }
    if preset.start.is_empty() {
        preset.start = base_preset.start;
    }
    let base_build = base_preset.build;
    preset.build.exclude = merge_string_lists_unique(
        base_build.exclude,
        std::mem::take(&mut preset.build.exclude),
    );
    if preset.build.install.is_none() {
        preset.build.install = base_build.install;
    }
    if preset.build.build.is_none() {
        preset.build.build = base_build.build;
    }
    if !preset.build.targets_explicit && preset.build.targets.is_empty() {
        preset.build.targets = base_build.targets;
    }
    if !preset.build.container_explicit && !preset.build.targets_explicit {
        preset.build.container = base_build.container;
    }

    Ok(())
}

fn merge_string_lists_unique(base: Vec<String>, extra: Vec<String>) -> Vec<String> {
    let mut merged = base;
    for item in extra {
        if !merged.contains(&item) {
            merged.push(item);
        }
    }
    merged
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
    let family_or_name = alias.split('/').next().unwrap_or(alias);
    BuildAdapter::from_id(family_or_name).unwrap_or(BuildAdapter::Unknown)
}

fn parse_build_target_labels(raw_targets: Option<toml::Value>) -> Result<Vec<String>, String> {
    let Some(raw_targets) = raw_targets else {
        return Ok(Vec::new());
    };
    let array = raw_targets.as_array().ok_or_else(|| {
        "Build preset [build].targets must be an array of target labels (for example [\"linux-x86_64-glibc\"]).".to_string()
    })?;
    let mut labels = Vec::new();
    for value in array {
        let Some(label) = value.as_str() else {
            return Err(
                "Build preset [build].targets entries must be strings (target labels).".to_string(),
            );
        };
        let trimmed = label.trim();
        if trimmed.is_empty() {
            return Err("Build preset [build].targets cannot contain empty values.".to_string());
        }
        labels.push(trimmed.to_string());
    }
    Ok(labels)
}

fn resolve_container_build_toggle(
    container: Option<bool>,
    target_labels: &[String],
) -> Result<bool, String> {
    if let Some(value) = container {
        return Ok(value);
    }
    Ok(!target_labels.is_empty())
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

    fn parse_preset(raw: &str) -> Result<BuildPreset, String> {
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
        let parsed = parse_preset_reference("js/tanstack-start").unwrap();
        assert_eq!(
            parsed,
            PresetReference::OfficialAlias {
                name: "js/tanstack-start".to_string(),
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
    fn official_alias_to_path_maps_family_layout() {
        assert_eq!(official_alias_to_path("bun"), "presets/bun/bun.toml");
        assert_eq!(
            official_alias_to_path("js/tanstack-start"),
            "presets/js.toml"
        );
        assert_eq!(official_alias_to_path("node"), "presets/node/node.toml");
        assert_eq!(official_alias_to_path("deno"), "presets/deno/deno.toml");
    }

    #[test]
    fn embedded_official_preset_content_supports_only_base_paths() {
        assert!(embedded_official_preset_content("presets/bun/bun.toml").is_some());
        assert!(embedded_official_preset_content("presets/node/node.toml").is_some());
        assert!(embedded_official_preset_content("presets/deno/deno.toml").is_some());
        assert!(embedded_official_preset_content("presets/js.toml").is_none());
    }

    #[test]
    fn parse_family_manifest_preset_names_collects_sorted_sections() {
        let names = parse_family_manifest_preset_names(
            "presets/js.toml",
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
    fn parse_family_manifest_preset_definitions_reads_optional_main() {
        let definitions = parse_family_manifest_preset_definitions(
            "presets/js.toml",
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
                FamilyPresetDefinition {
                    name: "no-main".to_string(),
                    main: None,
                },
                FamilyPresetDefinition {
                    name: "tanstack-start".to_string(),
                    main: Some("dist/server/tako-entry.mjs".to_string()),
                },
            ]
        );
    }

    #[test]
    fn load_available_family_presets_rejects_unknown_family() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let err = runtime
            .block_on(load_available_family_presets(PresetFamily::Unknown))
            .unwrap_err();
        assert!(err.contains("not supported"));
    }

    #[test]
    fn embedded_bun_preset_parses() {
        let content = builtin_base_preset_content_for_alias("bun").expect("embedded bun preset");
        let preset = parse_and_validate_preset(content, "bun").unwrap();
        assert_eq!(preset.name, "bun");
        assert!(!preset.dev.is_empty());
        assert!(preset.install.is_some());
        assert!(!preset.start.is_empty());
    }

    #[test]
    fn embedded_bun_tanstack_start_preset_parses() {
        let content = r#"
[tanstack-start]
main = "dist/server/tako-entry.mjs"

[tanstack-start.build]
assets = ["dist/client"]
"#;
        let preset =
            parse_official_alias_preset_content("js/tanstack-start", "presets/js.toml", content)
                .unwrap();
        assert_eq!(preset.name, "tanstack-start");
        assert_eq!(preset.main.as_deref(), Some("dist/server/tako-entry.mjs"));
        assert_eq!(preset.assets, vec!["dist/client"]);
    }

    #[test]
    fn infer_adapter_from_preset_reference_supports_official_aliases() {
        assert_eq!(
            infer_adapter_from_preset_reference("bun"),
            BuildAdapter::Bun
        );
        assert_eq!(
            infer_adapter_from_preset_reference("js/tanstack-start"),
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
    fn apply_adapter_base_runtime_defaults_fills_missing_fields_from_base_preset() {
        let raw = r#"
name = "tanstack-start"
main = "dist/server/tako-entry.mjs"

[build]
assets = ["dist/client"]
"#;
        let mut preset = parse_preset(raw).unwrap();
        apply_adapter_base_runtime_defaults(&mut preset, BuildAdapter::Bun).unwrap();

        assert_eq!(preset.main.as_deref(), Some("dist/server/tako-entry.mjs"));
        assert_eq!(preset.dev, vec!["bun", "run", "dev"]);
        assert!(preset.install.is_some());
        assert!(!preset.start.is_empty());
        assert_eq!(preset.build.exclude, vec!["node_modules/".to_string()]);
        assert!(preset.build.install.is_some());
        assert!(preset.build.build.is_some());
        assert_eq!(
            preset.build.targets,
            vec![
                "linux-x86_64-glibc".to_string(),
                "linux-aarch64-glibc".to_string(),
                "linux-x86_64-musl".to_string(),
                "linux-aarch64-musl".to_string(),
            ]
        );
        assert!(!preset.build.container);
        assert_eq!(preset.assets, vec!["dist/client".to_string()]);
    }

    #[test]
    fn apply_adapter_base_runtime_defaults_keeps_explicit_variant_overrides() {
        let raw = r#"
name = "custom-bun"
main = "custom-main.ts"
dev = ["bun", "run", "custom-dev"]
install = "custom-install"
start = ["bun", "run", "custom-start", "{main}"]

[build]
install = "custom-build-install"
build = "custom-build"
"#;
        let mut preset = parse_preset(raw).unwrap();
        apply_adapter_base_runtime_defaults(&mut preset, BuildAdapter::Bun).unwrap();

        assert_eq!(preset.main.as_deref(), Some("custom-main.ts"));
        assert_eq!(preset.dev, vec!["bun", "run", "custom-dev"]);
        assert_eq!(preset.install.as_deref(), Some("custom-install"));
        assert_eq!(preset.start, vec!["bun", "run", "custom-start", "{main}"]);
        assert_eq!(
            preset.build.install.as_deref(),
            Some("custom-build-install")
        );
        assert_eq!(preset.build.build.as_deref(), Some("custom-build"));
    }

    #[test]
    fn apply_adapter_base_runtime_defaults_uses_local_builds_for_node() {
        let raw = r#"
name = "custom-node"
"#;
        let mut preset = parse_preset(raw).unwrap();
        apply_adapter_base_runtime_defaults(&mut preset, BuildAdapter::Node).unwrap();

        assert!(!preset.build.container);
    }

    #[test]
    fn apply_adapter_base_runtime_defaults_uses_local_builds_for_deno() {
        let raw = r#"
name = "custom-deno"
"#;
        let mut preset = parse_preset(raw).unwrap();
        apply_adapter_base_runtime_defaults(&mut preset, BuildAdapter::Deno).unwrap();

        assert!(!preset.build.container);
    }

    #[test]
    fn apply_adapter_base_runtime_defaults_keeps_explicit_target_and_container_overrides() {
        let raw = r#"
name = "custom-bun"

[build]
assets = ["dist/client"]
exclude = []
targets = []
container = false
"#;
        let mut preset = parse_preset(raw).unwrap();
        apply_adapter_base_runtime_defaults(&mut preset, BuildAdapter::Bun).unwrap();

        assert_eq!(preset.build.exclude, vec!["node_modules/".to_string()]);
        assert!(preset.build.targets.is_empty());
        assert!(!preset.build.container);
    }

    #[test]
    fn apply_adapter_base_runtime_defaults_appends_variant_excludes_to_base() {
        let raw = r#"
name = "custom-bun"

[build]
assets = ["dist/client"]
exclude = ["dist/**/*.map", "node_modules/"]
"#;
        let mut preset = parse_preset(raw).unwrap();
        apply_adapter_base_runtime_defaults(&mut preset, BuildAdapter::Bun).unwrap();

        assert_eq!(
            preset.build.exclude,
            vec!["node_modules/".to_string(), "dist/**/*.map".to_string()]
        );
    }

    #[test]
    fn parse_and_validate_preset_accepts_toml() {
        let raw = r#"
name = "bun"
main = "index.ts"

dev = ["bun", "run", "dev"]
install = "bun install --production --frozen-lockfile"
start = ["bun", "run", "node_modules/tako.sh/src/wrapper.ts", "{main}"]

[build]
exclude = ["**/*.map"]
install = "bun install"
build = "bun run build"
targets = ["linux-x86_64-glibc", "linux-aarch64-musl"]
"#;
        let preset = parse_preset(raw).unwrap();
        assert_eq!(preset.name, "bun");
        assert_eq!(preset.main.as_deref(), Some("index.ts"));
        assert_eq!(preset.builder_image.as_deref(), None);
        assert_eq!(preset.build.exclude, vec!["**/*.map".to_string()]);
        assert_eq!(
            preset.build.targets,
            vec![
                "linux-x86_64-glibc".to_string(),
                "linux-aarch64-musl".to_string()
            ]
        );
        assert_eq!(preset.dev, vec!["bun", "run", "dev"]);
        assert_eq!(
            preset.start,
            vec![
                "bun",
                "run",
                "node_modules/tako.sh/src/wrapper.ts",
                "{main}"
            ]
        );
        assert!(preset.targets.is_empty());
    }

    #[test]
    fn parse_and_validate_preset_uses_inferred_name_when_missing() {
        let raw = r#"
[build]
install = "bun install"
"#;
        let preset = parse_preset(raw).unwrap();
        assert_eq!(preset.name, "bun");
    }

    #[test]
    fn parse_and_validate_preset_rejects_top_level_targets() {
        let raw = r#"
name = "bun"

        [targets]
        builder_image = "oven/bun:1.2"
        "#;
        let err = parse_preset(raw).unwrap_err();
        assert!(err.contains("Use [build].targets"));
    }

    #[test]
    fn parse_and_validate_preset_rejects_top_level_exclude() {
        let raw = r#"
name = "bun"
exclude = ["**/*.map"]

[build]
install = "bun install"
"#;
        let err = parse_preset(raw).unwrap_err();
        assert!(err.contains("top-level exclude"));
    }

    #[test]
    fn parse_and_validate_preset_rejects_top_level_assets() {
        let raw = r#"
name = "bun"
assets = ["dist/client"]

[build]
install = "bun install"
"#;
        let err = parse_preset(raw).unwrap_err();
        assert!(err.contains("top-level assets"));
    }

    #[test]
    fn parse_and_validate_preset_rejects_top_level_builder_image() {
        let raw = r#"
name = "bun"
builder_image = "oven/bun:1.2"

[build]
install = "bun install"
"#;
        let err = parse_preset(raw).unwrap_err();
        assert!(err.contains("top-level `builder_image`"));
    }

    #[test]
    fn parse_and_validate_preset_rejects_top_level_runtime() {
        let raw = r#"
runtime = "bun"

[build]
install = "bun install"
"#;
        let err = parse_preset(raw).unwrap_err();
        assert!(err.contains("top-level `runtime`"));
    }

    #[test]
    fn parse_and_validate_preset_rejects_top_level_id() {
        let raw = r#"
id = "bun"

[build]
install = "bun install"
"#;
        let err = parse_preset(raw).unwrap_err();
        assert!(err.contains("top-level `id`"));
    }

    #[test]
    fn parse_and_validate_preset_rejects_legacy_dev_cmd() {
        let raw = r#"
name = "bun"

dev_cmd = ["bun", "run", "dev"]

[build]
install = "bun install"
"#;
        let err = parse_preset(raw).unwrap_err();
        assert!(err.contains("top-level `dev_cmd`"));
    }

    #[test]
    fn parse_and_validate_preset_rejects_legacy_dev_table() {
        let raw = r#"
name = "bun"

[build]
install = "bun install"

[dev]
start = ["bun", "run", "dev"]
"#;
        let err = parse_preset(raw).unwrap_err();
        assert!(err.contains("no longer supports [dev]"));
    }

    #[test]
    fn parse_and_validate_preset_rejects_legacy_deploy_table() {
        let raw = r#"
name = "bun"

[build]
install = "bun install"

[deploy]
install = "bun install --production --frozen-lockfile"
start = ["bun", "run", "index.ts"]
"#;
        let err = parse_preset(raw).unwrap_err();
        assert!(err.contains("no longer supports [deploy]"));
    }

    #[test]
    fn parse_and_validate_preset_accepts_build_targets_array() {
        let raw = r#"
name = "bun"

[build]
install = "bun install --frozen-lockfile"
build = "bun run --if-present build"
targets = ["linux-x86_64-glibc", "linux-aarch64-musl"]
"#;
        let preset = parse_preset(raw).unwrap();
        assert_eq!(
            preset.build.targets,
            vec![
                "linux-x86_64-glibc".to_string(),
                "linux-aarch64-musl".to_string()
            ]
        );
    }

    #[test]
    fn parse_and_validate_preset_accepts_build_assets() {
        let raw = r#"
name = "tanstack-start"

[build]
assets = ["dist/client"]
"#;
        let preset = parse_preset(raw).unwrap();
        assert_eq!(preset.assets, vec!["dist/client".to_string()]);
    }

    #[test]
    fn parse_and_validate_preset_accepts_build_container_toggle() {
        let raw = r#"
name = "bun"

[build]
container = true
install = "bun install --frozen-lockfile"
build = "bun run --if-present build"
targets = ["linux-x86_64-glibc"]
"#;
        let preset = parse_preset(raw).unwrap();
        assert!(preset.build.container);
    }

    #[test]
    fn parse_and_validate_preset_rejects_legacy_build_docker_toggle() {
        let raw = r#"
name = "bun"

[build]
docker = true
install = "bun install --frozen-lockfile"
build = "bun run --if-present build"
targets = ["linux-x86_64-glibc"]
"#;
        let err = parse_preset(raw).unwrap_err();
        assert!(err.contains("[build].docker"));
    }

    #[test]
    fn parse_and_validate_preset_rejects_legacy_build_targets_table() {
        let raw = r#"
name = "bun"

[build]
install = "bun install"
build = "bun run build"

[build.targets]
builder_image = "oven/bun:1.2"
"#;
        let err = parse_preset(raw).unwrap_err();
        assert!(err.contains("[build].targets must be an array"));
    }

    #[test]
    fn parse_and_validate_preset_rejects_build_stages() {
        let raw = r#"
name = "bun"

[build]
install = "bun install"
build = "bun run build"

[[build.stages]]
run = "bun run build"
"#;
        let err = parse_preset(raw).unwrap_err();
        assert!(err.contains("does not support [build].stages"));
    }

    #[test]
    fn parse_and_validate_preset_supports_local_build_without_builder_image() {
        let raw = r#"
name = "bun"
[build]
install = "bun install --frozen-lockfile"
build = "bun run --if-present build"
"#;
        let preset = parse_preset(raw).unwrap();
        assert!(preset.targets.is_empty());
        assert_eq!(preset.build.targets, Vec::<String>::new());
        assert_eq!(
            preset.build.install.as_deref(),
            Some("bun install --frozen-lockfile")
        );
        assert_eq!(
            preset.build.build.as_deref(),
            Some("bun run --if-present build")
        );
    }

    #[test]
    fn parse_and_validate_preset_rejects_legacy_artifact_table() {
        let raw = r#"
[artifact]
include = ["**/*"]
"#;
        let err = parse_preset(raw).unwrap_err();
        assert!(err.contains("no longer supports [artifact]"));
    }

    #[test]
    fn parse_and_validate_preset_rejects_build_targets_default_table() {
        let raw = r#"
name = "bun"

[build]
install = "bun install"

[build.targets.default]
builder_image = "oven/bun:1.2"
"#;
        let err = parse_preset(raw).unwrap_err();
        assert!(err.contains("[build].targets must be an array"));
    }

    #[test]
    fn parse_and_validate_preset_rejects_top_level_include() {
        let raw = r#"
name = "bun"
include = ["dist/**"]

[build]
install = "bun install"
"#;
        let err = parse_preset(raw).unwrap_err();
        assert!(err.contains("top-level include"));
    }

    #[test]
    fn lock_round_trip_writes_and_reads_build_lock_file() {
        let temp = tempfile::TempDir::new().unwrap();
        let resolved = ResolvedPresetSource {
            preset_ref: "bun".to_string(),
            repo: "tako-sh/presets".to_string(),
            path: "presets/bun/bun.toml".to_string(),
            commit: "abc123".to_string(),
        };
        write_locked_preset(temp.path(), &resolved).unwrap();
        let loaded = read_locked_preset(temp.path()).unwrap().unwrap();
        assert_eq!(loaded, resolved);
        assert!(lock_file_path(temp.path()).exists());
    }

    #[test]
    fn load_build_preset_ignores_locked_commit_for_unpinned_alias() {
        let temp = tempfile::TempDir::new().unwrap();
        let locked = ResolvedPresetSource {
            preset_ref: "bun".to_string(),
            repo: "tako-sh/presets".to_string(),
            path: "presets/bun/bun.toml".to_string(),
            commit: "0000000000000000000000000000000000000000".to_string(),
        };
        write_locked_preset(temp.path(), &locked).unwrap();

        let runtime = tokio::runtime::Runtime::new().unwrap();
        let (_preset, resolved) = runtime
            .block_on(load_build_preset(temp.path(), "bun"))
            .unwrap();

        assert_eq!(resolved.preset_ref, "bun");
        assert_ne!(resolved.commit, locked.commit);
    }

    #[test]
    fn fetch_preset_content_from_master_branch_returns_generic_fetch_error() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let err = runtime
            .block_on(fetch_preset_content_from_master_branch(
                "invalid-repo-slug",
                "presets/js.toml",
            ))
            .unwrap_err();
        assert_eq!(err, "Failed to fetch preset");
    }
}

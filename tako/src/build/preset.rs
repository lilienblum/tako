use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::build::adapter::{BuildAdapter, PresetGroup};
use crate::build::preset_cache;

const FALLBACK_OFFICIAL_PRESET_REPO: &str = "tako-sh/presets";
const PACKAGE_REPOSITORY_URL: &str = env!("CARGO_PKG_REPOSITORY");
const OFFICIAL_PRESET_BRANCH: &str = "master";
const OFFICIAL_JS_GROUP_PRESETS_PATH: &str = "presets/javascript.toml";
const OFFICIAL_GO_GROUP_PRESETS_PATH: &str = "presets/go.toml";
const EMBEDDED_JS_GROUP_PRESETS: &str = include_str!("../../../presets/javascript.toml");
const EMBEDDED_GO_GROUP_PRESETS: &str = include_str!("../../../presets/go.toml");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PresetResolveMode {
    Deploy,
    Dev,
}

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
/// Loaded from `presets/<group>.toml` (fetched from GitHub, cached locally).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AppPreset {
    pub name: String,
    #[serde(default)]
    pub main: Option<String>,
    #[serde(default)]
    pub assets: Vec<String>,
    /// Custom dev command (overrides runtime default in `tako dev`).
    #[serde(default)]
    pub dev: Vec<String>,
}

/// Backward-compatible alias.
pub type BuildPreset = AppPreset;

const KNOWN_PRESET_FIELDS: &[&str] = &["name", "main", "assets", "dev"];

#[derive(Debug, Clone, Deserialize)]
struct AppPresetRaw {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    main: Option<String>,
    #[serde(default)]
    assets: Vec<String>,
    #[serde(default)]
    dev: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResolvedPresetSource {
    pub preset_ref: String,
    pub repo: String,
    pub path: String,
    pub commit: String,
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
    load_build_preset_with_mode(project_dir, preset_ref, PresetResolveMode::Deploy).await
}

pub async fn load_dev_build_preset(
    project_dir: &Path,
    preset_ref: &str,
) -> Result<(BuildPreset, ResolvedPresetSource), String> {
    load_build_preset_with_mode(project_dir, preset_ref, PresetResolveMode::Dev).await
}

async fn load_build_preset_with_mode(
    project_dir: &Path,
    preset_ref: &str,
    mode: PresetResolveMode,
) -> Result<(BuildPreset, ResolvedPresetSource), String> {
    let parsed_ref = parse_preset_reference(preset_ref)?;

    let (alias, commit_override) = match &parsed_ref {
        PresetReference::OfficialAlias { name, commit } => (name.as_str(), commit.clone()),
    };
    let path = official_alias_to_path(alias);
    let official_repo = official_preset_repo();

    let (repo, commit, content) = if let Some(commit) = commit_override {
        resolve_by_commit(&official_repo, &path, &commit).await?
    } else {
        resolve_by_branch(&official_repo, &path, OFFICIAL_PRESET_BRANCH, mode).await?
    };

    let preset = parse_resolved_preset_from_content(&parsed_ref, &path, &content)?;
    let resolved = ResolvedPresetSource {
        preset_ref: preset_ref.to_string(),
        repo,
        path,
        commit: commit.clone(),
    };
    remove_legacy_build_lock(project_dir);
    Ok((preset, resolved))
}

async fn resolve_by_commit(
    repo: &str,
    path: &str,
    commit: &str,
) -> Result<(String, String, String), String> {
    // Try cache first
    if let Some(content) = preset_cache::read_cached(repo, commit, path) {
        return Ok((repo.to_string(), commit.to_string(), content));
    }

    // Fetch from GitHub
    match fetch_preset_content_by_commit(repo, path, commit).await {
        Ok(content) => {
            let _ = preset_cache::write_cached(repo, commit, path, &content);
            Ok((repo.to_string(), commit.to_string(), content))
        }
        Err(fetch_err) => {
            // Stale fallback: any cached version of this path
            if let Some((_stale_sha, content)) = preset_cache::find_any_cached(repo, path) {
                tracing::warn!(
                    "Preset fetch failed for commit {}, using stale cache: {}",
                    commit,
                    fetch_err
                );
                Ok((repo.to_string(), commit.to_string(), content))
            } else {
                Err(fetch_err)
            }
        }
    }
}

async fn resolve_by_branch(
    repo: &str,
    path: &str,
    branch: &str,
    mode: PresetResolveMode,
) -> Result<(String, String, String), String> {
    // Check freshness — if TTL hasn't expired, use cached content
    if let Some(sha) = preset_cache::fresh_sha(repo, branch)
        && let Some(content) = preset_cache::read_cached(repo, &sha, path)
    {
        return Ok((repo.to_string(), sha, content));
    }

    if mode == PresetResolveMode::Dev {
        if let Some(sha) = preset_cache::last_known_sha(repo, branch)
            && let Some(content) = preset_cache::read_cached(repo, &sha, path)
        {
            return Ok((repo.to_string(), sha, content));
        }
        if let Some((sha, content)) = preset_cache::find_any_cached(repo, path) {
            return Ok((repo.to_string(), sha, content));
        }
        if let Some(content) = embedded_group_manifest_content(path) {
            return Ok((
                repo.to_string(),
                "embedded".to_string(),
                content.to_string(),
            ));
        }
    }

    // Fetch from GitHub to resolve current SHA
    match fetch_preset_content_from_master_branch(repo, path).await {
        Ok((sha, content)) => {
            let _ = preset_cache::write_cached(repo, &sha, path, &content);
            let _ = preset_cache::update_freshness(repo, branch, &sha);
            Ok((repo.to_string(), sha, content))
        }
        Err(fetch_err) => {
            // Stale fallback: last known SHA for this branch
            if let Some(sha) = preset_cache::last_known_sha(repo, branch)
                && let Some(content) = preset_cache::read_cached(repo, &sha, path)
            {
                tracing::warn!("Preset fetch failed, using stale cache: {}", fetch_err);
                return Ok((repo.to_string(), sha, content));
            }
            // Any cached version at all
            if let Some((sha, content)) = preset_cache::find_any_cached(repo, path) {
                tracing::warn!("Preset fetch failed, using stale cache: {}", fetch_err);
                return Ok((repo.to_string(), sha, content));
            }
            if let Some(content) = embedded_group_manifest_content(path) {
                tracing::warn!(
                    "Preset fetch failed, using embedded group manifest for {}: {}",
                    path,
                    fetch_err
                );
                return Ok((
                    repo.to_string(),
                    "embedded".to_string(),
                    content.to_string(),
                ));
            }
            Err(fetch_err)
        }
    }
}

fn official_alias_to_path(alias: &str) -> String {
    match alias.split_once('/') {
        Some((group, _)) => format!("presets/{group}.toml"),
        None => {
            if let Some(adapter) = BuildAdapter::from_id(alias) {
                let group = adapter.preset_group().id();
                format!("presets/{group}.toml")
            } else {
                format!("presets/{alias}.toml")
            }
        }
    }
}

fn official_group_manifest_path(group: PresetGroup) -> Option<&'static str> {
    match group {
        PresetGroup::Js => Some(OFFICIAL_JS_GROUP_PRESETS_PATH),
        PresetGroup::Go => Some(OFFICIAL_GO_GROUP_PRESETS_PATH),
        PresetGroup::Unknown => None,
    }
}

fn embedded_group_manifest_content(path: &str) -> Option<&'static str> {
    match path {
        OFFICIAL_JS_GROUP_PRESETS_PATH => Some(EMBEDDED_JS_GROUP_PRESETS),
        OFFICIAL_GO_GROUP_PRESETS_PATH => Some(EMBEDDED_GO_GROUP_PRESETS),
        _ => None,
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
    let (_repo, _commit, content) = resolve_by_branch(
        &official_repo,
        path,
        OFFICIAL_PRESET_BRANCH,
        PresetResolveMode::Deploy,
    )
    .await?;
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
    let (_repo, _commit, content) = resolve_by_branch(
        &official_repo,
        path,
        OFFICIAL_PRESET_BRANCH,
        PresetResolveMode::Deploy,
    )
    .await?;
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
        // Base runtime presets (bun, node, deno, go) may not have a section in
        // the presets file — they use runtime defaults instead.
        return match parse_group_preset_content(path, content, alias) {
            Ok(preset) => Ok(preset),
            Err(e) if e.contains("was not found") => Ok(BuildPreset {
                name: alias.to_string(),
                ..Default::default()
            }),
            Err(e) => Err(e),
        };
    }
    parse_and_validate_preset(content, alias)
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

pub fn parse_and_validate_preset(content: &str, inferred_name: &str) -> Result<AppPreset, String> {
    // Warn on unknown fields (legacy preset fields like dev, build, install, start).
    if let Ok(value) = toml::from_str::<toml::Value>(content)
        && let Some(table) = value.as_table()
    {
        for key in table.keys() {
            if !KNOWN_PRESET_FIELDS.contains(&key.as_str()) {
                tracing::warn!(
                    "Preset has unknown field '{}' — only name, main, assets are supported",
                    key
                );
            }
        }
    }

    let raw: AppPresetRaw =
        toml::from_str(content).map_err(|e| format!("Failed to parse preset TOML: {e}"))?;

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
        dev: raw.dev,
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

fn remove_legacy_build_lock(project_dir: &Path) {
    let lock_path = project_dir.join(".tako/build.lock.json");
    if !lock_path.exists() {
        return;
    }
    if let Err(error) = fs::remove_file(&lock_path) {
        tracing::warn!(
            "Failed to remove legacy preset lock file {}: {}",
            lock_path.display(),
            error
        );
    }
}

fn apply_github_auth(builder: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
    let token = std::env::var("GITHUB_TOKEN")
        .or_else(|_| std::env::var("GH_TOKEN"))
        .ok();
    match token {
        Some(t) => builder.header("Authorization", format!("Bearer {t}")),
        None => builder,
    }
}

async fn fetch_preset_content_by_commit(
    repo: &str,
    path: &str,
    commit: &str,
) -> Result<String, String> {
    let url = format!("https://raw.githubusercontent.com/{repo}/{commit}/{path}");
    let client = reqwest::Client::new();
    let response = apply_github_auth(client.get(url).header("User-Agent", "tako-cli"))
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
    let commit = fetch_github_branch_commit(repo, OFFICIAL_PRESET_BRANCH).await?;
    let content = fetch_preset_content_by_commit(repo, path, &commit).await?;
    Ok((commit, content))
}

async fn fetch_github_branch_commit(repo: &str, branch: &str) -> Result<String, String> {
    let Some((owner, repository)) = repo.split_once('/') else {
        return Err("Failed to fetch preset".to_string());
    };
    let url = format!("https://api.github.com/repos/{owner}/{repository}/git/ref/heads/{branch}");
    let client = reqwest::Client::new();
    let response = apply_github_auth(client.get(url).header("User-Agent", "tako-cli"))
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
    parse_github_branch_commit_sha(&raw)
}

fn parse_github_branch_commit_sha(raw: &str) -> Result<String, String> {
    let json: serde_json::Value =
        serde_json::from_str(raw).map_err(|_e| "Failed to fetch preset".to_string())?;
    let object = json
        .get("object")
        .and_then(|value| value.as_object())
        .ok_or_else(|| "Failed to fetch preset".to_string())?;
    if object.get("type").and_then(|value| value.as_str()) != Some("commit") {
        return Err("Failed to fetch preset".to_string());
    }
    object
        .get("sha")
        .and_then(|value| value.as_str())
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| "Failed to fetch preset".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(official_alias_to_path("bun"), "presets/javascript.toml");
        assert_eq!(
            official_alias_to_path("javascript/tanstack-start"),
            "presets/javascript.toml"
        );
        assert_eq!(official_alias_to_path("node"), "presets/javascript.toml");
        assert_eq!(official_alias_to_path("deno"), "presets/javascript.toml");
        assert_eq!(official_alias_to_path("go"), "presets/go.toml");
    }

    #[test]
    fn official_group_manifest_path_supports_known_families() {
        assert_eq!(
            official_group_manifest_path(PresetGroup::Js),
            Some("presets/javascript.toml")
        );
        assert_eq!(
            official_group_manifest_path(PresetGroup::Go),
            Some("presets/go.toml")
        );
        assert_eq!(official_group_manifest_path(PresetGroup::Unknown), None);
    }

    #[test]
    fn embedded_group_manifest_content_supports_known_group_paths() {
        assert!(embedded_group_manifest_content("presets/javascript.toml").is_some());
        assert!(embedded_group_manifest_content("presets/go.toml").is_some());
        assert!(embedded_group_manifest_content("presets/unknown.toml").is_none());
    }

    #[test]
    fn embedded_javascript_group_manifest_includes_nextjs() {
        let preset = parse_official_alias_preset_content(
            "javascript/nextjs",
            "presets/javascript.toml",
            embedded_group_manifest_content("presets/javascript.toml")
                .expect("embedded javascript manifest should exist"),
        )
        .expect("embedded javascript manifest should parse nextjs preset");
        assert_eq!(preset.name, "nextjs");
        assert_eq!(preset.main.as_deref(), Some(".next/tako-entry.mjs"));
        assert_eq!(preset.dev, vec!["next", "dev"]);
    }

    #[test]
    fn fetched_group_manifest_missing_preset_still_errors() {
        let fetched_content = r#"
[vite]
dev = ["vite", "dev"]
"#;

        let error = parse_resolved_preset_from_content(
            &PresetReference::OfficialAlias {
                name: "javascript/nextjs".to_string(),
                commit: None,
            },
            "presets/javascript.toml",
            fetched_content,
        )
        .expect_err("fetched manifest should not contain nextjs");
        assert!(error.contains("Preset 'nextjs' was not found"));
    }

    #[test]
    fn parse_group_manifest_preset_names_collects_sorted_sections() {
        let names = parse_group_manifest_preset_names(
            "presets/javascript.toml",
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
            "presets/javascript.toml",
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
    fn tanstack_start_preset_parses_from_group_manifest() {
        let content = r#"
[tanstack-start]
main = "dist/server/tako-entry.mjs"
assets = ["dist/client"]
"#;
        let preset = parse_official_alias_preset_content(
            "javascript/tanstack-start",
            "presets/javascript.toml",
            content,
        )
        .unwrap();
        assert_eq!(preset.name, "tanstack-start");
        assert_eq!(preset.main.as_deref(), Some("dist/server/tako-entry.mjs"));
        assert_eq!(preset.assets, vec!["dist/client"]);
    }

    #[test]
    fn nextjs_preset_parses_from_group_manifest() {
        let content = r#"
[nextjs]
main = ".next/tako-entry.mjs"
dev = ["next", "dev"]
"#;
        let preset = parse_official_alias_preset_content(
            "javascript/nextjs",
            "presets/javascript.toml",
            content,
        )
        .unwrap();
        assert_eq!(preset.name, "nextjs");
        assert_eq!(preset.main.as_deref(), Some(".next/tako-entry.mjs"));
        assert_eq!(preset.dev, vec!["next", "dev"]);
    }

    #[test]
    fn runtime_alias_returns_empty_preset_when_missing_from_manifest() {
        let content = r#"
[tanstack-start]
main = "dist/server/tako-entry.mjs"
"#;
        let preset = parse_official_alias_preset_content("bun", "presets/javascript.toml", content)
            .expect("base runtime preset should fall back to empty defaults");
        assert_eq!(preset.name, "bun");
        assert!(preset.main.is_none());
    }

    #[test]
    fn parse_official_alias_preset_content_rejects_missing_non_runtime_group_alias() {
        let content = r#"
[tanstack-start]
main = "dist/server/tako-entry.mjs"
"#;
        let err = parse_official_alias_preset_content(
            "javascript/missing",
            "presets/javascript.toml",
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
    fn fetch_preset_content_from_master_branch_returns_generic_fetch_error() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let err = runtime
            .block_on(fetch_preset_content_from_master_branch(
                "invalid-repo-slug",
                "presets/javascript.toml",
            ))
            .unwrap_err();
        assert_eq!(err, "Failed to fetch preset");
    }

    #[test]
    fn resolve_by_branch_uses_stale_cache_immediately_in_dev_mode() {
        let _lock = crate::paths::test_tako_home_env_lock();
        let previous = std::env::var_os("TAKO_HOME");
        let home = tempfile::TempDir::new().unwrap();
        unsafe {
            std::env::set_var("TAKO_HOME", home.path());
        }

        let repo = "invalid-repo-slug";
        let path = "presets/javascript.toml";
        let sha = "abc1234567890";
        let manifest = r#"
[nextjs]
main = ".next/tako-entry.mjs"
dev = ["next", "dev"]
"#;
        crate::build::preset_cache::write_cached(repo, sha, path, manifest).unwrap();

        let repo_dir = crate::paths::tako_cache_dir()
            .unwrap()
            .join("presets")
            .join(repo.replace('/', "__"));
        fs::create_dir_all(&repo_dir).unwrap();
        fs::write(
            repo_dir.join("_meta.json"),
            format!(
                r#"{{
  "branches": {{
    "{branch}": {{
      "sha": "{sha}",
      "last_checked": 0
    }}
  }}
}}"#,
                branch = OFFICIAL_PRESET_BRANCH,
                sha = sha
            ),
        )
        .unwrap();

        let runtime = tokio::runtime::Runtime::new().unwrap();
        let resolved = runtime.block_on(resolve_by_branch(
            repo,
            path,
            OFFICIAL_PRESET_BRANCH,
            PresetResolveMode::Dev,
        ));

        match previous {
            Some(value) => unsafe { std::env::set_var("TAKO_HOME", value) },
            None => unsafe { std::env::remove_var("TAKO_HOME") },
        }

        let (resolved_repo, resolved_sha, content) = resolved.unwrap();
        assert_eq!(resolved_repo, repo);
        assert_eq!(resolved_sha, sha);
        assert_eq!(content, manifest);
    }

    #[test]
    fn parse_github_branch_commit_sha_extracts_commit_sha() {
        let sha = parse_github_branch_commit_sha(
            r#"{
  "ref": "refs/heads/master",
  "object": {
    "sha": "d0ff9bec5b3d42a874b1bff544249b3a4c530d9f",
    "type": "commit"
  }
}"#,
        )
        .unwrap();
        assert_eq!(sha, "d0ff9bec5b3d42a874b1bff544249b3a4c530d9f");
    }

    #[test]
    fn parse_github_branch_commit_sha_rejects_non_commit_objects() {
        let err = parse_github_branch_commit_sha(
            r#"{
  "ref": "refs/heads/master",
  "object": {
    "sha": "eb9c0c1dd0b123ce72c29397826966d831617d0a",
    "type": "blob"
  }
}"#,
        )
        .unwrap_err();
        assert_eq!(err, "Failed to fetch preset");
    }

    #[test]
    fn load_build_preset_ignores_and_removes_legacy_build_lock() {
        let _lock = crate::paths::test_tako_home_env_lock();
        let previous = std::env::var_os("TAKO_HOME");
        let home = tempfile::TempDir::new().unwrap();
        let project = tempfile::TempDir::new().unwrap();
        unsafe {
            std::env::set_var("TAKO_HOME", home.path());
        }

        let repo = official_preset_repo();
        let path = "presets/javascript.toml";
        let branch_sha = "d0ff9bec5b3d42a874b1bff544249b3a4c530d9f";
        let manifest = r#"
[nextjs]
main = ".next/tako-entry.mjs"
dev = ["next", "dev"]
"#;
        crate::build::preset_cache::write_cached(&repo, branch_sha, path, manifest).unwrap();
        crate::build::preset_cache::update_freshness(&repo, OFFICIAL_PRESET_BRANCH, branch_sha)
            .unwrap();

        let lock_path = project.path().join(".tako/build.lock.json");
        fs::create_dir_all(lock_path.parent().unwrap()).unwrap();
        fs::write(
            &lock_path,
            r#"{
  "schema_version": 1,
  "preset": {
    "preset_ref": "javascript/nextjs",
    "repo": "lilienblum/tako",
    "path": "presets/javascript.toml",
    "commit": "eb9c0c1dd0b123ce72c29397826966d831617d0a"
  }
}"#,
        )
        .unwrap();

        let runtime = tokio::runtime::Runtime::new().unwrap();
        let (preset, resolved) = runtime
            .block_on(load_build_preset(project.path(), "javascript/nextjs"))
            .unwrap();

        match previous {
            Some(value) => unsafe { std::env::set_var("TAKO_HOME", value) },
            None => unsafe { std::env::remove_var("TAKO_HOME") },
        }

        assert_eq!(preset.name, "nextjs");
        assert_eq!(resolved.commit, branch_sha);
        assert!(!lock_path.exists());
    }
}

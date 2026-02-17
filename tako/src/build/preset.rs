use std::fs;
use std::path::{Path, PathBuf};

use base64::Engine;
use serde::{Deserialize, Serialize};

pub const BUILD_LOCK_RELATIVE_PATH: &str = ".tako/build.lock.json";
const OFFICIAL_PRESET_REPO: &str = "tako-sh/presets";
const EMBEDDED_PRESET_REPO: &str = "embedded";
const EMBEDDED_BUN_PRESET_PATH: &str = "presets/bun.toml";
const EMBEDDED_BUN_PRESET_CONTENT: &str = include_str!("../../../build-presets/bun.toml");

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PresetReference {
    OfficialAlias {
        name: String,
        commit: Option<String>,
    },
    Github {
        repo: String,
        path: String,
        commit: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BuildPreset {
    #[serde(default)]
    pub targets: std::collections::HashMap<String, BuildPresetTarget>,
    #[serde(default)]
    pub target_defaults: BuildPresetTargetDefaults,
    #[serde(default)]
    pub exclude: Vec<String>,
    #[serde(default)]
    pub assets: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BuildPresetTarget {
    pub builder_image: String,
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
    targets: BuildPresetRawTargets,
    #[serde(default)]
    exclude: Vec<String>,
    #[serde(default)]
    assets: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct BuildPresetRawTargets {
    #[serde(default)]
    builder_image: Option<String>,
    #[serde(default)]
    install: Option<String>,
    #[serde(default)]
    build: Option<String>,
    #[serde(flatten)]
    entries: std::collections::HashMap<String, BuildPresetRawTarget>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct BuildPresetRawTarget {
    #[serde(default)]
    builder_image: Option<String>,
    #[serde(default)]
    install: Option<String>,
    #[serde(default)]
    build: Option<String>,
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
        return Err("build.preset cannot be empty".to_string());
    }

    if let Some(rest) = trimmed.strip_prefix("github:") {
        return parse_github_reference(trimmed, rest);
    }

    if trimmed.contains('@') || trimmed.contains(':') {
        return Err(format!(
            "Invalid preset reference '{}'. Use an official alias like 'bun' or 'bun/<commit-hash>', or github:<owner>/<repo>/<path>.toml[@<commit-hash>].",
            trimmed
        ));
    }

    let (name, commit) = match trimmed.split_once('/') {
        Some((name, commit)) => {
            if name.is_empty() || commit.is_empty() || commit.contains('/') {
                return Err(format!(
                    "Invalid preset alias '{}'. Expected '<name>' or '<name>/<commit-hash>'.",
                    trimmed
                ));
            }
            validate_commit_hash(trimmed, commit)?;
            (name.to_string(), Some(commit.to_string()))
        }
        None => (trimmed.to_string(), None),
    };

    if !name
        .chars()
        .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-' || ch == '_')
    {
        return Err(format!(
            "Invalid preset alias '{}'. Alias must use lowercase letters, digits, '-' or '_'.",
            trimmed
        ));
    }

    Ok(PresetReference::OfficialAlias { name, commit })
}

fn parse_github_reference(raw_value: &str, rest: &str) -> Result<PresetReference, String> {
    let (location, commit) = match rest.rsplit_once('@') {
        Some((lhs, rhs)) => (lhs, Some(rhs.trim().to_string())),
        None => (rest, None),
    };
    if let Some(commit) = &commit
        && commit.is_empty()
    {
        return Err(format!(
            "Invalid preset reference '{}': commit hash cannot be empty after '@'.",
            raw_value
        ));
    }
    if let Some(commit) = &commit {
        validate_commit_hash(raw_value, commit)?;
    }

    let mut parts = location.split('/');
    let owner = parts.next().unwrap_or_default();
    let repo = parts.next().unwrap_or_default();
    let path_parts: Vec<&str> = parts.collect();

    if owner.is_empty() || repo.is_empty() || path_parts.is_empty() {
        return Err(format!(
            "Invalid preset reference '{}'. Expected github:<owner>/<repo>/<path>.toml[@<commit>].",
            raw_value
        ));
    }
    let path = path_parts.join("/");
    if !path.ends_with(".toml") {
        return Err(format!(
            "Invalid preset reference '{}': path must end with .toml.",
            raw_value
        ));
    }

    Ok(PresetReference::Github {
        repo: format!("{owner}/{repo}"),
        path,
        commit,
    })
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

pub async fn load_build_preset(
    project_dir: &Path,
    preset_ref: &str,
) -> Result<(BuildPreset, ResolvedPresetSource), String> {
    let parsed_ref = parse_preset_reference(preset_ref)?;

    if let Some(locked) = read_locked_preset(project_dir)?
        && locked.preset_ref == preset_ref
    {
        let content = load_content_for_resolved_source(&locked).await?;
        let preset = parse_and_validate_preset(&content)?;
        return Ok((preset, locked));
    }

    let (repo, path, commit, content) = match parsed_ref {
        PresetReference::OfficialAlias { name, commit } => {
            let path = format!("presets/{name}.toml");
            if let Some(commit) = commit {
                let content =
                    fetch_preset_content_by_commit(OFFICIAL_PRESET_REPO, &path, &commit).await?;
                (OFFICIAL_PRESET_REPO.to_string(), path, commit, content)
            } else {
                match fetch_preset_content_from_default_branch(OFFICIAL_PRESET_REPO, &path).await {
                    Ok((resolved_commit, content)) => (
                        OFFICIAL_PRESET_REPO.to_string(),
                        path,
                        resolved_commit,
                        content,
                    ),
                    Err(default_branch_error) => {
                        if let Some(content) = embedded_official_preset_content(&path) {
                            (
                                EMBEDDED_PRESET_REPO.to_string(),
                                path,
                                embedded_content_hash(content),
                                content.to_string(),
                            )
                        } else {
                            return Err(default_branch_error);
                        }
                    }
                }
            }
        }
        PresetReference::Github { repo, path, commit } => {
            if let Some(commit) = commit {
                let content = fetch_preset_content_by_commit(&repo, &path, &commit).await?;
                (repo, path, commit, content)
            } else {
                let fetched = fetch_preset_content_from_default_branch(&repo, &path).await?;
                (repo, path, fetched.0, fetched.1)
            }
        }
    };

    let preset = parse_and_validate_preset(&content)?;
    let resolved = ResolvedPresetSource {
        preset_ref: preset_ref.to_string(),
        repo,
        path,
        commit,
    };
    write_locked_preset(project_dir, &resolved)?;
    Ok((preset, resolved))
}

fn embedded_official_preset_content(path: &str) -> Option<&'static str> {
    match path {
        EMBEDDED_BUN_PRESET_PATH => Some(EMBEDDED_BUN_PRESET_CONTENT),
        _ => None,
    }
}

fn embedded_content_hash(content: &str) -> String {
    use sha2::{Digest, Sha256};

    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    hex::encode(hasher.finalize())
}

async fn load_content_for_resolved_source(source: &ResolvedPresetSource) -> Result<String, String> {
    if source.repo == EMBEDDED_PRESET_REPO {
        let content = embedded_official_preset_content(&source.path).ok_or_else(|| {
            format!(
                "Embedded preset '{}' referenced in build lock is not available.",
                source.path
            )
        })?;
        return Ok(content.to_string());
    }
    fetch_preset_content_by_commit(&source.repo, &source.path, &source.commit).await
}

fn parse_and_validate_preset(content: &str) -> Result<BuildPreset, String> {
    let value: toml::Value =
        toml::from_str(content).map_err(|e| format!("Failed to parse build preset TOML: {e}"))?;
    if value.get("artifact").is_some() {
        return Err(
            "Build preset no longer supports [artifact]. Move exclude to top-level and remove include."
                .to_string(),
        );
    }
    if value.get("include").is_some() {
        return Err(
            "Build preset no longer supports top-level include. Use app [build].include when needed."
                .to_string(),
        );
    }
    let raw: BuildPresetRaw = value
        .try_into()
        .map_err(|e| format!("Failed to parse build preset TOML: {e}"))?;
    let (target_defaults, targets) = resolve_targets(raw.targets)?;
    let preset = BuildPreset {
        targets,
        target_defaults,
        exclude: raw.exclude,
        assets: raw.assets,
    };

    Ok(preset)
}

fn resolve_targets(
    raw_targets: BuildPresetRawTargets,
) -> Result<
    (
        BuildPresetTargetDefaults,
        std::collections::HashMap<String, BuildPresetTarget>,
    ),
    String,
> {
    let mut default_target = BuildPresetRawTarget {
        builder_image: raw_targets.builder_image.clone(),
        install: raw_targets.install.clone(),
        build: raw_targets.build.clone(),
    };
    if let Some(explicit_default) = raw_targets.entries.get("default").cloned() {
        default_target = merge_raw_target(explicit_default, default_target);
    }

    let target_defaults = BuildPresetTargetDefaults {
        builder_image: default_target.builder_image.clone().and_then(|value| {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        }),
        install: default_target.install.clone(),
        build: default_target.build.clone(),
    };
    let mut resolved = std::collections::HashMap::new();

    for (target_label, target) in raw_targets.entries {
        if target_label == "default" {
            continue;
        }

        let merged = merge_raw_target(target, default_target.clone());
        let builder_image = merged.builder_image.unwrap_or_default();
        if builder_image.trim().is_empty() {
            return Err(format!(
                "Build preset target '{}' is missing required field 'builder_image' (and no [targets] defaults were provided).",
                target_label
            ));
        }

        resolved.insert(
            target_label,
            BuildPresetTarget {
                builder_image,
                install: merged.install,
                build: merged.build,
            },
        );
    }

    if resolved.is_empty() && target_defaults.builder_image.is_none() {
        return Err("Build preset must define at least one target, or provide [targets] defaults with builder_image.".to_string());
    }

    Ok((target_defaults, resolved))
}

fn merge_raw_target(
    primary: BuildPresetRawTarget,
    fallback: BuildPresetRawTarget,
) -> BuildPresetRawTarget {
    BuildPresetRawTarget {
        builder_image: primary.builder_image.or(fallback.builder_image),
        install: primary.install.or(fallback.install),
        build: primary.build.or(fallback.build),
    }
}

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
        .map_err(|e| format!("Failed to fetch build preset: {e}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "Failed to fetch build preset at commit {}: HTTP {}",
            commit,
            response.status()
        ));
    }
    response
        .text()
        .await
        .map_err(|e| format!("Failed to read build preset response body: {e}"))
}

async fn fetch_preset_content_from_default_branch(
    repo: &str,
    path: &str,
) -> Result<(String, String), String> {
    let Some((owner, repository)) = repo.split_once('/') else {
        return Err(format!(
            "Invalid preset repo '{}': expected owner/repo",
            repo
        ));
    };
    let url = format!("https://api.github.com/repos/{owner}/{repository}/contents/{path}");
    let client = reqwest::Client::new();
    let response = client
        .get(url)
        .header("User-Agent", "tako-cli")
        .send()
        .await
        .map_err(|e| format!("Failed to resolve build preset from GitHub: {e}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "Failed to resolve build preset '{}': HTTP {}",
            path,
            response.status()
        ));
    }
    let raw = response
        .text()
        .await
        .map_err(|e| format!("Failed to read build preset resolution response: {e}"))?;
    let json: serde_json::Value =
        serde_json::from_str(&raw).map_err(|e| format!("Invalid GitHub contents response: {e}"))?;

    let sha = json
        .get("sha")
        .and_then(|v| v.as_str())
        .map(ToOwned::to_owned)
        .ok_or_else(|| "GitHub response missing preset sha".to_string())?;
    let content_b64 = json
        .get("content")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "GitHub response missing preset content".to_string())?;
    let normalized = content_b64.replace('\n', "");
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(normalized)
        .map_err(|e| format!("Failed to decode preset content from GitHub: {e}"))?;
    let content = String::from_utf8(bytes)
        .map_err(|e| format!("Build preset content is not valid UTF-8: {e}"))?;
    Ok((sha, content))
}

pub fn lock_file_path(project_dir: &Path) -> PathBuf {
    project_dir.join(BUILD_LOCK_RELATIVE_PATH)
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let parsed = parse_preset_reference("bun/abc1234").unwrap();
        assert_eq!(
            parsed,
            PresetReference::OfficialAlias {
                name: "bun".to_string(),
                commit: Some("abc1234".to_string()),
            }
        );
    }

    #[test]
    fn parse_preset_reference_accepts_github_with_commit() {
        let parsed =
            parse_preset_reference("github:my-user/tako/presets/presets/bun.toml@abc123def456")
                .unwrap();
        assert_eq!(
            parsed,
            PresetReference::Github {
                repo: "my-user/tako".to_string(),
                path: "presets/presets/bun.toml".to_string(),
                commit: Some("abc123def456".to_string()),
            }
        );
    }

    #[test]
    fn parse_preset_reference_accepts_github_without_commit() {
        let parsed = parse_preset_reference("github:my-user/tako/presets/bun.toml").unwrap();
        assert_eq!(
            parsed,
            PresetReference::Github {
                repo: "my-user/tako".to_string(),
                path: "presets/bun.toml".to_string(),
                commit: None,
            }
        );
    }

    #[test]
    fn parse_preset_reference_rejects_invalid_values() {
        assert!(parse_preset_reference("").is_err());
        assert!(parse_preset_reference("bun/not-a-hash").is_err());
        assert!(parse_preset_reference("github:owner/repo").is_err());
        assert!(parse_preset_reference("github:owner/repo/path.jsonc").is_err());
        assert!(parse_preset_reference("github:owner/repo/path.json").is_err());
        assert!(parse_preset_reference("github:owner/repo/path.yaml").is_err());
        assert!(parse_preset_reference("bun/abc12").is_err());
        assert!(parse_preset_reference("bun/abc12345/extra").is_err());
    }

    #[test]
    fn parse_and_validate_preset_accepts_toml() {
        let raw = r#"
assets = ["dist/client"]

[targets.linux-x86_64-glibc]
builder_image = "oven/bun:1.2"
"#;
        let preset = parse_and_validate_preset(raw).unwrap();
        assert!(preset.targets.contains_key("linux-x86_64-glibc"));
        assert_eq!(preset.assets, vec!["dist/client".to_string()]);
    }

    #[test]
    fn parse_and_validate_preset_accepts_toml_with_comments() {
        let raw = r#"
# preset file for bun
exclude = ["**/*.map"]

[targets.linux-x86_64-glibc]
builder_image = "oven/bun:1.2"

[targets.linux-aarch64-glibc]
builder_image = "oven/bun:1.2"
"#;

        let preset = parse_and_validate_preset(raw).unwrap();
        assert!(preset.targets.contains_key("linux-x86_64-glibc"));
        assert!(preset.targets.contains_key("linux-aarch64-glibc"));
        assert_eq!(preset.exclude, vec!["**/*.map".to_string()]);
    }

    #[test]
    fn parse_and_validate_preset_rejects_legacy_artifact_table() {
        let raw = r#"
[targets.linux-x86_64-glibc]
builder_image = "oven/bun:1.2"

[artifact]
include = ["**/*"]
"#;
        let err = parse_and_validate_preset(raw).unwrap_err();
        assert!(err.contains("no longer supports [artifact]"));
    }

    #[test]
    fn parse_and_validate_preset_rejects_top_level_include() {
        let raw = r#"
include = ["dist/**"]

[targets.linux-x86_64-glibc]
builder_image = "oven/bun:1.2"
"#;
        let err = parse_and_validate_preset(raw).unwrap_err();
        assert!(err.contains("no longer supports top-level include"));
    }

    #[test]
    fn parse_and_validate_preset_accepts_multiline_scripts() {
        let raw = r#"
[targets.linux-x86_64-glibc]
builder_image = "oven/bun:1.2"
install = '''
if [ -f bun.lockb ] || [ -f bun.lock ]; then
  bun install --frozen-lockfile
else
  bun install
fi
'''
build = '''
bun run --if-present build
'''
"#;
        let preset = parse_and_validate_preset(raw).unwrap();
        let target = preset.targets.get("linux-x86_64-glibc").unwrap();
        assert!(target.install.as_deref().unwrap().contains("bun install"));
        assert!(target.build.as_deref().unwrap().contains("--if-present"));
    }

    #[test]
    fn parse_and_validate_preset_applies_targets_default() {
        let raw = r#"
[targets]
builder_image = "oven/bun:1.2"
install = "bun install"
build = "bun run build"

[targets.linux-x86_64-glibc]
[targets.linux-aarch64-glibc]
"#;

        let preset = parse_and_validate_preset(raw).unwrap();
        assert_eq!(preset.targets.len(), 2);
        let glibc = preset.targets.get("linux-x86_64-glibc").unwrap();
        assert_eq!(glibc.builder_image, "oven/bun:1.2");
        assert_eq!(glibc.install.as_deref(), Some("bun install"));
        assert_eq!(glibc.build.as_deref(), Some("bun run build"));
    }

    #[test]
    fn parse_and_validate_preset_allows_target_overrides_on_top_of_default() {
        let raw = r#"
[targets]
builder_image = "oven/bun:1.2"
install = "bun install"

[targets.linux-x86_64-glibc]
builder_image = "custom/image:latest"
build = "bun run custom-build"
"#;

        let preset = parse_and_validate_preset(raw).unwrap();
        let target = preset.targets.get("linux-x86_64-glibc").unwrap();
        assert_eq!(target.builder_image, "custom/image:latest");
        assert_eq!(target.install.as_deref(), Some("bun install"));
        assert_eq!(target.build.as_deref(), Some("bun run custom-build"));
    }

    #[test]
    fn parse_and_validate_preset_accepts_only_targets_defaults() {
        let raw = r#"
[targets]
builder_image = "oven/bun:1.2"
"#;
        let preset = parse_and_validate_preset(raw).unwrap();
        assert!(preset.targets.is_empty());
        assert_eq!(
            preset.target_defaults.builder_image.as_deref(),
            Some("oven/bun:1.2")
        );
    }

    #[test]
    fn parse_and_validate_preset_supports_legacy_targets_default_entry() {
        let raw = r#"
[targets.default]
builder_image = "oven/bun:1.2"
install = "bun install"

[targets.linux-x86_64-glibc]
"#;
        let preset = parse_and_validate_preset(raw).unwrap();
        let target = preset.targets.get("linux-x86_64-glibc").unwrap();
        assert_eq!(target.builder_image, "oven/bun:1.2");
        assert_eq!(target.install.as_deref(), Some("bun install"));
    }

    #[test]
    fn parse_and_validate_preset_rejects_target_without_builder_image_and_default() {
        let raw = r#"
[targets.linux-x86_64-glibc]
build = "bun run build"
"#;
        let err = parse_and_validate_preset(raw).unwrap_err();
        assert!(err.contains("missing required field 'builder_image'"));
    }

    #[test]
    fn parse_and_validate_preset_rejects_defaults_without_builder_image_when_no_targets() {
        let raw = r#"
[targets]
install = "bun install"
"#;
        let err = parse_and_validate_preset(raw).unwrap_err();
        assert!(err.contains("provide [targets] defaults with builder_image"));
    }

    #[test]
    fn parse_and_validate_preset_rejects_empty_targets() {
        let raw = r#"
exclude = ["**/*.map"]
"#;
        let err = parse_and_validate_preset(raw).unwrap_err();
        assert!(err.contains("at least one target"));
    }

    #[test]
    fn lock_round_trip_writes_and_reads_build_lock_file() {
        let temp = tempfile::TempDir::new().unwrap();
        let resolved = ResolvedPresetSource {
            preset_ref: "bun".to_string(),
            repo: "tako-sh/presets".to_string(),
            path: "presets/bun.toml".to_string(),
            commit: "abc123".to_string(),
        };
        write_locked_preset(temp.path(), &resolved).unwrap();
        let loaded = read_locked_preset(temp.path()).unwrap().unwrap();
        assert_eq!(loaded, resolved);
        assert!(lock_file_path(temp.path()).exists());
    }
}

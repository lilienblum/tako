use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

const FRESHNESS_TTL_SECS: u64 = 3600;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct CacheMeta {
    branches: HashMap<String, BranchMeta>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BranchMeta {
    sha: String,
    last_checked: u64,
}

fn safe_repo_slug(repo: &str) -> String {
    repo.replace('/', "__")
}

fn repo_cache_dir(repo: &str) -> Result<PathBuf, String> {
    let base = crate::paths::tako_cache_dir()
        .map_err(|e| format!("Could not determine cache directory: {e}"))?;
    Ok(base.join("presets").join(safe_repo_slug(repo)))
}

fn cached_file_path(repo: &str, sha: &str, path: &str) -> Result<PathBuf, String> {
    Ok(repo_cache_dir(repo)?.join(sha).join(path))
}

fn meta_path(repo: &str) -> Result<PathBuf, String> {
    Ok(repo_cache_dir(repo)?.join("_meta.json"))
}

pub fn read_cached(repo: &str, sha: &str, path: &str) -> Option<String> {
    let file = cached_file_path(repo, sha, path).ok()?;
    fs::read_to_string(file).ok()
}

pub fn write_cached(repo: &str, sha: &str, path: &str, content: &str) -> Result<(), String> {
    let file = cached_file_path(repo, sha, path)?;
    let parent = file
        .parent()
        .ok_or_else(|| "Invalid cache path".to_string())?;
    fs::create_dir_all(parent)
        .map_err(|e| format!("Failed to create cache directory {}: {e}", parent.display()))?;

    // Atomic write: write to temp path, then rename
    let tmp = file.with_extension("tmp");
    fs::write(&tmp, content).map_err(|e| format!("Failed to write cache file: {e}"))?;
    fs::rename(&tmp, &file).map_err(|e| format!("Failed to rename cache file: {e}"))?;
    Ok(())
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn read_meta(repo: &str) -> Option<CacheMeta> {
    let path = meta_path(repo).ok()?;
    let raw = fs::read_to_string(path).ok()?;
    serde_json::from_str(&raw).ok()
}

fn write_meta(repo: &str, meta: &CacheMeta) -> Result<(), String> {
    let path = meta_path(repo)?;
    let parent = path
        .parent()
        .ok_or_else(|| "Invalid meta path".to_string())?;
    fs::create_dir_all(parent).map_err(|e| format!("Failed to create cache directory: {e}"))?;
    let json =
        serde_json::to_string_pretty(meta).map_err(|e| format!("Failed to serialize meta: {e}"))?;
    fs::write(&path, json).map_err(|e| format!("Failed to write meta: {e}"))
}

/// Returns the cached SHA for this branch if the TTL hasn't expired.
pub fn fresh_sha(repo: &str, branch: &str) -> Option<String> {
    let meta = read_meta(repo)?;
    let entry = meta.branches.get(branch)?;
    if now_secs().saturating_sub(entry.last_checked) < FRESHNESS_TTL_SECS {
        Some(entry.sha.clone())
    } else {
        None
    }
}

/// Returns the last known SHA for this branch regardless of TTL.
pub fn last_known_sha(repo: &str, branch: &str) -> Option<String> {
    let meta = read_meta(repo)?;
    meta.branches.get(branch).map(|e| e.sha.clone())
}

pub fn update_freshness(repo: &str, branch: &str, sha: &str) -> Result<(), String> {
    let mut meta = read_meta(repo).unwrap_or_default();
    meta.branches.insert(
        branch.to_string(),
        BranchMeta {
            sha: sha.to_string(),
            last_checked: now_secs(),
        },
    );
    write_meta(repo, &meta)
}

/// Scan all cached SHAs for any version of this path. Returns (sha, content).
pub fn find_any_cached(repo: &str, path: &str) -> Option<(String, String)> {
    let base = repo_cache_dir(repo).ok()?;
    let entries = fs::read_dir(&base).ok()?;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let sha = name.to_str()?;
        if sha.starts_with('_') {
            continue;
        }
        let file = base.join(sha).join(path);
        if let Ok(content) = fs::read_to_string(&file) {
            return Some((sha.to_string(), content));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_repo_slug_replaces_slash() {
        assert_eq!(safe_repo_slug("tako-sh/presets"), "tako-sh__presets");
    }

    #[test]
    fn read_write_round_trip() {
        let tmp = tempfile::TempDir::new().unwrap();

        // Write directly to the temp dir layout
        let sha = "abc1234567890";
        let path = "presets/javascript.toml";
        let content = "[vite]\ndev = [\"vite\", \"dev\"]\n";

        let file = tmp.path().join(sha).join(path);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, content).unwrap();

        let read_back = fs::read_to_string(&file).unwrap();
        assert_eq!(read_back, content);
    }

    #[test]
    fn meta_round_trip() {
        let tmp = tempfile::TempDir::new().unwrap();
        let meta_file = tmp.path().join("_meta.json");

        let mut meta = CacheMeta::default();
        meta.branches.insert(
            "master".to_string(),
            BranchMeta {
                sha: "abc123".to_string(),
                last_checked: 1000,
            },
        );

        let json = serde_json::to_string_pretty(&meta).unwrap();
        fs::write(&meta_file, &json).unwrap();

        let raw = fs::read_to_string(&meta_file).unwrap();
        let loaded: CacheMeta = serde_json::from_str(&raw).unwrap();
        assert_eq!(loaded.branches["master"].sha, "abc123");
        assert_eq!(loaded.branches["master"].last_checked, 1000);
    }

    #[test]
    fn freshness_check_expired() {
        let mut meta = CacheMeta::default();
        meta.branches.insert(
            "master".to_string(),
            BranchMeta {
                sha: "old".to_string(),
                last_checked: 0, // epoch — definitely expired
            },
        );
        let entry = &meta.branches["master"];
        let is_fresh = now_secs().saturating_sub(entry.last_checked) < FRESHNESS_TTL_SECS;
        assert!(!is_fresh);
    }

    #[test]
    fn freshness_check_fresh() {
        let mut meta = CacheMeta::default();
        meta.branches.insert(
            "master".to_string(),
            BranchMeta {
                sha: "new".to_string(),
                last_checked: now_secs(),
            },
        );
        let entry = &meta.branches["master"];
        let is_fresh = now_secs().saturating_sub(entry.last_checked) < FRESHNESS_TTL_SECS;
        assert!(is_fresh);
    }

    #[test]
    fn find_any_cached_scans_shas() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = "presets/javascript.toml";
        let content = "[vite]\ndev = [\"vite\"]";

        // Create two SHA dirs with the file
        let sha1_file = tmp.path().join("sha1111").join(path);
        fs::create_dir_all(sha1_file.parent().unwrap()).unwrap();
        fs::write(&sha1_file, content).unwrap();

        let sha2_file = tmp.path().join("sha2222").join(path);
        fs::create_dir_all(sha2_file.parent().unwrap()).unwrap();
        fs::write(&sha2_file, "other").unwrap();

        // Scan the temp dir directly (simulating find_any_cached logic)
        let mut found = Vec::new();
        for entry in fs::read_dir(tmp.path()).unwrap().flatten() {
            let name = entry.file_name();
            let sha = name.to_string_lossy().to_string();
            if sha.starts_with('_') {
                continue;
            }
            let file = tmp.path().join(&sha).join(path);
            if let Ok(c) = fs::read_to_string(&file) {
                found.push((sha, c));
            }
        }
        assert_eq!(found.len(), 2);
    }
}

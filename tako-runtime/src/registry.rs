use std::path::Path;
use std::time::Duration;

use crate::cache::RuntimeCache;
use crate::fetch::fetch_runtime_toml;
use crate::types::RuntimeDef;

const DEFAULT_TTL_SECS: u64 = 86400; // 24 hours

/// Load a runtime definition by id.
///
/// Resolution order: local cache -> GitHub fetch -> parse.
/// The cache directory is caller-provided (CLI uses `~/.tako/cache/runtimes/`,
/// server uses its own path).
pub async fn load_runtime(id: &str, cache_dir: &Path) -> Result<RuntimeDef, String> {
    let cache = RuntimeCache::new(
        cache_dir.to_path_buf(),
        Duration::from_secs(DEFAULT_TTL_SECS),
    );

    if let Some(content) = cache.get(id) {
        let mut def = parse_runtime(&content)
            .map_err(|e| format!("cached runtime '{id}' is invalid: {e}"))?;
        def.id = id.to_string();
        return Ok(def);
    }

    let content = fetch_runtime_toml(id).await?;

    let mut def = parse_runtime(&content)?;
    def.id = id.to_string();

    // Cache after successful parse
    if let Err(e) = cache.put(id, &content) {
        // Non-fatal: log but don't fail
        eprintln!("warning: failed to cache runtime '{id}': {e}");
    }

    Ok(def)
}

/// Parse a runtime definition from a TOML string.
pub fn parse_runtime(content: &str) -> Result<RuntimeDef, String> {
    toml::from_str::<RuntimeDef>(content).map_err(|e| format!("failed to parse runtime TOML: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn parse_runtime_rejects_invalid_toml() {
        let result = parse_runtime("not valid toml [[[");
        assert!(result.is_err());
    }

    #[test]
    fn parse_runtime_rejects_missing_required_fields() {
        let result = parse_runtime("[entrypoint]\ncandidates = []");
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn load_runtime_uses_cache_on_hit() {
        let dir = TempDir::new().unwrap();
        let cache_dir = dir.path().join("cache");
        let cache = crate::cache::RuntimeCache::new(
            cache_dir.clone(),
            std::time::Duration::from_secs(3600),
        );
        let content = "language = \"javascript\"\n";
        cache.put("test-rt", content).unwrap();

        let def = load_runtime("test-rt", &cache_dir).await.unwrap();
        assert_eq!(def.id, "test-rt");
    }
}

use std::path::PathBuf;
use std::time::Duration;

/// File-based cache for runtime TOML definitions with TTL expiry.
pub struct RuntimeCache {
    cache_dir: PathBuf,
    ttl: Duration,
}

impl RuntimeCache {
    pub fn new(cache_dir: PathBuf, ttl: Duration) -> Self {
        Self { cache_dir, ttl }
    }

    /// Return cached content if the file exists and hasn't expired.
    pub fn get(&self, id: &str) -> Option<String> {
        let path = self.file_path(id);
        let metadata = std::fs::metadata(&path).ok()?;
        let modified = metadata.modified().ok()?;
        let age = modified.elapsed().ok()?;
        if age > self.ttl {
            return None;
        }
        std::fs::read_to_string(&path).ok()
    }

    /// Write content to the cache file.
    pub fn put(&self, id: &str, content: &str) -> Result<(), String> {
        std::fs::create_dir_all(&self.cache_dir)
            .map_err(|e| format!("failed to create cache dir: {e}"))?;
        let path = self.file_path(id);
        std::fs::write(&path, content)
            .map_err(|e| format!("failed to write cache file {}: {e}", path.display()))
    }

    fn file_path(&self, id: &str) -> PathBuf {
        self.cache_dir.join(format!("{id}.toml"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn put_and_get_roundtrip() {
        let dir = TempDir::new().unwrap();
        let cache = RuntimeCache::new(dir.path().to_path_buf(), Duration::from_secs(3600));
        cache.put("bun", "id = \"bun\"").unwrap();
        assert_eq!(cache.get("bun"), Some("id = \"bun\"".to_string()));
    }

    #[test]
    fn get_returns_none_for_missing_entry() {
        let dir = TempDir::new().unwrap();
        let cache = RuntimeCache::new(dir.path().to_path_buf(), Duration::from_secs(3600));
        assert_eq!(cache.get("nonexistent"), None);
    }

    #[test]
    fn get_returns_none_when_expired() {
        let dir = TempDir::new().unwrap();
        let cache = RuntimeCache::new(dir.path().to_path_buf(), Duration::ZERO);
        cache.put("bun", "id = \"bun\"").unwrap();
        // TTL is zero, so immediately expired
        assert_eq!(cache.get("bun"), None);
    }

    #[test]
    fn put_creates_cache_directory() {
        let dir = TempDir::new().unwrap();
        let nested = dir.path().join("deep").join("nested");
        let cache = RuntimeCache::new(nested.clone(), Duration::from_secs(3600));
        cache.put("bun", "id = \"bun\"").unwrap();
        assert!(nested.join("bun.toml").is_file());
    }
}

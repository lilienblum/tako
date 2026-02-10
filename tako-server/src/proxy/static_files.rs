//! Static file serving
//!
//! Serves static files directly from the proxy for configured apps.
//! Files are served from the app's `public/` directory.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use parking_lot::RwLock;
use thiserror::Error;

/// Errors that can occur during static file serving
#[derive(Debug, Error)]
pub enum StaticFileError {
    #[error("File not found: {0}")]
    NotFound(String),

    #[error("Path traversal detected: {0}")]
    PathTraversal(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Invalid path: {0}")]
    InvalidPath(String),
}

/// Configuration for static file serving
#[derive(Debug, Clone)]
pub struct StaticConfig {
    /// Whether to enable static file serving
    pub enabled: bool,
    /// Name of the public directory (relative to app root)
    pub public_dir: String,
    /// Cache-Control max-age in seconds for static files
    pub cache_max_age: u64,
    /// Whether to serve index.html for directories
    pub serve_index: bool,
    /// Whether to serve gzip-compressed files if available
    pub serve_gzip: bool,
    /// File extensions to consider as static
    pub static_extensions: Vec<String>,
}

impl Default for StaticConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            public_dir: "public".to_string(),
            cache_max_age: 3600, // 1 hour
            serve_index: true,
            serve_gzip: true,
            static_extensions: vec![
                "html", "css", "js", "json", "png", "jpg", "jpeg", "gif", "svg", "ico", "woff",
                "woff2", "ttf", "eot", "map", "webp", "avif", "mp4", "webm", "pdf", "txt", "xml",
            ]
            .into_iter()
            .map(String::from)
            .collect(),
        }
    }
}

/// A static file ready to be served
#[derive(Debug, Clone)]
pub struct StaticFile {
    /// Full path to the file
    pub path: PathBuf,
    /// MIME type
    pub content_type: String,
    /// File size in bytes
    pub size: u64,
    /// Last modified time
    pub last_modified: SystemTime,
    /// ETag (based on size and modified time)
    pub etag: String,
    /// Cache-Control header value
    pub cache_control: String,
}

impl StaticFile {
    /// Read file contents
    pub fn read_contents(&self) -> Result<Vec<u8>, StaticFileError> {
        Ok(std::fs::read(&self.path)?)
    }

    /// Check if file has been modified since the given time
    pub fn modified_since(&self, since: SystemTime) -> bool {
        self.last_modified > since
    }

    /// HTTP Last-Modified header value
    pub fn last_modified_header(&self) -> String {
        // Format as HTTP-date (RFC 7231)
        let duration = self
            .last_modified
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        let secs = duration.as_secs();

        // Simple formatting (a real implementation would use chrono or time crate)
        format!("{}", secs)
    }
}

/// Static file server for an app
pub struct AppStaticServer {
    /// App name
    app_name: String,
    /// Root directory for static files
    root: PathBuf,
    /// Configuration
    config: StaticConfig,
    /// MIME type map
    mime_types: HashMap<String, String>,
}

impl AppStaticServer {
    pub fn new(app_name: String, app_root: PathBuf, config: StaticConfig) -> Self {
        let root = app_root.join(&config.public_dir);

        Self {
            app_name,
            root,
            config,
            mime_types: Self::default_mime_types(),
        }
    }

    /// Get default MIME type mappings
    fn default_mime_types() -> HashMap<String, String> {
        let mut map = HashMap::new();

        // Text types
        map.insert("html".to_string(), "text/html; charset=utf-8".to_string());
        map.insert("htm".to_string(), "text/html; charset=utf-8".to_string());
        map.insert("css".to_string(), "text/css; charset=utf-8".to_string());
        map.insert(
            "js".to_string(),
            "application/javascript; charset=utf-8".to_string(),
        );
        map.insert("mjs".to_string(), "application/javascript".to_string());
        map.insert("json".to_string(), "application/json".to_string());
        map.insert("txt".to_string(), "text/plain; charset=utf-8".to_string());
        map.insert("xml".to_string(), "application/xml".to_string());
        map.insert("csv".to_string(), "text/csv".to_string());

        // Images
        map.insert("png".to_string(), "image/png".to_string());
        map.insert("jpg".to_string(), "image/jpeg".to_string());
        map.insert("jpeg".to_string(), "image/jpeg".to_string());
        map.insert("gif".to_string(), "image/gif".to_string());
        map.insert("svg".to_string(), "image/svg+xml".to_string());
        map.insert("ico".to_string(), "image/x-icon".to_string());
        map.insert("webp".to_string(), "image/webp".to_string());
        map.insert("avif".to_string(), "image/avif".to_string());

        // Fonts
        map.insert("woff".to_string(), "font/woff".to_string());
        map.insert("woff2".to_string(), "font/woff2".to_string());
        map.insert("ttf".to_string(), "font/ttf".to_string());
        map.insert(
            "eot".to_string(),
            "application/vnd.ms-fontobject".to_string(),
        );
        map.insert("otf".to_string(), "font/otf".to_string());

        // Video/Audio
        map.insert("mp4".to_string(), "video/mp4".to_string());
        map.insert("webm".to_string(), "video/webm".to_string());
        map.insert("mp3".to_string(), "audio/mpeg".to_string());
        map.insert("wav".to_string(), "audio/wav".to_string());
        map.insert("ogg".to_string(), "audio/ogg".to_string());

        // Documents
        map.insert("pdf".to_string(), "application/pdf".to_string());

        // Source maps
        map.insert("map".to_string(), "application/json".to_string());

        // Manifest
        map.insert(
            "webmanifest".to_string(),
            "application/manifest+json".to_string(),
        );

        map
    }

    /// Check if this server has static files enabled and the directory exists
    pub fn is_available(&self) -> bool {
        self.config.enabled && self.root.is_dir()
    }

    /// Get the MIME type for a file extension
    fn get_mime_type(&self, extension: &str) -> String {
        self.mime_types
            .get(extension)
            .cloned()
            .unwrap_or_else(|| "application/octet-stream".to_string())
    }

    /// Resolve a request path to a file
    pub fn resolve(&self, request_path: &str) -> Result<StaticFile, StaticFileError> {
        // Normalize the path
        let clean_path = self.normalize_path(request_path)?;

        // Build full path
        let full_path = self.root.join(&clean_path);

        // Security: ensure the resolved path is still within root
        let canonical = full_path
            .canonicalize()
            .map_err(|_| StaticFileError::NotFound(request_path.to_string()))?;

        let root_canonical = self.root.canonicalize().map_err(StaticFileError::Io)?;

        if !canonical.starts_with(&root_canonical) {
            return Err(StaticFileError::PathTraversal(request_path.to_string()));
        }

        // Check if it's a directory and we should serve index.html
        let target_path = if canonical.is_dir() && self.config.serve_index {
            let index_path = canonical.join("index.html");
            if index_path.exists() {
                index_path
            } else {
                return Err(StaticFileError::NotFound(request_path.to_string()));
            }
        } else if canonical.is_file() {
            canonical
        } else {
            return Err(StaticFileError::NotFound(request_path.to_string()));
        };

        // Get file metadata
        let metadata = std::fs::metadata(&target_path)?;
        let size = metadata.len();
        let last_modified = metadata.modified().unwrap_or(SystemTime::now());

        // Get extension and MIME type
        let extension = target_path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();

        let content_type = self.get_mime_type(&extension);

        // Generate ETag
        let etag = self.generate_etag(size, &last_modified);

        // Build cache control header
        let cache_control = format!("public, max-age={}", self.config.cache_max_age);

        Ok(StaticFile {
            path: target_path,
            content_type,
            size,
            last_modified,
            etag,
            cache_control,
        })
    }

    /// Normalize a URL path (remove leading slash, handle ..)
    fn normalize_path(&self, path: &str) -> Result<String, StaticFileError> {
        // Remove leading slash
        let path = path.trim_start_matches('/');

        // Decode URL encoding (simplified - real impl would use percent-encoding crate)
        let path = path.replace("%20", " ");

        // Check for path traversal attempts
        if path.contains("..") {
            return Err(StaticFileError::PathTraversal(path.to_string()));
        }

        // Remove any null bytes
        if path.contains('\0') {
            return Err(StaticFileError::InvalidPath(
                "null byte in path".to_string(),
            ));
        }

        Ok(path.to_string())
    }

    /// Generate an ETag for caching
    fn generate_etag(&self, size: u64, modified: &SystemTime) -> String {
        let modified_secs = modified
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        format!("\"{}{}\"", size, modified_secs)
    }

    /// Check if we have a gzipped version of the file
    pub fn has_gzip(&self, path: &Path) -> bool {
        if !self.config.serve_gzip {
            return false;
        }

        let gz_path = path.with_extension(format!(
            "{}.gz",
            path.extension().and_then(|e| e.to_str()).unwrap_or("")
        ));

        gz_path.exists()
    }

    /// Get app name
    pub fn app_name(&self) -> &str {
        &self.app_name
    }

    /// Get root directory
    pub fn root(&self) -> &Path {
        &self.root
    }
}

/// Global static file manager for all apps
pub struct StaticFileManager {
    /// Per-app static servers
    servers: RwLock<HashMap<String, Arc<AppStaticServer>>>,
    /// Default configuration
    default_config: StaticConfig,
}

impl StaticFileManager {
    pub fn new(default_config: StaticConfig) -> Self {
        Self {
            servers: RwLock::new(HashMap::new()),
            default_config,
        }
    }

    /// Register an app for static file serving
    pub fn register_app(&self, app_name: &str, app_root: PathBuf) {
        self.register_app_with_config(app_name, app_root, self.default_config.clone());
    }

    /// Register an app with custom configuration
    pub fn register_app_with_config(
        &self,
        app_name: &str,
        app_root: PathBuf,
        config: StaticConfig,
    ) {
        let server = Arc::new(AppStaticServer::new(app_name.to_string(), app_root, config));
        let mut servers = self.servers.write();
        servers.insert(app_name.to_string(), server);
    }

    /// Unregister an app
    pub fn unregister_app(&self, app_name: &str) {
        let mut servers = self.servers.write();
        servers.remove(app_name);
    }

    /// Try to resolve a static file for an app
    pub fn resolve(
        &self,
        app_name: &str,
        path: &str,
    ) -> Option<Result<StaticFile, StaticFileError>> {
        let servers = self.servers.read();
        let server = servers.get(app_name)?;

        if !server.is_available() {
            return None;
        }

        Some(server.resolve(path))
    }

    /// Check if an app has static file serving enabled
    pub fn has_static_files(&self, app_name: &str) -> bool {
        let servers = self.servers.read();
        servers
            .get(app_name)
            .map(|s| s.is_available())
            .unwrap_or(false)
    }

    /// List all registered apps
    pub fn list_apps(&self) -> Vec<String> {
        let servers = self.servers.read();
        servers.keys().cloned().collect()
    }
}

impl Default for StaticFileManager {
    fn default() -> Self {
        Self::new(StaticConfig::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn create_test_files(dir: &Path) {
        let public = dir.join("public");
        fs::create_dir_all(&public).unwrap();

        // Create test files
        fs::write(public.join("index.html"), "<html></html>").unwrap();
        fs::write(public.join("style.css"), "body { }").unwrap();
        fs::write(public.join("app.js"), "console.log()").unwrap();
        fs::write(public.join("logo.png"), [0x89, 0x50, 0x4E, 0x47]).unwrap();

        // Create subdirectory
        let sub = public.join("assets");
        fs::create_dir_all(&sub).unwrap();
        fs::write(sub.join("image.jpg"), [0xFF, 0xD8, 0xFF]).unwrap();
    }

    #[test]
    fn test_static_config_default() {
        let config = StaticConfig::default();
        assert!(config.enabled);
        assert_eq!(config.public_dir, "public");
        assert_eq!(config.cache_max_age, 3600);
        assert!(config.serve_index);
    }

    #[test]
    fn test_app_static_server_creation() {
        let temp = TempDir::new().unwrap();
        let config = StaticConfig::default();
        let server = AppStaticServer::new("test".to_string(), temp.path().to_path_buf(), config);

        assert_eq!(server.app_name(), "test");
    }

    #[test]
    fn test_resolve_index_html() {
        let temp = TempDir::new().unwrap();
        create_test_files(temp.path());

        let config = StaticConfig::default();
        let server = AppStaticServer::new("test".to_string(), temp.path().to_path_buf(), config);

        assert!(server.is_available());

        let file = server.resolve("/").unwrap();
        assert!(file.content_type.contains("text/html"));
        assert!(file.path.ends_with("index.html"));
    }

    #[test]
    fn test_resolve_css_file() {
        let temp = TempDir::new().unwrap();
        create_test_files(temp.path());

        let config = StaticConfig::default();
        let server = AppStaticServer::new("test".to_string(), temp.path().to_path_buf(), config);

        let file = server.resolve("/style.css").unwrap();
        assert!(file.content_type.contains("text/css"));
    }

    #[test]
    fn test_resolve_js_file() {
        let temp = TempDir::new().unwrap();
        create_test_files(temp.path());

        let config = StaticConfig::default();
        let server = AppStaticServer::new("test".to_string(), temp.path().to_path_buf(), config);

        let file = server.resolve("/app.js").unwrap();
        assert!(file.content_type.contains("javascript"));
    }

    #[test]
    fn test_resolve_image_file() {
        let temp = TempDir::new().unwrap();
        create_test_files(temp.path());

        let config = StaticConfig::default();
        let server = AppStaticServer::new("test".to_string(), temp.path().to_path_buf(), config);

        let file = server.resolve("/logo.png").unwrap();
        assert_eq!(file.content_type, "image/png");
    }

    #[test]
    fn test_resolve_subdirectory_file() {
        let temp = TempDir::new().unwrap();
        create_test_files(temp.path());

        let config = StaticConfig::default();
        let server = AppStaticServer::new("test".to_string(), temp.path().to_path_buf(), config);

        let file = server.resolve("/assets/image.jpg").unwrap();
        assert_eq!(file.content_type, "image/jpeg");
    }

    #[test]
    fn test_resolve_not_found() {
        let temp = TempDir::new().unwrap();
        create_test_files(temp.path());

        let config = StaticConfig::default();
        let server = AppStaticServer::new("test".to_string(), temp.path().to_path_buf(), config);

        let result = server.resolve("/nonexistent.txt");
        assert!(matches!(result, Err(StaticFileError::NotFound(_))));
    }

    #[test]
    fn test_path_traversal_blocked() {
        let temp = TempDir::new().unwrap();
        create_test_files(temp.path());

        let config = StaticConfig::default();
        let server = AppStaticServer::new("test".to_string(), temp.path().to_path_buf(), config);

        let result = server.resolve("/../../../etc/passwd");
        assert!(matches!(result, Err(StaticFileError::PathTraversal(_))));
    }

    #[test]
    fn test_static_file_read_contents() {
        let temp = TempDir::new().unwrap();
        create_test_files(temp.path());

        let config = StaticConfig::default();
        let server = AppStaticServer::new("test".to_string(), temp.path().to_path_buf(), config);

        let file = server.resolve("/index.html").unwrap();
        let contents = file.read_contents().unwrap();
        assert_eq!(contents, b"<html></html>");
    }

    #[test]
    fn test_etag_generation() {
        let temp = TempDir::new().unwrap();
        create_test_files(temp.path());

        let config = StaticConfig::default();
        let server = AppStaticServer::new("test".to_string(), temp.path().to_path_buf(), config);

        let file = server.resolve("/index.html").unwrap();
        assert!(file.etag.starts_with('"'));
        assert!(file.etag.ends_with('"'));
    }

    #[test]
    fn test_cache_control_header() {
        let temp = TempDir::new().unwrap();
        create_test_files(temp.path());

        let config = StaticConfig {
            cache_max_age: 7200,
            ..Default::default()
        };
        let server = AppStaticServer::new("test".to_string(), temp.path().to_path_buf(), config);

        let file = server.resolve("/index.html").unwrap();
        assert!(file.cache_control.contains("max-age=7200"));
    }

    #[test]
    fn test_static_file_manager() {
        let temp = TempDir::new().unwrap();
        create_test_files(temp.path());

        let manager = StaticFileManager::default();
        manager.register_app("myapp", temp.path().to_path_buf());

        assert!(manager.has_static_files("myapp"));
        assert!(!manager.has_static_files("other"));

        let result = manager.resolve("myapp", "/index.html");
        assert!(result.is_some());
        assert!(result.unwrap().is_ok());
    }

    #[test]
    fn test_static_file_manager_unregister() {
        let temp = TempDir::new().unwrap();
        create_test_files(temp.path());

        let manager = StaticFileManager::default();
        manager.register_app("myapp", temp.path().to_path_buf());

        assert!(manager.has_static_files("myapp"));

        manager.unregister_app("myapp");
        assert!(!manager.has_static_files("myapp"));
    }

    #[test]
    fn test_mime_types() {
        let temp = TempDir::new().unwrap();
        let config = StaticConfig::default();
        let server = AppStaticServer::new("test".to_string(), temp.path().to_path_buf(), config);

        // Test various MIME types
        assert!(server.get_mime_type("html").contains("text/html"));
        assert!(server.get_mime_type("css").contains("text/css"));
        assert!(server.get_mime_type("js").contains("javascript"));
        assert_eq!(server.get_mime_type("png"), "image/png");
        assert_eq!(server.get_mime_type("jpg"), "image/jpeg");
        assert_eq!(server.get_mime_type("svg"), "image/svg+xml");
        assert_eq!(server.get_mime_type("woff2"), "font/woff2");
        assert_eq!(server.get_mime_type("pdf"), "application/pdf");
        assert_eq!(server.get_mime_type("unknown"), "application/octet-stream");
    }

    #[test]
    fn test_disabled_static_files() {
        let temp = TempDir::new().unwrap();
        create_test_files(temp.path());

        let config = StaticConfig {
            enabled: false,
            ..Default::default()
        };
        let server = AppStaticServer::new("test".to_string(), temp.path().to_path_buf(), config);

        assert!(!server.is_available());
    }

    #[test]
    fn test_list_apps() {
        let temp1 = TempDir::new().unwrap();
        let temp2 = TempDir::new().unwrap();
        create_test_files(temp1.path());
        create_test_files(temp2.path());

        let manager = StaticFileManager::default();
        manager.register_app("app1", temp1.path().to_path_buf());
        manager.register_app("app2", temp2.path().to_path_buf());

        let apps = manager.list_apps();
        assert_eq!(apps.len(), 2);
        assert!(apps.contains(&"app1".to_string()));
        assert!(apps.contains(&"app2".to_string()));
    }
}
